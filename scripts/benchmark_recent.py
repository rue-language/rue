"""Renderer-neutral recent benchmark regression workspace."""

from __future__ import annotations

import re
import subprocess
from datetime import datetime, timezone
from pathlib import Path

from benchmark_history import run_regime
from benchmark_metrics import (
    comparison_explanation,
    derive_history_metrics,
    sample_summary,
)


WINDOWS = (20, 50, 100)
STALE_AFTER_SECONDS = 48 * 60 * 60
ISSUE_RE = re.compile(r"\bRUE-[0-9]+\b")
PR_RE = re.compile(r"#([1-9][0-9]*)\b")
REPOSITORY = "https://github.com/rue-language/rue"


class GitRecentResolver:
    def __init__(self, repository: Path):
        self.repository = repository

    def metadata(self, commit: str) -> dict:
        process = subprocess.run(
            ["git", "show", "-s", "--format=%H%x00%s%x00%cs%x00%B", commit],
            cwd=self.repository,
            text=True,
            capture_output=True,
            check=False,
        )
        fields = process.stdout.rstrip("\n").split("\0")
        if process.returncode != 0 or len(fields) != 4:
            raise ValueError(f"unknown benchmark commit: {commit}")
        canonical, subject, date, body = fields
        issues = sorted(set(ISSUE_RE.findall(subject + "\n" + body)))
        pulls = sorted({int(value) for value in PR_RE.findall(subject + "\n" + body)})
        return {
            "commit": canonical,
            "metadata_status": "available",
            "subject": subject,
            "date": date,
            "links": {
                "commit": f"{REPOSITORY}/commit/{canonical}",
                "issues": [
                    {"id": issue, "url": f"https://linear.app/steve-klabnik/issue/{issue}"}
                    for issue in issues
                ],
                "pull_requests": [
                    {"id": f"PR #{number}", "url": f"{REPOSITORY}/pull/{number}"}
                    for number in pulls
                ],
            },
        }

    def is_ancestor(self, start: str, end: str) -> bool:
        return subprocess.run(
            ["git", "merge-base", "--is-ancestor", start, end],
            cwd=self.repository,
            capture_output=True,
            check=False,
        ).returncode == 0


def _annotation_applies(event: dict, commit: str, resolver) -> bool:
    location = event.get("location", {})
    if location.get("kind") == "commit":
        return location.get("commit") == commit
    if location.get("kind") == "range":
        return resolver.is_ancestor(location["start_commit"], commit) and resolver.is_ancestor(
            commit, location["end_commit"]
        )
    return False


def _parse_timestamp(value: object) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)
    except ValueError:
        return None


def recent_workspace(
    runs: list[dict],
    annotations: list[dict],
    resolver,
    platform: str,
    as_of: datetime | None = None,
) -> dict:
    """Build the only model consumed by the recent-workspace UI."""
    selected_runs = runs[-max(WINDOWS):]
    derived = derive_history_metrics(selected_runs)
    timestamps = [
        value for run in selected_runs
        if (value := _parse_timestamp(run.get("timestamp"))) is not None
    ]
    newest = max(timestamps, default=None)
    as_of = as_of or datetime.now(timezone.utc)
    if as_of.tzinfo is None:
        as_of = as_of.replace(tzinfo=timezone.utc)
    else:
        as_of = as_of.astimezone(timezone.utc)
    points = []
    for run, semantic in zip(selected_runs, derived["points"]):
        commit = run.get("commit")
        try:
            metadata = resolver.metadata(commit)
        except ValueError:
            # Old checked-in history can name revisions no longer present in
            # the repository graph. Preserve the measurement while making the
            # missing authority explicit instead of inventing metadata.
            measured = run.get("timestamp")
            metadata = {
                "commit": commit,
                "metadata_status": "unavailable",
                "subject": "Commit metadata unavailable",
                "date": measured[:10] if isinstance(measured, str) else None,
                "links": {
                    "commit": f"{REPOSITORY}/commit/{commit}",
                    "issues": [],
                    "pull_requests": [],
                },
            }
        measured_at = _parse_timestamp(run.get("timestamp"))
        publication = run.get("publication", {})
        coverage = publication.get("coverage", {}) if isinstance(publication, dict) else {}
        point_annotations = [
            event for event in annotations
            if _annotation_applies(event, metadata["commit"], resolver)
        ]
        links = dict(metadata["links"])
        links["annotations"] = sorted({event["link"] for event in point_annotations})
        wall_age = (
            max(0, int((as_of - measured_at).total_seconds()))
            if measured_at is not None else None
        )
        points.append({
            **metadata,
            "links": links,
            "measured_at": run.get("timestamp"),
            "freshness": {
                "age_from_latest_seconds": (
                    int((newest - measured_at).total_seconds())
                    if newest is not None and measured_at is not None else None
                ),
                "latest": measured_at == newest if measured_at is not None else False,
                "wall_clock_age_seconds": wall_age,
                "stale": wall_age is None or wall_age > STALE_AFTER_SECONDS,
                "as_of": as_of.isoformat().replace("+00:00", "Z"),
            },
            "coverage": {
                "measured_commit": coverage.get("measured_commit"),
                "represented_commits": coverage.get("represented_commits", []),
                "skipped_commits": coverage.get("skipped_commits", []),
                "gap_unknown": coverage.get("gap_unknown", True),
            },
            "regime_id": run_regime(run),
            "comparability": semantic["previous"],
            "annotations": point_annotations,
            "workloads": [
                {
                    "name": bench.get("name"),
                    "raw_samples_ms": bench.get("samples_ms", []),
                    "uncertainty": sample_summary(bench.get("samples_ms")),
                }
                for bench in run.get("benchmarks", [])
            ],
        })

    segments: list[list[int]] = []
    for index, point in enumerate(points):
        if index == 0 or point["comparability"].get("status") != "comparable":
            segments.append([])
        segments[-1].append(index)

    comparisons = []
    for segment in segments:
        for offset, before_index in enumerate(segment):
            for after_index in segment[offset + 1:]:
                explanation = comparison_explanation(
                    selected_runs[after_index], selected_runs[before_index]
                )
                if explanation.get("status") != "comparable":
                    raise ValueError("canonical segment unexpectedly produced a non-comparable pair")
                comparisons.append({
                    "before": points[before_index]["commit"],
                    "after": points[after_index]["commit"],
                    "explanation": explanation,
                })

    default_comparison = next(
        (
            item for item in reversed(comparisons)
            if item["explanation"].get("headline", {}).get("classification")
            != "insufficient_data"
        ),
        comparisons[-1] if comparisons else None,
    )

    small_multiples = {}
    for point in points:
        for workload in point["workloads"]:
            summary = workload["uncertainty"]
            if summary is None:
                continue
            small_multiples.setdefault(workload["name"], []).append({
                "commit": point["commit"],
                "median_ms": summary["center"],
                "variation_pct": summary["relative_variation_pct"],
                "comparison_boundary": point["comparability"].get("status") != "comparable",
            })

    return {
        "schema_version": 1,
        "platform": platform,
        "window_options": list(WINDOWS),
        "default_window": WINDOWS[0],
        "points": points,
        "comparisons": comparisons,
        "default_selection": (
            {"before": default_comparison["before"], "after": default_comparison["after"]}
            if default_comparison else None
        ),
        "small_multiples": [
            {"workload": name, "points": values}
            for name, values in sorted(small_multiples.items())
        ],
        "annotations": annotations,
    }
