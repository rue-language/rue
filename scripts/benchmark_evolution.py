"""Renderer-neutral long-term benchmark evolution model.

The durable run history is authoritative.  This module only selects calendar
ranges, aggregates already-derived canonical performance indexes, and chooses
which raw points are cheap enough to draw.  It never replaces or truncates the
raw measurements embedded in the model.
"""

from __future__ import annotations

import math
from collections import defaultdict
from datetime import datetime, timedelta, timezone

from benchmark_history import comparison_break
from benchmark_metrics import derive_history_metrics, robust_summary


RANGES = {
    "30d": timedelta(days=30),
    "90d": timedelta(days=90),
    "1y": "calendar_year",
    "all": None,
}
MAX_RENDERED_RAW_POINTS = 400
REPOSITORY = "https://github.com/rue-language/rue"


def _timestamp(value: object) -> datetime:
    if not isinstance(value, str):
        raise ValueError("benchmark evolution requires ISO timestamps")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"invalid benchmark timestamp: {value}") from error
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def _calendar_year_before(value: datetime) -> datetime:
    """Return the prior-year anniversary, mapping Feb 29 to Feb 28."""
    try:
        return value.replace(year=value.year - 1)
    except ValueError:
        return value.replace(year=value.year - 1, day=28)


def _annotation_applies(event: dict, commit: str, resolver) -> bool:
    location = event.get("location", {})
    if location.get("kind") == "commit":
        return location.get("commit") == commit
    if location.get("kind") == "range":
        return resolver.is_ancestor(location["start_commit"], commit) and resolver.is_ancestor(
            commit, location["end_commit"]
        )
    return False


def _scope_applies(event: dict, platform: str, workload: str) -> bool:
    scope = event.get("scope", {})
    platforms = scope.get("platforms", ["*"])
    workloads = scope.get("workloads", ["*"])
    metrics = scope.get("metrics", ["*"])
    return ("*" in metrics or "latency" in metrics) and (
        "*" in platforms or platform in platforms
    ) and (
        "*" in workloads or workload == "all" or workload in workloads
    )


def _render_subset(points: list[dict], limit: int = MAX_RENDERED_RAW_POINTS) -> list[dict]:
    """Deterministically thin drawing points without crossing a boundary."""
    if len(points) <= limit:
        return points
    required = {0, len(points) - 1}
    required.update(index for index, point in enumerate(points) if point["boundary"] is not None)
    required.update(index for index, point in enumerate(points) if point["annotation_ids"])
    budget = max(0, limit - len(required))
    candidates = [index for index in range(len(points)) if index not in required]
    if budget >= len(candidates):
        selected = set(range(len(points)))
    elif budget:
        selected = required | {
            candidates[min(len(candidates) - 1, math.floor(offset * len(candidates) / budget))]
            for offset in range(budget)
        }
    else:
        # Boundaries are semantic evidence and may exceed the drawing budget.
        selected = required
    return [point for index, point in enumerate(points) if index in selected]


