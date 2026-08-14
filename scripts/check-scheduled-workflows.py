#!/usr/bin/env python3
"""Notice when a scheduled workflow has never once done its job.

A workflow on a `schedule:` trigger has no audience. Nobody opens a pull
request against it, no merge waits on it, and its result is a row in a tab
nobody visits. `correctness-repetitions.yml` failed on *every* scheduled run it
ever had — two for two, each dead in 18 seconds on an argument the invoked
script does not accept — and that went unnoticed for eleven days (RUE-1507).
The bug itself is small. What makes it worth a gate is that the workflow was
believed: a safeguard that fails silently is worth less than no safeguard,
because its absence is not noticed while its presence is assumed.

This runs in required CI, on the pull-request path, because that is the one
signal in this repository that provably reaches a human — reporting it on
another unattended timer would inherit the very bug being fixed.

**Exactly one condition blocks a merge**: a workflow that has run on its
schedule at least twice and has never once succeeded. That signal is durable
(no flaky run produces it), unambiguous (nothing it protects was ever
protected), and self-clearing (one green run ends it forever). It is the
RUE-1507 shape precisely.

Everything else warns and exits 0. This gate runs on every pull request in the
repository, so a false positive here blocks *all* work — strictly worse than
the bug it is defending against. Staleness in particular is a heuristic built
on cron cadence, and GitHub's scheduler jitter, queue delays, and a workflow's
own designed red runs all move it; `fuzz.yml` is red on 67.7% of its 226
retained scheduled runs, with a 75-day gap between successes, entirely by
design. A heuristic like that may inform a human. It may not block one.

The same asymmetry governs failure: an API that will not answer, a response
that cannot be parsed, or any condition this script cannot classify is
reported and passes. The repository's established posture for an unavailable
capability is to say so and continue (see the BuildBuddy provisioning steps in
`ci.yml`), not to halt every merge.

Usage:
    scripts/check-scheduled-workflows.py --repo OWNER/NAME   # structure + history
    scripts/check-scheduled-workflows.py --offline           # structure only
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

GITHUB_API_URL = "https://api.github.com"

WORKFLOWS_DIR = Path(__file__).resolve().parents[1] / ".github/workflows"

#: A workflow must have failed at least this many scheduled runs, with no
#: success, before the "never succeeded" verdict blocks a merge. One failed run
#: is a red run; two-for-two with nothing green is a workflow that has never
#: worked. `correctness-repetitions.yml` reached this on its second Monday.
MIN_RUNS_FOR_NEVER_SUCCEEDED = 2

#: Multiple of a workflow's own cron period allowed to pass with no successful
#: run before it is *reported* stale. Advisory only. Six rather than four
#: because a daily cron at four tolerates only three consecutive red runs, and
#: `release.yml` produced a three-run streak within the last three weeks.
DEFAULT_STALE_PERIODS = 6

#: Floor under any staleness budget. A sub-daily cron would otherwise get a
#: budget of hours, so one morning's outage would read as a dead workflow.
MIN_STALE_BUDGET_HOURS = 72.0

#: Issue reference required of every `known_broken` declaration. Rue tracks work
#: in Linear as `RUE-NN`; a waiver pointing at nothing is a waiver nobody can
#: follow up.
ISSUE_RE = re.compile(r"^RUE-\d+$")


@dataclass(frozen=True)
class Policy:
    """A declared deviation from the default rules, and why it exists.

    `stale_periods = None` disables the staleness report entirely, for a
    workflow whose red runs are designed behavior rather than evidence.
    """

    stale_periods: int | None = DEFAULT_STALE_PERIODS
    known_broken: str = ""
    note: str = ""


POLICIES: dict[str, Policy] = {
    # RUE-1507's original finding, fixed by RUE-1222. Both scheduled runs it has
    # ever had failed in 18s with `error: unexpected argument '--env' found`, so
    # it has never once done the work it was added for. This is the one waiver
    # that suppresses a *blocking* verdict, which is why it names the issue that
    # removes it.
    "correctness-repetitions.yml": Policy(
        known_broken="RUE-1222",
        note="never succeeded; `--env` argument bug fixed by RUE-1222",
    ),
    # Nightly fuzzing is red *by design* whenever it finds a crash, and already
    # reports each one into the Rue Linear team (RUE-802,
    # scripts/fuzz-report-failure.py). Its full retained history — 226 scheduled
    # runs from 2026-01-01, measured 2026-08-14 — is 67.7% failures, with a
    # longest failing streak of 74 runs and a longest gap between successes of
    # 75 days. No staleness threshold separates that from a fuzzer that has
    # stopped working, so this does not pretend one does. Whether the fuzzer is
    # healthy is answered by the crashes it files, not by the color of the job.
    "fuzz.yml": Policy(
        stale_periods=None,
        note="red on any crash found; 67.7% of 226 runs, 75-day success gaps",
    ),
}


# --------------------------------------------------------------------------
# Workflow discovery
# --------------------------------------------------------------------------

#: The `on:` key through the next top-level key. Tolerates the quoted `"on":`
#: form (YAML 1.1 reads a bare `on` as boolean true, so some repositories quote
#: it), and any trailing content or comment on the key's own line.
ON_BLOCK_RE = re.compile(
    r"^[\"']?on[\"']?:(?P<inline>[^\n]*)\n(?P<body>(?:[ \t].*\n|[ \t]*\n)*)",
    re.MULTILINE,
)

#: A cron value in either block style (`- cron: '0 0 * * *'`) or flow style
#: (`schedule: [{cron: '0 0 * * *'}]`), quoted or bare. Matching the value
#: rather than the surrounding structure is what makes both styles work.
CRON_VALUE_RE = re.compile(
    r"""cron:\s*(?:"(?P<dq>[^"]+)"|'(?P<sq>[^']+)'|(?P<bare>[^\n,\]}#]+))"""
)


