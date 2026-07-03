#!/usr/bin/env python3
"""
Per-pass compiler performance baseline harness (RUE-249).

Compiles a representative corpus with the real `rue` binary using
`--benchmark-json` (the machine-readable form of `--time-passes`, see
docs/process/logging.md), runs several *warm* iterations per program, and
reports the median per-pass timing plus the end-to-end wall-clock total.

This is the tool behind docs/process/perf-baseline.md: run it, read the
"hot passes", and re-run it after a "faster compilation" change to see
whether a pass got cheaper. It is intentionally separate from bench.sh
(which feeds the historical website dashboard): this one is a quick,
human-readable, self-contained snapshot with no history/network side effects.

## Timing model (important)

The compiler wraps each pass in a tracing span. Some spans are *nested*, so
the raw JSON contains both leaf passes and their aggregate parents:

    compile                         <- top-level, ~= end-to-end wall clock
    |- parse                        <- aggregate of the two parse leaves
    |  |- parse_file                <- leaf (lex + parse, summed over files)
    |  |- merge_symbols             <- leaf
    |- astgen                       <- leaf
    |- sema                         <- leaf
    |- cfg_construction             <- leaf
    |- codegen                      <- leaf
    |- linker                       <- leaf

`--time-passes`'s printed "Total" line SUMS every span, so it double-counts
the aggregate parents (parse + compile) and is ~2x the real wall clock. This
harness instead uses the `compile` span as the wall-clock total and reports
each *leaf* pass as a percentage of it, which is the number you actually want
when deciding what to optimize.

## Usage

    scripts/perf-baseline.py                     # default corpus, 5 warm iters
    scripts/perf-baseline.py --iterations 9
    scripts/perf-baseline.py --rue-bin /path/to/rue
    scripts/perf-baseline.py --format markdown   # tables for the baseline doc
    scripts/perf-baseline.py --format json       # machine-readable aggregate

By default the compiler is located via `scripts/rue-bin` (a normal build).
Pass `--release` to build/locate it with `--target-platforms //platforms:release`,
or `--rue-bin PATH` / the `RUE` env var to use an already-built binary.

Numbers are absolute milliseconds and are therefore MACHINE-SPECIFIC; treat
them as a relative profile (which passes dominate), not a hard guarantee.
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

# Aggregate/parent spans to exclude from the per-leaf table. `compile` is the
# end-to-end wall clock; `parse` is just parse_file + merge_symbols.
AGGREGATE_PASSES = {"parse", "compile"}
WALL_CLOCK_PASS = "compile"

# Canonical leaf-pass order for deterministic, pipeline-ordered output. Any
# leaf the compiler emits that is not listed here is appended afterwards in
# first-seen order, so a newly instrumented pass still shows up.
CANONICAL_LEAF_ORDER = [
    "parse_file",
    "merge_symbols",
    "astgen",
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
fn main() -> i64 {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 100 {
        total = total + square(i) + cube(i);
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


def compile_once(rue_bin: str, files, out_path: str, timeout: float):
    """Run one compilation with --benchmark-json; return the parsed JSON."""
    cmd = [rue_bin, "--benchmark-json"] + [str(f) for f in files] + ["-o", out_path]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired as e:
        raise RuntimeError(f"compile exceeded {timeout:.0f}s timeout") from e
    if proc.returncode != 0:
        raise RuntimeError(
            f"compile failed ({' '.join(cmd)}):\n{proc.stderr}\n{proc.stdout}"
        )
    # --benchmark-json prints the JSON object on stdout as the last line.
    line = proc.stdout.strip().splitlines()[-1]
    return json.loads(line)


def aggregate(samples):
    """Given a list of per-run JSON dicts, return the median per-pass timing.

    Returns (leaf_rows, wall_ms, source_metrics) where leaf_rows is an ordered
    list of (name, median_ms, percent_of_wall).
    """
    # Collect per-pass duration lists.
    per_pass = {}
    order_seen = []
    wall_samples = []
    source_metrics = None
    for s in samples:
        pass_ms = {p["name"]: p["duration_ms"] for p in s.get("passes", [])}
        wall_samples.append(pass_ms.get(WALL_CLOCK_PASS, s.get("total_ms", 0.0)))
        if source_metrics is None:
            source_metrics = s.get("source_metrics")
        for name, ms in pass_ms.items():
            if name in AGGREGATE_PASSES:
                continue
            if name not in per_pass:
                per_pass[name] = []
                order_seen.append(name)
        for name in per_pass:
            per_pass[name].append(pass_ms.get(name, 0.0))

    wall_ms = statistics.median(wall_samples) if wall_samples else 0.0

    # Deterministic order: canonical first, then any extras in first-seen order.
    ordered = [n for n in CANONICAL_LEAF_ORDER if n in per_pass]
    ordered += [n for n in order_seen if n not in CANONICAL_LEAF_ORDER]

    rows = []
    for name in ordered:
        med = statistics.median(per_pass[name])
        pct = (med / wall_ms * 100.0) if wall_ms > 0 else 0.0
        rows.append((name, med, pct))
    return rows, wall_ms, source_metrics


def run_corpus(rue_bin, corpus, iterations, warmup, workdir, timeout):
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
    multi_files = [
        os.path.join(multi_dir, "main.rue"),
        os.path.join(multi_dir, "a.rue"),
        os.path.join(multi_dir, "b.rue"),
    ]

    full = list(corpus) + [("multi_file", "multi", multi_files)]

    for label, bucket, files in full:
        missing = [f for f in files if not Path(f).exists()]
        if missing:
            sys.stderr.write(f"  skip {label}: missing {missing}\n")
            continue
        sys.stderr.write(f"  {label} ({bucket}) ... ")
        sys.stderr.flush()
        try:
            for _ in range(warmup):
                compile_once(rue_bin, files, out_bin, timeout)
            samples = [
                compile_once(rue_bin, files, out_bin, timeout)
                for _ in range(iterations)
            ]
        except RuntimeError as e:
            sys.stderr.write(f"FAILED\n{e}\n")
            continue
        rows, wall_ms, sm = aggregate(samples)
        results.append(
            {
                "name": label,
                "bucket": bucket,
                "wall_ms": wall_ms,
                "passes": [
                    {"name": n, "median_ms": ms, "percent": pct} for n, ms, pct in rows
                ],
                "source_metrics": sm,
            }
        )
        sys.stderr.write(f"{wall_ms:.1f}ms\n")
    return results


def hot_passes(results, top=3):
    """Sum each leaf pass's median ms across the corpus; return sorted list."""
    totals = {}
    for r in results:
        for p in r["passes"]:
            totals[p["name"]] = totals.get(p["name"], 0.0) + p["median_ms"]
    grand = sum(totals.values()) or 1.0
    ranked = sorted(totals.items(), key=lambda kv: kv[1], reverse=True)
    return [(name, ms, ms / grand * 100.0) for name, ms in ranked]


