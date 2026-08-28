#!/usr/bin/env python3
"""File nightly fuzz-CI failures as Linear issues (GitHub Issues is the fallback).

Rue tracks work in Linear, in the **Rue** team (docs/process/issue-tracking.md).
Until RUE-802 the nightly fuzz workflow was the one producer of work items that
did not: it opened GitHub Issues from an inline `actions/github-script` block,
so an automatically-found crash landed in a tracker nobody plans from. Worse,
its dedup was "one open issue carrying the `fuzz-crash` label at a time" — every
distinct crash found while that issue stayed open became a comment on it, so two
unrelated miscompiles were indistinguishable from the same one recurring.

This script replaces that with per-crash tracking:

* Every reproducer written under the crash directory is read back, and a stable
  **fingerprint** is computed from its target plus its *normalized* crash
  signature (see `normalize_signature`) — addresses, temp paths, source line
  numbers, and generator seeds are erased, so the same bug fingerprints
  identically from one night to the next, while two different bugs do not
  collide.
* Before filing, open issues in the Rue team are searched for that fingerprint
  (it is written into the issue description as a `Fuzz-Fingerprint:` line).
  A hit gets a comment recording the new occurrence; a miss files a new issue.
* New issues carry the `Bug` and `found-by:autonomous` labels plus the
  `[fuzz-crash]` title marker (Linear has no `fuzz-crash` label; the marker is
  what makes the class searchable, and `FUZZ_MARKER` is the single source of
  truth for it).

Transport is injected (`Transport`), so `scripts/test-fuzz-report-failure.py`
exercises fingerprinting, dedup, and payload construction against a mock without
network access or credentials, and `--dry-run` runs the real client code end to
end against a synthesizing transport.

Backend selection is deliberate and loud, because the failure mode being
defended against is *silence*: a night whose crashes are found and then dropped.
`LINEAR_API_KEY` present selects Linear. Absent, the script falls back to the
GitHub Issues path (clearly marked as a fallback in the log and in the filed
issue) so a missing secret degrades tracker quality instead of losing the
report. With neither credential available the script exits non-zero rather than
returning success having reported nothing.

Usage:
    scripts/fuzz-report-failure.py --run-url URL [--failed-targets a,b] \\
        [--crash-dir DIR] [--backend auto|linear|github] [--dry-run]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

LINEAR_API_URL = "https://api.linear.app/graphql"
GITHUB_API_URL = "https://api.github.com"

#: Linear team the Rue project plans from. Issues are `RUE-NN`.
LINEAR_TEAM_KEY = "RUE"

#: Labels applied to every issue this script files. `Bug` and
#: `found-by:autonomous` both exist in the Rue team; a label that cannot be
#: resolved is reported and skipped rather than failing the filing, because
#: losing the crash report is strictly worse than losing a label.
ISSUE_LABELS = ("Bug", "found-by:autonomous")

#: Title marker identifying the fuzz-crash class. Linear has no `fuzz-crash`
#: label, so this string is how a human (or a later query) finds every issue
#: this script has filed. Keep it in the title, not just the body: Linear's
#: issue lists show titles.
FUZZ_MARKER = "[fuzz-crash]"

#: Line written into every issue description, and the needle the dedup search
#: looks for. Changing its spelling orphans every previously-filed issue, so it
#: is a constant rather than an inline format string.
FINGERPRINT_FIELD = "Fuzz-Fingerprint"

#: Cap on quoted reproducer text in an issue body. Full inputs live in the
#: workflow's uploaded artifact; the issue only needs enough to triage.
MAX_QUOTED_INPUT = 1200


# --------------------------------------------------------------------------
# Crash collection and fingerprinting
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Crash:
    """One deduplicated crash, as recorded by a fuzz harness.

    `signature` is the harness's raw signature text; `fingerprint` is derived
    from its normalized form, and is what dedup keys on.
    """

    target: str
    signature: str
    outcome: str
    repro: str
    detail: str = ""

    @property
    def fingerprint(self) -> str:
        return fingerprint(self.target, self.signature)


#: How many trailing components of an absolute path to keep. Two is not enough:
#: `crates/rue-sema/src/check.rs` and `crates/rue-codegen/src/check.rs` both end
#: in `src/check.rs`, and folding two crates' panics into one fingerprint is
#: exactly the dedup failure this module exists to avoid.
_PATH_TAIL_COMPONENTS = 3


def _path_tail(match: re.Match[str]) -> str:
    """Drop an absolute path's leading directories, keeping its informative tail."""
    components = match.group(0).strip("/").split("/")
    return "/".join(components[-_PATH_TAIL_COMPONENTS:])


