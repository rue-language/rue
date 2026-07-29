#!/usr/bin/env python3
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("validate-ci-gate.py")
SPEC = importlib.util.spec_from_file_location("validate_ci_gate", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
SOURCE = Path(os.environ["RUE_CI_WORKFLOW"])
# Under Buck the crate source is a declared input; a direct run falls back to
# the checkout path the validator resolves on its own.
TEST_RUNNER_SOURCE = Path(
    os.environ.get("RUE_TEST_RUNNER_SOURCE", MODULE.TEST_RUNNER_SOURCE)
)
ROOT_BUCK = Path(os.environ.get("RUE_ROOT_BUCK", MODULE.ROOT_BUCK))


class GateValidatorTests(unittest.TestCase):
    def validate_text(self, text, native_runner=None, test_runner=None, buck=None):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ci.yml"
            path.write_text(text)
            runner_path = Path(directory) / "run-native-platform-corpus.sh"
            runner_path.write_text(
                native_runner
                if native_runner is not None
                else MODULE.NATIVE_RUNNER_SCRIPT.read_text()
            )
            test_runner_path = Path(directory) / "lib.rs"
            test_runner_path.write_text(
                test_runner
                if test_runner is not None
                else TEST_RUNNER_SOURCE.read_text()
            )
            buck_path = Path(directory) / "BUCK"
            buck_path.write_text(
                buck if buck is not None else ROOT_BUCK.read_text()
            )
            return MODULE.validate(path, runner_path, test_runner_path, buck_path)

    def test_current_workflow_is_valid(self):
        self.assertEqual(
            MODULE.validate(
                SOURCE, MODULE.NATIVE_RUNNER_SCRIPT, TEST_RUNNER_SOURCE, ROOT_BUCK
            ),
            [],
        )

    def test_removing_or_renaming_job_fails_inventory(self):
        source = SOURCE.read_text()
        removed = source.replace("\n  asan:\n", "\n  removed-asan:\n", 1)
        errors = "\n".join(self.validate_text(removed))
        self.assertIn("CI job inventory missing: asan", errors)
        self.assertIn("unaggregated jobs: removed-asan", errors)

    def test_actions_compatible_underscore_job_is_not_invisible(self):
        source = SOURCE.read_text()
        changed = source + "\n  unaggregated_job:\n    runs-on: ubuntu-latest\n"
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("unaggregated jobs: unaggregated_job", errors)

    def test_omitting_dependency_from_gate_fails(self):
        source = SOURCE.read_text()
        changed = source.replace("      - valgrind\n", "", 1)
        self.assertIn("ci-success needs drift", "\n".join(self.validate_text(changed)))

    def test_native_selector_cannot_silently_return_to_named_filters(self):
        source = SOURCE.read_text()
        runner = MODULE.NATIVE_RUNNER_SCRIPT.read_text().replace(
            "export RUE_PLATFORM_CASE_SELECTION=native",
            "export RUE_PLATFORM_CASE_SELECTION=all",
            1,
        )
        errors = "\n".join(self.validate_text(source, runner))
        self.assertIn("export RUE_PLATFORM_CASE_SELECTION=native", errors)

    def test_unexpected_skip_policy_and_platform_drift_fail(self):
        source = SOURCE.read_text()
        changed = source.replace(
            "if: github.event_name == 'merge_group'",
            "if: github.event_name != 'pull_request'",
            1,
        )
        self.assertIn("merge-group-only", "\n".join(self.validate_text(changed)))

        changed = source.replace("check_name: linux-x64-spec", "check_name: macos-spec", 1)
        self.assertIn("platform-corpus responsibility drift", "\n".join(
            self.validate_text(changed)
        ))

    def test_declared_platform_matrix_matches_the_harness(self):
        self.assertEqual(
            sorted(MODULE.ci_executed_targets(TEST_RUNNER_SOURCE.read_text())),
            sorted(MODULE.PLATFORM_LANES),
        )

    def test_platform_declared_ci_executed_without_a_lane_fails(self):
        runner = TEST_RUNNER_SOURCE.read_text().replace(
            '"aarch64-macos"]', '"aarch64-macos", "riscv64-linux"]', 1
        )
        errors = "\n".join(self.validate_text(SOURCE.read_text(), test_runner=runner))
        self.assertIn("CI_EXECUTED_TARGETS drift", errors)
        self.assertIn("riscv64-linux", errors)

    def test_dropping_a_native_lane_fails_the_platform_matrix(self):
        changed = SOURCE.read_text().replace("os: macos-15", "os: macos-14", 1)
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("aarch64-macos is declared CI-executed", errors)

    def test_unreadable_platform_matrix_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            missing = Path(directory) / "absent.rs"
            errors = MODULE.validate(
                SOURCE, MODULE.NATIVE_RUNNER_SCRIPT, missing, ROOT_BUCK
            )
        self.assertTrue(
            any("platform responsibility matrix unreadable" in error for error in errors),
            errors,
        )


    # RUE-1163: the label that replaced RUE_CI_DEFER_HEAVY_SUITES.
    def test_reintroducing_the_defer_protocol_fails(self):
        changed = SOURCE.read_text().replace(
            "          RUE_TEST_TIER: premerge\n",
            "          RUE_TEST_TIER: premerge\n"
            "          RUE_CI_DEFER_HEAVY_SUITES: '//:cli-tests'\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("RUE_CI_DEFER_HEAVY_SUITES is retired", errors)

    def test_dedicated_lane_corpus_without_a_job_fails(self):
        # spec-tests is skipped by the premerge suite because it carries the
        # label, so dropping its platform-corpus entry would drop it entirely.
        changed = SOURCE.read_text().replace("            target: //:spec-tests\n", "", 1)
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("//:spec-tests is marked rue_ci_dedicated_lane", errors)

    def test_unlabeled_buck_fails_closed(self):
        buck = ROOT_BUCK.read_text().replace('"rue_ci_dedicated_lane"', '"unused"')
        errors = "\n".join(self.validate_text(SOURCE.read_text(), buck=buck))
        self.assertIn("no corpus carries rue_ci_dedicated_lane", errors)

    def test_sharded_corpus_counts_as_covered(self):
        # //:cli-tests is labeled but never appears in the matrix by name; its
        # four shards are what run it, and that must satisfy the check.
        self.assertEqual(
            MODULE.uncovered_dedicated_lanes(
                'name = "cli-tests",\n    labels = ["rue_ci_dedicated_lane"]',
                "target: //:cli-tests-shard-0\ntarget: //:cli-tests-shard-1\n",
            ),
            [],
        )


if __name__ == "__main__":
    unittest.main()
