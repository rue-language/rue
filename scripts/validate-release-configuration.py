#!/usr/bin/env python3
"""Verify that Buck analyzes Rue's debug and release Rust flags correctly."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TARGET = "//crates/rue:rue"
PLATFORMS = {
    "debug": "//platforms:debug",
    "release": "//platforms:release",
}
RELEASE_FLAGS = ("-Copt-level=3", "-Clto=thin")
RELEASE_CALLERS = {
    "bench.sh": "//platforms:$BUILD_MODE",
    "scripts/parser-profile.py": "//platforms:release",
    "scripts/perf-baseline.py": "//platforms:release",
}


def rustc_cfg_command(payload: str, platform: str) -> str:
    try:
        actions = json.loads(payload)
    except json.JSONDecodeError as error:
        raise ValueError(f"{platform} aquery returned invalid JSON: {error}") from error

    matches = [
        attributes.get("cmd", "")
        for key, attributes in actions.items()
        if "prelude//rust/tools:rustc_cfg" in key and f"root//platforms:{platform}#" in key
    ]
    if len(matches) != 1:
        raise ValueError(
            f"{platform} aquery exposed {len(matches)} matching rustc_cfg actions, expected 1"
        )
    return matches[0]


def validate_commands(debug: str, release: str) -> list[str]:
    errors: list[str] = []
    for flag in RELEASE_FLAGS:
        if flag not in release:
            errors.append(f"release rustc_cfg is missing {flag}")
        if flag in debug:
            errors.append(f"debug rustc_cfg unexpectedly contains {flag}")
    return errors


def validate_callers(root: Path) -> list[str]:
    errors: list[str] = []
    for relative, expected_platform in RELEASE_CALLERS.items():
        source = (root / relative).read_text()
        if "--modifier //constraints:release" in source:
            errors.append(
                f"{relative}: uses the RUE-277 no-op release modifier; "
                "use --target-platforms //platforms:release"
            )
        if expected_platform not in source:
            errors.append(
                f"{relative}: release caller no longer names {expected_platform}"
            )
    return errors


def query(buck2: Path, platform: str) -> str:
    command = [
        str(buck2),
        "aquery",
        f"deps({TARGET})",
        "--target-platforms",
        PLATFORMS[platform],
        "--output-attribute",
        "^cmd$",
        "--json",
    ]
    result = subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return rustc_cfg_command(result.stdout, platform)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--buck2", type=Path, default=ROOT / "buck2")
    args = parser.parse_args()

    try:
        debug = query(args.buck2, "debug")
        release = query(args.buck2, "release")
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"error: could not inspect Buck release configuration: {error}", file=sys.stderr)
        return 1

    errors = validate_commands(debug, release) + validate_callers(ROOT)
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        "release configuration valid: release has "
        f"{' '.join(RELEASE_FLAGS)}; debug does not"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
