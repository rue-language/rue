# Lattice

Lattice is a workflow-language compiler, static analyzer, deterministic resource
scheduler, replay engine, and reporting toolkit written entirely in Rue. It is
both a maintained example application and a compiler stress workload.

The source is intentionally broad rather than padded: more than 13,000 lines in
131 Rue modules exercise parsing, pooled strings, compact graph indices, nested
loops, ownership, generics from the standard library, deterministic simulation,
and a large cross-module call graph. The main subsystems are:

- a lexer and recursive-descent parser for the `.lattice` workflow DSL;
- semantic validation for names, ranges, resource capacity, and DAG structure;
- topological sorting, transitive closure and reduction, dominators, layering,
  path cardinality, critical-path timing, graph cuts, and lint rules;
- a multi-pool scheduler with CPU, memory, slot, priority, cache, failure,
  retry, dependency, and resource-wait behavior;
- independent event replay, capacity, serial-work, timing, resource, policy,
  query, forecast, and schedule-difference oracles;
- 48 domain-specific analysis passes and an operational insight portfolio;
- 32 deterministic reports including JSON, CSV, DOT, Mermaid, HTML, Markdown,
  SQL, JUnit, SARIF, Prometheus, Chrome Trace, and OpenTelemetry projections.

Run the built-in demonstration:

```console
scripts/rue exec examples/lattice/main.rue demo
```

Running without arguments prints the command reference, keeping the repository's
automatic example smoke test fast even when the complete CLI corpus runs in
parallel on a constrained CI worker.

Compile and schedule a workflow file:

```console
scripts/rue exec examples/lattice/main.rue run examples/lattice/demo.lattice
```

Exercise generated invariant and scaling workloads:

```console
scripts/rue exec examples/lattice/main.rue selftest
scripts/rue exec examples/lattice/main.rue stress1
scripts/rue exec examples/lattice/main.rue stress2
scripts/rue exec examples/lattice/main.rue stress4
scripts/rue exec examples/lattice/main.rue benchmark
```

Stress mode generates a connected workflow and runs all graph, schedule,
replay, resource, forecast, policy, lint, query, and serialization paths. Every
report is generated twice and compared by size and digest. `benchmark` repeats
the 1x, 2x, and 4x tiers to catch nondeterminism across the complete program.

## Workflow language

```text
workflow release;
pool local slots 2 cpu 4 memory 8;
task build pool local duration 5 cpu 2 memory 4 priority 20 retries 1 cache true fail 0;
task test pool local duration 3 cpu 1 memory 2 priority 10 retries 0 cache false fail 0;
after test build;
```

`after dependent prerequisite;` means the prerequisite must complete before the
dependent task can start. All numeric fields are unsigned integers. A nonzero
`fail` value selects the attempt that should fail; the task recovers only when
its retry budget permits another attempt.
