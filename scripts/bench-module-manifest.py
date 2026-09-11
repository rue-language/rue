#!/usr/bin/env python3
"""Compare fresh compiler invocations over explicit and discovered import graphs.

These fixtures isolate import acquisition: only main is a semantic body root.
OS file caches are warm; every sample starts a new compiler process/session.
Retained-session parity and edit reuse are covered by the host tests separately.
"""
import argparse
import hashlib
import json
import platform
from pathlib import Path
import re
import statistics
import subprocess
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixture(directory, shape, helpers):
    directory.mkdir(parents=True, exist_ok=True)
    names = ["m{:05d}.rue".format(i) for i in range(helpers)]
    order = list(reversed(names)) if shape == "deep_reverse" else names
    if shape == "wide":
        root = "".join('const m{} = @import("{}");\n'.format(i, n) for i, n in enumerate(order))
    else:
        root = 'const next = @import("{}");\n'.format(order[0])
    (directory / "main.rue").write_text(root + "fn main() -> i32 { 42 }\n")
    for i, name in enumerate(order):
        source = ""
        if shape != "wide" and i + 1 < len(order):
            source = 'const next = @import("{}");\n'.format(order[i + 1])
        (directory / name).write_text(source + "pub fn value() -> i32 { 42 }\n")


def invoke(binary, directory, args, label):
    start = time.perf_counter_ns()
    result = subprocess.run([binary] + args, cwd=directory, capture_output=True, text=True, timeout=180)
    elapsed = (time.perf_counter_ns() - start) / 1_000_000
    (directory / (label + ".stdout")).write_text(result.stdout)
    (directory / (label + ".stderr")).write_text(result.stderr)
    if result.returncode:
        raise RuntimeError("{}: exit {}: {}".format(label, result.returncode, result.stderr[:2500]))
    return result.stdout, elapsed


