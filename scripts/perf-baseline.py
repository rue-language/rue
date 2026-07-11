#!/usr/bin/env python3
"""
Per-pass compiler performance baseline harness (RUE-249).

Compiles a representative corpus with the real `rue` binary using
`--benchmark-json` (the machine-readable form of `--time-passes`, see
docs/process/logging.md), runs several fresh compiler processes per program,
and reports median compiler-span and process wall time plus per-pass timing.

This is the tool behind docs/process/perf-baseline.md: run it, read the
"hot passes", and re-run it after a "faster compilation" change to see
whether a pass got cheaper. It is intentionally separate from bench.sh
(which feeds the historical website dashboard): this one is a quick,
human-readable, self-contained snapshot with no history/network side effects.

## Timing model (important)

The compiler wraps each pass in a tracing span. Some spans are *nested*, so
the raw JSON contains both leaf passes and their aggregate parents:

    compile                         <- top-level compiler pipeline wall clock
    |- parse                        <- aggregate
    |  |- parse_file                <- aggregate, repeated for every file
    |  |  |- lexer                  <- leaf
    |  |  `- parser                 <- leaf
    |  |- definition_snapshot       <- leaf
    |  `- merge_symbols             <- leaf
    |- astgen                       <- leaf
    |- semantic_astgen              <- leaf
    |- sema                         <- leaf
    |- cfg_construction             <- leaf
    |- codegen                      <- leaf
    |- linker                       <- leaf

Schema v2 marks root and leaf invocations explicitly. `total_ms` is the
inclusive duration of root compiler spans; nested pass rows are also inclusive
and therefore overlap their parents. This harness reports only leaves in its
phase table. Leaf/root ratio, unattributed time, overlap, and driver overhead
are calculated within each sample before their medians are taken, so unrelated
samples are never combined into a fictional timing breakdown. It also measures
the fresh process around `subprocess.run`: that includes startup, source
discovery, and driver work outside the `compile` span.

In JSON phase rows, `median_percent` is the median of the per-sample
phase/root ratios. It is intentionally not the ratio between the independently
aggregated `median_ms` phase duration and compile median.

## Usage

    scripts/perf-baseline.py                     # 5 samples after 1 warmup
    scripts/perf-baseline.py --iterations 9
    scripts/perf-baseline.py --rue-bin /path/to/rue
    scripts/perf-baseline.py --format markdown   # compact median tables
    scripts/perf-baseline.py --format json       # machine-readable aggregate

By default the compiler is located via `scripts/rue-bin` (a normal build).
Pass `--release` to build/locate it with `--target-platforms //platforms:release`,
or `--rue-bin PATH` / the `RUE` env var to use an already-built binary.

Numbers are absolute milliseconds and are therefore MACHINE-SPECIFIC; treat
them as a relative profile (which passes dominate), not a hard guarantee.
"""

import argparse
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# Schema-v1 fallback for benchmark JSON produced before passes described their
# own nesting. Schema v2 uses invocation metadata instead of this name list.
AGGREGATE_PASSES = {"parse", "compile"}
WALL_CLOCK_PASS = "compile"

