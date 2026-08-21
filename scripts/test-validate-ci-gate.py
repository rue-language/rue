#!/usr/bin/env python3
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

MODULE = load_script("validate-ci-gate.py", __file__)
SOURCE = Path(os.environ["RUE_CI_WORKFLOW"])
# Under Buck the crate source is a declared input; a direct run falls back to
# the checkout path the validator resolves on its own.
TEST_RUNNER_SOURCE = Path(
    os.environ.get("RUE_TEST_RUNNER_SOURCE", MODULE.TEST_RUNNER_SOURCE)
)
ROOT_BUCK = Path(os.environ.get("RUE_ROOT_BUCK", MODULE.ROOT_BUCK))


class GateValidatorTests(unittest.TestCase):
    def validate_text(
        self, text, native_runner=None, test_runner=None, buck=None, valgrind_install=None
    ):
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
            installer_path = Path(directory) / "install-valgrind"
            installer_path.write_text(
                valgrind_install
                if valgrind_install is not None
                else MODULE.VALGRIND_INSTALL_SCRIPT.read_text()
            )
            return MODULE.validate(
                path, runner_path, test_runner_path, buck_path, installer_path
            )

    def test_current_workflow_is_valid(self):
        self.assertEqual(
            MODULE.validate(
                SOURCE, MODULE.NATIVE_RUNNER_SCRIPT, TEST_RUNNER_SOURCE, ROOT_BUCK
            ),
            [],
        )

    def test_valgrind_cannot_return_to_inline_apt(self):
        source = SOURCE.read_text().replace(
            "run: scripts/install-valgrind",
            "run: |\n          sudo apt-get update\n          sudo apt-get install -y valgrind",
            1,
        )
        errors = "\n".join(self.validate_text(source))
        self.assertIn("must invoke scripts/install-valgrind", errors)
        self.assertIn("must not contain an inline unbounded apt-get operation", errors)

    def test_valgrind_policy_drift_fails_contract(self):
        installer = MODULE.VALGRIND_INSTALL_SCRIPT.read_text().replace(
            "APT_ACQUIRE_TIMEOUT_SECONDS=30", "APT_ACQUIRE_TIMEOUT_SECONDS=45", 1
        )
        errors = "\n".join(self.validate_text(SOURCE.read_text(), valgrind_install=installer))
        self.assertIn("30-second per-acquisition timeout", errors)

    def test_valgrind_cancellation_cleanup_is_required(self):
        installer = MODULE.VALGRIND_INSTALL_SCRIPT.read_text().replace(
            'kill -KILL -- "-$child_pid"', "# cleanup removed", 1
        )
        errors = "\n".join(self.validate_text(SOURCE.read_text(), valgrind_install=installer))
        self.assertIn("forced process-group cleanup", errors)

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

    def test_native_abi_filter_excludes_only_the_accidental_intersection(self):
        changed = SOURCE.read_text().replace(
            "scripts/rue cli abi --skip "
            "cli.differential_opt::aggregate_abi_across_opt_levels",
            "scripts/rue cli abi",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn(
            "scripts/rue cli abi --skip "
            "cli.differential_opt::aggregate_abi_across_opt_levels",
            errors,
        )

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

    # RUE-1265: the duplication gate is the only check that reads test contents
    # rather than target lists, so dropping its step restores a blind spot no
    # other gate can cover.
    def test_dropping_the_duplication_gate_step_fails(self):
        changed = SOURCE.read_text().replace(
            "scripts/validate-test-duplication.py", "true", 1
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn(
            "linux-premerge responsibility missing 'scripts/validate-test-duplication.py'",
            errors,
        )

    def test_dedicated_lane_corpus_without_a_job_fails(self):
        # spec-tests is skipped by the premerge suite because it carries the
        # label, so dropping its platform-corpus entry would drop it entirely.
        changed = SOURCE.read_text().replace("            target: //:spec-tests\n", "", 1)
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("//:spec-tests is marked rue_ci_dedicated_lane", errors)
        self.assertIn("no exactly-one dedicated owner", errors)

    def test_release_smoke_label_is_owned_by_release_job(self):
        # The label removes release-smoke from linux-premerge; its release job
        # is therefore the required dedicated owner.
        changed = SOURCE.read_text().replace(
            "run: scripts/ci-timed \"release smoke\" -- ./buck2 test //:release-smoke --target-platforms //platforms:release",
            "run: scripts/ci-timed \"release smoke\" -- ./buck2 test //:other --target-platforms //platforms:release",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("//:release-smoke is marked rue_ci_dedicated_lane", errors)
        self.assertIn("no exactly-one dedicated owner", errors)

    def test_release_smoke_cannot_be_owned_by_two_dedicated_jobs(self):
        changed = SOURCE.read_text().replace(
            "            target: //:spec-tests\n",
            "            target: //:release-smoke\n            target: //:spec-tests\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("//:release-smoke (owned by platform-corpus, release)", errors)

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


    # RUE-1504: the staleness gate moved out of the premerge lane into its own
    # job. Moving required work is only safe while the contract moves with it.
    def test_staleness_gate_without_a_deep_checkout_fails(self):
        changed = SOURCE.read_text().replace(
            "          # measurement, so it needs the measured commit in local history. The\n"
            "          # default depth-1 checkout has none of them, and a gate that cannot\n"
            "          # see the history fails rather than passing. (RUE-1258)\n"
            "          fetch-depth: 0\n",
            "",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("performance-staleness responsibility missing 'fetch-depth: 0'", errors)

    def test_dropping_the_staleness_step_fails(self):
        changed = SOURCE.read_text().replace(
            "          scripts/validate-performance-stall.py \\\n", "", 1
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn(
            "performance-staleness responsibility missing "
            "'scripts/validate-performance-stall.py'",
            errors,
        )

    def test_returning_the_staleness_gate_to_the_premerge_lane_fails(self):
        changed = SOURCE.read_text().replace(
            "        run: scripts/ci-timed \"gazette goldens\" -- scripts/gazette-corpus-diff.py golden\n",
            "        run: scripts/ci-timed \"gazette goldens\" -- scripts/gazette-corpus-diff.py golden\n"
            "      - name: Check the performance series is still advancing\n"
            "        run: scripts/validate-performance-stall.py --data d --repo . --ref origin/trunk\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("the staleness gate belongs to performance-staleness", errors)

    def test_runtime_series_cannot_be_folded_into_the_staleness_gate(self):
        # ADR-0072 Decision 9. A stalled runtime series is a triage item, not a
        # repository-wide block, and the difference is one flag.
        changed = SOURCE.read_text().replace(
            "            --manifest performance/manifest.toml \\\n"
            "            --data-root \"$DATA\" \\\n",
            "            --manifest performance/manifest.toml \\\n"
            "            --runtime-manifest performance/runtime.toml \\\n"
            "            --data-root \"$DATA\" \\\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("must stay compile-time only", errors)

    def test_continue_on_error_voids_the_staleness_gate(self):
        # The gate has no bypass, and `continue-on-error` is a one-line bypass
        # that leaves the required check reporting success.
        for splice in (
            "  performance-staleness:\n    runs-on: ubuntu-latest\n"
            "    continue-on-error: true\n",
            "  performance-staleness:\n    runs-on: ubuntu-latest\n"
            "    steps:\n      - continue-on-error: true\n",
        ):
            changed = SOURCE.read_text().replace(
                "  performance-staleness:\n    runs-on: ubuntu-latest\n", splice, 1
            )
            self.assertNotEqual(
                changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml"
            )
            errors = "\n".join(self.validate_text(changed))
            self.assertIn(
                "performance-staleness must not use continue-on-error", errors
            )

    def test_gating_the_staleness_job_on_another_fails(self):
        changed = SOURCE.read_text().replace(
            "  performance-staleness:\n    runs-on: ubuntu-latest\n",
            "  performance-staleness:\n    runs-on: ubuntu-latest\n"
            "    needs:\n      - affected-targets\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("performance-staleness must not depend on another CI job", errors)

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
        # Anchored on the array's LAST target so the splice keeps working as
        # entries are appended; the inequality assertion turns a stale anchor
        # into a clear failure here rather than a silent no-op that lets the
        # drift assertion below fail with an empty error list (RUE-1404 hit
        # exactly that when the fixture target joined the array).
        changed = SOURCE.read_text().replace(
            "            //fixtures/rue-program:hello-runs-test\n          )",
            "            //fixtures/rue-program:hello-runs-test\n"
            "            //crates/rue-query:rue-query-test\n          )",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
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
