#!/usr/bin/env python3
"""Require every Rue test tier to be selected by a *named* CI job (RUE-1117).

`//test_tiers.bxl:validate` proves every first-party test target declares
exactly one execution tier. It cannot prove anything ever runs that tier: a
target moved into a tier no CI job selects keeps a valid ownership label while
its coverage silently leaves the merge queue. That is how the RUE-205/RUE-204
codegen differential left required CI — `tier = "slow"` was correct, and nothing
selected the slow tier.

Tier coverage was not absent even then, because `release.yml`'s nightly sweep
runs `//...` with no tier filter and therefore happens to include every tier.
That is exactly the property this gate refuses to accept as coverage: an
unfiltered `//...` selects a tier by accident, so it keeps reporting "covered"
through the very edit that strands a tier, and it says nothing about which job
owns the tier or when it runs. Only a job that *names* the tier, or names
targets belonging to it, counts here.

What this proves:

* the tier vocabulary in `test_defs.bzl` and `test_tiers.bxl` agrees;
* every tier in that vocabulary has at least one declared CI selector, so a new
  tier fails the build until a job is made responsible for it;
* every declared selector still exists — its workflow, its job, and the literal
  selection expression it was registered for. Deleting the oracle-diff lane, or
  renaming its targets, fails here instead of going quiet.

What it does not prove: that every *target* in a tier is selected. Coverage at
that granularity belongs to each suite's own inventory gate (the RUE-924 audit
in `test.sh`, `//:cli-shard-coverage-validation`). `//:cli-tests-slow` is
deliberately nightly-only, and this gate is not the place to relitigate that.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import job_blocks

DEFS_TIER_RE = re.compile(
    r'^TEST_TIER_[A-Z]+ = "(rue_test_tier_[a-z]+)"$', re.MULTILINE
)

# test_tiers.bxl loads the vocabulary from test_defs.bzl instead of keeping
# its own copy (RUE-1523), so agreement is structural. This gate pins the
# load line itself: without it, reverting the bxl to a local list would
# silently dissolve that guarantee.
BXL_LOAD_LINE = 'load("//:test_defs.bzl", "RUE_TEST_TIER_LABELS")'


@dataclass(frozen=True)
class Selector:
    """One CI job deliberately responsible for executing a tier."""

    workflow: str
    job: str
    # Literal text that must appear inside that job block. This is the
    # registered *reason* the job selects the tier — a tier env filter, or the
    # targets it names — so an edit that removes the selection fails here.
    evidence: tuple[str, ...]
    why: str


TIER_SELECTORS: dict[str, tuple[Selector, ...]] = {
    "rue_test_tier_premerge": (
        Selector(
            workflow="ci.yml",
            job="linux-premerge",
            evidence=("RUE_TEST_TIER: premerge", "./test.sh"),
            why="the complete target-independent premerge suite on every PR",
        ),
    ),
    "rue_test_tier_slow": (
        Selector(
            workflow="ci.yml",
            job="platform-corpus",
            evidence=(
                "//crates/rue-oracle-diff:oracle-diff-test",
                "//crates/rue-oracle-diff:oracle-diff-spec-test",
            ),
            why="the RUE-205/RUE-204 codegen differential, in its own pre-merge lane",
        ),
    ),
    "rue_test_tier_stress": (
        Selector(
            workflow="release.yml",
            job="large-program",
            evidence=(
                "inputs.tier == 'stress'",
                '//:large-example-${{ matrix.program }}-stress',
            ),
            why="the manually dispatched 4x large-program stress matrix",
        ),
    ),
}


def declared_tiers(defs_path: Path, bxl_path: Path) -> tuple[set[str], list[str]]:
    """Returns the tier vocabulary and any break in its single-sourcing."""
    defs_tiers = set(DEFS_TIER_RE.findall(defs_path.read_text()))
    errors = []
    if not defs_tiers:
        errors.append(f"{defs_path}: no TEST_TIER_* constants found")
    if BXL_LOAD_LINE not in bxl_path.read_text():
        errors.append(
            f"{bxl_path.name}: does not load RUE_TEST_TIER_LABELS from "
            f"{defs_path.name}; the selector and the tier macros can drift"
        )
    return defs_tiers, errors


def validate(defs_path: Path, bxl_path: Path, workflows: dict[str, Path]) -> list[str]:
    tiers, errors = declared_tiers(defs_path, bxl_path)
    if errors:
        return errors

    registered = set(TIER_SELECTORS)
    for tier in sorted(tiers - registered):
        errors.append(
            f"{tier} has no declared CI selector: give it a named job in a "
            "workflow and register that job here. An unfiltered '//...' run is "
            "not a selector."
        )
    for tier in sorted(registered - tiers):
        errors.append(f"{tier} is registered here but is no longer a declared tier")

    for tier in sorted(registered & tiers):
        for selector in TIER_SELECTORS[tier]:
            path = workflows.get(selector.workflow)
            if path is None:
                errors.append(
                    f"{tier}: workflow {selector.workflow} was not supplied to "
                    "this validator"
                )
                continue
            try:
                jobs = job_blocks(path.read_text())
            except ValueError as error:
                errors.append(f"{selector.workflow}: {error}")
                continue
            block = jobs.get(selector.job)
            if block is None:
                errors.append(
                    f"{tier}: {selector.workflow} no longer defines job "
                    f"'{selector.job}' ({selector.why})"
                )
                continue
            for evidence in selector.evidence:
                if evidence not in block:
                    errors.append(
                        f"{tier}: {selector.workflow} job '{selector.job}' no "
                        f"longer selects it via {evidence!r} ({selector.why})"
                    )
    return errors


def parse_workflow_args(values: list[str]) -> tuple[dict[str, Path], list[str]]:
    workflows: dict[str, Path] = {}
    errors: list[str] = []
    for value in values:
        path = Path(value)
        if not path.is_file():
            errors.append(f"{value}: not a readable workflow file")
            continue
        workflows[path.name] = path
    return workflows, errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--test-defs", type=Path, required=True)
    parser.add_argument("--test-tiers-bxl", type=Path, required=True)
    parser.add_argument(
        "--workflow",
        action="append",
        default=[],
        help="path to a workflow file; registered by basename",
    )
    args = parser.parse_args()

    workflows, errors = parse_workflow_args(args.workflow)
    errors += validate(args.test_defs, args.test_tiers_bxl, workflows)
    if errors:
        for error in errors:
            print(f"error: {error}")
        return 1
    print(
        f"Rue tier CI selectors valid: {len(TIER_SELECTORS)} tiers, each "
        "deliberately selected by a named CI job"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
