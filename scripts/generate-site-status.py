#!/usr/bin/env python3
"""
Generate the homepage status-board data (website/static/status.json).

Everything here is derived at build time from the repo itself — no services,
no APIs:

- commit / commit_date / commit_count  — from git
- spec_rules      — count of normative-category {{ rule(...) }} markers in
                    docs/spec/src (categories that the traceability check in
                    ./test.sh requires 100% test coverage for)
- spec_cases      — count of [[case]] entries in crates/rue-spec/cases
- bench           — compile-time sparkline over the last 20 runs, from the
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
from pathlib import Path

from benchmark_history import latest_comparable_segment, load_history
from benchmark_annotations import (
    DEFAULT_ANNOTATIONS,
    GitCommitResolver,
    load_annotations,
    normalized_annotations,
)
from benchmark_metrics import derive_history_metrics

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

SPARKLINE_RUNS = 20
SPARK_W, SPARK_H = 300, 52
SPARK_Y_MIN, SPARK_Y_MAX = 8, 46  # keep the line off the edges


def git(*args: str) -> str:
    return subprocess.check_output(
        ["git", *args], cwd=REPO_ROOT, text=True
    ).strip()


def commit_info() -> dict:
    try:
        return {
            "commit": git("rev-parse", "--short", "HEAD"),
            "commit_date": git("log", "-1", "--format=%cs"),
            "commit_count": int(git("rev-list", "--count", "HEAD")),
        }
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


def sparkline_data(runs: list[dict], annotations: list[dict] | None = None) -> dict | None:
    comparable_segment = latest_comparable_segment(runs)
    runs = comparable_segment[-SPARKLINE_RUNS:]
    totals = []
    for run in runs:
        benches = run.get("benchmarks", [])
        if benches:
            totals.append(sum(b.get("mean_ms", 0) for b in benches))
    if not totals:
        return None

    lo, hi = min(totals), max(totals)
    span = (hi - lo) or 1.0
    points = []
    for i, total in enumerate(totals):
        if len(totals) == 1:
            x = SPARK_W
        else:
            x = SPARK_W * i / (len(totals) - 1)
        y = SPARK_Y_MIN + (SPARK_Y_MAX - SPARK_Y_MIN) * (total - lo) / span
        points.append(f"{x:.0f},{y:.1f}")

    result = {
        "latest_ms": round(totals[-1]),
        "n_runs": len(totals),
        "points": " ".join(points),
        "last_x": points[-1].split(",")[0],
        "last_y": points[-1].split(",")[1],
    }
    # The visual is deliberately bounded, but the index remains anchored to
    # the beginning of the full comparable segment.
    derived = derive_history_metrics(comparable_segment)
    if derived["points"]:
        latest = derived["points"][-1]
        result["performance_index"] = latest["performance_index"]
        result["previous_comparison"] = latest["previous"]
        result["baseline_comparison"] = latest["rolling_baseline"]
    result["annotations"] = annotations or []
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
