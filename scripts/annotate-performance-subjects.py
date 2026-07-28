#!/usr/bin/env python3
"""Add commit subjects to the derived performance data (ADR-0067 §11).

Tooltips are required to name the commit short hash *and* its subject. A run
object deliberately does not carry the subject: it records what was measured,
not how the change was described, and a subject can be rewritten after the
measurement without invalidating it.

So the subject is resolved here, at site build time, from the repository. A
commit that is not present locally — a shallow clone, or history that has since
been rewritten — simply has no subject, and the page falls back to the hash
rather than inventing a description.
"""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path


def subject_of(repo: Path, commit: str) -> str | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(repo), "log", "-1", "--format=%s", commit],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return None
    if result.returncode != 0:
        return None
    subject = result.stdout.strip()
    return subject or None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    args = parser.parse_args()

    data = json.loads(args.data.read_text())

    commits = {
        point["commit"]
        for platform in data.get("platforms", [])
        for epoch in platform.get("epochs", [])
        for point in epoch.get("points", [])
    }

    subjects = {}
    for commit in sorted(commits):
        subject = subject_of(args.repo, commit)
        if subject is not None:
            subjects[commit] = subject

    data["commit_subjects"] = subjects
    args.data.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    print(f"  resolved {len(subjects)}/{len(commits)} commit subject(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
