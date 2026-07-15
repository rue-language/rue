"""Canonical benchmark comparison and performance-index semantics.

The policy is intentionally independent of chart rendering.  A run is a
``compiler/cold_compilation`` measurement unless it carries an explicit
identity; this default describes the historical ``bench.sh`` data.

Latency uses the median of each benchmark's raw samples.  Variation is the
scaled median absolute deviation (1.4826 * MAD), expressed relative to the
median.  A comparison combines current and reference variation by root-sum-
square.  A delta is stable inside that observed-variation band, improved below
it, and regressed above it (compilation latency is lower-is-better).  At least
three samples are required for each run.  The rolling baseline is the median
of per-run medians from the preceding three to seven comparable runs; its
variation combines within-run sample variation with variation between run
medians.  There is no fixed absolute or percentage threshold.

The latency performance index is the geometric mean of per-workload
``baseline / current`` ratios times 100, so 100 is the baseline and higher is
better.  Regime, scenario, or workload-composition changes explicitly rebase
the index instead of producing a misleading movement.  Latency, throughput,
memory, and binary size remain separate metric families.
"""

from __future__ import annotations

import math
import statistics

from benchmark_history import comparison_break, run_regime


POLICY_VERSION = 1
BASELINE_MIN_RUNS = 3
BASELINE_WINDOW = 7
MIN_SAMPLES = 3
DEFAULT_MEASUREMENT_FAMILY = "compiler"
DEFAULT_SCENARIO_FAMILY = "cold_compilation"


def measurement_identity(run: dict) -> dict[str, str]:
    """Return the semantic identity of a run, including historical defaults."""
    publication = run.get("publication", {})
    semantics = publication.get("metric_semantics", {}) if isinstance(publication, dict) else {}
    measurement = semantics.get("measurement_family", run.get("measurement_family"))
    scenario = semantics.get("scenario_family", run.get("scenario_family"))
    return {
        "measurement_family": measurement or DEFAULT_MEASUREMENT_FAMILY,
        "scenario_family": scenario or DEFAULT_SCENARIO_FAMILY,
    }


def require_same_identity(left: dict, right: dict) -> dict[str, str]:
    """Reject comparisons that mix measurement or execution scenarios."""
    left_identity = measurement_identity(left)
    right_identity = measurement_identity(right)
    if left_identity != right_identity:
        raise ValueError(
            "cannot mix benchmark measurement/scenario families: "
            f"{left_identity} != {right_identity}"
        )
    return left_identity


def workload_names(run: dict) -> tuple[str, ...]:
    names = [bench.get("name") for bench in run.get("benchmarks", []) if isinstance(bench, dict)]
    if any(not isinstance(name, str) or not name for name in names) or len(names) != len(set(names)):
        raise ValueError("benchmark workload names must be non-empty and unique")
    return tuple(sorted(names))


def _benches(run: dict) -> dict[str, dict]:
    return {
        bench["name"]: bench
        for bench in run.get("benchmarks", [])
        if isinstance(bench, dict) and isinstance(bench.get("name"), str)
    }


def _sample_summary(samples: object) -> dict | None:
    if not isinstance(samples, list) or len(samples) < MIN_SAMPLES:
        return None
    if any(
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
        or value <= 0
        for value in samples
    ):
        return None
    values = [float(value) for value in samples]
    center = statistics.median(values)
    mad = statistics.median(abs(value - center) for value in values)
    return {
        "center": center,
        "relative_variation_pct": 100.0 * 1.4826 * mad / center,
        "sample_count": len(values),
    }


def sample_summary(samples: object) -> dict | None:
    """Return the public robust center/uncertainty summary for raw samples."""
    return _sample_summary(samples)


def robust_summary(values: object) -> dict | None:
    """Return the canonical median and scaled-MAD summary for numeric values."""
    if not isinstance(values, list) or not values:
        return None
    if any(
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
        for value in values
    ):
        return None
    numeric = [float(value) for value in values]
    center = statistics.median(numeric)
    mad = statistics.median(abs(value - center) for value in numeric)
    return {
        "center": center,
        "scaled_mad": 1.4826 * mad,
        "minimum": min(numeric),
        "maximum": max(numeric),
        "sample_count": len(numeric),
    }


