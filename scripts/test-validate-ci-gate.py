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


    def test_undeclared_need_output_fails_closed(self):
        # RUE-1130 regression. GitHub resolves an undeclared job output to the
        # empty string instead of failing, so a lane gate reading it would see
        # "nothing selected" and deselect every lane. That is invisible on any
        # PR touching CI, because those force a full run — i.e. invisible on
        # exactly the PRs that would be used to test the feature.
        changed = SOURCE.read_text().replace(
            "      selected_lanes: ${{ steps.decide.outputs.selected_lanes }}\n", "", 1
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("needs.affected-targets.outputs.selected_lanes is referenced", errors)
        self.assertIn("silently resolve to the empty string", errors)

    def test_declared_need_outputs_pass(self):
        self.assertEqual(self.validate_text(SOURCE.read_text()), [])

    def test_need_output_from_unknown_job_fails(self):
        self.assertIn(
            "references unknown job",
            "\n".join(
                MODULE.undeclared_need_outputs(
                    "${{ needs.ghost.outputs.thing }}", {"real": "    outputs:\n      thing: x\n"}
                )
            ),
        )

    def test_declared_outputs_parses_past_comments(self):
        # The declaration this guard protects carries an explanatory comment;
        # the parser must not stop at it and report the output as undeclared.
        block = (
            "    outputs:\n"
            "      full: ${{ steps.decide.outputs.full }}\n"
            "      # why this exists\n"
            "      selected_lanes: ${{ steps.decide.outputs.selected_lanes }}\n"
        )
        self.assertEqual(MODULE.declared_outputs(block), {"full", "selected_lanes"})


    def test_lane_target_drift_fails_closed(self):
        # RUE-1130. A target the native job runs but the determinator does not
        # list is invisible to selection: the lane can be deselected, or
        # narrowed away, by a diff that actually reaches it.
        changed = SOURCE.read_text().replace(
            "            //crates/rue-target:rue-target-test\n          )",
            "            //crates/rue-target:rue-target-test\n"
            "            //crates/rue-query:rue-query-test\n          )",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("//crates/rue-query:rue-query-test", errors)
        self.assertIn("selection cannot see it", errors)

    def test_lane_targets_and_job_agree_today(self):
        self.assertEqual(MODULE.lane_target_drift(SOURCE.read_text()), [])

    def test_unreadable_lane_script_fails_closed(self):
        # The gate must not pass silently when it cannot read the lane list.
        self.assertIn(
            "produced nothing",
            "\n".join(
                MODULE.lane_target_drift(SOURCE.read_text(), script=Path("/nonexistent/affected-targets"))
            ),
        )


if __name__ == "__main__":
    unittest.main()
