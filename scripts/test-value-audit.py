#!/usr/bin/env python3
"""Focused protocol tests for rue-value-audit.py."""

import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("value_audit", ROOT / "scripts/rue-value-audit.py")
assert SPEC is not None and SPEC.loader is not None
value_audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(value_audit)


class ValueAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.manifest = value_audit.load_audit_manifest(ROOT)
        cls.thresholds = cls.manifest["thresholds"]

    def locality_policy(self, scenario):
        return self.manifest["locality"][scenario]

    def work(self, *, body=0, cfg=0, modules=0, rir=0, declarations=0,
             durable_source=0, queries=0, reused=1):
        def counters(computed, reused_count):
            return {
                "computed": computed,
                "invalidated": computed,
                "reused": reused_count,
            }
        return {
            "schema_version": 2,
            "counter_source": "production_metrics",
            "exact_identity_sets": {
                "available": False,
                "reason": "test fixture has no identity-set protocol",
            },
            "modules": counters(modules, reused),
            "rir": counters(rir, reused),
            "declarations": counters(declarations, reused),
            "durable_source": counters(durable_source, reused),
            "semantic_bodies": counters(body, reused),
            "cfgs": counters(cfg, reused),
            "semantic_queries": counters(queries, reused),
        }

    def parity(self):
        return {
            "schema_version": 1,
            "status": "pass",
            "comparison": "cold_vs_reused_in_process",
            "comparison_completed": True,
            "public_semantic_cfg_artifacts": "exact",
            "public_type_universe": "exact_ordered_entries",
            "durable_specialized_body_payloads": "exact",
            "durable_ordinary_body_manifest_artifacts": "exact",
            "diagnostics": "exact",
            "warnings": "exact",
            "stable_identities": "exact",
            "dependency_records": "exact",
            "emitted_output": "byte_exact",
            "emitted_output_sha256": "0" * 64,
            "emitted_output_size_bytes": 1,
            "cold_semantic_work": {},
        }

    def row(self, scenario, **kwargs):
        return {
            "wall_time_ns": 1,
            "evidence_schema": {
                "version": 2,
                "differential_parity_version": 1,
                "locality_work_version": 2,
            },
            "differential_parity": self.parity(),
            "required_vs_reused_work": self.work(**kwargs),
        }

    def test_median_and_mad_are_raw_and_deterministic(self):
        self.assertEqual(
            value_audit.median_mad([10.0, 12.0, 11.0, 13.0, 9.0]),
            {
                "available": True,
                "samples": [10.0, 12.0, 11.0, 13.0, 9.0],
                "median": 11.0,
                "mad": 1.0,
            },
        )

    def test_improvement_requires_threshold_and_three_mad_win(self):
        left = value_audit.median_mad([100.0] * 7)
        right = value_audit.median_mad([40.0] * 7)
        result = value_audit.pair_verdict(left, right, "improvement", 0.50, self.thresholds)
        self.assertEqual(result["status"], "pass")
        self.assertAlmostEqual(result["improvement_fraction"], 0.60)

        noisy_right = value_audit.median_mad([20.0, 20.0, 20.0, 40.0, 60.0, 60.0, 60.0])
        noisy = value_audit.pair_verdict(left, noisy_right, "improvement", 0.50, self.thresholds)
        self.assertIn(noisy["status"], {"indeterminate", "fail"})

    def test_cold_gate_uses_larger_mad_and_two_percent_floor(self):
        left = value_audit.median_mad([100.0] * 7)
        right = value_audit.median_mad([102.0] * 7)
        result = value_audit.pair_verdict(left, right, "regression", thresholds=self.thresholds)
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["allowed_regression"], 2.0)

    def test_session_locality_gate_consumes_exact_upper_bounds(self):
        row = self.row("warm_no_op", reused=128)
        self.assertEqual(
            value_audit.locality_check("warm_no_op", row, self.locality_policy("warm_no_op"))["status"],
            "pass",
        )
        all_modules_reparsed = self.row("warm_unrelated_declaration", modules=128)
        self.assertEqual(
            value_audit.locality_check(
                "warm_unrelated_declaration", all_modules_reparsed,
                self.locality_policy("warm_unrelated_declaration"),
            )["status"],
            "fail",
        )

    def test_all_warm_required_evidence_unsupported_is_not_a_pass(self):
        workload = {
            "supported_scenarios": ["warm_leaf_body"],
            "family": "synthetic",
        }
        rows = {role: {"status": "unsupported"} for role in value_audit.ROLES}
        result = value_audit.scenario_verdict(
            "warm_leaf_body", rows, workload,
            self.manifest["scenario_policy"]["warm_leaf_body"], self.thresholds,
        )
        self.assertEqual(result["status"], "indeterminate")

    def test_empty_parity_object_is_not_exact_evidence(self):
        row = self.row("warm_leaf_body", body=1, cfg=1)
        row["differential_parity"] = {}
        self.assertEqual(value_audit.parity_check(row)["status"], "unsupported")

    def test_ordered_type_universe_is_valid_exact_parity_evidence(self):
        self.assertEqual(value_audit.parity_check(self.row("warm_leaf_body"))["status"], "pass")

    def test_leaf_edit_with_full_program_recompute_fails(self):
        row = self.row("warm_leaf_body", body=128, cfg=127, modules=128)
        result = value_audit.locality_check(
            "warm_leaf_body", row, self.locality_policy("warm_leaf_body")
        )
        self.assertEqual(result["status"], "fail")

    def test_unrelated_edit_with_semantic_rerun_fails(self):
        row = self.row("warm_unrelated_declaration", declarations=1, queries=1)
        result = value_audit.locality_check(
            "warm_unrelated_declaration", row,
            self.locality_policy("warm_unrelated_declaration"),
        )
        self.assertEqual(result["status"], "fail")

    def test_manifest_threshold_drift_is_rejected(self):
        drifted = dict(self.manifest)
        drifted["thresholds"] = dict(self.manifest["thresholds"])
        drifted["thresholds"]["warm_leaf_body_improvement"] = 0.24
        original_loader = value_audit.tomllib.load
        value_audit.tomllib.load = lambda _stream: drifted
        try:
            with self.assertRaises(ValueError):
                value_audit.load_audit_manifest(ROOT)
        finally:
            value_audit.tomllib.load = original_loader

    def test_role_provenance_is_explicit_and_never_defaults(self):
        with self.assertRaises(ValueError):
            value_audit.role_metadata(
                "candidate", Path("/does/not/exist"), None
            )

    def test_old_protocol_is_explicitly_supported_as_black_box(self):
        self.assertEqual(value_audit.parse_rss("maximum resident set size: 123", "darwin"), 123)
        self.assertEqual(value_audit.parse_rss("123456 maximum resident set size", "darwin"), 123456)
        self.assertEqual(value_audit.parse_rss("Maximum resident set size (kbytes): 2", "gnu"), 2048)
        self.assertIsNone(value_audit.parse_rss("no memory field", "none"))

    def test_cold_rss_failure_rolls_into_overall_gate(self):
        def role(protocol, rss):
            return {
                "status": "pass",
                "protocol": protocol,
                "wall": {"median": 100.0, "mad": 0.0},
                "rss": {"median": rss, "mad": 0.0},
            }
        result = value_audit.scenario_verdict(
            "cold",
            {
                "historical_baseline": role("benchmark_json", 100.0),
                "current_production": role("benchmark_json", 103.0),
                "candidate": role("benchmark_json", 103.0),
            },
            {"family": "synthetic", "supported_scenarios": ["cold"]},
            self.manifest["scenario_policy"]["cold"],
            self.thresholds,
        )
        self.assertEqual(result["status"], "fail")
        self.assertEqual(result["pairs"]["historical_baseline_vs_current_production"]["rss"]["status"], "fail")

    def test_cross_protocol_timing_comparison_is_indeterminate(self):
        def role(protocol):
            return {
                "status": "pass",
                "protocol": protocol,
                "wall": {"median": 100.0, "mad": 0.0},
                "rss": {"median": 100.0, "mad": 0.0},
            }
        result = value_audit.scenario_verdict(
            "cold",
            {
                "historical_baseline": role("black_box_compile"),
                "current_production": role("benchmark_json"),
                "candidate": role("benchmark_json"),
            },
            {"family": "synthetic", "supported_scenarios": ["cold"]},
            self.manifest["scenario_policy"]["cold"],
            self.thresholds,
        )
        self.assertEqual(result["status"], "indeterminate")

    def test_partial_locality_row_fails_closed(self):
        row = self.row("warm_leaf_body", body=1, cfg=1)
        del row["required_vs_reused_work"]["cfgs"]
        self.assertEqual(
            value_audit.locality_check(
                "warm_leaf_body", row, self.locality_policy("warm_leaf_body")
            )["status"],
            "unsupported",
        )

    def test_unsupported_sample_preserves_collected_role_row(self):
        role = {"status": "pass", "wall_samples": [], "rss_samples": [], "details": []}
        value_audit.merge_role_sample(
            role,
            {"status": "pass", "wall_ms": 10.0, "peak_rss_bytes": 100, "protocol": "session_benchmark_json"},
            "warm_leaf_body",
        )
        value_audit.merge_role_sample(role, {"status": "unsupported", "reason": "missing"}, "warm_leaf_body")
        self.assertEqual(role["status"], "indeterminate")
        self.assertEqual(role["wall_samples"], [10.0])

    def test_measurement_environment_removes_rust_log(self):
        original = value_audit.os.environ.get("RUST_LOG")
        value_audit.os.environ["RUST_LOG"] = "debug"
        try:
            self.assertNotIn("RUST_LOG", value_audit.measurement_env(ROOT))
        finally:
            if original is None:
                value_audit.os.environ.pop("RUST_LOG", None)
            else:
                value_audit.os.environ["RUST_LOG"] = original

    def test_perf_baseline_loader_is_cached(self):
        value_audit.load_perf_baseline.cache_clear()
        value_audit.load_perf_baseline(ROOT)
        value_audit.load_perf_baseline(ROOT)
        self.assertEqual(value_audit.load_perf_baseline.cache_info().hits, 1)

    def test_manifest_pins_warmup_and_caldera_timeout_headroom(self):
        self.assertEqual(self.manifest["protocol"]["warmup"], 1)
        self.assertGreater(self.manifest["protocol"]["caldera_timeout_headroom_seconds"], 0)

    def test_fixture_manifest_reuses_canonical_benchmark_manifest(self):
        manifest = value_audit.load_audit_manifest(ROOT)
        self.assertEqual(manifest["schema_version"], 2)
        self.assertEqual(manifest["protocol"]["paired_samples"], 7)
        self.assertEqual(manifest["protocol"]["warmup"], 1)


if __name__ == "__main__":
    unittest.main()
