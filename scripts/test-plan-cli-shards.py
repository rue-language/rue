#!/usr/bin/env python3
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

MODULE = load_script("plan-cli-shards.py", __file__)


def fixture(**cli_updates):
    cli = {
        "unsharded_wall_range_ms": [900000, 1098000],
        "planning_total_ms": 1098000,
        "runner_count_skew_allowance_basis_points": 2500,
        "observed_balance_budget_basis_points": 2000,
        "source_run_ids": [101],
        "indivisible_inventory": {
            "scope": "fixed per-lane work",
            "acquisition": "inspect lane steps",
            "completeness": "manual-reviewed; not graph-derived",
        },
        "indivisible_items": [
            {
                "name": "compiler build",
                "observed_wall_ms": 317000,
                "source_metric": "build step wall",
                "source_run_ids": [101],
            }
        ],
    }
    cli.update(cli_updates)
    return {
        "version": 1,
        "measured_at": "2026-08-08",
        "provenance": {
            "source": "fixture",
            "acquisition": "fixture acquisition",
            "refresh": "fixture refresh",
        },
        "floor": {
            "name": "native",
            "observed_wall_range_ms": [341000, 407000],
            "planning_floor_ms": 407000,
            "source_run_ids": [101],
        },
        "cli": cli,
        "phase_6_remeasurement": {
            "pre_change": {
                "event": "merge_group",
                "run_id": 100,
                "head_sha": "a" * 40,
                "created_at": "2026-01-01T00:00:00Z",
                "completed_at": "2026-01-01T00:01:00Z",
                "workflow_wall_ms": 60000,
                "url": "https://example.invalid/actions/runs/100",
                "binding_job": {
                    "name": "binding",
                    "job_id": 200,
                    "started_at": "2026-01-01T00:00:01Z",
                    "completed_at": "2026-01-01T00:00:59Z",
                    "wall_ms": 58000,
                },
            },
            "post_change": {
                "status": "pending_pr_ci",
                "pull_request_run_ids": [],
                "observed_critical_path_ms": [],
            },
            "comparison": "fixture comparison policy",
        },
    }


