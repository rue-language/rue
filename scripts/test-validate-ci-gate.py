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

    def test_performance_pin_build_is_visible_and_timed(self):
        changed = SOURCE.read_text().replace(
            '          scripts/ci-timed "rue-bench build" -- ./buck2 build //crates/rue-bench:rue-bench\n',
            "          # build moved to an untracked location\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("no executable ci-timed rue-bench build", errors)

    def test_performance_pin_build_must_precede_warm_capture(self):
        source = SOURCE.read_text()
        visible = (
            '          scripts/ci-timed "rue-bench build" -- ./buck2 build '
            '//crates/rue-bench:rue-bench\n'
        )
        warm = (
            '          BENCH="$(./buck2 build //crates/rue-bench:rue-bench '
            '--show-simple-output 2>/dev/null | tail -1)"\n'
        )
        changed = source.replace(visible + warm, warm + visible, 1)
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("must precede the warm path-only capture", errors)

    def test_performance_pin_build_cannot_follow_a_true_or_continuation(self):
        source = SOURCE.read_text()
        visible = (
            '          scripts/ci-timed "rue-bench build" -- ./buck2 build '
            '//crates/rue-bench:rue-bench\n'
        )
        changed = source.replace(
            visible,
            "          true || \\\n"
            + visible,
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exact straight-line command sequence", errors)

    def test_performance_pin_build_cannot_follow_a_bang_continuation(self):
        source = SOURCE.read_text()
        visible = (
            '          scripts/ci-timed "rue-bench build" -- ./buck2 build '
            '//crates/rue-bench:rue-bench\n'
        )
        changed = source.replace(
            visible,
            "          ! \\\n"
            + visible,
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exact straight-line command sequence", errors)

    def test_performance_pin_rejects_adjacent_hash_on_visible_target(self):
        source = SOURCE.read_text()
        changed = source.replace(
            "//crates/rue-bench:rue-bench\n",
            "//crates/rue-bench:rue-bench# || true\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("no executable ci-timed rue-bench build", errors)

    def test_performance_pin_rejects_adjacent_hash_on_final_argument(self):
        source = SOURCE.read_text()
        changed = source.replace(
            '          --compiler "$RUE"\n',
            '          --compiler "$RUE"# || true\n',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exact straight-line command sequence", errors)

    def test_performance_pin_rejects_a_gap_inside_the_backslash_chain(self):
        source = SOURCE.read_text()
        changed = source.replace(
            '          "$BENCH" check-pins \\\n',
            '          "$BENCH" check-pins \\\n\n',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exact straight-line command sequence", errors)

    def test_performance_pin_rejects_spaces_after_a_backslash(self):
        source = SOURCE.read_text()
        changed = source.replace(
            '          "$BENCH" check-pins \\\n',
            '          "$BENCH" check-pins \\  \n',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exact straight-line command sequence", errors)

    def test_performance_pin_build_cannot_be_disabled_by_a_branch(self):
        source = SOURCE.read_text()
        visible = (
            '          scripts/ci-timed "rue-bench build" -- ./buck2 build '
            '//crates/rue-bench:rue-bench\n'
        )
        changed = source.replace(
            visible,
            "          if false; then\n"
            + visible
            + "          fi\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("without an intervening command or control line", errors)

    def test_performance_pin_build_cannot_be_hidden_in_a_heredoc(self):
        source = SOURCE.read_text()
        visible = (
            '          scripts/ci-timed "rue-bench build" -- ./buck2 build '
            '//crates/rue-bench:rue-bench\n'
        )
        changed = source.replace(
            visible,
            "          cat <<'RUE_BUILD'\n"
            + visible
            + "          RUE_BUILD\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("without an intervening command or control line", errors)

    def test_performance_pin_step_cannot_be_conditionally_disabled(self):
        source = SOURCE.read_text()
        marker = "      - name: Check the performance pins still match the tree\n"
        changed = source.replace(
            marker,
            marker + "        if: ${{ false }}\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("disabling or custom execution metadata", errors)

    def test_performance_pin_step_cannot_ignore_failure(self):
        source = SOURCE.read_text()
        marker = "      - name: Check the performance pins still match the tree\n"
        changed = source.replace(
            marker,
            marker + "        continue-on-error: true\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("disabling or custom execution metadata", errors)

    def test_performance_pin_step_cannot_use_custom_shell_execution(self):
        source = SOURCE.read_text()
        marker = "      - name: Check the performance pins still match the tree\n"
        changed = source.replace(
            marker,
            marker + "        shell: true {0}\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("disabling or custom execution metadata", errors)

    def test_performance_pin_step_rejects_quoted_metadata_keys(self):
        source = SOURCE.read_text()
        marker = "      - name: Check the performance pins still match the tree\n"
        changed = source.replace(
            marker,
            marker + "        'if': false\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("only comments or blanks may precede", errors)

    def test_linux_premerge_cannot_ignore_performance_pin_failure(self):
        source = SOURCE.read_text()
        marker = "  linux-premerge:\n    runs-on: ubuntu-latest\n"
        changed = source.replace(
            marker,
            marker + "    continue-on-error: true\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("job-level continue-on-error", errors)

        changed = source.replace(
            marker,
            marker + "    'continue-on-error': true\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("job-level continue-on-error", errors)

    def test_linux_premerge_execution_condition_is_pinned(self):
        source = SOURCE.read_text()
        marker = (
            "  linux-premerge:\n"
            "    runs-on: ubuntu-latest\n"
            "    name: premerge (linux-x64)\n"
            "    if: ${{ always() }}\n"
        )
        changed = source.replace(
            marker,
            marker.replace("if: ${{ always() }}", "if: ${{ false }}"),
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("must contain exactly one direct job if", errors)

    def test_linux_premerge_cannot_override_job_shell_defaults(self):
        source = SOURCE.read_text()
        marker = "  linux-premerge:\n    runs-on: ubuntu-latest\n"
        changed = source.replace(
            marker,
            marker
            + "    defaults:\n"
            + "      run:\n"
            + "        shell: true {0}\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("defaults overrides", errors)

    def test_workflow_cannot_override_run_shell_defaults(self):
        source = SOURCE.read_text()
        changed = source.replace(
            "jobs:\n",
            'defaults: {run: {shell: "true {0}"}}\n\n'
            "jobs:\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("workflow must not define top-level defaults", errors)

    def test_performance_pin_failure_uploader_cannot_be_deleted(self):
        source = SOURCE.read_text()
        uploader = (
            "      - name: Upload failing-suite output\n"
            "        if: failure()\n"
            "        uses: actions/upload-artifact@v6\n"
            "        with:\n"
            "          name: premerge-linux-x64-failure-logs\n"
            "          path: ${{ runner.temp }}/rue-ci-failed-logs\n"
            "          if-no-files-found: ignore\n"
        )
        changed = source.replace(uploader, "", 1)
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("failure-artifact uploader", errors)

    def test_performance_pin_failure_uploader_must_follow_pin_step(self):
        source = SOURCE.read_text()
        uploader = (
            "      - name: Upload failing-suite output\n"
            "        if: failure()\n"
            "        uses: actions/upload-artifact@v6\n"
            "        with:\n"
            "          name: premerge-linux-x64-failure-logs\n"
            "          path: ${{ runner.temp }}/rue-ci-failed-logs\n"
            "          if-no-files-found: ignore\n"
        )
        marker = "      - name: Check the performance pins still match the tree\n"
        changed = source.replace(uploader, "", 1).replace(
            marker,
            uploader + marker,
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("must upload failing-suite output after", errors)

    def test_performance_pin_failure_uploader_condition_is_pinned(self):
        source = SOURCE.read_text()
        changed = source.replace(
            "      - name: Upload failing-suite output\n"
            "        if: failure()\n",
            "      - name: Upload failing-suite output\n"
            "        if: always()\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exact raw metadata mapping", errors)

    def test_performance_pin_failure_uploader_path_is_pinned(self):
        source = SOURCE.read_text()
        changed = source.replace(
            "          path: ${{ runner.temp }}/rue-ci-failed-logs\n",
            "          path: ${{ runner.temp }}/other-logs\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("runner.temp", errors)

    def test_performance_pin_failure_uploader_rejects_extra_metadata(self):
        source = SOURCE.read_text()
        marker = (
            "      - name: Upload failing-suite output\n"
            "        if: failure()\n"
            "        uses: actions/upload-artifact@v6\n"
        )
        for extra in ("        continue-on-error: true\n", "        if: failure()\n"):
            changed = source.replace(
                marker,
                marker.replace(
                    "        uses: actions/upload-artifact@v6\n", extra
                    + "        uses: actions/upload-artifact@v6\n"
                ),
                1,
            )
            errors = "\n".join(self.validate_text(changed))
            self.assertIn("exact raw metadata mapping", errors)

    def test_performance_pin_cannot_exit_after_path_capture(self):
        source = SOURCE.read_text()
        warm = (
            '          BENCH="$(./buck2 build //crates/rue-bench:rue-bench '
            '--show-simple-output 2>/dev/null | tail -1)"\n'
        )
        changed = source.replace(warm, warm + "          exit 0\n", 1)
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exact straight-line command sequence", errors)

    def test_performance_pin_step_cannot_ignore_failure_after_run_block(self):
        source = SOURCE.read_text()
        changed = source.replace(
            '          --compiler "$RUE"\n',
            '          --compiler "$RUE"\n'
            "        continue-on-error: true\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("disabling or custom execution metadata", errors)

    def test_performance_pin_cannot_ignore_check_pins_failure(self):
        source = SOURCE.read_text()
        changed = source.replace(
            '          "$BENCH" check-pins \\\n',
            '          # "$BENCH" check-pins \\\n',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exact straight-line command sequence", errors)

    def test_performance_pin_cannot_ignore_check_pins_with_or_true(self):
        source = SOURCE.read_text()
        changed = source.replace(
            '          --compiler "$RUE"\n',
            '          --compiler "$RUE" || true\n',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exact straight-line command sequence", errors)

    def test_performance_pin_build_cannot_be_satisfied_by_comment_or_other_step(self):
        source = SOURCE.read_text()
        visible = (
            '          scripts/ci-timed "rue-bench build" -- ./buck2 build '
            '//crates/rue-bench:rue-bench\n'
        )
        changed = source.replace(visible, "", 1).replace(
            "          # Would this change stop runs entering their series? Decidable from this\n",
            "          # " + visible.strip() + "\n"
            "          # Would this change stop runs entering their series? Decidable from this\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("no executable ci-timed rue-bench build", errors)

        changed = source.replace(visible, "", 1).replace(
            "      - name: Check the performance pins still match the tree\n",
            "      - name: Unrelated rue-bench build\n"
            "        run: " + visible + "\n"
            "      - name: Check the performance pins still match the tree\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("no executable ci-timed rue-bench build", errors)

    def test_performance_pin_build_must_target_rue_bench_exactly(self):
        changed = SOURCE.read_text().replace(
            "//crates/rue-bench:rue-bench\n",
            "//crates/rue:rue\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("targeting //crates/rue-bench:rue-bench", errors)

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

    def test_future_narrowing_consumer_must_be_registered(self):
        changed = SOURCE.read_text() + (
            "\n  future-narrowed-lane:\n"
            "    runs-on: ubuntu-latest\n"
            "    needs: affected-targets\n"
            "    steps:\n"
            "      - run: echo ${{ needs.affected-targets.outputs.impacted }}\n"
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("future-narrowed-lane consumes impacted narrowing", errors)

    def test_direct_scope_operation_cannot_bypass_registry(self):
        changed = SOURCE.read_text().replace(
            "scripts/affected-targets narrow-scope linux-premerge-build \"$NARROW_FILE\"",
            "scripts/affected-targets intersect \"$NARROW_FILE\" \"${targets[@]}\"",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("must use the registry-backed narrow-scope command", errors)

    def test_second_raw_impacted_consumer_fails_closed(self):
        changed = SOURCE.read_text().replace(
            "RUE_TEST_TARGETS_FILE: ${{ steps.narrow.outputs.test_file }}",
            'RUE_TEST_TARGETS_FILE: "$RUE_AFFECTED_IMPACTED"',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("raw impacted-closure consumer", errors)
        self.assertIn("registry-intersected target file", errors)

    def test_second_raw_impacted_file_consumer_fails_closed(self):
        changed = SOURCE.read_text().replace(
            "NARROW_FILE: ${{ steps.narrow.outputs.file }}",
            "NARROW_FILE: ${{ steps.narrow.outputs.file }}\n"
            "          SECOND_RAW_FILE: ${{ steps.narrow.outputs.file }}",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("raw impacted file", errors)

    def test_second_narrow_file_use_fails_closed(self):
        changed = SOURCE.read_text().replace(
            'elif ! scope="$(scripts/affected-targets narrow-scope '
            'linux-premerge-build "$NARROW_FILE")"; then',
            'head -n1 "$NARROW_FILE"\n'
            '          elif ! scope="$(scripts/affected-targets narrow-scope '
            'linux-premerge-build "$NARROW_FILE")"; then',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("expose $NARROW_FILE only", errors)

    def test_second_local_raw_file_use_fails_closed(self):
        changed = SOURCE.read_text().replace(
            'if scripts/affected-targets narrow-scope linux-premerge-tests '
            '"$file" >"$test_file"; then',
            'head -n1 "$file"\n'
            '            if scripts/affected-targets narrow-scope linux-premerge-tests '
            '"$file" >"$test_file"; then',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("local raw impacted file only", errors)

    def test_second_native_local_raw_file_use_fails_closed(self):
        marker = 'file="${RUNNER_TEMP}/impacted-targets.txt"'
        changed = SOURCE.read_text().replace(
            marker,
            marker + '\n          head -n1 "$file" >/dev/null',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("native-platforms must use its local raw impacted file only", errors)

    def test_each_registered_consumer_must_intersect(self):
        changed = SOURCE.read_text().replace(
            'scripts/affected-targets narrow-scope linux-premerge-tests "$file"',
            'printf "%s\\n" "$RUE_AFFECTED_IMPACTED"',
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("linux-premerge-tests", errors)

    def test_degraded_build_fallback_cannot_be_removed(self):
        changed = SOURCE.read_text().replace(
            'scripts/ci-timed "linux-x64 build" -- ./buck2 build //crates/...',
            "echo './buck2 build //crates/...'",
            2,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("full-scope degraded fallback", errors)

    def test_saved_share_summary_cannot_be_removed(self):
        changed = SOURCE.read_text().replace("saved share", "scope share")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("saved-share visibility", errors)

    def test_impacted_reference_in_comment_is_not_a_consumer(self):
        changed = SOURCE.read_text().replace(
            "  ci-success:\n",
            "  # needs.affected-targets.outputs.impacted is documented only\n"
            "  ci-success:\n",
            1,
        )
        self.assertEqual(self.validate_text(changed), [])

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
        # A direct Buck target in the native workflow is a second membership
        # source and must be rejected; the live graph label is authoritative.
        changed = SOURCE.read_text().replace(
            "          native_targets=\"$(scripts/affected-targets native-targets)\" || exit 1\n",
            "          ./buck2 test //crates/rue-query:rue-query-test\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("//crates/rue-query:rue-query-test", errors)
        self.assertIn("must not name Buck targets", errors)

    def test_second_native_buck_test_step_cannot_hide_a_target_list(self):
        changed = SOURCE.read_text().replace(
            "          scripts/ci-timed \"$LANE_NAME native units\" -- ./buck2 test \"${targets[@]}\"\n",
            "          scripts/ci-timed \"$LANE_NAME native units\" -- ./buck2 test \"${targets[@]}\"\n"
            "      - name: Another native test step\n"
            "        run: ./buck2 test //crates/rue-query:rue-query-test\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("must not name Buck targets", errors)

    def test_native_target_cannot_be_appended_to_graph_derived_array(self):
        changed = SOURCE.read_text().replace(
            "          if [ \"$NARROW_COUNT\" -gt 0 ]; then\n",
            "          targets+=(//crates/rue-query:rue-query-test)\n"
            "          if [ \"$NARROW_COUNT\" -gt 0 ]; then\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("unit membership must come only from the graph", errors)
        self.assertIn("//crates/rue-query:rue-query-test", errors)

    def test_dynamic_peer_lane_target_query_cannot_be_appended(self):
        changed = SOURCE.read_text().replace(
            "          if [ \"$NARROW_COUNT\" -gt 0 ]; then\n",
            "          targets+=(\"$(scripts/affected-targets lane-targets release)\")\n"
            "          if [ \"$NARROW_COUNT\" -gt 0 ]; then\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("may use scripts/affected-targets only", errors)

    def test_dynamic_peer_lane_query_cannot_share_the_native_assignment_line(self):
        changed = SOURCE.read_text().replace(
            "          native_targets=\"$(scripts/affected-targets native-targets)\" || exit 1\n",
            "          native_targets=\"$(scripts/affected-targets native-targets)\" || exit 1; "
            "targets+=(\"$(scripts/affected-targets lane-targets release)\")\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("may use scripts/affected-targets only", errors)

    def test_native_job_cannot_run_a_second_direct_buck_query(self):
        changed = SOURCE.read_text().replace(
            "          native_targets=\"$(scripts/affected-targets native-targets)\" || exit 1\n",
            "          ./buck2 uquery //...\n"
            "          native_targets=\"$(scripts/affected-targets native-targets)\" || exit 1\n",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("must not run direct Buck graph queries", errors)

    def test_native_job_cannot_hide_query_behind_buck_global_flags(self):
        changed = SOURCE.read_text().replace(
            "          native_targets=\"$(scripts/affected-targets native-targets)\" || exit 1\n",
            "          targets+=(\"$(./buck2 --isolation-dir peer uquery \"attrfilter(labels, rue_other, //...)\")\")\n"
            "          native_targets=\"$(scripts/affected-targets native-targets)\" || exit 1\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("must not run direct Buck graph queries", errors)

    def test_renamed_native_step_cannot_bypass_graph_derived_invocation(self):
        changed = SOURCE.read_text().replace(
            "      - name: Run graph-scoped native unit tests\n",
            "      - name: Renamed native step\n",
            1,
        ).replace(
            "scripts/ci-timed \"$LANE_NAME native units\" -- ./buck2 test \"${targets[@]}\"",
            "./buck2 test //crates/rue-query:rue-query-test",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("exactly one graph-derived unit invocation", errors)
        self.assertIn("must not name Buck targets", errors)

    def test_native_build_and_comment_labels_are_not_unit_target_drift(self):
        changed = SOURCE.read_text().replace(
            "# rue_platform_native label.",
            "# rue_platform_native label; //:comment-only-target is documentation.",
            1,
        )
        self.assertEqual(MODULE.lane_target_drift(changed), [])

    def test_ci_contract_installs_dotslash_before_live_validator(self):
        source = SOURCE.read_text()
        prefix, contract = source.split("  ci-contract:\n", 1)
        contract = contract.replace(
            "      - name: Bootstrap dotslash\n        uses: ./.github/actions/bootstrap-dotslash\n",
            "",
            1,
        )
        changed = prefix + "  ci-contract:\n" + contract
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("ci-contract must install dotslash before the live Buck validator", errors)

    def test_lane_targets_and_job_agree_today(self):
        self.assertEqual(MODULE.lane_target_drift(SOURCE.read_text()), [])

    def test_unreadable_lane_script_fails_closed(self):
        # Ownership validation fails closed when its canonical graph query is
        # unavailable.
        self.assertIn(
            "graph query failed",
            "\n".join(
                MODULE.native_lane_ownership(SOURCE.read_text(), script=Path("/nonexistent/affected-targets"))
            ),
        )

    def test_native_graph_ownership_fails_on_an_unowned_graph_target(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "affected-targets"
            script.write_text(
                "#!/usr/bin/env bash\n"
                "case \"$1\" in\n"
                "native-targets) echo //:native-one //:native-two;;\n"
                "lane-targets) echo //:native-one //:spec-tests //:cli-tests;;\n"
                "esac\n"
            )
            errors = MODULE.native_lane_ownership(SOURCE.read_text(), script=script)
        rendered = "\n".join(errors)
        self.assertIn("native-linux-arm64 is missing graph-owned targets", rendered)
        self.assertIn("native-macos-arm64 is missing graph-owned targets", rendered)

    def test_native_graph_ownership_rejects_unexpected_targets_in_both_lanes(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "affected-targets"
            script.write_text(
                "#!/usr/bin/env bash\n"
                "case \"$1\" in\n"
                "native-targets) echo //:native-one;;\n"
                "lane-targets) echo //:native-one //:unit-two //:spec-tests //:cli-tests;;\n"
                "esac\n"
            )
            errors = MODULE.native_lane_ownership(SOURCE.read_text(), script=script)
        rendered = "\n".join(errors)
        self.assertIn("native-linux-arm64 selected unlabelled or unexpected targets", rendered)
        self.assertIn("native-macos-arm64 selected unlabelled or unexpected targets", rendered)

    def test_native_graph_ownership_rejects_empty_graph(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "affected-targets"
            script.write_text(
                "#!/usr/bin/env bash\n"
                "if [ \"$1\" = native-targets ]; then exit 0; fi\n"
            )
            errors = MODULE.native_lane_ownership(SOURCE.read_text(), script=script)
        self.assertIn("graph selection is empty", "\n".join(errors))


if __name__ == "__main__":
    unittest.main()