@dataclass(frozen=True)
class Scheduled:
    """A workflow file that GitHub will run on a timer."""

    name: str
    path: Path
    crons: tuple[str, ...]
    source: str

    @property
    def period_hours(self) -> float:
        """The shortest interval between two fires of this workflow."""
        return min(cron_period_hours(expr) for expr in self.crons)

    @property
    def policy(self) -> Policy:
        return POLICIES.get(self.name, Policy())


@dataclass
class Discovery:
    """What the tree scan found, and what it could not read.

    The warnings matter as much as the findings. A workflow whose triggers this
    parser cannot read is absent from the audit entirely, which is the
    "inspects nothing, reports success" failure at per-workflow granularity —
    so it is reported rather than skipped.
    """

    scheduled: list[Scheduled] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)


def strip_comments(text: str) -> str:
    """Remove YAML comments so prose cannot satisfy a structural check.

    Deliberately simple: a `#` inside a quoted string would be stripped too.
    Nothing this reads (cron expressions, step keys, artifact paths) contains
    one, and the conservative direction is to see less, not more.
    """
    out = []
    for line in text.splitlines():
        stripped = line.lstrip()
        if stripped.startswith("#"):
            continue
        out.append(re.sub(r"\s+#.*$", "", line))
    return "\n".join(out) + "\n"


def cron_period_hours(expr: str) -> float:
    """How often a 5-field cron expression fires, in hours.

    Only the coarse magnitude matters — this feeds an advisory staleness window
    — so the answer errs toward the *longer* period, which widens the window
    and makes the report quieter rather than noisier. The calendar fields are
    therefore tested from the rarest constraint inward: a month restriction
    fires yearly however permissive the day and hour fields look.
    """
    fields = expr.split()
    if len(fields) != 5:
        # Not something to guess at, so assume the rarest cadence in use.
        return 8760.0
    minute, hour, dom, month, dow = fields
    if month != "*":
        return 8760.0
    if dom != "*":
        return 720.0
    if dow != "*":
        return 168.0
    step = re.fullmatch(r"\*/(\d+)", hour)
    if step:
        return max(1.0, float(step.group(1)))
    if hour != "*":
        return 24.0
    step = re.fullmatch(r"\*/(\d+)", minute)
    if step:
        return max(1.0, float(step.group(1)) / 60.0)
    return 1.0


