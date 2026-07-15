#!/usr/bin/env python3
"""Validate, enrich, and append a run to durable partitioned history."""

import argparse
import json
import sys
from pathlib import Path

from benchmark_history import (
    HISTORY_VERSION,
    corpus_metadata,
    git_coverage,
    legacy_history,
    load_history,
    mark_legacy,
    regime_metadata,
    save_history,
    validate_publication,
)
from benchmark_validation import DEFAULT_MANIFEST, load_manifest_names, validate_results


def latest_measured_commit(runs: list[dict]) -> str | None:
    for run in reversed(runs):
        publication = run.get("publication", {})
        coverage = publication.get("coverage", {}) if isinstance(publication, dict) else {}
        commit = coverage.get("measured_commit") or run.get("commit")
        if isinstance(commit, str) and commit and commit != "unknown":
            return commit
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path)
    parser.add_argument("history", type=Path, help="v3 per-platform history directory")
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--legacy-history", type=Path)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--runner-image")
    parser.add_argument("--reason", choices=["push", "manual", "scheduled"], required=True)
    parser.add_argument("--git-repository", type=Path)
    args = parser.parse_args()

    try:
        result = json.loads(args.results.read_text())
        expected = load_manifest_names(args.manifest)
        errors = validate_results(result, expected)
        for bench in result.get("benchmarks", []) if isinstance(result, dict) else []:
            if isinstance(bench, dict) and "samples_ms" not in bench:
                errors.append(f"benchmark '{bench.get('name', '?')}' is missing raw samples_ms")
        if errors:
            raise ValueError("; ".join(errors))

        history = load_history(args.history)
        runs = list(history["runs"])
        if not runs and args.legacy_history:
            runs = [mark_legacy(run) for run in legacy_history(args.legacy_history)]

        corpus = corpus_metadata(args.manifest)
        runner_image = args.runner_image or result.get("runner_image")
        if not isinstance(runner_image, str) or not runner_image:
            raise ValueError("runner image/environment identity is required")
        regime = regime_metadata(result, args.platform, runner_image, corpus)
        commit = result.get("commit")
        if not isinstance(commit, str) or not commit or commit == "unknown":
            raise ValueError("commit must identify the measured compiler revision")
        previous = latest_measured_commit(runs)
        coverage = git_coverage(previous, commit, args.git_repository) if args.git_repository else {
            "measured_commit": commit,
            "represented_commits": [commit],
            "skipped_commits": [],
            "gap_unknown": previous is None or previous != commit,
        }
        timing_versions = regime["dimensions"]["timing_schema_versions"]
        result["version"] = HISTORY_VERSION
        result["publication"] = {
            "schema_version": HISTORY_VERSION,
            "comparable": True,
            "regime_id": regime["id"],
            "regime": regime["dimensions"],
            "corpus": corpus,
            "timing_schema_versions": timing_versions,
            "build_mode": result.get("build_mode"),
            "compiler": {"commit": commit, "identity": f"rue@{commit}"},
            "iteration_policy": {"kind": "fixed", "iterations": result.get("iterations")},
            "platform": args.platform,
            "runner": {"image": runner_image, "host": result.get("host")},
            "metric_semantics": {
                "measurement_family": result.get("measurement_family", "compiler"),
                "scenario_family": result.get("scenario_family", "cold_compilation"),
            },
            "trigger_reason": args.reason,
            "coverage": coverage,
        }
        publication_errors = validate_publication(result)
        if publication_errors:
            raise ValueError("; ".join(publication_errors))
        runs.append(result)
        save_history(args.history, runs, args.platform)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1

    print(f"Appended result to partitioned history ({len(runs)} total runs)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
