#!/usr/bin/env python3
"""Add commit facts to the derived performance data (ADR-0067 §11).

Tooltips are required to name the commit short hash *and* its subject, and to
report commits-since-last-measurement when runs were skipped. A run object
carries neither: it records what was measured, not how the change was described
nor where it sat in trunk's history, and both of those can be rewritten after
the measurement without invalidating it.

So both are resolved here, at site build time, from the repository. That keeps
`rue-bench derive` free of git, and it degrades honestly: a commit that is not
present locally — a shallow clone, or history that has since been rewritten —
gets no subject and no ordinal, and the page falls back to the hash and omits
the skipped-commit line rather than inventing either.

The ordinal is a position on trunk's first-parent line, not a count of
ancestors. The difference between two ordinals is then exactly the number of
trunk commits that landed between two measurements, which is the number §11
asks for. Commits off that line get no ordinal at all, because subtracting
positions across a fork point would produce a confident wrong answer instead of
no answer.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from collections.abc import Callable, Iterable
from pathlib import Path

# Both accept an optional `user@` or `user:token@` prefix and drop it. A
# checkout can carry a credential in its remote URL — GitHub Actions writes one
# during checkout — and this value is published to a world-readable file.
SSH_REMOTE = re.compile(
    r"^(?:ssh://)?(?:[^@/]+@)?github\.com[:/]([^/]+)/([^/]+?)(?:\.git)?/?$"
)
HTTPS_REMOTE = re.compile(
    r"^https?://(?:[^@/]+@)?github\.com/([^/]+)/([^/]+?)(?:\.git)?/?$"
)

# Probed in order. The website builds from a full checkout whose HEAD may sit on
# a feature branch, so the remote-tracking ref is preferred over whatever
# happens to be checked out.
TRUNK_REFS = ("origin/trunk", "trunk", "HEAD")


def git(repo: Path, *args: str) -> str | None:
    """Run a read-only git command, or return None if it cannot be run."""
    try:
        result = subprocess.run(
            ["git", "-C", str(repo), *args],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return None
    if result.returncode != 0:
        return None
    return result.stdout


def subject_of(repo: Path, commit: str) -> str | None:
    output = git(repo, "log", "-1", "--format=%s", commit)
    if output is None:
        return None
    return output.strip() or None


def first_parent_ordinals(repo: Path, refs: Iterable[str] = TRUNK_REFS) -> dict[str, int]:
    """Map each commit on trunk's first-parent line to its position on it.

    One git call for the whole history rather than one per commit, and the
    oldest reachable commit is position 1 so the numbers ascend with time.
    """
    for ref in refs:
        output = git(repo, "rev-list", "--first-parent", ref)
        if output is None:
            continue
        # Newest first, which is why the index is counted from the other end.
        commits = output.split()
        return {commit: len(commits) - index for index, commit in enumerate(commits)}
    return {}


def commit_url_prefix(url: str | None) -> str | None:
    """The `.../commit/` prefix for a GitHub remote URL, or None.

    Rebuilt from the owner and repository name rather than edited in place, so
    nothing from the original URL — credentials above all — can survive into
    the published prefix. A remote this does not recognize yields no prefix,
    and the page renders the hash as plain text.
    """
    for pattern in (HTTPS_REMOTE, SSH_REMOTE):
        found = pattern.match((url or "").strip())
        if found:
            return f"https://github.com/{found.group(1)}/{found.group(2)}/commit/"
    return None


def remote_url(repo: Path, remote: str = "origin") -> str | None:
    output = git(repo, "remote", "get-url", remote)
    return output.strip() if output else None


def measured_commits(data: dict) -> set[str]:
    """Every compiler revision any series in the derived data plots.

    Both record kinds, because both dashboards want the same tooltip. The
    ADR-0072 runtime series is a sibling section rather than another platform
    (its points measure the compiled program, not the compiler), so it has to be
    walked explicitly — a section this missed would render every hash without a
    subject and quietly lose the skipped-commit line.
    """
    sections = [data, *([data["runtime"]] if isinstance(data.get("runtime"), dict) else [])]
    return {
        point["commit"]
        for section in sections
        for platform in section.get("platforms", [])
        for epoch in platform.get("epochs", [])
        for point in epoch.get("points", [])
    }


def annotate(
    data: dict,
    subject_of_commit: Callable[[str], str | None],
    ordinal_of_commit: Callable[[str], int | None],
    url_prefix: str | None = None,
) -> tuple[int, int, int]:
    """Attach subjects, trunk ordinals, and the commit-link prefix.

    Pure with respect to git: the lookups are injected. Returns the resolved
    subject count, the resolved ordinal count, and the number of measured
    commits, for the build log.
    """
    commits = sorted(measured_commits(data))

    subjects = {}
    ordinals = {}
    for commit in commits:
        subject = subject_of_commit(commit)
        if subject is not None:
            subjects[commit] = subject
        ordinal = ordinal_of_commit(commit)
        if ordinal is not None:
            ordinals[commit] = ordinal

    data["commit_subjects"] = subjects
    data["commit_ordinals"] = ordinals
    data["commit_url_prefix"] = url_prefix
    return len(subjects), len(ordinals), len(commits)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    args = parser.parse_args()

    data = json.loads(args.data.read_text())

    ordinals = first_parent_ordinals(args.repo)
    prefix = commit_url_prefix(remote_url(args.repo))
    subjects_found, ordinals_found, total = annotate(
        data,
        lambda commit: subject_of(args.repo, commit),
        ordinals.get,
        prefix,
    )

    args.data.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    print(f"  resolved {subjects_found}/{total} commit subject(s)")
    print(f"  placed {ordinals_found}/{total} commit(s) on trunk's first-parent line")
    print(f"  commit links: {prefix or 'unavailable, hashes render as plain text'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