def on_block(source: str) -> str | None:
    """The text of a workflow's trigger block, comments removed.

    Scoped to `on:` so the word `schedule` in a job name, a comment, or a
    `run:` heredoc cannot invent a trigger that does not exist.
    """
    match = ON_BLOCK_RE.search(strip_comments(source))
    if not match:
        return None
    return match.group("inline") + "\n" + match.group("body")


def schedule_expressions(source: str) -> tuple[str, ...]:
    """Cron expressions in this workflow's trigger block."""
    block = on_block(source)
    if block is None:
        return ()
    crons = []
    for match in CRON_VALUE_RE.finditer(block):
        value = match.group("dq") or match.group("sq") or match.group("bare") or ""
        value = value.strip()
        if value:
            crons.append(value)
    return tuple(crons)


def discover(workflows_dir: Path) -> Discovery:
    """Every workflow in the tree carrying a `schedule:` trigger.

    Discovery is by trigger rather than from a list in this file, so a
    scheduled workflow added later is covered without anyone remembering to
    register it — the omission this gate exists to prevent is precisely the
    kind nobody remembers.
    """
    result = Discovery()
    for path in sorted(workflows_dir.glob("*.y*ml")):
        source = path.read_text()
        block = on_block(source)
        if block is None:
            result.warnings.append(
                f"{path.name}: no `on:` trigger block could be read, so this file "
                "was not audited. The parser, not the workflow, is likely wrong."
            )
            continue
        crons = schedule_expressions(source)
        if crons:
            result.scheduled.append(Scheduled(path.name, path, crons, source))
        elif "schedule" in block:
            result.warnings.append(
                f"{path.name}: its `on:` block mentions `schedule` but no cron "
                "expression could be read from it, so it was not audited."
            )
    return result


# --------------------------------------------------------------------------
# Run history
# --------------------------------------------------------------------------


class TransportError(RuntimeError):
    """A request could not be completed. Reported, never fatal."""


class Transport:
    """Minimal injectable HTTP seam, so tests run the real client code."""

    def get(self, url: str) -> dict:
        raise NotImplementedError


