#!/usr/bin/env python3
"""Regression tests for benchmark validation and dashboard aggregation."""

import importlib.util
import json
import os
from datetime import datetime, timezone
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


INPUT_ROOT = Path(
    os.environ.get("RUE_BENCHMARK_TEST_ROOT", Path(__file__).resolve().parent.parent)
)
DEEP_NESTING_CASE = Path(
    os.environ.get(
        "RUE_DEEP_NESTING_CASE",
        INPUT_ROOT / "crates/rue-cli-tests/cases/deep_nesting.toml",
    )
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
annotations = load_module("benchmark_annotations", "scripts/benchmark_annotations.py")
charts = load_module("generate_charts", "scripts/generate-charts.py")
site_status = load_module("generate_site_status", "scripts/generate-site-status.py")
history_tools = load_module("benchmark_history", "scripts/benchmark_history.py")
metrics = load_module("benchmark_metrics", "scripts/benchmark_metrics.py")
evolution = load_module("benchmark_evolution", "scripts/benchmark_evolution.py")
recent = load_module("benchmark_recent", "scripts/benchmark_recent.py")
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

    def test_deep_nesting_corpus_shape_and_explicit_complexity_gate(self):
        source = (
            INPUT_ROOT / "benchmarks" / "stress" / "deep_nesting.rue"
        ).read_text()
        self.assertEqual(len(source.splitlines()), 12198)
        self.assertEqual(source.count("fn nested_blocks_"), 50)
        self.assertEqual(source.count("fn nested_if_"), 50)
        self.assertEqual(source.count("fn nested_while_"), 20)
        self.assertEqual(source.count("fn deep_expr_"), 30)
        self.assertIn("let v39 = 40;", source)

        corpus = perf_baseline.default_corpus(INPUT_ROOT)
        deep = [entry for entry in corpus if entry[0] == "deep_nesting"]
        self.assertEqual(len(deep), 1)
        self.assertEqual(deep[0][2], [INPUT_ROOT / "benchmarks/stress/deep_nesting.rue"])
        self.assertEqual(
            deep[0][3], perf_baseline.DEEP_NESTING_TIMEOUT_SECONDS
        )

        cli_case = DEEP_NESTING_CASE.read_text()
        self.assertIn("A 60-level nested block", cli_case)
        self.assertIn("timeout_ms = 10000", cli_case)

    def test_deep_nesting_budget_is_enforced_independently_of_global_timeout(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            ordinary = root / "ordinary.rue"
            deep = root / "deep_nesting.rue"
            ordinary.write_text("fn main() -> i32 { 0 }")
            deep.write_text("fn main() -> i32 { 0 }")
            seen = []

            def compile_once(_rue, files, _output, timeout, _jobs):
                seen.append((Path(files[0]).name, timeout))
                return self.sample(10.0, 12.0)

            corpus = [
                ("ordinary", "large", [ordinary], None),
                (
                    "deep_nesting",
                    "large",
                    [deep],
                    perf_baseline.DEEP_NESTING_TIMEOUT_SECONDS,
                ),
            ]
            with mock.patch.object(perf_baseline.sys, "stderr"):
                with mock.patch.object(
                    perf_baseline, "compile_once", side_effect=compile_once
                ):
                    results = perf_baseline.run_corpus(
                        "rue", corpus, 1, 0, temp_dir, 60.0, 1
                    )

        by_name = {result["name"]: result for result in results}
        self.assertEqual(by_name["ordinary"]["compile_timeout_seconds"], 60.0)
        self.assertEqual(
            by_name["deep_nesting"]["compile_timeout_seconds"],
            perf_baseline.DEEP_NESTING_TIMEOUT_SECONDS,
        )
        self.assertEqual(by_name["multi_file"]["compile_timeout_seconds"], 60.0)
        self.assertIn(("deep_nesting.rue", 10.0), seen)
        self.assertIn(("ordinary.rue", 60.0), seen)

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
        def run(commit, total, day, regime="new", gap=False):
            return {
                "commit": commit,
                "timestamp": f"2026-07-{day:02d}T00:00:00Z",
                "publication": {
                    "comparable": True, "regime_id": regime,
                    "coverage": {"measured_commit": commit, "represented_commits": [commit], "skipped_commits": [], "gap_unknown": gap},
                },
                "benchmarks": [{"name": "probe", "mean_ms": total, "samples_ms": [total] * 3}],
            }

        runs = [run("old", 1, 1, "old"), run("new-1", 10, 2), run("new-2", 20, 3)]
        summary = charts.generate_summary_data(runs)
        self.assertEqual(summary["avg_time_ms"], 15)
        self.assertEqual(summary["best_time_ms"], 10)
        self.assertEqual(summary["comparable_run_count"], 2)
        sparkline = site_status.sparkline_data(runs)
        self.assertEqual(sparkline["n_runs"], 2)
        self.assertEqual(sparkline["state"]["kind"], "insufficient_data")


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

    def test_status_index_uses_full_thirty_day_segment(self):
        runs = [
            {
                **self.sample_run(
                f"commit-{index}",
                {"probe": [100 + index, 100 + index, 100 + index]},
                ),
                "timestamp": f"2026-07-{index + 1:02d}T00:00:00Z",
            }
            for index in range(25)
        ]
        status = site_status.sparkline_data(runs)
        self.assertEqual(status["n_runs"], 25)
        self.assertAlmostEqual(
            status["latest_index"],
            100 * 100 / 124,
        )


class HomepageBenchmarkStatusTests(unittest.TestCase):
    class Resolver:
        def is_ancestor(self, start, end):
            return start == end

    @staticmethod
    def sample_run(index, center, regime="same", workloads=None, day=None):
        values = workloads or {"probe": center}
        run = BenchmarkMetricSemanticsTests.sample_run(
            f"{index:040x}",
            {name: [value - 0.1, value, value + 0.1] for name, value in values.items()},
            regime=regime,
        )
        run["timestamp"] = f"2026-07-{day or index:02d}T00:00:00Z"
        run["publication"]["coverage"] = {
            "measured_commit": run["commit"],
            "represented_commits": [run["commit"]],
            "skipped_commits": [],
            "gap_unknown": False,
        }
        return run

    def status(self, runs, annotations=None, as_of=None):
        return site_status.sparkline_data(
            runs,
            annotations or [],
            resolver=self.Resolver(),
            as_of=as_of or datetime(2026, 7, 10, tzinfo=timezone.utc),
        )

    def test_stable_improved_regressed_and_insufficient_states(self):
        preceding = [self.sample_run(1, 100), self.sample_run(2, 101), self.sample_run(3, 99)]
        self.assertEqual(self.status(preceding + [self.sample_run(4, 100)])["state"]["kind"], "stable")
        self.assertEqual(self.status(preceding + [self.sample_run(4, 80)])["state"]["kind"], "improved")
        regressed = self.status(preceding + [self.sample_run(4, 120)])
        self.assertEqual(regressed["state"]["kind"], "regressed")
        self.assertGreater(regressed["state"]["delta_pct"], regressed["state"]["variation_pct"])
        self.assertEqual(self.status(preceding[:2])["state"]["kind"], "insufficient_data")

    def test_regime_and_corpus_changes_are_explicit_resets_without_percentage(self):
        runs = [self.sample_run(1, 100), self.sample_run(2, 100), self.sample_run(3, 100)]
        regime = self.status(runs + [self.sample_run(4, 120, regime="runner-b")])
        self.assertEqual(regime["state"]["kind"], "baseline_reset")
        self.assertNotIn("delta_pct", regime["state"])

        corpus_a = self.status(runs + [self.sample_run(4, 120, workloads={"probe": 120, "new": 1})])
        corpus_b = self.status(runs + [self.sample_run(4, 120, workloads={"probe": 120, "new": 10000})])
        self.assertEqual(corpus_a["state"], corpus_b["state"])
        self.assertEqual(corpus_a["state"]["kind"], "baseline_reset")

    def test_capability_annotation_freshness_coverage_and_deep_link(self):
        runs = [self.sample_run(1, 100), self.sample_run(2, 100), self.sample_run(3, 100), self.sample_run(4, 120)]
        event = {
            "id": "capability-cost",
            "source": "authored",
            "location": {"kind": "commit", "commit": runs[-1]["commit"]},
            "category": "capability",
            "title": "Intentional capability cost",
            "explanation": "Accepted cost for a language feature.",
            "link": "https://github.com/rue-language/rue/pull/1",
            "scope": {"metrics": ["latency"], "workloads": ["*"], "platforms": ["*"]},
        }
        status = self.status(
            runs, [event], as_of=datetime(2026, 7, 7, 1, tzinfo=timezone.utc)
        )
        self.assertEqual(status["state"]["kind"], "regressed")
        self.assertEqual(status["freshness"]["kind"], "stale")
        self.assertEqual(status["annotation"]["title"], "Intentional capability cost")
        self.assertIn("range=30d", status["annotation"]["dashboard_url"])
        self.assertIn("annotation=capability-cost", status["annotation"]["dashboard_url"])
        self.assertFalse(status["coverage"]["gap_unknown"])
        self.assertEqual(status["coverage"]["represented_commits"], [runs[-1]["commit"]])

    def test_missing_or_malformed_time_data_is_not_presented(self):
        self.assertIsNone(self.status([]))
        run = self.sample_run(1, 100)
        del run["timestamp"]
        self.assertIsNone(self.status([run]))

    def test_insufficient_legacy_samples_do_not_fabricate_endpoint(self):
        run = self.sample_run(1, 100)
        run["benchmarks"][0]["samples_ms"] = [100, 100]
        status = self.status([run])
        self.assertEqual(status["state"]["kind"], "insufficient_data")
        self.assertEqual(status["n_trend_points"], 0)
        self.assertEqual(status["points"], "")
        self.assertNotIn("last_x", status)
        self.assertNotIn("last_y", status)

    def test_thirty_day_window_uses_calendar_time_and_latest_segment(self):
        runs = [
            self.sample_run(1, 100, day=1),
            self.sample_run(2, 100, day=2),
            self.sample_run(3, 100, day=3),
            self.sample_run(4, 100, day=4),
        ]
        runs[0]["timestamp"] = "2026-05-01T00:00:00Z"
        status = self.status(runs)
        self.assertEqual(status["window"], "30d")
        self.assertEqual(status["n_runs"], 3)

    def test_sparkline_x_coordinates_use_calendar_spacing(self):
        runs = [
            self.sample_run(1, 100, day=1),
            self.sample_run(2, 90, day=2),
            self.sample_run(3, 80, day=30),
        ]
        status = self.status(runs)
        coordinates = [point.split(",") for point in status["points"].split()]
        self.assertEqual([int(point[0]) for point in coordinates], [10, 20, 300])

    def test_shallow_checkout_omits_misleading_commit_count(self):
        def fake_git(*args):
            responses = {
                ("rev-parse", "--short", "HEAD"): "abc123",
                ("log", "-1", "--format=%cs"): "2026-07-01",
                ("rev-parse", "--is-shallow-repository"): "true",
            }
            if args == ("rev-list", "--count", "HEAD"):
                self.fail("shallow checkout must not count the truncated history")
            return responses[args]
        with mock.patch.object(site_status, "git", side_effect=fake_git):
            status = site_status.commit_info()
        self.assertFalse(status["history_complete"])
        self.assertNotIn("commit_count", status)

    def test_homepage_template_reports_honest_states(self):
        template = (INPUT_ROOT / "website" / "templates" / "index.html").read_text()
        self.assertIn("30-day comparable trend", template)
        self.assertIn("no trustworthy percentage", template)
        self.assertIn("history count unavailable", template)
        self.assertIn("commit coverage is unknown", template)
        self.assertIn("benchmark data unavailable", template)
        self.assertIn("{% if status.bench.n_trend_points > 0 %}", template)
        self.assertNotIn("measurements taken on every commit", template)
        performance = (INPUT_ROOT / "website" / "templates" / "performance.html").read_text()
        self.assertIn("deepLink.get('range')", performance)
        self.assertIn("evolution-deep-linked-annotation", performance)
        self.assertIn("annotationTargetId(requestedAnnotation)", performance)
        self.assertIn("target.scrollIntoView", performance)
        self.assertIn("target.focus", performance)
        self.assertIn("item.dataset.deepLinked = 'true'", performance)
        self.assertIn("item.style.outline", performance)


class TestBenchmarkAnnotations(unittest.TestCase):
    class Resolver:
        def __init__(self, commits=("aaaaaaa", "bbbbbbb", "ccccccc")):
            self.commits = {commit: commit[0] * 40 for commit in commits}

        def canonical(self, commit):
            if commit not in self.commits and commit not in self.commits.values():
                raise ValueError(f"unknown annotation commit: {commit}")
            return self.commits.get(commit, commit)

        def is_ancestor(self, start, end):
            order = list(self.commits.values())
            return order.index(start) <= order.index(end)

        def timestamp(self, commit):
            values = list(self.commits.values())
            if commit not in values:
                raise ValueError("unknown")
            return values.index(commit) + 1

    @staticmethod
    def authored(commit="aaaaaaa", category="optimization", title="Useful optimization"):
        return {
            "commit": commit,
            "category": category,
            "title": title,
            "explanation": "This is a concise explanation of the benchmark event.",
            "link": "https://github.com/rue-language/rue/pull/123",
            "scope": {
                "metrics": ["latency"],
                "workloads": ["probe"],
                "platforms": ["x86-64-linux"],
            },
        }

    @staticmethod
    def sample_run(commit, regime=None):
        publication = {"comparable": True, "regime_id": "regime"}
        if regime is not None:
            publication["regime"] = regime
        return {"commit": commit, "publication": publication, "benchmarks": []}

    def test_commit_ranges_and_same_commit_events_normalize_deterministically(self):
        resolver = self.Resolver()
        first = self.authored("bbbbbbb", title="Zulu")
        second = self.authored("bbbbbbb", category="repair", title="Alpha")
        ranged = {
            **self.authored(title="Range"),
            "start_commit": "aaaaaaa",
            "end_commit": "ccccccc",
        }
        del ranged["commit"]
        stream = annotations.normalized_annotations([], [first, ranged, second], resolver)
        self.assertEqual([event["title"] for event in stream], ["Range", "Zulu", "Alpha"])
        self.assertEqual(stream[0]["location"]["kind"], "range")
        self.assertEqual(len({event["id"] for event in stream}), 3)

    def test_invalid_categories_commits_links_scopes_and_fields_are_rejected(self):
        resolver = self.Resolver()
        mutations = [
            (lambda event: event.update(category="invalid"), "invalid category"),
            (lambda event: event.update(commit="ddddddd"), "unknown annotation commit"),
            (lambda event: event.update(link="https://example.com/123"), "must link"),
            (lambda event: event["scope"].update(metrics=["seconds_plus_bytes"]), "invalid metric"),
            (lambda event: event["scope"].update(workloads=[" probe"]), "unique string list"),
            (lambda event: event["scope"].update(platforms=["linux/x86"]), "unique string list"),
            (lambda event: event["scope"].update(workloads=["*", "probe"]), "cannot combine"),
            (lambda event: event["scope"].update(metrics=["*", "latency"]), "cannot combine"),
            (lambda event: event.update(typo=True), "unknown fields"),
            (lambda event: event.update(title=""), "title must be"),
        ]
        for mutate, message in mutations:
            with self.subTest(message=message):
                event = self.authored()
                mutate(event)
                with self.assertRaisesRegex(ValueError, message):
                    annotations.normalized_annotations([], [event], resolver)

    def test_duplicate_and_conflicting_events_are_rejected(self):
        resolver = self.Resolver()
        event = self.authored()
        with self.assertRaisesRegex(ValueError, "duplicate"):
            annotations.normalized_annotations([], [event, dict(event)], resolver)
        conflict = {**event, "explanation": "A different explanation."}
        with self.assertRaisesRegex(ValueError, "conflicting"):
            annotations.normalized_annotations([], [event, conflict], resolver)

    def test_invalid_or_incomplete_ranges_are_rejected(self):
        resolver = self.Resolver()
        reversed_range = {
            **self.authored(),
            "start_commit": "ccccccc",
            "end_commit": "aaaaaaa",
        }
        del reversed_range["commit"]
        with self.assertRaisesRegex(ValueError, "ancestor"):
            annotations.normalized_annotations([], [reversed_range], resolver)
        incomplete = {**self.authored(), "start_commit": "aaaaaaa"}
        del incomplete["commit"]
        with self.assertRaisesRegex(ValueError, "commit keys"):
            annotations.normalized_annotations([], [incomplete], resolver)

    def test_every_measurement_identity_change_is_derived_with_all_dimensions(self):
        before = {
            "platform": "x86-64-linux",
            "corpus_sha256": "old",
            "timing_schema_versions": [1],
            "build_mode": "release",
            "iteration_policy": {"kind": "fixed", "iterations": 5},
            "runner_image": "old-image",
            "scenario_family": "cold_compilation",
        }
        after = {
            **before,
            "corpus_sha256": "new",
            "timing_schema_versions": [2],
            "runner_image": "new-image",
        }
        events = annotations.derived_identity_annotations(
            [
                self.sample_run("a" * 40, before),
                self.sample_run("b" * 40, after),
            ],
            self.Resolver(),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(
            events[0]["changed_dimensions"],
            ["corpus_sha256", "runner_image", "timing_schema_versions"],
        )
        self.assertEqual(events[0]["dimension_changes"]["corpus_sha256"], {
            "before": "old", "after": "new"
        })
        self.assertEqual(events[0]["identity_change"]["before"]["status"], "explicit")
        self.assertEqual(events[0]["identity_change"]["after"]["status"], "explicit")
        self.assertEqual(events[0]["category"], "measurement_infrastructure")

    def test_legacy_to_explicit_transition_is_automatically_annotated(self):
        regime = {"platform": "x86-64-linux", "runner_image": "image"}
        events = annotations.derived_identity_annotations(
            [
                self.sample_run("a" * 40),
                self.sample_run("b" * 40, regime),
            ],
            self.Resolver(),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["location"]["commit"], "b" * 40)
        self.assertEqual(events[0]["identity_change"]["before"], {
            "status": "unknown", "dimensions": None
        })
        self.assertEqual(events[0]["identity_change"]["after"], {
            "status": "explicit", "dimensions": regime
        })

    def test_explicit_to_legacy_transition_is_automatically_annotated(self):
        regime = {"platform": "x86-64-linux", "runner_image": "image"}
        events = annotations.derived_identity_annotations(
            [
                self.sample_run("a" * 40, regime),
                self.sample_run("b" * 40),
            ],
            self.Resolver(),
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["identity_change"]["before"]["status"], "explicit")
        self.assertEqual(events[0]["identity_change"]["after"], {
            "status": "unknown", "dimensions": None
        })

    def test_derived_transition_rejects_unknown_run_commit(self):
        with self.assertRaisesRegex(ValueError, "unknown annotation commit"):
            annotations.derived_identity_annotations(
                [
                    self.sample_run("a" * 40, {"platform": "one"}),
                    self.sample_run("d" * 40, {"platform": "two"}),
                ],
                self.Resolver(),
            )

    def test_commit_order_resolution_errors_are_not_silenced(self):
        resolver = self.Resolver()
        with mock.patch.object(
            resolver, "timestamp", side_effect=ValueError("timestamp failure")
        ):
            with self.assertRaisesRegex(ValueError, "timestamp failure"):
                annotations.normalized_annotations(
                    [], [self.authored()], resolver
                )

    def test_legacy_history_and_missing_annotation_file_are_supported(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            self.assertEqual(
                annotations.load_annotations(Path(temp_dir) / "missing.toml"), []
            )
        self.assertEqual(
            annotations.normalized_annotations(
                [self.sample_run("legacy")], [], self.Resolver()
            ),
            [],
        )

    def test_recent_status_receives_the_canonical_stream_unchanged(self):
        run = BenchmarkMetricSemanticsTests.sample_run(
            "a" * 40, {"probe": [10, 10, 10]}
        )
        run["timestamp"] = "2026-07-01T00:00:00Z"
        stream = annotations.normalized_annotations(
            [run], [self.authored()], self.Resolver()
        )
        status = site_status.sparkline_data(
            [run], stream, resolver=self.Resolver(),
            as_of=datetime(2026, 7, 1, tzinfo=timezone.utc),
        )
        self.assertEqual(status["annotation"]["id"], stream[0]["id"])


class TestRecentRegressionWorkspace(unittest.TestCase):
    class Resolver:
        def metadata(self, commit):
            index = int(commit[0], 16)
            return {
                "commit": commit,
                "subject": f"RUE-{900 + index}: measured change {index}",
                "date": f"2026-07-{index + 1:02d}",
                "links": {
                    "commit": f"https://github.com/rue-language/rue/commit/{commit}",
                    "issues": [{
                        "id": f"RUE-{900 + index}",
                        "url": f"https://linear.app/steve-klabnik/issue/RUE-{900 + index}",
                    }],
                    "pull_requests": [],
                },
            }

        def is_ancestor(self, start, end):
            return int(start[0], 16) <= int(end[0], 16)

    @staticmethod
    def sample_run(index, probe_samples, other_samples, skipped=()):
        commit = f"{index:x}" * 40
        def bench(name, samples, parser):
            return {
                "name": name,
                "mean_ms": sum(samples) / len(samples),
                "samples_ms": samples,
                "passes": {
                    "parser": {"mean_ms": parser},
                    "codegen": {"mean_ms": 2.0},
                },
                "peak_memory_bytes": 1000 + index * 10,
                "binary_size_bytes": 2000 + index * 5,
            }
        return {
            "commit": commit,
            "timestamp": f"2026-07-{index + 1:02d}T00:00:00Z",
            "publication": {
                "comparable": True,
                "regime_id": "same",
                "regime": {"platform": "x86-64-linux"},
                "coverage": {
                    "measured_commit": commit,
                    "represented_commits": [commit],
                    "skipped_commits": list(skipped),
                    "gap_unknown": False,
                },
            },
            "benchmarks": [
                bench("probe", probe_samples, 5.0 if index < 4 else 20.0),
                bench("other", other_samples, 3.0 if index < 4 else 4.0),
            ],
        }

    def test_fixture_distinguishes_noise_regression_annotation_and_gap(self):
        skipped = "a" * 40
        runs = [
            self.sample_run(1, [99.9, 100, 100.1], [49.9, 50, 50.1]),
            self.sample_run(2, [90, 100, 110], [49.9, 50, 50.1]),
            self.sample_run(3, [99.9, 100, 100.1], [49.9, 50, 50.1], [skipped]),
            self.sample_run(4, [119.9, 120, 120.1], [50.9, 51, 51.1]),
        ]
        capability = {
            "id": "capability",
            "source": "authored",
            "location": {"kind": "commit", "commit": "4" * 40},
            "category": "capability",
            "title": "Queryable compiler",
            "explanation": "Compiler queries are now available.",
            "link": "https://github.com/rue-language/rue/pull/1",
            "scope": {"metrics": ["latency"], "workloads": ["*"], "platforms": ["*"]},
        }
        model = recent.recent_workspace(
            runs,
            [capability],
            self.Resolver(),
            "x86-64-linux",
            as_of=datetime(2026, 7, 5, tzinfo=timezone.utc),
        )

        self.assertEqual(model["window_options"], [20, 50, 100])
        self.assertEqual(model["points"][1]["comparability"]["headline"]["classification"], "stable")
        self.assertGreater(model["points"][1]["comparability"]["headline"]["variation_pct"], 0)
        self.assertEqual(model["points"][2]["comparability"]["status"], "non_comparable")
        self.assertEqual(model["points"][2]["coverage"]["represented_commits"], ["3" * 40])
        self.assertEqual(model["points"][2]["coverage"]["skipped_commits"], [skipped])
        self.assertEqual(model["points"][3]["comparability"]["headline"]["classification"], "regressed")
        self.assertEqual(model["points"][3]["annotations"][0]["category"], "capability")
        self.assertFalse(model["points"][3]["freshness"]["stale"])
        self.assertTrue(model["points"][0]["freshness"]["stale"])
        self.assertGreater(model["points"][0]["freshness"]["wall_clock_age_seconds"], 0)
        self.assertGreater(model["points"][0]["freshness"]["age_from_latest_seconds"], 0)
        self.assertEqual(model["default_selection"], {"before": "3" * 40, "after": "4" * 40})
        self.assertFalse(any(
            item["before"] == "2" * 40 and item["after"] == "4" * 40
            for item in model["comparisons"]
        ))

        explanation = model["comparisons"][-1]["explanation"]
        self.assertEqual(explanation["dominant_workload"]["name"], "probe")
        self.assertEqual(explanation["dominant_pass"]["name"], "parser")
        parser = next(item for item in explanation["passes"] if item["name"] == "parser")
        self.assertEqual(parser["delta_ms"], parser["after_ms"] - parser["before_ms"])
        self.assertTrue(explanation["memory_bytes"])
        self.assertTrue(explanation["binary_size_bytes"])
        probe = next(item for item in model["small_multiples"] if item["workload"] == "probe")
        self.assertTrue(probe["points"][2]["comparison_boundary"])

    def test_workspace_caps_payload_at_one_hundred_points(self):
        runs = [self.sample_run(index % 15 + 1, [10, 10, 10], [5, 5, 5]) for index in range(105)]
        model = recent.recent_workspace(
            runs, [], self.Resolver(), "x86-64-linux",
            as_of=datetime(2026, 8, 1, tzinfo=timezone.utc),
        )
        self.assertEqual(len(model["points"]), 100)

    def test_every_ordered_pair_inside_one_segment_is_selectable(self):
        runs = [
            self.sample_run(index, [10 + index] * 3, [5 + index] * 3)
            for index in (1, 2, 3)
        ]
        model = recent.recent_workspace(
            runs, [], self.Resolver(), "x86-64-linux",
            as_of=datetime(2026, 7, 4, tzinfo=timezone.utc),
        )
        self.assertEqual(
            {(item["before"], item["after"]) for item in model["comparisons"]},
            {
                ("1" * 40, "2" * 40),
                ("1" * 40, "3" * 40),
                ("2" * 40, "3" * 40),
            },
        )

    def test_insufficient_pair_is_explicit_but_not_preferred_over_evidence(self):
        runs = [
            self.sample_run(1, [10, 10], [5, 5]),
            self.sample_run(2, [11, 11, 11], [6, 6, 6]),
            self.sample_run(3, [12, 12, 12], [7, 7, 7]),
        ]
        model = recent.recent_workspace(
            runs, [], self.Resolver(), "x86-64-linux",
            as_of=datetime(2026, 7, 4, tzinfo=timezone.utc),
        )
        insufficient = next(item for item in model["comparisons"] if item["before"] == "1" * 40)
        self.assertEqual(
            insufficient["explanation"]["headline"]["classification"],
            "insufficient_data",
        )
        self.assertEqual(model["default_selection"], {
            "before": "2" * 40, "after": "3" * 40
        })

    def test_missing_pass_is_not_treated_as_zero_measurement(self):
        before = self.sample_run(1, [10, 10, 10], [5, 5, 5])
        after = self.sample_run(2, [11, 11, 11], [6, 6, 6])
        del after["benchmarks"][0]["passes"]["codegen"]
        explanation = metrics.comparison_explanation(after, before)
        self.assertNotIn(
            "codegen",
            {item["name"] for item in explanation["passes"]},
        )

    def test_git_metadata_uses_real_linear_form_and_only_discovered_prs(self):
        completed = subprocess.CompletedProcess(
            [], 0,
            "a" * 40 + "\0RUE-897: workspace (#1648)\0" + "2026-07-14\0body\n",
            "",
        )
        with mock.patch.object(recent.subprocess, "run", return_value=completed):
            metadata = recent.GitRecentResolver(Path(".")).metadata("a" * 40)
        self.assertEqual(
            metadata["links"]["issues"],
            [{
                "id": "RUE-897",
                "url": "https://linear.app/steve-klabnik/issue/RUE-897",
            }],
        )
        self.assertEqual(
            metadata["links"]["pull_requests"],
            [{
                "id": "PR #1648",
                "url": "https://github.com/rue-language/rue/pull/1648",
            }],
        )

    def test_recent_template_keeps_semantics_server_side_and_guards_insufficient_data(self):
        template = (INPUT_ROOT / "website" / "templates" / "performance.html").read_text()
        self.assertIn("insufficient data for a delta", template)
        self.assertIn("point.coverage.skipped_commits.join", template)
        self.assertIn("annotationLink.href = item.link", template)
        self.assertIn("value.before_ms.toFixed", template)
        self.assertIn("headline.classification === 'insufficient_data'", template)
        self.assertIn("populateComparisons(points)", template)
        self.assertNotIn("function calculateDelta", template)


class BenchmarkEvolutionTests(unittest.TestCase):
    class Resolver:
        def is_ancestor(self, start, end):
            return int(start[:8], 16) <= int(end[:8], 16)

    @staticmethod
    def sample_run(index, instant, value=100.0, regime="corpus-a", skipped=(), runner="runner-a"):
        commit = f"{index:08x}" * 5
        return {
            "commit": commit,
            "timestamp": instant.astimezone(timezone.utc).isoformat().replace("+00:00", "Z"),
            "publication": {
                "comparable": True,
                "regime_id": regime,
                "regime": {
                    "platform": "x86-64-linux",
                    "corpus_sha256": regime,
                    "runner_image": runner,
                },
                "coverage": {
                    "measured_commit": commit,
                    "represented_commits": [commit],
                    "skipped_commits": list(skipped),
                    "gap_unknown": False,
                },
            },
            "benchmarks": [{
                "name": "probe",
                "samples_ms": [value - 0.2, value, value + 0.2],
                "mean_ms": value,
                "passes": {"compile": {"mean_ms": value}},
            }],
        }

    def test_calendar_ranges_boundaries_annotations_and_canonical_source(self):
        end = datetime(2026, 12, 31, tzinfo=timezone.utc)
        runs = [
            self.sample_run(1, end - evolution.timedelta(days=400), 120),
            self.sample_run(2, end - evolution.timedelta(days=200), 110),
            self.sample_run(3, end - evolution.timedelta(days=60), 100, regime="corpus-b"),
            self.sample_run(4, end - evolution.timedelta(days=20), 105, regime="corpus-b", skipped=("deadbeef",)),
            self.sample_run(5, end, 106, regime="environment-c", runner="runner-b"),
        ]
        intentional = {
            "id": "intentional-cost",
            "source": "authored",
            "location": {"kind": "commit", "commit": runs[3]["commit"]},
            "category": "capability",
            "title": "Intentional-cost feature",
            "explanation": "Accepted compile cost for a language capability.",
            "link": "https://github.com/rue-language/rue/pull/1",
            "scope": {"metrics": ["latency"], "workloads": ["*"], "platforms": ["*"]},
        }
        model = evolution.evolution_workspace(
            {"x86-64-linux": runs}, [intentional], self.Resolver()
        )
        series = next(item for item in model["series"] if item["workload"] == "all")
        self.assertEqual(series["ranges"]["30d"]["raw_point_count"], 2)
        self.assertEqual(series["ranges"]["90d"]["raw_point_count"], 3)
        self.assertEqual(series["ranges"]["1y"]["raw_point_count"], 4)
        self.assertEqual(series["ranges"]["all"]["raw_point_count"], 5)
        self.assertEqual(series["ranges"]["30d"]["resolution"], "day")
        self.assertEqual(series["ranges"]["1y"]["resolution"], "week")
        self.assertEqual(model["raw_history_run_count"], len(runs))
        self.assertEqual(model["metric_policy"], "benchmark_metrics.derive_history_metrics")
        self.assertEqual(model["annotation_policy"], "benchmark_annotations.normalized_annotation_stream")
        self.assertIs(model["annotations"][0], intentional)

        canonical = metrics.derive_history_metrics(runs)
        self.assertEqual(
            [point["index"] for point in series["raw_points"]],
            [point["performance_index"]["value"] for point in canonical["points"]],
        )
        points = series["raw_points"]
        self.assertEqual(points[2]["boundary"]["reason"], "regime_boundary")
        self.assertEqual(points[3]["boundary"]["skipped_commits"], ["deadbeef"])
        self.assertEqual(points[4]["boundary"]["reason"], "regime_boundary")
        self.assertIn("intentional-cost", points[3]["annotation_ids"])
        self.assertEqual(
            {point["segment"] for point in series["ranges"]["all"]["trend_points"]},
            {0, 1, 2, 3},
        )
        self.assertEqual(model["cross_platform_default"], "normalized_index_separate_machine_series")
        self.assertEqual(model["workload_families"][0]["source"], "identity_fallback")

    def test_non_latency_annotations_are_excluded_from_aggregate_and_workload(self):
        end = datetime(2026, 12, 31, tzinfo=timezone.utc)
        run = self.sample_run(1, end)
        events = []
        for metric in ("memory", "binary_size", "throughput"):
            events.append({
                "id": metric,
                "location": {"kind": "commit", "commit": run["commit"]},
                "scope": {"metrics": [metric], "workloads": ["*"], "platforms": ["*"]},
            })
        model = evolution.evolution_workspace({"x86-64-linux": [run]}, events, self.Resolver())
        aggregate = next(item for item in model["series"] if item["workload"] == "all")
        workload = next(item for item in model["series"] if item["workload"] == "probe")
        self.assertEqual(aggregate["raw_points"][0]["annotation_ids"], [])
        self.assertEqual(workload["raw_points"][0]["annotation_ids"], [])

    def test_shared_declared_family_unions_workloads_across_platforms(self):
        instant = datetime(2026, 12, 31, tzinfo=timezone.utc)
        left = self.sample_run(1, instant)
        right = self.sample_run(2, instant)
        left["benchmarks"][0].update(name="parser_probe", workload_family="compiler-core", workload_family_label="Compiler core")
        right["benchmarks"][0].update(name="codegen_probe", workload_family="compiler-core", workload_family_label="Compiler core")
        model = evolution.evolution_workspace(
            {"aarch64-linux": [right], "x86-64-linux": [left]}, [], self.Resolver()
        )
        family = next(item for item in model["workload_families"] if item["id"] == "compiler-core")
        self.assertEqual(family["workloads"], ["codegen_probe", "parser_probe"])
        inconsistent = self.sample_run(3, instant)
        inconsistent["benchmarks"][0].update(
            name="other", workload_family="compiler-core", workload_family_label="Different label"
        )
        with self.assertRaisesRegex(ValueError, "inconsistent workload-family metadata"):
            evolution.evolution_workspace(
                {"aarch64-linux": [right], "x86-64-linux": [left], "other": [inconsistent]},
                [], self.Resolver(),
            )
        fallback = self.sample_run(4, instant)
        declared_collision = self.sample_run(5, instant)
        declared_collision["benchmarks"][0].update(workload_family="identity:probe")
        with self.assertRaisesRegex(ValueError, "inconsistent workload-family metadata"):
            evolution.evolution_workspace(
                {"fallback": [fallback], "declared": [declared_collision]},
                [], self.Resolver(),
            )

    def test_one_year_uses_calendar_anniversary_across_leap_day(self):
        end = datetime(2028, 2, 29, 12, tzinfo=timezone.utc)
        runs = [
            self.sample_run(1, datetime(2027, 2, 27, 12, tzinfo=timezone.utc)),
            self.sample_run(2, datetime(2027, 2, 28, 12, tzinfo=timezone.utc)),
            self.sample_run(3, end),
        ]
        model = evolution.evolution_workspace({"x86-64-linux": runs}, [], self.Resolver())
        series = next(item for item in model["series"] if item["workload"] == "all")
        self.assertEqual(series["ranges"]["1y"]["raw_point_count"], 2)
        self.assertEqual(series["ranges"]["1y"]["raw_start_index"], 1)

    def test_trend_uses_exact_canonical_median_and_scaled_mad(self):
        start = datetime(2026, 1, 5, tzinfo=timezone.utc)
        runs = [
            self.sample_run(1, start, 100),
            self.sample_run(2, start + evolution.timedelta(hours=1), 200),
            self.sample_run(3, start + evolution.timedelta(hours=2), 400),
        ]
        model = evolution.evolution_workspace({"x86-64-linux": runs}, [], self.Resolver())
        series = next(item for item in model["series"] if item["workload"] == "all")
        trend = series["ranges"]["all"]["trend_points"][0]
        self.assertEqual(trend["index"], 50.0)
        self.assertAlmostEqual(trend["variation"], 1.4826 * 25.0)
        self.assertEqual(metrics.robust_summary([100.0, 50.0, 25.0])["scaled_mad"], trend["variation"])

    def test_high_velocity_history_is_thinned_only_for_rendering(self):
        start = datetime(2026, 1, 1, tzinfo=timezone.utc)
        runs = [
            self.sample_run(
                index + 1,
                start + evolution.timedelta(hours=index),
                100 + (index % 7),
                regime="corpus-a" if index < 200 else "corpus-b",
                skipped=("feedface",) if index == 350 else (),
            )
            for index in range(500)
        ]
        annotation = {
            "id": "non-boundary-milestone",
            "location": {"kind": "commit", "commit": runs[123]["commit"]},
            "scope": {"metrics": ["latency"], "workloads": ["*"], "platforms": ["*"]},
        }
        model = evolution.evolution_workspace(
            {"x86-64-linux": runs}, [annotation], self.Resolver()
        )
        series = next(
            item for item in model["series"] if item["workload"] == "all"
        )
        view = series["ranges"]["all"]
        self.assertEqual(len(series["raw_points"]), 500)
        self.assertEqual(view["rendered_raw_point_count"], evolution.MAX_RENDERED_RAW_POINTS)
        self.assertLess(len(view["trend_points"]), len(series["raw_points"]))
        self.assertEqual(view["rendered_raw_points"][0]["commit"], runs[0]["commit"])
        self.assertEqual(view["rendered_raw_points"][-1]["commit"], runs[-1]["commit"])
        rendered = {point["commit"] for point in view["rendered_raw_points"]}
        self.assertIn(runs[200]["commit"], rendered)
        self.assertIn(runs[350]["commit"], rendered)
        self.assertIn(runs[123]["commit"], rendered)

    def test_evolution_template_only_renders_precomputed_policy(self):
        template = (INPUT_ROOT / "website" / "templates" / "performance.html").read_text()
        self.assertIn("series.raw_points.slice(series.ranges[activeRange].raw_start_index)", template)
        self.assertIn("view.trend_points.forEach", template)
        self.assertIn("point.absolute_ms.toFixed", template)
        self.assertIn("point.boundary.skipped_commits.join", template)
        self.assertIn("All machines (normalized, separate series)", template)
        self.assertIn("Calendar time", template)
        self.assertIn("Normalized performance index", template)
        self.assertIn("evolution-boundary-marker", template)
        self.assertIn("evolution-annotation-marker", template)
        self.assertIn("point.boundary.regime_id", template)
        self.assertIn("point.boundary.represented_commits.join", template)
        self.assertNotIn("function calculateEvolutionIndex", template)


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