def _classification(delta_pct: float, variation_pct: float) -> str:
    if delta_pct > variation_pct:
        return "regressed"
    if delta_pct < -variation_pct:
        return "improved"
    return "stable"


def _headline(per_workload: dict[str, dict]) -> dict:
    if not per_workload or any(value["classification"] == "insufficient_data" for value in per_workload.values()):
        return {"classification": "insufficient_data"}
    ratios = [1.0 + value["delta_pct"] / 100.0 for value in per_workload.values()]
    delta = 100.0 * (math.exp(sum(math.log(value) for value in ratios) / len(ratios)) - 1.0)
    variation = math.sqrt(
        sum(value["variation_pct"] ** 2 for value in per_workload.values())
        / len(per_workload)
    )
    return {
        "delta_pct": delta,
        "variation_pct": variation,
        "classification": _classification(delta, variation),
        "direction": "lower_is_better",
    }


def compare_runs(current: dict, reference: dict) -> dict:
    """Compare two points after the caller has preserved intervening boundaries."""
    identity = require_same_identity(reference, current)
    if run_regime(reference) != run_regime(current) or comparison_break(reference, current):
        return {"status": "non_comparable", "reason": "regime_boundary", **identity}
    names = workload_names(current)
    if names != workload_names(reference):
        return {"status": "non_comparable", "reason": "workload_composition_changed", **identity}
    current_benches, reference_benches = _benches(current), _benches(reference)
    comparisons = {}
    for name in names:
        current_sample = _sample_summary(current_benches[name].get("samples_ms"))
        reference_sample = _sample_summary(reference_benches[name].get("samples_ms"))
        if current_sample is None or reference_sample is None:
            comparisons[name] = {"classification": "insufficient_data"}
            continue
        delta = 100.0 * (current_sample["center"] / reference_sample["center"] - 1.0)
        variation = math.hypot(
            current_sample["relative_variation_pct"],
            reference_sample["relative_variation_pct"],
        )
        comparisons[name] = {
            "delta_pct": delta,
            "variation_pct": variation,
            "classification": _classification(delta, variation),
            "current_median_ms": current_sample["center"],
            "reference_median_ms": reference_sample["center"],
        }
    return {
        "status": "comparable",
        "reference": "previous",
        **identity,
        "headline": _headline(comparisons),
        "workloads": comparisons,
    }


def compare_to_rolling_baseline(current: dict, preceding: list[dict]) -> dict:
    """Compare against up to seven preceding points in one contiguous segment."""
    identity = measurement_identity(current)
    names = workload_names(current)
    candidates = preceding[-BASELINE_WINDOW:]
    if len(candidates) < BASELINE_MIN_RUNS:
        return {"status": "insufficient_data", "reason": "baseline_requires_three_runs", **identity}
    chain = candidates + [current]
    for index, run in enumerate(candidates):
        require_same_identity(run, current)
        if run_regime(run) != run_regime(current) or workload_names(run) != names:
            return {"status": "non_comparable", "reason": "baseline_boundary", **identity}
        if comparison_break(chain[index], chain[index + 1]):
            return {"status": "non_comparable", "reason": "baseline_boundary", **identity}
    current_benches = _benches(current)
    prior_benches = [_benches(run) for run in candidates]
    comparisons = {}
    for name in names:
        current_sample = _sample_summary(current_benches[name].get("samples_ms"))
        prior_samples = [_sample_summary(benches[name].get("samples_ms")) for benches in prior_benches]
        if current_sample is None or any(sample is None for sample in prior_samples):
            comparisons[name] = {"classification": "insufficient_data"}
            continue
        samples = [sample for sample in prior_samples if sample is not None]
        centers = [sample["center"] for sample in samples]
        baseline = statistics.median(centers)
        between_mad = statistics.median(abs(center - baseline) for center in centers)
        between_variation = 100.0 * 1.4826 * between_mad / baseline
        within_variation = statistics.median(sample["relative_variation_pct"] for sample in samples)
        reference_variation = math.hypot(between_variation, within_variation)
        variation = math.hypot(current_sample["relative_variation_pct"], reference_variation)
        delta = 100.0 * (current_sample["center"] / baseline - 1.0)
        comparisons[name] = {
            "delta_pct": delta,
            "variation_pct": variation,
            "classification": _classification(delta, variation),
            "current_median_ms": current_sample["center"],
            "reference_median_ms": baseline,
            "baseline_run_count": len(samples),
        }
    return {
        "status": "comparable",
        "reference": "rolling_baseline",
        **identity,
        "headline": _headline(comparisons),
        "workloads": comparisons,
    }


