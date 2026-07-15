"""Shared schema and corpus validation for benchmark result publication."""

from collections import Counter
import math
from pathlib import Path

from benchmark_manifest import (
    load_manifest,
    scenario_families,
    scaling_families,
    validate_readme_inventory,
    validate_static_dimensions,
)
from benchmark_scaling import budget_status, derive_family


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = REPO_ROOT / "benchmarks" / "manifest.toml"


def _is_finite_non_negative_number(value: object) -> bool:
    return (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and value >= 0
        and (isinstance(value, int) or math.isfinite(value))
    )


def load_manifest_names(path: Path) -> list[str]:
    """Load and validate the benchmark names declared by the manifest."""
    return [entry["name"] for entry in load_manifest(path)["benchmark"]]


def validate_scaling_results(data: object, manifest_path: Path) -> list[str]:
    """Re-derive every scaling claim from finite raw tier evidence."""
    scaling = data.get("scaling") if isinstance(data, dict) else None
    if not isinstance(scaling, dict):
        return ["benchmark result is missing scaling diagnostics"]
    errors = []
    expected_identity = {
        "schema_version": 1,
        "measurement_family": "compiler_scaling_diagnostics",
        "scenario_family": "synthetic_phase_probes",
        "representative": False,
    }
    for field, expected_value in expected_identity.items():
        if scaling.get(field) != expected_value:
            errors.append(f"scaling diagnostics {field} is malformed")
    if scaling.get("iterations") != data.get("iterations"):
        errors.append("scaling diagnostics iterations do not match root iterations")
    actual = scaling.get("families")
    if not isinstance(actual, list):
        return errors + ["scaling diagnostics families must be an array"]
    if any(not isinstance(family, dict) for family in actual):
        return errors + ["scaling diagnostics families must contain objects"]
    declared_families = scaling_families(manifest_path)
    expected_names = [family["name"] for family in declared_families]
    if [family.get("name") for family in actual] != expected_names:
        errors.append("scaling diagnostic family composition does not match manifest")
        return errors
    canonical_families = []
    iterations = data.get("iterations")
    for declared, family in zip(declared_families, actual):
        name = declared["name"]
        tiers = family.get("tiers", [])
        if not isinstance(tiers, list):
            errors.append(f"scaling family '{name}' tiers must be an array")
            continue
        sizes = [tier.get("input_size") for tier in tiers if isinstance(tier, dict)]
        if sizes != declared["sizes"]:
            errors.append(f"scaling family '{name}' tiers do not match manifest sizes")
        raw_tiers = []
        for tier in tiers:
            tier_error_count = len(errors)
            if not isinstance(tier, dict):
                errors.append(f"scaling family '{name}' tiers must contain objects")
                continue
            latency = tier.get("samples_ms")
            memory = tier.get("peak_memory_samples_bytes")
            for label, samples in (("latency", latency), ("peak memory", memory)):
                if (
                    not isinstance(samples, list)
                    or len(samples) != iterations
                    or any(
                        not _is_finite_non_negative_number(value) or value <= 0
                        for value in samples
                    )
                ):
                    errors.append(
                        f"scaling family '{name}' tier has invalid {label} samples"
                    )
            source = tier.get("source_metrics")
            if not isinstance(source, dict) or any(
                not isinstance(source.get(field), int)
                or isinstance(source.get(field), bool)
                or source[field] < 0
                for field in ("bytes", "files", "lines", "tokens")
            ):
                errors.append(f"scaling family '{name}' tier has invalid source counters")
            elif source["files"] == 0:
                errors.append(f"scaling family '{name}' tier has no source files")
            passes = tier.get("passes")
            if not isinstance(passes, dict) or not passes:
                errors.append(f"scaling family '{name}' tier has invalid pass counters")
            else:
                for pass_name, counters in passes.items():
                    if not isinstance(pass_name, str) or not isinstance(counters, dict):
                        errors.append(f"scaling family '{name}' tier has invalid pass counters")
                        continue
                    if not _is_finite_non_negative_number(counters.get("mean_ms")):
                        errors.append(f"scaling family '{name}' tier has invalid pass timing")
                    for field in ("invocations", "root_invocations", "leaf_invocations"):
                        value = counters.get(field)
                        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                            errors.append(
                                f"scaling family '{name}' tier has invalid structural counter"
                            )
                    invocations = counters.get("invocations")
                    roots = counters.get("root_invocations")
                    leaves = counters.get("leaf_invocations")
                    if (
                        all(
                            isinstance(value, int) and not isinstance(value, bool)
                            for value in (invocations, roots, leaves)
                        )
                        and (invocations == 0 or roots > invocations or leaves > invocations)
                    ):
                        errors.append(
                            f"scaling family '{name}' tier has inconsistent structural counters"
                        )
                if declared["focus_pass"] not in passes:
                    errors.append(f"scaling family '{name}' tier lacks focus pass")
            if len(errors) == tier_error_count:
                raw_tiers.append(
                    {
                        "input_size": tier["input_size"],
                        "samples_ms": latency,
                        "peak_memory_samples_bytes": memory,
                        "passes": passes,
                        "source_metrics": source,
                    }
                )
        if len(raw_tiers) == len(tiers):
            try:
                canonical = derive_family(declared, raw_tiers)
            except (KeyError, TypeError, ValueError, ZeroDivisionError) as error:
                errors.append(f"scaling family '{name}' evidence cannot be derived: {error}")
            else:
                canonical_families.append(canonical)
                if family != canonical:
                    errors.append(
                        f"scaling family '{name}' derived claims do not match raw evidence"
                    )
    if len(canonical_families) == len(declared_families):
        expected_status = budget_status(canonical_families)
        if scaling.get("budget_status") != expected_status:
            errors.append("scaling diagnostics root budget status is not canonical")
        if expected_status == "failed":
            errors.append("scaling diagnostics contain a proven complexity budget violation")
        elif expected_status == "indeterminate":
            errors.append("scaling diagnostics have insufficient evidence for publication")
    return errors


