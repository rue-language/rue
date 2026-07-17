#!/usr/bin/env python3
"""Reject moving container image tags in required CI workflows."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_WORKFLOWS = [ROOT / ".github" / "workflows" / "ci.yml"]
LATEST_IMAGE = re.compile(
    r"(?P<image>[A-Za-z0-9][A-Za-z0-9._:/-]*:latest)(?=@|\s|[\"']|$)"
)


def uncommented(line: str) -> str:
    """Remove a YAML/shell comment while preserving hashes inside quotes."""

    quote: str | None = None
    escaped = False
    for index, character in enumerate(line):
        if escaped:
            escaped = False
            continue
        if character == "\\" and quote == '"':
            escaped = True
            continue
        if character in "\"'":
            if quote is None:
                quote = character
            elif quote == character:
                quote = None
            continue
        if character == "#" and quote is None and (index == 0 or line[index - 1].isspace()):
            return line[:index]
    return line


def validate(path: Path) -> list[str]:
    errors: list[str] = []
    for line_number, line in enumerate(path.read_text().splitlines(), 1):
        for match in LATEST_IMAGE.finditer(uncommented(line)):
            errors.append(
                f"{path}:{line_number}: required CI container {match.group('image')!r} "
                "uses the moving latest tag; use <release>@sha256:<digest>"
            )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workflows", nargs="*", type=Path)
    args = parser.parse_args()

    workflows = args.workflows or DEFAULT_WORKFLOWS
    errors = [error for path in workflows for error in validate(path)]
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(f"required CI container pins valid: {len(workflows)} workflow(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
