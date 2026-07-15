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
- bench           — canonical 30-day comparable trend, from the
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
from benchmark_metrics import derive_history_metrics
from benchmark_recent import GitRecentResolver

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


def _spark_points(
    trend_points: list[dict], window_start: datetime, window_end: datetime
) -> tuple[str, str, str]:
    values = [point["index"] for point in trend_points]
    lo, hi = min(values), max(values)
    span = (hi - lo) or 1.0
    window_seconds = max(1.0, (window_end - window_start).total_seconds())
    points = []
    for point, value in zip(trend_points, values):
        instant = _parse_timestamp(point.get("timestamp"))
        x = SPARK_W * (instant - window_start).total_seconds() / window_seconds
        x = max(0.0, min(float(SPARK_W), x))
        # The canonical index is higher-is-better, so improvement rises.
        y = SPARK_Y_MAX - (SPARK_Y_MAX - SPARK_Y_MIN) * (value - lo) / span
        points.append(f"{x:.0f},{y:.1f}")
    return " ".join(points), *points[-1].split(",")


def sparkline_data(
    runs: list[dict],
    annotations: list[dict] | None = None,
    platform: str = "x86-64-linux",
    as_of: datetime | None = None,
    resolver=None,
) -> dict | None:
    """Build the compact homepage view from canonical evolution semantics."""
    if not runs or any(_parse_timestamp(run.get("timestamp")) is None for run in runs):
        return None
    annotations = annotations or []
    resolver = resolver or GitRecentResolver(REPO_ROOT)
    evolution = evolution_workspace({platform: runs}, annotations, resolver)
    series = next(item for item in evolution["series"] if item["workload"] == "all")
    view = series["ranges"]["30d"]
    window_points = series["raw_points"][view["raw_start_index"]:]
    if not window_points:
        return None
    latest_point = window_points[-1]
    segment = latest_point["segment"]
    comparable_points = [point for point in window_points if point["segment"] == segment]
    trend_points = [point for point in view["trend_points"] if point["segment"] == segment]
    trend_points = [
        point for point in trend_points if isinstance(point["index"], (int, float))
    ]
    range_end = _parse_timestamp(evolution["range_end"])
    range_start = range_end - timedelta(days=30)
    endpoint = _spark_points(trend_points, range_start, range_end) if trend_points else None

    derived = derive_history_metrics(runs)
    latest_semantic = derived["points"][-1]
    performance_index = latest_semantic["performance_index"]
    baseline = latest_semantic["rolling_baseline"]
    if performance_index.get("status") == "rebase":
        state = {
            "kind": "baseline_reset",
            "label": "baseline reset",
            "reason": performance_index.get("reason", "comparison boundary"),
        }
    elif (
        baseline.get("status") != "comparable"
        or baseline.get("headline", {}).get("classification") == "insufficient_data"
    ):
        state = {
            "kind": "insufficient_data",
            "label": "insufficient data",
            "reason": baseline.get("reason", "not enough comparable baseline runs"),
        }
    else:
        headline = baseline["headline"]
        state = {
            "kind": headline["classification"],
            "label": headline["classification"],
            "delta_pct": headline["delta_pct"],
            "variation_pct": headline["variation_pct"],
            "reference": "rolling baseline",
        }

    latest_run = runs[-1]
    measured_at = _parse_timestamp(latest_run.get("timestamp"))
    now = as_of or datetime.now(timezone.utc)
    if now.tzinfo is None:
        now = now.replace(tzinfo=timezone.utc)
    age_seconds = max(0, int((now.astimezone(timezone.utc) - measured_at).total_seconds()))
    freshness = {
        "kind": "stale" if age_seconds > 48 * 60 * 60 else "current",
        "measured_at": latest_run["timestamp"],
        "age_seconds": age_seconds,
    }
    publication = latest_run.get("publication", {})
    coverage = publication.get("coverage", {}) if isinstance(publication, dict) else {}
    coverage_status = {
        "measured_commit": coverage.get("measured_commit", latest_run.get("commit")),
        "represented_commits": coverage.get("represented_commits", []),
        "skipped_commits": coverage.get("skipped_commits", []),
        "gap_unknown": coverage.get("gap_unknown", True),
    }

    events_by_id = {event.get("id"): event for event in annotations}
    relevant_annotation = None
    for point in reversed(window_points):
        candidates = [
            events_by_id[event_id]
            for event_id in point["annotation_ids"]
            if event_id in events_by_id
        ]
        if candidates:
            candidates.sort(
                key=lambda event: (
                    event.get("source") != "authored",
                    event.get("id", ""),
                )
            )
            relevant_annotation = dict(candidates[0])
            annotation_id = quote(str(relevant_annotation.get("id", "")), safe="")
            relevant_annotation["dashboard_url"] = (
                f"/performance/?range=30d&platform={quote(platform, safe='')}&annotation={annotation_id}"
                "#evolution-workspace"
            )
            break

    result = {
        "platform": platform,
        "window": "30d",
        "metric": "canonical_normalized_performance_index",
        "n_runs": len(comparable_points),
        "n_trend_points": len(trend_points),
        "points": endpoint[0] if endpoint else "",
        "latest_index": performance_index.get("value"),
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