def print_text(results, iterations):
    for r in results:
        print(f"\n{r['name']}  ({r['bucket']}, n={iterations} warm, median)")
        sm = r["source_metrics"] or {}
        if sm:
            print(
                f"  source: {sm.get('lines', '?')} lines, "
                f"{sm.get('bytes', '?')} bytes, {sm.get('tokens', '?')} tokens"
            )
        namew = max((len(p["name"]) for p in r["passes"]), default=8)
        for p in r["passes"]:
            print(
                f"  {p['name']:<{namew}}  {p['median_ms']:>8.2f} ms  "
                f"({p['percent']:>4.1f}%)"
            )
        print(f"  {'TOTAL':<{namew}}  {r['wall_ms']:>8.2f} ms  (compile span)")

    print("\nHot passes across the whole corpus (sum of medians):")
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
    header = "| program | " + " | ".join(names) + " | **total** |"
    sep = "|" + "|".join(["---"] * (len(names) + 2)) + "|"
    lines = [header, sep]
    for r in rows:
        pm = {p["name"]: p["median_ms"] for p in r["passes"]}
        cells = [f"{pm.get(n, 0.0):.2f}" for n in names]
        lines.append(
            f"| {r['name']} | " + " | ".join(cells) + f" | **{r['wall_ms']:.2f}** |"
        )
    return "\n".join(lines)


def print_markdown(results):
    print("<!-- generated by scripts/perf-baseline.py; ms are machine-specific -->\n")
    print("### Small / medium programs\n")
    print(_md_table(results, {"small", "medium", "multi"}))
    print("\n### Large (generated stress) programs\n")
    print(_md_table(results, {"large"}))
    print("\n### Hot passes across the corpus (sum of per-program medians)\n")
    print("| pass | total ms | share |")
    print("|---|---|---|")
    for name, ms, pct in hot_passes(results):
        print(f"| {name} | {ms:.2f} | {pct:.1f}% |")


def main():
    ap = argparse.ArgumentParser(description="Per-pass compiler perf baseline (RUE-249)")
    ap.add_argument("--iterations", type=int, default=5, help="warm iterations (default 5)")
    ap.add_argument("--warmup", type=int, default=1, help="warmup runs before timing (default 1)")
    ap.add_argument("--timeout", type=float, default=60.0, help="per-compile timeout seconds (default 60)")
    ap.add_argument("--rue-bin", help="path to an already-built rue binary")
    ap.add_argument("--release", action="store_true", help="locate the compiler via //platforms:release")
    ap.add_argument("--format", choices=["text", "markdown", "json"], default="text")
    args = ap.parse_args()

    root = repo_root()
    rue_bin = resolve_rue_bin(args, root)
    if not (Path(rue_bin).exists() and os.access(rue_bin, os.X_OK)):
        sys.exit(f"perf-baseline: rue binary not usable: {rue_bin}")

    sys.stderr.write(f"perf-baseline: using {rue_bin}\n")
    sys.stderr.write(f"perf-baseline: {args.iterations} warm iterations per program\n")

    workdir = tempfile.mkdtemp(prefix="rue-perf-")
    try:
        results = run_corpus(
            rue_bin,
            default_corpus(root),
            args.iterations,
            args.warmup,
            workdir,
            args.timeout,
        )
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    if not results:
        sys.exit("perf-baseline: no results collected")

    if args.format == "json":
        print(json.dumps({"iterations": args.iterations, "results": results}, indent=2))
    elif args.format == "markdown":
        print_markdown(results)
    else:
        print_text(results, args.iterations)


if __name__ == "__main__":
    main()
