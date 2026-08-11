# Structured wait-label ownership

Status: current implementation audit for RUE-1349. ADR-0063 remains the
architectural authority.

## Why this boundary exists

ADR-0063 requires task-local dependency stacks, a cross-task wait graph,
deterministic cycle reporting, cancellation, and progress with one execution
permit. It does not require the wait graph to store presentation strings.
RUE-1348 showed that the previous representation formatted every registered
batch key before scheduling, even though successful builds never rendered a
wait-graph cycle.

The typed `K: QueryKey` remains the lookup authority. `stable_identity()` is a
presentation operation and may collide; it must not participate in cycle
detection or memo equality.

## Ownership and lifetime

| owner | state | lifetime |
| --- | --- | --- |
| `QueryContext::query_registered_batch` | one `Arc<RegisteredBatchItems<K>>` containing the family name, request ids, and typed keys | the complete batch call |
| worker queue | compact item indexes | until workers claim every item |
| child task | one request id and a cloned typed key for evaluation | that child evaluation |
| `RuntimeCore::wait_graph` | waiter task id → owner task id plus `WaitEdgeLabel` | from structured guard installation or join parking until its guard/wait ends |
| structured `WaitEdgeLabel` | shared batch-table `Arc` plus item index | exactly the corresponding wait edge |
| ordinary join `WaitEdgeLabel` | the memo node's already materialized `NodeIdentity` | exactly the corresponding wait edge |

The wait graph therefore keeps a structured label recoverable even when its
child is still queued or has already completed. It does not keep a second key
copy or a formatted string per edge.

```text
registered batch owns [K0, K1, ...]
  ├─ worker queue owns [0, 1, ...]
  └─ wait graph owns (shared batch table, index)
       └─ only on a detected cycle: stable_identity(K[index])
```

## Selected representation

Cycle detection and cycle presentation are separate:

1. `RuntimeCore` detects paths using only ordered `TaskId` edges.
2. An ordinary join edge reuses its memo node's existing display identity.
3. A structured edge stores an item index into the batch's shared typed table.
4. If inserting an edge closes a cycle, the runtime removes that tentative
   edge, releases the wait-graph mutex, and only then resolves the selected
   path's labels.
5. `canonical_cycle` still sorts and deduplicates the resulting
   `NodeIdentity` values, preserving the published diagnostic order and text.

Resolving after releasing the mutex keeps user-defined display formatting out
of the global critical section. Successful and contended-but-acyclic waits do
not invoke `stable_identity()` through this path.

## Alternatives considered

### Retain eager `NodeIdentity` edges

This preserved behavior but kept the measured serial formatting prefix and a
separate string allocation for every structured child. It was rejected because
the strings are not needed for detection and successful builds render no wait
cycles.

### Share one eagerly formatted identity with a future memo node

This could remove duplicate formatting on memo misses, but a scheduled child
does not necessarily create a node and the family memo store, not the batch,
owns node publication. It would couple scheduling to memo allocation and still
format every key before work begins.

### Store one type-erased key allocation per edge

This makes formatting lazy but adds an allocation and key clone per child. The
selected batch table already owns every typed key, so one shared table plus an
index is smaller and has a simpler lifetime.

## Progress, cancellation, and failure behavior

- All parent→child edges are installed before scheduling, as before. A queued
  child has no outgoing edge until it runs, but its label is already recoverable.
- The parent still donates its execution permit for the complete join interval.
  Worker selection, task ancestry, and one-/many-worker behavior are unchanged.
- True query dependency cycles still come from `Task::stack_cycle`; the wait
  graph continues to classify a cross-task scheduling loop as a declined join
  or a structured cycle at the same insertion points.
- `StructuredWaitGuard` removes every installed edge on success, cancellation,
  abort, or unwind. Removing the final edge releases the graph's last batch-table
  reference.
- A failed edge insertion removes its tentative edge before formatting. A panic
  in presentation therefore cannot leave a false wait dependency installed.

## Falsifiable evidence

The query-runtime tests require:

- successful batches to report zero structured-wait identity materializations;
- a cycle to recover exact canonical family/key text after the producer's
  external batch-table references have been dropped;
- dropping the structured guard to leave the wait graph empty;
- registered parent cycles to terminate with one and many execution permits.

The `structured_wait_materializations` and `structured_wait_bytes` counters now
measure only identities actually formatted to render a structured wait cycle.
The fresh-process scaling workloads are successful, so both counters should be
exactly zero. A nonzero value is evidence that a cycle path was rendered, not
that an edge was merely registered.

The four-workload same-host report confirmed that invariant twice per workload
with fixed one-worker structural probes:

| workload | materializations before → after | formatted bytes before → after |
| --- | ---: | ---: |
| Ruelex | 8,827 → 0 | 2,545,490 → 0 |
| Mosaic | 30,979 → 0 | 10,414,280 → 0 |
| Harbor | 70,671 → 0 | 28,402,213 → 0 |
| Lattice | 89,895 → 0 | 33,777,606 → 0 |

All non-display deterministic query counters were identical before and after.
A repeated timing pass moved compiler-root medians from 293.38, 809.73,
1,937.37 and 2,142.29 ms to 284.62, 809.45, 1,929.82 and 2,122.68 ms. These
are neutral-to-slightly-lower observations, not a speedup claim: each median has
only three fresh processes and the operating-system page cache is uncontrolled.
Peak RSS moved -0.1, -0.7, +0.5 and +4.3 MiB, which is likewise neutral.

## ADR classification

This is a representation refinement within ADR-0063's required wait graph. It
does not change query identity, cycle semantics, the execution budget, task
ownership, or artifact publication, so no replacement ADR is required.
