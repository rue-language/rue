#!/usr/bin/env python3
"""Focused tests for the tier CI-selector gate (RUE-1117)."""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

selectors = load_script("validate-tier-ci-selectors.py", __file__)

TIERS = ("premerge", "slow", "stress")

CI_WORKFLOW = """name: CI
on:
  pull_request:
jobs:
  affected-targets:
    outputs:
      corpus_matrix: ${{ steps.cli-plan.outputs.corpus_matrix }}
    steps:
      - run: scripts/affected-targets corpus-targets
      - run: scripts/plan-cli-shards.py

  linux-premerge:
    steps:
      - name: Run complete target-independent premerge suite
        env:
          RUE_TEST_TIER: premerge
        run: ./test.sh

  platform-corpus:
    strategy:
      matrix: ${{ fromJSON(needs.affected-targets.outputs.corpus_matrix) }}

  ci-contract:
    steps:
      - run: >-
          scripts/validate-tier-ci-selectors.py
          --test-defs test_defs.bzl
          --test-tiers-bxl test_tiers.bxl
          --affected-targets scripts/affected-targets
          --workflow .github/workflows/ci.yml
          --live-graph
"""

RELEASE_WORKFLOW = """name: Release
on:
  schedule:
    - cron: '0 6 * * *'
jobs:
  release-suite:
    steps:
      - name: Run full release suite
        run: ./buck2 test //... toolchains//...

  large-program:
    steps:
      - name: Run manual stress program
        if: github.event_name == 'workflow_dispatch' && inputs.tier == 'stress'
        run: ./buck2 test "//examples:large-example-${{ matrix.program }}-stress"
"""

CORPUS = ("//:cli-tests-shard-0", "//crates/rue-oracle-diff:oracle-diff-test")
SLOW = ("//:cli-tests-slow", "//crates/rue-oracle-diff:oracle-diff-test")


def defs_source(tiers: tuple[str, ...]) -> str:
    return "".join(
        f'TEST_TIER_{tier.upper()} = "rue_test_tier_{tier}"\n' for tier in tiers
    )


def bxl_source(tiers: tuple[str, ...] = ()) -> str:
    del tiers  # the vocabulary is loaded, not spelled, since RUE-1523
    return (
        'load("//:test_defs.bzl", "RUE_TEST_TIER_LABELS")\n'
        "_TEST_TIER_LABELS = RUE_TEST_TIER_LABELS\n"
    )


def fake_tool(path: Path, body: str) -> Path:
    path.write_text("#!/usr/bin/env bash\n" + body)
    path.chmod(0o755)
    return path


