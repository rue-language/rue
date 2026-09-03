#!/usr/bin/env python3
"""Route every dotslash install through the one repository-owned bootstrap.

Installing the dotslash launcher and caching the buck2 binary it downloads are
one operation, and for a long time every job spelled both halves itself. The
copies drifted -- differing store paths, three different cache-key spellings --
and eight of them (the four performance workflows and the website deploy)
carried the install with no cache at all, so every scheduled run re-downloaded
buck2 from the releases CDN. RUE-1476 fixed the merge-queue half of that and
named these gaps; they survived because the install/cache pairing had no owner
(RUE-1825).

`.github/actions/bootstrap-dotslash` is that owner. This gate keeps it the only
one: a workflow may not reach for `facebook/install-dotslash`, and may not
declare a `dotslash-` cache of its own, because either half on its own is the
copy whose other half goes missing.

The gate also checks the action still does both jobs. Without that, deleting
the install or the cache step inside it -- or renaming the action out from
under the callers -- would leave every workflow trivially conforming and this
gate passing over nothing (the vacuous-pass failure mode of RUE-1152).

And it holds the surviving key to the RUE-1854 rule. `buck2` is a bash
wrapper; the pinned release digests live in the `buck2-bin` DotSlash
manifest. A key hashing the wrapper does not change when the pin is bumped,
so `actions/cache` reports an exact hit on a store that lacks the new binary
and never saves the freshly downloaded one back -- every job re-downloads tens
of megabytes, indefinitely and silently, because dotslash succeeds either
way. So every `dotslash-` key in the action must hash `buck2-bin` and must
not hash `buck2`.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import run_gate

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_PATTERNS = ("*.yml", "*.yaml")
ACTION_DIRECTORY = "bootstrap-dotslash"
ACTION_REFERENCE = f"./.github/actions/{ACTION_DIRECTORY}"
UPSTREAM_INSTALLER = "facebook/install-dotslash"
DOTSLASH_KEY = re.compile(r"^\s*key:\s*.*dotslash-")
CACHE_STEP = re.compile(r"uses:\s*actions/cache(?:/[a-z]+)?@")
HASH_FILES = re.compile(r"hashFiles\((?P<arguments>[^)]*)\)")
QUOTED = re.compile(r"""['"](?P<name>[^'"]*)['"]""")
PINNED_MANIFEST = "buck2-bin"
WRAPPER = "buck2"


def workflows_in(directory: Path) -> list[Path]:
    """Every workflow file under `directory`, in a stable order."""

    return sorted(
        path for pattern in WORKFLOW_PATTERNS for path in directory.rglob(pattern)
    )


def check_workflows(workflows: list[Path]) -> tuple[list[str], int]:
    """Errors from the caller side, and how many callers use the action."""

    errors: list[str] = []
    callers = 0
    for path in workflows:
        lines = path.read_text().splitlines()
        callers += sum(1 for line in lines if ACTION_REFERENCE in line)
        for number, line in enumerate(lines, 1):
            if UPSTREAM_INSTALLER in line:
                errors.append(
                    f"{path}:{number}: installs dotslash directly; use "
                    f"`uses: {ACTION_REFERENCE}` so the install cannot be "
                    "separated from its cache"
                )
            if DOTSLASH_KEY.match(line):
                errors.append(
                    f"{path}:{number}: declares its own dotslash cache key; "
                    f"{ACTION_REFERENCE} owns the store paths and key policy"
                )
    return errors, callers


def check_action(action: Path) -> list[str]:
    """Errors from the action side: it must still install and still cache."""

    if not action.is_file():
        return [
            f"{action}: the canonical dotslash bootstrap is missing; every "
            "workflow would be free to install dotslash its own way again"
        ]
    text = action.read_text()
    errors: list[str] = []
    if UPSTREAM_INSTALLER not in text:
        errors.append(
            f"{action}: no longer installs dotslash, so the workflows that "
            "call it get no toolchain and this gate checks nothing"
        )
    keys = [line for line in text.splitlines() if DOTSLASH_KEY.match(line)]
    if not (CACHE_STEP.search(text) and keys):
        errors.append(
            f"{action}: no longer declares a dotslash cache, which is the half "
            "the copies kept losing"
        )
    for key in keys:
        hashed = [
            name
            for arguments in HASH_FILES.finditer(key)
            for name in QUOTED.findall(arguments.group("arguments"))
        ]
        if WRAPPER in hashed:
            errors.append(
                f"{action}: a dotslash cache key hashes the {WRAPPER!r} wrapper; the "
                f"pinned digests live in {PINNED_MANIFEST!r}, so a pin bump would leave "
                "this key unchanged and the cached store permanently stale (RUE-1854)"
            )
        if PINNED_MANIFEST not in hashed:
            errors.append(
                f"{action}: a dotslash cache key does not hash {PINNED_MANIFEST!r}; a "
                "bumped pin would restore a store without the new binary and never "
                "save it back (RUE-1854)"
            )
    return errors


def validate(github: Path) -> list[str]:
    workflows = workflows_in(github / "workflows")
    errors, callers = check_workflows(workflows)
    errors += check_action(github / "actions" / ACTION_DIRECTORY / "action.yml")
    if not callers:
        errors.append(
            f"no workflow uses {ACTION_REFERENCE}; a renamed bootstrap would "
            "leave this gate passing over workflows it no longer recognizes"
        )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "github",
        nargs="?",
        type=Path,
        default=ROOT / ".github",
        help="the .github directory holding workflows/ and actions/",
    )
    arguments = parser.parse_args()
    github = arguments.github
    return run_gate(
        lambda: validate(github),
        f"dotslash bootstrap centralized: every install in "
        f"{len(workflows_in(github / 'workflows'))} workflow(s) goes through "
        f"{ACTION_REFERENCE}",
    )


if __name__ == "__main__":
    sys.exit(main())
