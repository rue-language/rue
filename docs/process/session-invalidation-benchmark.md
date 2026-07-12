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
before warmup starts. Each measured iteration creates fresh sessions and runs:
cold parse through semantic analysis, an exact no-op, a leaf body edit, a module
identity change, a failed syntax edit and recovery, then cold and reused stable
definition queries.

The JSON contains nanosecond wall-time observations and structural work for each
scenario. Treat structural counters as correctness gates: the driver aborts if
reuse and invalidation do not match the scenario. Wall time is observational;
compare repeated runs on the same otherwise-idle machine and do not impose a
regression threshold without collecting a stable baseline first. The existing
`bench.sh` history schema describes whole compiler invocations and binary output,
so these stateful session samples intentionally use a separate schema rather
than silently changing dashboard meaning.

The ordinary test suite runs only a four-module, single-iteration structural
smoke test through
`//crates/rue-compiler-session-bench:rue-compiler-session-bench-test`; the
128-module timing workload remains opt-in.