#: Substitutions applied, in order, to make a crash signature stable across
#: runs. Each erases something that varies between two observations of the SAME
#: bug; nothing here may erase something that distinguishes two different bugs.
_NORMALIZERS: tuple[tuple[re.Pattern[str], str | Callable[[re.Match[str]], str]], ...] = (
    # Heap/stack addresses and pointer-shaped hex.
    (re.compile(r"0x[0-9a-f]+"), "0xADDR"),
    # Absolute paths — the runner's checkout root and /tmp scratch directories
    # both vary per run. The lookbehind keeps this from firing mid-token, so a
    # relative path (already stable) is left exactly as the harness wrote it.
    (re.compile(r"(?<![\w.+-])(?:/[\w.+-]+)+"), _path_tail),
    # `file.rs:120:9` — a line number moves with any unrelated edit.
    (re.compile(r"\.rs:\d+(?::\d+)?"), ".rs:LINE"),
    # Generator seeds: a differential finding is the same bug at seed 4 and
    # seed 4000.
    (re.compile(r"\bseed[ =]+\d+"), "seed N"),
    # Long digit runs (byte counts, offsets, sizes). Short runs survive, so
    # `signal 6` and `signal 11` stay distinct.
    (re.compile(r"\b\d{4,}\b"), "N"),
    # Collapse whitespace last, after the patterns that may span it.
    (re.compile(r"\s+"), " "),
)


def normalize_signature(signature: str) -> str:
    """Erase run-to-run noise from a crash signature.

    The result is the fingerprint input, so this function decides what "the
    same crash" means. Two observations of one bug must normalize identically;
    two distinct bugs must not.
    """
    text = signature.strip().lower()
    for pattern, replacement in _NORMALIZERS:
        text = pattern.sub(replacement, text)
    return text.strip()[:300]


def fingerprint(target: str, signature: str) -> str:
    """Stable per-crash identity: the dedup key, and the issue's search needle.

    Truncated to 16 hex chars — long enough that a collision across the handful
    of crashes a night produces is not a practical concern, short enough to read
    in a title.
    """
    digest = hashlib.sha256(
        f"{target}\n{normalize_signature(signature)}".encode()
    ).hexdigest()
    return digest[:16]


def parse_meta(text: str) -> dict[str, str]:
    """Parse a `.txt.meta` sibling written by `rue-fuzz`'s `write_reproducer`.

    The format is line-oriented `key: value` (the harness flattens multi-line
    panic messages precisely so this stays true).
    """
    fields: dict[str, str] = {}
    for line in text.splitlines():
        key, separator, value = line.partition(":")
        if separator and key.strip():
            fields.setdefault(key.strip(), value.strip())
    return fields


def parse_oracle_diff_repro(text: str) -> tuple[str, str]:
    """Extract `(signature, detail)` from a `rue-oracle-diff` repro file.

    Those repros are Rue source with a leading `// key: value` comment block
    rather than a `.meta` sibling; `reason` is the signature-bearing field.
    """
    reason = ""
    detail_lines: list[str] = []
    for line in text.splitlines():
        if not line.startswith("//"):
            break
        body = line[2:].strip()
        key, separator, value = body.partition(":")
        if separator and key.strip() == "reason":
            reason = value.strip()
        detail_lines.append(body)
    return reason or "oracle/compiled disagreement", "\n".join(detail_lines)


