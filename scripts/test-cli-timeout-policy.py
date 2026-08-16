#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

MODULE = load_script("cli-timeout-policy.py", __file__)


class TimeoutPolicyTests(unittest.TestCase):
    def test_mosaic_section_and_automatic_example_use_slow_profile(self):
        # JSON twins materialized by //crates/rue-toml2json genrules (RUE-1524).
        authority = json.loads(
            Path(os.environ["RUE_CLI_CONTRACTS_JSON"]).read_text()
        )
        mosaic = json.loads(Path(os.environ["RUE_CLI_MOSAIC_JSON"]).read_text())
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

    def test_shards_consume_reported_loads_plus_headroom(self):
        # The packing itself lives in the harness (RUE_CLI_EMIT_SHARD_LOADS);
        # this gate only applies policy arithmetic to the loads it reported.
        loads = {
            "version": 1,
            "shard_count": 4,
            "platforms": {
                "macos": {
                    "loads_ms": [1000, 800, 600, 600],
                    "case_counts": [1, 1, 1, 1],
                    "total_cases": 4,
                }
            },
        }
        policy = {
            "expected_cost_multiplier_percent": 150,
            "fixed_headroom_ms": 500,
            "minimum_shard_timeout_ms": 1,
            "minimum_monolith_timeout_ms": 1,
            "minimum_slow_suite_timeout_ms": 9000,
        }
        timeout, expected = MODULE.timeout_for_target(
            "//:cli-tests-shard-0", loads, "macos", policy
        )
        self.assertEqual(expected, 1000)
        self.assertEqual(timeout, 2000)
        # The monolith's expected cost is the whole reported inventory.
        timeout, expected = MODULE.timeout_for_target(
            "//:cli-tests", loads, "macos", policy
        )
        self.assertEqual(expected, 3000)
        self.assertEqual(timeout, 5000)

    def test_unmodeled_platform_and_bad_shard_index_are_rejected(self):
        loads = {
            "version": 1,
            "shard_count": 2,
            "platforms": {"linux-x64": {"loads_ms": [10, 10]}},
        }
        policy = {
            "expected_cost_multiplier_percent": 100,
            "fixed_headroom_ms": 0,
            "minimum_shard_timeout_ms": 1,
            "minimum_monolith_timeout_ms": 1,
            "minimum_slow_suite_timeout_ms": 1,
        }
        with self.assertRaisesRegex(ValueError, "do not model platform"):
            MODULE.timeout_for_target("//:cli-tests-shard-0", loads, "macos", policy)
        with self.assertRaisesRegex(ValueError, "invalid CLI shard index"):
            MODULE.timeout_for_target(
                "//:cli-tests-shard-2", loads, "linux-x64", policy
            )
        with self.assertRaisesRegex(ValueError, "requires --shard-loads"):
            MODULE.timeout_for_target("//:cli-tests", None, "linux-x64", policy)

    def test_shard_loads_file_is_validated(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "shard-loads.json"
            good = {
                "version": 1,
                "shard_count": 2,
                "platforms": {"linux-x64": {"loads_ms": [5, 5]}},
            }
            path.write_text(json.dumps(good))
            self.assertEqual(MODULE.load_shard_loads(path), good)
            for mutation, message in [
                ({"version": 2}, "unsupported shard-loads version"),
                ({"shard_count": 0}, "shard_count must be a positive integer"),
                ({"platforms": {}}, "platforms must be a non-empty object"),
                (
                    # One load per shard, or the report models a different
                    # shard topology than the one BUCK declares.
                    {"platforms": {"linux-x64": {"loads_ms": [5]}}},
                    "must list 2 non-negative integer loads",
                ),
            ]:
                path.write_text(json.dumps({**good, **mutation}))
                with self.assertRaisesRegex(ValueError, message):
                    MODULE.load_shard_loads(path)

    def test_minimum_prevents_sparse_weight_underbudget(self):
        policy = {
            "expected_cost_multiplier_percent": 150,
            "fixed_headroom_ms": 500,
            "minimum_shard_timeout_ms": 5000,
        }
        self.assertEqual(MODULE.derive_timeout_ms(10, 5000, policy), 5000)

    def test_main_reports_a_malformed_policy_and_exits_two(self):
        # The one test that enters main()'s error path. RUE-1524 left a
        # stale tomllib name in the except tuple, and because Python only
        # evaluates that tuple when an exception is raised, every failure
        # of the gate died as a NameError traceback while the passing path
        # stayed green. Exercising the path is what pins the contract:
        # a diagnosable `error:` line and exit 2, not a traceback.
        with tempfile.TemporaryDirectory() as directory:
            policy = Path(directory) / "policy.json"
            policy.write_text("{ not json")
            result = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).with_name("cli-timeout-policy.py")),
                    "--policy",
                    str(policy),
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("error:", result.stderr)
            self.assertNotIn("Traceback", result.stderr)

    def test_raw_contract_deadlines_are_rejected(self):
        # The gate reads the JSON twin the build derives from the authored
        # TOML, so the fixture is that JSON shape directly (RUE-1524).
        data = {
            "timeout_profile": {
                "ordinary": {"compile_hang_timeout_ms": 1, "runtime_hang_timeout_ms": 1},
                "slow": {"compile_hang_timeout_ms": 2, "runtime_hang_timeout_ms": 2},
                "stress": {"compile_hang_timeout_ms": 3, "runtime_hang_timeout_ms": 3},
            },
            "timeout_policy": {
                "expected_cost_multiplier_percent": 100,
                "fixed_headroom_ms": 1,
                "minimum_shard_timeout_ms": 1,
                "minimum_monolith_timeout_ms": 1,
                "minimum_slow_suite_timeout_ms": 1,
            },
            "contract": {
                "bad": {"class": "ordinary", "compile_timeout_ms": 10},
            },
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "policy.json"
            path.write_text(json.dumps(data))
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

    LOADS = {
        "version": 1,
        "shard_count": 2,
        "platforms": {"linux-x64": {"loads_ms": [1000, 1000]}},
    }

    def fixture(self, directory, cli_seconds):
        buck = Path(directory) / "BUCK"
        buck.write_text(
            f"_CLI_TESTS_TIMEOUT_SECONDS = {cli_seconds}\n"
            "_CLI_SHARD_TIMEOUT_SECONDS = 9999\n"
            'cached_corpus_suite(\n    name = "cli-tests-slow",\n'
            "    timeout_seconds = 9999,\n)\n"
        )
        return buck

    def test_bound_covering_the_deadline_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            buck = self.fixture(directory, 9999)
            self.assertEqual(
                MODULE.check_buck_timeouts(buck, self.LOADS, self.POLICY), []
            )

    def test_bound_inside_the_deadline_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            buck = self.fixture(directory, 1)
            errors = MODULE.check_buck_timeouts(buck, self.LOADS, self.POLICY)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("//:cli-tests", errors[0])
            self.assertIn("A healthy run would be killed", errors[0])

    def test_missing_bound_is_an_error_not_a_silent_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            buck = self.fixture(directory, 9999)
            buck.write_text("# no timeouts here\n")
            with self.assertRaisesRegex(ValueError, "no action timeout found"):
                MODULE.check_buck_timeouts(buck, self.LOADS, self.POLICY)


if __name__ == "__main__":
    unittest.main()