# Canonical leaf-pass order for deterministic, pipeline-ordered output. Any
# leaf the compiler emits that is not listed here is appended afterwards in
# first-seen order, so a newly instrumented pass still shows up.
CANONICAL_LEAF_ORDER = [
    "parse_file",  # schema-v1 combined lexer/parser leaf
    "lexer",
    "parser",
    "definition_snapshot",
    "merge_symbols",
    "astgen",
    "semantic_astgen",
    "sema",
    "cfg_construction",
    "codegen",
    "linker",
]


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def default_corpus(root: Path):
    """The representative corpus: (label, size-bucket, [files]).

    Small/medium come from examples/; the large programs are the generated
    stress suite (benchmarks/stress/, also used by bench.sh); the multi-file
    entry is synthesized at run time (see run_corpus).
    """
    ex = root / "examples"
    st = root / "benchmarks" / "stress"
    return [
        ("hello", "small", [ex / "hello.rue"]),
        ("fibonacci", "small", [ex / "fibonacci.rue"]),
        ("quicksort", "medium", [ex / "quicksort.rue"]),
        ("structs", "medium", [ex / "structs.rue"]),
        ("many_functions", "large", [st / "many_functions.rue"]),
        ("large_structs", "large", [st / "large_structs.rue"]),
        ("arithmetic_heavy", "large", [st / "arithmetic_heavy.rue"]),
        ("control_flow", "large", [st / "control_flow.rue"]),
        ("register_pressure", "large", [st / "register_pressure.rue"]),
        # NOTE: benchmarks/stress/deep_nesting.rue is deliberately NOT in the
        # default corpus. Its parser stage is superlinear in block-nesting
        # depth (lexing ~20ms, but `--emit ast` exceeds a minute), so it hangs
        # the harness. See docs/process/perf-baseline.md ("Known pathology").
        # Add it back once that is fixed, or run it directly with --timeout.
    ]


# A small, deterministic multi-file program written to a temp dir so the
# corpus exercises the multi-file merge path (merge_symbols) without depending
# on files that live in the tree.
MULTI_MAIN = """\
const a = @import("a.rue");
const b = @import("b.rue");

fn main() -> i64 {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 100 {
        total = total + a.square(i) + b.cube(i);
        i = i + 1;
    }
    total
}
"""

MULTI_A = """\
pub fn square(x: i64) -> i64 {
    x * x
}
"""

MULTI_B = """\
pub fn cube(x: i64) -> i64 {
    x * x * x
}
"""


def resolve_rue_bin(args, root: Path) -> str:
    if args.rue_bin:
        return args.rue_bin
    env = os.environ.get("RUE")
    if env:
        return env
    cmd = [str(root / "scripts" / "rue-bin")]
    if args.release:
        cmd += ["--target-platforms", "//platforms:release"]
    out = subprocess.run(cmd, capture_output=True, text=True)
    if out.returncode != 0:
        sys.stderr.write(out.stderr)
        sys.exit(f"perf-baseline: scripts/rue-bin failed: {out.stderr.strip()}")
    return out.stdout.strip()


def compile_once(rue_bin: str, files, out_path: str, timeout: float, jobs: int):
    """Run one fresh compiler process and return timing JSON plus process time."""
    cmd = (
        [rue_bin, "--benchmark-json", "--jobs", str(jobs), "-O0"]
        + [str(f) for f in files]
        + ["-o", out_path]
    )
    output = Path(out_path)
    output.unlink(missing_ok=True)
    child_env = os.environ.copy()
    # Developer logging configuration is not a compiler input and can add
    # substantial formatting/I/O work to otherwise identical samples.
    child_env.pop("RUST_LOG", None)
    started = time.perf_counter_ns()
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=child_env,
        )
    except subprocess.TimeoutExpired as e:
        output.unlink(missing_ok=True)
        raise RuntimeError(f"compile exceeded {timeout:.0f}s timeout") from e
    process_ms = (time.perf_counter_ns() - started) / 1_000_000
    if proc.returncode != 0:
        output.unlink(missing_ok=True)
        raise RuntimeError(
            f"compile failed ({' '.join(cmd)}):\n{proc.stderr}\n{proc.stdout}"
        )
    if not output.is_file():
        raise RuntimeError("compiler succeeded without producing its output artifact")
    try:
        result = json.loads(proc.stdout)
    except (json.JSONDecodeError, TypeError) as error:
        raise RuntimeError("compiler stdout was not exactly one benchmark JSON value") from error
    finally:
        output.unlink(missing_ok=True)
    if not isinstance(result, dict):
        raise RuntimeError("compiler benchmark JSON must be an object")
    result["_process_ms"] = process_ms
    return result


def median_and_mad(values):
    """Return median and median absolute deviation for numeric samples."""
    if not values:
        return 0.0, 0.0
    median = statistics.median(values)
    mad = statistics.median(abs(value - median) for value in values)
    return median, mad