def collect_crashes(crash_dir: Path) -> list[Crash]:
    """Read every reproducer under `crash_dir`, deduplicated by fingerprint.

    Both producers are handled: `rue-fuzz` (`crash-<target>-...txt` plus a
    `.txt.meta` sibling) and `rue-oracle-diff`
    (`oracle-diff-seed-N[-O<level>].rue`).
    A reproducer whose metadata is missing still reports — with a degraded
    signature — because an unexplained crash file is still a crash.
    """
    crashes: dict[str, Crash] = {}
    if not crash_dir.is_dir():
        return []

    for path in sorted(crash_dir.iterdir()):
        if path.name.endswith(".meta") or not path.is_file():
            continue

        if path.name.startswith("oracle-diff-seed-"):
            text = _read_text(path)
            signature, detail = parse_oracle_diff_repro(text)
            crash = Crash(
                target="differential",
                signature=signature,
                outcome=signature,
                repro=path.name,
                detail=detail,
            )
        elif path.name.startswith("crash-"):
            meta = parse_meta(_read_text(Path(f"{path}.meta")))
            target = meta.get("target") or _target_from_filename(path.name)
            signature = meta.get("signature", "")
            outcome = meta.get("outcome", "")
            if not signature:
                # No `.meta`: the filename's signature hash is the only stable
                # identity left, so fingerprint from that rather than lumping
                # every metadata-less crash into one bucket.
                signature = f"unrecorded signature (file {path.name})"
            crash = Crash(
                target=target,
                signature=signature,
                outcome=outcome or signature,
                repro=path.name,
                detail=_read_text(path)[:MAX_QUOTED_INPUT],
            )
        else:
            continue

        crashes.setdefault(crash.fingerprint, crash)

    return list(crashes.values())


def _target_from_filename(name: str) -> str:
    """`crash-<target>-<sighash>-<inputhash>.txt` -> `<target>`."""
    match = re.match(r"crash-(.+)-[0-9a-f]{8}-[0-9a-f]{16}\.txt$", name)
    return match.group(1) if match else "unknown"


def _read_text(path: Path) -> str:
    try:
        return path.read_text(errors="replace")
    except OSError:
        return ""


def crashes_from_failed_targets(targets: list[str]) -> list[Crash]:
    """Synthesize reports for a run that failed without leaving reproducers.

    A step can fail for reasons that never reach `write_reproducer` (a wedged
    build, a timeout killing the harness). Reporting nothing in that case is the
    exact silence RUE-802 is about, so each failed target gets a low-detail
    report whose fingerprint is stable for that target.
    """
    if not targets:
        targets = ["(job-level failure)"]
    return [
        Crash(
            target=target,
            signature="fuzz step failed without a saved reproducer",
            outcome="fuzz step failed without a saved reproducer",
            repro="(none)",
        )
        for target in targets
    ]


# --------------------------------------------------------------------------
# Issue payloads
# --------------------------------------------------------------------------


def issue_title(crash: Crash) -> str:
    """`[fuzz-crash] <target>: <outcome> (<fingerprint>)`.

    The marker leads so the class is scannable; the fingerprint trails so a
    human comparing two issues can tell recurrence from a new bug at a glance.
    """
    summary = normalize_signature(crash.outcome or crash.signature)[:80] or "crash"
    return f"{FUZZ_MARKER} {crash.target}: {summary} ({crash.fingerprint})"


def issue_body(crash: Crash, run_url: str, fallback_note: str = "") -> str:
    """Issue description: dedup key first, then everything triage needs."""
    sections = [
        f"{FINGERPRINT_FIELD}: `{crash.fingerprint}`",
        "",
        "The nightly fuzz workflow found a crash.",
        "",
        f"- **Target:** `{crash.target}`",
        f"- **Crash signature:** `{crash.signature}`",
        f"- **Outcome:** {crash.outcome}",
        f"- **Reproducer artifact:** `{crash.repro}`",
        f"- **Run:** {run_url}",
    ]
    if fallback_note:
        sections += ["", fallback_note]
    sections += [
        "",
        "### Reproducing",
        "",
        "1. Download the `fuzz-crashes` artifact from the run above.",
        "2. For a `rue-fuzz` target, feed the saved input back to it:",
        "   `./buck2 run //crates/rue-fuzz:rue-fuzz -- <target> <dir-with-the-file>`",
        "   For a `differential` finding, rerun the recorded seed:",
        "   `./buck2 run //crates/rue-oracle-diff:rue-oracle-diff -- fuzz --start <seed> --seeds 1`",
        "3. Fix the bug and add a regression test (CLI case or spec case).",
        "",
        "Occurrences of this same fingerprint are added as comments below rather "
        "than filed as new issues.",
    ]
    if crash.detail:
        sections += [
            "",
            "<details><summary>Reproducer detail</summary>",
            "",
            "```",
            crash.detail[:MAX_QUOTED_INPUT],
            "```",
            "",
            "</details>",
        ]
    return "\n".join(sections)


