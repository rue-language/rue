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
# The graph-derived platform-corpus inventory a live run supplies; the current
# graph's answer, so the RUE-1163 ownership check can be exercised without Buck.
INVENTORY = (
    "//:cli-tests-shard-0",
    "//:cli-tests-shard-1",
    "//:cli-tests-shard-2",
    "//:cli-tests-shard-3",
    "//:spec-tests",
    "//crates/rue-oracle-diff:oracle-diff-test",
)


def fake_script(directory: Path, body: str) -> Path:
    script = directory / "affected-targets"
    script.write_text("#!/usr/bin/env bash\n" + body)
    return script


class GateValidatorTests(unittest.TestCase):
    def validate_text(
        self,
        text,
        native_runner=None,
        test_runner=None,
        buck=None,
        inventory=INVENTORY,
        script_body=None,
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
            buck_path.write_text(buck if buck is not None else ROOT_BUCK.read_text())
            script = (
                fake_script(Path(directory), script_body)
                if script_body is not None
                else MODULE.AFFECTED_TARGETS_SCRIPT
            )
            return MODULE.validate(
                path, runner_path, test_runner_path, buck_path, script, inventory
            )

    def test_current_workflow_is_valid(self):
        self.assertEqual(self.validate_text(SOURCE.read_text()), [])
        # The structural Buck run has no inventory and must pass too.
        self.assertEqual(
            MODULE.validate(SOURCE, MODULE.NATIVE_RUNNER_SCRIPT, TEST_RUNNER_SOURCE, ROOT_BUCK),
            [],
        )

    # --- the aggregate is the whole branch-protection contract ------------
    def test_removing_or_renaming_job_fails_inventory(self):
        removed = SOURCE.read_text().replace("\n  asan:\n", "\n  removed-asan:\n", 1)
        errors = "\n".join(self.validate_text(removed))
        self.assertIn("unaggregated jobs: removed-asan", errors)
        self.assertIn("jobs the workflow does not define: asan", errors)

    def test_actions_compatible_underscore_job_is_not_invisible(self):
        changed = SOURCE.read_text() + "\n  unaggregated_job:\n    runs-on: ubuntu-latest\n"
        self.assertIn("unaggregated jobs: unaggregated_job", "\n".join(self.validate_text(changed)))

    def test_omitting_dependency_from_gate_fails(self):
        changed = SOURCE.read_text().replace("      - valgrind\n", "", 1)
        self.assertIn("unaggregated jobs: valgrind", "\n".join(self.validate_text(changed)))

    def test_gate_must_evaluate_its_needs_map(self):
        changed = SOURCE.read_text().replace("${{ toJSON(needs) }}", "'{}'", 1)
        self.assertIn("no longer evaluates ${{ toJSON(needs) }}", "\n".join(self.validate_text(changed)))

    def test_remote_execution_stays_merge_group_only(self):
        changed = SOURCE.read_text().replace(
            "if: github.event_name == 'merge_group'", "if: github.event_name != 'pull_request'", 1
        )
        self.assertIn("merge-group-only", "\n".join(self.validate_text(changed)))

    # --- ci-contract ------------------------------------------------------
    def test_ci_contract_tier_selector_receives_affected_targets_and_live_graph(self):
        source = SOURCE.read_text()
        changed = source.replace("          --affected-targets scripts/affected-targets\n", "", 1)
        self.assertIn("canonical affected-targets input", "\n".join(self.validate_text(changed)))
        changed = source.replace("          --live-graph\n", "", 1)
        self.assertNotEqual(changed, source, "splice anchor no longer matches ci.yml")
        self.assertIn("prove the derived matrix from the live graph", "\n".join(self.validate_text(changed)))

    def test_ci_contract_installs_dotslash_before_live_validator(self):
        prefix, contract = SOURCE.read_text().split("  ci-contract:\n", 1)
        contract = contract.replace(
            "      - name: Bootstrap dotslash\n        uses: ./.github/actions/bootstrap-dotslash\n", "", 1
        )
        errors = "\n".join(self.validate_text(prefix + "  ci-contract:\n" + contract))
        self.assertIn("ci-contract must install dotslash before the live Buck validator", errors)

    def test_ci_contract_must_run_live_and_check_scheduled_workflows(self):
        source = SOURCE.read_text()
        changed = source.replace(
            "run: scripts/validate-ci-gate.py .github/workflows/ci.yml\n",
            "run: scripts/validate-ci-gate.py .github/workflows/ci.yml --structural-only\n",
            1,
        )
        self.assertIn("not structural-only mode", "\n".join(self.validate_text(changed)))
        changed = source.replace("      actions: read\n", "", 1)
        self.assertNotEqual(changed, source, "splice anchor no longer matches ci.yml")
        self.assertIn("without `actions: read`", "\n".join(self.validate_text(changed)))

    # --- lane responsibilities ---------------------------------------------
    def test_dropping_the_duplication_gate_step_fails(self):
        changed = SOURCE.read_text().replace("scripts/validate-test-duplication.py", "true", 1)
        self.assertIn(
            "linux-premerge responsibility missing 'scripts/validate-test-duplication.py'",
            "\n".join(self.validate_text(changed)),
        )

    def test_staleness_gate_contract_follows_its_job(self):
        source = SOURCE.read_text()
        anchor = "  performance-staleness:\n    runs-on: ubuntu-latest\n"
        for splice, message in (
            (anchor + "    continue-on-error: true\n", "must not use continue-on-error"),
            (anchor + "    needs:\n      - affected-targets\n", "must not depend on another CI job"),
        ):
            changed = source.replace(anchor, splice, 1)
            self.assertNotEqual(changed, source, "splice anchor no longer matches ci.yml")
            self.assertIn(message, "\n".join(self.validate_text(changed)))
        changed = source.replace("          scripts/validate-performance-stall.py \\\n", "", 1)
        self.assertNotEqual(changed, source, "splice anchor no longer matches ci.yml")
        self.assertIn(
            "performance-staleness responsibility missing 'scripts/validate-performance-stall.py'",
            "\n".join(self.validate_text(changed)),
        )

    def test_reintroducing_the_defer_protocol_fails(self):
        changed = SOURCE.read_text().replace(
            "          RUE_TEST_TIER: premerge\n",
            "          RUE_TEST_TIER: premerge\n          RUE_CI_DEFER_HEAVY_SUITES: '//:cli-tests'\n",
            1,
        )
        self.assertIn("RUE_CI_DEFER_HEAVY_SUITES is retired", "\n".join(self.validate_text(changed)))

    def test_native_selector_cannot_silently_return_to_named_filters(self):
        runner = MODULE.NATIVE_RUNNER_SCRIPT.read_text().replace(
            "export RUE_PLATFORM_CASE_SELECTION=native", "export RUE_PLATFORM_CASE_SELECTION=all", 1
        )
        errors = "\n".join(self.validate_text(SOURCE.read_text(), runner))
        self.assertIn("export RUE_PLATFORM_CASE_SELECTION=native", errors)

    def test_native_abi_filter_excludes_only_the_accidental_intersection(self):
        changed = SOURCE.read_text().replace(
            "scripts/rue cli abi --skip cli.differential_opt::aggregate_abi_across_opt_levels",
            "scripts/rue cli abi",
            1,
        )
        self.assertIn(
            "scripts/rue cli abi --skip cli.differential_opt::aggregate_abi_across_opt_levels",
            "\n".join(self.validate_text(changed)),
        )

    def test_valgrind_cannot_return_to_inline_apt(self):
        changed = SOURCE.read_text().replace(
            "run: scripts/install-valgrind",
            "run: |\n          sudo apt-get update\n          sudo apt-get install -y valgrind",
            1,
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("must invoke scripts/install-valgrind", errors)
        self.assertIn("inline unbounded apt-get", errors)

    # --- RUE-1267: the derived matrix wiring --------------------------------
    def test_platform_corpus_must_consume_the_derived_matrix(self):
        changed = SOURCE.read_text().replace(
            "matrix: ${{ fromJSON(needs.affected-targets.outputs.corpus_matrix) }}", "matrix: {}", 1
        )
        errors = "\n".join(self.validate_text(changed, inventory=None))
        self.assertIn("must take its matrix from the derived corpus_matrix output", errors)
        # With a literal (empty) matrix the labeled corpora lose their owner,
        # and that is decidable from the text alone, without a live inventory.
        self.assertIn("//:spec-tests is marked rue_ci_dedicated_lane", errors)

    def test_planner_contract_and_bootstraps_are_pinned(self):
        source = SOURCE.read_text()
        changed = source.replace("scripts/affected-targets corpus-targets >", "echo //:spec-tests >", 1)
        self.assertIn("planner contract 'scripts/affected-targets corpus-targets'", "\n".join(self.validate_text(changed)))
        changed = source.replace(
            "      - name: Bootstrap dotslash for shard planning\n"
            "        # Every non-PR trigger, including workflow_dispatch, needs Buck for the\n"
            "        # live graph query. PRs use the BTD-aware bootstrap immediately above.\n"
            "        if: github.event_name != 'pull_request'\n",
            "      - name: Bootstrap dotslash for shard planning\n"
            "        if: github.event_name == 'merge_group'\n",
            1,
        )
        self.assertNotEqual(changed, source, "splice anchor no longer matches ci.yml")
        self.assertIn("including workflow_dispatch", "\n".join(self.validate_text(changed)))

    # --- RUE-1161: platform responsibility matrix ---------------------------
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
        self.assertIn("aarch64-macos is declared CI-executed", "\n".join(self.validate_text(changed)))

    def test_unreadable_platform_matrix_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            errors = MODULE.validate(
                SOURCE, MODULE.NATIVE_RUNNER_SCRIPT, Path(directory) / "absent.rs", ROOT_BUCK
            )
        self.assertTrue(any("platform responsibility matrix unreadable" in e for e in errors), errors)

    # --- RUE-1163: dedicated-lane ownership ---------------------------------
    def test_dedicated_corpus_missing_from_the_live_inventory_fails(self):
        # The live run supplies the graph-derived matrix membership; a labeled
        # corpus the derivation dropped is skipped by premerge and run by nobody.
        errors = "\n".join(
            self.validate_text(SOURCE.read_text(), inventory=[t for t in INVENTORY if t != "//:spec-tests"])
        )
        self.assertIn("//:spec-tests is marked rue_ci_dedicated_lane", errors)
        self.assertIn("no exactly-one dedicated owner", errors)

    def test_release_smoke_label_is_owned_by_release_job(self):
        changed = SOURCE.read_text().replace(
            "./buck2 test //:release-smoke --target-platforms", "./buck2 test //:other --target-platforms", 1
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("//:release-smoke is marked rue_ci_dedicated_lane", errors)

    def test_release_smoke_cannot_be_owned_by_two_dedicated_jobs(self):
        errors = "\n".join(
            self.validate_text(SOURCE.read_text(), inventory=INVENTORY + ("//:release-smoke",))
        )
        self.assertIn("//:release-smoke (owned by platform-corpus, release)", errors)

    def test_unlabeled_buck_fails_closed(self):
        buck = ROOT_BUCK.read_text().replace('"rue_ci_dedicated_lane"', '"unused"')
        self.assertIn(
            "no corpus carries rue_ci_dedicated_lane",
            "\n".join(self.validate_text(SOURCE.read_text(), buck=buck)),
        )

    def test_sharded_corpus_counts_as_covered(self):
        # //:cli-tests is labeled but never appears by name; its shards run it.
        self.assertEqual(
            MODULE.uncovered_dedicated_lanes(
                'name = "cli-tests",\n    labels = ["rue_ci_dedicated_lane"]',
                "target: //:cli-tests-shard-0\ntarget: //:cli-tests-shard-1\n",
            ),
            [],
        )

    # --- RUE-1130: outputs and gates ----------------------------------------
    def test_undeclared_need_output_fails_closed(self):
        # GitHub resolves an undeclared job output to the empty string, so a
        # lane gate reading it sees "nothing selected" and deselects every
        # lane — invisible on any PR touching CI, because those force a full run.
        changed = SOURCE.read_text().replace(
            "      selected_lanes: ${{ steps.decide.outputs.selected_lanes }}\n", "", 1
        )
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("needs.affected-targets.outputs.selected_lanes is referenced", errors)
        self.assertIn("silently resolve to the empty string", errors)

    def test_need_output_from_unknown_job_fails(self):
        self.assertIn(
            "references unknown job",
            "\n".join(MODULE.undeclared_need_outputs(
                "${{ needs.ghost.outputs.thing }}", {"real": "    outputs:\n      thing: x\n"}
            )),
        )

    def test_declared_outputs_parses_past_comments(self):
        block = (
            "    outputs:\n"
            "      full: ${{ steps.decide.outputs.full }}\n"
            "      # why this exists\n"
            "      selected_lanes: ${{ steps.decide.outputs.selected_lanes }}\n"
        )
        self.assertEqual(MODULE.declared_outputs(block), {"full", "selected_lanes"})

    def test_matrix_gate_expands_to_every_matrix_lane(self):
        changed = SOURCE.read_text().replace("            name: macos-arm64\n", "            name: macos-x64\n", 1)
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("gates on 'native-macos-x64', which scripts/affected-targets never selects", errors)
        self.assertIn("lane 'native-macos-arm64' must be gated by exactly one job", errors)

    def test_gate_on_an_unselectable_lane_name_fails(self):
        # A lane the determinator never emits is deselected on every selective run.
        changed = SOURCE.read_text().replace(
            'run: scripts/ci-corpus-selected "valgrind"', 'run: scripts/ci-corpus-selected "memcheck"', 1
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("gates on 'memcheck', which scripts/affected-targets never selects", errors)
        self.assertIn("lane 'valgrind' must be gated by exactly one job", errors)

    def test_gate_reading_the_wrong_selection_output_fails(self):
        source = SOURCE.read_text()
        _, asan = source.split("\n  asan:\n", 1)
        wrong = asan.replace(
            "RUE_AFFECTED_LANES: ${{ needs.affected-targets.outputs.selected_lanes }}",
            "RUE_AFFECTED_LANES: ${{ needs.affected-targets.outputs.selected }}",
            1,
        )
        self.assertNotEqual(wrong, asan, "splice anchor no longer matches ci.yml")
        changed = source.replace(asan, wrong, 1)
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("asan gate step for 'asan' lacks 'RUE_AFFECTED_LANES:", errors)

    def test_unavailable_lane_inventory_fails_closed(self):
        errors = "\n".join(self.validate_text(SOURCE.read_text(), script_body="exit 1\n"))
        self.assertIn("scripts/affected-targets lanes is unavailable or empty", errors)

    # --- RUE-1266: native membership belongs to the graph --------------------
    def test_lane_target_drift_fails_closed(self):
        changed = SOURCE.read_text().replace(
            '          native_targets="$(scripts/affected-targets native-targets)" || exit 1\n',
            "          ./buck2 test //crates/rue-query:rue-query-test\n",
            1,
        )
        self.assertNotEqual(changed, SOURCE.read_text(), "splice anchor no longer matches ci.yml")
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("//crates/rue-query:rue-query-test", errors)
        self.assertIn("must not name Buck targets", errors)
        self.assertIn("exactly once with scripts/affected-targets native-targets", errors)

    def test_native_job_cannot_run_a_direct_graph_query(self):
        changed = SOURCE.read_text().replace(
            '          native_targets="$(scripts/affected-targets native-targets)" || exit 1\n',
            '          native_targets="$(scripts/affected-targets native-targets)" || exit 1\n'
            '          extra="$(./buck2 uquery "attrfilter(labels, rue_platform_native, //...)")"\n',
            1,
        )
        self.assertIn("must not run direct Buck graph queries", "\n".join(self.validate_text(changed)))

    def test_native_build_and_comment_labels_are_not_unit_target_drift(self):
        changed = SOURCE.read_text().replace(
            "# rue_platform_native label.",
            "# rue_platform_native label; //:comment-only-target is documentation.",
            1,
        )
        self.assertEqual(MODULE.lane_target_drift(changed), [])

    def test_native_graph_ownership_compares_lanes_with_the_graph(self):
        with tempfile.TemporaryDirectory() as directory:
            script = fake_script(
                Path(directory),
                'case "$1" in\n'
                "native-targets) echo //:native-one //:native-two;;\n"
                "lane-targets) echo //:native-one //:unit-two //:spec-tests //:cli-tests;;\n"
                "esac\n",
            )
            rendered = "\n".join(MODULE.native_lane_ownership(SOURCE.read_text(), script=script))
        for lane in ("native-linux-arm64", "native-macos-arm64"):
            self.assertIn(f"{lane} is missing graph-owned targets: //:native-two", rendered)
            self.assertIn(f"{lane} selected unlabelled or unexpected targets: //:unit-two", rendered)

    def test_native_graph_ownership_rejects_empty_or_failed_graph(self):
        with tempfile.TemporaryDirectory() as directory:
            script = fake_script(Path(directory), 'if [ "$1" = native-targets ]; then exit 0; fi\n')
            self.assertIn("graph selection is empty", "\n".join(MODULE.native_lane_ownership("", script=script)))
        self.assertIn(
            "graph query failed",
            "\n".join(MODULE.native_lane_ownership("", script=Path("/nonexistent/affected-targets"))),
        )

    # --- RUE-1855: clippy ownership -------------------------------------------
    def test_clippy_live_proxy_and_registered_scope_must_agree(self):
        with tempfile.TemporaryDirectory() as directory:
            script = fake_script(
                Path(directory),
                'case "$1:${2:-}" in\n'
                "  lane-targets:clippy) echo //crates/one:one-clippy;;\n"
                "  scope-targets:clippy) echo //crates/two:two-clippy;;\n"
                "  clippy-owned-targets:) echo //crates/two:two-clippy;;\n"
                "esac\n",
            )
            errors = MODULE.clippy_lane_ownership(script)
        self.assertIn("clippy lane proxy and registered runnable scope disagree", "\n".join(errors))

    def test_clippy_owner_label_exactly_matches_canonical_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            script = fake_script(Path(directory), "echo //crates/one:one-clippy //crates/two:two-clippy\n")
            self.assertEqual(MODULE.clippy_lane_ownership(script), [])
            script = fake_script(
                Path(directory),
                'case "$1" in\n'
                "  lane-targets|scope-targets) echo //crates/one:one-clippy //crates/two:two-clippy;;\n"
                "  clippy-owned-targets) echo //crates/one:one-clippy //crates/one:one-test;;\n"
                "esac\n",
            )
            errors = "\n".join(MODULE.clippy_lane_ownership(script))
        self.assertIn("canonical live clippy targets missing rue_ci_clippy_lane: //crates/two:two-clippy", errors)
        self.assertIn("owner label contains targets outside the canonical live set", errors)


if __name__ == "__main__":
    unittest.main()
