#!/usr/bin/env python3
"""Focused tests for the tier CI-selector gate (RUE-1117)."""

from __future__ import annotations

import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("validate-tier-ci-selectors.py")
SPEC = importlib.util.spec_from_file_location("tier_ci_selectors", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
selectors = importlib.util.module_from_spec(SPEC)
# `@dataclass` resolves annotations through `sys.modules[cls.__module__]`, so a
# file loaded by spec alone must be registered before it executes.
sys.modules[SPEC.name] = selectors
SPEC.loader.exec_module(selectors)

TIERS = ("premerge", "slow", "stress")

CI_WORKFLOW = """name: CI
on:
  pull_request:
jobs:
  linux-premerge:
    steps:
      - name: Run complete target-independent premerge suite
        env:
          RUE_TEST_TIER: premerge
        run: ./test.sh

  platform-corpus:
    strategy:
      matrix:
        include:
          - target: //:spec-tests
            check_name: linux-x64-spec
          - target: //crates/rue-oracle-diff:oracle-diff-test
            check_name: linux-x64-oracle-diff
          - target: //crates/rue-oracle-diff:oracle-diff-spec-test
            check_name: linux-x64-oracle-diff-spec
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
        run: ./buck2 test "//:large-example-${{ matrix.program }}-stress"
"""


def defs_source(tiers: tuple[str, ...]) -> str:
    return "".join(
        f'TEST_TIER_{tier.upper()} = "rue_test_tier_{tier}"\n' for tier in tiers
    )


def bxl_source(tiers: tuple[str, ...]) -> str:
    body = "".join(f'    "rue_test_tier_{tier}",\n' for tier in tiers)
    return f"_TEST_TIER_LABELS = [\n{body}]\n"


class TierCiSelectorTests(unittest.TestCase):
    def validate(
        self,
        *,
        defs: str | None = None,
        bxl: str | None = None,
        ci: str | None = None,
        release: str | None = None,
        omit_release: bool = False,
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
            workflows = {"ci.yml": ci_path}
            if not omit_release:
                workflows["release.yml"] = release_path
            return selectors.validate(defs_path, bxl_path, workflows)

    def test_registered_selectors_pass(self) -> None:
        self.assertEqual(self.validate(), [])

    def test_repository_state_is_valid(self) -> None:
        # The gate must pass against the real files it ships to guard, not only
        # against fixtures. Under Buck the declared input filegroup provides
        # them at their repository-relative paths.
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
        )
        self.assertEqual(errors, [])

    def test_deleted_oracle_lane_fails(self) -> None:
        # The RUE-1117 regression itself: the differential leaves required CI
        # while its `tier = "slow"` ownership stays perfectly valid.
        stripped = "\n".join(
            line
            for line in CI_WORKFLOW.splitlines()
            if "oracle-diff" not in line
        ) + "\n"
        errors = self.validate(ci=stripped)
        self.assertEqual(len(errors), 2, errors)
        for error in errors:
            self.assertIn("rue_test_tier_slow", error)
            self.assertIn("no longer selects it", error)

    def test_renamed_job_fails(self) -> None:
        errors = self.validate(ci=CI_WORKFLOW.replace("platform-corpus:", "corpus:"))
        self.assertTrue(
            any(
                "no longer defines job 'platform-corpus'" in error
                for error in errors
            ),
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
            '        run: ./buck2 test "//:large-example-${{ matrix.program }}-stress"\n',
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

    def test_tier_vocabulary_drift_fails(self) -> None:
        errors = self.validate(bxl=bxl_source(("premerge", "slow")))
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("tier vocabulary drift", errors[0])

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