def recurrence_comment(crash: Crash, run_url: str) -> str:
    return (
        f"Seen again in a later nightly fuzz run.\n\n"
        f"- **Run:** {run_url}\n"
        f"- **Target:** `{crash.target}`\n"
        f"- **Reproducer artifact:** `{crash.repro}`\n"
        f"- **{FINGERPRINT_FIELD}:** `{crash.fingerprint}`\n"
    )


# --------------------------------------------------------------------------
# Transport
# --------------------------------------------------------------------------


class TransportError(RuntimeError):
    """A request could not be completed. Always surfaced; never swallowed."""


class Transport:
    """Minimal injectable HTTP seam.

    Tests substitute a mock; `--dry-run` substitutes a synthesizer. Keeping the
    seam this thin means the client code under test is the same code CI runs.
    """

    def request(
        self, method: str, url: str, headers: dict[str, str], payload: dict | None
    ) -> dict | list:
        """Send `payload` as JSON and return the decoded response.

        The return type is `dict | list` because GitHub's REST list endpoints
        answer with a bare array; callers that expect an object must say so.
        """
        raise NotImplementedError


class UrllibTransport(Transport):
    """The real transport: stdlib only, so the script has no dependencies."""

    def __init__(self, timeout: float = 30.0) -> None:
        self.timeout = timeout

    def request(
        self, method: str, url: str, headers: dict[str, str], payload: dict | None
    ) -> dict | list:
        data = json.dumps(payload).encode() if payload is not None else None
        request = urllib.request.Request(url, data=data, method=method)
        for key, value in headers.items():
            request.add_header(key, value)
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                body = response.read().decode(errors="replace")
        except urllib.error.HTTPError as error:
            detail = error.read().decode(errors="replace")[:500]
            raise TransportError(f"{method} {url} -> HTTP {error.code}: {detail}")
        except urllib.error.URLError as error:
            raise TransportError(f"{method} {url} -> {error.reason}")
        if not body:
            return {}
        try:
            return json.loads(body)
        except json.JSONDecodeError:
            raise TransportError(f"{method} {url} -> non-JSON response: {body[:200]}")


class DryRunTransport(Transport):
    """Print requests and synthesize plausible responses.

    This exists so `--dry-run` exercises the *real* client methods — query
    construction, label resolution, dedup branching, payload assembly — instead
    of a separate "what I would do" printer that can drift from them. Responses
    are shaped like an empty tracker, so a dry run always takes the create path.
    """

    def __init__(self, stream=None) -> None:
        self.stream = stream or sys.stdout
        self.requests: list[tuple[str, str, dict | None]] = []

    def request(
        self, method: str, url: str, headers: dict[str, str], payload: dict | None
    ) -> dict | list:
        self.requests.append((method, url, payload))
        operation = (payload or {}).get("operationName", "")
        print(f"[dry-run] {method} {url} {operation}".rstrip(), file=self.stream)
        if payload is not None:
            print(
                json.dumps(payload, indent=2, sort_keys=True)[:2000], file=self.stream
            )

        if operation == "FuzzTeamAndLabels":
            return {
                "data": {
                    "teams": {"nodes": [{"id": "team-dry-run", "key": LINEAR_TEAM_KEY}]},
                    "issueLabels": {
                        "nodes": [
                            {"id": f"label-{name}", "name": name}
                            for name in ISSUE_LABELS
                        ]
                    },
                }
            }
        if operation == "FuzzDedup":
            return {"data": {"issues": {"nodes": []}}}
        if operation == "FuzzIssueCreate":
            return {
                "data": {
                    "issueCreate": {
                        "success": True,
                        "issue": {
                            "id": "issue-dry-run",
                            "identifier": "RUE-DRY",
                            "url": "https://linear.app/dry-run",
                        },
                    }
                }
            }
        if operation == "FuzzCommentCreate":
            return {"data": {"commentCreate": {"success": True}}}

        # GitHub fallback (REST, no operationName).
        if method == "GET":
            return {"items": [], "nodes": []} if "search" in url else {}
        return {"html_url": "https://github.com/dry-run/issues/0", "number": 0}