def finite_nonnegative(value, label):
    """Validate and normalize one duration from machine-readable timing JSON."""
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
        or value < 0
    ):
        raise ValueError(f"{label} must be a finite non-negative number")
    return float(value)


def nonnegative_int(value, label, *, positive=False):
    """Validate an integer counter from machine-readable timing JSON."""
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError(f"{label} must be a non-negative integer")
    if positive and value == 0:
        raise ValueError(f"{label} must be greater than zero")
    return value


def validate_v2_pass_counters(pass_data, sample_number, name):
    """Validate the invocation metadata that defines v2 span nesting."""
    prefix = f"sample {sample_number} pass {name!r}"
    invocations = nonnegative_int(
        pass_data.get("invocations"), f"{prefix} invocations", positive=True
    )
    root_invocations = nonnegative_int(
        pass_data.get("root_invocations"), f"{prefix} root_invocations"
    )
    leaf_invocations = nonnegative_int(
        pass_data.get("leaf_invocations"), f"{prefix} leaf_invocations"
    )
    if root_invocations > invocations:
        raise ValueError(f"{prefix} root_invocations exceeds invocations")
    if leaf_invocations > invocations:
        raise ValueError(f"{prefix} leaf_invocations exceeds invocations")


def validate_source_metrics(source_metrics, sample_number, schema_version):
    """Validate workload-size metadata used for throughput comparisons."""
    label = f"sample {sample_number} source_metrics"
    if not isinstance(source_metrics, dict):
        if source_metrics is None and schema_version == 1:
            return
        raise ValueError(f"{label} must be an object")
    if schema_version == 2:
        nonnegative_int(
            source_metrics.get("files"), f"{label} files", positive=True
        )
        for field in ("bytes", "lines", "tokens"):
            nonnegative_int(source_metrics.get(field), f"{label} {field}")


def is_leaf_pass(pass_data, schema_version):
    """Use v2 nesting metadata, with a name-based fallback for v1 JSON."""
    if schema_version == 2:
        return pass_data["leaf_invocations"] == pass_data["invocations"]
    return pass_data.get("name") not in AGGREGATE_PASSES


