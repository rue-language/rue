#!/usr/bin/env python3
"""Run paired fresh-process AIR payload benchmarks (ADR-0056)."""

import argparse
import hashlib
import json
import re
import statistics
import subprocess
from pathlib import Path


REAL = re.compile(r"^\s*([0-9.]+) real", re.MULTILINE)
RSS = re.compile(r"^\s*(\d+)\s+maximum resident set size", re.MULTILINE)


def run(binary: Path, workload: Path) -> dict:
    completed = subprocess.run(
        ["/usr/bin/time", "-l", str(binary), str(workload)],
        check=True,
        capture_output=True,
        text=True,
    )
    sample = json.loads(completed.stdout)
    sample["wall_seconds"] = float(REAL.search(completed.stderr).group(1))
    sample["peak_rss_bytes"] = int(RSS.search(completed.stderr).group(1))
    return sample


def distribution(samples: list[dict], key: str) -> dict:
    raw = [sample[key] for sample in samples]
    median = statistics.median(raw)
    return {
        "raw": raw,
        "median": median,
        "mad": statistics.median(abs(value - median) for value in raw),
    }


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--baseline-revision", required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--candidate-revision", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("workloads", nargs="+", type=Path)
    args = parser.parse_args()
    result = {
        "schema_version": 2,
        "samples_per_revision": 7,
        "identity": {
            "baseline": {
                "revision": args.baseline_revision,
                "binary": str(args.baseline.resolve()),
                "sha256": sha256(args.baseline),
            },
            "candidate": {
                "revision": args.candidate_revision,
                "binary": str(args.candidate.resolve()),
                "sha256": sha256(args.candidate),
            },
        },
        "workloads": {},
    }
    metric_keys = (
        "wall_seconds",
        "peak_rss_bytes",
        "air_phase_ns",
        "allocations",
        "allocated_bytes",
    )
    for workload in args.workloads:
        run(args.baseline, workload)
        run(args.candidate, workload)
        samples = {"baseline": [], "candidate": []}
        collection_order = []
        for pair in range(7):
            order = ("baseline", "candidate") if pair % 2 == 0 else ("candidate", "baseline")
            collection_order.append(list(order))
            for revision in order:
                samples[revision].append(run(getattr(args, revision), workload))
        summary = {}
        for revision in ("baseline", "candidate"):
            summary[revision] = {
                key: distribution(samples[revision], key)
                for key in metric_keys
            }
        profiles = {
            revision: {
                key: value
                for key, value in samples[revision][0].items()
                if key not in metric_keys
            }
            for revision in ("baseline", "candidate")
        }
        result["workloads"][workload.name] = {
            "sha256": sha256(workload),
            "collection_order": collection_order,
            "profiles": profiles,
            "summary": summary,
        }
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