class UrllibTransport(Transport):
    """The real transport: stdlib only, so the script has no dependencies.

    Retries the statuses that mean "ask again" — rate limiting and server
    faults — honoring `Retry-After` when GitHub sends one. This job issues its
    queries on every pull request and every merge-group commit against a token
    bucket shared with the rest of CI, so a transient 429 must not be the
    script's final answer.
    """

    RETRY_STATUSES = frozenset({429, 500, 502, 503, 504})

    def __init__(
        self,
        token: str = "",
        timeout: float = 30.0,
        attempts: int = 3,
        sleep=time.sleep,
    ) -> None:
        self.token = token
        self.timeout = timeout
        self.attempts = attempts
        self.sleep = sleep

    def get(self, url: str) -> dict:
        last: TransportError | None = None
        for attempt in range(self.attempts):
            try:
                return self._get_once(url)
            except NotRegistered:
                raise
            except TransportError as error:
                last = error
                if not getattr(error, "retryable", False):
                    raise
                if attempt + 1 < self.attempts:
                    self.sleep(min(getattr(error, "retry_after", 0.0) or 2.0**attempt, 30.0))
        assert last is not None
        raise last

    def _get_once(self, url: str) -> dict:
        request = urllib.request.Request(url, method="GET")
        request.add_header("Accept", "application/vnd.github+json")
        request.add_header("X-GitHub-Api-Version", "2022-11-28")
        if self.token:
            request.add_header("Authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                body = response.read().decode(errors="replace")
        except urllib.error.HTTPError as error:
            if error.code == 404:
                raise NotRegistered(url)
            detail = error.read().decode(errors="replace")[:300]
            failure = TransportError(f"GET {url} -> HTTP {error.code}: {detail}")
            if error.code in self.RETRY_STATUSES:
                failure.retryable = True  # type: ignore[attr-defined]
                header = (error.headers or {}).get("Retry-After")
                failure.retry_after = float(header) if header else 0.0  # type: ignore[attr-defined]
            raise failure
        except urllib.error.URLError as error:
            failure = TransportError(f"GET {url} -> {error.reason}")
            failure.retryable = True  # type: ignore[attr-defined]
            raise failure
        try:
            return json.loads(body)
        except json.JSONDecodeError:
            raise TransportError(f"GET {url} -> non-JSON response: {body[:200]}")


class NotRegistered(TransportError):
    """GitHub does not know this workflow: it is not on the default branch yet."""


@dataclass
class History:
    """What GitHub knows about one workflow's scheduled runs."""

    state: str
    scheduled_runs: int
    successful_runs: int
    last_success: datetime | None


def plural(count: int, noun: str) -> str:
    """`1 run` / `2 runs`. Output is read by people; agreement is not optional."""
    return f"{count} {noun}" if count == 1 else f"{count} {noun}s"


def parse_time(value: str) -> datetime:
    """Parse a GitHub timestamp as an aware UTC datetime.

    GitHub answers with both `...Z` and `...-05:00` forms depending on the
    endpoint; `fromisoformat` before Python 3.11 rejects the former.
    """
    return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)


class Client:
    """Reads the Actions API.

    Deliberately frugal. Workflow states arrive in one listing rather than one
    request each, and the second runs query is issued only when the first shows
    no successes, so an all-green repository costs one request per workflow
    plus one.
    """

    def __init__(self, transport: Transport, repo: str) -> None:
        self.transport = transport
        self.repo = repo
        self._states: dict[str, str] | None = None
        self._states_error: TransportError | None = None

    def states(self) -> dict[str, str]:
        """Every registered workflow's state, keyed by file name.

        A failure is remembered, not retried. The caller asks once per
        workflow, so without this a single outage would re-issue the listing
        (and its retries) seven times over.
        """
        if self._states_error is not None:
            raise self._states_error
        if self._states is None:
            try:
                data = self.transport.get(
                    f"{GITHUB_API_URL}/repos/{self.repo}/actions/workflows?per_page=100"
                )
            except TransportError as error:
                self._states_error = error
                raise
            self._states = {
                Path(entry.get("path", "")).name: entry.get("state", "unknown")
                for entry in data.get("workflows", [])
            }
        return self._states

    def _runs(self, name: str, **params: str) -> dict:
        query = urllib.parse.urlencode({"event": "schedule", "per_page": "1", **params})
        return self.transport.get(
            f"{GITHUB_API_URL}/repos/{self.repo}/actions/workflows/{name}/runs?{query}"
        )

    def history(self, name: str) -> History:
        states = self.states()
        if name not in states:
            raise NotRegistered(name)
        successes = self._runs(name, status="success")
        successful = int(successes.get("total_count", 0))
        runs = successes.get("workflow_runs") or []

        last_success = None
        if runs:
            # A run enters the `status=success` filter when it *finishes*, so
            # `updated_at` is when the success actually existed. `created_at`
            # can be tens of minutes earlier, which on a daily cadence is the
            # difference between inside and outside a budget.
            entry = runs[0]
            stamp = entry.get("updated_at") or entry.get("created_at")
            if stamp:
                last_success = parse_time(stamp)

        # Only needed to tell "never fired" from "fired and never succeeded",
        # which matters only when there are no successes at all.
        scheduled = successful
        if successful == 0:
            scheduled = int(self._runs(name).get("total_count", 0))

        return History(
            state=states[name],
            scheduled_runs=scheduled,
            successful_runs=successful,
            last_success=last_success,
        )