def aggregate(samples):
    """Given a list of per-run JSON dicts, return the median per-pass timing.

    The result separates the compiler's root span from fresh-process wall time.
    Relationships between leaf work, the root, and process time are calculated
    per sample before aggregation; the independently reported pass medians are
    descriptive central tendencies, not an additive breakdown of a median run.
    """
    if not samples:
        return {
            "rows": [],
            "compile_ms": 0.0,
            "compile_mad_ms": 0.0,
            "process_ms": 0.0,
            "process_mad_ms": 0.0,
            "driver_overhead_ms": 0.0,
            "driver_overhead_mad_ms": 0.0,
            "leaf_to_root_ratio_percent": 0.0,
            "leaf_to_root_ratio_mad_percent": 0.0,
            "unattributed_ms": 0.0,
            "unattributed_mad_ms": 0.0,
            "overlapping_leaf_work_ms": 0.0,
            "overlapping_leaf_work_mad_ms": 0.0,
            "source_metrics": None,
            "target": None,
            "compiler_version": None,
            "peak_memory_bytes": None,
            "peak_memory_mad_bytes": None,
        }

    per_pass = {}
    order_seen = []
    compile_samples = []
    process_samples = []
    overhead_samples = []
    leaf_to_root_ratio_samples = []
    unattributed_samples = []
    overlapping_leaf_work_samples = []
    memory_samples = []
    memory_available = None
    source_metrics = None
    source_metrics_seen = False
    compiler_identity = None
    expected_leaf_names = None
    expected_schema_version = None

    for sample_index, sample in enumerate(samples):
        sample_number = sample_index + 1
        if not isinstance(sample, dict):
            raise ValueError(f"sample {sample_number} must be an object")
        schema_version = sample.get("schema_version", 1)
        if (
            isinstance(schema_version, bool)
            or not isinstance(schema_version, int)
            or schema_version not in (1, 2)
        ):
            raise ValueError(
                f"sample {sample_number} schema_version must be integer 1 or 2"
            )
        if expected_schema_version is None:
            expected_schema_version = schema_version
        elif schema_version != expected_schema_version:
            raise ValueError(
                f"sample {sample_index + 1} schema changed from "
                f"{expected_schema_version} to {schema_version}"
            )
        passes = sample.get("passes", [])
        if not isinstance(passes, list):
            raise ValueError(f"sample {sample_index + 1} passes must be a list")
        pass_ms = {}
        passes_by_name = {}
        for pass_index, pass_data in enumerate(passes):
            if not isinstance(pass_data, dict):
                raise ValueError(
                    f"sample {sample_index + 1} pass {pass_index + 1} must be an object"
                )
            name = pass_data.get("name")
            if not isinstance(name, str) or not name:
                raise ValueError(
                    f"sample {sample_index + 1} pass {pass_index + 1} has no name"
                )
            if name in pass_ms:
                raise ValueError(
                    f"sample {sample_index + 1} has duplicate pass name {name!r}"
                )
            pass_ms[name] = finite_nonnegative(
                pass_data.get("duration_ms"),
                f"sample {sample_index + 1} pass {name!r}",
            )
            passes_by_name[name] = pass_data
            if schema_version == 2:
                validate_v2_pass_counters(pass_data, sample_number, name)
        if WALL_CLOCK_PASS not in pass_ms:
            raise ValueError(f"sample {sample_index + 1} has no compile span")

        if schema_version == 2:
            if sample.get("timing_model") != "inclusive_spans":
                raise ValueError(
                    f"sample {sample_index + 1} has unknown timing model "
                    f"{sample.get('timing_model')!r}"
                )
            compile_ms = finite_nonnegative(
                sample.get("total_ms"), f"sample {sample_index + 1} total_ms"
            )
            if abs(compile_ms - pass_ms[WALL_CLOCK_PASS]) > 1e-6:
                raise ValueError(
                    f"sample {sample_index + 1} total_ms does not match compile span"
                )
            compile_pass = passes_by_name[WALL_CLOCK_PASS]
            if compile_pass["root_invocations"] != compile_pass["invocations"]:
                raise ValueError(
                    f"sample {sample_number} compile span must have every "
                    "invocation at the root"
                )
            if compile_pass["leaf_invocations"] != 0:
                raise ValueError(
                    f"sample {sample_number} compile span must not be a leaf"
                )
            other_roots = [
                name
                for name, pass_data in passes_by_name.items()
                if name != WALL_CLOCK_PASS and pass_data["root_invocations"] != 0
            ]
            if other_roots:
                raise ValueError(
                    f"sample {sample_number} compile span must be the sole root; "
                    f"other rooted pass(es): {other_roots}"
                )
        else:
            # V1's total_ms summed nested spans; its compile row is the only
            # trustworthy compiler-pipeline wall clock.
            compile_ms = pass_ms[WALL_CLOCK_PASS]

        leaf_names = [
            pass_data["name"]
            for pass_data in passes
            if is_leaf_pass(pass_data, schema_version)
        ]
        leaf_name_set = set(leaf_names)
        if expected_leaf_names is None:
            expected_leaf_names = leaf_name_set
            order_seen.extend(leaf_names)
            per_pass = {name: [] for name in leaf_names}
        elif leaf_name_set != expected_leaf_names:
            missing = sorted(expected_leaf_names - leaf_name_set)
            extra = sorted(leaf_name_set - expected_leaf_names)
            raise ValueError(
                f"sample {sample_index + 1} leaf pass set drifted; "
                f"missing={missing}, extra={extra}"
            )

        for name in per_pass:
            per_pass[name].append(pass_ms[name])

        leaf_work_ms = sum(pass_ms[name] for name in leaf_names)
        if compile_ms == 0 and leaf_work_ms > 0:
            raise ValueError(
                f"sample {sample_number} has positive leaf work with a zero "
                "compile span"
            )
        leaf_to_root_ratio_samples.append(
            leaf_work_ms / compile_ms * 100.0 if compile_ms > 0 else 0.0
        )
        unattributed_samples.append(max(compile_ms - leaf_work_ms, 0.0))
        overlapping_leaf_work_samples.append(max(leaf_work_ms - compile_ms, 0.0))

        process_ms = finite_nonnegative(
            sample.get("_process_ms", compile_ms),
            f"sample {sample_index + 1} process time",
        )
        if process_ms + 1e-6 < compile_ms:
            raise ValueError(
                f"sample {sample_index + 1} process time is shorter than compile span"
            )
        compile_samples.append(compile_ms)
        process_samples.append(process_ms)
        overhead_samples.append(max(process_ms - compile_ms, 0.0))

        peak_memory = sample.get("peak_memory_bytes")
        has_memory = peak_memory is not None
        if memory_available is None:
            memory_available = has_memory
        elif has_memory != memory_available:
            raise ValueError(f"sample {sample_index + 1} peak-memory availability drifted")
        if has_memory:
            memory_samples.append(
                finite_nonnegative(
                    peak_memory, f"sample {sample_index + 1} peak_memory_bytes"
                )
            )

        sample_source_metrics = sample.get("source_metrics")
        validate_source_metrics(sample_source_metrics, sample_number, schema_version)
        if not source_metrics_seen:
            source_metrics = sample_source_metrics
            source_metrics_seen = True
        elif sample_source_metrics != source_metrics:
            raise ValueError(f"sample {sample_index + 1} source metrics drifted")

        metadata = sample.get("metadata", {})
        if not isinstance(metadata, dict):
            raise ValueError(f"sample {sample_number} metadata must be an object")
        if schema_version == 2:
            for field in ("target", "version"):
                value = metadata.get(field)
                if not isinstance(value, str) or not value:
                    raise ValueError(
                        f"sample {sample_number} metadata {field} must be a "
                        "non-empty string"
                    )
            # Timestamp is provenance rather than compiler identity. Validate
            # its schema shape, but leave calendar/format policy to producers.
            timestamp = metadata.get("timestamp")
            if not isinstance(timestamp, str) or not timestamp:
                raise ValueError(
                    f"sample {sample_number} metadata timestamp must be a "
                    "non-empty string"
                )
        sample_identity = (metadata.get("target"), metadata.get("version"))
        if compiler_identity is None:
            compiler_identity = sample_identity
        elif sample_identity != compiler_identity:
            raise ValueError(f"sample {sample_index + 1} compiler identity drifted")

    compile_ms, compile_mad_ms = median_and_mad(compile_samples)
    process_ms, process_mad_ms = median_and_mad(process_samples)
    overhead_ms, overhead_mad_ms = median_and_mad(overhead_samples)
    leaf_to_root_ratio_percent, leaf_to_root_ratio_mad_percent = median_and_mad(
        leaf_to_root_ratio_samples
    )
    unattributed_ms, unattributed_mad_ms = median_and_mad(unattributed_samples)
    overlapping_leaf_work_ms, overlapping_leaf_work_mad_ms = median_and_mad(
        overlapping_leaf_work_samples
    )
    peak_memory_bytes, peak_memory_mad_bytes = median_and_mad(memory_samples)

    # Deterministic order: canonical first, then any extras in first-seen order.
    ordered = [n for n in CANONICAL_LEAF_ORDER if n in per_pass]
    ordered += [n for n in order_seen if n not in CANONICAL_LEAF_ORDER]

    rows = []
    for name in ordered:
        med = statistics.median(per_pass[name])
        pct = statistics.median(
            pass_ms / root_ms * 100.0 if root_ms > 0 else 0.0
            for pass_ms, root_ms in zip(per_pass[name], compile_samples)
        )
        rows.append((name, med, pct))
    return {
        "rows": rows,
        "compile_ms": compile_ms,
        "compile_mad_ms": compile_mad_ms,
        "process_ms": process_ms,
        "process_mad_ms": process_mad_ms,
        "driver_overhead_ms": overhead_ms,
        "driver_overhead_mad_ms": overhead_mad_ms,
        "leaf_to_root_ratio_percent": leaf_to_root_ratio_percent,
        "leaf_to_root_ratio_mad_percent": leaf_to_root_ratio_mad_percent,
        "unattributed_ms": unattributed_ms,
        "unattributed_mad_ms": unattributed_mad_ms,
        "overlapping_leaf_work_ms": overlapping_leaf_work_ms,
        "overlapping_leaf_work_mad_ms": overlapping_leaf_work_mad_ms,
        "source_metrics": source_metrics,
        "target": compiler_identity[0] if compiler_identity else None,
        "compiler_version": compiler_identity[1] if compiler_identity else None,
        "peak_memory_bytes": peak_memory_bytes if memory_samples else None,
        "peak_memory_mad_bytes": peak_memory_mad_bytes if memory_samples else None,
    }


