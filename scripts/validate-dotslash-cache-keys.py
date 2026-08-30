#!/usr/bin/env python3
"""Key every dotslash cache on the pinned manifest, not the wrapper script.

`buck2` is a bash wrapper; the DotSlash manifest carrying the pinned release
digests is `buck2-bin` (the wrapper execs `dotslash "$repo_root/buck2-bin"`).
A cache key hashing the wrapper does not change when the pin is bumped, so
`actions/cache` reports an exact hit on a store that lacks the new binary and
therefore never saves the freshly downloaded one back — every job in every run
re-downloads tens of megabytes, indefinitely and silently, because dotslash
succeeds either way (RUE-1854).

So a `dotslash-` cache key must hash `buck2-bin`, and must not hash the
`buck2` wrapper: naming the wrapper would reinstate the stale-key window on
any pin bump that leaves it untouched.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_PATTERNS = ("*.yml", "*.yaml")


def workflows_in(directory: Path) -> list[Path]:
    """Every workflow file in `directory`, in a stable order."""

    return sorted(
        path for pattern in WORKFLOW_PATTERNS for path in directory.glob(pattern)
    )


DEFAULT_WORKFLOWS = workflows_in(ROOT / ".github" / "workflows")
# An `actions/cache` key for the dotslash store. The store path is what makes
# it a dotslash cache, but the key prefix is the reviewed convention and is
# what a new copy-pasted step carries.
DOTSLASH_KEY = re.compile(r"^\s*key:\s*(?P<key>.*dotslash-.*)$")
HASH_FILES = re.compile(r"hashFiles\((?P<arguments>[^)]*)\)")
QUOTED = re.compile(r"""['"](?P<name>[^'"]*)['"]""")
PINNED_MANIFEST = "buck2-bin"
WRAPPER = "buck2"


def validate(path: Path) -> tuple[list[str], int]:
    """Return this file's errors and how many dotslash keys it declares."""

    errors: list[str] = []
    keys = 0
    for line_number, line in enumerate(path.read_text().splitlines(), 1):
        match = DOTSLASH_KEY.match(line)
        if not match:
            continue
        keys += 1
        key = match.group("key")
        hashed = [
            name
            for arguments in HASH_FILES.finditer(key)
            for name in QUOTED.findall(arguments.group("arguments"))
        ]
        if not hashed:
            errors.append(
                f"{path}:{line_number}: dotslash cache key hashes no file; key it on "
                f"{PINNED_MANIFEST!r} so a pin bump invalidates the entry"
            )
            continue
        if WRAPPER in hashed:
            errors.append(
                f"{path}:{line_number}: dotslash cache key hashes the {WRAPPER!r} wrapper; "
                f"the pinned digests live in {PINNED_MANIFEST!r}, so a pin bump would "
                "leave this key unchanged and the cached store permanently stale"
            )
        if PINNED_MANIFEST not in hashed:
            errors.append(
                f"{path}:{line_number}: dotslash cache key does not hash {PINNED_MANIFEST!r}; "
                "a bumped pin would restore a store without the new binary and never save it back"
            )
    return errors, keys


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "workflows",
        nargs="*",
        type=Path,
        help="workflow files, or a directory whose workflow files are all checked",
    )
    args = parser.parse_args()

    # A directory argument keeps the checked set identical to what CI actually
    # runs: a workflow added tomorrow is covered without editing this gate.
    workflows = [
        workflow
        for argument in args.workflows
        for workflow in (workflows_in(argument) if argument.is_dir() else [argument])
    ] or DEFAULT_WORKFLOWS
    errors: list[str] = []
    keys = 0
    for path in workflows:
        file_errors, file_keys = validate(path)
        errors += file_errors
        keys += file_keys
    if not keys:
        # A rename of the cache-key convention would otherwise turn this gate
        # into a vacuous pass over files it no longer recognizes.
        errors.append(
            "no dotslash cache key found in any checked workflow; the gate would "
            "silently check nothing"
        )
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        f"dotslash cache keys valid: {keys} key(s) across "
        f"{len(workflows)} workflow(s) hash {PINNED_MANIFEST}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
