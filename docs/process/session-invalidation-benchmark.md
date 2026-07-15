# Compiler session invalidation benchmark

This opt-in benchmark is the canonical structural workload for
`CompilerSession`. It runs entirely in process over owned `SourceSnapshot`
values. Its measured semantic scenarios stop before backend emission; bounded
cold-versus-reused parity checks additionally run the canonical backend and
linker and require byte-identical executables.

Run the deterministic N=128 workload with:

```sh
./buck2 run //crates/rue-compiler-session-bench:rue-compiler-session-bench -- \
  --modules 128 --warmup 3 --iterations 10 > session-invalidation.json
```

The historical `--modules` spelling is retained for command compatibility. In
the RUE-720 completion scenarios N is the exact number of reachable analyzed
bodies: `main` plus N-1 generated functions. One additional unreachable
function is present solely to make an unrelated source edit execute the
semantic query without changing the reachable program.

## Contract

Every iteration runs the original parse, declaration, diagnostic-retention,
dependency-manifest, and invalidation-plan scenarios, followed by these
supported body/CFG scenarios:

- `completion_cold_n`: one cold analysis of exactly N reachable bodies;
- `completion_exact_noop`: whole-query reuse with no semantic execution;
- `completion_unrelated_edit`: all N body and CFG artifacts import;
- `completion_changed_reachable_body`: the edited leaf and its direct reverse
  caller are reanalyzed while the other N-2 bodies import;
- `completion_reverse_closure`: an N-dependent call chain proves exact
  transitive reverse invalidation;
- `completion_cfg_o0` and `completion_cfg_o1`: all N CFGs import and perform no
  construction or optimization at either optimization level;
- `completion_specialization_reuse`: stable specialization identity imports a
  specialized body and its CFG after an unrelated edit;
- `completion_failed_semantic` and `completion_failure_recovery`: a failed
  request is retained as work evidence but cannot replace the last-good body or
  CFG baseline.

The executable aborts on any structural mismatch. Wall time is observational;
compare it only across repeated runs on an otherwise idle machine. The
structural counters, not elapsed time, are the correctness gates.

For every reused completion scenario, a fresh session compiles the same source.
The benchmark exact-compares the public canonical semantic/CFG artifacts,
ordered type-pool entries, strings, warnings, stable definition keys,
specialized durable-body payloads, body-owner and dependency records, retained
diagnostics, and every non-work field exposed by the stable dependency manifest,
including its durable ordinary bodies. The session-private retained CFG cache
is not exposed as a comparison API; parity for it is established through the
public remapped CFGs, exact import counters, and emitted output rather than an
inaccessible cache-object claim. The benchmark closes the same canonical
import-discovery revision in each session and requires byte-identical emitted
executables and equal backend warnings. Successful comparisons are recorded as
`differential_parity`; counter expectations remain scenario-specific because
reuse is supposed to perform less work than cold compilation. Each parity
record also serializes the fresh session's complete `cold_semantic_work` beside
the scenario's reused `semantic_work`, and both sides are covered by exact
structural gates. Exact no-op runs the same differential parity check even
though its semantic output is a whole-query reuse.

## Work ledger

Schema version 11 contains the complete body and CFG ledgers. Every field is a
direct operation count reduced deterministically from value-owned records;
none is inferred from timing.

`semantic_work.durable_bodies` contains:

- candidate comparisons and fallbacks;
- stable specialization-map attempts, successes, and failures;
- export attempts, successes, rejections, and exported instruction/place/string
  counts;
- durable conversions, completions, failures, and stable-key joins;
- finalization and projection attempts, completions, failures, and projected
  instruction/place/string counts;
- atomic import attempts, successes, failures, installed entities, and atomic
  discards;
- bodies reused and ordinary body analyses skipped.

`semantic_work.cfg` contains:

- drop-glue synthesis, functions considered and comptime functions filtered;
- CFG builds attempted/succeeded/failed and AIR instructions consumed;
- optimization attempts/completions and non-O0 level attempts;
- warnings and implicit destructor targets emitted;
- reuse candidates, import attempts/successes/failures, reuses, and fallbacks;
- warnings and implicit destructor targets reused;
- durable CFG export attempts/successes/rejections.

All body, specialization, and CFG fields include work performed by failed
semantic requests. Attempts are incremented before their fallible operation.
Parallel CFG builders return per-function values which are reduced in canonical
machine-symbol order.

The cold N scenario requires exactly N body analyses, N durable body exports,
and N CFG builds. It also requires exactly one declaration-cache population
export from that same semantic epoch, proving that cold cache population does
not run a duplicate binder or body-analysis pass. Exact no-op requires query
reuse and zero semantic execution. Unrelated edit requires N imported bodies,
N skipped analyses, N imported CFGs, zero body fallback, zero CFG build, and
zero optimization.

## Schema history and test coverage

Schemas 2-3 introduced manifest/planner work and declaration reuse. Schema 4
added body, specialization, and CFG work. Schemas 5-7 added body dependency
records, stable owner tokens, and durable-body conversion/import accounting.
Schema 8 added bounded diagnostic/manifest/plan retention. Schema 9 removed an
obsolete population-binding field. Schema 10 added failed-request phase and
partial-work accounting. Schema 11 adds the RUE-720 completion workloads, the
remaining body and CFG counters, exact structural reuse gates, and bounded
cold-versus-reused artifact/diagnostic/executable parity.

The ordinary suite runs an N=4 structural smoke test through
`//crates/rue-compiler-session-bench:rue-compiler-session-bench-test`. The full
N=128 timing workload remains opt-in, but it executes the same assertions.

This benchmark is deliberately bounded to synthetic, supported programs.
[RUE-813](https://linear.app/steve-klabnik/issue/RUE-813) tracks a reusable,
broader differential oracle. [RUE-901](https://linear.app/steve-klabnik/issue/RUE-901)
tracks realistic multi-module cold/reused projects and performance baselines.
Neither gap weakens the exact structural completion evidence recorded here.