def run_corpus(rue_bin, corpus, iterations, warmup, workdir, timeout, jobs):
    """Run every corpus program; return an ordered list of result dicts."""
    results = []
    out_bin = os.path.join(workdir, "perf_out")

    # Synthesize the multi-file program.
    multi_dir = os.path.join(workdir, "multi")
    os.makedirs(multi_dir, exist_ok=True)
    with open(os.path.join(multi_dir, "main.rue"), "w") as f:
        f.write(MULTI_MAIN)
    with open(os.path.join(multi_dir, "a.rue"), "w") as f:
        f.write(MULTI_A)
    with open(os.path.join(multi_dir, "b.rue"), "w") as f:
        f.write(MULTI_B)
    # Pass only the root. The compiler must discover the other two files from
    # the real import graph, so fresh-process time includes module discovery.
    multi_files = [os.path.join(multi_dir, "main.rue")]

    full = list(corpus) + [("multi_file", "multi", multi_files)]

    for label, bucket, files in full:
        missing = [f for f in files if not Path(f).exists()]
        if missing:
            raise RuntimeError(f"workload {label!r} is missing source file(s): {missing}")
        sys.stderr.write(f"  {label} ({bucket}) ... ")
        sys.stderr.flush()
        try:
            for _ in range(warmup):
                compile_once(rue_bin, files, out_bin, timeout, jobs)
            samples = [
                compile_once(rue_bin, files, out_bin, timeout, jobs)
                for _ in range(iterations)
            ]
            aggregated = aggregate(samples)
        except (RuntimeError, ValueError) as e:
            sys.stderr.write(f"FAILED\n{e}\n")
            raise RuntimeError(f"workload {label!r} failed") from e
        results.append(
            {
                "name": label,
                "bucket": bucket,
                "target": aggregated["target"],
                "compiler_version": aggregated["compiler_version"],
                # Keep wall_ms as a compatibility alias for generated tables.
                "wall_ms": aggregated["compile_ms"],
                "compile_ms": {
                    "median": aggregated["compile_ms"],
                    "mad": aggregated["compile_mad_ms"],
                },
                "process_ms": {
                    "median": aggregated["process_ms"],
                    "mad": aggregated["process_mad_ms"],
                },
                "driver_overhead_ms": {
                    "median": aggregated["driver_overhead_ms"],
                    "mad": aggregated["driver_overhead_mad_ms"],
                },
                "leaf_to_root_ratio_percent": {
                    "median": aggregated["leaf_to_root_ratio_percent"],
                    "mad": aggregated["leaf_to_root_ratio_mad_percent"],
                },
                "unattributed_ms": {
                    "median": aggregated["unattributed_ms"],
                    "mad": aggregated["unattributed_mad_ms"],
                },
                "overlapping_leaf_work_ms": {
                    "median": aggregated["overlapping_leaf_work_ms"],
                    "mad": aggregated["overlapping_leaf_work_mad_ms"],
                },
                "peak_memory_bytes": (
                    {
                        "median": aggregated["peak_memory_bytes"],
                        "mad": aggregated["peak_memory_mad_bytes"],
                    }
                    if aggregated["peak_memory_bytes"] is not None
                    else None
                ),
                "passes": [
                    {"name": n, "median_ms": ms, "median_percent": pct}
                    for n, ms, pct in aggregated["rows"]
                ],
                "source_metrics": aggregated["source_metrics"],
            }
        )
        sys.stderr.write(
            f"{aggregated['compile_ms']:.1f}ms compile / "
            f"{aggregated['process_ms']:.1f}ms process\n"
        )
    return results