def performance_index(current: dict, baseline: dict) -> dict:
    """Return a composition-safe latency index whose baseline is exactly 100."""
    identity = require_same_identity(baseline, current)
    if run_regime(current) != run_regime(baseline):
        return {"status": "rebase", "reason": "regime_changed", **identity}
    # A segment anchor is compared with itself so evolution consumers retain
    # an explicit 100-valued rebase point even when the run also records a
    # coverage gap.  Comparisons between distinct runs still honor that gap.
    if current is not baseline and comparison_break(baseline, current):
        return {"status": "rebase", "reason": "comparison_boundary", **identity}
    names = workload_names(current)
    if names != workload_names(baseline):
        return {"status": "rebase", "reason": "workload_composition_changed", **identity}
    current_benches, baseline_benches = _benches(current), _benches(baseline)
    ratios = []
    workloads = {}
    for name in names:
        current_sample = _sample_summary(current_benches[name].get("samples_ms"))
        baseline_sample = _sample_summary(baseline_benches[name].get("samples_ms"))
        if current_sample is None or baseline_sample is None:
            return {"status": "insufficient_data", **identity}
        ratio = baseline_sample["center"] / current_sample["center"]
        ratios.append(ratio)
        workloads[name] = {
            "value": 100.0 * ratio,
            "baseline": 100.0,
            "absolute_ms": current_sample["center"],
            "relative_variation_pct": current_sample["relative_variation_pct"],
        }
    if not ratios:
        return {"status": "insufficient_data", **identity}
    return {
        "status": "comparable",
        "value": 100.0 * math.exp(sum(math.log(value) for value in ratios) / len(ratios)),
        "baseline": 100.0,
        "direction": "higher_is_better",
        "workloads": workloads,
        **identity,
    }


def metric_families(run: dict) -> dict:
    """Expose absolute products without combining unlike metric families."""
    families = {"latency_ms": {}, "throughput": {}, "memory_bytes": {}, "binary_size_bytes": {}}
    for name, bench in _benches(run).items():
        sample = _sample_summary(bench.get("samples_ms"))
        if sample:
            families["latency_ms"][name] = sample["center"]
        source = bench.get("source_metrics", {})
        mean_ms = bench.get("mean_ms")
        if isinstance(source, dict) and isinstance(mean_ms, (int, float)) and mean_ms > 0:
            families["throughput"][name] = {
                key: source[key] * 1000.0 / mean_ms
                for key in ("lines", "tokens")
                if isinstance(source.get(key), (int, float))
            }
        if isinstance(bench.get("peak_memory_bytes"), (int, float)):
            families["memory_bytes"][name] = bench["peak_memory_bytes"]
        if isinstance(bench.get("binary_size_bytes"), (int, float)):
            families["binary_size_bytes"][name] = bench["binary_size_bytes"]
    return families


