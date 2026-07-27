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
parser_profile = load_module("parser_profile", "scripts/parser-profile.py")
manifest_tools = load_module("benchmark_manifest", "scripts/benchmark_manifest.py")
scaling = load_module("benchmark_scaling", "scripts/benchmark_scaling.py")
scenario_tools = load_module("benchmark_scenarios", "scripts/benchmark_scenarios.py")


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
        # The share is paired per sample before the median, exactly like the
        # absolute residual: median(2/11, 1/10, 3/12) as percentages.
        self.assertAlmostEqual(result["unattributed_percent"], 2.0 / 11.0 * 100.0)
        self.assertEqual(result["inclusive_rows"], [("parse", 5.5, 50.0)])
        self.assertEqual(result["overlapping_leaf_work_ms"], 0.0)
        self.assertEqual(result["overlapping_leaf_work_mad_ms"], 0.0)
        self.assertEqual(result["peak_memory_bytes"], 1024.0)
        self.assertEqual(result["peak_memory_mad_bytes"], 0.0)
        self.assertEqual(
            [name for name, _ms, _percent in result["rows"]],
            ["lexer", "parser", "codegen"],
        )

    def test_driver_phases_aggregate_outside_the_compiler_total(self):
        samples = []
        for compile_ms, write_ms in ((10.0, 1.0), (12.0, 3.0), (11.0, 2.0)):
            sample = self.sample(compile_ms, compile_ms + 5.0)
            sample["driver_phases"] = [
                {"name": "output_write", "duration_ms": write_ms, "invocations": 1}
            ]
            samples.append(sample)
        result = perf_baseline.aggregate(samples)
        self.assertEqual(result["driver_phases"], [("output_write", 2.0)])
        # Driver phases describe process-minus-root overhead only; they must
        # not enter the leaf table or shrink the unattributed residual.
        self.assertNotIn("output_write", [name for name, _ms, _pct in result["rows"]])
        self.assertEqual(result["unattributed_ms"], 2.0)

    def test_driver_phase_shape_and_membership_are_validated(self):
        def with_phases(phases):
            sample = self.sample(10.0, 15.0)
            sample["driver_phases"] = phases
            return sample

        cases = [
            ([{"duration_ms": 1.0, "invocations": 1}], "has no name"),
            (
                [{"name": "output_write", "duration_ms": -1.0, "invocations": 1}],
                "must be a finite non-negative number",
            ),
            (
                [{"name": "output_write", "duration_ms": 1.0, "invocations": 0}],
                "must be greater than zero",
            ),
            (
                [
                    {"name": "output_write", "duration_ms": 1.0, "invocations": 1},
                    {"name": "output_write", "duration_ms": 2.0, "invocations": 1},
                ],
                "duplicate driver phase",
            ),
        ]
        for phases, message in cases:
            with self.subTest(phases=phases):
                with self.assertRaisesRegex(ValueError, message):
                    perf_baseline.aggregate([with_phases(phases)])

        with self.assertRaisesRegex(ValueError, "driver phase set drifted"):
            perf_baseline.aggregate(
                [
                    with_phases(
                        [
                            {
                                "name": "output_write",
                                "duration_ms": 1.0,
                                "invocations": 1,
                            }
                        ]
                    ),
                    with_phases([]),
                ]
            )

    def test_unattributed_bound_fails_only_above_the_configured_tolerance(self):
        def result(name, percent):
            return {"name": name, "unattributed_percent": {"median": percent}}

        results = [result("tight", 9.0), result("loose", 30.0), result("edge", 25.0)]
        self.assertEqual(
            perf_baseline.unattributed_violations(results, 25.0),
            [("loose", 30.0)],
        )
        self.assertEqual(perf_baseline.unattributed_violations(results, 100.0), [])
        self.assertEqual(
            [name for name, _percent in perf_baseline.unattributed_violations(results, 5.0)],
            ["tight", "loose", "edge"],
        )

    def test_inclusive_ratio_handles_zero_root_and_is_paired_before_median(self):
        zero = self.sample(
            0.0,
            0.0,
            leaf_durations={"lexer": 0.0, "parser": 0.0, "codegen": 0.0},
        )
        next(pass_data for pass_data in zero["passes"] if pass_data["name"] == "parse")[
            "duration_ms"
        ] = 0.0
        self.assertEqual(
            perf_baseline.aggregate([zero])["inclusive_rows"],
            [("parse", 0.0, 0.0)],
        )

        samples = [
            self.sample(10.0, 11.0),
            self.sample(100.0, 101.0),
            self.sample(1000.0, 1001.0),
        ]
        for sample, duration in zip(samples, (1.0, 40.0, 100.0)):
            next(
                pass_data
                for pass_data in sample["passes"]
                if pass_data["name"] == "parse"
            )["duration_ms"] = duration
        inclusive = perf_baseline.aggregate(samples)["inclusive_rows"][0]
        self.assertEqual(inclusive[1], 40.0)
        self.assertEqual(inclusive[2], 10.0)

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
            "inclusive_passes": [
                {"name": "parser", "median_ms": 6.0, "median_percent": 30.0}
            ],
            "source_metrics": {"files": 1, "lines": 1, "bytes": 10, "tokens": 5},
            "driver_overhead_ms": {"median": 5.0, "mad": 0.0},
            "leaf_to_root_ratio_percent": {"median": 40.0, "mad": 0.0},
            "unattributed_ms": {"median": 12.0, "mad": 0.0},
            "unattributed_percent": {"median": 60.0, "mad": 0.0},
            "driver_phases": [{"name": "output_write", "median_ms": 1.5}],
            "overlapping_leaf_work_ms": {"median": 0.0, "mad": 0.0},
            "peak_memory_bytes": None,
        }
        self.assertEqual(
            perf_baseline.hot_passes([result]),
            [("lexer", 5.0, 25.0), ("future_phase", 3.0, 15.0)],
        )
        markdown = perf_baseline._md_table([result])
        self.assertIn("future_phase", markdown)
        self.assertIn("**20.00**", markdown)
        self.assertIn("**25.00**", markdown)
        with mock.patch("builtins.print") as output:
            perf_baseline.print_text([result], 3)
        self.assertTrue(
            any("parser (inclusive)" in str(call) for call in output.call_args_list)
        )
        v1 = dict(result)
        v1.pop("inclusive_passes")
        self.assertNotIn("parser (inclusive)", perf_baseline._md_table([v1]))

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
        inclusive = results[0]["inclusive_passes"][0]
        self.assertEqual(inclusive["name"], "parse")
        self.assertAlmostEqual(inclusive["median_percent"], 55.0)

    def test_deep_nesting_corpus_shape_and_implicit_cli_complexity_gate(self):
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
        # CLI cases without a named contract use the shared ordinary contract,
        # whose Rust unit test pins both compiler and produced-program phases to
        # DEFAULT_TIMEOUT_MS (the test-runner's strict 10-second default). Keep
        # this case on that single source of truth instead of duplicating a
        # per-case timeout that can drift from the harness default.
        self.assertNotIn("contract =", cli_case)
        self.assertNotIn("timeout_ms", cli_case)

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


