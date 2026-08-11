# Post-ADR-0063 cold compiler architecture audit

Status: current implementation audit, 2026-08-10. This note records the
implemented compiler architecture after ADR-0063 and the cold-performance work
through RUE-1348. Current source and tests are authoritative; issue descriptions
and the two pre-ADR completion audits are historical context only.

## Decision status

[ADR-0063](../designs/0063-parallel-demand-driven-incremental-compilation.md)
remains the architectural authority. Its live decisions still match the source:

- compilation is a revisioned graph of typed, memoized queries;
- requests pin immutable input revisions;
- stable semantic identities and immutable canonical artifacts cross query
  boundaries;
- one runtime owns query concurrency, joining, cycle detection, validation, and
  retention;
- CFG, optimization, code generation, and object projection are per function;
- linking is deliberately fresh behind `ProgramImagePlan`.

No implemented decision contradicts or replaces ADR-0063, so this audit does
not create a new ADR. A new ADR would be appropriate for a changed decision,
such as stateful incremental linking or a different terminal codegen boundary,
not for updated Rust names or a more precise implementation map.

## Authoritative cold path

The one-shot entry point is a thin owner of a normal session:

```text
compile_snapshot()
  -> CompilerSession::update_for_presentation()
  -> CompilerSession::rooted_codegen()
       -> rooted body graph
       -> per-function Cfg / OptimizedCfg batch
       -> per-function CodegenUnit batch
       -> per-function ObjectProjection batch
       -> publish exact backend root
  -> ProgramImage::from_rooted()
  -> ProgramImage::fresh_link()
```

The production call chain is in `crates/rue-compiler/src/queries.rs`,
`crates/rue-compiler/src/session.rs`, and
`crates/rue-compiler/src/program_image_plan.rs`. `compile_snapshot()` does not
own a peer frontend or phase machine. Long-lived hosts use the same rooted
session operations and compiler-issued continuation values.

## Ownership and sharing

| Owner | Mutable state | Shared with query workers | Lifetime |
| --- | --- | --- | --- |
| `CompilerSession` | host/discovery continuation, current presentation publication, diagnostics and measurement projection | no; the coordinator is used through `&mut self` | one-shot compile or retained host session |
| `RevisionedQueryDatabase` | revision publication, input stores, family handles, retained semantic/backend roots | family handles and input stores point into the shared runtime | session |
| `QueryRuntime` / `RuntimeCore` | immutable revision store, permit budget, wait graph, node registry, retention registry and metrics | yes, through one `Arc<RuntimeCore>` | database |
| `QueryFamily<K, V>` | 32 sharded typed-key memo maps and one family retention queue | yes | database |
| memo node | retained attempts, validation certificate, wait cell and exact monotonic incarnation | yes; one mutex protects one logical key's state | key incarnation |
| query task | pinned revision, dependency/input observations, leases, validation proof scopes and nested-attempt ledger | child tasks receive or absorb explicit state; it is not a global mutable stack | one root or batch child |
| query terminal | immutable value, stamp, exact revision, observations and stable node identity | yes, by `Arc` | bounded retention plus active roots/leases |
| `ProgramImage` | ordered rooted object terminals, export thunks and link plan | no query mutation; it consumes already-published terminals | one final link |

This is not a coarse “shared compiler behind locks.” The coordinator remains
serial, while the data needed by independent compiler work lives in immutable
revisions and per-key query nodes. Shared synchronization is concentrated in:

- one execution-permit budget;
- the cross-task wait graph used for cycle detection;
- revision, node-incarnation and retention registries;
- one sharded memo index per query family; and
- one mutex per logical memo node.

The maintained cold profiles do not currently show one of these locks as a
dominant serialized region. They show diffuse hashing, allocation, comparison,
validation and compiler-pass work. That is evidence to improve attribution and
payload ownership before replacing the synchronization design.

## Identity and invalidation

Rue has three intentionally different identity domains:

1. A `QueryKey`'s typed `Eq` and `Hash` select a memo node. The memo maps use
   randomized hashing for caller-controlled keys; display text never decides
   equality.
2. Stable compiler identities such as module, definition, function instance and
   type instance keys make semantic artifacts request-independent.
3. `NodeIdentity::key` is deterministic presentation text for diagnostics and
   cycle reports. A monotonic runtime incarnation distinguishes separate live
   memo nodes even when presentation identity collides.

