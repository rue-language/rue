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
             durable_source=0, queries=0, shells=0, projections=0,
             semantics=0, endpoints=0, cold_preparations=0,
             declarations_inspected=0, modules_registered=0,
             rir_indexes=0, rir_instructions=0, durable_inspected=0,
             durable_copied=0, reused=1):
        def counters(computed, reused_count):
            return {
                "computed": computed,
                "invalidated": computed,
                "required": computed,
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
            "cold_body_preparations": counters(cold_preparations, reused),
            "declarations_inspected": counters(declarations_inspected, reused),
            "modules_registered": counters(modules_registered, reused),
            "rir_indexes_constructed": counters(rir_indexes, reused),
            "rir_instructions_visited": counters(rir_instructions, reused),
            "durable_source_records_inspected": counters(durable_inspected, reused),
            "durable_source_records_copied": counters(durable_copied, reused),
            "shells_prepared": counters(shells, reused),
            "projections_performed": counters(projections, reused),
            "semantics_installed": counters(semantics, reused),
            "endpoints_installed": counters(endpoints, reused),
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
            "cold_semantic_work": {
                "schema_version": 1,
                "bodies_attempted": 1,
                "bodies_succeeded": 1,
                "bodies_failed": 0,
                "body_analyses_computed": 1,
                "body_analyses_reused": 0,
                "body_analyses_invalidated": 0,
                "per_body_declaration_context": {
                    "cold_body_preparations": 1,
                    "declarations_inspected": 1,
                    "modules_registered": 1,
                    "rir_indexes_constructed": 1,
                    "rir_instructions_visited": 1,
                    "shells_prepared": 1,
                    "projections_performed": 1,
                    "semantics_installed": 1,
                    "endpoints_installed": 1,
                    "durable_source_records_inspected": 1,
                    "durable_source_records_copied": 1,
                },
                "durable_bodies": {"candidate_comparisons": 0, "reused_bodies": 0},
                "cfg": {"builds_attempted": 1, "builds_succeeded": 1, "builds_failed": 0},
            },
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

    def test_empty_cold_semantic_work_is_not_exact_evidence(self):
        row = self.row("warm_leaf_body")
        row["differential_parity"]["cold_semantic_work"] = {}
        self.assertEqual(value_audit.parity_check(row)["status"], "unsupported")

    def test_partial_or_malformed_cold_semantic_work_fails_closed(self):
        for mutation in ("bodies_attempted", "per_body_declaration_context"):
            row = self.row("warm_leaf_body")
            if mutation == "bodies_attempted":
                del row["differential_parity"]["cold_semantic_work"][mutation]
            else:
                row["differential_parity"]["cold_semantic_work"][mutation] = []
            self.assertEqual(value_audit.parity_check(row)["status"], "unsupported")

    def test_ordered_type_universe_is_valid_exact_parity_evidence(self):
        self.assertEqual(value_audit.parity_check(self.row("warm_leaf_body"))["status"], "pass")

    def test_leaf_edit_with_full_program_recompute_fails(self):
        row = self.row("warm_leaf_body", body=128, cfg=127, modules=128)
        result = value_audit.locality_check(
            "warm_leaf_body", row, self.locality_policy("warm_leaf_body")
        )
        self.assertEqual(result["status"], "fail")

    def test_omitted_counter_universe_traversal_fails(self):
        row = self.row("warm_leaf_body", shells=129)
        result = value_audit.locality_check(
            "warm_leaf_body", row, self.locality_policy("warm_leaf_body")
        )
        self.assertEqual(result["status"], "fail")

    def test_each_newly_carried_counter_is_bounded(self):
        counters = (
            "cold_body_preparations",
            "declarations_inspected",
            "modules_registered",
            "rir_indexes_constructed",
            "rir_instructions_visited",
            "durable_source_records_inspected",
            "durable_source_records_copied",
        )
        for counter in counters:
            row = self.row("warm_leaf_body")
            maximum = self.locality_policy("warm_leaf_body")[f"max_{counter}_computed"]
            row["required_vs_reused_work"][counter]["computed"] = maximum + 1
            result = value_audit.locality_check(
                "warm_leaf_body", row, self.locality_policy("warm_leaf_body")
            )
            self.assertEqual(result["status"], "fail", counter)

    def test_unavailable_dimensions_are_not_published_as_work(self):
        row = self.row("warm_leaf_body", body=1, cfg=1, queries=1)
        row["required_vs_reused_work"]["durable_source"]["reused"] = {
            "available": False,
            "reason": "durable-body reuse is not durable-source reuse",
        }
        row["required_vs_reused_work"]["durable_source"]["invalidated"] = {
            "available": False,
            "reason": "no durable-source invalidation counter",
        }
        self.assertEqual(
            value_audit.locality_check(
                "warm_leaf_body", row, self.locality_policy("warm_leaf_body")
            )["status"],
            "pass",
        )

    def test_historical_cached_work_cannot_claim_current_recomputation(self):
        row = self.row("warm_leaf_body", body=1, cfg=1)
        row["required_vs_reused_work"]["semantic_queries"]["computed"] = 0
        self.assertEqual(
            value_audit.locality_check(
                "warm_leaf_body", row, self.locality_policy("warm_leaf_body")
            )["status"],
            "unsupported",
        )

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

    def test_manifest_contract_mutations_are_rejected_before_measurement(self):
        original_loader = value_audit.tomllib.load
        for mutation in ("root", "warm_runner"):
            drifted = dict(self.manifest)
            drifted["workload"] = [dict(item) for item in self.manifest["workload"]]
            drifted["workload"][0][mutation] = "mutated"
            value_audit.tomllib.load = lambda _stream, drifted=drifted: drifted
            try:
                with self.assertRaises(ValueError):
                    value_audit.load_audit_manifest(ROOT)
            finally:
                value_audit.tomllib.load = original_loader

        drifted = dict(self.manifest)
        drifted["protocol"] = dict(self.manifest["protocol"])
        drifted["protocol"]["pair_order"] = "candidate,current,historical"
        value_audit.tomllib.load = lambda _stream, drifted=drifted: drifted
        try:
            with self.assertRaises(ValueError):
                value_audit.load_audit_manifest(ROOT)
        finally:
            value_audit.tomllib.load = original_loader

    def test_identical_binary_pairs_cannot_claim_value(self):
        def role(binary):
            return {
                "status": "pass",
                "protocol": "session_benchmark_json",
                "binary_sha256": binary,
                "wall": {"median": 100.0, "mad": 0.0},
                "rss": {"median": 100.0, "mad": 0.0},
            }
        result = value_audit.scenario_verdict(
            "warm_leaf_body",
            {
                "historical_baseline": role("historical"),
                "current_production": role("same"),
                "candidate": role("same"),
            },
            {"family": "synthetic", "supported_scenarios": ["warm_leaf_body"]},
            self.manifest["scenario_policy"]["warm_leaf_body"],
            self.thresholds,
        )
        pair = result["pairs"]["current_production_vs_candidate"]
        self.assertEqual(pair["status"], "unsupported")
        self.assertTrue(pair["same_binary"])

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

    def regressed_workloads(self):
        return [
            workload for workload in value_audit.WORKLOADS
            if workload["family"] == "regressed_example"
        ]

    def cold_role(self, median_ms):
        return {
            "status": "pass",
            "protocol": "black_box_compile",
            "wall": {"median": median_ms, "mad": 0.0},
            "rss": {"median": 100.0, "mad": 0.0},
        }

    def test_every_regressed_example_is_a_cold_only_workload_with_a_budget(self):
        names = {workload["name"] for workload in self.regressed_workloads()}
        self.assertEqual(names, {"rill", "mosaic", "harbor", "lattice", "meridian"})
        manifest_workloads = {item["name"]: item for item in self.manifest["workload"]}
        for workload in self.regressed_workloads():
            entry = manifest_workloads[workload["name"]]
            self.assertEqual(entry["supported_scenarios"], ["cold"])
            self.assertGreater(workload["cold_wall_seconds"], 0)
            self.assertGreater(workload["cold_timeout_seconds"], workload["cold_wall_seconds"])
            self.assertIn(workload["family"], value_audit.DIRECTORY_ROOTED_FAMILIES)

    def test_absolute_cold_budget_fails_a_same_binary_run(self):
        """The gate is per-role, so it verdicts without a historical binary.

        Every pair is `unsupported` here because all three roles share one
        binary hash. The workload must still fail on its own observed wall time,
        otherwise the regressed-example rows would report nothing until a
        pre-cutover baseline binary exists.
        """
        rill = next(item for item in self.regressed_workloads() if item["name"] == "rill")
        roles = {
            role: {**self.cold_role(rill["cold_wall_seconds"] * 1000 * 5), "binary_sha256": "same"}
            for role in ("historical_baseline", "current_production", "candidate")
        }
        result = value_audit.scenario_verdict(
            "cold", roles,
            {**rill, "supported_scenarios": ["cold"]},
            self.manifest["scenario_policy"]["cold"], self.thresholds,
        )
        self.assertEqual(result["status"], "fail")
        self.assertTrue(all(
            pair["status"] == "unsupported"
            for name, pair in result["pairs"].items() if "_vs_" in name
        ))
        self.assertEqual(
            result["pairs"]["current_production"]["budget_seconds"],
            rill["cold_wall_seconds"],
        )

    def test_cold_budget_under_gate_does_not_fail(self):
        rill = next(item for item in self.regressed_workloads() if item["name"] == "rill")
        roles = {
            role: {**self.cold_role(rill["cold_wall_seconds"] * 1000 * 0.5), "binary_sha256": "same"}
            for role in ("historical_baseline", "current_production", "candidate")
        }
        result = value_audit.scenario_verdict(
            "cold", roles,
            {**rill, "supported_scenarios": ["cold"]},
            self.manifest["scenario_policy"]["cold"], self.thresholds,
        )
        self.assertNotEqual(result["status"], "fail")

    def cold_roles(self, median_ms, *, distinct_baseline):
        """Three cold role rows, optionally with a genuinely distinct baseline."""
        rows = {}
        for role in ("historical_baseline", "current_production", "candidate"):
            rows[role] = {
                **self.cold_role(median_ms),
                "binary_sha256": (
                    "historical" if distinct_baseline and role == "historical_baseline" else "same"
                ),
            }
        return rows

    def test_over_budget_is_advisory_when_a_real_baseline_is_present(self):
        """With a baseline, the host-independent pair comparison decides.

        The absolute budget is calibrated on a different machine, so once a
        distinct historical binary makes the pair comparison available, an
        over-budget row must not be able to fail the scenario on its own.
        """
        rill = next(item for item in self.regressed_workloads() if item["name"] == "rill")
        roles = self.cold_roles(rill["cold_wall_seconds"] * 1000 * 5, distinct_baseline=True)
        result = value_audit.scenario_verdict(
            "cold", roles,
            {**rill, "supported_scenarios": ["cold"]},
            self.manifest["scenario_policy"]["cold"], self.thresholds,
            True,
        )
        self.assertNotEqual(result["status"], "fail")
        advisory = [
            pair for name, pair in result["pairs"].items()
            if "_vs_" not in name and pair.get("status") == "advisory"
        ]
        self.assertTrue(advisory, f"expected an advisory row: {result['pairs']}")
        self.assertEqual(advisory[0]["budget_seconds"], rill["cold_wall_seconds"])
        self.assertIn("advisory_because", advisory[0])

    def test_over_budget_still_fails_without_a_baseline(self):
        """No baseline means no pair evidence, so the budget stays a hard gate."""
        rill = next(item for item in self.regressed_workloads() if item["name"] == "rill")
        roles = self.cold_roles(rill["cold_wall_seconds"] * 1000 * 5, distinct_baseline=False)
        result = value_audit.scenario_verdict(
            "cold", roles,
            {**rill, "supported_scenarios": ["cold"]},
            self.manifest["scenario_policy"]["cold"], self.thresholds,
            False,
        )
        self.assertEqual(result["status"], "fail")

    def test_advisory_alone_never_manufactures_a_pass(self):
        """An advisory is an observation, not passing evidence.

        Every pair here is unsupported, so the scenario must not become a pass
        merely because the only other row present is an advisory.
        """
        rill = next(item for item in self.regressed_workloads() if item["name"] == "rill")
        roles = self.cold_roles(rill["cold_wall_seconds"] * 1000 * 5, distinct_baseline=True)
        for role in roles:
            roles[role]["status"] = "unsupported"
        result = value_audit.scenario_verdict(
            "cold", roles,
            {**rill, "supported_scenarios": ["cold"]},
            self.manifest["scenario_policy"]["cold"], self.thresholds,
            True,
        )
        self.assertNotEqual(result["status"], "pass")

    def test_a_real_pair_failure_still_fails_with_a_baseline(self):
        """Downgrading the absolute gate must not soften the pair verdict."""
        rill = next(item for item in self.regressed_workloads() if item["name"] == "rill")
        roles = self.cold_roles(rill["cold_wall_seconds"] * 1000 * 5, distinct_baseline=True)
        roles["current_production"]["status"] = "fail"
        result = value_audit.scenario_verdict(
            "cold", roles,
            {**rill, "supported_scenarios": ["cold"]},
            self.manifest["scenario_policy"]["cold"], self.thresholds,
            True,
        )
        self.assertEqual(result["status"], "fail")

    def reject_mutation(self, mutate):
        drifted = {key: value for key, value in self.manifest.items()}
        mutate(drifted)
        original_loader = value_audit.tomllib.load
        value_audit.tomllib.load = lambda _stream, drifted=drifted: drifted
        try:
            with self.assertRaises(ValueError):
                value_audit.load_audit_manifest(ROOT)
        finally:
            value_audit.tomllib.load = original_loader

    def test_caldera_budget_drift_from_thresholds_is_rejected(self):
        def mutate(drifted):
            drifted["workload"] = [dict(item) for item in self.manifest["workload"]]
            caldera = next(item for item in drifted["workload"] if item["name"] == "caldera")
            caldera["cold_wall_seconds"] = 299
        self.reject_mutation(mutate)

    def test_cold_gate_above_its_process_timeout_is_rejected(self):
        def mutate(drifted):
            drifted["workload"] = [dict(item) for item in self.manifest["workload"]]
            rill = next(item for item in drifted["workload"] if item["name"] == "rill")
            rill["cold_timeout_seconds"] = rill["cold_wall_seconds"]
        self.reject_mutation(mutate)

    def test_historical_reference_cannot_be_promoted_to_role_evidence(self):
        def mutate(drifted):
            drifted["historical_reference"] = dict(self.manifest["historical_reference"])
            drifted["historical_reference"]["usable_as_role_evidence"] = True
        self.reject_mutation(mutate)

        def mutate_protocol(drifted):
            drifted["historical_reference"] = dict(self.manifest["historical_reference"])
            drifted["historical_reference"]["produced_by_this_protocol"] = True
        self.reject_mutation(mutate_protocol)

    def test_cold_gate_detached_from_its_reference_is_rejected(self):
        def mutate(drifted):
            drifted["historical_reference"] = dict(self.manifest["historical_reference"])
            drifted["historical_reference"]["cold_gate_multiplier"] = 8.0
        self.reject_mutation(mutate)

    def test_reference_that_does_not_separate_regression_from_gate_is_rejected(self):
        def mutate(drifted):
            reference = dict(self.manifest["historical_reference"])
            programs = {name: dict(entry) for name, entry in reference["programs"].items()}
            programs["rill"]["post_cutover_seconds"] = programs["rill"]["pre_cutover_seconds"] * 1.1
            reference["programs"] = programs
            drifted["historical_reference"] = reference
        self.reject_mutation(mutate)

    def test_leaf_bound_scales_with_the_declared_reverse_closure(self):
        """A leaf edit costs its reverse closure, not one unit.

        Three computed bodies must fail at a closure of one (the old shape) and
        pass at the representative fixture's declared closure of three, so the
        gate does not fail an implementation that is invalidating correctly.
        """
        row = self.row("warm_leaf_body", body=3, cfg=3, queries=1)
        policy = self.locality_policy("warm_leaf_body")
        self.assertEqual(
            value_audit.locality_check("warm_leaf_body", row, policy, 1)["status"],
            "fail",
        )
        self.assertEqual(
            value_audit.locality_check("warm_leaf_body", row, policy, 3)["status"],
            "pass",
        )

    def test_scaling_never_loosens_a_zero_bound(self):
        row = self.row("warm_unrelated_declaration", body=1, cfg=1)
        self.assertEqual(
            value_audit.locality_check(
                "warm_unrelated_declaration", row,
                self.locality_policy("warm_unrelated_declaration"), 8,
            )["status"],
            "fail",
        )

    def test_unit_scale_applies_only_to_the_leaf_scenario(self):
        for workload in value_audit.WORKLOADS:
            scale = value_audit.warm_unit_scale(workload, "warm_signature_fanout")
            self.assertEqual(scale, 1)
        synthetic = next(w for w in value_audit.WORKLOADS if w["name"] == "synthetic")
        self.assertEqual(value_audit.warm_unit_scale(synthetic, "warm_leaf_body"), 2)
        representative = next(
            w for w in value_audit.WORKLOADS if w["name"] == "representative_multi_module"
        )
        self.assertEqual(value_audit.warm_unit_scale(representative, "warm_leaf_body"), 3)

    def test_a_nonsense_unit_scale_is_unsupported_not_a_pass(self):
        row = self.row("warm_leaf_body", body=1, cfg=1)
        policy = self.locality_policy("warm_leaf_body")
        for scale in (0, -1, True):
            self.assertEqual(
                value_audit.locality_check("warm_leaf_body", row, policy, scale)["status"],
                "unsupported",
            )

    def test_leaf_closure_must_accompany_leaf_support(self):
        def mutate(drifted):
            drifted["workload"] = [dict(item) for item in self.manifest["workload"]]
            synthetic = next(item for item in drifted["workload"] if item["name"] == "synthetic")
            del synthetic["warm_leaf_body_reverse_closure"]
        self.reject_mutation(mutate)

    def test_reference_programs_must_match_the_regressed_workloads(self):
        def mutate(drifted):
            reference = dict(self.manifest["historical_reference"])
            programs = {name: dict(entry) for name, entry in reference["programs"].items()}
            del programs["meridian"]
            reference["programs"] = programs
            drifted["historical_reference"] = reference
        self.reject_mutation(mutate)


if __name__ == "__main__":
    unittest.main()
