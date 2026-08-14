#!/usr/bin/env python3
"""Prove every scheduled workflow is still doing the job it was added to do.

A workflow on a `schedule:` trigger has no audience. Nobody opens a pull
request against it, no merge waits on it, and its result is a row in a tab
nobody visits. `correctness-repetitions.yml` failed on *every* scheduled run it
ever had — two for two, each dead in 18 seconds on an argument the invoked
script does not accept — and that went unnoticed for eleven days (RUE-1507).
The bug itself is small. What makes it worth a gate is that the workflow was
believed: a safeguard that fails silently is worth less than no safeguard,
because its absence is not noticed while its presence is assumed.

This checks the two properties of a scheduled workflow that stay quiet when it
is healthy and cannot stay quiet when it is not:

**It has succeeded at least once.** A workflow whose entire history is failures
never worked, so nothing it claims to protect has ever been protected. That is
strictly stronger evidence than one red run, and it is the exact signature of
the RUE-1507 bug.

**It has succeeded recently.** "Recently" is a generous multiple of the
workflow's own cron period, so an ordinary red night is not a repository-wide
event, while a workflow that has quietly stopped working is one.

Both are durable: a workflow that is fine never trips either, no matter how
flaky an individual run is. That is what makes it safe to run this where it
will actually be read — required CI on every pull request — rather than on
another unattended timer. The mechanism this file exists to fix cannot be the
mechanism that reports on it.

Two failure modes get their own answers because a general one would be wrong:

* A workflow GitHub has **disabled** never fires again. It has no failing run
  to notice, so run history alone cannot see it. The workflow's `state` can.
  (GitHub disables scheduled workflows in repositories with no activity for 60
  days, and a disabled workflow reports nothing forever.)
* A workflow this branch **adds** has no history and is not registered at all.
  Failing the pull request that introduces a scheduled workflow would be a gate
  that punishes its own adoption, so an unregistered or just-created workflow
  passes with a note until its cron has had a chance to fire.

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
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

GITHUB_API_URL = "https://api.github.com"

WORKFLOWS_DIR = Path(__file__).resolve().parents[1] / ".github/workflows"

#: Multiple of a workflow's own cron period allowed to pass with no successful
#: run before it is called stale. Four is deliberately loose: this gate blocks
#: pull requests, so it must answer "has this stopped working?" and never "did
#: last night go badly?".
DEFAULT_STALE_PERIODS = 4


@dataclass(frozen=True)
class Policy:
    """A declared deviation from the default rules, and why it exists.

    `known_broken` is the workflow equivalent of the repository's
    `known_bug = "RUE-NN"` xfail markers: it records a real failure against the
    issue that will fix it instead of hiding it. It expires the same way those
    markers do — a waived workflow that has become healthy fails this check as
    a stale waiver, so the declaration cannot outlive the breakage and quietly
    keep a future regression silent.
    """

    stale_periods: int = DEFAULT_STALE_PERIODS
    known_broken: str = ""
    note: str = ""


POLICIES: dict[str, Policy] = {
    # RUE-1507's original finding, fixed by RUE-1222. Both scheduled runs it
    # has ever had failed in 18s with `error: unexpected argument '--env'
    # found`, so it has never once done the work it was added for. The waiver
    # exists so this gate does not block the very pull request that repairs it;
    # when RUE-1222 lands and the next Monday run goes green, this entry starts
    # failing as stale and must be deleted.
    "correctness-repetitions.yml": Policy(
        known_broken="RUE-1222",
        note="never succeeded; `--env` argument bug fixed by RUE-1222",
    ),
    # Nightly fuzzing is red *by design* whenever it finds a crash, and it
    # already reports each one into the Rue Linear team (RUE-802,
    # scripts/fuzz-report-failure.py), so its red nights reach a human without
    # this gate's help. Blocking every pull request because the fuzzer did its
    # job would be strictly harmful. Measured over the 100 scheduled runs
    # ending 2026-08-14: 46 failures, a longest run of 13 consecutive failing
    # nights, and a longest gap between successes of 14 days. Thirty days
    # leaves that ordinary behavior alone while still catching a fuzzer that
    # has stopped working outright.
    "fuzz.yml": Policy(
        stale_periods=30,
        note="red on any crash found; per-crash reporting is RUE-802's job",
    ),
}


# --------------------------------------------------------------------------
# Workflow discovery
# --------------------------------------------------------------------------

#: `on:` through the next top-level key. Workflows are discovered from the tree
#: rather than from a list in this file, so a scheduled workflow added later is
#: covered without anyone remembering to register it here — the omission this
#: gate exists to prevent is precisely the kind nobody remembers.
ON_BLOCK_RE = re.compile(r"^on:\s*\n(?P<body>(?:[ \t].*\n|\n)*)", re.MULTILINE)
CRON_RE = re.compile(r"^\s+-\s+cron:\s*['\"]?(?P<expr>[^'\"#\n]+?)['\"]?\s*(?:#.*)?$")


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


def cron_period_hours(expr: str) -> float:
    """How often a 5-field cron expression fires, in hours.

    Only the coarse magnitude matters — this feeds a staleness window measured
    in multiples of it — so the answer errs toward the longer period, which
    makes the window wider and the gate quieter rather than noisier.
    """
    fields = expr.split()
    if len(fields) != 5:
        # Not something to guess at. Treat it as weekly, the most forgiving of
        # the cadences actually in use.
        return 168.0
    _minute, hour, dom, month, dow = fields
    if dow != "*" or month != "*":
        return 168.0
    if dom != "*":
        return 720.0
    step = re.fullmatch(r"\*/(\d+)", hour)
    if step:
        return max(1.0, float(step.group(1)))
    if hour != "*":
        return 24.0
    return 1.0


def schedule_expressions(source: str) -> tuple[str, ...]:
    """Cron expressions in this workflow's `on:` trigger block.

    Scoped to the `on:` block rather than the whole file so the word
    `schedule:` in a comment, a job name, or a `run:` heredoc cannot invent a
    trigger that does not exist.
    """
    match = ON_BLOCK_RE.search(source)
    if not match:
        return ()
    crons: list[str] = []
    in_schedule = False
    for line in match.group("body").splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if re.fullmatch(r"schedule:", stripped):
            in_schedule = True
            schedule_indent = indent
            continue
        if in_schedule:
            if indent <= schedule_indent and not stripped.startswith("-"):
                in_schedule = False
            else:
                cron = CRON_RE.match(line)
                if cron:
                    crons.append(cron.group("expr").strip())
    return tuple(crons)


def discover(workflows_dir: Path) -> list[Scheduled]:
    """Every workflow in the tree carrying a `schedule:` trigger."""
    found = []
    for path in sorted(workflows_dir.glob("*.y*ml")):
        source = path.read_text()
        crons = schedule_expressions(source)
        if crons:
            found.append(Scheduled(path.name, path, crons, source))
    return found


# --------------------------------------------------------------------------
# Structural checks (no network)
# --------------------------------------------------------------------------

#: The directory `scripts/ci-timed` copies a failing command's complete output
#: into, and the artifact path the `ci.yml` lanes upload from.
FAILURE_LOG_DIR = "rue-ci-failed-logs"


def structural_problems(scheduled: list[Scheduled]) -> list[str]:
    """Structure that must hold for a scheduled failure to stay debuggable.

    A weekly job's console log ages out of retention, and the Actions job-log
    API serves only a truncated tail even before it does, so a failure nobody
    looked at for a fortnight is undiagnosable from the run page alone
    (RUE-1268). `scripts/ci-timed` already preserves the full output of a
    failing command; a workflow that uses it and then does not upload what it
    preserved is throwing the evidence away at the last step. Requiring the
    upload only of workflows that actually run through `ci-timed` keeps the
    rule honest: elsewhere the artifact would always be empty, and an empty
    artifact that looks like coverage is the shape of problem this file exists
    to catch.
    """
    problems = []
    for workflow in scheduled:
        if "scripts/ci-timed" not in workflow.source:
            continue
        if FAILURE_LOG_DIR not in workflow.source:
            problems.append(
                f"{workflow.name}: runs commands through scripts/ci-timed but never "
                f"uploads {FAILURE_LOG_DIR}; a scheduled failure stops being "
                "debuggable as soon as the job log ages out. Add the "
                "`if: failure()` upload-artifact step used by the ci.yml lanes."
            )

    return problems


def orphaned_policies(scheduled: list[Scheduled]) -> list[str]:
    """Declared policies naming a workflow that no longer exists.

    Asked of the complete discovered set, never of a subset: this is a question
    about the repository, not about one workflow. An entry matching nothing
    exempts nothing, so it is harmless in itself — but it is evidence that the
    declarations have stopped being re-read, which is how a waiver written for
    one breakage ends up covering the next.
    """
    known = {workflow.name for workflow in scheduled}
    return [
        f"POLICIES declares {name!r}, which is not a scheduled workflow in "
        f"{WORKFLOWS_DIR.name}/. Remove the entry: it no longer exempts anything, "
        "and its presence hides that the declarations were never re-examined."
        for name in sorted(POLICIES)
        if name not in known
    ]


# --------------------------------------------------------------------------
# Run history
# --------------------------------------------------------------------------


class TransportError(RuntimeError):
    """A request could not be completed. Always surfaced; never swallowed."""


class Transport:
    """Minimal injectable HTTP seam, so tests run the real client code."""

    def get(self, url: str) -> dict:
        raise NotImplementedError


class UrllibTransport(Transport):
    """The real transport: stdlib only, so the script has no dependencies."""

    def __init__(self, token: str = "", timeout: float = 30.0) -> None:
        self.token = token
        self.timeout = timeout

    def get(self, url: str) -> dict:
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
            raise TransportError(f"GET {url} -> HTTP {error.code}: {detail}")
        except urllib.error.URLError as error:
            raise TransportError(f"GET {url} -> {error.reason}")
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
    created_at: datetime
    scheduled_runs: int
    successful_runs: int
    last_success: datetime | None


def parse_time(value: str) -> datetime:
    """Parse a GitHub timestamp as an aware UTC datetime.

    GitHub answers with both `...Z` and `...-05:00` forms depending on the
    endpoint; `fromisoformat` before Python 3.11 rejects the former.
    """
    return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)


class Client:
    """Reads the Actions API. Two counts and a state per workflow, nothing more."""

    def __init__(self, transport: Transport, repo: str) -> None:
        self.transport = transport
        self.repo = repo

    def _runs(self, name: str, **params: str) -> dict:
        query = urllib.parse.urlencode({"event": "schedule", "per_page": "1", **params})
        return self.transport.get(
            f"{GITHUB_API_URL}/repos/{self.repo}/actions/workflows/{name}/runs?{query}"
        )

    def history(self, name: str) -> History:
        meta = self.transport.get(
            f"{GITHUB_API_URL}/repos/{self.repo}/actions/workflows/{name}"
        )
        all_runs = self._runs(name)
        successes = self._runs(name, status="success")
        runs = successes.get("workflow_runs") or []
        return History(
            state=meta.get("state", "unknown"),
            created_at=parse_time(meta["created_at"]),
            scheduled_runs=int(all_runs.get("total_count", 0)),
            successful_runs=int(successes.get("total_count", 0)),
            last_success=parse_time(runs[0]["created_at"]) if runs else None,
        )


# --------------------------------------------------------------------------
# Classification
# --------------------------------------------------------------------------

OK = "ok"
FAIL = "fail"


@dataclass
class Finding:
    """One workflow's verdict, with the evidence it was reached from."""

    workflow: str
    status: str
    summary: str
    detail: str = ""

    @property
    def failed(self) -> bool:
        return self.status == FAIL


