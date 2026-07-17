#!/usr/bin/env python3
"""Reproduce the RUE-843 17/10/10 typed-payload microbenchmark matrix."""

import argparse
import hashlib
import json
import statistics
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RIR_TEST = "every_payload_builder_records_per_family_allocation_and_storage_evidence"
CFG_TEST = "every_payload_builder_has_explicit_allocation_storage_and_staging_evidence"
AIR_FAMILIES = {
    "match_arms", "call_args", "type_args", "const_values", "intrinsic_args",
    "block_statements", "struct_fields", "source_order", "array_elements", "enum_payload",
}


def run(command):
    completed = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=True)
    return completed.stdout + completed.stderr


def owner_rows(target, test_name):
    output = run(["./buck2", "run", target, "--", "--nocapture", test_name])
    rows = []
    for line in output.splitlines():
        if not line.startswith("RUE843_FAMILY\t"):
            continue
        row = {}
        for field in line.split("\t")[1:]:
            key, value = field.split("=", 1)
            if key in {"phase", "family"}:
                row[key] = value
            elif "." in value:
                row[key] = float(value)
            else:
                row[key] = int(value)
        rows.append(row)
    return rows


def air_rows():
    output = run(["./buck2", "run", "//crates/rue-air-profile:rue-air-profile", "--", "--microbench"])
    payload = json.loads(next(line for line in reversed(output.splitlines()) if line.startswith("{")))
    rows = []
    for family, values in payload["families"].items():
        if family not in AIR_FAMILIES:
            continue
        rows.append({
            "phase": "AIR",
            "family": family,
            "elements": values["elements"],
            "build_ns": values["build_ns"],
            "build_elements_per_second": values["build_elements_per_second"],
            "build_allocations": values["build_allocations"],
            "build_allocated_bytes": values["build_allocated_bytes"],
            "traversal_ns": values["traversal_ns"],
            "traversal_elements_per_second": values["elements_per_second"],
            "traversal_allocations": values["traversal_allocations"],
            "logical_bytes": values["logical_bytes"],
            "capacity_bytes": values["capacity_bytes"],
            "total_bytes": values["total_bytes"],
            "envelopes": values["envelopes"],
            "peak_staging_bytes": values["peak_staging_bytes"],
        })
    return rows


def source_identity():
    paths = [
        "crates/rue-rir/src/inst.rs", "crates/rue-rir/src/inst/payload_support.rs",
        "crates/rue-air/src/inst.rs", "crates/rue-air/src/inst/payload_support.rs",
        "crates/rue-cfg/src/inst.rs", "crates/rue-cfg/src/inst/payload_metrics.rs",
        "crates/rue-cfg/src/payload.rs", "crates/rue-air-profile/src/main.rs",
        "scripts/payload-family-benchmark.py",
    ]
    digest = hashlib.sha256()
    for path in paths:
        digest.update(path.encode())
        digest.update((ROOT / path).read_bytes())
    return {
        "parent_revision": run(["git", "rev-parse", "HEAD"]).strip(),
        "benchmark_source_sha256": digest.hexdigest(),
        "files": paths,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    samples = []
    for index in range(args.samples):
        rows = owner_rows("//crates/rue-rir:rue-rir-test", RIR_TEST)
        rows += air_rows()
        rows += owner_rows("//crates/rue-cfg:rue-cfg-test", CFG_TEST)
        if len(rows) != 37:
            raise SystemExit(f"sample {index}: expected 37 rows, got {len(rows)}")
        samples.append(rows)
    grouped = {}
    for rows in samples:
        for row in rows:
            grouped.setdefault(f"{row['phase']}:{row['family']}", []).append(row)
    medians = {}
    for key, rows in grouped.items():
        medians[key] = {
            field: statistics.median(row[field] for row in rows)
            for field in rows[0]
            if field not in {"phase", "family"}
        }
    payload = {
        "schema_version": 1,
        "protocol": "seven fresh-process samples; typed builder and complete typed-view traversal",
        "source_identity": source_identity(),
        "samples": samples,
        "medians": medians,
    }
    Path(args.output).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
