#!/usr/bin/env python3
"""Collect the representative cold-driver and reused-session scenarios."""

from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import subprocess
import tempfile

from benchmark_collection import parse_iteration_json
from benchmark_manifest import scenario_families


def _mean(samples: list[float]) -> float:
    return round(sum(samples) / len(samples), 3)


def collect_cold(rue: Path, root: Path, iterations: int) -> dict:
    samples, digest, size = [], None, None
    environment = os.environ.copy()
    environment["RUE_STD_PATH"] = str((root.parent / "std").resolve())
    with tempfile.TemporaryDirectory(prefix="rue-representative-") as directory:
        for index in range(iterations):
            output = Path(directory) / f"output-{index}"
            result = subprocess.run(
                [str(rue), "--benchmark-json", str(root), "-o", str(output)],
                check=True, text=True, capture_output=True, env=environment,
            )
            timing = parse_iteration_json(result.stdout)
            samples.append(float(timing["total_ms"]))
            emitted = timing.get("emitted_output", {})
            current_digest = emitted.get("sha256")
            current_size = emitted.get("size_bytes")
            if not isinstance(current_digest, str) or not isinstance(current_size, int):
                raise ValueError("cold driver did not report its raw emitted output")
            if digest is not None and (current_digest, current_size) != (digest, size):
                raise ValueError("cold scenario emitted output changed between iterations")
            digest, size = current_digest, current_size
    return {
        "name": "cold_batch_root_compiler", "samples_ms": samples,
        "mean_ms": _mean(samples),
        "emitted_output": {
            "parity": "byte_exact",
            "representation": "raw_compiler_executable_before_platform_postprocessing",
            "sha256": digest,
            "size_bytes": size,
        },
    }


def collect_reused(session_bench: Path, iterations: int) -> tuple[list[dict], dict]:
    result = subprocess.run(
        [str(session_bench), "--representative", "--warmup", "1", "--iterations", str(iterations)],
        check=True, text=True, capture_output=True,
    )
    payload = json.loads(result.stdout)
    if payload.get("schema_version") != 1 or payload.get("workload") != "representative_compiler_session_scenarios":
        raise ValueError("reused scenario runner returned an unknown schema")
    rows = payload.get("iterations")
    if not isinstance(rows, list) or len(rows) != iterations:
        raise ValueError("reused scenario runner returned the wrong iteration count")
    names = [entry["name"] for entry in rows[0]]
    collected = []
    for position, name in enumerate(names):
        scenarios = [iteration[position] for iteration in rows]
        if any(scenario.get("name") != name for scenario in scenarios):
            raise ValueError("reused scenario sequence changed between iterations")
        work = scenarios[0].get("required_vs_reused_work")
        if any(scenario.get("required_vs_reused_work") != work for scenario in scenarios[1:]):
            raise ValueError(f"structural work changed between iterations for {name}")
        samples = [scenario["wall_time_ns"] / 1_000_000 for scenario in scenarios]
        if any(not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0 for value in samples):
            raise ValueError(f"invalid reused timing sample for {name}")
        entry = {"name": name, "samples_ms": samples, "mean_ms": _mean(samples), "required_vs_reused_work": work}
        for evidence in ("differential_parity", "diagnostic_parity", "recovered_from_diagnostic_error"):
            if evidence in scenarios[0]:
                entry[evidence] = scenarios[0][evidence]
        collected.append(entry)
    no_change_parity = collected[0].get("differential_parity", {})
    emitted_output = {
        "sha256": no_change_parity.get("emitted_output_sha256"),
        "size_bytes": no_change_parity.get("emitted_output_size_bytes"),
    }
    return collected, emitted_output


def collect(manifest: Path, rue: Path, session_bench: Path, iterations: int) -> dict:
    families = scenario_families(manifest)
    root = manifest.parent / families[0]["fixture"]
    if families[0]["target"] != families[1]["target"]:
        raise ValueError("cold and reused scenario targets must match")
    cold = collect_cold(rue, root, iterations)
    reused, session_output = collect_reused(session_bench, iterations)
    cold_output = cold["emitted_output"]
    if (cold_output["sha256"], cold_output["size_bytes"]) != (
        session_output["sha256"], session_output["size_bytes"]
    ):
        raise ValueError(
            "real batch-driver output differs from fresh/reused CompilerSession output"
        )
    return {
        "schema_version": 1,
        "measurement_family": "compiler_build_and_query_performance",
        "representative": True,
        "generated_program_runtime": False,
        "cross_family_emitted_output": {
            "cold_batch_vs_fresh_and_reused_session": "byte_exact",
            "representation": "raw_compiler_executable_before_platform_postprocessing",
            "sha256": cold_output["sha256"],
            "size_bytes": cold_output["size_bytes"],
        },
        "iterations": iterations,
        "families": [
            {**{key: families[0][key] for key in ("name", "interpretation", "not_runtime")}, "scenarios": [cold]},
            {**{key: families[1][key] for key in ("name", "interpretation", "not_runtime")}, "scenarios": reused},
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--rue-bin", type=Path, required=True)
    parser.add_argument("--session-bench-bin", type=Path, required=True)
    parser.add_argument("--iterations", type=int, required=True)
    args = parser.parse_args()
    print(json.dumps(collect(args.manifest, args.rue_bin, args.session_bench_bin, args.iterations)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
