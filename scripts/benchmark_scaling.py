#!/usr/bin/env python3
"""Run and derive deterministic compiler scaling-family diagnostics."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from benchmark_collection import aggregate_iterations, parse_iteration_json
from benchmark_manifest import scaling_families
from benchmark_metrics import robust_summary
from scaling_workloads import generate


def parse_peak_memory(stderr: str, platform: str) -> int | None:
    for line in stderr.splitlines():
        if platform == "darwin" and "maximum resident set size" in line:
            try:
                return int(line.split()[0])
            except (IndexError, ValueError):
                pass
        if platform != "darwin" and "Maximum resident set size" in line:
            try:
                return int(line.rsplit(":", 1)[1].strip()) * 1024
            except (IndexError, ValueError):
                pass
    return None


def compile_once(
    rue_bin: Path, source: Path, output: Path, platform: str, timeout: float
) -> tuple[str, int | None]:
    time_args = ["-l"] if platform == "darwin" else ["-v"]
    process = subprocess.run(
        [
            "/usr/bin/time",
            *time_args,
            str(rue_bin),
            "--benchmark-json",
            str(source),
            "-o",
            str(output),
        ],
        cwd=source.parent,
        text=True,
        capture_output=True,
        check=False,
        timeout=timeout,
        env={**os.environ, "RUE_LOG": ""},
    )
    if process.returncode != 0:
        raise RuntimeError(f"scaling compile failed for {source}: {process.stderr.strip()}")
    payload = process.stdout.strip()
    parse_iteration_json(payload)
    return payload, parse_peak_memory(process.stderr, platform)


MIN_GROWTH_SAMPLES = 3

# Shared CI runners can make one collection round indeterminate through
# scheduler noise alone (an extreme sample range or growth uncertainty that
# crosses the budget). Those reasons are evidence problems, not proven
# regressions, so the runner collects additional rounds of samples for the
# implicated tiers instead of failing enforcement on the first noisy round.
# Each tier is bounded at MAX_SAMPLE_MULTIPLIER times the base iterations;
# evidence that is still indeterminate at the bound fails enforcement as
# before. `at_least_three_samples_required` is deliberately excluded: it
# reflects the caller's --iterations choice, not runner noise.
MAX_SAMPLE_MULTIPLIER = 3
RESAMPLE_REASONS = frozenset(
    {"extreme_sample_range", "uncertainty_crosses_budget", "variation_reaches_zero"}
)


def _metric_summary(samples: list[float | int]) -> dict:
    summary = robust_summary(samples)
    if summary is None:
        raise ValueError("scaling samples must be finite numeric values")
    return summary


def _growth_evidence(
    previous: dict, current: dict, size_ratio: float, budget: float
) -> dict:
    center = (current["center"] / previous["center"]) / size_ratio
    evidence = {
        "center": round(center, 4),
        "lower_bound": None,
        "upper_bound": None,
        "budget": budget,
        "classification": "indeterminate",
        "reason": "at_least_three_samples_required",
    }
    if min(previous["sample_count"], current["sample_count"]) < MIN_GROWTH_SAMPLES:
        return evidence
    if "extreme_range" in (
        previous["dispersion_classification"],
        current["dispersion_classification"],
    ):
        evidence["reason"] = "extreme_sample_range"
        return evidence
    previous_low = previous["center"] - previous["scaled_mad"]
    if previous_low <= 0:
        evidence["reason"] = "variation_reaches_zero"
        return evidence
    lower = (
        max(0.0, current["center"] - current["scaled_mad"])
        / (previous["center"] + previous["scaled_mad"])
        / size_ratio
    )
    upper = (
        (current["center"] + current["scaled_mad"])
        / previous_low
        / size_ratio
    )
    evidence.update(
        lower_bound=round(lower, 4),
        upper_bound=round(upper, 4),
        reason=None,
    )
    if lower > budget:
        evidence["classification"] = "budget_exceeded"
    elif upper <= budget:
        evidence["classification"] = "within_budget"
    else:
        evidence["classification"] = "indeterminate"
        evidence["reason"] = "uncertainty_crosses_budget"
    return evidence


def derive_family(family: dict, tiers: list[dict]) -> dict:
    """Canonically derive uncertainty-aware growth from raw tier evidence."""
    derived_tiers = []
    for tier in tiers:
        latency = _metric_summary(tier["samples_ms"])
        memory = _metric_summary(tier["peak_memory_samples_bytes"])
        derived_tiers.append(
            {
                "input_size": tier["input_size"],
                "samples_ms": tier["samples_ms"],
                "peak_memory_samples_bytes": tier["peak_memory_samples_bytes"],
                "latency_summary_ms": latency,
                "peak_memory_summary_bytes": memory,
                "median_ms": latency["center"],
                "peak_memory_bytes": int(memory["center"]),
                "passes": tier["passes"],
                "source_metrics": tier["source_metrics"],
            }
        )
    adjacent = []
    violations = []
    for previous, current in zip(derived_tiers, derived_tiers[1:]):
        size_ratio = current["input_size"] / previous["input_size"]
        latency = _growth_evidence(
            previous["latency_summary_ms"],
            current["latency_summary_ms"],
            size_ratio,
            family["latency_per_unit_growth_budget"],
        )
        memory = _growth_evidence(
            previous["peak_memory_summary_bytes"],
            current["peak_memory_summary_bytes"],
            size_ratio,
            family["memory_per_unit_growth_budget"],
        )
        focus = family["focus_pass"]
        before_focus = previous.get("passes", {}).get(focus, {}).get("mean_ms", 0)
        after_focus = current.get("passes", {}).get(focus, {}).get("mean_ms", 0)
        focus_ratio = after_focus / before_focus if before_focus > 0 else None
        classifications = {latency["classification"], memory["classification"]}
        if "budget_exceeded" in classifications:
            classification = "budget_exceeded"
        elif "indeterminate" in classifications:
            classification = "indeterminate"
        else:
            classification = "within_normalized_growth_budget"
        row = {
            "from_size": previous["input_size"],
            "to_size": current["input_size"],
            "size_ratio": round(size_ratio, 4),
            "latency_per_unit_growth": latency,
            "memory_per_unit_growth": memory,
            "focus_pass": focus,
            "focus_pass_ratio": round(focus_ratio, 4) if focus_ratio is not None else None,
            "classification": classification,
        }
        for metric, evidence in (("latency", latency), ("memory", memory)):
            if evidence["classification"] == "budget_exceeded":
                violations.append(
                    {
                        "metric": metric,
                        "from_size": previous["input_size"],
                        "to_size": current["input_size"],
                        "lower_bound": evidence["lower_bound"],
                        "budget": evidence["budget"],
                    }
                )
        adjacent.append(row)
    row_classes = {row["classification"] for row in adjacent}
    if violations:
        classification = "budget_exceeded"
    elif "indeterminate" in row_classes:
        classification = "indeterminate"
    else:
        classification = "within_normalized_growth_budget"
    return {
        "name": family["name"],
        "kind": "synthetic_scaling_probe",
        "subsystem": family["subsystem"],
        "workload_family": family["workload_family"],
        "input_axis": family["input_axis"],
        "focus_pass": family["focus_pass"],
        "interpretation": family["interpretation"],
        "non_representative": family["non_representative"],
        "budgets": {
            "latency_per_unit_growth": family["latency_per_unit_growth_budget"],
            "memory_per_unit_growth": family["memory_per_unit_growth_budget"],
            "policy": "adjacent tier metric growth divided by input-size growth",
            "per_tier_compile_timeout_seconds": family["timeout_seconds"],
        },
        "tiers": derived_tiers,
        "adjacent_growth": adjacent,
        "classification": classification,
        "violations": violations,
    }


def collect_round(state: dict, family: dict, rue_bin: Path, iterations: int, platform: str) -> None:
    """Append one round of compile samples to a tier's raw evidence."""
    for offset in range(iterations):
        iteration = len(state["payloads"]) + offset
        try:
            payload, peak = compile_once(
                rue_bin,
                state["source"],
                state["root"] / f"out-{iteration}",
                platform,
                family["timeout_seconds"],
            )
        except subprocess.TimeoutExpired as error:
            raise RuntimeError(
                f"scaling compile timed out after {family['timeout_seconds']}s: "
                f"{family['name']} size {state['input_size']}"
            ) from error
        state["payloads"].append(payload)
        if peak is not None and peak > 0:
            state["peak_memory_samples_bytes"].append(peak)