def comparison_explanation(current: dict, reference: dict) -> dict:
    """Explain one valid comparison by workload, pass, memory, and binary size."""
    comparison = compare_runs(current, reference)
    if comparison.get("status") != "comparable":
        return comparison
    current_benches, reference_benches = _benches(current), _benches(reference)
    workloads = []
    pass_totals: dict[str, dict[str, float]] = {}
    memory = []
    binary = []
    for name in workload_names(current):
        detail = comparison["workloads"][name]
        current_center = detail.get("current_median_ms")
        reference_center = detail.get("reference_median_ms")
        delta_ms = (
            current_center - reference_center
            if current_center is not None and reference_center is not None
            else None
        )
        workloads.append({"name": name, "delta_ms": delta_ms, **detail})
        current_passes = current_benches[name].get("passes", {})
        reference_passes = reference_benches[name].get("passes", {})
        if isinstance(current_passes, dict) and isinstance(reference_passes, dict):
            # A pass observed on only one side is missing evidence, not a zero.
            for pass_name in sorted(set(current_passes) & set(reference_passes)):
                if pass_name in {"compile", "parse"}:
                    continue
                current_pass = current_passes[pass_name]
                reference_pass = reference_passes[pass_name]
                current_value = current_pass.get("mean_ms") if isinstance(current_pass, dict) else None
                reference_value = reference_pass.get("mean_ms") if isinstance(reference_pass, dict) else None
                if isinstance(current_value, (int, float)) and isinstance(reference_value, (int, float)):
                    totals = pass_totals.setdefault(
                        pass_name,
                        {"before_ms": 0, "after_ms": 0, "observations": 0},
                    )
                    totals["before_ms"] += reference_value
                    totals["after_ms"] += current_value
                    totals["observations"] += 1
        for field, target in (("peak_memory_bytes", memory), ("binary_size_bytes", binary)):
            current_value = current_benches[name].get(field)
            reference_value = reference_benches[name].get(field)
            if isinstance(current_value, (int, float)) and isinstance(reference_value, (int, float)):
                target.append({
                    "name": name,
                    "before": reference_value,
                    "after": current_value,
                    "delta": current_value - reference_value,
                })
    passes = [
        {
            "name": name,
            "before_ms": values["before_ms"],
            "after_ms": values["after_ms"],
            "delta_ms": values["after_ms"] - values["before_ms"],
        }
        for name, values in sorted(pass_totals.items())
        if values["observations"] == len(workloads)
    ]
    comparable_workloads = [item for item in workloads if item["delta_ms"] is not None]
    return {
        "status": "comparable",
        "headline": comparison["headline"],
        "workloads": workloads,
        "passes": passes,
        "memory_bytes": memory,
        "binary_size_bytes": binary,
        "dominant_workload": max(comparable_workloads, key=lambda item: abs(item["delta_ms"]), default=None),
        "dominant_pass": max(passes, key=lambda item: abs(item["delta_ms"]), default=None),
    }


def derive_history_metrics(runs: list[dict]) -> dict:
    """Derive renderer-neutral point metadata for regression/evolution consumers."""
    points = []
    segment: list[dict] = []
    anchor = None
    for run in runs:
        reason = None
        if not segment:
            reason = "history_start"
        else:
            try:
                same_identity = measurement_identity(segment[-1]) == measurement_identity(run)
                same_workloads = workload_names(segment[-1]) == workload_names(run)
            except ValueError:
                same_identity = same_workloads = False
            if comparison_break(segment[-1], run):
                reason = "regime_boundary"
            elif not same_identity:
                reason = "scenario_changed"
            elif not same_workloads:
                reason = "workload_composition_changed"
        if reason:
            segment = []
            anchor = run
        previous = compare_runs(run, segment[-1]) if segment else {
            "status": "non_comparable", "reason": reason, **measurement_identity(run)
        }
        rolling = compare_to_rolling_baseline(run, segment)
        index = performance_index(run, anchor) if anchor is not None else {"status": "insufficient_data"}
        if run is anchor and index.get("status") == "comparable":
            index["status"] = "rebase"
            index["reason"] = reason
        points.append({
            "commit": run.get("commit"),
            "regime_id": run_regime(run),
            "workloads": list(workload_names(run)),
            "identity": measurement_identity(run),
            "previous": previous,
            "rolling_baseline": rolling,
            "performance_index": index,
            "metric_families": metric_families(run),
        })
        segment.append(run)
    return {
        "schema_version": POLICY_VERSION,
        "policy": {
            "estimator": "median_and_scaled_mad",
            "minimum_samples": MIN_SAMPLES,
            "baseline_min_runs": BASELINE_MIN_RUNS,
            "baseline_window": BASELINE_WINDOW,
            "threshold": "root_sum_square_observed_relative_variation",
            "latency_direction": "lower_is_better",
            "index_direction": "higher_is_better",
        },
        "points": points,
    }
