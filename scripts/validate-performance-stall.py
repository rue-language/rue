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

Three things can stop, and all three are checked here. Points can stop arriving,
which is what the commit-count rule catches. Points can also keep arriving while
the headline index they exist to move goes missing, in two ways that look
identical on the page and are opposite in the manifest. An epoch whose declared
baseline resolves to nothing publishes a ratio for no workload (RUE-1486). An
epoch that never declared one at all publishes no ratio either, and that state
is legitimate right after the epoch is declared, which is why it survived
fourteen complete runs on every platform (RUE-1533).

Neither of those failed openly anywhere: the collector succeeded, no run was
rejected, and the page simply stopped saying whether the compiler is getting
slower.

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

# Complete runs an epoch may collect before it must have pinned a baseline
# (RUE-1533). Counted in the epoch's own collected points rather than in trunk
# commits, which is what every other rule here counts, for two reasons.
#
# Trunk commits are the right unit for "collection stopped", where the question
# is how far the series has fallen behind the tree. They are the wrong unit for
# "a maintainer has not done something yet", because commits do not arrive one
# at a time: the RUE-1522 stack put twelve on trunk in a single merge, so a
# twenty-commit deadline can expire in the time it takes one pull request to
# land. Points arrive once per collection, so a stack costs one.
#
# Points also measure from inside the epoch. A deadline counted from the
# measured commit to trunk gives an epoch declared while collection is behind
# no grace at all — its first point arrives already past due, which is exactly
# the state a stall's remedy creates.
#
# Ten is a handful of hours at the current collection cadence, and well inside
# the fourteen that went uncaught here. A quiet weekend collects nothing and
# spends none of it.
DEFAULT_MAX_UNPINNED_POINTS = 10


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


def newest_epochs(data: dict) -> list[tuple[str, dict]]:
    """Return each platform's live epoch: the one holding its newest point.

    A retired epoch keeps whatever it published and is not the signal anyone
    reads, so only the epoch still receiving points is held to publishing one.
    """
    live = []
    for platform in data.get("platforms", []):
        newest = None
        for epoch in platform.get("epochs", []):
            for point in epoch.get("points", []):
                if newest is None or point["finished_at"] > newest[0]:
                    newest = (point["finished_at"], epoch)
        if newest is not None:
            live.append((platform["platform"], newest[1]))
    return live


def unindexed(data: dict) -> list[tuple[str, int, int]]:
    """Return (platform, epoch, points) for live epochs publishing no index.

    An epoch with no baseline yet is the documented state of one that has just
    been declared, and it publishes no index by design; `unpinned` is what holds
    that state to a deadline. An epoch that *has* a baseline and still publishes
    no index is the failure: the baseline names a run nothing resolves to, so no
    point carries a ratio and the headline index is absent from a series that
    otherwise looks healthy.
    """
    missing = []
    for platform, epoch in newest_epochs(data):
        if not epoch.get("baseline_commit"):
            continue
        points = epoch.get("points", [])
        if points and not any(point.get("index") for point in points):
            missing.append((platform, epoch.get("epoch"), len(points)))
    return missing


def unpinned(
    data: dict,
    max_points: int = DEFAULT_MAX_UNPINNED_POINTS,
) -> list[tuple[str, int, str, str, int]]:
    """Return live epochs that could have pinned a baseline and still have not.

    Declaring an epoch without a baseline is correct — a baseline is a complete
    valid run *under* that epoch, so it cannot be named until collection has
    produced one — but the state is meant to end at the first such run. Nothing
    ended it for epoch 6, which collected fourteen complete runs on every
    platform while the headline index the page exists to publish stayed absent
    (RUE-1533).

    The deadline is the epoch's own complete points, for the reasons given at
    `DEFAULT_MAX_UNPINNED_POINTS`. Complete points only, because a partial run
    is never a baseline; the run named is the first of them, because that is
    the one the manifest should pin and the moment the clock started.

    Takes no repository: this rule is decidable from the derived data alone,
    which also keeps a shallow checkout from turning it into an error.

    Returns (platform, epoch, run, commit, collected), naming the run to pin
    rather than only the omission.
    """
    late = []
    for platform, epoch in newest_epochs(data):
        if epoch.get("baseline_commit"):
            continue
        complete = sorted(
            (point for point in epoch.get("points", []) if point.get("complete")),
            key=lambda point: point["finished_at"],
        )
        if not complete:
            # Collecting, but nothing yet that could serve as a baseline. The
            # honest state of an epoch declared moments ago, and of one whose
            # runs are all partial — which the commit-count rule owns.
            continue
        if len(complete) > max_points:
            first = complete[0]
            late.append(
                (
                    platform,
                    epoch.get("epoch"),
                    first["run"],
                    first["commit"],
                    len(complete),
                )
            )
    return late


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


def report_unindexed(missing: list[tuple[str, int, int]]) -> str:
    lines = [
        "The published performance series is collecting but publishes no index.",
        "",
    ]
    for platform, epoch, points in missing:
        lines.append(
            f"  {platform}: epoch {epoch} declares a baseline, has {points} "
            f"plotted point(s), and none of them carries a headline index"
        )
    lines += [
        "",
        "This is a repository-wide condition and almost certainly not caused by",
        "this pull request. Points are arriving, so nothing fails on its own:",
        "the page keeps drawing per-workload series while the one figure that",
        "answers 'is the compiler getting slower' is quietly absent.",
        "",
        "An index needs a baseline that resolves. To resolve it:",
        "",
        "  1. Check that the run named by `[epoch.baseline]` in",
        "     performance/manifest.toml is still stored under that name on",
        "     performance-data-v1. A record is named by the bytes it was",
        "     published as; nothing may rename it afterwards.",
        "  2. Check that the epoch's runs are complete. A headline point",
        "     publishes only from a run that measured every suite workload.",
    ]
    return "\n".join(lines)


def report_unpinned(late: list[tuple[str, int, str, str, int]], max_points: int) -> str:
    lines = [
        "A collecting epoch has never pinned its baseline, so no index is published.",
        "",
    ]
    for platform, epoch, _run, commit, count in late:
        lines.append(
            f"  {platform}: epoch {epoch} has collected {count} complete runs "
            f"(threshold {max_points}) and could have pinned a baseline since "
            f"the first of them, at {commit[:12]}"
        )
    lines += [
        "",
        "This is a repository-wide condition and almost certainly not caused by",
        "this pull request. An epoch is declared without a baseline on purpose —",
        "a baseline is a complete valid run under that epoch, so it cannot be",
        "named until one exists — but that state ends at the first such run, and",
        "these did not end.",
        "",
        "Pin each platform's first complete valid run in performance/manifest.toml,",
        "in that epoch's block:",
        "",
    ]
    for platform, epoch, run, commit, _count in late:
        lines += [
            f"  # {platform}, epoch {epoch}",
            "  [epoch.baseline]",
            f'  commit = "{commit}"',
            f'  run = "{run}"',
            "",
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
    parser.add_argument(
        "--max-unpinned-points",
        type=int,
        default=DEFAULT_MAX_UNPINNED_POINTS,
        help="complete runs an epoch may collect before it must pin a baseline",
    )
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

    missing = unindexed(data)
    if missing:
        print(report_unindexed(missing), file=sys.stderr)
        return 1

    late = unpinned(data, args.max_unpinned_points)
    if late:
        print(report_unpinned(late, args.max_unpinned_points), file=sys.stderr)
        return 1

    for platform, commit, finished_at in plotted:
        print(f"{platform}: current at {commit[:12]} ({finished_at})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