def _aggregate(points: list[dict], resolution: str) -> list[dict]:
    buckets: dict[tuple[int, str], list[dict]] = defaultdict(list)
    for point in points:
        if not isinstance(point["index"], (int, float)):
            continue
        instant = _timestamp(point["timestamp"])
        if resolution == "day":
            bucket = instant.date().isoformat()
        else:
            monday = (instant - timedelta(days=instant.weekday())).date()
            bucket = monday.isoformat()
        buckets[(point["segment"], bucket)].append(point)
    result = []
    for (segment, bucket), values in sorted(buckets.items(), key=lambda item: item[1][0]["timestamp"]):
        index_summary = robust_summary([point["index"] for point in values])
        if index_summary is None:
            continue
        absolute_values = [point["absolute_ms"] for point in values if point["absolute_ms"] is not None]
        absolute_summary = robust_summary(absolute_values)
        result.append({
            "timestamp": values[len(values) // 2]["timestamp"],
            "bucket": bucket,
            "resolution": resolution,
            "segment": segment,
            "index": index_summary["center"],
            "variation": index_summary["scaled_mad"],
            "minimum": index_summary["minimum"],
            "maximum": index_summary["maximum"],
            "sample_count": index_summary["sample_count"],
            "absolute_ms": absolute_summary["center"] if absolute_summary else None,
            "commits": [point["commit"] for point in values],
            "boundary": values[0]["boundary"],
        })
    return result


def _range_view(points: list[dict], start: datetime | None, resolution: str) -> dict:
    start_index = next(
        (
            index for index, point in enumerate(points)
            if start is None or _timestamp(point["timestamp"]) >= start
        ),
        len(points),
    )
    selected = points[start_index:]
    return {
        "raw_start_index": start_index,
        "rendered_raw_points": _render_subset(selected),
        "trend_points": _aggregate(selected, resolution),
        "raw_point_count": len(selected),
        "rendered_raw_point_count": len(_render_subset(selected)),
        "resolution": resolution,
    }


def _workload_families(runs: list[dict]) -> dict[str, dict]:
    """Expose explicit metadata with identity fallbacks for future RUE-900 data."""
    families = {}
    for run in runs:
        for bench in run.get("benchmarks", []):
            name = bench.get("name")
            if not isinstance(name, str):
                continue
            declared = bench.get("workload_family")
            family = declared if isinstance(declared, str) and declared else f"identity:{name}"
            label = bench.get("workload_family_label", declared or name)
            if not isinstance(label, str) or not label:
                raise ValueError(f"invalid workload-family label for {family}")
            candidate = {
                "id": family,
                "label": label,
                "source": "declared" if declared else "identity_fallback",
                "workloads": [],
            }
            entry = families.setdefault(family, candidate)
            if (entry["label"], entry["source"]) != (candidate["label"], candidate["source"]):
                raise ValueError(f"inconsistent workload-family metadata for {family}")
            if name not in entry["workloads"]:
                entry["workloads"].append(name)
    return families


def evolution_workspace(
    platform_histories: dict[str, list[dict]], annotations: list[dict], resolver
) -> dict:
    """Build long-term views from durable runs and canonical metric semantics."""
    instants = [_timestamp(run.get("timestamp")) for runs in platform_histories.values() for run in runs]
    range_end = max(instants, default=datetime.now(timezone.utc))
    all_series = []
    workload_names = set()
    family_metadata = {}

    for platform, runs in sorted(platform_histories.items()):
        derived = derive_history_metrics(runs)
        families = _workload_families(runs)
        for family, candidate in families.items():
            entry = family_metadata.setdefault(family, {
                **candidate,
                "workloads": [],
            })
            if (entry["label"], entry["source"]) != (
                candidate["label"], candidate["source"]
            ):
                raise ValueError(f"inconsistent workload-family metadata for {family}")
            entry["workloads"] = sorted(set(entry["workloads"]) | set(candidate["workloads"]))
        per_workload: dict[str, list[dict]] = defaultdict(list)
        overall: list[dict] = []
        segment = -1
        for index, (run, semantic) in enumerate(zip(runs, derived["points"])):
            performance = semantic["performance_index"]
            previous_semantic = semantic["previous"]
            if previous_semantic.get("status") != "comparable":
                segment += 1
            value = performance.get("value")
            publication = run.get("publication", {})
            coverage = publication.get("coverage", {}) if isinstance(publication, dict) else {}
            previous = runs[index - 1] if index else None
            reason = (
                performance.get("reason")
                if performance.get("status") == "rebase"
                else previous_semantic.get("reason")
                if previous_semantic.get("status") != "comparable"
                else None
            )
            if reason is None and comparison_break(previous, run):
                reason = "comparison_boundary"
            boundary = None
            if reason is not None:
                boundary = {
                    "reason": reason,
                    "regime_id": semantic["regime_id"],
                    "represented_commits": coverage.get("represented_commits", []),
                    "skipped_commits": coverage.get("skipped_commits", []),
                    "gap_unknown": coverage.get("gap_unknown", True),
                }
            commit = run.get("commit")
            applicable = [
                event["id"] for event in annotations
                if _scope_applies(event, platform, "all")
                and _annotation_applies(event, commit, resolver)
            ]
            metric_families = semantic["metric_families"]
            latency_values = list(metric_families["latency_ms"].values())
            base = {
                "timestamp": run.get("timestamp"),
                "commit": commit,
                "commit_url": f"{REPOSITORY}/commit/{commit}",
                "segment": segment,
                "index": value,
                "absolute_ms": sum(latency_values) if latency_values else None,
                "variation_pct": None,
                "boundary": boundary,
                "annotation_ids": applicable,
                "raw_samples_ms": {
                    bench.get("name"): bench.get("samples_ms", [])
                    for bench in run.get("benchmarks", [])
                    if isinstance(bench, dict) and isinstance(bench.get("name"), str)
                },
            }
            overall.append(base)
            performance_workloads = performance.get("workloads", {})
            for name in semantic["workloads"]:
                workload = performance_workloads.get(name, {})
                workload_names.add(name)
                per_workload[name].append({
                    **base,
                    "index": workload.get("value"),
                    "absolute_ms": workload.get("absolute_ms"),
                    "variation_pct": workload.get("relative_variation_pct"),
                    "raw_samples_ms": base["raw_samples_ms"].get(name, []),
                    "annotation_ids": [
                        event["id"] for event in annotations
                        if _scope_applies(event, platform, name)
                        and _annotation_applies(event, commit, resolver)
                    ],
                })

        for workload, points in [("all", overall), *sorted(per_workload.items())]:
            range_views = {}
            for range_id, duration in RANGES.items():
                start = (
                    _calendar_year_before(range_end)
                    if duration == "calendar_year"
                    else range_end - duration
                    if duration is not None
                    else None
                )
                resolution = "day" if range_id in {"30d", "90d"} else "week"
                range_views[range_id] = _range_view(points, start, resolution)
            all_series.append({
                "platform": platform,
                "workload": workload,
                "raw_points": points,
                "ranges": range_views,
            })

    return {
        "schema_version": 1,
        "metric_policy": "benchmark_metrics.derive_history_metrics",
        "annotation_policy": "benchmark_annotations.normalized_annotation_stream",
        "cross_platform_default": "normalized_index_separate_machine_series",
        "index": {"baseline": 100.0, "direction": "higher_is_better"},
        "range_end": range_end.isoformat().replace("+00:00", "Z"),
        "range_options": list(RANGES),
        "default_range": "90d",
        "platform_options": sorted(platform_histories),
        "workload_options": ["all", *sorted(workload_names)],
        "workload_families": [family_metadata[key] for key in sorted(family_metadata)],
        "series": all_series,
        "annotations": annotations,
        "raw_history_run_count": sum(len(runs) for runs in platform_histories.values()),
    }