class PlannerTests(unittest.TestCase):
    def test_current_measurements_derive_four(self):
        count, floor, total, projected = MODULE.derive_runner_count(fixture())
        self.assertEqual((count, floor, total), (4, 407000, 1098000))
        self.assertLessEqual(projected, floor)
        three = (total + 2) // 3
        self.assertGreater((three * 125 + 99) // 100, floor)

    def test_overweight_indivisible_item_names_item_and_no_count(self):
        data = fixture(
            indivisible_items=[
                {
                    "name": "pressure corpus",
                    "observed_wall_ms": 407001,
                    "source_metric": "corpus wall",
                    "source_run_ids": [102],
                }
            ]
        )
        with self.assertRaisesRegex(ValueError, "pressure corpus.*runner count is undefined"):
            MODULE.derive_runner_count(data)

    def test_single_ceiling_selects_the_minimal_boundary_count(self):
        data = fixture(
            unsharded_wall_range_ms=[4, 4],
            planning_total_ms=4,
            indivisible_items=[
                {
                    "name": "fixed",
                    "observed_wall_ms": 2,
                    "source_metric": "fixture wall",
                    "source_run_ids": [101],
                }
            ],
        )
        data["floor"]["observed_wall_range_ms"] = [2, 2]
        data["floor"]["planning_floor_ms"] = 2
        count, _floor, _total, projected = MODULE.derive_runner_count(data)
        self.assertEqual((count, projected), (3, 2))
        self.assertGreater(MODULE.ceil_div(4 * 12500, 2 * 10000), 2)

    def test_graph_union_owns_the_old_matrix_drift_case(self):
        targets = ["//:cli-tests-shard-0", "//:cli-tests-shard-2", "//:cli-tests-shard-3"]
        with self.assertRaisesRegex(ValueError, "missing from graph: //:cli-tests-shard-1"):
            MODULE.validate_graph_union(targets, 4)

    def test_matrix_preserves_required_check_names(self):
        targets = [f"//:cli-tests-shard-{index}" for index in range(4)] + [
            "//:spec-tests",
            "//crates/rue-oracle-diff:oracle-diff-test",
            "//crates/rue-oracle-diff:oracle-diff-spec-test-o3",
        ]
        plan = MODULE.matrix(targets)
        names = [row["check_name"] for row in plan["include"]]
        self.assertEqual(names[:4], [f"linux-x64-cli-shard-{index}" for index in range(4)])
        self.assertIn("linux-x64-oracle-diff", names)
        self.assertIn("linux-x64-oracle-diff-spec-o3", names)

    def test_canonical_corpus_inventory_must_cover_live_shard_union(self):
        graph = [f"//:cli-tests-shard-{index}" for index in range(4)]
        with self.assertRaisesRegex(ValueError, "graph shards missing.*shard-2"):
            MODULE.validate_corpus_union([graph[0], graph[1], graph[3], "//:spec-tests"], graph)

    def test_historical_24_9_percent_observed_skew_fires(self):
        # Mean 1000ms, maximum 1249ms: exactly the historical 24.9% replay.
        observations = {
            "samples": [
                {"name": "historical-run", "lane_wall_ms": [1249, 917, 917, 917]}
            ]
        }
        errors = MODULE.check_observed(fixture(), observations)
        self.assertEqual(len(errors), 1)
        self.assertIn("24.90%", errors[0])

    def test_observed_skew_ceiling_stays_integer_for_large_values(self):
        walls = [9007199254740993, 9007199254740992, 9007199254740992]
        total = sum(walls)
        numerator = max(walls) * len(walls) - total
        expected = (numerator * 10000 + total - 1) // total
        self.assertEqual(MODULE.observed_skew_basis_points(walls), expected)

    def test_measurement_provenance_is_required(self):
        data = fixture()
        del data["cli"]["indivisible_items"][0]["source_run_ids"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "measurements.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "source_run_ids"):
                MODULE.load_measurements(path)

    def test_pending_remeasurement_cannot_claim_observations(self):
        data = fixture()
        post_change = data["phase_6_remeasurement"]["post_change"]
        post_change["pull_request_run_ids"] = [123]
        post_change["observed_critical_path_ms"] = [456]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "measurements.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "pending Phase 6"):
                MODULE.load_measurements(path)

    def test_observed_wall_guard_accepts_balanced_lanes(self):
        observations = {"samples": [{"name": "current", "lane_wall_ms": [100, 95, 105, 100]}]}
        self.assertEqual(MODULE.check_observed(fixture(), observations), [])

    def test_observed_wall_guard_requires_every_planned_lane(self):
        observations = {"samples": [{"name": "partial", "lane_wall_ms": [100, 95, 105]}]}
        with self.assertRaisesRegex(ValueError, "expected 4 observed lane walls, got 3"):
            MODULE.check_observed(fixture(), observations)

    def test_repetition_artifacts_become_observed_lane_walls(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for shard, elapsed in enumerate((100, 95, 105, 100)):
                path = root / f"shard-{shard}" / "results.tsv"
                path.parent.mkdir()
                path.write_text(
                    f"//:cli-tests-shard-{shard}\t1\tPASS\t{elapsed}\tignored.log\n"
                )
            observations = MODULE.load_repetition_observations(root, 4)
            self.assertEqual(
                observations["samples"][0]["lane_wall_ms"],
                [100000, 95000, 105000, 100000],
            )

    def test_live_measurement_file_is_well_formed(self):
        path = Path(__file__).resolve().parents[1] / "ci/cli-shard-planning.json"
        data = MODULE.load_measurements(path)
        self.assertEqual(MODULE.derive_runner_count(data)[0], 4)


if __name__ == "__main__":
    unittest.main()
