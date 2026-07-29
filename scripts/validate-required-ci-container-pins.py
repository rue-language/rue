#!/usr/bin/env python3
"""Reject unpinned container images in required CI workflows and executors.

Two rules, both about the same property: required CI must execute exactly the
bytes a human reviewed.

* Every checked file rejects a moving `latest` tag.
* Files named with `--digest-pinned` additionally require every executor
  `container-image` property to carry an immutable `@sha256:` digest. These are
  the remote-execution platform definitions, where a republished worker image
  would silently change what a green canary proves.
"""

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
# The Buck `remote_execution_properties` entry naming the worker image, in
# either Starlark (`"container-image": "docker://..."`) or YAML/RE-properties
# (`container-image = docker://...`) spelling.
CONTAINER_IMAGE = re.compile(
    r"""["']?container-image["']?\s*[:=]\s*["'](?P<image>[^"']*)["']"""
)
IMMUTABLE_DIGEST = re.compile(r"@sha256:[0-9a-f]{64}\b")


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


def validate_digest_pins(path: Path) -> list[str]:
    """Require an immutable digest on every executor container image."""

    errors: list[str] = []
    found = 0
    for line_number, line in enumerate(path.read_text().splitlines(), 1):
        for match in CONTAINER_IMAGE.finditer(uncommented(line)):
            found += 1
            image = match.group("image")
            if not IMMUTABLE_DIGEST.search(image):
                errors.append(
                    f"{path}:{line_number}: remote executor image {image!r} is not "
                    "pinned; append the immutable @sha256:<digest> of the reviewed image"
                )
    if not found:
        # A rename or refactor that drops the property would otherwise turn this
        # gate into a vacuous pass, which is how RUE-1152 hid a dead formatting
        # check.
        errors.append(
            f"{path}: no container-image property found; this file is declared "
            "digest-pinned, so the gate would silently check nothing"
        )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workflows", nargs="*", type=Path)
    parser.add_argument(
        "--digest-pinned",
        action="append",
        default=[],
        type=Path,
        metavar="FILE",
        help="file whose container-image properties must carry an @sha256: digest",
    )
    args = parser.parse_args()

    workflows = args.workflows or DEFAULT_WORKFLOWS
    checked = list(workflows) + list(args.digest_pinned)
    errors = [error for path in checked for error in validate(path)]
    errors += [error for path in args.digest_pinned for error in validate_digest_pins(path)]
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        f"required CI container pins valid: {len(workflows)} workflow(s), "
        f"{len(args.digest_pinned)} digest-pinned executor file(s)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