class ParserProfileTests(unittest.TestCase):
    def test_fixture_matrix_covers_required_parser_regimes(self):
        with tempfile.TemporaryDirectory() as directory:
            fixtures = parser_profile.write_fixtures(Path(directory))
            self.assertEqual(
                [scale for _name, kind, scale, _files in fixtures if kind == "malformed"],
                [8, 32, 128, 512],
            )
            self.assertEqual(
                [scale for _name, kind, scale, _files in fixtures if kind == "adversarial"],
                [12, 24, 48, 96, 192],
            )
            malformed = fixtures[4][3][0].read_text()
            adversarial = fixtures[-1][3][0].read_text()
            self.assertEqual(malformed.count("fn broken_"), 8)
            self.assertEqual(adversarial.count("{ "), 64 * 193)

    def test_scaling_summary_normalizes_endpoint_growth_by_tokens(self):
        results = [
            {
                "class": "adversarial",
                "scale": 12,
                "tokens": 100,
                "parser_ms": {"median": 1.0},
                "allocations": {"median": 50},
            },
            {
                "class": "adversarial",
                "scale": 192,
                "tokens": 1000,
                "parser_ms": {"median": 8.0},
                "allocations": {"median": 500},
            },
        ]
        summary = parser_profile.scaling_summary(results, "adversarial")
        self.assertEqual(summary["token_growth"], 10)
        self.assertEqual(summary["time_growth_per_token_growth"], 0.8)
        self.assertEqual(summary["allocation_growth_per_token_growth"], 1.0)

    def test_profile_run_enforces_subprocess_timeout(self):
        with mock.patch.object(
            parser_profile.subprocess,
            "run",
            side_effect=subprocess.TimeoutExpired(["driver"], 0.25),
        ):
            with self.assertRaisesRegex(RuntimeError, "exceeded 0.25s timeout"):
                parser_profile.run("driver", ["fixture.rue"], 1, 0, 0.25)


class BenchmarkValidationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.expected_names = validator.load_manifest_names(
            INPUT_ROOT / "benchmarks" / "manifest.toml"
        )

    def valid_result(self):
        result = {
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
                    "passes": {"compile": {
                        "mean_ms": index + 0.5,
                        "invocations": 1,
                        "root_invocations": 1,
                        "leaf_invocations": 0,
                    }},
                }
                for index, name in enumerate(self.expected_names)
            ],
        }
        manifest_path = INPUT_ROOT / "benchmarks" / "manifest.toml"
        manifest_tools.enrich_results(result, manifest_path)
        families = []
        for family in manifest_tools.scaling_families(manifest_path):
            tiers = [
                {
                    "input_size": size,
                    "samples_ms": [float(size)] * 5,
                    "peak_memory_samples_bytes": [size * 1024] * 5,
                    "source_metrics": {
                        "bytes": size * 8,
                        "files": 1,
                        "lines": size,
                        "tokens": size * 4,
                    },
                    "passes": {
                        family["focus_pass"]: {
                            "mean_ms": float(size),
                            "invocations": size,
                            "root_invocations": 0,
                            "leaf_invocations": size,
                        }
                    },
                }
                for size in family["sizes"]
            ]
            families.append(scaling.derive_family(family, tiers))
        result["scaling"] = {
            "schema_version": 1,
            "measurement_family": "compiler_scaling_diagnostics",
            "scenario_family": "synthetic_phase_probes",
            "representative": False,
            "iterations": 5,
            "families": families,
            "budget_status": "passed",
        }
        scenario_families = manifest_tools.scenario_families(manifest_path)
        reused = []
        for declared in scenario_families[1]["scenarios"]:
            work = {
                artifact: {"required": declared[artifact][0], "reused": declared[artifact][1]}
                for artifact in ("modules", "semantic_bodies", "cfgs", "semantic_queries")
            }
            row = {
                "name": declared["name"], "samples_ms": [1.0] * 5, "mean_ms": 1.0,
                "required_vs_reused_work": work,
            }
            if declared["name"] == "diagnostic_error_edit":
                row["diagnostic_parity"] = {"fresh_session": "exact", "last_good_preserved": True}
            else:
                row["differential_parity"] = {
                    "dependency_records": "exact", "diagnostics": "exact",
                    "durable_ordinary_body_manifest_artifacts": "exact",
                    "durable_specialized_body_payloads": "exact",
                    "emitted_output": "byte_exact", "public_semantic_cfg_artifacts": "exact",
                    "public_type_universe": "exact_ordered_entries", "stable_identities": "exact",
                    "warnings": "exact",
                    "emitted_output_sha256": "a" * 64,
                    "emitted_output_size_bytes": 100,
                }
            if declared["name"] == "diagnostic_recovery":
                row["recovered_from_diagnostic_error"] = True
            reused.append(row)
        result["scenarios"] = {
            "schema_version": 1,
            "measurement_family": "compiler_build_and_query_performance",
            "representative": True,
            "generated_program_runtime": False,
            "cross_family_emitted_output": {
                "cold_batch_vs_fresh_and_reused_session": "byte_exact",
                "representation": "raw_compiler_executable_before_platform_postprocessing",
                "sha256": "a" * 64,
                "size_bytes": 100,
            },
            "iterations": 5,
            "families": [
                {
                    **{field: scenario_families[0][field] for field in ("name", "interpretation", "not_runtime")},
                    "scenarios": [{
                        "name": "cold_batch_root_compiler", "samples_ms": [2.0] * 5, "mean_ms": 2.0,
                        "emitted_output": {"parity": "byte_exact", "representation": "raw_compiler_executable_before_platform_postprocessing", "sha256": "a" * 64, "size_bytes": 100},
                    }],
                },
                {
                    **{field: scenario_families[1][field] for field in ("name", "interpretation", "not_runtime")},
                    "scenarios": reused,
                },
            ],
        }
        return result

    def test_exact_manifest_corpus_is_accepted(self):
        result = self.valid_result()
        self.assertEqual(validator.validate_results(result, self.expected_names), [])
        result["benchmarks"].reverse()
        self.assertEqual(validator.validate_results(result, self.expected_names), [])

    def test_manifest_classifies_every_probe_and_verified_dimensions_match(self):
        manifest_path = INPUT_ROOT / "benchmarks" / "manifest.toml"
        manifest = manifest_tools.load_manifest(manifest_path)
        self.assertEqual(manifest_tools.validate_static_dimensions(manifest_path), [])
        self.assertEqual(manifest_tools.validate_readme_inventory(manifest_path), [])
        self.assertEqual(len(manifest["benchmark"]), 7)
        self.assertEqual(len(manifest["scaling_family"]), 5)
        self.assertEqual(len(manifest["scenario_family"]), 2)
        for probe in manifest["benchmark"]:
            self.assertEqual(probe["kind"], "synthetic_phase_probe")
            self.assertIn("not representative", probe["non_representative"].lower())
            self.assertTrue(probe["subsystem"])
            self.assertTrue(probe["workload_family"])
        for family in manifest["scaling_family"]:
            self.assertIn("not", family["non_representative"].lower())
            self.assertIn("runtime", family["non_representative"].lower())
        subsystems = {family["subsystem"].split("/", 1)[0] for family in manifest["scaling_family"]}
        self.assertEqual(subsystems, {"frontend", "semantic", "cfg", "backend"})

    def test_append_authority_rejects_copied_manifest_with_false_dimensions(self):
        original = INPUT_ROOT / "benchmarks/manifest.toml"
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            copied = root / "benchmarks/manifest.toml"
            copied.parent.mkdir()
            copied.write_text(
                original.read_text().replace(
                    "dimensions = { source_lines = 2120,",
                    "dimensions = { source_lines = 2121,",
                    1,
                )
            )
            (copied.parent / "README.md").write_bytes(
                (original.parent / "README.md").read_bytes()
            )
            for relative, source in manifest_tools.corpus_paths(original):
                target = copied.parent / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(source.read_bytes())
            errors = validator.validate_manifest_results(self.valid_result(), copied)
        self.assertTrue(any("manifest=2121, source=2120" in error for error in errors))

    def test_scaling_generators_are_deterministic_and_monotonic(self):
        workload_module = load_module("scaling_workloads_test", "scripts/scaling_workloads.py")
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            for name in workload_module.GENERATORS:
                first = workload_module.generate(name, root / "first", 12)
                second = workload_module.generate(name, root / "second", 12)
                first_files = sorted(
                    (path.relative_to(first.parent), path.read_text())
                    for path in first.parent.rglob("*.rue")
                )
                second_files = sorted(
                    (path.relative_to(second.parent), path.read_text())
                    for path in second.parent.rglob("*.rue")
                )
                self.assertEqual(first_files, second_files)
                larger = workload_module.generate(name, root / "larger", 36)
                self.assertGreater(
                    sum(path.stat().st_size for path in larger.parent.rglob("*.rue")),
                    sum(path.stat().st_size for path in first.parent.rglob("*.rue")),
                )

    def test_scaling_growth_budget_is_size_normalized(self):
        family = manifest_tools.scaling_families(INPUT_ROOT / "benchmarks/manifest.toml")[0]
        def tier(size, latency, samples=5):
            return {
                "input_size": size,
                "samples_ms": [latency] * samples,
                "peak_memory_samples_bytes": [size * 1024] * samples,
                "source_metrics": {"bytes": size, "files": 1, "lines": size, "tokens": size},
                "passes": {family["focus_pass"]: {
                    "mean_ms": latency,
                    "invocations": 1,
                    "root_invocations": 0,
                    "leaf_invocations": 1,
                }},
            }

        quiet = scaling.derive_family(
            family, [tier(100, 10.0), tier(300, 30.0), tier(900, 90.0)]
        )
        self.assertEqual(
            quiet["adjacent_growth"][0]["latency_per_unit_growth"]["center"],
            1.0,
        )
        self.assertEqual(quiet["classification"], "within_normalized_growth_budget")
        self.assertEqual(scaling.budget_status([quiet]), "passed")

        proven = scaling.derive_family(
            family, [tier(100, 10.0), tier(300, 90.0), tier(900, 810.0)]
        )
        self.assertEqual(proven["classification"], "budget_exceeded")
        self.assertEqual(scaling.budget_status([proven]), "failed")
        self.assertGreater(
            proven["adjacent_growth"][0]["latency_per_unit_growth"]["lower_bound"],
            family["latency_per_unit_growth_budget"],
        )

        noisy_tiers = [tier(100, 10.0), tier(300, 60.0), tier(900, 180.0)]
        noisy_tiers[0]["samples_ms"] = [9.0, 10.0, 11.0]
        noisy_tiers[1]["samples_ms"] = [50.0, 60.0, 70.0]
        noisy_tiers[2]["samples_ms"] = [150.0, 180.0, 210.0]
        noisy = scaling.derive_family(family, noisy_tiers)
        self.assertEqual(noisy["classification"], "indeterminate")
        self.assertEqual(scaling.budget_status([noisy]), "indeterminate")
        self.assertEqual(
            noisy["adjacent_growth"][0]["latency_per_unit_growth"]["reason"],
            "uncertainty_crosses_budget",
        )

        isolated_outlier_tiers = [
            tier(100, 10.0),
            tier(300, 30.0),
            tier(900, 90.0),
        ]
        for current, center in zip(isolated_outlier_tiers, (10.0, 30.0, 90.0)):
            current["samples_ms"] = [center, center, center, center, center * 100]
        isolated_outlier = scaling.derive_family(family, isolated_outlier_tiers)
        self.assertEqual(
            isolated_outlier["tiers"][0]["latency_summary_ms"][
                "dispersion_classification"
            ],
            "robust",
        )
        self.assertEqual(
            isolated_outlier["classification"],
            "within_normalized_growth_budget",
        )

        opposing_outlier_tiers = [
            tier(100, 10.0),
            tier(300, 30.0),
            tier(900, 90.0),
        ]
        for current, center in zip(opposing_outlier_tiers, (10.0, 30.0, 90.0)):
            current["samples_ms"] = [center / 100, center, center, center, center * 100]
        opposing_outliers = scaling.derive_family(family, opposing_outlier_tiers)
        self.assertEqual(
            opposing_outliers["tiers"][0]["latency_summary_ms"][
                "dispersion_classification"
            ],
            "extreme_range",
        )
        self.assertEqual(opposing_outliers["classification"], "indeterminate")

        one_sample = scaling.derive_family(
            family,
            [tier(100, 10.0, 1), tier(300, 90.0, 1), tier(900, 810.0, 1)],
        )
        self.assertEqual(one_sample["classification"], "indeterminate")
        self.assertEqual(
            one_sample["adjacent_growth"][0]["latency_per_unit_growth"]["reason"],
            "at_least_three_samples_required",
        )

        extreme_tiers = [
            tier(100, 10.0),
            tier(300, 30.0),
            tier(900, 90.0),
        ]
        for current, center in zip(extreme_tiers, (10.0, 30.0, 90.0)):
            current["samples_ms"] = [center, center, center, center * 100, center * 100]
        extreme = scaling.derive_family(family, extreme_tiers)
        summary = extreme["tiers"][0]["latency_summary_ms"]
        self.assertEqual(summary["scaled_mad"], 0.0)
        self.assertEqual(summary["minimum"], 10.0)
        self.assertEqual(summary["maximum"], 1000.0)
        self.assertEqual(summary["range"], 990.0)
        self.assertEqual(summary["dispersion_classification"], "extreme_range")
        self.assertEqual(extreme["classification"], "indeterminate")
        self.assertEqual(scaling.budget_status([extreme]), "indeterminate")
        self.assertEqual(
            extreme["adjacent_growth"][0]["latency_per_unit_growth"]["reason"],
            "extreme_sample_range",
        )
        self.assertIn("per_tier_compile_timeout_seconds", quiet["budgets"])

    def test_range_guard_trims_one_edge_outlier_per_five_samples(self):
        # Five samples tolerate one isolated scheduler delay but not two.
        one_outlier = [10.0] * 4 + [1000.0]
        self.assertEqual(
            metrics.robust_summary(one_outlier)["dispersion_classification"],
            "robust",
        )
        two_outliers = [0.1] + [10.0] * 3 + [1000.0]
        self.assertEqual(
            metrics.robust_summary(two_outliers)["dispersion_classification"],
            "extreme_range",
        )
        # A noise-driven top-up to ten samples earns a second trimmed edge, so
        # the same two isolated outliers no longer poison the evidence.
        topped_up = [0.1] + [10.0] * 8 + [1000.0]
        self.assertEqual(
            metrics.robust_summary(topped_up)["dispersion_classification"],
            "robust",
        )
        # More isolated outliers than the trim allowance stay extreme: ten
        # samples earn two trims, so a third outlier still poisons the run.
        three_outliers = [10.0] * 7 + [1000.0, 2000.0, 3000.0]
        self.assertEqual(
            metrics.robust_summary(three_outliers)["dispersion_classification"],
            "extreme_range",
        )

    def test_range_guard_trusts_a_healthy_mad_despite_a_wide_range(self):
        # A tight core with two one-sided scheduler spikes (the shared-runner
        # regime that filed the trunk benchmark-failure issue): the scaled MAD
        # is a healthy fraction of the center, so it honestly represents the
        # spread and its growth bounds are trustworthy. The raw range exceeds
        # the guard threshold, but distrusting it here only manufactures
        # indeterminate false alarms, so the samples stay robust.
        healthy = [92.0, 96.0, 100.0, 175.0, 180.0]
        summary = metrics.robust_summary(healthy)
        self.assertGreater(summary["scaled_mad"], 100.0 * metrics.MAD_TRUST_FRACTION)
        self.assertGreater(
            summary["range"], max(100.0 * 0.5, summary["scaled_mad"] * 6.0)
        )
        self.assertEqual(summary["dispersion_classification"], "robust")

        # The guard still fires when the same wide range hides behind a
        # deceptively tight MAD: a clustered core plus two far outliers keeps
        # the scaled MAD near zero, so the evidence cannot be certified.
        deceptive = [99.0, 100.0, 100.0, 300.0, 305.0]
        deceptive_summary = metrics.robust_summary(deceptive)
        self.assertLessEqual(
            deceptive_summary["scaled_mad"], 100.0 * metrics.MAD_TRUST_FRACTION
        )
        self.assertEqual(
            deceptive_summary["dispersion_classification"], "extreme_range"
        )

    def test_scaling_runner_resamples_noisy_tiers_until_evidence_is_determinate(self):
        family = manifest_tools.scaling_families(INPUT_ROOT / "benchmarks/manifest.toml")[0]
        centers = dict(zip(family["sizes"], (10.0, 30.0, 90.0)))

        def payload(total_ms):
            return json.dumps(
                {
                    "total_ms": total_ms,
                    "passes": [{"name": family["focus_pass"], "duration_ms": total_ms}],
                    "source_metrics": {"bytes": 8, "files": 1, "lines": 1, "tokens": 4},
                }
            )

        def run_with_samples(sample_for_call):
            calls = {}

            def fake_compile(rue_bin, source, output, platform, timeout):
                size = int(source.parent.name)
                index = calls.get(size, 0)
                calls[size] = index + 1
                return payload(sample_for_call(size, index)), size * 1024

            with tempfile.TemporaryDirectory() as temp:
                with mock.patch.object(
                    scaling, "generate", side_effect=lambda name, root, size: root / "main.rue"
                ), mock.patch.object(scaling, "compile_once", side_effect=fake_compile):
                    return scaling.run_family(family, Path("rue"), 5, "darwin", Path(temp))

        # Two opposing scheduler outliers in the middle tier's first round
        # made round one indeterminate; one top-up round of clean samples
        # must recover determinate, in-budget evidence.
        def transient_noise(size, index):
            if size == family["sizes"][1] and index in (3, 4):
                return centers[size] / 100 if index == 3 else centers[size] * 100
            return centers[size]

        recovered = run_with_samples(transient_noise)
        self.assertEqual(recovered["classification"], "within_normalized_growth_budget")
        self.assertEqual(scaling.budget_status([recovered]), "passed")
        self.assertEqual(
            [len(tier["samples_ms"]) for tier in recovered["tiers"]], [10, 10, 10]
        )

        # Persistent noise never converges; sampling must stop at
        # MAX_SAMPLE_MULTIPLIER rounds and report indeterminate as before.
        def persistent_noise(size, index):
            if size == family["sizes"][1]:
                return centers[size] * 100 if index % 2 else centers[size]
            return centers[size]

        capped = run_with_samples(persistent_noise)
        self.assertEqual(capped["classification"], "indeterminate")
        self.assertEqual(scaling.budget_status([capped]), "indeterminate")
        self.assertEqual(
            [len(tier["samples_ms"]) for tier in capped["tiers"]],
            [5 * scaling.MAX_SAMPLE_MULTIPLIER] * 3,
        )

    def test_scaling_validation_accepts_topped_up_tiers_and_bounds_sample_counts(self):
        manifest_path = INPUT_ROOT / "benchmarks/manifest.toml"
        declared = manifest_tools.scaling_families(manifest_path)[0]

        def build_result(middle_tier_samples):
            result = self.valid_result()
            counts = [5, middle_tier_samples, 5]
            tiers = [
                {
                    "input_size": size,
                    "samples_ms": [float(size)] * count,
                    "peak_memory_samples_bytes": [size * 1024] * count,
                    "source_metrics": {
                        "bytes": size * 8,
                        "files": 1,
                        "lines": size,
                        "tokens": size * 4,
                    },
                    "passes": {
                        declared["focus_pass"]: {
                            "mean_ms": float(size),
                            "invocations": size,
                            "root_invocations": 0,
                            "leaf_invocations": size,
                        }
                    },
                }
                for size, count in zip(declared["sizes"], counts)
            ]
            result["scaling"]["families"][0] = scaling.derive_family(declared, tiers)
            return result

        # A tier topped up within the multiplier publishes cleanly.
        topped_up = build_result(5 * scaling.MAX_SAMPLE_MULTIPLIER)
        self.assertEqual(
            validator.validate_manifest_results(topped_up, manifest_path), []
        )
        # Counts beyond the multiplier or below the base iterations are
        # invalid evidence, not a bigger top-up.
        for count in (5 * scaling.MAX_SAMPLE_MULTIPLIER + 1, 4):
            errors = validator.validate_manifest_results(
                build_result(count), manifest_path
            )
            self.assertTrue(
                any("invalid latency samples" in error for error in errors),
                errors,
            )

    def test_scaling_cli_enforcement_rejects_failed_and_indeterminate(self):
        manifest_path = INPUT_ROOT / "benchmarks/manifest.toml"
        argv = [
            "benchmark_scaling.py",
            "--manifest",
            str(manifest_path),
            "--rue-bin",
            "/unused/rue",
            "--enforce-budgets",
        ]
        failed = {
            "budget_status": "failed",
            "families": [
                {
                    "name": "probe",
                    "classification": "budget_exceeded",
                    "violations": [
                        {
                            "metric": "latency",
                            "from_size": 1,
                            "to_size": 2,
                            "lower_bound": 3.0,
                            "budget": 2.0,
                        }
                    ],
                }
            ],
        }
        indeterminate = {
            "budget_status": "indeterminate",
            "families": [
                {
                    "name": "probe",
                    "classification": "indeterminate",
                    "violations": [],
                    "adjacent_growth": [
                        {
                            "latency_per_unit_growth": {
                                "classification": "indeterminate",
                                "reason": "extreme_sample_range",
                            },
                            "memory_per_unit_growth": {
                                "classification": "within_budget",
                                "reason": None,
                            },
                        }
                    ],
                }
            ],
        }
        for result, expected, diagnostic in (
            (failed, 2, "Budget violation"),
            (indeterminate, 3, "Insufficient scaling evidence"),
        ):
            with self.subTest(status=result["budget_status"]):
                with mock.patch.object(sys, "argv", argv), mock.patch.object(
                    scaling, "run_scaling", return_value=result
                ), mock.patch("builtins.print") as printer:
                    self.assertEqual(scaling.main(), expected)
                self.assertTrue(
                    any(diagnostic in str(call) for call in printer.call_args_list)
                )

    def test_scaling_runner_enforces_bounded_per_tier_compile_timeout(self):
        family = manifest_tools.scaling_families(INPUT_ROOT / "benchmarks/manifest.toml")[0]
        with tempfile.TemporaryDirectory() as temp_dir:
            with mock.patch.object(scaling, "generate", return_value=Path(temp_dir) / "main.rue"):
                with mock.patch.object(
                    scaling,
                    "compile_once",
                    side_effect=subprocess.TimeoutExpired(["rue"], family["timeout_seconds"]),
                ):
                    with self.assertRaisesRegex(RuntimeError, "timed out after 20s"):
                        scaling.run_family(family, Path("rue"), 1, "darwin", Path(temp_dir))

    def test_scaling_validation_requires_structural_counters_and_exact_tiers(self):
        result = self.valid_result()
        manifest_path = INPUT_ROOT / "benchmarks/manifest.toml"
        self.assertEqual(validator.validate_manifest_results(result, manifest_path), [])
        family = result["scaling"]["families"][0]
        del family["tiers"][0]["passes"][family["focus_pass"]]["invocations"]
        self.assertTrue(
            any(
                "structural counter" in error
                for error in validator.validate_manifest_results(
                    result, manifest_path
                )
            )
        )

    def test_scaling_validation_rederives_all_claims_and_rejects_bad_raw_evidence(self):
        manifest_path = INPUT_ROOT / "benchmarks/manifest.toml"
        mutations = [
            lambda result: result["scaling"].update(measurement_family="other"),
            lambda result: result["scaling"]["families"][0].update(
                classification="budget_exceeded"
            ),
            lambda result: result["scaling"]["families"][0]["adjacent_growth"][0][
                "latency_per_unit_growth"
            ].update(lower_bound=0.0),
            lambda result: result["scaling"]["families"][0]["tiers"][0][
                "samples_ms"
            ].__setitem__(0, float("nan")),
            lambda result: result["scaling"]["families"][0]["tiers"][0][
                "peak_memory_samples_bytes"
            ].__setitem__(0, True),
            lambda result: result["scaling"]["families"][0]["tiers"][0][
                "passes"
            ][result["scaling"]["families"][0]["focus_pass"]].update(
                invocations=True
            ),
            lambda result: result["scaling"]["families"][0]["tiers"][0][
                "passes"
            ][result["scaling"]["families"][0]["focus_pass"]].update(
                invocations=0
            ),
            lambda result: result["scaling"]["families"][0].update(tiers=None),
            lambda result: result["scaling"].update(budget_status="failed"),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                result = self.valid_result()
                mutate(result)
                self.assertTrue(validator.validate_manifest_results(result, manifest_path))

        proven = self.valid_result()
        declared = manifest_tools.scaling_families(manifest_path)[0]
        existing = proven["scaling"]["families"][0]["tiers"]
        raw = [
            {
                "input_size": tier["input_size"],
                "samples_ms": [value] * 5,
                "peak_memory_samples_bytes": tier["peak_memory_samples_bytes"],
                "passes": tier["passes"],
                "source_metrics": tier["source_metrics"],
            }
            for tier, value in zip(existing, (10.0, 90.0, 810.0))
        ]
        proven["scaling"]["families"][0] = scaling.derive_family(declared, raw)
        proven["scaling"]["budget_status"] = "failed"
        self.assertTrue(
            any(
                "proven complexity budget violation" in error
                for error in validator.validate_manifest_results(proven, manifest_path)
            )
        )

        indeterminate = self.valid_result()
        declared = manifest_tools.scaling_families(manifest_path)[0]
        existing = indeterminate["scaling"]["families"][0]["tiers"]
        raw = []
        for tier, center in zip(existing, (10.0, 30.0, 90.0)):
            raw.append(
                {
                    "input_size": tier["input_size"],
                    "samples_ms": [center, center, center, center * 100, center * 100],
                    "peak_memory_samples_bytes": tier["peak_memory_samples_bytes"],
                    "passes": tier["passes"],
                    "source_metrics": tier["source_metrics"],
                }
            )
        indeterminate["scaling"]["families"][0] = scaling.derive_family(
            declared, raw
        )
        indeterminate["scaling"]["budget_status"] = "indeterminate"
        self.assertTrue(
            any(
                "insufficient evidence for publication" in error
                for error in validator.validate_manifest_results(
                    indeterminate, manifest_path
                )
            )
        )

    def test_website_publishes_scaling_separately_from_phase_probe_headline(self):
        template = (INPUT_ROOT / "website/templates/performance.html").read_text()
        self.assertIn("Synthetic compiler scaling diagnostics", template)
        self.assertIn("excluded from the aggregate headline", template)
        self.assertIn("latency_per_unit_growth", template)
        self.assertIn("node('caption'", template)
        self.assertIn("setAttribute('scope', 'col')", template)
        self.assertIn("scaled_mad", template)
        self.assertIn("extreme dispersion", template)
        result = self.valid_result()
        self.assertIs(charts.scaling_diagnostics([result]), result["scaling"])
        phase_total = charts.get_total_time(result)
        result["scaling"]["families"][0]["tiers"][0]["median_ms"] = 10**12
        self.assertEqual(charts.get_total_time(result), phase_total)
        representative = {"scaling": {**result["scaling"], "representative": True}}
        self.assertIsNone(charts.scaling_diagnostics([representative]))
        self.assertIs(charts.scenario_diagnostics([result]), result["scenarios"])
        self.assertIn("Representative compiler build/query scenarios", template)
        self.assertIn("required_vs_reused_work", template)

    def test_representative_scenario_validation_is_fail_closed(self):
        manifest_path = INPUT_ROOT / "benchmarks/manifest.toml"
        result = self.valid_result()
        self.assertEqual(validator.validate_manifest_results(result, manifest_path), [])
        result["scenarios"]["families"][1]["scenarios"][1]["required_vs_reused_work"]["modules"]["required"] += 1
        self.assertTrue(any("structural work" in error for error in validator.validate_manifest_results(result, manifest_path)))
        missing = self.valid_result()
        del missing["scenarios"]
        self.assertIn("benchmark result is missing representative scenarios", validator.validate_manifest_results(missing, manifest_path))

    def test_cross_family_output_parity_is_required(self):
        manifest_path = INPUT_ROOT / "benchmarks/manifest.toml"
        result = self.valid_result()
        result["scenarios"]["cross_family_emitted_output"]["sha256"] = "b" * 64
        errors = validator.validate_manifest_results(result, manifest_path)
        self.assertTrue(any("does not match the batch driver" in error for error in errors))

        families = manifest_tools.scenario_families(manifest_path)
        cold = {
            "name": "cold_batch_root_compiler",
            "samples_ms": [1.0],
            "mean_ms": 1.0,
            "emitted_output": {"parity": "byte_exact", "representation": "raw_compiler_executable_before_platform_postprocessing", "sha256": "a" * 64, "size_bytes": 100},
        }
        with mock.patch.object(scenario_tools, "collect_cold", return_value=cold), mock.patch.object(
            scenario_tools,
            "collect_reused",
            return_value=([], {"sha256": "b" * 64, "size_bytes": 100}),
        ):
            with self.assertRaisesRegex(ValueError, "differs from fresh/reused"):
                scenario_tools.collect(manifest_path, Path("rue"), Path("session"), 1)
        self.assertEqual(len(families), 2)

    def test_cold_output_must_equal_cross_family_identity(self):
        manifest_path = INPUT_ROOT / "benchmarks/manifest.toml"
        for field, value in (("sha256", "b" * 64), ("size_bytes", 101)):
            with self.subTest(field=field):
                result = self.valid_result()
                result["scenarios"]["families"][0]["scenarios"][0][
                    "emitted_output"
                ][field] = value
                errors = validator.validate_manifest_results(result, manifest_path)
                self.assertTrue(
                    any("does not match cross-family parity" in error for error in errors)
                )

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
            source = (INPUT_ROOT / "benchmarks/manifest.toml").read_text()
            manifest_path.write_text(source.replace(
                'name = "deep_nesting"', 'name = "many_functions"', 1
            ))
            with self.assertRaisesRegex(ValueError, "duplicate benchmark name"):
                validator.load_manifest_names(manifest_path)

            manifest_path.write_text(source.replace('name = "many_functions"\n', "", 1))
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
        corpus = {"schema_version": 2, "sha256": "a" * 64}
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

    def test_corpus_hash_includes_scaling_and_scenario_definitions(self):
        manifest = INPUT_ROOT / "benchmarks/manifest.toml"
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            copied_manifest = root / "benchmarks/manifest.toml"
            copied_manifest.parent.mkdir()
            copied_manifest.write_bytes(manifest.read_bytes())
            for relative, source in manifest_tools.corpus_paths(manifest):
                target = copied_manifest.parent / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(source.read_bytes())
            before = history_tools.corpus_metadata(copied_manifest)
            generator = root / "scripts/scaling_workloads.py"
            generator.write_text(generator.read_text() + "\n# workload definition changed\n")
            after = history_tools.corpus_metadata(copied_manifest)
            fixture = root / "benchmarks/scenarios/representative/main.rue"
            fixture.write_text(fixture.read_text() + "\n")
            after_fixture = history_tools.corpus_metadata(copied_manifest)
        self.assertEqual(before["schema_version"], 2)
        self.assertIn("../scripts/scaling_workloads.py", before["files"])
        self.assertIn("../scripts/benchmark_scenarios.py", before["files"])
        self.assertIn("../crates/rue/src/main.rs", before["files"])
        self.assertIn("../crates/rue-compiler-session-bench/src/main.rs", before["files"])
        self.assertNotEqual(before["sha256"], after["sha256"])
        self.assertNotEqual(after["sha256"], after_fixture["sha256"])

    def test_schema_one_and_two_corpora_remain_loadable_across_regime_boundary(self):
        def published(schema, digest, commit):
            run = {
                "timestamp": f"2026-07-{schema:02d}T00:00:00Z",
                "commit": commit,
                "build_mode": "release",
                "iterations": 3,
                "measurement_family": "compiler",
                "scenario_family": "cold_compilation",
                "benchmarks": [{
                    "name": "probe",
                    "samples_ms": [10.0, 10.0, 10.0],
                    "mean_ms": 10.0,
                    "passes": {"compile": {"mean_ms": 10.0}},
                    "timing_schema_version": 2,
                }],
            }
            corpus = {"schema_version": schema, "sha256": digest, "files": []}
            regime = history_tools.regime_metadata(run, "x86-64-linux", "runner", corpus)
            run["publication"] = {
                "schema_version": 3,
                "comparable": True,
                "regime_id": regime["id"],
                "regime": regime["dimensions"],
                "corpus": corpus,
                "timing_schema_versions": [2],
                "build_mode": "release",
                "compiler": {"commit": commit, "identity": f"rue@{commit}"},
                "iteration_policy": {"kind": "fixed", "iterations": 3},
                "platform": "x86-64-linux",
                "runner": {"image": "runner", "host": None},
                "metric_semantics": {"measurement_family": "compiler", "scenario_family": "cold_compilation"},
                "trigger_reason": "push",
                "coverage": {"measured_commit": commit, "represented_commits": [commit], "skipped_commits": [], "gap_unknown": False},
            }
            self.assertEqual(history_tools.validate_publication(run), [])
            return run

        old = published(1, "a" * 64, "old")
        current = published(2, "b" * 64, "current")
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history"
            history_tools.save_history(path, [old, current], "x86-64-linux")
            loaded = history_tools.load_history(path)["runs"]
        self.assertEqual(len(loaded), 2)
        self.assertTrue(history_tools.comparison_break(loaded[0], loaded[1]))
        self.assertEqual(metrics.compare_runs(loaded[1], loaded[0])["status"], "non_comparable")

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

    def test_status_uses_absolute_suite_time_and_thirty_calendar_days(self):
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
        self.assertEqual(status["latest_suite_ms"], 124)
        self.assertEqual(status["window"], "30d")
        self.assertNotIn("latest_index", status)


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

    def test_homepage_exposes_concise_annotation_coverage_and_deep_link(self):
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
        self.assertEqual(status["freshness"]["label"], "measured 3 days ago")
        self.assertEqual(status["annotation"]["category"], "capability")
        self.assertEqual(status["annotation"]["title"], "Intentional capability cost")
        self.assertEqual(status["coverage"], {
            "measured_commit": runs[-1]["commit"],
            "represented_count": 1,
            "skipped_count": 0,
            "gap_unknown": False,
        })
        self.assertEqual(
            status["annotation"]["dashboard_url"],
            "/performance/?range=30d&platform=x86-64-linux&annotation=capability-cost#evolution-workspace",
        )

    def test_homepage_excludes_events_outside_range_and_current_segment(self):
        runs = [
            self.sample_run(1, 100, day=1),
            self.sample_run(2, 101, day=2),
            self.sample_run(3, 99, day=3),
            self.sample_run(4, 100, regime="new", day=4),
            self.sample_run(5, 100, regime="new", day=5),
            self.sample_run(6, 100, regime="new", day=6),
        ]
        runs[0]["timestamp"] = "2026-05-01T00:00:00Z"
        def event(identifier, run):
            return {
                "id": identifier, "source": "authored",
                "location": {"kind": "commit", "commit": run["commit"]},
                "category": "optimization", "title": identifier,
                "explanation": identifier, "link": "https://github.com/rue-language/rue/pull/1",
                "scope": {"metrics": ["latency"], "workloads": ["*"], "platforms": ["*"]},
            }
        status = self.status(runs, [event("outside-range", runs[0]), event("old-segment", runs[2])])
        self.assertNotIn("annotation", status)
        self.assertEqual(status["n_runs"], 3)

    def test_homepage_prefers_authored_event_over_newer_derived_event(self):
        runs = [self.sample_run(index, 100, day=index) for index in range(1, 5)]
        def event(identifier, run, source):
            return {
                "id": identifier, "source": source,
                "location": {"kind": "commit", "commit": run["commit"]},
                "category": "optimization" if source == "authored" else "measurement_infrastructure",
                "title": identifier, "explanation": identifier,
                "link": "https://github.com/rue-language/rue/pull/1",
                "scope": {"metrics": ["latency"], "workloads": ["*"], "platforms": ["*"]},
            }
        status = self.status(runs, [event("authored", runs[2], "authored"), event("derived", runs[3], "derived")])
        self.assertEqual(status["annotation"]["id"], "authored")

    def test_missing_or_malformed_time_data_is_not_presented(self):
        self.assertIsNone(self.status([]))
        run = self.sample_run(1, 100)
        del run["timestamp"]
        self.assertIsNone(self.status([run]))

    def test_legacy_mean_is_absolute_but_does_not_fabricate_classification(self):
        run = self.sample_run(1, 100)
        run["benchmarks"][0]["samples_ms"] = [100, 100]
        status = self.status([run])
        self.assertEqual(status["state"]["kind"], "insufficient_data")
        self.assertEqual(status["n_trend_points"], 0)
        self.assertEqual(status["latest_suite_ms"], 100)
        self.assertNotIn("last_x", status)
        self.assertNotIn("last_y", status)

    def test_legacy_root_compile_timer_wins_over_inflated_mean(self):
        run = self.sample_run(1, 100)
        bench = run["benchmarks"][0]
        bench["samples_ms"] = [100, 100]
        bench["mean_ms"] = 999
        bench.setdefault("passes", {})["compile"] = {"mean_ms": 123}
        status = self.status([run])
        self.assertEqual(status["latest_suite_ms"], 123)

    def test_sparkline_uses_thirty_days_and_latest_segment(self):
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

    def test_sparkline_x_coordinates_use_calendar_distance(self):
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
        self.assertIn("latest_suite_ms", template)
        self.assertIn("status.bench.state.summary", template)
        self.assertIn("status.bench.latest_suite_ms", template)
        self.assertIn("comparable daily points", template)
        self.assertIn("history count unavailable", template)
        self.assertIn("benchmark data unavailable", template)
        self.assertIn("{% if status.bench.n_trend_points > 0 %}", template)
        self.assertIn("status.bench.coverage", template)
        self.assertIn("status.bench.annotation.dashboard_url", template)
        self.assertIn("status.bench.annotation.explanation", template)
        self.assertNotIn("annotated capability cost", template)


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

    def test_homepage_surfaces_the_most_relevant_annotation(self):
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
        self.assertEqual(status["annotation"]["source"], "authored")
        self.assertEqual(status["annotation"]["category"], "optimization")
        self.assertIn("annotation=", status["annotation"]["dashboard_url"])


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
        self.assertEqual(model["schema_version"], 2)
        self.assertAlmostEqual(model["points"][3]["suite_ms"], 171)
        self.assertEqual(
            {item["name"] for item in model["points"][3]["passes"]},
            {"parser", "codegen"},
        )
        self.assertEqual(
            next(item for item in model["points"][3]["passes"] if item["name"] == "parser")["mean_ms"],
            24,
        )
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
        self.assertEqual(model["comparisons"][-1]["before_point"]["subject"], "RUE-903: measured change 3")
        self.assertEqual(model["annotation_groups"][0]["title"], "Queryable compiler")
        point = model["points"][3]
        for field in (
            "commit", "date", "subject", "links", "freshness", "suite_ms", "workloads",
            "coverage", "comparability", "annotations",
        ):
            self.assertIn(field, point)
        self.assertIn("commit", point["links"])
        self.assertIn("issues", point["links"])
        self.assertEqual(point["annotations"][0]["category"], "capability")
        self.assertFalse(any(
            item["before"] == "2" * 40 and item["after"] == "4" * 40
            for item in model["comparisons"]
        ))

        explanation = model["comparisons"][-1]["explanation"]
        self.assertEqual(explanation["dominant_workload"]["name"], "probe")
        self.assertEqual(explanation["dominant_pass"]["name"], "parser")
        parser = next(item for item in explanation["passes"] if item["name"] == "parser")
        self.assertEqual(parser["delta_ms"], parser["after_ms"] - parser["before_ms"])
        probe_detail = next(item for item in explanation["workloads"] if item["name"] == "probe")
        self.assertEqual(probe_detail["dominant_pass"]["name"], "parser")
        self.assertEqual(
            next(item for item in probe_detail["passes"] if item["name"] == "parser")["delta_ms"],
            15,
        )
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
        for contract in (
            "Raw samples are insufficient", "point.coverage.skipped_commits.map",
            "point.links?.commit", "point.date", "point.subject", "point.freshness.latest",
            "point.comparability.headline", "point.annotations.forEach",
            "annotationBadge(event.category)", "point.links?.pull_requests", "point.links?.issues",
            "explanation.workloads.forEach", "explanation.memory_bytes", "explanation.binary_size_bytes",
            "fmt(value.before_ms)", "headline.classification === 'insufficient_data'",
            "populateComparisons()",
        ):
            self.assertIn(contract, template)
        self.assertNotIn("function calculateDelta", template)
        self.assertNotIn("JSON.stringify", template)
        build = (INPUT_ROOT / "website" / "build.sh").read_text()
        self.assertIn("'x86-64-linux' in platforms_with_data", build)
        self.assertNotIn("'default_platform': 'comparison'", build)


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
        self.assertEqual(
            series["ranges"]["30d"]["calendar_start"],
            (end - evolution.timedelta(days=30)).isoformat().replace("+00:00", "Z"),
        )
        self.assertEqual(series["ranges"]["30d"]["calendar_end"], end.isoformat().replace("+00:00", "Z"))
        self.assertIsNone(series["ranges"]["all"]["calendar_start"])
        self.assertIsNone(series["ranges"]["all"]["calendar_end"])
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
        self.assertEqual(model["range_options"], ["30d", "90d", "1y", "all"])
        self.assertEqual(
            [segment["segment"] for segment in series["ranges"]["all"]["trend_segments"]],
            [0, 1, 2, 3],
        )
        self.assertTrue(all(
            {point["segment"] for point in segment["points"]} == {segment["segment"]}
            for segment in series["ranges"]["all"]["trend_segments"]
        ))
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
        self.assertEqual(series["ranges"]["1y"]["calendar_start"], "2027-02-28T12:00:00Z")
        self.assertEqual(series["ranges"]["1y"]["calendar_end"], "2028-02-29T12:00:00Z")

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

    def test_evolution_renderer_consumes_complete_precomputed_contract(self):
        template = (INPUT_ROOT / "website" / "templates" / "performance.html").read_text()
        for contract in (
            "workspace.range_options", "workspace.platform_options", "workspace.workload_families",
            "view.rendered_raw_points", "view.trend_points", "view.trend_segments",
            "rangeView.calendar_start", "rangeView.calendar_end",
            "point.variation", "point.boundary", "boundary.skipped_commits",
            "point.annotation_ids", "annotationBadge(event.category)", "event.link",
            "range=${encodeURIComponent(activeRange)}", "annotation=${encodeURIComponent(event.id)}",
        ):
            self.assertIn(contract, template)
        self.assertIn("Date.parse(point.timestamp)", template)
        self.assertIn("evolution-raw-point", template)
        self.assertIn("evolution-trend", template)
        self.assertIn("evolution-boundary", template)
        self.assertIn("Raw evolution measurements with comparison boundaries and coverage", template)
        self.assertEqual(template.count('id="evolution-workspace"'), 1)
        self.assertNotIn('id="normalized-chart"', template)
        self.assertNotIn("renderNormalized", template)
        self.assertNotIn("comparability: {status: 'comparable'}", template)
        self.assertIn("view.trend_segments.forEach", template)
        self.assertIn("How to read this chart", template)
        self.assertIn("new measurement segment", template)
        self.assertIn("The marker is not a compiler", template)
        self.assertIn("line stops because a change across it", template)
        self.assertIn("normalized compile speed, not milliseconds", template)
        self.assertIn("first measurement in each connected machine segment", template)
        self.assertIn("105 means compile speed is 5% above", template)
        self.assertIn("Do not rank machines", template)
        self.assertIn("measurementBoundaryReason(point.comparability.reason)", template)
        self.assertNotIn("comparison boundary</div>", template)
        self.assertNotIn("function calculateEvolutionIndex", template)
        css = (INPUT_ROOT / "website" / "css" / "input.css").read_text()
        for category in (
            "capability", "optimization", "benchmark_corpus", "measurement_infrastructure",
            "known_regression", "repair",
        ):
            self.assertIn(f".category-{category}", css)


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
        self.assertEqual(result["passes"], {"lexer": {
            "mean_ms": 1.0,
            "invocations": 1,
            "root_invocations": 0,
            "leaf_invocations": 1,
        }})

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

    def test_rejects_structural_counter_drift_between_iterations(self):
        changed = json.loads(self.payload())
        changed["passes"][0].update(invocations=2, leaf_invocations=2)
        with self.assertRaisesRegex(ValueError, "invocation structure changed"):
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
