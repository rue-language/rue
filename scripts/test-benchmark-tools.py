#!/usr/bin/env python3
"""Regression tests for benchmark validation and dashboard aggregation."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


INPUT_ROOT = Path(
    os.environ.get("RUE_BENCHMARK_TEST_ROOT", Path(__file__).resolve().parent.parent)
)
sys.path.insert(0, str(INPUT_ROOT / "scripts"))


def load_module(name: str, relative_path: str):
    path = INPUT_ROOT / relative_path
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


validator = load_module("validate_benchmark", "scripts/validate-benchmark.py")
charts = load_module("generate_charts", "scripts/generate-charts.py")


class BenchmarkValidationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.expected_names = validator.load_manifest_names(
            INPUT_ROOT / "benchmarks" / "manifest.toml"
        )

    def valid_result(self):
        return {
            "version": 1,
            "timestamp": "2026-07-09T00:00:00Z",
            "commit": "probe",
            "benchmarks": [
                {"name": name, "mean_ms": index + 0.5}
                for index, name in enumerate(self.expected_names)
            ],
        }

    def test_exact_manifest_corpus_is_accepted(self):
        result = self.valid_result()
        self.assertEqual(validator.validate_results(result, self.expected_names), [])
        result["benchmarks"].reverse()
        self.assertEqual(validator.validate_results(result, self.expected_names), [])

    def test_missing_unknown_and_duplicate_names_are_rejected(self):
        missing = self.valid_result()
        removed = missing["benchmarks"].pop()["name"]
        self.assertIn(
            f"missing benchmark result(s): {removed}",
            validator.validate_results(missing, self.expected_names),
        )

        unknown = self.valid_result()
        unknown["benchmarks"][-1]["name"] = "not_in_manifest"
        errors = validator.validate_results(unknown, self.expected_names)
        self.assertTrue(any("missing benchmark result" in error for error in errors))
        self.assertIn("unknown benchmark result name(s): not_in_manifest", errors)

        duplicate = self.valid_result()
        duplicate["benchmarks"].append(duplicate["benchmarks"][0].copy())
        self.assertIn(
            f"duplicate benchmark result name(s): {self.expected_names[0]}",
            validator.validate_results(duplicate, self.expected_names),
        )

    def test_non_numeric_mean_is_rejected(self):
        result = self.valid_result()
        result["benchmarks"][0]["mean_ms"] = "fast"
        errors = validator.validate_results(result, self.expected_names)
        self.assertIn(
            f"benchmark '{self.expected_names[0]}' has no numeric mean_ms", errors
        )

    def test_empty_and_malformed_corpora_are_rejected(self):
        self.assertEqual(
            validator.validate_results({"benchmarks": []}, self.expected_names),
            ["no benchmark results collected; all benchmarks failed"],
        )
        errors = validator.validate_results(
            {"benchmarks": ["not an object"]}, self.expected_names
        )
        self.assertIn("benchmark #1 must be an object", errors)
        self.assertTrue(any("missing benchmark result" in error for error in errors))

    def test_duplicate_and_malformed_manifest_entries_are_rejected(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            manifest_path = Path(temp_dir) / "manifest.toml"
            manifest_path.write_text(
                '[[benchmark]]\nname = "same"\npath = "a.rue"\n'
                '[[benchmark]]\nname = "same"\npath = "b.rue"\n'
            )
            with self.assertRaisesRegex(ValueError, "duplicate benchmark name"):
                validator.load_manifest_names(manifest_path)

            manifest_path.write_text('[[benchmark]]\npath = "missing-name.rue"\n')
            with self.assertRaisesRegex(ValueError, "has no valid name"):
                validator.load_manifest_names(manifest_path)

    def test_ci_validator_and_history_append_both_reject_partial_corpus(self):
        result = self.valid_result()
        removed = result["benchmarks"].pop()["name"]

        with tempfile.TemporaryDirectory() as temp_dir:
            result_path = Path(temp_dir) / "partial.json"
            history_path = Path(temp_dir) / "history.json"
            result_path.write_text(json.dumps(result))
            validator_process = subprocess.run(
                [
                    sys.executable,
                    str(INPUT_ROOT / "scripts" / "validate-benchmark.py"),
                    str(result_path),
                    "--manifest",
                    str(INPUT_ROOT / "benchmarks" / "manifest.toml"),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            appender_process = subprocess.run(
                [
                    sys.executable,
                    str(INPUT_ROOT / "scripts" / "append-benchmark.py"),
                    str(result_path),
                    str(history_path),
                    "--manifest",
                    str(INPUT_ROOT / "benchmarks" / "manifest.toml"),
                ],
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertNotEqual(validator_process.returncode, 0)
            self.assertIn(
                f"missing benchmark result(s): {removed}", validator_process.stdout
            )
            self.assertNotEqual(appender_process.returncode, 0)
            self.assertIn(
                f"missing benchmark result(s): {removed}", appender_process.stderr
            )
            self.assertFalse(history_path.exists())

            result_path.write_text(json.dumps(self.valid_result()))
            valid_process = subprocess.run(
                [
                    sys.executable,
                    str(INPUT_ROOT / "scripts" / "append-benchmark.py"),
                    str(result_path),
                    str(history_path),
                    "--manifest",
                    str(INPUT_ROOT / "benchmarks" / "manifest.toml"),
                    "--reason",
                    "push",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(valid_process.returncode, 0, valid_process.stderr)
            published = json.loads(history_path.read_text())["runs"]
            self.assertEqual(len(published), 1)
            self.assertEqual(published[0]["version"], 2)
            self.assertEqual(published[0]["benchmark_reason"], "push")
            self.assertEqual(published[0]["commit_range"], ["probe"])


class ChartAggregationTests(unittest.TestCase):
    def setUp(self):
        self.run = {
            "benchmarks": [
                {
                    "name": "first",
                    "mean_ms": 10,
                    "peak_memory_bytes": 2 * 1024 * 1024,
                    "binary_size_bytes": 3 * 1024,
                    "passes": {
                        "parallel_astgen": {"mean_ms": 1},
                        "sema": {"mean_ms": 2},
                        "compile": {"mean_ms": 9},
                    },
                },
                {
                    "name": "second",
                    "mean_ms": 20,
                    "peak_memory_bytes": 5 * 1024 * 1024,
                    "binary_size_bytes": 7 * 1024,
                    "passes": {
                        "parallel_astgen": {"mean_ms": 3},
                        "sema": {"mean_ms": 4},
                        "new_pass": {"mean_ms": 5},
                        "parse": {"mean_ms": 8},
                    },
                },
            ]
        }

    def test_suite_metrics_cover_every_benchmark(self):
        self.assertEqual(charts.get_total_time(self.run), 30)
        self.assertEqual(
            charts.get_pass_times(self.run),
            {"parallel_astgen": 4, "sema": 6, "new_pass": 5},
        )
        self.assertEqual(charts.get_peak_memory(self.run), 5)
        self.assertEqual(charts.get_binary_size(self.run), 10)

        summary = charts.generate_summary_data([self.run])
        self.assertEqual(summary["latest_time_ms"], 30)
        self.assertEqual(summary["latest_memory_mb"], 5)
        self.assertEqual(summary["latest_binary_kb"], 10)

    def test_suite_metrics_are_invariant_to_manifest_order(self):
        reversed_run = {"benchmarks": list(reversed(self.run["benchmarks"]))}
        self.assertEqual(
            charts.get_total_time(reversed_run), charts.get_total_time(self.run)
        )
        self.assertEqual(
            charts.get_pass_times(reversed_run), charts.get_pass_times(self.run)
        )
        self.assertEqual(
            charts.get_peak_memory(reversed_run), charts.get_peak_memory(self.run)
        )
        self.assertEqual(
            charts.get_binary_size(reversed_run), charts.get_binary_size(self.run)
        )

    def test_legacy_total_time_schema_is_summed(self):
        run = {
            "benchmarks": [
                {"name": "a", "total_ms": {"mean": 1.5}},
                {"name": "b", "total_ms": 2.5},
            ]
        }
        self.assertEqual(charts.get_total_time(run), 4)


if __name__ == "__main__":
    unittest.main()
