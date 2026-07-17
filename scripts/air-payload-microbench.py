#!/usr/bin/env python3
"""Collect paired alternating AIR payload microbenchmark samples."""

import argparse
import hashlib
import json
import statistics
import subprocess
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--baseline-revision", required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--candidate-revision", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    def run(binary: Path) -> dict:
        return json.loads(
            subprocess.run(
                [str(binary), "--microbench"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        )["families"]

    run(args.baseline)
    run(args.candidate)
    samples = {"baseline": [], "candidate": []}
    collection_order = []
    for pair in range(7):
        order = ("baseline", "candidate") if pair % 2 == 0 else ("candidate", "baseline")
        collection_order.append(list(order))
        for revision in order:
            samples[revision].append(run(getattr(args, revision)))

    summaries = {}
    for revision in ("baseline", "candidate"):
        summaries[revision] = summarize(samples[revision])
    args.output.write_text(
        json.dumps(
            {
                "schema_version": 2,
                "samples_per_revision": 7,
                "collection_order": collection_order,
                "identity": {
                    "baseline": identity(args.baseline, args.baseline_revision),
                    "candidate": identity(args.candidate, args.candidate_revision),
                },
                "baseline": summaries["baseline"],
                "candidate": summaries["candidate"],
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )


def identity(binary: Path, revision: str) -> dict:
    return {
        "revision": revision,
        "binary": str(binary.resolve()),
        "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
    }


def summarize(samples: list[dict]) -> dict:
    summary = {}
    for family in samples[0]:
        summary[family] = {}
        for metric in (
            "build_ns",
            "build_allocations",
            "build_allocated_bytes",
            "traversal_ns",
            "traversal_allocations",
            "elements_per_second",
        ):
            raw = [sample[family][metric] for sample in samples]
            median = statistics.median(raw)
            summary[family][metric] = {
                "raw": raw,
                "median": median,
                "mad": statistics.median(abs(value - median) for value in raw),
            }
        summary[family]["profile"] = {
            key: value
            for key, value in samples[0][family].items()
            if key
            not in {
                "build_ns",
                "build_allocations",
                "build_allocated_bytes",
                "traversal_ns",
                "traversal_allocations",
                "elements_per_second",
            }
        }
    return {"schema_version": 1, "samples": len(samples), "families": summary}


if __name__ == "__main__":
    main()