def validate_manifest_results(data: object, manifest_path: Path) -> list[str]:
    errors = (
        validate_static_dimensions(manifest_path)
        + validate_readme_inventory(manifest_path)
        + validate_results(data, load_manifest_names(manifest_path))
        + validate_scaling_results(data, manifest_path)
        + validate_scenario_results(data, manifest_path)
    )
    expected_suite = {
        "kind": "synthetic_phase_probe",
        "representative": False,
        "aggregate_interpretation": "synthetic compiler phase diagnostics only",
    }
    if not isinstance(data, dict) or data.get("probe_suite") != expected_suite:
        errors.append("benchmark result lacks non-representative phase-probe suite semantics")
        return errors
    declared = {probe["name"]: probe for probe in load_manifest(manifest_path)["benchmark"]}
    for bench in data.get("benchmarks", []):
        if not isinstance(bench, dict) or bench.get("name") not in declared:
            continue
        probe = declared[bench["name"]]
        for field in (
            "kind",
            "subsystem",
            "workload_family",
            "interpretation",
            "non_representative",
            "dimensions",
        ):
            if bench.get(field) != probe[field]:
                errors.append(f"benchmark '{bench['name']}' {field} does not match manifest")
    return errors


def validate_scenario_results(data: object, manifest_path: Path) -> list[str]:
    """Validate representative samples, parity proof, and exact structural work."""
    scenarios = data.get("scenarios") if isinstance(data, dict) else None
    if not isinstance(scenarios, dict):
        return ["benchmark result is missing representative scenarios"]
    errors = []
    expected_identity = {
        "schema_version": 1,
        "measurement_family": "compiler_build_and_query_performance",
        "representative": True,
        "generated_program_runtime": False,
        "iterations": data.get("iterations"),
    }
    for field, expected in expected_identity.items():
        if scenarios.get(field) != expected:
            errors.append(f"representative scenarios {field} is malformed")
    cross_family = scenarios.get("cross_family_emitted_output")
    if (
        not isinstance(cross_family, dict)
        or cross_family.get("cold_batch_vs_fresh_and_reused_session") != "byte_exact"
        or cross_family.get("representation")
        != "raw_compiler_executable_before_platform_postprocessing"
        or not isinstance(cross_family.get("sha256"), str)
        or len(cross_family["sha256"]) != 64
        or any(character not in "0123456789abcdef" for character in cross_family["sha256"])
        or not isinstance(cross_family.get("size_bytes"), int)
        or cross_family["size_bytes"] <= 0
    ):
        errors.append("representative scenarios lack cross-family emitted-output parity")
    actual_families = scenarios.get("families")
    declared_families = scenario_families(manifest_path)
    if not isinstance(actual_families, list) or [family.get("name") for family in actual_families if isinstance(family, dict)] != [family["name"] for family in declared_families]:
        return errors + ["representative scenario family composition does not match manifest"]
    iterations = data.get("iterations")
    for declared, family in zip(declared_families, actual_families):
        for field in ("interpretation", "not_runtime"):
            if family.get(field) != declared[field]:
                errors.append(f"scenario family '{declared['name']}' {field} does not match manifest")
        rows = family.get("scenarios")
        expected_names = [declared["name"]] if declared["runner"] == "cold_batch_root_compiler" else [row["name"] for row in declared["scenarios"]]
        if not isinstance(rows, list) or [row.get("name") for row in rows if isinstance(row, dict)] != expected_names:
            errors.append(f"scenario family '{declared['name']}' composition does not match manifest")
            continue
        for row in rows:
            samples = row.get("samples_ms")
            samples_valid = isinstance(samples, list) and len(samples) == iterations and all(_is_finite_non_negative_number(value) and value > 0 for value in samples)
            if not samples_valid:
                errors.append(f"representative scenario '{row.get('name')}' has invalid samples")
            if not _is_finite_non_negative_number(row.get("mean_ms")) or row.get("mean_ms") <= 0:
                errors.append(f"representative scenario '{row.get('name')}' has invalid mean")
            elif samples_valid and row["mean_ms"] != round(sum(samples) / len(samples), 3):
                errors.append(f"representative scenario '{row.get('name')}' mean does not match samples")
        if declared["runner"] == "cold_batch_root_compiler":
            emitted = rows[0].get("emitted_output")
            if not isinstance(emitted, dict) or emitted.get("parity") != "byte_exact" or emitted.get("representation") != "raw_compiler_executable_before_platform_postprocessing" or not isinstance(emitted.get("sha256"), str) or len(emitted["sha256"]) != 64 or any(character not in "0123456789abcdef" for character in emitted.get("sha256", "")) or not isinstance(emitted.get("size_bytes"), int) or emitted["size_bytes"] <= 0:
                errors.append("cold representative scenario lacks exact emitted-output parity")
            elif not isinstance(cross_family, dict) or (
                emitted["sha256"] != cross_family.get("sha256")
                or emitted["size_bytes"] != cross_family.get("size_bytes")
            ):
                errors.append(
                    "cold representative emitted output does not match cross-family parity"
                )
            continue
        declared_rows = {row["name"]: row for row in declared["scenarios"]}
        for row in rows:
            expected = declared_rows[row["name"]]
            work = row.get("required_vs_reused_work")
            expected_work = {
                artifact: {"required": expected[artifact][0], "reused": expected[artifact][1]}
                for artifact in ("modules", "semantic_bodies", "cfgs", "semantic_queries")
            }
            if work != expected_work:
                errors.append(f"representative scenario '{row['name']}' structural work does not match manifest")
            if row["name"] == "diagnostic_error_edit":
                parity = row.get("diagnostic_parity")
                if not isinstance(parity, dict) or parity.get("fresh_session") != "exact" or parity.get("last_good_preserved") is not True:
                    errors.append("diagnostic representative scenario lacks exact fresh parity")
            else:
                parity = row.get("differential_parity")
                exact_fields = {
                    "dependency_records": "exact",
                    "diagnostics": "exact",
                    "durable_ordinary_body_manifest_artifacts": "exact",
                    "durable_specialized_body_payloads": "exact",
                    "emitted_output": "byte_exact",
                    "public_semantic_cfg_artifacts": "exact",
                    "public_type_universe": "exact_ordered_entries",
                    "stable_identities": "exact",
                    "warnings": "exact",
                }
                if not isinstance(parity, dict) or any(parity.get(field) != value for field, value in exact_fields.items()):
                    errors.append(f"representative scenario '{row['name']}' lacks differential parity")
                elif row["name"] == "no_change_query" and (
                    not isinstance(cross_family, dict) or
                    parity.get("emitted_output_sha256") != cross_family.get("sha256")
                    or parity.get("emitted_output_size_bytes")
                    != cross_family.get("size_bytes")
                ):
                    errors.append(f"representative scenario '{row['name']}' emitted output does not match the batch driver")
        recovery = rows[-1] if rows else {}
        if recovery.get("recovered_from_diagnostic_error") is not True:
            errors.append("diagnostic recovery scenario lacks recovery proof")
    return errors