def hot_passes(results, top=3):
    """Rank summed workload medians; percentages are descriptive, not additive."""
    totals = {}
    for r in results:
        for p in r["passes"]:
            totals[p["name"]] = totals.get(p["name"], 0.0) + p["median_ms"]
    grand = sum(result["compile_ms"]["median"] for result in results) or 1.0
    ranked = sorted(totals.items(), key=lambda kv: kv[1], reverse=True)
    return [(name, ms, ms / grand * 100.0) for name, ms in ranked]


def print_text(results, iterations):
    for r in results:
        print(f"\n{r['name']}  ({r['bucket']}, n={iterations} fresh processes, median)")
        sm = r["source_metrics"] or {}
        if sm:
            print(
                f"  source: {sm.get('files', '?')} files, "
                f"{sm.get('lines', '?')} lines, "
                f"{sm.get('bytes', '?')} bytes, {sm.get('tokens', '?')} tokens"
            )
        namew = max((len(p["name"]) for p in r["passes"]), default=8)
        for p in r["passes"]:
            print(
                f"  {p['name']:<{namew}}  {p['median_ms']:>8.2f} ms  "
                f"({p['median_percent']:>4.1f}% median phase/root)"
            )
        print(
            f"  {'COMPILE':<{namew}}  {r['compile_ms']['median']:>8.2f} ms  "
            f"(MAD {r['compile_ms']['mad']:.2f}, root span)"
        )
        print(
            f"  {'PROCESS':<{namew}}  {r['process_ms']['median']:>8.2f} ms  "
            f"(MAD {r['process_ms']['mad']:.2f}, fresh process)"
        )
        print(
            f"  {'DRIVER':<{namew}}  {r['driver_overhead_ms']['median']:>8.2f} ms  "
            f"(MAD {r['driver_overhead_ms']['mad']:.2f}, paired samples)"
        )
        leaf_ratio = r["leaf_to_root_ratio_percent"]
        print(
            f"  {'LEAF/ROOT':<{namew}}  {leaf_ratio['median']:>8.1f}%     "
            f"(MAD {leaf_ratio['mad']:.1f}, paired samples)"
        )
        unattributed = r["unattributed_ms"]
        print(
            f"  {'UNATTRIBUTED':<{namew}}  {unattributed['median']:>8.2f} ms  "
            f"(MAD {unattributed['mad']:.2f}, paired samples)"
        )
        overlap = r["overlapping_leaf_work_ms"]
        if overlap["median"] > 0 or overlap["mad"] > 0:
            print(
                f"  {'OVERLAP':<{namew}}  {overlap['median']:>8.2f} ms  "
                f"(MAD {overlap['mad']:.2f}, paired leaf work above root time)"
            )
        if r["peak_memory_bytes"] is not None:
            memory = r["peak_memory_bytes"]
            print(
                f"  peak RSS: {memory['median'] / (1024 * 1024):.2f} MiB "
                f"(MAD {memory['mad'] / (1024 * 1024):.2f} MiB)"
            )

    print(
        "\nHot passes across the whole corpus "
        "(ratio of workload medians; profiling only):"
    )
    for name, ms, pct in hot_passes(results):
        print(f"  {name:<18} {ms:>9.2f} ms  ({pct:>4.1f}%)")


