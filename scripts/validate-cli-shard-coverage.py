#!/usr/bin/env python3
"""Assert the CLI test shards in BUCK are exactly mirrored by the CI matrix.

RUE-1116 splits //:cli-tests into CLI_TEST_SHARD_COUNT parallel shard targets
(//:cli-tests-shard-0 .. //:cli-tests-shard-{N-1}) that CI runs on separate
`platform-corpus` jobs. Nothing else re-runs those slices on CI, so a shard that
exists in BUCK but is missing from the matrix silently drops that fraction of
the corpus — exactly the RUE-924 false-green failure mode. This gate fails the
build when BUCK and the workflow disagree.

The platform set is derived directly from the `*-cli-shard-N` check names, so
no unrelated corpus job or hardcoded platform list is needed as a sentinel.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BUCK = ROOT / "BUCK"
DEFAULT_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"

SHARD_COUNT_RE = re.compile(r"^\s*CLI_TEST_SHARD_COUNT\s*=\s*(\d+)\s*$", re.MULTILINE)
# BUCK generates the shard targets with a loop over range(CLI_TEST_SHARD_COUNT),
# which is exhaustive by construction: the generator's presence is the whole
# BUCK-side check. Literal "cli-tests-shard-<n>" spellings are not recognized —
# a BUCK file without the generator fails, whatever else it contains.
BUCK_SHARD_LOOP_RE = re.compile(r'name\s*=\s*"cli-tests-shard-\{\}"\.format\(')
BUCK_SHARD_RANGE_RE = re.compile(r"range\(\s*CLI_TEST_SHARD_COUNT\s*\)")
# platform-corpus jobs are identified by their check_name, e.g.
#   check_name: linux-x64-cli-shard-2
# The non-greedy platform group stops at the "-cli-shard" suffix.
CHECK_SHARD_RE = re.compile(r"([A-Za-z0-9]+(?:-[A-Za-z0-9]+)*?)-cli-shard-(\d+)\b")


def shard_count(buck_text: str) -> int | None:
    match = SHARD_COUNT_RE.search(buck_text)
    return int(match.group(1)) if match else None


def validate(buck_path: Path, workflow_path: Path) -> list[str]:
    errors: list[str] = []
    buck_text = buck_path.read_text()
    workflow_text = workflow_path.read_text()

    count = shard_count(buck_text)
    if count is None:
        return [f"{buck_path}: CLI_TEST_SHARD_COUNT is not defined"]
    if count < 1:
        return [f"{buck_path}: CLI_TEST_SHARD_COUNT must be >= 1, got {count}"]

    expected = set(range(count))

    # 1. BUCK defines shard targets 0..count-1 with a generator over
    #    range(CLI_TEST_SHARD_COUNT), exhaustive by construction. This is
    #    fail-closed: a BUCK file without the generator — whether it has no
    #    shard targets at all or spells them some other way — is an error,
    #    never a silent pass.
    if not (BUCK_SHARD_LOOP_RE.search(buck_text) and BUCK_SHARD_RANGE_RE.search(buck_text)):
        errors.append(
            f"{buck_path}: found no cli-tests-shard targets — expected a "
            '"cli-tests-shard-{}".format(...) generator over '
            "range(CLI_TEST_SHARD_COUNT)"
        )

    # 2. Every platform represented by a shard check must list all shards
    #    0..count-1, and no others.
    by_platform: dict[str, set[int]] = {}
    for platform, shard in CHECK_SHARD_RE.findall(workflow_text):
        by_platform.setdefault(platform, set()).add(int(shard))
    platforms = sorted(by_platform)
    if not platforms:
        errors.append(f"{workflow_path}: no *-cli-shard-N jobs found")

    for platform in platforms:
        got = by_platform.get(platform, set())
        if got != expected:
            detail = []
            missing = sorted(expected - got)
            extra = sorted(got - expected)
            if missing:
                detail.append(f"missing shards {missing}")
            if extra:
                detail.append(f"unexpected shards {extra}")
            errors.append(
                f"{workflow_path}: platform {platform!r} cli shards = {sorted(got)}, "
                f"expected 0..{count - 1} ({'; '.join(detail)})"
            )

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--buck", type=Path, default=DEFAULT_BUCK)
    parser.add_argument("--workflow", type=Path, default=DEFAULT_WORKFLOW)
    args = parser.parse_args()

    errors = validate(args.buck, args.workflow)
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    count = shard_count(args.buck.read_text())
    print(f"CLI shard coverage valid: {count} shard(s) mirrored on every CLI platform")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
