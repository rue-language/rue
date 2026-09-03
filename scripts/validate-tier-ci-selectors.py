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
* every declared selector still exists — its workflow, its job, and the
  selection evidence it was registered for;
* for the tier the derived `platform-corpus` matrix owns, that the matrix is
  still derived from `scripts/affected-targets corpus-targets` and, with
  `--live-graph`, that the graph-derived inventory really contains a target
  carrying the tier (RUE-1936). The inventory is a label query, so the one
  edit that strands the slow tier — dropping `rue_ci_dedicated_lane` from the
  oracle differentials — is visible only in the graph, which is why the
  `ci-contract` job runs this live while the Buck sh_test stays structural.

What it does not prove: that every *target* in a tier is selected. Coverage at
that granularity belongs to each suite's own inventory gate (the RUE-924 audit
in `test.sh`, the shard planner's live-union assertion). `//:cli-tests-slow` is
deliberately nightly-only, and this gate is not the place to relitigate that.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import job_blocks

ROOT = Path(__file__).resolve().parent.parent
DEFS_TIER_RE = re.compile(
    r'^TEST_TIER_[A-Z]+ = "(rue_test_tier_[a-z]+)"$', re.MULTILINE
)

# test_tiers.bxl loads the vocabulary from test_defs.bzl instead of keeping
# its own copy (RUE-1523), so agreement is structural. This gate pins the
# load line itself: without it, reverting the bxl to a local list would
# silently dissolve that guarantee.
BXL_LOAD_LINE = 'load("//:test_defs.bzl", "RUE_TEST_TIER_LABELS")'

# The two commands that turn the graph into the platform-corpus matrix.
MATRIX_DERIVATION = (
    "scripts/affected-targets corpus-targets",
    "scripts/plan-cli-shards.py",
)
# Direct workflow invocations must carry both: the script input, and the
# live-graph proof only a runner with Buck can give.
REQUIRED_WORKFLOW_FLAGS = ("--affected-targets", "--live-graph")


@dataclass(frozen=True)
class Selector:
    """One CI job deliberately responsible for executing a tier."""

    workflow: str
    job: str
    # Literal text that must appear inside that job block: the registered
    # *reason* the job selects the tier.
    evidence: tuple[str, ...]
    why: str
    # The job runs the matrix derived from `scripts/affected-targets
    # corpus-targets`; whether that graph-derived inventory carries a target
    # of this tier is a live-graph question.
    derived_from_graph: bool = False


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
            evidence=("fromJSON(needs.affected-targets.outputs.corpus_matrix)",),
            why="the RUE-205/RUE-204 codegen differential, in its own pre-merge lane",
            derived_from_graph=True,
        ),
    ),
    "rue_test_tier_stress": (
        Selector(
            workflow="release.yml",
            job="large-program",
            evidence=(
                "inputs.tier == 'stress'",
                '//examples:large-example-${{ matrix.program }}-stress',
            ),
            why="the manually dispatched 4x large-program stress matrix",
        ),
    ),
}


class LiveGraph:
    """The two graph answers a derived selector needs, via the real tools."""

    def __init__(self, buck2: Path, affected_targets: Path) -> None:
        self.buck2 = buck2
        self.affected_targets = affected_targets

    @staticmethod
    def _labels(command: list[str]) -> Optional[list[str]]:
        result = subprocess.run(command, capture_output=True, text=True, check=False)
        if result.returncode != 0:
            return None
        return [
            re.sub(r" \([^()]*\)$", "", line.strip()).replace("root//", "//", 1)
            for line in result.stdout.splitlines()
            if line.strip()
        ]

    def corpus_targets(self) -> Optional[list[str]]:
        return self._labels(["bash", str(self.affected_targets), "corpus-targets"])

    def tier_targets(self, tier: str) -> Optional[list[str]]:
        return self._labels(
            [str(self.buck2), "uquery", f"attrfilter(labels, '{tier}', set(//... toolchains//...))"]
        )


def direct_invocation_errors(workflows: dict[str, Path]) -> list[str]:
    """Require every workflow invocation to pass the complete live-input set."""
    errors = []
    command = "scripts/validate-tier-ci-selectors.py"
    for workflow, path in sorted(workflows.items()):
        source = path.read_text()
        offset = 0
        while True:
            start = source.find(command, offset)
            if start < 0:
                break
            following = source[start:]
            next_step = re.search(r"\n\s+- name:", following)
            invocation = following[: next_step.start()] if next_step else following
            for flag in REQUIRED_WORKFLOW_FLAGS:
                if flag not in invocation:
                    errors.append(f"{workflow}: direct {command} invocation must pass {flag}")
            offset = start + len(command)
    return errors


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


def derived_tier_errors(tier: str, selector: Selector, live: Optional[LiveGraph]) -> list[str]:
    """The derived matrix really runs a target of `tier` (live only)."""
    if live is None:
        return []
    corpus = live.corpus_targets()
    if not corpus:
        return [
            f"{tier}: scripts/affected-targets corpus-targets is "
            + ("unavailable" if corpus is None else "empty")
            + f"; the derived matrix runs nothing ({selector.why})"
        ]
    tiered = live.tier_targets(tier)
    if tiered is None:
        return [f"{tier}: the live graph query for the tier failed"]
    if not set(corpus) & set(tiered):
        return [
            f"{tier}: no target carrying it is in scripts/affected-targets "
            f"corpus-targets, so the derived platform-corpus matrix runs none of "
            f"it ({selector.why})"
        ]
    return []


def validate(
    defs_path: Path,
    bxl_path: Path,
    workflows: dict[str, Path],
    affected_targets_path: Path,
    live: Optional[LiveGraph] = None,
) -> list[str]:
    tiers, errors = declared_tiers(defs_path, bxl_path)
    if errors:
        return errors
    errors.extend(direct_invocation_errors(workflows))
    if not affected_targets_path.is_file():
        errors.append(f"{affected_targets_path}: canonical corpus inventory script is missing")

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
            if selector.derived_from_graph:
                affected_block = jobs.get("affected-targets", "")
                for evidence in MATRIX_DERIVATION:
                    if evidence not in affected_block:
                        errors.append(
                            f"{tier}: {selector.workflow} job 'affected-targets' "
                            f"no longer derives the corpus matrix via {evidence!r} "
                            f"({selector.why})"
                        )
                errors.extend(derived_tier_errors(tier, selector, live))
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
    parser.add_argument("--affected-targets", type=Path, required=True)
    parser.add_argument(
        "--workflow",
        action="append",
        default=[],
        help="path to a workflow file; registered by basename",
    )
    parser.add_argument(
        "--live-graph",
        action="store_true",
        help="also prove from the live Buck graph that the derived matrix runs its tier",
    )
    parser.add_argument("--buck2", type=Path, default=ROOT / "buck2")
    args = parser.parse_args()

    workflows, errors = parse_workflow_args(args.workflow)
    live = LiveGraph(args.buck2, args.affected_targets) if args.live_graph else None
    errors += validate(
        args.test_defs, args.test_tiers_bxl, workflows, args.affected_targets, live
    )
    if errors:
        for error in errors:
            print(f"error: {error}")
        return 1
    print(
        f"Rue tier CI selectors valid: {len(TIER_SELECTORS)} tiers, each "
        "deliberately selected by a named CI job"
        + (" (derived matrix proved from the live graph)" if live else "")
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
