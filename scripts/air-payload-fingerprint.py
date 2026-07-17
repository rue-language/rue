#!/usr/bin/env python3
"""Fingerprint every tracked modification and untracked RUE-842 artifact."""

import argparse
import hashlib
import json
import subprocess
from pathlib import Path


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    output = args.output.as_posix()
    changed = set(git("diff", "--name-only", "--relative", args.base).splitlines())
    changed.update(
        git("ls-files", "--others", "--exclude-standard").splitlines()
    )
    changed.discard(output)
    files = {}
    aggregate = hashlib.sha256()
    for name in sorted(changed):
        path = Path(name)
        if not path.is_file():
            continue
        content = path.read_bytes()
        digest = hashlib.sha256(content).hexdigest()
        files[name] = {"bytes": len(content), "sha256": digest}
        aggregate.update(name.encode())
        aggregate.update(b"\0")
        aggregate.update(digest.encode())
        aggregate.update(b"\0")
        aggregate.update(str(len(content)).encode())
        aggregate.update(b"\n")

    args.output.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "base_commit": args.base,
                "base_tree": git("show", "-s", "--format=%T", args.base).strip(),
                "aggregate_sha256": aggregate.hexdigest(),
                "files": files,
                "self_excluded": output,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