# --------------------------------------------------------------------------
# Classification
# --------------------------------------------------------------------------

OK = "ok"
WARN = "warn"
BLOCK = "block"


@dataclass
class Finding:
    """One workflow's verdict, with the evidence it was reached from."""

    workflow: str
    severity: str
    summary: str
    detail: str = ""

    @property
    def blocks(self) -> bool:
        return self.severity == BLOCK


def classify(workflow: Scheduled, history: History | None, now: datetime) -> Finding:
    """Judge one workflow. Only a never-succeeded verdict may block a merge."""
    policy = workflow.policy
    name = workflow.name

    if history is None:
        return Finding(
            name,
            OK,
            "not yet registered with GitHub; will be checked once it reaches trunk",
        )

    if history.state != "active":
        # Run history cannot see this: a disabled workflow stops producing runs
        # and its last one may well have been green. It is reported rather than
        # blocking because a workflow disabled on purpose, for an afternoon, is
        # indistinguishable here from one GitHub retired for inactivity.
        return Finding(
            name,
            WARN,
            f"disabled by GitHub (state: {history.state}) and will never fire again",
            "`disabled_inactivity` means 60 days passed with no repository "
            "activity. Re-enable it from the Actions tab or with `gh workflow "
            "enable` if it is still wanted.",
        )

    if history.scheduled_runs == 0:
        # Never fired. This is also exactly how a pull request that *adds* a
        # schedule to an already-registered workflow file looks, so it reports
        # rather than blocking: the API cannot say when the schedule was added,
        # only when the file was first registered.
        return Finding(
            name,
            WARN,
            "has never run on its schedule",
            f"cron {' / '.join(workflow.crons)}. Expected for a schedule added "
            "in this branch; if it has been on trunk for longer than that, the "
            "workflow exists and is producing nothing.",
        )

    if history.successful_runs == 0:
        if history.scheduled_runs < MIN_RUNS_FOR_NEVER_SUCCEEDED:
            finding = Finding(
                name,
                WARN,
                f"its only scheduled run failed ({history.scheduled_runs} run so far)",
                "One red run is not yet evidence the workflow has never worked.",
            )
        else:
            finding = Finding(
                name,
                BLOCK,
                f"has never succeeded in {history.scheduled_runs} scheduled runs",
                "Every run it has ever had failed, so whatever it was added to "
                "protect has never once been protected. This is RUE-1507's shape: "
                "the workflow is believed while doing nothing.",
            )
    elif policy.stale_periods is None or history.last_success is None:
        finding = Finding(
            name,
            OK,
            f"{plural(history.successful_runs, 'successful scheduled run')}; "
            "staleness not assessed",
        )
    else:
        budget = max(
            policy.stale_periods * workflow.period_hours, MIN_STALE_BUDGET_HOURS
        )
        idle = (now - history.last_success).total_seconds() / 3600.0
        if idle > budget:
            finding = Finding(
                name,
                WARN,
                f"last succeeded {idle / 24:.1f} days ago, past its "
                f"{budget / 24:.1f}-day advisory window",
                f"cron {' / '.join(workflow.crons)} has fired repeatedly since "
                "without a green run. Worth a look; not proof of breakage.",
            )
        else:
            finding = Finding(
                name,
                OK,
                f"last succeeded {idle / 24:.1f} days ago "
                f"({plural(history.successful_runs, 'successful scheduled run')})",
            )

    if policy.known_broken:
        if finding.severity == BLOCK:
            return Finding(
                name,
                OK,
                f"known broken, tracked by {policy.known_broken}: {finding.summary}",
            )
        if finding.severity == OK:
            # The waiver has outlived the breakage. Reported, not blocking: a
            # workflow turning green is good news, and good news arriving on a
            # cron nobody chose must not stop every merge in the repository
            # until someone edits this file.
            return Finding(
                name,
                WARN,
                f"waiver for {policy.known_broken} is no longer needed: the "
                f"workflow is healthy again ({finding.summary})",
                f"Delete the {name!r} entry from POLICIES in "
                f"{Path(__file__).name}. A waiver that outlives its breakage "
                "would silently exempt the next one.",
            )
    return finding


