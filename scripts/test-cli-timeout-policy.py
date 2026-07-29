#!/usr/bin/env python3
import importlib.util
import json
import os
import tempfile
import tomllib
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("cli-timeout-policy.py")
SPEC = importlib.util.spec_from_file_location("cli_timeout_policy", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class TimeoutPolicyTests(unittest.TestCase):
    def test_mosaic_section_and_automatic_example_use_slow_profile(self):
        cases = Path(os.environ["RUE_CLI_CASES"])
        authority = tomllib.loads((cases / "execution_contracts.toml").read_text())
        mosaic = tomllib.loads((cases / "examples_mosaic.toml").read_text())
        automatic = next(
            entry
            for entry in authority["automatic_example"]
            if entry["path"] == "mosaic/main.rue"
        )
        self.assertEqual(automatic["tier"], "slow")
        self.assertEqual(automatic["contract"], "heavyweight_long")
        self.assertEqual(mosaic["section"]["tier"], "slow")
        self.assertEqual(mosaic["section"]["contract"], "heavyweight_long")
        self.assertEqual(
            authority["contract"]["heavyweight_long"]["timeout_profile"], "slow"
        )

    def test_shards_use_lpt_expected_cost_plus_headroom(self):
        with tempfile.TemporaryDirectory() as directory:
            weights = Path(directory) / "weights.json"
            weights.write_text(
                '{"version":1,"default_ms":1,"common":{"a":1000,"b":800,"c":600},'
                '"platforms":{"macos":{"c":1000}}}'
            )
            policy = {
                "expected_cost_multiplier_percent": 150,
                "fixed_headroom_ms": 500,
                "minimum_shard_timeout_ms": 1,
                "minimum_monolith_timeout_ms": 1,
                "minimum_slow_suite_timeout_ms": 9000,
            }
            timeout, expected = MODULE.timeout_for_target(
                "//:cli-tests-shard-0", weights, "macos", policy
            )
            self.assertEqual(expected, 1000)
            self.assertEqual(timeout, 2000)

    def test_minimum_prevents_sparse_weight_underbudget(self):
        policy = {
            "expected_cost_multiplier_percent": 150,
            "fixed_headroom_ms": 500,
            "minimum_shard_timeout_ms": 5000,
        }
        self.assertEqual(MODULE.derive_timeout_ms(10, 5000, policy), 5000)

    def test_raw_contract_deadlines_are_rejected(self):
        text = """
[timeout_profile.ordinary]
compile_hang_timeout_ms=1
runtime_hang_timeout_ms=1
[timeout_profile.slow]
compile_hang_timeout_ms=2
runtime_hang_timeout_ms=2
[timeout_profile.stress]
compile_hang_timeout_ms=3
runtime_hang_timeout_ms=3
[timeout_policy]
expected_cost_multiplier_percent=100
fixed_headroom_ms=1
minimum_shard_timeout_ms=1
minimum_monolith_timeout_ms=1
minimum_slow_suite_timeout_ms=1
[contract.bad]
class="ordinary"
compile_timeout_ms=10
"""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "policy.toml"
            path.write_text(text)
            with self.assertRaisesRegex(ValueError, "raw deadlines are forbidden"):
                MODULE.load_policy(path)


class BuckActionBoundTests(unittest.TestCase):
    """RUE-1163: the BUCK action bounds must cover the derived deadlines."""

    POLICY = {
        "expected_cost_multiplier_percent": 100,
        "fixed_headroom_ms": 0,
        "minimum_shard_timeout_ms": 1000,
        "minimum_monolith_timeout_ms": 2000,
        "minimum_slow_suite_timeout_ms": 3000,
    }

    def fixture(self, directory, cli_seconds):
        weights = Path(directory) / "shard-weights.json"
        weights.write_text(
            json.dumps(
                {
                    "version": 1,
                    "default_ms": 1,
                    "common": {"a": 1000, "b": 1000},
                    "platforms": {"linux-x64": {}},
                }
            )
        )
        buck = Path(directory) / "BUCK"
        buck.write_text(
            f"_CLI_TESTS_TIMEOUT_SECONDS = {cli_seconds}\n"
            "_CLI_SHARD_TIMEOUT_SECONDS = 9999\n"
            'cached_corpus_suite(\n    name = "cli-tests-slow",\n'
            "    timeout_seconds = 9999,\n)\n"
        )
        return buck, weights

    def test_bound_covering_the_deadline_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            buck, weights = self.fixture(directory, 9999)
            self.assertEqual(
                MODULE.check_buck_timeouts(buck, weights, self.POLICY), []
            )

    def test_bound_inside_the_deadline_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            buck, weights = self.fixture(directory, 1)
            errors = MODULE.check_buck_timeouts(buck, weights, self.POLICY)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("//:cli-tests", errors[0])
            self.assertIn("A healthy run would be killed", errors[0])

    def test_missing_bound_is_an_error_not_a_silent_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            buck, weights = self.fixture(directory, 9999)
            buck.write_text("# no timeouts here\n")
            with self.assertRaisesRegex(ValueError, "no action timeout found"):
                MODULE.check_buck_timeouts(buck, weights, self.POLICY)


if __name__ == "__main__":
    unittest.main()