def measure(binary, directory, mode, workers, label):
    args = ["--daemon=off", "--time-passes", "-j{}".format(workers)]
    if mode != "filesystem":
        args += ["--module-manifest", "permuted.json" if mode == "permuted" else "modules.json"]
    args += ["main.rue", "-o", label + ".program"]
    _, wall_ms = invoke(binary, directory, args, label)
    stderr = (directory / (label + ".stderr")).read_text()
    passes = {}
    for name, value in re.findall(r"^\s+([A-Za-z_]+):\s+([0-9.]+)ms", stderr, re.MULTILINE):
        passes[name.lower()] = {"name": name.lower(), "duration_ms": float(value)}
    if "compile" not in passes:
        raise RuntimeError("timing report has no compiler root")
    source_inputs = [{"path": p.name, "sha256": digest(p)} for p in sorted(directory.glob("*.rue"))]
    manifest_path = directory / ("permuted.json" if mode == "permuted" else "modules.json")
    row = {
        "mode": mode,
        "workers": workers,
        "wall_ms": wall_ms,
        "compiler_total_ms": passes["compile"]["duration_ms"],
        "output_sha256": digest(directory / (label + ".program")),
        "source_inputs": source_inputs,
        "manifest_sha256": digest(manifest_path) if mode != "filesystem" else None,
        "passes": {name: passes.get(name) for name in ["source_loading", "import_discovery_round", "import_discovery_close", "parse_file", "parse_query_key", "parse_query_commit"]},
    }
    executed = subprocess.run(
        [str((directory / (label + ".program")).resolve())],
        capture_output=True, timeout=10,
    )
    if executed.returncode != 42:
        raise RuntimeError("{}: executable returned {}, expected 42".format(label, executed.returncode))
    row["program_exit"] = executed.returncode
    return row


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("compiler", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sizes", type=int, nargs="+", default=[32, 128, 512])
    parser.add_argument("--workers", type=int, nargs="+", default=[1, 4])
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--allow-debug", action="store_true")
    options = parser.parse_args()
    if options.repeats < 1 or any(value < 1 for value in options.sizes + options.workers):
        parser.error("sizes, workers, and repeats must be positive integers")
    options.output = options.output.resolve()
    if options.output.exists() and any(options.output.iterdir()):
        parser.error("output directory must be empty so stale inputs cannot contaminate measurements")
    binary = str(options.compiler.resolve())
    options.output.mkdir(parents=True, exist_ok=True)
    metadata = {
        "compiler": binary, "compiler_sha256": digest(options.compiler),
        "host": platform.platform(), "workers": options.workers, "repeats": options.repeats,
        "description": __doc__, "sizes_are_helper_counts": True,
        "timing_precision": "process monotonic nanoseconds; --time-passes inclusive rows rounded to 0.1 ms",
        "contract": "external prototype measurement, not a fresh_source_to_native_v1 manifest-mode observation",
    }
    profile_directory = options.output / "profile"
    fixture(profile_directory, "wide", 1)
    profile_json, _ = invoke(binary, profile_directory, ["--daemon=off", "--benchmark-json", "-j1", "main.rue", "-o", "profile.program"], "ordinary-profile")
    metadata["compiler_configuration"] = json.loads(profile_json)["compiler_boundary"]["configuration"]
    if metadata["compiler_configuration"]["compiler_build_profile"] != "release_thin_lto" and not options.allow_debug:
        raise RuntimeError("final measurements require a release compiler")
    (options.output / "metadata.json").write_text(json.dumps(metadata, indent=2))
    summaries = []
    with (options.output / "samples.jsonl").open("w") as samples:
        for shape in ["deep_forward", "deep_reverse", "wide"]:
            for size in options.sizes:
                directory = options.output / (shape + "-" + str(size))
                fixture(directory, shape, size)
                generation = []
                encoded = None
                for repeat in range(options.repeats):
                    stdout, wall_ms = invoke(binary, directory, ["--daemon=off", "-j1", "--emit", "module-manifest", "main.rue"], "generate-{}".format(repeat))
                    generation.append(wall_ms)
                    manifest = json.loads(stdout)
                    if encoded is not None and manifest != encoded:
                        raise RuntimeError("manifest generation is nondeterministic")
                    encoded = manifest
                    samples.write(json.dumps({
                        "mode": "generation", "shape": shape, "modules": size + 1,
                        "workers": 1, "repeat": repeat, "wall_ms": wall_ms,
                        "manifest_sha256": hashlib.sha256(stdout.encode("utf-8")).hexdigest(),
                    }) + "\n")
                    samples.flush()
                (directory / "modules.json").write_text(json.dumps(encoded))
                permuted = dict(encoded)
                for key in ["modules", "imports", "std_requirements"]:
                    permuted[key] = list(reversed(permuted[key]))
                (directory / "permuted.json").write_text(json.dumps(permuted))
                for workers in options.workers:
                    rows = []
                    expected = None
                    for repeat in range(options.repeats):
                        modes = ["filesystem", "manifest"] if repeat % 2 == 0 else ["manifest", "filesystem"]
                        for mode in modes:
                            label = "{}-j{}-{}".format(mode, workers, repeat)
                            row = measure(binary, directory, mode, workers, label)
                            row.update(shape=shape, modules=size + 1, repeat=repeat)
                            identity = (row["output_sha256"], row["source_inputs"])
                            if expected is not None and identity != expected:
                                raise RuntimeError("fresh explicit/discovered executable or source-input mismatch")
                            expected = identity
                            samples.write(json.dumps(row) + "\n"); samples.flush()
                            rows.append(row)
                    check = measure(binary, directory, "permuted", workers, "permuted-j{}".format(workers))
                    check.update(shape=shape, modules=size + 1)
                    samples.write(json.dumps(check) + "\n")
                    samples.flush()
                    if (check["output_sha256"], check["source_inputs"]) != expected:
                        raise RuntimeError("manifest input-order permutation changes output or source inputs")
                    summary = {"shape": shape, "modules": size + 1, "workers": workers, "generation_wall_ms_median": statistics.median(generation), "permutation_equal": True}
                    for mode in ["filesystem", "manifest"]:
                        selected = [row for row in rows if row["mode"] == mode]
                        summary[mode] = {"wall_ms_median": statistics.median(row["wall_ms"] for row in selected), "compiler_total_ms_median": statistics.median(row["compiler_total_ms"] for row in selected)}
                        source_rows = [row["passes"]["source_loading"] for row in selected]
                        summary[mode]["source_loading_ms_median"] = statistics.median(row["duration_ms"] for row in source_rows) if all(source_rows) else None
                    summaries.append(summary)
                    (options.output / "summary.json").write_text(json.dumps(summaries, indent=2))
                    print(json.dumps(summary), flush=True)


if __name__ == "__main__":
    main()