Each request pins one immutable `Revision`. Query terminals record ordered
input and dependency observations. On a compatible successor revision, the
runtime recursively validates those observations. If dependencies remain green,
the retained value is reused; if recomputation produces an equal value, its
stamp is preserved. This is Rue's implemented red/green query algorithm, not a
missing future tree feature.

The runtime retains bounded attempts and revisions. Active computations,
waiters, rooted request leases, published semantic/backend roots and retained
pin sets protect the exact terminals they still need. Retention pressure grows
and records an overflow rather than evicting a live proof cone.

## Artifact movement

The intended cross-query operation is sharing an immutable canonical value, not
copying a request-local compiler arena.

- Parsed modules, semantic projections, CFGs, codegen units and object
  projections are immutable terminal values.
- Stable canonical semantic bodies are materialized into fresh body-local AIR,
  type, symbol and string domains inside CFG evaluation. That conversion is
  intentional: compact local indices do not cross query boundaries.
- RUE-1346 removed accidental deep copies of the same `CanonicalBody` between
  the body transaction, canonical-body projection, analysis bundle and CFG
  input. `CanonicalBody` is now deliberately non-`Clone`; all four boundaries
  share one immutable allocation.
- Object serialization is retained per function. The fresh-link adapter still
  copies projected object bytes into the linker's owned-vector interface. That
  is the explicit ADR-0063 fresh-link seam, not an accidental peer codegen path.

RUE-1346 produced a neutral timing result under a same-host rerun: compiler
medians moved within roughly plus or minus 1.5 percent. Deterministic query work
was identical. Peak memory moved from 637.1 to 632.7 MiB for Harbor and from
702.1 to 693.0 MiB for Lattice. The architectural result is stronger than the
clock result: redundant work proportional to semantic-body payload size is no
longer representable by the type.

RUE-1348 then made display-only query identity movement explicit. The counters
add two relaxed atomic increments only where a key string was already being
formatted; they do not add formatting to memo hits or successful ordinary
nested requests. Fresh and retained performance reports preserve the three
causes separately rather than presenting one undifferentiated allocation total.

## Comparison with other query compilers

Rue's broad shape is conventional for a modern query compiler:

