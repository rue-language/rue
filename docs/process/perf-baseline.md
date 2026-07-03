# Compiler performance baseline

**Purpose.** Establish *where compile time goes today* so that any future
"faster compilation" work (RUE-249, and the `-O` policy in RUE-245) is driven
by data instead of guesswork. This is a measurement + tracking document, not a
set of guarantees.

**How to reproduce.** Run the harness from the repo root:

```bash
scripts/perf-baseline.py                 # text report (default corpus, 5 warm iters)
scripts/perf-baseline.py --iterations 7  # more iterations = steadier medians
scripts/perf-baseline.py --format markdown   # regenerates the tables below
scripts/perf-baseline.py --format json       # machine-readable aggregate
```

The harness compiles a representative corpus with the real `rue` binary using
`--benchmark-json` (the machine-readable form of `--time-passes`, see
[logging.md](logging.md)), runs several *warm* iterations, and reports the
**median** per-pass timing plus the end-to-end wall clock. It is separate from
`bench.sh` (which feeds the historical website dashboard); this one is a quick,
self-contained snapshot with no history/network side effects.

## Reading the numbers

The compiler wraps each pass in a tracing span, and some spans are **nested**,
so the raw timing JSON contains both leaf passes and their aggregate parents:

```
compile                 <- top-level span, ~= end-to-end wall clock
├─ parse                <- aggregate of the two parse leaves (excluded below)
│  ├─ parse_file        <- leaf: lex + parse, summed over all input files
│  └─ merge_symbols     <- leaf: cross-file symbol merge
├─ astgen               <- leaf: AST -> RIR
├─ sema                 <- leaf: type checking / AIR
├─ cfg_construction     <- leaf
├─ codegen              <- leaf: MIR lower + regalloc + emit
└─ linker               <- leaf: ELF link
```

> **Caveat about `--time-passes`.** Its printed **"Total"** line *sums every
> span*, so it double-counts the aggregate parents (`parse` + `compile`) and
> comes out ~2× the real wall clock. This document (and the harness) instead
> use the `compile` span as the wall-clock total and report each **leaf** pass
> as a percentage of it. When you read `--time-passes` output by hand, ignore
> the `parse` and `compile` rows and the inflated `Total`.

## Baseline (measured 2026-07-03)

**These are absolute milliseconds and are MACHINE-SPECIFIC — treat them as a
relative profile (which passes dominate), not a hard threshold.**

- Host: Intel Core i9-14900K, Linux x86-64 (WSL2), 32 logical CPUs.
- Build: the buck2 **DEFAULT** profile as produced by `scripts/rue-bin`
  (unoptimized — see "Build caveat" below). Numbers on an optimized build
  would be smaller, but the *pass profile* (parse-dominated) holds.
- Corpus: `examples/` (small/medium) + `benchmarks/stress/` (large) + a
  synthesized 3-file program (multi-file merge path). `deep_nesting.rue` is
  excluded — see "Known pathology".
- 7 warm iterations, median.

### Small / medium programs (ms)

| program | parse_file | merge_symbols | astgen | sema | cfg | codegen | linker | **total** |
|---|---|---|---|---|---|---|---|---|
| hello | 0.60 | 0.01 | 0.01 | 0.12 | 1.23 | 0.09 | 8.43 | **10.58** |
| fibonacci | 1.52 | 0.02 | 0.03 | 0.27 | 1.42 | 0.38 | 7.94 | **11.94** |
| quicksort | 2.45 | 0.03 | 0.04 | 0.44 | 1.61 | 1.22 | 8.29 | **14.33** |
| structs | 2.11 | 0.03 | 0.03 | 0.48 | 1.60 | 1.14 | 8.41 | **13.94** |
| multi_file | 1.72 | 0.02 | 0.02 | 0.23 | 1.45 | 0.36 | 8.17 | **12.30** |

### Large (generated stress) programs (ms)

| program | parse_file | merge_symbols | astgen | sema | cfg | codegen | linker | **total** |
|---|---|---|---|---|---|---|---|---|
| many_functions | 112.90 | 1.32 | 0.80 | 15.93 | 4.49 | 70.20 | 16.12 | **223.80** |
| large_structs | 132.10 | 2.12 | 1.33 | 36.11 | 4.83 | 79.49 | 14.52 | **273.60** |
| arithmetic_heavy | 179.36 | 3.15 | 2.45 | 54.50 | 4.22 | 49.97 | 15.80 | **312.92** |
| control_flow | 395.94 | 1.93 | 1.38 | 28.54 | 4.28 | 54.33 | 12.17 | **502.14** |
| register_pressure | 163.20 | 1.78 | 1.22 | 36.43 | 3.86 | 22.68 | 11.75 | **243.69** |

### Hot passes across the corpus (sum of per-program medians)

