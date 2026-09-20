#!/usr/bin/env python3
"""Hold the two Lean toolchain pins equal (ADR-0097).

`docs/formal/lean/lean-toolchain` is what `lake` and the editor extension read
(`leanprover/lean4:v4.33.1`); `toolchains/lean/defs.bzl` is what Buck fetches
(`LEAN_VERSION = "4.33.1"`, and release URLs that embed it). A developer who
bumps one and not the other would build and prove against a different Lean
than CI or a reviewer, which is exactly the drift the mechanization exists to
rule out. This gate reads both and fails when they name different releases.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_LEAN_TOOLCHAIN = ROOT / "docs" / "formal" / "lean" / "lean-toolchain"
DEFAULT_BUCK_DEFS = ROOT / "toolchains" / "lean" / "defs.bzl"

# `leanprover/lean4:v4.33.1`, possibly with surrounding whitespace.
LEAN_TOOLCHAIN = re.compile(r"^\s*leanprover/lean4:v(?P<version>\d+\.\d+\.\d+)\s*$")
BUCK_VERSION = re.compile(r'^LEAN_VERSION\s*=\s*"(?P<version>\d+\.\d+\.\d+)"\s*$', re.M)


def lean_toolchain_version(path: Path) -> str:
    text = path.read_text()
    match = LEAN_TOOLCHAIN.match(text)
    if match is None:
        raise ValueError(
            f"{path}: expected one line `leanprover/lean4:vX.Y.Z`, found {text.strip()!r}"
        )
    return match.group("version")


def buck_version(path: Path) -> str:
    matches = BUCK_VERSION.findall(path.read_text())
    if len(matches) != 1:
        raise ValueError(f"{path}: expected exactly one `LEAN_VERSION = \"X.Y.Z\"`, found {len(matches)}")
    return matches[0]


def errors(lean_toolchain: Path, buck_defs: Path) -> list[str]:
    try:
        pinned = lean_toolchain_version(lean_toolchain)
        fetched = buck_version(buck_defs)
    except (OSError, ValueError) as error:
        return [str(error)]
    if pinned != fetched:
        return [
            f"Lean toolchain pins disagree: {lean_toolchain} names v{pinned}, "
            f"{buck_defs} fetches v{fetched}; bump both in one change"
        ]
    return []


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--lean-toolchain", type=Path, default=DEFAULT_LEAN_TOOLCHAIN)
    parser.add_argument("--buck-defs", type=Path, default=DEFAULT_BUCK_DEFS)
    args = parser.parse_args(argv)
    problems = errors(args.lean_toolchain, args.buck_defs)
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    if not problems:
        print(f"Lean toolchain pins agree: v{lean_toolchain_version(args.lean_toolchain)}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