def classify(workflow: Scheduled, history: History | None, now: datetime) -> Finding:
    """Judge one workflow against the two durable properties.

    `history` of None means GitHub has never registered the workflow, which is
    what a pull request adding one looks like.
    """
    policy = workflow.policy
    name = workflow.name

    if history is None:
        return Finding(
            name,
            OK,
            "not yet registered with GitHub; will be checked once it reaches trunk",
        )

    if history.state != "active":
        # No amount of run history can see this: a disabled workflow simply
        # stops producing runs, and its last one may well have been green.
        return Finding(
            name,
            FAIL,
            f"disabled by GitHub (state: {history.state})",
            "It will never fire again until it is re-enabled. `disabled_inactivity` "
            "means 60 days passed with no repository activity; re-enable it from the "
            "Actions tab or with `gh workflow enable`.",
        )

    period = workflow.period_hours
    age_hours = (now - history.created_at).total_seconds() / 3600.0

    if history.scheduled_runs == 0:
        # Distinguish "too new to have fired" from "should have fired and did
        # not" — the second is a real finding and the first is not.
        if age_hours < 2 * period:
            return Finding(
                name, OK, "registered recently; its cron has not fired yet"
            )
        return Finding(
            name,
            FAIL,
            f"registered {age_hours / 24:.0f} days ago and has never run on its "
            "schedule",
            f"cron {' / '.join(workflow.crons)} should have fired by now. The "
            "workflow exists but is producing nothing.",
        )

    if history.successful_runs == 0:
        finding = Finding(
            name,
            FAIL,
            f"has never succeeded in {history.scheduled_runs} scheduled run(s)",
            "Every run it has ever had failed, so whatever it was added to protect "
            "has never once been protected. This is RUE-1507's shape: the workflow "
            "is believed while doing nothing.",
        )
    else:
        assert history.last_success is not None
        stale_after = policy.stale_periods * period
        idle_hours = (now - history.last_success).total_seconds() / 3600.0
        if idle_hours > stale_after:
            finding = Finding(
                name,
                FAIL,
                f"last succeeded {idle_hours / 24:.1f} days ago, over its "
                f"{stale_after / 24:.1f}-day budget",
                f"cron {' / '.join(workflow.crons)} has fired repeatedly since "
                "without a single green run. Treat it as broken, not unlucky.",
            )
        else:
            finding = Finding(
                name,
                OK,
                f"last succeeded {idle_hours / 24:.1f} days ago "
                f"({history.successful_runs} successful scheduled run(s))",
            )

    if policy.known_broken:
        if finding.failed:
            return Finding(
                name,
                OK,
                f"known broken, tracked by {policy.known_broken}: {finding.summary}",
            )
        # The waiver has outlived the breakage. Left in place it would keep
        # this workflow exempt forever, so the next regression would be as
        # silent as the one the waiver was written for.
        return Finding(
            name,
            FAIL,
            f"waiver for {policy.known_broken} is stale: the workflow is healthy "
            f"again ({finding.summary})",
            f"Delete the {name!r} entry from POLICIES in "
            f"{Path(__file__).name}. A waiver that outlives its breakage silently "
            "exempts every future one.",
        )
    return finding


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------