# --------------------------------------------------------------------------
# Trackers
# --------------------------------------------------------------------------


@dataclass
class FiledIssue:
    """What happened to one crash. `created` False means it was a recurrence."""

    fingerprint: str
    created: bool
    reference: str


class Tracker:
    """Backend-agnostic issue filing, so `report` has one code path."""

    name = "tracker"

    def find_open(self, fingerprint: str) -> str | None:
        """Return an opaque handle to an open issue carrying `fingerprint`."""
        raise NotImplementedError

    def create(self, crash: Crash, run_url: str) -> str:
        raise NotImplementedError

    def comment(self, handle: str, crash: Crash, run_url: str) -> None:
        raise NotImplementedError


class LinearTracker(Tracker):
    """Files into the Rue team via Linear's GraphQL API.

    Team id and label ids are resolved once and cached: a night with four
    distinct crashes should not re-resolve them four times.
    """

    name = "Linear"

    def __init__(
        self, transport: Transport, api_key: str, team_key: str = LINEAR_TEAM_KEY
    ) -> None:
        self.transport = transport
        self.api_key = api_key
        self.team_key = team_key
        self._team_id: str | None = None
        self._label_ids: list[str] = []

    def _headers(self) -> dict[str, str]:
        # Personal API keys are sent raw; OAuth access tokens need `Bearer`.
        # Sending a personal key as `Bearer` is rejected, so this is not a
        # cosmetic distinction.
        authorization = (
            f"Bearer {self.api_key}"
            if self.api_key.startswith("lin_oauth")
            else self.api_key
        )
        return {
            "Authorization": authorization,
            "Content-Type": "application/json",
        }

    def _graphql(self, operation: str, query: str, variables: dict) -> dict:
        response = self.transport.request(
            "POST",
            LINEAR_API_URL,
            self._headers(),
            {"operationName": operation, "query": query, "variables": variables},
        )
        if not isinstance(response, dict):
            raise TransportError(f"Linear {operation} returned a non-object response")
        if response.get("errors"):
            raise TransportError(f"Linear {operation} failed: {response['errors']}")
        data = response.get("data")
        if data is None:
            raise TransportError(f"Linear {operation} returned no data: {response}")
        return data

    def _resolve_context(self) -> tuple[str, list[str]]:
        if self._team_id is not None:
            return self._team_id, self._label_ids

        data = self._graphql(
            "FuzzTeamAndLabels",
            """
            query FuzzTeamAndLabels($teamKey: String!) {
              teams(filter: { key: { eq: $teamKey } }, first: 1) {
                nodes { id key }
              }
              issueLabels(first: 250) { nodes { id name } }
            }
            """,
            {"teamKey": self.team_key},
        )
        teams = data.get("teams", {}).get("nodes", [])
        if not teams:
            raise TransportError(f"Linear team {self.team_key!r} not found")
        self._team_id = teams[0]["id"]

        by_name = {node["name"]: node["id"] for node in data["issueLabels"]["nodes"]}
        label_ids = []
        for label in ISSUE_LABELS:
            if label in by_name:
                label_ids.append(by_name[label])
            else:
                # Missing label is a warning, never a filing failure: a crash
                # report with the wrong labels still gets triaged; a crash
                # report that was never filed does not.
                print(
                    f"warning: Linear label {label!r} not found; filing without it",
                    file=sys.stderr,
                )
        self._label_ids = label_ids
        return self._team_id, self._label_ids

    def find_open(self, fingerprint: str) -> str | None:
        data = self._graphql(
            "FuzzDedup",
            """
            query FuzzDedup($teamKey: String!, $needle: String!) {
              issues(
                first: 50
                filter: {
                  team: { key: { eq: $teamKey } }
                  state: { type: { nin: ["completed", "canceled"] } }
                  description: { contains: $needle }
                }
              ) {
                nodes { id identifier url title }
              }
            }
            """,
            {"teamKey": self.team_key, "needle": self._needle(fingerprint)},
        )
        nodes = data.get("issues", {}).get("nodes", [])
        if not nodes:
            return None
        print(
            f"dedup: {fingerprint} already tracked by "
            f"{nodes[0].get('identifier', nodes[0]['id'])}"
        )
        return nodes[0]["id"]

    @staticmethod
    def _needle(fingerprint: str) -> str:
        """Search for the labelled field, not the bare hash.

        A bare 16-hex-character string can appear in unrelated issues (a commit
        prefix, a hash in a pasted log); matching `Fuzz-Fingerprint: <hash>`
        cannot.
        """
        return f"{FINGERPRINT_FIELD}: `{fingerprint}`"

    def create(self, crash: Crash, run_url: str) -> str:
        team_id, label_ids = self._resolve_context()
        data = self._graphql(
            "FuzzIssueCreate",
            """
            mutation FuzzIssueCreate($input: IssueCreateInput!) {
              issueCreate(input: $input) {
                success
                issue { id identifier url }
              }
            }
            """,
            {
                "input": {
                    "teamId": team_id,
                    "title": issue_title(crash),
                    "description": issue_body(crash, run_url),
                    "labelIds": label_ids,
                }
            },
        )
        result = data.get("issueCreate", {})
        if not result.get("success"):
            raise TransportError(f"Linear issueCreate did not succeed: {data}")
        issue = result["issue"]
        return issue.get("url") or issue.get("identifier") or issue["id"]

    def comment(self, handle: str, crash: Crash, run_url: str) -> None:
        self._graphql(
            "FuzzCommentCreate",
            """
            mutation FuzzCommentCreate($input: CommentCreateInput!) {
              commentCreate(input: $input) { success }
            }
            """,
            {"input": {"issueId": handle, "body": recurrence_comment(crash, run_url)}},
        )


