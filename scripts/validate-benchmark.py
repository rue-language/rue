#!/usr/bin/env python3
"""
Validate a bench.sh results file before it enters the benchmark history.

Run in CI after bench.sh so a platform whose benchmarks all failed turns
into a failed job (a distinct, visible failure) instead of silently
appending an empty run to the history. Prints a summary of what was
collected.

Usage:
    ./validate-benchmark.py <results.json>
"""

import json
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <results.json>", file=sys.stderr)
        return 1

    with open(sys.argv[1]) as f:
        data = json.load(f)

    benchmarks = data.get("benchmarks", [])
    print(f"Commit: {data.get('commit', 'unknown')}")
    print(f"Benchmarks collected: {len(benchmarks)}")

    if not benchmarks:
        print("::error::No benchmark results collected! All benchmarks failed.")
        return 1

    ok = True
    for bench in benchmarks:
        name = bench.get("name", "unknown")
        mean = bench.get("mean_ms")
        if mean is None:
            print(f"::error::Benchmark '{name}' is missing mean_ms")
            ok = False
        else:
            print(f"  - {name}: {mean:.2f}ms")

    if ok:
        print("Validation passed!")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