class TierCiSelectorTests(unittest.TestCase):
    def validate(
        self,
        *,
        defs: str | None = None,
        bxl: str | None = None,
        ci: str | None = None,
        release: str | None = None,
        omit_release: bool = False,
        corpus: tuple[str, ...] | None = None,
        slow: tuple[str, ...] | None = None,
        buck_fails: bool = False,
        live: bool = False,
    ) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            defs_path = root / "test_defs.bzl"
            bxl_path = root / "test_tiers.bxl"
            ci_path = root / "ci.yml"
            release_path = root / "release.yml"
            defs_path.write_text(defs if defs is not None else defs_source(TIERS))
            bxl_path.write_text(bxl if bxl is not None else bxl_source(TIERS))
            ci_path.write_text(ci if ci is not None else CI_WORKFLOW)
            release_path.write_text(
                release if release is not None else RELEASE_WORKFLOW
            )
            corpus_lines = " ".join(corpus if corpus is not None else CORPUS)
            affected = fake_tool(
                root / "affected-targets",
                f'[ "$1" = corpus-targets ] || exit 2\n'
                f'for t in {corpus_lines}; do echo "$t"; done\n',
            )
            slow_lines = " ".join(f"root{t}" for t in (slow if slow is not None else SLOW))
            buck2 = fake_tool(
                root / "buck2",
                ("exit 1\n" if buck_fails else "")
                + f'case "$2" in *rue_test_tier_slow*) for t in {slow_lines}; do echo "$t"; done ;; *) exit 1 ;; esac\n',
            )
            workflows = {"ci.yml": ci_path}
            if not omit_release:
                workflows["release.yml"] = release_path
            graph = selectors.LiveGraph(buck2, affected) if live else None
            return selectors.validate(defs_path, bxl_path, workflows, affected, graph)

    def test_registered_selectors_pass(self) -> None:
        self.assertEqual(self.validate(), [])
        self.assertEqual(self.validate(live=True), [])

    def test_repository_state_is_valid(self) -> None:
        # The gate must pass against the real files it ships to guard, not only
        # against fixtures. Under Buck the declared input filegroup provides
        # them at their repository-relative paths; the live half needs Buck
        # and is exercised by the ci-contract job.
        root = Path(
            os.environ.get(
                "RUE_TIER_VALIDATION_ROOT", Path(__file__).resolve().parent.parent
            )
        )
        errors = selectors.validate(
            root / "test_defs.bzl",
            root / "test_tiers.bxl",
            {
                "ci.yml": root / ".github/workflows/ci.yml",
                "release.yml": root / ".github/workflows/release.yml",
            },
            root / "scripts/affected-targets",
        )
        self.assertEqual(errors, [])

    def test_slow_tier_absent_from_the_derived_inventory_fails_live(self) -> None:
        # The RUE-1117 regression in its graph-derived form: the differential
        # keeps `tier = "slow"` but loses the CI-owner label, so the derived
        # matrix stops running it while every structural check stays green.
        errors = self.validate(live=True, corpus=("//:cli-tests-shard-0", "//:spec-tests"))
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("rue_test_tier_slow", errors[0])
        self.assertIn("derived platform-corpus matrix runs none of it", errors[0])

    def test_unavailable_or_empty_inventory_fails_live(self) -> None:
        errors = "\n".join(self.validate(live=True, corpus=()))
        self.assertIn("corpus-targets is empty", errors)
        errors = "\n".join(self.validate(live=True, buck_fails=True))
        self.assertIn("live graph query for the tier failed", errors)

    def test_bypassing_canonical_matrix_planner_fails(self) -> None:
        errors = self.validate(
            ci=CI_WORKFLOW.replace("scripts/plan-cli-shards.py", "echo static")
        )
        self.assertTrue(
            any("no longer derives the corpus matrix" in error for error in errors),
            errors,
        )

    def test_direct_workflow_invocation_requires_both_flags(self) -> None:
        for flag in ("--affected-targets scripts/affected-targets", "--live-graph"):
            errors = self.validate(ci=CI_WORKFLOW.replace(f"          {flag}\n", "", 1))
            self.assertTrue(
                any(f"must pass {flag.split()[0]}" in error for error in errors),
                errors,
            )

    def test_renamed_job_fails(self) -> None:
        errors = self.validate(ci=CI_WORKFLOW.replace("platform-corpus:", "corpus:"))
        self.assertTrue(
            any("no longer defines job 'platform-corpus'" in error for error in errors),
            errors,
        )

    def test_dropped_premerge_tier_filter_fails(self) -> None:
        errors = self.validate(ci=CI_WORKFLOW.replace("RUE_TEST_TIER: premerge", ""))
        self.assertTrue(
            any("rue_test_tier_premerge" in error for error in errors), errors
        )

    def test_unfiltered_release_sweep_is_not_a_selector(self) -> None:
        # release.yml runs `//...` with no tier filter, which executes every
        # tier by accident. Removing every *named* stress selection must fail
        # even though that sweep still runs the stress targets.
        release = RELEASE_WORKFLOW.replace(
            "      - name: Run manual stress program\n"
            "        if: github.event_name == 'workflow_dispatch'"
            " && inputs.tier == 'stress'\n"
            '        run: ./buck2 test "//examples:large-example-${{ matrix.program }}-stress"\n',
            "",
        )
        self.assertIn("//... toolchains//...", release)
        errors = self.validate(release=release)
        self.assertTrue(
            any("rue_test_tier_stress" in error for error in errors), errors
        )

    def test_new_tier_without_a_selector_fails(self) -> None:
        extended = TIERS + ("nightly",)
        errors = self.validate(defs=defs_source(extended), bxl=bxl_source(extended))
        self.assertTrue(
            any(
                "rue_test_tier_nightly has no declared CI selector" in error
                for error in errors
            ),
            errors,
        )

    def test_removed_tier_leaves_a_stale_registration(self) -> None:
        reduced = ("premerge", "slow")
        errors = self.validate(defs=defs_source(reduced), bxl=bxl_source(reduced))
        self.assertTrue(
            any(
                "rue_test_tier_stress is registered here but is no longer a "
                "declared tier" in error
                for error in errors
            ),
            errors,
        )

    def test_bxl_dropping_the_vocabulary_load_fails(self) -> None:
        local_list = '_TEST_TIER_LABELS = [\n    "rue_test_tier_premerge",\n]\n'
        errors = self.validate(bxl=local_list)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("does not load RUE_TEST_TIER_LABELS", errors[0])

    def test_missing_workflow_fails_closed(self) -> None:
        errors = self.validate(omit_release=True)
        self.assertTrue(
            any("was not supplied to this validator" in error for error in errors),
            errors,
        )

    def test_unparseable_workflow_fails_closed(self) -> None:
        errors = self.validate(ci="name: CI\n")
        self.assertTrue(
            any("no top-level jobs mapping" in error for error in errors), errors
        )

    def test_unreadable_workflow_argument_is_reported(self) -> None:
        workflows, errors = selectors.parse_workflow_args(["/nonexistent/ci.yml"])
        self.assertEqual(workflows, {})
        self.assertEqual(len(errors), 1)
        self.assertIn("not a readable workflow file", errors[0])


if __name__ == "__main__":
    unittest.main()