# --------------------------------------------------------------------------
# Structural checks
# --------------------------------------------------------------------------

#: The directory `scripts/ci-timed` copies a failing command's output into, and
#: the artifact path the `ci.yml` lanes upload from.
FAILURE_LOG_DIR = "rue-ci-failed-logs"


def structural_problems(scheduled: list[Scheduled]) -> list[str]:
    """Structure that must hold for a scheduled failure to stay debuggable.

    A weekly job's console log ages out of retention, and the Actions job-log
    API serves only a truncated tail even before it does, so a failure nobody
    looked at for a fortnight is undiagnosable from the run page alone
    (RUE-1268). `scripts/ci-timed` already preserves the full output of a
    failing command; a workflow that uses it and then does not upload what it
    preserved is discarding the evidence at the last step.

    The check is a whole-file one: it proves the workflow uploads the preserved
    directory on failure somewhere, not that every `ci-timed` job has its own
    upload. Pairing them per job needs a real YAML parse, which is not
    available here. Comments are stripped first, so prose mentioning the
    directory cannot satisfy it — that much would otherwise be exactly the
    false-green shape this file exists to catch.
    """
    problems = []
    for workflow in scheduled:
        source = strip_comments(workflow.source)
        if "scripts/ci-timed" not in source:
            continue
        if FAILURE_LOG_DIR not in source or "if: failure()" not in source:
            problems.append(
                f"{workflow.name}: runs commands through scripts/ci-timed but has no "
                f"`if: failure()` step uploading {FAILURE_LOG_DIR}; a scheduled "
                "failure stops being debuggable as soon as the job log ages out. "
                "Add the upload-artifact step used by the ci.yml lanes."
            )
    return problems


def policy_problems(scheduled: list[Scheduled]) -> list[str]:
    """Declared policies that name nothing, or that explain nothing.

    Asked of the complete discovered set, never a subset: this is a question
    about the repository. These block because they are deterministic facts
    about the tree, with no external state that could make them spuriously
    true.
    """
    known = {workflow.name for workflow in scheduled}
    problems = []
    for name in sorted(POLICIES):
        policy = POLICIES[name]
        if name not in known:
            problems.append(
                f"POLICIES declares {name!r}, which is not a scheduled workflow in "
                f"{WORKFLOWS_DIR.name}/. Remove the entry: it no longer exempts "
                "anything, and its presence hides that the declarations were never "
                "re-examined."
            )
            continue
        if policy.known_broken and not ISSUE_RE.match(policy.known_broken):
            problems.append(
                f"POLICIES[{name!r}].known_broken is {policy.known_broken!r}, which "
                "is not a RUE-NN issue. A waiver suppresses a blocking verdict, so "
                "it must name the issue that removes it."
            )
        if not policy.note.strip():
            problems.append(
                f"POLICIES[{name!r}] carries no note. A declaration without a "
                "stated reason cannot be re-examined by the next reader."
            )
    return problems


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------


@dataclass
class Report:
    findings: list[Finding] = field(default_factory=list)
    problems: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)

    @property
    def blocked(self) -> bool:
        return bool(self.problems) or any(f.blocks for f in self.findings)