@dataclass
class Report:
    findings: list[Finding] = field(default_factory=list)
    structural: list[str] = field(default_factory=list)

    @property
    def failed(self) -> bool:
        return bool(self.structural) or any(f.failed for f in self.findings)


def check(
    scheduled: list[Scheduled],
    client: Client | None,
    now: datetime | None = None,
) -> Report:
    """Run the structural checks, and the history checks when given a client."""
    now = now or datetime.now(timezone.utc)
    report = Report(
        structural=structural_problems(scheduled) + orphaned_policies(scheduled)
    )
    if client is None:
        return report
    for workflow in scheduled:
        try:
            history: History | None = client.history(workflow.name)
        except NotRegistered:
            history = None
        report.findings.append(classify(workflow, history, now))
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

    scheduled = discover(args.workflows)
    if not scheduled:
        # The repository has scheduled workflows. Finding none means the
        # discovery broke, and a gate that inspects an empty list reports
        # success having checked nothing — the exact failure it is here to
        # prevent, one level up.
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
                "error: --repo (or $GITHUB_REPOSITORY) is required without --offline",
                file=sys.stderr,
            )
            return 2
        token = os.environ.get("GITHUB_TOKEN", "").strip()
        if not token:
            print(
                "warning: GITHUB_TOKEN is unset; using unauthenticated API quota",
                file=sys.stderr,
            )
        client = Client(UrllibTransport(token), args.repo)

    try:
        report = check(scheduled, client)
    except TransportError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    for finding in sorted(report.findings, key=lambda f: f.workflow):
        mark = "FAIL" if finding.failed else "ok  "
        print(f"{mark} {finding.workflow}: {finding.summary}")
        if finding.failed and finding.detail:
            print(f"       {finding.detail}")
    for problem in report.structural:
        print(f"FAIL {problem}")

    if report.failed:
        print(
            "\nA scheduled workflow is not doing its job. Fix it, or — if a tracked "
            "issue already owns the fix — declare it in POLICIES with that issue.",
            file=sys.stderr,
        )
        return 1
    # Say which question was actually answered. A structural pass reported as
    # if it were a clean bill of health is the same overclaim this gate exists
    # to catch.
    if client is None:
        print(f"\nStructure only: {len(scheduled)} workflow(s) checked, no run "
              "history read (--offline).")
    else:
        print("\nEvery scheduled workflow has succeeded, and succeeded recently.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
