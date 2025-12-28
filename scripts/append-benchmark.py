#!/usr/bin/env python3
"""
Append benchmark results to the history file.

This script manages the benchmark history file used by the performance dashboard.
It handles:
- Appending new results to existing history
- Creating the history file if it doesn't exist
- Limiting history to the most recent 100 entries
- Validating JSON structure

Usage:
    ./append-benchmark.py <results.json> <history.json>

The results.json should have the format output by bench.sh:
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

The history.json file stores an array of such results:
{
    "version": 1,
    "runs": [
        { ...result1... },
        { ...result2... }
    ]
}
"""

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
    """Validate that a result has the required fields."""
    required_fields = ["timestamp", "benchmarks"]
    for field in required_fields:
        if field not in result:
            print(f"Warning: Result missing required field: {field}", file=sys.stderr)
            return False

    if not isinstance(result["benchmarks"], list):
        print("Warning: benchmarks field must be an array", file=sys.stderr)
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
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <results.json> <history.json>", file=sys.stderr)
        sys.exit(1)

    results_path = Path(sys.argv[1])
    history_path = Path(sys.argv[2])

    # Load the new results
    if not results_path.exists():
        print(f"Error: Results file not found: {results_path}", file=sys.stderr)
        sys.exit(1)

    with open(results_path, "r") as f:
        result = json.load(f)

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
