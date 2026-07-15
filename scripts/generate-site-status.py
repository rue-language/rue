#!/usr/bin/env python3
"""
Generate the homepage status-board data (website/static/status.json).

Everything here is derived at build time from the repo itself — no services,
no APIs:

- commit / commit_date / commit_count  — from git (count only when complete)
- spec_rules      — count of normative-category {{ rule(...) }} markers in
                    docs/spec/src (categories that the traceability check in
                    ./test.sh requires 100% test coverage for)
- spec_cases      — count of [[case]] entries in crates/rue-spec/cases
- bench           — absolute suite time and recent measured-commit trend, from the
                    x86-64-linux benchmark history (fetched from the perf
                    branch by the deploy workflow; absent locally is fine)

Run from anywhere; paths resolve relative to the repo root. Missing inputs
degrade to omitted fields — the homepage template renders what exists.

Usage:
    ./generate-site-status.py [--output PATH]
"""

import argparse
import json
import re
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.parse import quote

from benchmark_history import load_history
from benchmark_annotations import (
    DEFAULT_ANNOTATIONS,
    GitCommitResolver,
    load_annotations,
    normalized_annotations,
)
from benchmark_evolution import evolution_workspace
from benchmark_metrics import absolute_latency_ms, derive_history_metrics

REPO_ROOT = Path(__file__).resolve().parent.parent

# Categories the spec traceability check treats as normative (must have
# test coverage); keep in sync with crates/rue-spec's traceability check.
NORMATIVE_CATS = {
    "normative",
    "legality-rule",
    "dynamic-semantics",
    "syntax",
    "undefined-behavior",
}

SPARK_W, SPARK_H = 300, 52
SPARK_Y_MIN, SPARK_Y_MAX = 8, 46  # keep the line off the edges


def git(*args: str) -> str:
    return subprocess.check_output(
        ["git", *args], cwd=REPO_ROOT, text=True
    ).strip()


def commit_info() -> dict:
    try:
        result = {
            "commit": git("rev-parse", "--short", "HEAD"),
            "commit_date": git("log", "-1", "--format=%cs"),
            "history_complete": git("rev-parse", "--is-shallow-repository") == "false",
        }
        if result["history_complete"]:
            result["commit_count"] = int(git("rev-list", "--count", "HEAD"))
        return result
    except (subprocess.CalledProcessError, FileNotFoundError):
        return {}


def spec_rule_count() -> int:
    spec_dir = REPO_ROOT / "docs" / "spec" / "src"
    if not spec_dir.is_dir():
        return 0
    pattern = re.compile(r'rule\(id="[^"]+",\s*cat="([^"]+)"')
    count = 0
    for md in spec_dir.rglob("*.md"):
        for cat in pattern.findall(md.read_text()):
            if cat in NORMATIVE_CATS:
                count += 1
    return count


def spec_case_count() -> int:
    cases_dir = REPO_ROOT / "crates" / "rue-spec" / "cases"
    if not cases_dir.is_dir():
        return 0
    return sum(
        toml.read_text().count("[[case]]") for toml in cases_dir.rglob("*.toml")
    )


def _parse_timestamp(value: object) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def _run_suite_ms(run: dict) -> float | None:
    values = []
    for benchmark in run.get("benchmarks", []):
        value, _ = absolute_latency_ms(benchmark)
        if value is None:
            return None
        values.append(value)
    return sum(values) if values else None


def _spark_points(
    points: list[dict], window_start: datetime, window_end: datetime
) -> tuple[str, str, str]:
    values = [point["index"] for point in points]
    lo, hi = min(values), max(values)
    span = (hi - lo) or 1.0
    coordinates = []
    window_seconds = max(1.0, (window_end - window_start).total_seconds())
    for point, value in zip(points, values):
        instant = _parse_timestamp(point.get("timestamp"))
        x = SPARK_W * (instant - window_start).total_seconds() / window_seconds
        x = max(0.0, min(float(SPARK_W), x))
        y = SPARK_Y_MAX - (SPARK_Y_MAX - SPARK_Y_MIN) * (value - lo) / span
        coordinates.append(f"{x:.0f},{y:.1f}")
    return " ".join(coordinates), *coordinates[-1].split(",")