def tier_evidence(state: dict) -> dict:
    aggregate = aggregate_iterations(state["payloads"])
    return {
        "input_size": state["input_size"],
        "samples_ms": [
            float(parse_iteration_json(payload)["total_ms"])
            for payload in state["payloads"]
        ],
        "peak_memory_samples_bytes": state["peak_memory_samples_bytes"],
        "passes": aggregate["passes"],
        "source_metrics": aggregate["source_metrics"],
    }


def noisy_tier_sizes(derived: dict) -> set[int]:
    """Return tier sizes implicated in noise-driven indeterminate evidence."""
    sizes = set()
    for row in derived["adjacent_growth"]:
        for evidence in (row["latency_per_unit_growth"], row["memory_per_unit_growth"]):
            if (
                evidence["classification"] == "indeterminate"
                and evidence["reason"] in RESAMPLE_REASONS
            ):
                sizes.update((row["from_size"], row["to_size"]))
    return sizes


def run_family(family: dict, rue_bin: Path, iterations: int, platform: str, work: Path) -> dict:
    states = []
    for size in family["sizes"]:
        tier_root = work / family["name"] / str(size)
        states.append(
            {
                "input_size": size,
                "root": tier_root,
                "source": generate(family["generator"], tier_root, size),
                "payloads": [],
                "peak_memory_samples_bytes": [],
            }
        )
    for state in states:
        collect_round(state, family, rue_bin, iterations, platform)
    derived = derive_family(family, [tier_evidence(state) for state in states])
    max_samples = iterations * MAX_SAMPLE_MULTIPLIER
    while True:
        eligible = [
            state
            for state in states
            if state["input_size"] in noisy_tier_sizes(derived)
            and len(state["payloads"]) + iterations <= max_samples
        ]
        if not eligible:
            return derived
        for state in eligible:
            collect_round(state, family, rue_bin, iterations, platform)
        derived = derive_family(family, [tier_evidence(state) for state in states])


