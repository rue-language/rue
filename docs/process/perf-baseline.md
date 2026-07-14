# Compiler performance baseline

**Purpose.** Establish *where compile time goes today* so that any future
"faster compilation" work (RUE-249, and the `-O` policy in RUE-245) is driven
by data instead of guesswork. This is a measurement + tracking document, not a
set of guarantees.

**How to reproduce.** Run the harness from the repo root:

```bash
scripts/perf-baseline.py                 # 5 fresh samples after 1 cache warmup
scripts/perf-baseline.py --iterations 7  # more iterations = steadier medians
scripts/perf-baseline.py --format markdown   # prints compact current tables
scripts/perf-baseline.py --format json       # machine-readable aggregate
```

The harness compiles a representative corpus with the real `rue` binary using
`--benchmark-json` (the machine-readable form of `--time-passes`, see
[logging.md](logging.md)). Every sample is a **fresh compiler process**; the
optional warmups warm host/filesystem caches, not compiler state. The harness
reports median absolute deviation alongside both the compiler pipeline span
and fresh-process wall time, plus peak resident memory where the host exposes
it. It is separate from `bench.sh` (which feeds the
historical website dashboard); this one is a quick, self-contained snapshot
with no history/network side effects.

Every workload invocation explicitly uses Rue program optimization level
`-O0`, so changes in the compiler's optimization policy do not silently change
the amount of generated-program optimization being measured. `--release`
selects an optimized *compiler binary*; it does not change that workload
`-O0`. The text and JSON formats include root/process/accounting MAD, the paired
accounting metrics, and peak RSS. Markdown deliberately stays compact: it
prints median phase, compile, and process times plus the hot-pass table, but
omits MAD and RSS. It prints new tables to stdout and does not rewrite the
historical tables in this document.

## Reading the numbers

The compiler wraps each pass in a tracing span, and some spans are **nested**,
so the raw timing JSON contains both leaf passes and their aggregate parents:

```
compile                 <- canonical discovery parsing + compiler work
├─ parse_file           <- aggregate, repeated for each canonical module
│  ├─ lexer             <- leaf
│  └─ parser            <- aggregate
│     ├─ parser_token_adaptation   <- leaf
│     ├─ parser_nesting_scan       <- leaf
│     ├─ parser_state_setup        <- leaf
│     ├─ parser_worker             <- thread-lifecycle aggregate
│     │  ├─ parser_graph_construction <- leaf
│     │  └─ parser_grammar_execution   <- leaf
│     │     (parser_error_conversion is a failure-only sibling leaf)
│     └─ parser_directive_validation <- leaf after grammar success
└─ compile_pipeline     <- post-discovery query/backend aggregate
   ├─ semantic_astgen   <- aggregate: canonical semantic AST -> RIR
   │  └─ definition_snapshot_modules <- durable module/name-candidate index
   ├─ rir_declaration_index <- leaf: snapshot-local RIR declaration candidates
   ├─ sema              <- leaf: type checking / AIR
   ├─ cfg_construction  <- leaf
   ├─ codegen           <- leaf: MIR lower + regalloc + emit
   └─ linker            <- leaf: built-in ELF link in these baseline runs
```

Since RUE-642, timing JSON schema v2 labels the model as `inclusive_spans`.
`total_ms` is the inclusive duration of root compiler spans, and pass
percentages use that honest total. Pass rows are themselves inclusive, so an
aggregate still overlaps its children. The harness uses `leaf_invocations` to
select phase rows. Each displayed phase duration is that phase's own median;
those independent medians are useful for profiling, but they are **not an
additive breakdown of one median run** and should not be summed to reconstruct
the compile median. In JSON, a phase's `median_percent` is separately computed
as the median of its per-sample phase/root ratios; it is not
`median_ms / compile median`.
The harness also rejects malformed v2 trees: `compile` must be the sole root
and must have children, every reported leaf must be nested beneath it, and the
source workload counters must be non-negative integers with at least one file.

For accounting, the harness instead computes four values within every sample
and only then reports their median and MAD: leaf/root ratio, unattributed time
(`max(root - leaf work, 0)`), overlapping leaf work
(`max(leaf work - root, 0)`), and driver overhead (`process - root`). Pairing
before aggregation prevents phase medians from different runs from inventing
overlap or residual time that occurred in no real run. Rue's current
coordinator-level leaves are sequential, so overlapping leaf work should be
zero today; if future parallel leaf spans overlap, their sum is work time and
may legitimately exceed root wall time.
`process_ms` additionally covers process startup, output handling, and other
driver work outside `compile`. For the filesystem CLI, `compile` includes the
stable reads, import resolution, and canonical parsing that produce the closed
discovery revision, as well as the later query/backend interval. Output-path
validation and output-file I/O remain outside it.
The corpus-wide hot-pass table is likewise a descriptive ratio of summed
per-workload medians, not the accounting for a single run.

> **Historical discontinuity.** Before RUE-642, `total_ms` summed every nested
> span and was commonly about twice the real compiler time. Old dashboard data
> retains that inflated raw `mean_ms`; the dashboard now prefers the stored
> `passes.compile.mean_ms` for those runs. The baseline harness had a name-based
> workaround and can still read schema-v1 samples. Any other comparison across
> the schema boundary must likewise use the old `compile` row rather than old
> `total_ms`.

