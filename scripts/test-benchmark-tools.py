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
from unittest import mock


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
collection = load_module("benchmark_collection", "scripts/benchmark_collection.py")
charts = load_module("generate_charts", "scripts/generate-charts.py")
site_status = load_module("generate_site_status", "scripts/generate-site-status.py")
history_tools = load_module("benchmark_history", "scripts/benchmark_history.py")
metrics = load_module("benchmark_metrics", "scripts/benchmark_metrics.py")
perf_baseline = load_module("perf_baseline", "scripts/perf-baseline.py")


class PerfBaselineAggregationTests(unittest.TestCase):
    @staticmethod
    def sample(
        compile_ms,
        process_ms,
        leaf_names=("lexer", "parser", "codegen"),
        leaf_durations=None,
    ):
        durations = leaf_durations or {"lexer": 3.0, "parser": 2.0, "codegen": 4.0}
        passes = [
            {
                "name": "compile",
                "duration_ms": compile_ms,
                "percent": 100.0,
                "invocations": 1,
                "root_invocations": 1,
                "leaf_invocations": 0,
            },
            {
                "name": "parse",
                "duration_ms": 5.5,
                "percent": 50.0,
                "invocations": 1,
                "root_invocations": 0,
                "leaf_invocations": 0,
            },
        ]
        for name in leaf_names:
            passes.append(
                {
                    "name": name,
                    "duration_ms": durations.get(name, 1.0),
                    "percent": 0.0,
                    "invocations": 1,
                    "root_invocations": 0,
                    "leaf_invocations": 1,
                }
            )
        return {
            "schema_version": 2,
            "timing_model": "inclusive_spans",
            "total_ms": compile_ms,
            "passes": passes,
            "source_metrics": {"files": 1, "bytes": 10, "lines": 1, "tokens": 5},
            "metadata": {
                "timestamp": "2026-07-11T00:00:00Z",
                "target": "x86-64-linux",
                "version": "0.1.0",
            },
            "peak_memory_bytes": 1024,
            "_process_ms": process_ms,
        }

    def test_v2_uses_paired_root_and_process_samples(self):
        result = perf_baseline.aggregate(
            [
                self.sample(10.0, 15.0),
                self.sample(12.0, 17.0),
                self.sample(11.0, 15.0),
            ]
        )
        self.assertEqual(result["compile_ms"], 11.0)
        self.assertEqual(result["compile_mad_ms"], 1.0)
        self.assertEqual(result["process_ms"], 15.0)
        self.assertEqual(result["process_mad_ms"], 0.0)
        # This is median(process - compile), not the difference of the two
        # marginal medians (which would be 4 ms for these samples).
        self.assertEqual(result["driver_overhead_ms"], 5.0)
        self.assertEqual(result["driver_overhead_mad_ms"], 0.0)
        self.assertAlmostEqual(
            result["leaf_to_root_ratio_percent"], 9.0 / 11.0 * 100.0
        )
        self.assertAlmostEqual(
            result["leaf_to_root_ratio_mad_percent"],
            9.0 / 11.0 * 100.0 - 75.0,
        )
        self.assertEqual(result["unattributed_ms"], 2.0)
        self.assertEqual(result["unattributed_mad_ms"], 1.0)
        self.assertEqual(result["overlapping_leaf_work_ms"], 0.0)
        self.assertEqual(result["overlapping_leaf_work_mad_ms"], 0.0)
        self.assertEqual(result["peak_memory_bytes"], 1024.0)
        self.assertEqual(result["peak_memory_mad_bytes"], 0.0)
        self.assertEqual(
            [name for name, _ms, _percent in result["rows"]],
            ["lexer", "parser", "codegen"],
        )

    def test_current_leaf_order_includes_semantic_lowering_and_frontend_indexes(self):
        result = perf_baseline.aggregate(
            [
                self.sample(
                    10.0,
                    12.0,
                    leaf_names=(
                        "codegen",
                        "rir_declaration_index",
                        "sema",
                        "semantic_astgen",
                        "definition_snapshot",
                        "parser",
                        "lexer",
                    ),
                    leaf_durations={
                        "codegen": 1.0,
                        "rir_declaration_index": 1.0,
                        "sema": 1.0,
                        "semantic_astgen": 1.0,
                        "definition_snapshot": 1.0,
                        "parser": 1.0,
                        "lexer": 1.0,
                    },
                )
            ]
        )
        self.assertEqual(
            [name for name, _ms, _percent in result["rows"]],
            [
                "lexer",
                "parser",
                "definition_snapshot",
                "semantic_astgen",
                "rir_declaration_index",
                "sema",
                "codegen",
            ],
        )

    def test_leaf_accounting_is_paired_before_taking_medians(self):
        # Every individual run has exactly 100% sequential leaf coverage. The
        # independent leaf medians are nevertheless 5 + 5 while the root
        # median is only 5; summing marginal medians would invent 5 ms overlap.
        result = perf_baseline.aggregate(
            [
                self.sample(
                    5.0,
                    6.0,
                    leaf_names=("lexer", "parser"),
                    leaf_durations={"lexer": 5.0, "parser": 0.0},
                ),
                self.sample(
                    5.0,
                    12.0,
                    leaf_names=("lexer", "parser"),
                    leaf_durations={"lexer": 0.0, "parser": 5.0},
                ),
                self.sample(
                    10.0,
                    13.0,
                    leaf_names=("lexer", "parser"),
                    leaf_durations={"lexer": 5.0, "parser": 5.0},
                ),
            ]
        )
        self.assertEqual(result["compile_ms"], 5.0)
        self.assertEqual(sum(row[1] for row in result["rows"]), 10.0)
        self.assertEqual(
            result["rows"], [("lexer", 5.0, 50.0), ("parser", 5.0, 50.0)]
        )
        self.assertEqual(result["leaf_to_root_ratio_percent"], 100.0)
        self.assertEqual(result["leaf_to_root_ratio_mad_percent"], 0.0)
        self.assertEqual(result["unattributed_ms"], 0.0)
        self.assertEqual(result["overlapping_leaf_work_ms"], 0.0)
        self.assertEqual(result["driver_overhead_ms"], 3.0)
        self.assertEqual(result["driver_overhead_mad_ms"], 2.0)

    def test_legacy_json_uses_compile_span_instead_of_inflated_total(self):
        sample = {
            "total_ms": 22.0,
            "passes": [
                {"name": "compile", "duration_ms": 10.0},
                {"name": "parse", "duration_ms": 6.0},
                {"name": "parse_file", "duration_ms": 5.0},
                {"name": "codegen", "duration_ms": 4.0},
            ],
            "_process_ms": 12.0,
        }
        result = perf_baseline.aggregate([sample])
        self.assertEqual(result["compile_ms"], 10.0)
        self.assertEqual(
            result["rows"],
            [("parse_file", 5.0, 50.0), ("codegen", 4.0, 40.0)],
        )

    def test_rejects_leaf_pass_set_drift(self):
        first = self.sample(10.0, 12.0)
        second = self.sample(
            10.0, 12.0, leaf_names=("lexer", "parser", "codegen", "new_pass")
        )
        with self.assertRaisesRegex(ValueError, "leaf pass set drifted"):
            perf_baseline.aggregate([first, second])

    def test_rejects_inconsistent_root_or_process_measurement(self):
        wrong_total = self.sample(10.0, 12.0)
        wrong_total["total_ms"] = 11.0
        with self.assertRaisesRegex(ValueError, "total_ms does not match compile span"):
            perf_baseline.aggregate([wrong_total])

        impossible_process = self.sample(10.0, 9.0)
        with self.assertRaisesRegex(ValueError, "process time is shorter"):
            perf_baseline.aggregate([impossible_process])

    def test_rejects_unknown_or_non_integer_schema_versions(self):
        for bad_version in (0, 3, True, 2.0, "2"):
            with self.subTest(schema_version=bad_version):
                sample = self.sample(10.0, 12.0)
                sample["schema_version"] = bad_version
                with self.assertRaisesRegex(ValueError, "integer 1 or 2"):
                    perf_baseline.aggregate([sample])

    def test_rejects_malformed_v2_invocation_counters(self):
        mutations = [
            ("invocations", 0, "greater than zero"),
            ("invocations", 1.5, "non-negative integer"),
            ("invocations", True, "non-negative integer"),
            ("root_invocations", -1, "non-negative integer"),
            ("root_invocations", 2, "exceeds invocations"),
            ("leaf_invocations", 2, "exceeds invocations"),
        ]
        for field, value, message in mutations:
            with self.subTest(field=field, value=value):
                sample = self.sample(10.0, 12.0)
                sample["passes"][0][field] = value
                with self.assertRaisesRegex(ValueError, message):
                    perf_baseline.aggregate([sample])

    def test_rejects_v2_rows_that_do_not_describe_compile_rooted_leaves(self):
        cases = [
            (
                "compile not root",
                lambda sample: sample["passes"][0].update(root_invocations=0),
                "every invocation at the root",
            ),
            (
                "only some compile invocations are roots",
                lambda sample: sample["passes"][0].update(
                    invocations=2, root_invocations=1
                ),
                "every invocation at the root",
            ),
            (
                "compile is leaf",
                lambda sample: sample["passes"][0].update(leaf_invocations=1),
                "compile span must not be a leaf",
            ),
            (
                "selected phase is root",
                lambda sample: sample["passes"][2].update(root_invocations=1),
                "compile span must be the sole root",
            ),
            (
                "aggregate phase is another root",
                lambda sample: sample["passes"][1].update(root_invocations=1),
                "compile span must be the sole root",
            ),
        ]
        for label, mutate, message in cases:
            with self.subTest(label=label):
                sample = self.sample(10.0, 12.0)
                mutate(sample)
                with self.assertRaisesRegex(ValueError, message):
                    perf_baseline.aggregate([sample])

    def test_rejects_positive_leaf_work_with_zero_compile_time(self):
        sample = self.sample(
            0.0,
            0.0,
            leaf_durations={"lexer": 1.0, "parser": 0.0, "codegen": 0.0},
        )
        with self.assertRaisesRegex(ValueError, "positive leaf work with a zero"):
            perf_baseline.aggregate([sample])

    def test_rejects_malformed_v2_source_metrics(self):
        cases = [
            (None, "must be an object"),
            ([], "must be an object"),
            (
                {"files": 0, "bytes": 10, "lines": 1, "tokens": 5},
                "files must be greater than zero",
            ),
            (
                {"files": True, "bytes": 10, "lines": 1, "tokens": 5},
                "files must be a non-negative integer",
            ),
            (
                {"files": 1, "bytes": False, "lines": 1, "tokens": 5},
                "bytes must be a non-negative integer",
            ),
            (
                {"files": 1, "bytes": 10, "lines": -1, "tokens": 5},
                "lines must be a non-negative integer",
            ),
            (
                {"files": 1, "bytes": 10, "lines": 1, "tokens": 2.5},
                "tokens must be a non-negative integer",
            ),
            (
                {"files": 1, "bytes": 10, "lines": 1},
                "tokens must be a non-negative integer",
            ),
        ]
        for source_metrics, message in cases:
            with self.subTest(source_metrics=source_metrics):
                sample = self.sample(10.0, 12.0)
                sample["source_metrics"] = source_metrics
                with self.assertRaisesRegex(ValueError, message):
                    perf_baseline.aggregate([sample])

    def test_malformed_sample_and_metadata_have_validation_errors(self):
        with self.assertRaisesRegex(ValueError, "sample 1 must be an object"):
            perf_baseline.aggregate([None])

        sample = self.sample(10.0, 12.0)
        sample["metadata"] = []
        with self.assertRaisesRegex(ValueError, "metadata must be an object"):
            perf_baseline.aggregate([sample])

    def test_rejects_missing_or_non_string_v2_metadata_fields(self):
        cases = [
            ({}, "metadata target must be a non-empty string"),
            (
                {
                    "timestamp": "2026-07-11T00:00:00Z",
                    "target": "",
                    "version": "0.1.0",
                },
                "metadata target must be a non-empty string",
            ),
            (
                {
                    "timestamp": "2026-07-11T00:00:00Z",
                    "target": 42,
                    "version": "0.1.0",
                },
                "metadata target must be a non-empty string",
            ),
            (
                {
                    "timestamp": "2026-07-11T00:00:00Z",
                    "target": "x86-64-linux",
                },
                "metadata version must be a non-empty string",
            ),
            (
                {
                    "timestamp": "2026-07-11T00:00:00Z",
                    "target": "x86-64-linux",
                    "version": "",
                },
                "metadata version must be a non-empty string",
            ),
            (
                {
                    "timestamp": "2026-07-11T00:00:00Z",
                    "target": "x86-64-linux",
                    "version": False,
                },
                "metadata version must be a non-empty string",
            ),
            (
                {"target": "x86-64-linux", "version": "0.1.0"},
                "metadata timestamp must be a non-empty string",
            ),
            (
                {
                    "timestamp": 0,
                    "target": "x86-64-linux",
                    "version": "0.1.0",
                },
                "metadata timestamp must be a non-empty string",
            ),
        ]
        for metadata, message in cases:
            with self.subTest(metadata=metadata):
                sample = self.sample(10.0, 12.0)
                sample["metadata"] = metadata
                with self.assertRaisesRegex(ValueError, message):
                    perf_baseline.aggregate([sample])

    def test_hot_pass_share_uses_compiler_time_and_markdown_keeps_new_passes(self):
        result = {
            "name": "probe",
            "bucket": "small",
            "compile_ms": {"median": 20.0, "mad": 0.0},
            "process_ms": {"median": 25.0, "mad": 0.0},
            "passes": [
                {"name": "lexer", "median_ms": 5.0, "median_percent": 25.0},
                {
                    "name": "future_phase",
                    "median_ms": 3.0,
                    "median_percent": 15.0,
                },
            ],
        }
        self.assertEqual(
            perf_baseline.hot_passes([result]),
            [("lexer", 5.0, 25.0), ("future_phase", 3.0, 15.0)],
        )
        markdown = perf_baseline._md_table([result])
        self.assertIn("future_phase", markdown)
        self.assertIn("**20.00**", markdown)
        self.assertIn("**25.00**", markdown)

    def test_corpus_json_names_paired_phase_ratio_as_a_median(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            with mock.patch.object(perf_baseline.sys, "stderr"):
                with mock.patch.object(
                    perf_baseline,
                    "compile_once",
                    return_value=self.sample(10.0, 12.0),
                ):
                    results = perf_baseline.run_corpus(
                        "rue", [], 1, 0, temp_dir, 1.0, 1
                    )
        phase = results[0]["passes"][0]
        self.assertEqual(phase["median_percent"], 30.0)
        self.assertNotIn("percent", phase)

    def test_compile_once_sanitizes_logging_checks_artifact_and_exact_json(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            output = Path(temp_dir) / "program"
            sample = self.sample(10.0, 12.0)

            def successful_run(cmd, **kwargs):
                self.assertNotIn("RUST_LOG", kwargs["env"])
                Path(cmd[-1]).write_bytes(b"artifact")
                return subprocess.CompletedProcess(
                    cmd, 0, stdout=json.dumps(sample), stderr=""
                )

            with mock.patch.dict(os.environ, {"RUST_LOG": "trace"}):
                with mock.patch.object(
                    perf_baseline.subprocess, "run", side_effect=successful_run
                ):
                    result = perf_baseline.compile_once(
                        "rue", ["main.rue"], str(output), 1.0, 1
                    )
            self.assertEqual(result["schema_version"], 2)
            self.assertFalse(output.exists())

            def contaminated_run(cmd, **_kwargs):
                Path(cmd[-1]).write_bytes(b"artifact")
                return subprocess.CompletedProcess(
                    cmd, 0, stdout="debug spew\n{}", stderr=""
                )

            with mock.patch.object(
                perf_baseline.subprocess, "run", side_effect=contaminated_run
            ):
                with self.assertRaisesRegex(RuntimeError, "exactly one benchmark JSON"):
                    perf_baseline.compile_once(
                        "rue", ["main.rue"], str(output), 1.0, 1
                    )
            self.assertFalse(output.exists())

            with mock.patch.object(
                perf_baseline.subprocess,
                "run",
                return_value=subprocess.CompletedProcess(
                    ["rue"], 0, stdout=json.dumps(sample), stderr=""
                ),
            ):
                with self.assertRaisesRegex(RuntimeError, "without producing"):
                    perf_baseline.compile_once(
                        "rue", ["main.rue"], str(output), 1.0, 1
                    )


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
            "build_mode": "release",
            "iterations": 5,
            "benchmarks": [
                {
                    "name": name,
                    "mean_ms": index + 0.5,
                    "std_ms": 0.1,
                    "iterations": 5,
                    "samples_ms": [index + 0.5] * 5,
                    "timing_schema_version": 2,
                    "passes": {"compile": {"mean_ms": index + 0.5}},
                }
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
            f"benchmark '{self.expected_names[0]}' has no finite non-negative mean_ms",
            errors,
        )

    def test_rejects_partial_and_mixed_iteration_counts(self):
        partial = self.valid_result()
        partial["benchmarks"][0]["iterations"] = 1
        self.assertIn(
            f"benchmark '{self.expected_names[0]}' iterations (1) does not match "
            "root iterations (5)",
            validator.validate_results(partial, self.expected_names),
        )

        mixed = self.valid_result()
        mixed["benchmarks"][1]["iterations"] = 4
        errors = validator.validate_results(mixed, self.expected_names)
        self.assertTrue(any("iterations (4) does not match" in error for error in errors))

    def test_rejects_malformed_counts_timing_and_pass_data(self):
        mutations = [
            (lambda result: result.update(iterations=True), "iterations must be"),
            (lambda result: result.update(iterations=0), "iterations must be"),
            (
                lambda result: result["benchmarks"][0].update(iterations=1.0),
                "iterations must be a positive integer",
            ),
            (
                lambda result: result["benchmarks"][0].update(mean_ms=float("inf")),
                "finite non-negative mean_ms",
            ),
            (
                lambda result: result["benchmarks"][0].update(std_ms=-0.1),
                "finite non-negative std_ms",
            ),
            (
                lambda result: result["benchmarks"][0].update(passes=[]),
                "passes must be an object",
            ),
            (
                lambda result: result["benchmarks"][0]["passes"]["compile"].update(
                    mean_ms=float("nan")
                ),
                "finite non-negative mean_ms",
            ),
        ]
        for mutate, message in mutations:
            with self.subTest(message=message):
                result = self.valid_result()
                mutate(result)
                self.assertTrue(
                    any(
                        message in error
                        for error in validator.validate_results(
                            result, self.expected_names
                        )
                    )
                )


    def test_empty_and_malformed_corpora_are_rejected(self):
        self.assertEqual(
            validator.validate_results(
                {"iterations": 1, "benchmarks": []}, self.expected_names
            ),
            ["no benchmark results collected; all benchmarks failed"],
        )
        errors = validator.validate_results(
            {"iterations": 1, "benchmarks": ["not an object"]}, self.expected_names
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
                    "--platform",
                    "x86-64-linux",
                    "--runner-image",
                    "test-runner-v1",
                    "--reason",
                    "push",
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
                    "--reason", "push",
                    "--platform", "x86-64-linux",
                    "--runner-image", "test-runner-v1",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(valid_process.returncode, 0, valid_process.stderr)
            published = history_tools.load_history(history_path)["runs"]
            self.assertEqual(len(published), 1)
            self.assertEqual(published[0]["version"], 3)
            self.assertEqual(published[0]["publication"]["trigger_reason"], "push")
            self.assertEqual(published[0]["publication"]["coverage"]["represented_commits"], ["probe"])
            self.assertEqual(
                published[0]["publication"]["metric_semantics"],
                {
                    "measurement_family": "compiler",
                    "scenario_family": "cold_compilation",
                },
            )


class DurableBenchmarkHistoryTests(unittest.TestCase):
    @staticmethod
    def sample_run(index: int, month: int = 7) -> dict:
        return {
            "timestamp": f"2026-{month:02d}-{index % 28 + 1:02d}T00:00:00Z",
            "commit": f"commit-{index}",
            "iterations": 2,
            "benchmarks": [],
        }

    def test_partitioned_history_retains_more_than_one_hundred_runs(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history-x86-64-linux"
            runs = [self.sample_run(index, 6 if index < 40 else 7) for index in range(101)]
            history_tools.save_history(path, runs, "x86-64-linux")
            loaded = history_tools.load_history(path)
            self.assertEqual(loaded["runs"], runs)
            self.assertEqual(loaded["run_count"], 101)
            self.assertEqual(len(loaded["shards"]), 101)
            self.assertEqual(
                {Path(entry["path"]).parts[2] for entry in loaded["shards"]},
                {"06", "07"},
            )

    def test_index_is_atomic_visibility_boundary(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history"
            original = [self.sample_run(0)]
            history_tools.save_history(path, original, "test")
            real_write = history_tools.atomic_write_json

            def fail_index(target, value):
                if target.name == "index.json":
                    raise OSError("injected index failure")
                return real_write(target, value)

            with mock.patch.object(history_tools, "atomic_write_json", side_effect=fail_index):
                with self.assertRaisesRegex(OSError, "injected"):
                    history_tools.save_history(path, original + [self.sample_run(1)], "test")
            self.assertEqual(history_tools.load_history(path)["runs"], original)

    def test_legacy_list_and_object_migrate_as_visibly_unknown(self):
        for value in ([self.sample_run(0)], {"version": 1, "runs": [self.sample_run(0)]}):
            with self.subTest(kind=type(value).__name__), tempfile.TemporaryDirectory() as temp_dir:
                legacy = Path(temp_dir) / "history.json"
                legacy.write_text(json.dumps(value))
                migrated = [history_tools.mark_legacy(run) for run in history_tools.legacy_history(legacy)]
                publication = migrated[0]["publication"]
                self.assertFalse(publication["comparable"])
                self.assertEqual(publication["regime_id"], "unknown")
                self.assertTrue(publication["coverage"]["gap_unknown"])

    def test_regime_ignores_commit_but_tracks_comparability_dimensions(self):
        corpus = {"schema_version": 1, "sha256": "a" * 64}
        base = {
            "commit": "one",
            "build_mode": "release",
            "iterations": 5,
            "benchmarks": [{"timing_schema_version": 2}],
        }
        first = history_tools.regime_metadata(base, "linux", "image-v1", corpus)
        changed_commit = history_tools.regime_metadata({**base, "commit": "two"}, "linux", "image-v1", corpus)
        changed_iterations = history_tools.regime_metadata({**base, "iterations": 10}, "linux", "image-v1", corpus)
        changed_runner = history_tools.regime_metadata(base, "linux", "image-v2", corpus)
        self.assertEqual(first["id"], changed_commit["id"])
        self.assertNotEqual(first["id"], changed_iterations["id"])
        self.assertNotEqual(first["id"], changed_runner["id"])

    def test_coverage_makes_skipped_commits_and_unknown_gaps_explicit(self):
        completed = subprocess.CompletedProcess([], 0, "middle\ncurrent\n", "")
        coverage = history_tools.git_coverage(
            "previous", "current", Path("."), runner=lambda *args, **kwargs: completed
        )
        self.assertEqual(coverage["represented_commits"], ["current"])
        self.assertEqual(coverage["skipped_commits"], ["middle"])
        self.assertFalse(coverage["gap_unknown"])
        self.assertTrue(history_tools.git_coverage(None, "current", Path("."))["gap_unknown"])
        not_ancestor = subprocess.CompletedProcess([], 1, "", "")
        unknown = history_tools.git_coverage(
            "other-line", "current", Path("."), runner=lambda *args, **kwargs: not_ancestor
        )
        self.assertTrue(unknown["gap_unknown"])
        self.assertEqual(unknown["skipped_commits"], [])

    def test_chart_breaks_at_regime_changes_and_measurement_gaps(self):
        def run(regime, skipped=None, unknown=False):
            return {"publication": {
                "comparable": True,
                "regime_id": regime,
                "coverage": {"skipped_commits": skipped or [], "gap_unknown": unknown},
            }}

        base = run("same")
        self.assertFalse(charts.comparison_break(base, run("same")))
        self.assertTrue(charts.comparison_break(base, run("changed")))
        self.assertTrue(charts.comparison_break(base, run("same", ["skipped"])))
        self.assertTrue(charts.comparison_break(base, run("same", unknown=True)))
        legacy = {"commit": "legacy"}
        self.assertTrue(charts.comparison_break(legacy, legacy))

    def test_malformed_publication_metadata_is_rejected(self):
        errors = history_tools.validate_publication({"commit": "x", "publication": {}})
        self.assertTrue(any("missing regime" in error for error in errors))
        self.assertTrue(any("coverage" in error for error in errors))

    def test_index_and_shard_metadata_must_be_coherent(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history"
            history_tools.save_history(path, [self.sample_run(0)], "test")
            index_path = path / "index.json"
            index = json.loads(index_path.read_text())
            index["run_count"] = 2
            index_path.write_text(json.dumps(index))
            with self.assertRaisesRegex(ValueError, "index run count mismatch"):
                history_tools.load_history(path)

            index["run_count"] = 1
            index_path.write_text(json.dumps(index))
            shard_path = path / index["shards"][0]["path"]
            shard = json.loads(shard_path.read_text())
            shard["platform"] = "other"
            shard_path.write_text(json.dumps(shard))
            with self.assertRaisesRegex(ValueError, "shard metadata mismatch"):
                history_tools.load_history(path)

    def test_shard_digest_and_index_timestamps_detect_tampering(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history"
            history_tools.save_history(path, [self.sample_run(0)], "test")
            index = json.loads((path / "index.json").read_text())
            shard_path = path / index["shards"][0]["path"]
            shard = json.loads(shard_path.read_text())
            shard["runs"][0]["commit"] = "tampered"
            shard_path.write_text(json.dumps(shard))
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                history_tools.load_history(path)

            shard_path.unlink()
            history_tools.save_history(path, [self.sample_run(0)], "test")
            index = json.loads((path / "index.json").read_text())
            index["shards"][0]["last_timestamp"] = "wrong"
            (path / "index.json").write_text(json.dumps(index))
            with self.assertRaisesRegex(ValueError, "timestamp mismatch"):
                history_tools.load_history(path)

    def test_every_time_series_splits_at_comparison_boundaries(self):
        def run(index, gap=False):
            value = float(index + 1)
            return {
                "commit": str(index + 1) * 40,
                "publication": {
                    "comparable": True,
                    "regime_id": "same",
                    "coverage": {"skipped_commits": ["gap"] if gap else [], "gap_unknown": False},
                },
                "benchmarks": [{
                    "name": "probe", "mean_ms": value,
                    "peak_memory_bytes": value * 1024 * 1024,
                    "binary_size_bytes": value * 1024,
                }],
            }

        runs = [run(0), run(1), run(2, gap=True), run(3)]
        self.assertEqual([len(segment) for segment in charts.point_segments(runs, [1, 2, 3, 4])], [2, 2])
        self.assertEqual(charts.generate_multi_timeline_chart(runs, ["probe"]).count("<path d="), 2)
        self.assertEqual(charts.generate_memory_chart(runs).count('<path class="chart-line"'), 2)
        self.assertEqual(charts.generate_binary_size_chart(runs).count('<path class="chart-line"'), 2)
        self.assertEqual(charts.generate_comparison_timeline_chart({"probe": runs}).count("<path d="), 2)

    def test_summary_and_homepage_use_only_latest_comparable_segment(self):
        def run(commit, total, regime="new", gap=False):
            return {
                "commit": commit,
                "publication": {
                    "comparable": True, "regime_id": regime,
                    "coverage": {"skipped_commits": [], "gap_unknown": gap},
                },
                "benchmarks": [{"name": "probe", "mean_ms": total}],
            }

        runs = [run("old", 1, "old"), run("new-1", 10), run("new-2", 20)]
        summary = charts.generate_summary_data(runs)
        self.assertEqual(summary["avg_time_ms"], 15)
        self.assertEqual(summary["best_time_ms"], 10)
        self.assertEqual(summary["comparable_run_count"], 2)
        sparkline = site_status.sparkline_data(runs)
        self.assertEqual(sparkline["n_runs"], 2)
        self.assertEqual(sparkline["latest_ms"], 20)


class BenchmarkMetricSemanticsTests(unittest.TestCase):
    @staticmethod
    def sample_run(commit, values, regime="same", scenario="cold_compilation"):
        benchmarks = []
        for name, samples in values.items():
            benchmarks.append({
                "name": name,
                "samples_ms": samples,
                "mean_ms": sum(samples) / len(samples),
                "source_metrics": {"lines": 100, "tokens": 200},
                "peak_memory_bytes": 1024,
                "binary_size_bytes": 2048,
            })
        return {
            "commit": commit,
            "measurement_family": "compiler",
            "scenario_family": scenario,
            "publication": {
                "comparable": True,
                "regime_id": regime,
                "coverage": {"skipped_commits": [], "gap_unknown": False},
                "metric_semantics": {
                    "measurement_family": "compiler",
                    "scenario_family": scenario,
                },
            },
            "benchmarks": benchmarks,
        }

    def test_previous_delta_variation_and_classification_use_samples(self):
        reference = self.sample_run("a", {"probe": [99, 100, 101]})
        stable = self.sample_run("b", {"probe": [100, 101, 102]})
        regression = self.sample_run("c", {"probe": [109.9, 110, 110.1]})
        improvement = self.sample_run("d", {"probe": [89.9, 90, 90.1]})

        stable_result = metrics.compare_runs(stable, reference)
        self.assertEqual(stable_result["status"], "comparable")
        self.assertEqual(stable_result["headline"]["classification"], "stable")
        self.assertAlmostEqual(stable_result["workloads"]["probe"]["delta_pct"], 1.0)
        self.assertGreater(stable_result["workloads"]["probe"]["variation_pct"], 1.0)
        self.assertEqual(
            metrics.compare_runs(regression, reference)["headline"]["classification"],
            "regressed",
        )
        self.assertEqual(
            metrics.compare_runs(improvement, reference)["headline"]["classification"],
            "improved",
        )

    def test_rolling_baseline_is_robust_and_requires_three_runs(self):
        current = self.sample_run("d", {"probe": [109.9, 110, 110.1]})
        preceding = [
            self.sample_run("a", {"probe": [99.9, 100, 100.1]}),
            self.sample_run("b", {"probe": [100.9, 101, 101.1]}),
            self.sample_run("c", {"probe": [98.9, 99, 99.1]}),
        ]
        self.assertEqual(
            metrics.compare_to_rolling_baseline(current, preceding[:2])["status"],
            "insufficient_data",
        )
        result = metrics.compare_to_rolling_baseline(current, preceding)
        self.assertEqual(result["headline"]["classification"], "regressed")
        self.assertEqual(result["workloads"]["probe"]["reference_median_ms"], 100)
        self.assertEqual(result["workloads"]["probe"]["baseline_run_count"], 3)

    def test_missing_samples_are_insufficient_data(self):
        result = metrics.compare_runs(
            self.sample_run("b", {"probe": [100, 101]}),
            self.sample_run("a", {"probe": [100, 100, 100]}),
        )
        self.assertEqual(result["headline"]["classification"], "insufficient_data")

    def test_workload_and_regime_changes_are_non_comparable_rebases(self):
        baseline = self.sample_run("a", {"one": [100, 100, 100]})
        composition = self.sample_run("b", {"one": [100, 100, 100], "two": [50, 50, 50]})
        regime = self.sample_run("c", {"one": [100, 100, 100]}, regime="new")
        self.assertEqual(metrics.compare_runs(composition, baseline)["reason"], "workload_composition_changed")
        self.assertEqual(metrics.performance_index(composition, baseline)["status"], "rebase")
        self.assertEqual(metrics.compare_runs(regime, baseline)["reason"], "regime_boundary")
        self.assertEqual(metrics.performance_index(regime, baseline)["reason"], "regime_changed")

        derived = metrics.derive_history_metrics([baseline, composition])
        self.assertEqual(derived["points"][1]["performance_index"]["status"], "rebase")
        self.assertEqual(
            derived["points"][1]["performance_index"]["reason"],
            "workload_composition_changed",
        )

    def test_performance_index_is_geometric_mean_with_baseline_one_hundred(self):
        baseline = self.sample_run("a", {
            "one": [100, 100, 100],
            "two": [400, 400, 400],
        })
        current = self.sample_run("b", {
            "one": [50, 50, 50],
            "two": [800, 800, 800],
        })
        result = metrics.performance_index(current, baseline)
        self.assertEqual(result["status"], "comparable")
        self.assertAlmostEqual(result["value"], 100.0)
        self.assertEqual(metrics.performance_index(baseline, baseline)["value"], 100.0)

    def test_scenario_mixing_is_rejected(self):
        cold = self.sample_run("a", {"probe": [100, 100, 100]})
        reused = self.sample_run(
            "b", {"probe": [50, 50, 50]}, scenario="reused_session_query"
        )
        with self.assertRaisesRegex(ValueError, "cannot mix"):
            metrics.compare_runs(reused, cold)
        with self.assertRaisesRegex(ValueError, "cannot mix"):
            metrics.performance_index(reused, cold)

    def test_metric_products_remain_separate_families(self):
        run = self.sample_run("a", {"probe": [10, 10, 10]})
        families = metrics.metric_families(run)
        self.assertEqual(
            set(families),
            {"latency_ms", "throughput", "memory_bytes", "binary_size_bytes"},
        )
        self.assertEqual(families["latency_ms"]["probe"], 10)
        self.assertEqual(families["throughput"]["probe"]["lines"], 10000)
        self.assertEqual(families["memory_bytes"]["probe"], 1024)
        self.assertEqual(families["binary_size_bytes"]["probe"], 2048)

    def test_dashboard_summary_routes_latency_through_canonical_policy(self):
        reference = self.sample_run("a", {"probe": [99.9, 100, 100.1]})
        current = self.sample_run("b", {"probe": [109.9, 110, 110.1]})
        summary = charts.generate_summary_data([reference, current])
        self.assertEqual(summary["time_comparison"]["headline"]["classification"], "regressed")
        self.assertAlmostEqual(summary["time_delta_pct"], 10.0)
        self.assertEqual(summary["time_delta_str"], "↑ 10.0%")

    def test_status_index_uses_full_segment_when_sparkline_is_truncated(self):
        runs = [
            self.sample_run(
                f"commit-{index}",
                {"probe": [100 + index, 100 + index, 100 + index]},
            )
            for index in range(25)
        ]
        status = site_status.sparkline_data(runs)
        self.assertEqual(status["n_runs"], site_status.SPARKLINE_RUNS)
        self.assertAlmostEqual(
            status["performance_index"]["value"],
            100 * 100 / 124,
        )


class BenchmarkCollectionTests(unittest.TestCase):
    @staticmethod
    def payload(total_ms=2.0, passes=None):
        return json.dumps(
            {
                "schema_version": 2,
                "total_ms": total_ms,
                "passes": passes
                if passes is not None
                else [
                    {
                        "name": "lexer",
                        "duration_ms": 1.0,
                        "invocations": 1,
                        "root_invocations": 0,
                        "leaf_invocations": 1,
                    }
                ],
            }
        )

    def test_valid_iterations_are_all_aggregated(self):
        result = collection.aggregate_iterations(
            [self.payload(2.0), self.payload(4.0)]
        )
        self.assertEqual(result["passes"], {"lexer": {"mean_ms": 1.0}})

    def test_malformed_timing_or_pass_payload_aborts_collection(self):
        bad_payloads = [
            "not json",
            self.payload(float("inf")),
            self.payload(passes={}),
            self.payload(passes=[{"name": "lexer", "duration_ms": "fast"}]),
            self.payload(
                passes=[
                    {
                        "name": "lexer",
                        "duration_ms": 1.0,
                        "invocations": 1,
                        "leaf_invocations": 1,
                    }
                ]
            ),
            self.payload(
                passes=[
                    {
                        "name": "lexer",
                        "duration_ms": 1.0,
                        "invocations": True,
                        "root_invocations": 0,
                        "leaf_invocations": 1,
                    }
                ]
            ),
            self.payload(
                passes=[
                    {
                        "name": "lexer",
                        "duration_ms": 1.0,
                        "invocations": 1,
                        "root_invocations": 0,
                        "leaf_invocations": 2,
                    }
                ]
            ),
        ]
        for payload in bad_payloads:
            with self.subTest(payload=payload):
                with self.assertRaises((ValueError, json.JSONDecodeError)):
                    collection.aggregate_iterations([self.payload(), payload])

    def test_rejects_unsupported_timing_schema(self):
        payload = json.loads(self.payload())
        payload["schema_version"] = 3
        with self.assertRaisesRegex(ValueError, "integer 1 or 2"):
            collection.aggregate_iterations([json.dumps(payload)])

    def test_rejects_pass_set_drift_between_iterations(self):
        changed = json.loads(self.payload())
        changed["passes"][0]["name"] = "parser"
        with self.assertRaisesRegex(ValueError, "pass set changed"):
            collection.aggregate_iterations([self.payload(), json.dumps(changed)])


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
        # Historical mean_ms double-counted nested spans. Prefer the stored
        # compile root (9) when present, then the honest/fallback mean (20).
        self.assertEqual(charts.get_total_time(self.run), 29)
        self.assertEqual(
            charts.get_pass_times(self.run),
            {"parallel_astgen": 4, "sema": 6, "new_pass": 5},
        )
        self.assertEqual(charts.get_peak_memory(self.run), 5)
        self.assertEqual(charts.get_binary_size(self.run), 10)

        summary = charts.generate_summary_data([self.run])
        self.assertEqual(summary["latest_time_ms"], 29)
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

    def test_current_phase_order_includes_both_frontend_indexes_and_sema(self):
        ordered = charts.order_pass_times(
            {
                "linker": 1,
                "rir_declaration_index": 1,
                "semantic_astgen": 1,
                "sema": 1,
                "definition_snapshot": 1,
                "parser": 1,
                "lexer": 1,
            }
        )
        self.assertEqual(
            list(ordered),
            [
                "lexer",
                "parser",
                "definition_snapshot",
                "semantic_astgen",
                "rir_declaration_index",
                "sema",
                "linker",
            ],
        )


if __name__ == "__main__":
    unittest.main()