def check(
    scheduled: list[Scheduled],
    client: Client | None,
    now: datetime | None = None,
) -> Report:
    """Run the structural checks, and the history checks when given a client."""
    now = now or datetime.now(timezone.utc)
    report = Report(
        problems=structural_problems(scheduled) + policy_problems(scheduled)
    )
    if client is None:
        return report
    for workflow in scheduled:
        try:
            history: History | None = client.history(workflow.name)
        except NotRegistered:
            history = None
        except TransportError as error:
            # One unreadable workflow must not decide the repository's fate.
            report.warnings.append(f"{workflow.name}: run history unavailable: {error}")
            continue
        try:
            report.findings.append(classify(workflow, history, now))
        except Exception as error:  # noqa: BLE001 - deliberately total
            # A shape the classifier does not expect is a bug in this file, and
            # a bug in this file must not block every merge in the repository.
            report.warnings.append(
                f"{workflow.name}: could not be classified ({error!r}); "
                "treating as unknown rather than failing the build."
            )
    return report


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--repo",
        default=os.environ.get("GITHUB_REPOSITORY", ""),
        help="OWNER/NAME to read run history from (default: $GITHUB_REPOSITORY)",
    )
    parser.add_argument(
        "--workflows",
        type=Path,
        default=WORKFLOWS_DIR,
        help="directory of workflow files to audit",
    )
    parser.add_argument(
        "--offline",
        action="store_true",
        help="run only the structural checks, which need no network",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)

    discovered = discover(args.workflows)
    scheduled = discovered.scheduled
    if not scheduled:
        # The repository has scheduled workflows. Finding none means discovery
        # broke, and a gate that inspects an empty list and reports success is
        # the same bug one level up. This one is deterministic and offline, so
        # it is safe to block on.
        print(
            f"error: no scheduled workflows found in {args.workflows}; "
            "the discovery is broken, not the tree",
            file=sys.stderr,
        )
        return 1
    print(f"auditing {len(scheduled)} scheduled workflow(s) in {args.workflows}")

    client = None
    if not args.offline:
        if not args.repo:
            print(
                "warning: no --repo and no $GITHUB_REPOSITORY; checking structure "
                "only. Run history was not read.",
                file=sys.stderr,
            )
        else:
            token = os.environ.get("GITHUB_TOKEN", "").strip()
            if not token:
                print(
                    "warning: GITHUB_TOKEN is unset; using unauthenticated API quota",
                    file=sys.stderr,
                )
            client = Client(UrllibTransport(token), args.repo)

    # `check` absorbs every per-workflow transport and classification failure
    # into warnings, so there is no error path here to catch: an unavailable
    # API produces a warned, passing report rather than an exception.
    report = check(scheduled, client)
    report.warnings = discovered.warnings + report.warnings

    for finding in sorted(report.findings, key=lambda f: f.workflow):
        mark = {BLOCK: "FAIL", WARN: "warn", OK: "ok  "}[finding.severity]
        print(f"{mark} {finding.workflow}: {finding.summary}")
        if finding.severity != OK and finding.detail:
            print(f"       {finding.detail}")
    for problem in report.problems:
        print(f"FAIL {problem}")
    for warning in report.warnings:
        print(f"warn {warning}")

    if report.blocked:
        print(
            "\nA scheduled workflow has never once succeeded, or the audit itself is "
            "misdeclared. Fix it, or — if a tracked issue already owns the fix — "
            "declare it in POLICIES with that issue.",
            file=sys.stderr,
        )
        return 1

    # Say exactly what was established, and nothing more. A partial audit
    # reported as a clean bill of health is the overclaim this file condemns.
    checked = len(report.findings)
    waived = sum(1 for f in POLICIES.values() if f.known_broken)
    warned = sum(1 for f in report.findings if f.severity == WARN)
    if client is None:
        print(f"\nStructure only: {len(scheduled)} workflow(s), no run history read.")
    else:
        print(
            f"\nNo scheduled workflow has failed every run: {checked} audited, "
            f"{warned} with warnings, {waived} waived, "
            f"{len(scheduled) - checked} not reached."
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
