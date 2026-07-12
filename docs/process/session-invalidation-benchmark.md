# Canonical frontend session invalidation benchmark

This opt-in benchmark characterizes `CanonicalFrontendSession` without starting
a compiler process per query. It uses generated source held in memory, performs
no filesystem discovery during measured updates, and stops before backend code
generation or linking.

Run the full deterministic workload with:

```sh
./buck2 run //crates/rue-compiler-session-bench:rue-compiler-session-bench -- \
  --modules 128 --warmup 3 --iterations 10 > session-invalidation.json
```

The defaults are the values shown above. Setup constructs every `SourceSnapshot`
before warmup starts. Each measured iteration creates fresh sessions and runs
cold parse through semantic analysis, an exact no-op, a reachable root body edit,
a module identity change, a failed syntax edit and recovery, then cold and reused
stable definition queries. The semantic cold path performs one declaration bind
and exports its durable baseline from that same bind. For supported named
declarations, the reachable root body edit installs durable declaration payloads
into a fresh AIR epoch, then analyzes current bodies and builds current CFGs. A
second persistent session measures cold, exact-noop, reachable-root-body-edit,
and module-identity-change dependency-manifest and semantic-invalidation-plan
workloads.

The JSON contains nanosecond wall-time observations and structural work for each
scenario. Treat structural counters as correctness gates: the driver aborts if
reuse and invalidation do not match the scenario. Wall time is observational;
compare repeated runs on the same otherwise-idle machine and do not impose a
regression threshold without collecting a stable baseline first. The existing
`bench.sh` history schema describes whole compiler invocations and binary output,
so these stateful session samples intentionally use a separate schema rather
than silently changing dashboard meaning.

Schema version 2 added dependency-manifest and invalidation-plan query counters,
separate manifest/plan timings, fingerprint comparisons, dependency/closure
visits, and result cardinalities. At that historical stage production graphs
were incomplete and plans conservatively reported full invalidation. Schema
version 3 exercises complete supported graphs: exact no-op and reachable-root
edits produce incremental plans with reusable declarations, while unsupported
surfaces and global semantic-input changes still fail closed. Planning itself
must run no RIR query or RIR instruction scan.

At N=128 the structural gates require the supported reachable-root-body-edit
workload to reuse and atomically install all 128 durable declaration records,
skip ordinary declaration resolution, and perform zero cache-population binds.
Cold compilation remains one bind. This is a measured declaration-resolution
saving, not whole-pipeline incremental compilation: canonical merge/RIR and
current body/CFG work still run. Constants, module values, function aliases,
generic named methods, and anonymous structural owners currently fail closed to
ordinary declaration resolution with zero partial installs. Persistent cache
formats and LSP consumers remain deliberately out of scope.

Schema version 3 adds the exact declaration-reuse ledger under
`semantic_work.declaration_reuse`: plans, durable comparisons and reuse,
skipped ordinary resolution, installs, fallbacks, semantic/index/shell epochs,
population exports, and fallback epochs. Cold and successful declaration-reuse
scenarios are hard-gated to one epoch/index/shell-predeclaration each. Cold
reports one export; reuse reports none and no fallback epoch. The legacy
`queries.durable_cache_population_bindings` value remains present and must be
zero.

The ordinary test suite runs only a four-module, single-iteration structural
smoke test through
`//crates/rue-compiler-session-bench:rue-compiler-session-bench-test`; the
128-module timing workload remains opt-in.

The
[body-analysis and CFG incrementality audit](../notes/body-analysis-cfg-incrementality-audit.md)
requires this ledger before body or CFG reuse is introduced. Counters describe
actual operations, and parallel CFG counters reduce deterministic per-function
values rather than timing-dependent shared state.

Schema version 4 adds value-only body and CFG work. Top-level
`semantic_work` records body attempts/successes/failures, AIR and local-string
production, string remapping, specialization scans/calls/unique and duplicate
requests/rewrites/rounds, and specialized-body attempts/successes/failures.
`semantic_work.cfg` records synthesized glue, functions considered and
filtered, CFG attempts/successes/failures, AIR consumed, optimization attempts
and completions (including non-O0 attempts), warnings, and implicit destructor
targets. Counts describe completed semantic records; failed requests cannot yet
be retained by the session record API and are tested at their owning phase.

Schema version 5 adds `manifest_work.body_owner_events_translated`,
`body_named_events_translated`, and `body_dependency_records_built`. The
manifest now publishes one ordered stable input record for each supported
ordinary body analyzed in the existing semantic pass, and the benchmark
hard-gates zero extra RIR traversal.
`semantic_work.body_dependency_air_instructions_observed` records the explicit
post-emission type observation needed for already-resolved inferred/comptime
nominal types; it is bounded by produced AIR and is not a second RIR pass.

Schema version 6 adds `semantic_work.body_owner_tokens`, with exact provisional,
authoritative, validated, installed, and failed-validation counts. Ordinary and
durable-fallback semantic epochs each receive a fresh issuer; the benchmark
continues to claim no AIR or CFG retention or reuse.