def validate_results(data: object, expected_names: list[str]) -> list[str]:
    """Return every schema or corpus-membership error in a benchmark result."""
    if not isinstance(data, dict):
        return ["benchmark result must be a JSON object"]

    iterations = data.get("iterations")
    if (
        not isinstance(iterations, int)
        or isinstance(iterations, bool)
        or iterations <= 0
    ):
        errors = ["iterations must be a positive integer"]
    else:
        errors = []

    benchmarks = data.get("benchmarks")
    if not isinstance(benchmarks, list):
        return errors + ["benchmarks must be an array"]
    if not benchmarks:
        return errors + ["no benchmark results collected; all benchmarks failed"]

    result_names = []
    for index, bench in enumerate(benchmarks):
        if not isinstance(bench, dict):
            errors.append(f"benchmark #{index + 1} must be an object")
            continue

        name = bench.get("name")
        if not isinstance(name, str) or not name:
            errors.append(f"benchmark #{index + 1} has no valid name")
        else:
            result_names.append(name)

        display_name = name if isinstance(name, str) and name else f"#{index + 1}"
        for field in ("mean_ms", "std_ms"):
            value = bench.get(field)
            if not _is_finite_non_negative_number(value):
                errors.append(
                    f"benchmark '{display_name}' has no finite non-negative {field}"
                )

        bench_iterations = bench.get("iterations")
        if (
            not isinstance(bench_iterations, int)
            or isinstance(bench_iterations, bool)
            or bench_iterations <= 0
        ):
            errors.append(
                f"benchmark '{display_name}' iterations must be a positive integer"
            )
        elif isinstance(iterations, int) and not isinstance(iterations, bool):
            if bench_iterations != iterations:
                errors.append(
                    f"benchmark '{display_name}' iterations ({bench_iterations}) "
                    f"does not match root iterations ({iterations})"
                )

        samples = bench.get("samples_ms")
        if samples is not None:
            if not isinstance(samples, list) or len(samples) != bench_iterations:
                errors.append(
                    f"benchmark '{display_name}' samples_ms must contain exactly "
                    f"{bench_iterations} samples"
                )
            elif any(not _is_finite_non_negative_number(sample) for sample in samples):
                errors.append(
                    f"benchmark '{display_name}' samples_ms must contain finite non-negative numbers"
                )

        passes = bench.get("passes")
        if not isinstance(passes, dict):
            errors.append(f"benchmark '{display_name}' passes must be an object")
        else:
            for pass_name, pass_data in passes.items():
                if not isinstance(pass_name, str) or not pass_name:
                    errors.append(
                        f"benchmark '{display_name}' has an invalid pass name"
                    )
                    continue
                pass_mean = (
                    pass_data.get("mean_ms") if isinstance(pass_data, dict) else None
                )
                if not _is_finite_non_negative_number(pass_mean):
                    errors.append(
                        f"benchmark '{display_name}' pass '{pass_name}' has no "
                        "finite non-negative mean_ms"
                    )
                if bench.get("timing_schema_version", 1) >= 2 and isinstance(
                    pass_data, dict
                ):
                    counters = []
                    for field in (
                        "invocations",
                        "root_invocations",
                        "leaf_invocations",
                    ):
                        value = pass_data.get(field)
                        counters.append(value)
                        if (
                            not isinstance(value, int)
                            or isinstance(value, bool)
                            or value < 0
                        ):
                            errors.append(
                                f"benchmark '{display_name}' pass '{pass_name}' "
                                f"has invalid {field}"
                            )
                    if all(
                        isinstance(value, int) and not isinstance(value, bool)
                        for value in counters
                    ):
                        invocations, roots, leaves = counters
                        if invocations <= 0 or roots > invocations or leaves > invocations:
                            errors.append(
                                f"benchmark '{display_name}' pass '{pass_name}' "
                                "has inconsistent structural counters"
                            )

    duplicates = sorted(
        name for name, count in Counter(result_names).items() if count > 1
    )
    if duplicates:
        errors.append("duplicate benchmark result name(s): " + ", ".join(duplicates))

    expected = set(expected_names)
    actual = set(result_names)
    missing = sorted(expected - actual)
    unknown = sorted(actual - expected)
    if missing:
        errors.append("missing benchmark result(s): " + ", ".join(missing))
    if unknown:
        errors.append("unknown benchmark result name(s): " + ", ".join(unknown))

    return errors