def _freshness_label(age_seconds: int) -> str:
    if age_seconds < 60 * 60:
        return "measured within the last hour"
    if age_seconds < 24 * 60 * 60:
        hours = max(1, age_seconds // (60 * 60))
        return f"measured {hours} hour{'s' if hours != 1 else ''} ago"
    days = max(1, age_seconds // (24 * 60 * 60))
    if days == 1:
        return "measured yesterday"
    return f"measured {days} days ago"


def sparkline_data(
    runs: list[dict],
    annotations: list[dict] | None = None,
    platform: str = "x86-64-linux",
    as_of: datetime | None = None,
    resolver=None,
) -> dict | None:
    """Build a compact 30-day view from canonical evolution semantics."""
    if not runs or any(_parse_timestamp(run.get("timestamp")) is None for run in runs):
        return None
    annotations = annotations or []
    resolver = resolver or GitCommitResolver(REPO_ROOT)
    workspace = evolution_workspace({platform: runs}, annotations, resolver)
    series = next(item for item in workspace["series"] if item["workload"] == "all")
    view = series["ranges"]["30d"]
    window_points = series["raw_points"][view["raw_start_index"]:]
    if not window_points:
        return None
    latest_segment = window_points[-1]["segment"]
    comparable_points = [point for point in window_points if point["segment"] == latest_segment]
    trend_points = [
        point for point in view["trend_points"]
        if point["segment"] == latest_segment and isinstance(point["index"], (int, float))
    ]
    range_end = _parse_timestamp(workspace["range_end"])
    range_start = range_end - timedelta(days=30)
    endpoint = _spark_points(trend_points, range_start, range_end) if trend_points else None

    derived = derive_history_metrics(runs)
    latest_semantic = derived["points"][-1]
    baseline = latest_semantic["rolling_baseline"]
    previous = latest_semantic["previous"]
    if previous.get("status") == "non_comparable" and len(runs) > 1:
        state = {
            "kind": "baseline_reset",
            "label": "New comparison baseline",
            "summary": "The measurement setup changed, so no trend is claimed yet.",
            "reason": previous.get("reason", "comparison boundary"),
        }
    elif (
        baseline.get("status") != "comparable"
        or baseline.get("headline", {}).get("classification") == "insufficient_data"
    ):
        state = {
            "kind": "insufficient_data",
            "label": "Establishing a baseline",
            "summary": "There are not enough comparable measurements for a trustworthy change yet.",
            "reason": baseline.get("reason", "not enough comparable baseline runs"),
        }
    else:
        headline = baseline["headline"]
        labels = {
            "stable": ("Within usual variation", "about the same as the recent baseline"),
            "improved": ("Compiler is faster", "faster than the recent baseline"),
            "regressed": ("Possible regression", "slower than the recent baseline"),
        }
        label, direction = labels[headline["classification"]]
        delta = abs(headline["delta_pct"])
        state = {
            "kind": headline["classification"],
            "label": label,
            "summary": f"{delta:.1f}% {direction}",
            "delta_pct": headline["delta_pct"],
            "variation_pct": headline["variation_pct"],
        }

    latest_run = runs[-1]
    measured_at = _parse_timestamp(latest_run.get("timestamp"))
    now = as_of or datetime.now(timezone.utc)
    if now.tzinfo is None:
        now = now.replace(tzinfo=timezone.utc)
    age_seconds = max(0, int((now.astimezone(timezone.utc) - measured_at).total_seconds()))
    freshness = {
        "kind": "stale" if age_seconds > 48 * 60 * 60 else "current",
        "age_seconds": age_seconds,
        "label": _freshness_label(age_seconds),
    }

    suite_ms = _run_suite_ms(latest_run)
    publication = latest_run.get("publication", {})
    coverage = publication.get("coverage", {}) if isinstance(publication, dict) else {}
    coverage_status = {
        "measured_commit": coverage.get("measured_commit", latest_run.get("commit")),
        "represented_count": len(coverage.get("represented_commits", [])),
        "skipped_count": len(coverage.get("skipped_commits", [])),
        "gap_unknown": coverage.get("gap_unknown", True),
    }

    events_by_id = {event.get("id"): event for event in annotations}
    relevant_annotation = None
    candidates = [
        (event.get("source") != "authored", -point_index, event.get("id", ""), event)
        for point_index, point in enumerate(comparable_points)
        for event_id in point["annotation_ids"]
        if (event := events_by_id.get(event_id)) is not None
    ]
    if candidates:
        candidates.sort(key=lambda item: item[:3])
        relevant_annotation = dict(candidates[0][3])
        annotation_id = quote(str(relevant_annotation.get("id", "")), safe="")
        relevant_annotation["dashboard_url"] = (
            f"/performance/?range=30d&platform={quote(platform, safe='')}"
            f"&annotation={annotation_id}#evolution-workspace"
        )
        relevant_annotation["category_label"] = str(
            relevant_annotation.get("category", "measurement_infrastructure")
        ).replace("_", " ")

    result = {
        "platform": platform,
        "platform_label": "Linux x86-64",
        "window": "30d",
        "n_runs": len(comparable_points),
        "n_trend_points": len(trend_points),
        "points": endpoint[0] if endpoint else "",
        "latest_suite_ms": suite_ms,
        "state": state,
        "freshness": freshness,
        "coverage": coverage_status,
        "dashboard_url": f"/performance/?range=30d&platform={quote(platform, safe='')}#evolution-workspace",
    }
    if endpoint:
        result["last_x"] = endpoint[1]
        result["last_y"] = endpoint[2]
    if relevant_annotation:
        result["annotation"] = relevant_annotation
    return result


def bench_sparkline() -> dict | None:
    history_path = REPO_ROOT / "website" / "static" / "benchmarks" / "history-x86-64-linux"
    legacy_path = history_path.with_suffix(".json")
    if not history_path.exists() and not legacy_path.exists():
        return None

    try:
        history = load_history(history_path if history_path.exists() else legacy_path)
    except (OSError, json.JSONDecodeError, ValueError):
        return None

    runs = history.get("runs", [])
    annotations = normalized_annotations(
        runs,
        load_annotations(DEFAULT_ANNOTATIONS),
        GitCommitResolver(REPO_ROOT),
    )
    return sparkline_data(runs, annotations)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        default=REPO_ROOT / "website" / "static" / "status.json",
    )
    args = parser.parse_args()

    status = commit_info()
    status["spec_rules"] = spec_rule_count()
    status["spec_cases"] = spec_case_count()
    bench = bench_sparkline()
    if bench:
        status["bench"] = bench

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(status, indent=2) + "\n")
    print(f"  Generated {args.output}: {json.dumps(status)[:120]}...")
    return 0


if __name__ == "__main__":
    sys.exit(main())