def _md_table(results, bucket_filter=None):
    rows = [r for r in results if bucket_filter is None or r["bucket"] in bucket_filter]
    if not rows:
        return ""
    # Union of leaf pass names in canonical order.
    names = []
    for n in CANONICAL_LEAF_ORDER:
        if any(p["name"] == n for r in rows for p in r["passes"]):
            names.append(n)
    for result in rows:
        for pass_data in result["passes"]:
            if pass_data["name"] not in names:
                names.append(pass_data["name"])
    header = "| program | " + " | ".join(names) + " | **compile** | **process** |"
    sep = "|" + "|".join(["---"] * (len(names) + 3)) + "|"
    lines = [header, sep]
    for r in rows:
        pm = {p["name"]: p["median_ms"] for p in r["passes"]}
        cells = [f"{pm.get(n, 0.0):.2f}" for n in names]
        lines.append(
            f"| {r['name']} | "
            + " | ".join(cells)
            + f" | **{r['compile_ms']['median']:.2f}**"
            + f" | **{r['process_ms']['median']:.2f}** |"
        )
    return "\n".join(lines)


def print_markdown(results):
    print("<!-- generated by scripts/perf-baseline.py; ms are machine-specific -->\n")
    print("### Small / medium programs\n")
    print(_md_table(results, {"small", "medium", "multi"}))
    print("\n### Large (generated stress) programs\n")
    print(_md_table(results, {"large"}))
    print("\n### Hot passes across the corpus (ratio of workload medians)\n")
    print("| pass | total ms | share |")
    print("|---|---|---|")
    for name, ms, pct in hot_passes(results):
        print(f"| {name} | {ms:.2f} | {pct:.1f}% |")