> **Discovery-boundary discontinuity.** RUE-890 made the parser result produced
> during import discovery the exact canonical frontend artifact. RUE-892 moves
> that now-once-only parse under the `compile` root and introduces the
> `compile_pipeline` post-discovery aggregate. Comparisons from before this
> boundary omitted discovery parsing from the `compile` row even though they
> subsequently consumed its artifact.

## Baseline (measured 2026-07-03)

**These are absolute milliseconds and are MACHINE-SPECIFIC — treat them as a
relative profile (which passes dominate), not a hard threshold.**

This historical table predates schema v2, the canonical parsed-module frontend,
`definition_snapshot_modules`, and `rir_declaration_index`. It therefore shows
the retired symbol-merge span and the then-combined `parse_file` leaf rather
than separate `lexer` and `parser` rows. Its totals were already taken from the
old `compile` row, not the inflated old `total_ms`, and remain valid as
compiler-pipeline measurements.

- Host: Intel Core i9-14900K, Linux x86-64 (WSL2), 32 logical CPUs.
- Build: the buck2 **DEFAULT** profile as produced by `scripts/rue-bin`
  (unoptimized — see "Build caveat" below). Numbers on an optimized build
  would be smaller, but the *pass profile* (parse-dominated) holds.
- Corpus: `examples/` (small/medium) + `benchmarks/stress/` (large) + a
  synthesized 3-file import graph (discovery + multi-file merge paths).
  `deep_nesting.rue` is
  excluded — see "Known pathology".
- 7 fresh compiler processes after one host-cache warmup, median.

### Small / medium programs (ms)

| program | parse_file | symbol merge | astgen | sema | cfg | codegen | linker | **total** |
|---|---|---|---|---|---|---|---|---|
| hello | 0.60 | 0.01 | 0.01 | 0.12 | 1.23 | 0.09 | 8.43 | **10.58** |
| fibonacci | 1.52 | 0.02 | 0.03 | 0.27 | 1.42 | 0.38 | 7.94 | **11.94** |
| quicksort | 2.45 | 0.03 | 0.04 | 0.44 | 1.61 | 1.22 | 8.29 | **14.33** |
| structs | 2.11 | 0.03 | 0.03 | 0.48 | 1.60 | 1.14 | 8.41 | **13.94** |
| multi_file | 1.72 | 0.02 | 0.02 | 0.23 | 1.45 | 0.36 | 8.17 | **12.30** |

### Large (generated stress) programs (ms)

| program | parse_file | symbol merge | astgen | sema | cfg | codegen | linker | **total** |
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
| symbol merge | 10.44 | 0.7% |
| astgen | 7.31 | 0.5% |

## Findings

1. **`parse_file` is the dominant cost — by a wide margin.** It is 62% of total
   corpus time and rises to **48–79% of a single large compile**. Lexing itself
   is cheap (measured ~20 ms for the 12k-line `deep_nesting.rue`); the cost is
   in the recursive-descent **parser** and AST construction. This is the first
   place to look for a speedup. It is also a good future parallelization
   candidate, but files do **not** parse in parallel today: the compiler parses
   them sequentially with a shared interner. Per-file parser throughput is the
   immediate target; parse parallelism would also require interner merging and
   AST symbol remapping.

2. **`codegen` is second (~17%)**, concentrated where there is a lot of code to
   emit (`many_functions`, `large_structs`). This covers MIR lowering, register
   allocation, and instruction emission together — worth splitting into finer
   spans before optimizing, so the hot sub-phase is visible.

3. **`sema` is third (~11%)** and scales with type/expression volume
   (`arithmetic_heavy`, `large_structs`, `register_pressure`).

4. **Rue's built-in `linker` was a fixed ~8 ms floor on this host.** These
   measurements used Rue's internal ELF linker, not an external host/system
   linker. On the small programs it was 60–80% of a sub-15 ms compile; it was
   negligible on large programs. It matters for edit-compile-run latency on
   small inputs but is not a throughput bottleneck.

5. **The historical `astgen`, retired symbol-merge, and `cfg_construction`
   rows were all <2%** — they were not bottlenecks at these program sizes.

### What would speed the hot passes up

- **parser:** the now-separated lexer/parser rows confirm lexing is cheap.
  Profile parser internals, reduce per-token allocation and re-scanning, and
  fix the exponential blowup below (the extreme tail of "parsing is
  expensive"). Split parser sub-phases only where that profile shows another
  broad boundary hiding the cost.
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
means `deep_nesting.rue` (nests ~40 levels) cannot be parsed. `bench.sh` logs
and drops failed benchmark iterations, but result validation now rejects any
run that does not contain the manifest's exact benchmark set, so this failure
cannot silently enter benchmark history. This is a real parser bug, filed
separately; it is out of scope for this measurement-only change. Once fixed,
add `deep_nesting` back to `default_corpus()` in `scripts/perf-baseline.py`.

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