class GitHubTracker(Tracker):
    """Fallback path: GitHub Issues, used only when `LINEAR_API_KEY` is absent.

    Kept deliberately, and marked as a fallback in every issue it files, so the
    day the secret is missing CI still records what it found. Dedup is the same
    fingerprint search, applied to open issues carrying the `fuzz-crash` label —
    an improvement on the pre-RUE-802 behavior, which deduplicated on the label
    alone and so folded unrelated crashes into one issue.
    """

    name = "GitHub Issues (fallback)"
    LABEL = "fuzz-crash"
    FALLBACK_NOTE = (
        "> **Filed by the GitHub fallback path.** `LINEAR_API_KEY` was not set "
        "for this run, so this crash could not be filed in the Rue Linear team. "
        "See docs/process/fuzz-failure-reporting.md."
    )

    def __init__(self, transport: Transport, token: str, repo: str) -> None:
        self.transport = transport
        self.token = token
        self.repo = repo

    def _headers(self) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {self.token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "Content-Type": "application/json",
        }

    def find_open(self, fingerprint: str) -> str | None:
        issues = self.transport.request(
            "GET",
            f"{GITHUB_API_URL}/repos/{self.repo}/issues"
            f"?state=open&labels={self.LABEL}&per_page=100",
            self._headers(),
            None,
        )
        # The REST list endpoint returns a bare array; the transport normalizes
        # nothing, so accept both shapes.
        nodes = issues if isinstance(issues, list) else issues.get("items", [])
        needle = LinearTracker._needle(fingerprint)
        for issue in nodes:
            if needle in (issue.get("body") or ""):
                print(f"dedup: {fingerprint} already tracked by #{issue['number']}")
                return str(issue["number"])
        return None

    def create(self, crash: Crash, run_url: str) -> str:
        response = self.transport.request(
            "POST",
            f"{GITHUB_API_URL}/repos/{self.repo}/issues",
            self._headers(),
            {
                "title": issue_title(crash),
                "body": issue_body(crash, run_url, self.FALLBACK_NOTE),
                "labels": [self.LABEL, "bug"],
            },
        )
        if not isinstance(response, dict):
            raise TransportError("GitHub issue creation returned a non-object response")
        return response.get("html_url") or str(response.get("number", "?"))

    def comment(self, handle: str, crash: Crash, run_url: str) -> None:
        self.transport.request(
            "POST",
            f"{GITHUB_API_URL}/repos/{self.repo}/issues/{handle}/comments",
            self._headers(),
            {"body": recurrence_comment(crash, run_url)},
        )


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------


@dataclass
class Environment:
    """The credentials and repository coordinates a run was given."""

    linear_api_key: str = ""
    github_token: str = ""
    repo: str = ""

    @classmethod
    def from_os(cls, env: dict[str, str] | None = None) -> Environment:
        env = os.environ if env is None else env
        return cls(
            linear_api_key=env.get("LINEAR_API_KEY", "").strip(),
            github_token=env.get("GITHUB_TOKEN", "").strip(),
            repo=env.get("GITHUB_REPOSITORY", "").strip(),
        )


