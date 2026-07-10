#!/usr/bin/env python3
"""
Validate a bench.sh results file before it enters the benchmark history.

Run in CI after bench.sh so a platform with a missing, duplicate, or unknown
benchmark result turns into a failed job instead of publishing an incomplete
corpus. Prints a summary of what was collected.

Usage:
    ./validate-benchmark.py <results.json>
"""

import argparse
import json
from pathlib import Path
import sys

from benchmark_validation import (
    DEFAULT_MANIFEST,
    load_manifest_names,
    validate_results,
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path, help="results.json from bench.sh")
    parser.add_argument(
        "--manifest",
        type=Path,
        default=DEFAULT_MANIFEST,
        help="benchmark manifest (default: benchmarks/manifest.toml)",
    )
    args = parser.parse_args()

    try:
        with args.results.open() as f:
            data = json.load(f)
        expected_names = load_manifest_names(args.manifest)
    except (OSError, ValueError) as error:
        print(f"::error::Cannot validate benchmark results: {error}")
        return 1

    benchmarks = data.get("benchmarks", []) if isinstance(data, dict) else []
    commit = data.get("commit", "unknown") if isinstance(data, dict) else "unknown"
    print(f"Commit: {commit}")
    print(f"Benchmarks collected: {len(benchmarks)}")

    for bench in benchmarks:
        if not isinstance(bench, dict):
            continue
        name = bench.get("name", "unknown")
        mean = bench.get("mean_ms")
        if isinstance(mean, (int, float)) and not isinstance(mean, bool):
            print(f"  - {name}: {mean:.2f}ms")

    errors = validate_results(data, expected_names)
    for error in errors:
        print(f"::error::{error}")

    if not errors:
        print("Validation passed!")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
