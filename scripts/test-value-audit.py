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
        result = value_audit.pair_verdict(left, right, "improvement", 0.50)
        self.assertEqual(result["status"], "pass")
        self.assertAlmostEqual(result["improvement_fraction"], 0.60)

        noisy_right = value_audit.median_mad([20.0, 20.0, 20.0, 40.0, 60.0, 60.0, 60.0])
        noisy = value_audit.pair_verdict(left, noisy_right, "improvement", 0.50)
        self.assertIn(noisy["status"], {"indeterminate", "fail"})

    def test_cold_gate_uses_larger_mad_and_two_percent_floor(self):
        left = value_audit.median_mad([100.0] * 7)
        right = value_audit.median_mad([102.0] * 7)
        result = value_audit.pair_verdict(left, right, "regression")
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["allowed_regression"], 2.0)

    def test_session_locality_gate_is_black_box(self):
        row = {
            "required_vs_reused_work": {
                "semantic_bodies": {"required": 0, "reused": 128},
                "cfgs": {"required": 0, "reused": 128},
                "semantic_queries": {"required": 0, "reused": 1},
            }
        }
        self.assertEqual(value_audit.locality_check("warm_no_op", row)["status"], "pass")
        self.assertEqual(
            value_audit.locality_check("warm_unrelated_declaration", row)["status"],
            "pass",
        )

    def test_old_protocol_is_explicitly_supported_as_black_box(self):
        self.assertEqual(value_audit.parse_rss("maximum resident set size: 123", "darwin"), 123)
        self.assertEqual(value_audit.parse_rss("Maximum resident set size (kbytes): 2", "gnu"), 2048)
        self.assertIsNone(value_audit.parse_rss("no memory field", "none"))

    def test_fixture_manifest_reuses_canonical_benchmark_manifest(self):
        manifest = value_audit.load_audit_manifest(ROOT)
        self.assertEqual(manifest["protocol"]["paired_samples"], 7)
        self.assertEqual(manifest["protocol"]["warmup"], 1)


if __name__ == "__main__":
    unittest.main()