| pass | total ms | share |
|---|---|---|
| **parse_file** | 991.88 | **61.9%** |
| **codegen** | 279.84 | **17.5%** |
| **sema** | 173.05 | 10.8% |
| linker | 111.60 | 7.0% |
| cfg_construction | 29.00 | 1.8% |
| merge_symbols | 10.44 | 0.7% |
| astgen | 7.31 | 0.5% |

## Findings

1. **`parse_file` is the dominant cost — by a wide margin.** It is 62% of total
   corpus time and rises to **48–79% of a single large compile**. Lexing itself
   is cheap (measured ~20 ms for the 12k-line `deep_nesting.rue`); the cost is
   in the recursive-descent **parser** and AST construction. This is the first
   place to look for a speedup, and it is a good parallelization candidate
   (files already parse in parallel; the win now is per-file parser throughput).

2. **`codegen` is second (~17%)**, concentrated where there is a lot of code to
   emit (`many_functions`, `large_structs`). This covers MIR lowering, register
   allocation, and instruction emission together — worth splitting into finer
   spans before optimizing, so the hot sub-phase is visible.

3. **`sema` is third (~11%)** and scales with type/expression volume
   (`arithmetic_heavy`, `large_structs`, `register_pressure`).

4. **`linker` is a fixed ~8 ms floor.** On the small programs it is 60–80% of a
   sub-15 ms compile purely because of that fixed ELF-emit cost; it is
   negligible on large programs. It matters for edit-compile-run latency on
   small inputs but is not a throughput bottleneck.

5. **`astgen`, `merge_symbols`, `cfg_construction` are all <2%** — not worth
   optimizing at current program sizes.

### What would speed the hot passes up

- **parse_file:** profile the parser proper (lexing is already cheap). Reduce
  per-token allocation, avoid re-scanning, and fix the exponential blowup
  below (which is the extreme tail of "parsing is expensive"). Finer spans
  (lex vs. parse vs. RIR-build) would sharpen the target.
- **codegen:** add per-sub-phase spans (lower / regalloc / emit) to see which
  dominates before touching it; regalloc is the usual suspect under pressure.
- **sema:** watch for quadratic scans over symbols/types as programs grow.

## Known pathology: exponential parse time on nested blocks

`benchmarks/stress/deep_nesting.rue` is **excluded from the default corpus**
because the parser is **exponential in block-nesting depth**. Lexing is fine
(~20 ms), but `--emit ast` never completes in reasonable time. A controlled
measurement (plain `{ … }` blocks nested N deep, DEFAULT build, this host):

| nesting depth | parse (`--emit ast`) |
|---|---|
| 10 | 0.05 s |
| 12 | 0.11 s |
| 14 | 0.42 s |
| 16 | 1.73 s |
| 18 | 6.94 s |
| 20 | > 30 s (killed) |

Each added level roughly **doubles** parse time — i.e. **O(2^depth)**. That
means `deep_nesting.rue` (nests ~40 levels) cannot be parsed, and `bench.sh`
has almost certainly been silently skipping it (a failed benchmark iteration is
logged and dropped, and `validate-benchmark.py` only fails when *all*
benchmarks fail). This is a real parser bug, filed separately; it is out of
scope for this measurement-only change. Once fixed, add `deep_nesting` back to
`default_corpus()` in `scripts/perf-baseline.py`.

## Build profile (release vs. DEFAULT)

The numbers above may come from either the buck2 **DEFAULT** (unoptimized)
profile or the optimized **release** profile, depending on how the compiler was
built. The `-Copt-level=3 -Clto=thin` release flags live in
`toolchains/rust/BUCK` and are applied to `//crates/rue:rue` when it is built
through the **`//platforms:release` target platform**:

```bash
buck2 build //crates/rue:rue --target-platforms //platforms:release
scripts/rue-bin --target-platforms //platforms:release   # absolute path to it
```

`bench.sh` (and `scripts/perf-baseline.py --release`) build through
`//platforms:release`, so their "release" numbers now measure a genuinely
optimized binary — release is byte-distinct from and roughly 2x smaller than the
DEFAULT build.

Historical note (RUE-277): a bare `--modifier //constraints:release` was a
**no-op** — it left the configured-target hash unchanged, so debug and "release"
resolved to the same `buck-out/.../<hash>/rue` path and `bench.sh` measured an
unoptimized binary. The fix routes the opt-level constraint through a target
*platform* (`//platforms:{debug,release}`), which gives each mode a distinct
configuration the toolchain's `rustc_flags` select can actually see. A plain
`//crates/rue:rue` build (no `--target-platforms`) still uses the DEFAULT
unoptimized profile.

## Related

- RUE-245 — define what each `-O` level does (this baseline informs it).
- RUE-45 — release-mode CI (the build caveat above is a symptom).
- `docs/process/logging.md` — the wide-events instrumentation behind
  `--time-passes` / `--benchmark-json`.
