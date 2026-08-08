#!/usr/bin/env python3
"""Fail when the published performance series has stopped advancing (RUE-1258).

A stall is a repository-wide condition, not a property of whichever change
caused it, so this runs on every pull request rather than only on changes that
touch performance. There is deliberately no bypass: the remedy is to declare the
next epoch, which is a small manifest change, and a genuine emergency is served
by bypassing branch protection rather than by machinery kept here for the
purpose.

"Stalled" is measured in merged trunk commits rather than wall-clock, because
commit count degrades gracefully across quiet weekends while a clock does not.
Collection legitimately lags trunk by minutes, and a single failed collection
should not block the repository, so the threshold tolerates both.

The series state comes from `rue-bench derive`, never from a status file stored
alongside the raw records: ADR-0067 keeps everything derived out of the data
branch, and a stored staleness flag is exactly the kind of value that would
drift from the records it came from and be believed anyway.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

# Five merged trunk commits with no new plotted point, per platform (RUE-1258).
# Low enough to catch a real stall within hours at the current merge rate; high
# enough that one failed collection, or collection simply lagging a merge, never
# blocks a pull request.
DEFAULT_MAX_COMMITS = 5


class HistoryUnavailable(Exception):
    """A measured commit is not present in the checkout's history."""


def newest_plotted(data: dict) -> list[tuple[str, str, str]]:
    """Return (platform, commit, finished_at) for each platform's newest point.

    Points are ordered by measurement time within an epoch, and epochs are
    ordered by identifier, so the newest point is the last point of the last
    epoch that has any.
    """
    newest = []
    for platform in data.get("platforms", []):
        latest = None
        for epoch in platform.get("epochs", []):
            for point in epoch.get("points", []):
                if latest is None or point["finished_at"] > latest["finished_at"]:
                    latest = point
        if latest is not None:
            newest.append((platform["platform"], latest["commit"], latest["finished_at"]))
    return newest


def stalled(
    data: dict,
    commits_since,
    max_commits: int = DEFAULT_MAX_COMMITS,
) -> list[tuple[str, str, int]]:
    """Return (platform, commit, commits_behind) for every stalled platform.

    `commits_since` maps a commit to the number of trunk commits merged after
    it. Injected rather than called directly so the rule is testable without a
    repository.
    """
    behind = []
    for platform, commit, _ in newest_plotted(data):
        count = commits_since(commit)
        if count > max_commits:
            behind.append((platform, commit, count))
    return behind


def git_commits_since(repo: Path, ref: str):
    """Count trunk commits merged after a given commit."""

    def count(commit: str) -> int:
        result = subprocess.run(
            ["git", "-C", str(repo), "rev-list", "--count", f"{commit}..{ref}"],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            # Almost always a shallow checkout that does not contain the
            # measured commit. Distinguished from "not stalled" on purpose: a
            # gate that cannot see the history must say so rather than pass.
            raise HistoryUnavailable(
                f"could not count commits between {commit[:12]} and {ref}: "
                f"{result.stderr.strip()}"
            )
        return int(result.stdout.strip())

    return count


def report(behind: list[tuple[str, str, int]], max_commits: int) -> str:
    lines = [
        "The published performance series has stopped advancing.",
        "",
    ]
    for platform, commit, count in behind:
        lines.append(
            f"  {platform}: newest plotted point is {commit[:12]}, "
            f"{count} trunk commits behind (threshold {max_commits})"
        )
    lines += [
        "",
        "This is a repository-wide condition and almost certainly not caused by",
        "this pull request. Every pull request fails while it lasts, because a",
        "stalled series hides regressions rather than reporting them.",
        "",
        "To resolve it:",
        "",
        "  1. Find why runs stopped entering their series. The dashboard's",
        "     'Collection health' disclosure lists rejected runs and the reason.",
        "  2. If a pinned input changed, declare the next epoch in",
        "     performance/manifest.toml and mark it `collection = true`. A new",
        "     epoch needs no baseline to begin accepting runs.",
        "  3. If collection itself is broken, fix the collector; the runs are",
        "     kept on performance-data-v1 either way and will back-fill.",
    ]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--data",
        type=Path,
        required=True,
        help="derived performance data, as written by `rue-bench derive`",
    )
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument(
        "--ref",
        default="origin/trunk",
        help="the branch a stall is measured against (default: origin/trunk)",
    )
    parser.add_argument("--max-commits", type=int, default=DEFAULT_MAX_COMMITS)
    args = parser.parse_args()

    data = json.loads(args.data.read_text())

    plotted = newest_plotted(data)
    if not plotted:
        # An empty dashboard is the honest first state of a suite that has not
        # collected yet, not a stall. Failing here would block the repository
        # on the day collection is introduced.
        print("no plotted points yet; nothing to stall")
        return 0

    try:
        behind = stalled(data, git_commits_since(args.repo, args.ref), args.max_commits)
    except HistoryUnavailable as error:
        print(f"error: {error}", file=sys.stderr)
        print(
            "The measured commit must be present in the checkout. Fetch with "
            "enough depth to reach it.",
            file=sys.stderr,
        )
        return 2

    if behind:
        print(report(behind, args.max_commits), file=sys.stderr)
        return 1

    for platform, commit, finished_at in plotted:
        print(f"{platform}: current at {commit[:12]} ({finished_at})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