- The [rustc query model](https://rustc-dev-guide.rust-lang.org/parallel-rustc.html)
  requires immutable query results, claims one missing key, joins an in-progress
  identical key and performs separate parallel cycle detection. Rue implements
  the same claim/join/immutable-result pattern with explicit structured batches
  and one permit budget.
- [rustc incremental compilation](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation.html)
  records the query DAG and uses red/green validation. Rue records ordered
  observations and preserves stamps for equal recomputation for the same
  reason.
- [Salsa's runtime model](https://salsa-rs.github.io/salsa/plumbing/database_and_runtime.html)
  separates shared data-at-rest from per-handle active query state. Rue makes a
  similar separation between `RuntimeCore`/families and query tasks, but Rue's
  immutable pinned revisions permit explicit overlapping request lifetimes
  instead of exposing one mutable current revision to computations.
- [Salsa memos](https://salsa-rs.github.io/salsa/plumbing/terminology/memo.html)
  retain values, verification/change revisions and dependencies, and can drop
  values under LRU while preserving dependency information. Rue's bounded
  attempts, stamps, certificates and retention roots solve the corresponding
  compiler-lifetime problem with a different concrete policy.
- [rust-analyzer's architecture](https://rust-analyzer.github.io/book/contributing/architecture.html)
  separates input state from lazy derived state, parses one file at a time, and
  requires that typing inside one function not invalidate unrelated global
  derived data. Rue now follows the same granularity principle while extending
  the graph through native code generation and fresh linking.
- The [Swift compiler request evaluator](https://download.swift.org/docs/assets/generics.pdf)
  also decomposes type checking into on-demand cached requests, detects request
  cycles, and records dependency information. Swift's cached requests must
  replay their recorded required-name dependencies into an active caller;
  Rue's reused terminals similarly preserve dependency observations rather
  than treating a cached value alone as sufficient. Swift normally obtains
  process-level parallelism from multiple frontend jobs, explicitly accepting
  duplicated secondary-file work because those jobs share no cache. Rue instead
  shares one in-process query database across workers, trading that duplication
  for the measured runtime bookkeeping and synchronization described above.

The comparison supports the current ownership split. It does not support
adding a process-wide semantic arena or making `CompilerSession` itself the
unit of parallel mutation.

## Historical documents

Two earlier notes remain useful records of the migrations they closed, but are
not descriptions of the live compiler:

- `canonical-query-completion-audit.md` records the pre-RUE-648 session. Its
  statements that revisioned long-lived transactions, cancellation and
  dependency-derived invalidation remain future work are historical.
- `body-analysis-cfg-incrementality-audit.md` records the RUE-720 durable-import
  implementation. Its fresh whole-program AIR epoch, durable body/CFG import
  candidates and last-successful-baseline protocol were removed by the
  ADR-0063 query-native cutover.

Their still-valid invariants survived into ADR-0063: one compiler graph, stable
request-independent artifacts, fail-closed validation, per-function semantic
and CFG boundaries, and thin presentation consumers. Both notes carry an
explicit historical banner pointing here.

## Current performance findings

After the cold fixes through RUE-1348, the largest maintained workload records
160,776 validation traversals, 628,280 dependency observations, 570,840 memo
hits and 57,440 misses for 155,488 source tokens. These counters expose real
query bookkeeping, but the release profile no longer identifies one
algorithmic cliff. Its leading residual samples are distributed across hashing,
allocation/free, memory movement/comparison, body-local materialization, CFG
optimization and code generation.

The new display-identity counters do identify one concrete representation cost:

| workload | memo nodes count/bytes | structured waits count/bytes | abort fallbacks count/bytes | total bytes/token |
| --- | ---: | ---: | ---: | ---: |
| Ruelex | 8,307 / 2,181,171 | 8,827 / 2,545,490 | 0 / 0 | 96.15 |
| Mosaic | 18,330 / 5,698,363 | 30,979 / 10,414,280 | 0 / 0 | 202.88 |
| Harbor | 32,299 / 15,479,840 | 70,671 / 28,402,213 | 0 / 0 | 382.38 |
| Lattice | 38,380 / 9,494,659 | 89,895 / 33,777,606 | 0 / 0 | 278.30 |

This was the pre-RUE-1349 baseline. Lattice formatted 43.3 MB of
presentation-only key text. Structured batch waits accounted for 78 percent of
those bytes and 70 percent of materializations. The old path formatted every
child key before scheduling so the global wait graph could retain a complete
`NodeIdentity` for cycle rendering; a child which created a memo node then
formatted the key again for that node's retained identity. Abort fallback
contributed nothing on the maintained successful workloads.

```text
before RUE-1349
  parent owns typed child key K
    -> format K for StructuredWaitGuard
         -> global wait graph retains NodeIdentity while the batch joins
    -> schedule K in a child task
         -> QueryFamily::node(K)
              -> on a memo miss, format K again for the retained memo node

after RUE-1349
  batch owns one typed table [K0, K1, ...]
    -> wait graph retains (shared table, item index)
    -> only a rendered wait cycle formats K[index]
    -> memo misses retain their independently owned NodeIdentity as before
```

The typed key controls lookup in both cases. The duplicate text exists because
cycle presentation and memo-node presentation acquire their labels through
separate lifetime paths.

The structured-wait formatting was also a serial prefix of each batch: the
parent constructed every labeled edge before it released work to child tasks.
RUE-1349 removed that prefix with one batch-owned typed key table. Detection
continues to use ordered task edges; cycle presentation resolves only the
selected path after releasing the global wait-graph mutex. The complete
ownership and failure analysis is in
[`structured-wait-label-ownership.md`](structured-wait-label-ownership.md).

The same-host fresh-process comparison used the immediately preceding trunk
revision as its baseline and repeated the changed build after the first Harbor
timing pass was noisy:

| workload | structured wait identities/bytes before → after | total identity bytes/token before → after | compiler ms before → after |
| --- | ---: | ---: | ---: |
| Ruelex | 8,827 / 2,545,490 → 0 / 0 | 96.15 → 44.37 | 293.38 → 284.62 |
| Mosaic | 30,979 / 10,414,280 → 0 / 0 | 202.88 → 71.75 | 809.73 → 809.45 |
| Harbor | 70,671 / 28,402,213 → 0 / 0 | 382.38 → 134.89 | 1,937.37 → 1,929.82 |
| Lattice | 89,895 / 33,777,606 → 0 / 0 | 278.30 → 61.06 | 2,142.29 → 2,122.68 |

All other deterministic query-work counters were unchanged. Peak RSS moved
-0.1, -0.7, +0.5 and +4.3 MiB, so this experiment establishes no consistent
memory result. Compiler medians were neutral to slightly lower, but three
fresh processes with uncontrolled page-cache state do not justify a clock-time
speedup claim. The structural result is exact: successful maintained workloads
perform none of the previous 2.5–33.8 MB of structured-wait formatting.

The measurement overhead is neutral in the clock data: compiler-root medians
versus the immediately preceding same-host report moved -0.6, -0.6, +1.0 and
+0.7 percent from Ruelex through Lattice. The clock still does not isolate this
bookkeeping reliably, while the counters prove its scale and source.

RUE-1350 then measured the semantic body scheduler directly after a
fresh-process one-worker versus ten-worker profile found the semantic/body
closure phase flat at roughly 1.1 seconds while CFG and backend work scaled.
The deterministic counters separate transactions reached through a prefetched
frontier from transactions that still use the exact serial producer path:

| workload | batches | batched keys | keys/batch | prefetched transactions | serial transactions |
| --- | ---: | ---: | ---: | ---: | ---: |
| Harbor | 3 | 282 | 94.00 | 282 | 1,097 |
| Lattice | 4 | 175 | 43.75 | 175 | 1,088 |

Every non-root batch on these workloads had at least eight keys, so the batches
that exist are useful. However, 79.6 percent of Harbor transactions and 86.1
percent of Lattice transactions still cross the anonymous-producer boundary
serially. The counts were exact across three repetitions and both worker
settings. This identifies the producer-before-consumer boundary, rather than
linking or a compiler-wide lock, as the strongest next cold-performance target;
RUE-1351 tracks batching ready anonymous-producer body frontiers.

RUE-1351 made that producer ordering explicit as dependency-ready logical
frontiers. Static instance-key dependencies and dynamically discovered producer
dependencies now enter one deterministic scheduler; a bounded execution window
prefetches ready transactions without retaining an unbounded frontier. The same
logical frontier runs inline with one query permit, so work counters and output
do not depend on worker count. On the maintained workloads the previous 1,097
Harbor and 1,088 Lattice serial transactions both fell to zero. Harbor compiler
time moved from 1,777.56 to 944.35 ms and Lattice from 2,042.64 to 1,110.13 ms
in the same-host release profiles, while Lattice validation observations fell
from 628,280 to 614,202. The Lattice executable hash remained
`3f5ff289241c1fd9019c8a1ef618193458a209e0a83c1eb8d14e0882282a5f59`.

The speedup has an explicit peak-memory tradeoff. Across the final same-source
runs, Lattice peak RSS rose from about 693.0 to 735--744 MiB with the ordinary
worker setting and from 686.75 to about 728.5 MiB with one worker, roughly six
to seven percent. Retained
query records, retained bytes, dependency pins, task leases and active retained
pins were exactly unchanged. Allocation instrumentation measured only about
0.6 percent more requested bytes; the larger RSS high-water change comes from
the allocator's size-class and lifetime shape around the ready scheduler, not a
larger retained query graph. Harbor remained approximately memory-neutral. This
tradeoff is tracked explicitly rather than being inferred from clock noise.

## Next actions and decision boundary

Authorized low-risk work:

1. Keep the display-identity counters in cold and retained-session reports so
   any representation change has an exact before/after witness.
2. Continue removing accidental owned copies where an immutable terminal
   already owns the payload.
3. Clean stale comments and keep phase attribution exhaustive when profiles
   expose unattributed work.
4. Preserve bounded dependency-ready body frontiers and use their exact work
   counters to identify any remaining fallback coordinator path.

Maintainer review required before implementation:

1. Add a retained `LoweredMir` terminal. ADR-0063 deliberately leaves that
   query-granularity choice open pending a direct consumer or reuse result.
2. Introduce stateful incremental linking. `ProgramImagePlanDelta` is the
   prepared seam, but placement, patching, failure recovery and executable
   publication require their own ADR and joint planning.

The current evidence does not justify a new compiler-wide lock strategy, a
whole-program semantic owner, or finer backend query fragmentation.