def main():
    ap = argparse.ArgumentParser(description="Per-pass compiler perf baseline (RUE-249)")
    ap.add_argument(
        "--iterations", type=int, default=5, help="fresh process samples (default 5)"
    )
    ap.add_argument(
        "--warmup",
        type=int,
        default=1,
        help="host-cache warmup processes before sampling (default 1)",
    )
    ap.add_argument("--timeout", type=float, default=60.0, help="per-compile timeout seconds (default 60)")
    ap.add_argument(
        "--jobs",
        type=int,
        default=1,
        help="compiler jobs per fresh process (default 1; use 0 for auto)",
    )
    ap.add_argument("--rue-bin", help="path to an already-built rue binary")
    ap.add_argument("--release", action="store_true", help="locate the compiler via //platforms:release")
    ap.add_argument("--format", choices=["text", "markdown", "json"], default="text")
    args = ap.parse_args()
    if args.iterations < 1:
        ap.error("--iterations must be at least 1")
    if args.warmup < 0:
        ap.error("--warmup must be non-negative")
    if args.jobs < 0:
        ap.error("--jobs must be non-negative")
    if args.release and (args.rue_bin or os.environ.get("RUE")):
        ap.error("--release cannot be combined with --rue-bin or RUE")

    root = repo_root()
    rue_bin = resolve_rue_bin(args, root)
    if not (Path(rue_bin).exists() and os.access(rue_bin, os.X_OK)):
        sys.exit(f"perf-baseline: rue binary not usable: {rue_bin}")

    sys.stderr.write(f"perf-baseline: using {rue_bin}\n")
    sys.stderr.write(
        f"perf-baseline: {args.iterations} fresh processes per program "
        f"after {args.warmup} host-cache warmup(s), jobs={args.jobs}\n"
    )

    workdir = tempfile.mkdtemp(prefix="rue-perf-")
    try:
        try:
            results = run_corpus(
                rue_bin,
                default_corpus(root),
                args.iterations,
                args.warmup,
                workdir,
                args.timeout,
                args.jobs,
            )
        except RuntimeError as error:
            sys.exit(f"perf-baseline: {error}")
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    if not results:
        sys.exit("perf-baseline: no results collected")

    if args.format == "json":
        print(
            json.dumps(
                {
                    "schema_version": 2,
                    "measurement": {
                        "model": "fresh_process_warm_host",
                        "iterations": args.iterations,
                        "warmup_processes": args.warmup,
                        "jobs": args.jobs,
                        "compiler_profile": (
                            "external"
                            if args.rue_bin or os.environ.get("RUE")
                            else ("release" if args.release else "default")
                        ),
                        "program_opt_level": "O0",
                    },
                    "results": results,
                },
                indent=2,
            )
        )
    elif args.format == "markdown":
        print_markdown(results)
    else:
        print_text(results, args.iterations)


if __name__ == "__main__":
    main()
