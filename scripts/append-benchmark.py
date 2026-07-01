#!/usr/bin/env python3
"""
Append benchmark results to the history file.

This script manages the benchmark history file used by the performance dashboard.
It handles:
- Appending new results to existing history
- Creating the history file if it doesn't exist
- Limiting history to the most recent 100 entries
- Validating JSON structure
- Supporting both version 1 and version 2 result schemas

Usage:
    ./append-benchmark.py <results.json> <history.json> [--reason push|manual]

When --reason is given, the result is upgraded to the version 2 schema
before appending: `benchmark_reason` is set, and `commit_range` defaults to
the run's own commit (benchmarks run once per commit, so a run covers
exactly the commit it was built from).

The results.json can be version 1 or version 2 format:

Version 1 (legacy):
{
    "version": 1,
    "timestamp": "2025-12-27T10:30:00Z",
    "commit": "abc123def",
    "host": {"os": "darwin", "arch": "arm64"},
    "iterations": 5,
    "benchmarks": [
        {"name": "many_functions", "mean_ms": 10.5, "std_ms": 0.5, "iterations": 5}
    ]
}

Version 2 (with commit range tracking, ADR-0031 Phase 4):
{
    "version": 2,
    "timestamp": "2025-12-27T10:30:00Z",
    "commit": "abc123def",
    "commit_range": ["abc123", "def456", "789abc"],
    "benchmark_reason": "scheduled" | "manual" | "push",
    "host": {"os": "darwin", "arch": "arm64"},
    "iterations": 5,
    "benchmarks": [...]
}

The history.json file stores an array of such results:
{
    "version": 1,
    "runs": [
        { ...result1... },
        { ...result2... }
    ]
}
"""

import argparse
import json
import sys
from pathlib import Path

# Maximum number of runs to keep in history
MAX_HISTORY_SIZE = 100


def load_json(path: Path) -> dict:
    """Load JSON from a file, returning empty structure if file doesn't exist."""
    if not path.exists():
        return {"version": 1, "runs": []}

    with open(path, "r") as f:
        data = json.load(f)

    # Handle legacy format (direct array)
    if isinstance(data, list):
        return {"version": 1, "runs": data}

    return data


def save_json(path: Path, data: dict) -> None:
    """Save JSON to a file with pretty printing."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        json.dump(data, f, indent=2)
        f.write("\n")


def validate_result(result: dict) -> bool:
    """Validate that a result has the required fields and non-empty data."""
    required_fields = ["timestamp", "benchmarks"]
    for field in required_fields:
        if field not in result:
            print(f"Error: Result missing required field: {field}", file=sys.stderr)
            return False

    if not isinstance(result["benchmarks"], list):
        print("Error: benchmarks field must be an array", file=sys.stderr)
        return False

    if len(result["benchmarks"]) == 0:
        print("Error: benchmarks array is empty - no data to record", file=sys.stderr)
        print("This usually means all benchmark iterations failed.", file=sys.stderr)
        return False

    # Validate each benchmark has required fields
    for i, bench in enumerate(result["benchmarks"]):
        if "name" not in bench:
            print(f"Error: benchmark[{i}] missing 'name' field", file=sys.stderr)
            return False
        if "mean_ms" not in bench:
            print(f"Error: benchmark[{i}] ({bench.get('name', '?')}) missing 'mean_ms' field", file=sys.stderr)
            return False

    return True


def append_result(history: dict, result: dict) -> dict:
    """Append a result to the history, maintaining size limit."""
    if "runs" not in history:
        history["runs"] = []

    history["runs"].append(result)

    # Keep only the most recent MAX_HISTORY_SIZE entries
    if len(history["runs"]) > MAX_HISTORY_SIZE:
        history["runs"] = history["runs"][-MAX_HISTORY_SIZE:]

    return history


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path, help="results.json from bench.sh")
    parser.add_argument("history", type=Path, help="history file to append to")
    parser.add_argument(
        "--reason",
        choices=["push", "manual", "scheduled"],
        help="what triggered this run; upgrades the result to the v2 schema",
    )
    args = parser.parse_args()

    results_path = args.results
    history_path = args.history

    # Load the new results
    if not results_path.exists():
        print(f"Error: Results file not found: {results_path}", file=sys.stderr)
        sys.exit(1)

    with open(results_path, "r") as f:
        result = json.load(f)

    # Upgrade to the version 2 schema (commit range tracking)
    if args.reason is not None:
        result["version"] = 2
        result["benchmark_reason"] = args.reason
        if not result.get("commit_range"):
            commit = result.get("commit")
            result["commit_range"] = [commit] if commit else []

    # Validate the result
    if not validate_result(result):
        print("Error: Invalid result format", file=sys.stderr)
        sys.exit(1)

    # Load existing history
    history = load_json(history_path)

    # Append the new result
    history = append_result(history, result)

    # Save updated history
    save_json(history_path, history)

    print(f"Appended result to history ({len(history['runs'])} total runs)")


if __name__ == "__main__":
    main()