def select_tracker(
    environment: Environment, transport: Transport, backend: str = "auto"
) -> Tracker:
    """Pick the backend, preferring Linear, and explain the choice.

    Raises when nothing is usable: exiting non-zero is the whole point of the
    fallback design — CI must never report success having filed nothing.
    """
    if backend in ("auto", "linear") and environment.linear_api_key:
        return LinearTracker(transport, environment.linear_api_key)
    if backend == "linear":
        raise TransportError("--backend linear requires LINEAR_API_KEY")

    if backend in ("auto", "github") and environment.github_token and environment.repo:
        if backend == "auto":
            print(
                "warning: LINEAR_API_KEY is not set; falling back to GitHub Issues. "
                "Add the secret (docs/process/fuzz-failure-reporting.md) so fuzz "
                "crashes land in the Rue Linear team.",
                file=sys.stderr,
            )
        return GitHubTracker(transport, environment.github_token, environment.repo)

    raise TransportError(
        "no usable issue tracker: set LINEAR_API_KEY (preferred), or "
        "GITHUB_TOKEN plus GITHUB_REPOSITORY for the fallback path"
    )


@dataclass
class ReportResult:
    backend: str
    filed: list[FiledIssue] = field(default_factory=list)

    @property
    def created(self) -> int:
        return sum(1 for issue in self.filed if issue.created)

    @property
    def recurrences(self) -> int:
        return sum(1 for issue in self.filed if not issue.created)


def report(tracker: Tracker, crashes: list[Crash], run_url: str) -> ReportResult:
    """File or update one issue per distinct crash fingerprint."""
    result = ReportResult(backend=tracker.name)
    for crash in crashes:
        handle = tracker.find_open(crash.fingerprint)
        if handle is None:
            reference = tracker.create(crash, run_url)
            print(f"filed {crash.fingerprint} ({crash.target}): {reference}")
            result.filed.append(FiledIssue(crash.fingerprint, True, reference))
        else:
            tracker.comment(handle, crash, run_url)
            print(f"commented {crash.fingerprint} ({crash.target}): {handle}")
            result.filed.append(FiledIssue(crash.fingerprint, False, handle))
    return result


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--crash-dir",
        default="crates/rue-fuzz/crashes",
        help="directory the fuzz harnesses wrote reproducers to",
    )
    parser.add_argument(
        "--failed-targets",
        default="",
        help="comma-separated fuzz targets whose step failed; used to report a "
        "failure that left no reproducer behind",
    )
    parser.add_argument("--run-url", default="", help="URL of the CI run")
    parser.add_argument(
        "--backend",
        choices=("auto", "linear", "github"),
        default="auto",
        help="auto (default) uses Linear when LINEAR_API_KEY is set, else GitHub",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="run the real client code against a synthesizing transport, "
        "printing every request instead of sending it",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)

    crashes = collect_crashes(Path(args.crash_dir))
    if not crashes:
        targets = [t.strip() for t in args.failed_targets.split(",") if t.strip()]
        crashes = crashes_from_failed_targets(targets)
        print(
            f"no reproducers under {args.crash_dir}; reporting "
            f"{len(crashes)} failed target(s) instead"
        )
    else:
        print(f"{len(crashes)} distinct crash fingerprint(s) under {args.crash_dir}")

    environment = Environment.from_os()
    if args.dry_run:
        transport: Transport = DryRunTransport()
        # A dry run must exercise a real client, so synthesize whichever
        # credential the selected backend needs.
        if args.backend == "github":
            environment = Environment(
                github_token="dry-run", repo=environment.repo or "rue-lang/rue"
            )
        else:
            environment = Environment(linear_api_key="dry-run")
    else:
        transport = UrllibTransport()

    try:
        tracker = select_tracker(environment, transport, args.backend)
        print(f"reporting via {tracker.name}")
        result = report(tracker, crashes, args.run_url or "(no run URL)")
    except TransportError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        f"done: {result.created} issue(s) filed, "
        f"{result.recurrences} recurrence comment(s) via {result.backend}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