def budget_status(families: list[dict]) -> str:
    if any(family["violations"] for family in families):
        return "failed"
    if any(family["classification"] == "indeterminate" for family in families):
        return "indeterminate"
    return "passed"


def run_scaling(manifest: Path, rue_bin: Path, iterations: int, platform: str) -> dict:
    with tempfile.TemporaryDirectory(prefix="rue-scaling-") as temp:
        families = [
            run_family(family, rue_bin, iterations, platform, Path(temp))
            for family in scaling_families(manifest)
        ]
    return {
        "schema_version": 1,
        "measurement_family": "compiler_scaling_diagnostics",
        "scenario_family": "synthetic_phase_probes",
        "representative": False,
        "iterations": iterations,
        "families": families,
        "budget_status": budget_status(families),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--rue-bin", type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=5)
    parser.add_argument(
        "--platform",
        choices=["darwin", "linux"],
        default="darwin" if sys.platform == "darwin" else "linux",
    )
    parser.add_argument("--enforce-budgets", action="store_true")
    args = parser.parse_args()
    if args.iterations <= 0:
        parser.error("--iterations must be positive")
    try:
        result = run_scaling(args.manifest, args.rue_bin, args.iterations, args.platform)
    except (OSError, ValueError, RuntimeError) as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    if args.enforce_budgets and result["budget_status"] == "failed":
        for family in result["families"]:
            for violation in family["violations"]:
                print(
                    f"Budget violation {family['name']}: "
                    f"{violation['metric']} {violation['from_size']}->{violation['to_size']} "
                    f"lower bound {violation['lower_bound']} > {violation['budget']}",
                    file=sys.stderr,
                )
        return 2
    if args.enforce_budgets and result["budget_status"] == "indeterminate":
        for family in result["families"]:
            if family["classification"] == "indeterminate":
                reasons = sorted(
                    {
                        evidence["reason"]
                        for row in family["adjacent_growth"]
                        for evidence in (
                            row["latency_per_unit_growth"],
                            row["memory_per_unit_growth"],
                        )
                        if evidence["classification"] == "indeterminate"
                    }
                )
                print(
                    f"Insufficient scaling evidence {family['name']}: "
                    + ", ".join(reasons),
                    file=sys.stderr,
                )
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
