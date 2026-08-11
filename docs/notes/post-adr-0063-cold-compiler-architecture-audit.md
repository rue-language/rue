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

RUE-1354 then tested whether scheduler-container lifetime was the source of
that peak. Replacing the bounded prefetched-result tree with an ordered FIFO
removed 4,591 compiler allocations and 211,374 requested bytes from Lattice
while preserving byte-identical output and every reachability and query-work
counter. Controlled parent/current release medians were clock-neutral within
run noise: Ruelex 145.28/140.32 ms, Mosaic 422.53/425.48 ms, Harbor
953.79/938.11 ms and Lattice 1,143.10/1,150.37 ms. Peak RSS did not materially
move, confirming that the prefetched tree was bookkeeping churn but not the
source of the RUE-1351 high-water change. Halving the prefetch window did
recover Lattice RSS (733.0 versus 731.5 MiB for the parent) but slowed Harbor
and Lattice by roughly six to seven percent, so that tradeoff was rejected.
The FIFO remains worthwhile as a representation win, and the one-worker path
now aggregates missing toolchain modules across the complete ready frontier
instead of requiring avoidable acquisition rounds.

RUE-1355 then removed repeated successful re-derivation from validation's
exact-node path. A runtime-created `NodeIdentity` now shares one immutable
family/key payload among its node, terminals and dependency observations. That
payload carries a runtime-scoped weak handle to the exact erased node; expired,
display-only and foreign-runtime identities fall back to the incarnation
registry and preserve the existing fail-closed behavior. Equality, ordering,
hashing and debug presentation still use only the stable family/key pair. The
representation shrinks `NodeIdentity` from four machine words to one and a
complete `Observation` from six words to three without keeping evicted nodes
alive.

The new `registry_index_lookups` counter separates shared-index access from the
existing logical `registry_probes` contract. On every maintained cold workload,
all live runtime-created observations bypass the index:

| workload | logical node resolutions | shared registry index lookups |
| --- | ---: | ---: |
| Ruelex | 85,784 | 0 |
| Mosaic | 275,890 | 0 |
| Harbor | 570,103 | 0 |
| Lattice | 768,446 | 0 |

Every other query and reachability work counter is byte-for-byte identical to
the exact parent report. The same-host release comparison moved compiler time
and peak RSS in the same favorable direction across the full scaling curve:

| workload | compiler ms parent → RUE-1355 | peak MiB parent → RUE-1355 |
| --- | ---: | ---: |
| Ruelex | 145.63 → 133.77 | 113.2 → 110.7 |
| Mosaic | 425.62 → 391.91 | 266.5 → 261.4 |
| Harbor | 950.81 → 883.76 | 639.9 → 625.6 |
| Lattice | 1,140.62 → 1,017.15 | 739.1 → 727.4 |

Allocation instrumentation makes the representation tradeoff explicit. The
shared payload adds one allocation per materialized memo identity, increasing
Lattice's allocation count by 28,909 (0.13 percent), while halving every
observation's inline identity footprint reduces requested bytes by 47,663,507
(1.04 percent). The lower release RSS and compiler time show that the extra
small owner allocation is preferable to copying and refcounting two fat string
pointers across the graph. The final Lattice executable remains byte-identical
to its parent (`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`).

RUE-1356 then removed request-wide lookup preparation from the per-CFG-input
loop. `LocalFactSelectionIndex` is one immutable, request-scoped convenience
index over the exact durable declaration and anonymous-nominal slices. It is
not a query or semantic authority: each body still selects and owns its exact
transitive fact closure in the CFG memo key. The index borrows its source keys,
is shared only while CFG inputs are assembled, and is dropped before CFG query
evaluation begins.

The scaling schema now separates index construction from exact body-local fact
selection. Every maintained workload builds one index; the prior source
performed the declaration, anonymous-nominal and recursive slice-type scans
once per selection. The old work below is the exact source-derived product of
the same input cardinalities, while the new values are emitted counters:

| workload | index builds old → new | declaration scans old → new | type-node scans old → new | fact selections |
| --- | ---: | ---: | ---: | ---: |
| Ruelex | 216 → 1 | 151,848 → 703 | 243,000 → 1,125 | 216 |
| Mosaic | 630 → 1 | 666,540 → 1,058 | 1,217,160 → 1,932 | 630 |
| Harbor | 1,359 → 1 | 1,954,242 → 1,438 | 4,305,312 → 3,168 | 1,359 |
| Lattice | 1,280 → 1 | 2,927,360 → 2,287 | 5,560,320 → 4,344 | 1,280 |

The same-host release comparison against merged RUE-1355 shows the expected
size-sensitive result. Small workloads are neutral within noise, while the
larger maintained programs spend materially less time in CFG preparation:

| workload | compiler ms parent → RUE-1356 | peak MiB parent → RUE-1356 |
| --- | ---: | ---: |
| Ruelex | 133.77 → 132.05 | 110.7 → 110.1 |
| Mosaic | 391.91 → 389.20 | 261.4 → 261.8 |
| Harbor | 883.76 → 804.38 | 625.6 → 629.0 |
| Lattice | 1,017.15 → 932.14 | 727.4 → 727.6 |

The fixed one-worker Lattice allocation probe fell from 22,297,695 to
21,842,394 allocations (-2.04 percent) and from 4,524,520,772 to
3,543,025,272 requested bytes (-21.69 percent). Every pre-existing query and
reachability counter remains exact, and the Lattice executable hash is still
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1357 then removed the next coherent allocator-heavy cluster from the cold
profile. Shared liveness previously built three separately allocated lists per
machine instruction: successors, virtual-register uses and virtual-register
definitions. MIR bounds those facts tightly — two successors and at most three
current virtual operands — so the canonical liveness adapter now stores them
in fixed-capacity inline lists. The four-register list deliberately has one
slot of headroom, and both inline representations are no wider than the `Vec`
elements they replace. A production capacity check fails loudly if a future
MIR instruction changes the audited bound.

That width constraint is material. A growable small-vector experiment removed
the same tiny allocations but widened every row of the three outer fact tables,
raising Lattice's peak by roughly 9 MiB in the first controlled run. The fixed
representation was selected instead: its final peak was 722.5 MiB versus the
RUE-1356 parent's 727.6 MiB, while the other maintained workloads moved -0.2,
+1.0 and 0.0 MiB. The fixed one-worker Lattice allocation probe fell from
21,842,394 to 20,723,763 calls (-5.12 percent) and from 3,543,025,272 to
3,535,874,140 requested bytes (-0.20 percent).

Cold clock time is neutral overall rather than a universal speedup: compiler
medians moved 132.05 to 130.84 ms for Ruelex, 389.20 to 363.53 ms for Mosaic,
804.38 to 813.01 ms for Harbor and 932.14 to 944.40 ms for Lattice. The latter
two deltas are small relative to the paired runs' noise, while the exact
allocation reduction establishes the work result independently. All existing
query, reachability and materialization counters remain identical, and the
Lattice executable remains byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1358 followed the post-RUE-1357 release profile into stable machine-symbol
encoding. The version-one encoder previously allocated temporary decimal
strings for every tag, field length, and numeric value, invoked generic
formatting once per input byte, and allocated a fresh scratch string for every
sequence element. It now writes integers from stack-backed `itoa` buffers,
hex-encodes bytes through a fixed lookup table, and reuses one scratch buffer
per sequence. The wire format and ownership boundary are unchanged; this is a
local representation improvement rather than a symbol cache or a new shared
lifetime.

On the fixed one-worker Lattice allocation probe, calls fell from 20,723,870 to
19,233,621 (-7.19 percent) and requested bytes from 3,535,911,692 to
3,533,206,987 (-0.08 percent). Every query, reachability, and CFG-materialization
counter remained exact. Two ordinary-allocator scaling runs had no consistent
clock or peak-memory direction: the first moved the four compiler medians by
+1.5, +1.5, +0.5, and -2.1 percent, while the repeat was visibly host-noisy and
bracketed the parent's memory on the two large workloads. The deterministic
allocation result therefore establishes the win without claiming a clock
speedup. The Lattice executable remains byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1359 then specialized the shared instruction scheduler's transient
bookkeeping to the graph it actually constructs. Machine instructions already
have dense indices, and every dependency is discovered from an earlier
instruction to the current one. Edge deduplication therefore uses one dense
"last target" stamp per predecessor instead of hashing every `(from, to)` pair.
The same forward-edge invariant makes reverse instruction order a topological
order, so critical-path priorities no longer need a hash map or an explicit
DFS stack. The scheduler policy, dependency graph, priority equation, and
backend fact adapters are unchanged.

The fixed one-worker Lattice allocation probe fell from 19,233,621 to
19,144,997 calls (-0.46 percent) and from 3,533,206,987 to 3,493,225,879
requested bytes (-1.13 percent). Every query, reachability, and
CFG-materialization counter remained identical. Two ordinary-allocator runs
showed no regression: the first moved compiler medians by +2.4, -0.5, -1.2,
and -1.0 percent from Ruelex through Lattice, while the repeat moved them by
-5.0, -1.9, -1.8, and +0.7 percent. Peak memory was neutral on the two smaller
workloads and about 6 MiB and 2 MiB lower on Harbor and Lattice in both runs.
The Lattice executable remains byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1361 removed full-width bitset allocation from the shared liveness
fixed-point loop. The transfer function now clears and reuses one `live_in` and
one `live_out` scratch set across every instruction and convergence round.
After ranges and the optional debug projection consume the final dataflow
tables, the production `live_in` table becomes the retained `live_at` union in
place instead of allocating a third table. The equations, reverse visitation
order, convergence test and backend adapters are unchanged.

On the fixed one-worker Lattice allocation probe, calls fell from 19,144,997 to
17,700,930 (-7.54 percent) and requested bytes from 3,493,225,879 to
3,382,572,379 (-3.17 percent). Every query, reachability and CFG-materialization
counter remained identical. An immediate parent/current ordinary-allocator
comparison moved compiler medians from 140.87 to 133.36 ms for Ruelex, 369.36
to 362.58 ms for Mosaic, 830.06 to 784.99 ms for Harbor and 939.84 to 904.37 ms
for Lattice. Peak memory was neutral on the smaller workloads and fell from
626.5 to 620.3 MiB for Harbor and from 734.8 to 720.2 MiB for Lattice. The
Lattice executable remains byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1362 simplified the looped half of shared live-range construction. Virtual
registers are dense and the canonical result already owns one indexed slot per
register, but the looped path previously accumulated endpoints in two hash
maps and then scanned every register to look them up again. Definitions, uses,
and exact `live_in`/`live_out` membership now extend the canonical dense table
directly. The interval semantics and the loop-free fast path are unchanged.

On the fixed one-worker Lattice allocation probe, calls fell from 17,700,930 to
17,694,739 (-0.03 percent) and requested bytes from 3,382,572,379 to
3,374,368,531 (-0.24 percent). Every query, reachability, and
CFG-materialization counter remained identical. An immediate parent/current
ordinary-allocator comparison moved compiler medians from 129.08 to 128.28 ms
for Ruelex, 361.36 to 355.20 ms for Mosaic, 785.64 to 786.87 ms for Harbor, and
897.61 to 917.22 ms for Lattice. The Lattice sample had a 21.59 ms MAD, so the
mixed clock result is neutral rather than evidence of a regression. Peak
memory moved -0.8, +1.3, -3.5, and -4.3 MiB respectively. The Lattice
executable remains byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1363 removed per-interval hash-table construction from both production
linear-scan register-allocation paths. The allocator already maintains the
active intervals separately for each register class, and that list cannot
outgrow the class's small physical-register file. Free-register selection now
checks that canonical bounded list directly instead of collecting an
equivalent `HashSet` for every arriving virtual register. Register preference,
clobber, spill, rematerialization, and optional reuse-pass policies are
unchanged.

On the fixed one-worker Lattice allocation probe, calls fell from 17,694,739 to
17,593,676 (-0.57 percent) and requested bytes from 3,374,368,531 to
3,371,985,007 (-0.07 percent). Every query, reachability, and
CFG-materialization counter remained identical. An immediate parent/current
ordinary-allocator comparison moved compiler medians from 128.01 to 130.36 ms
for Ruelex, 367.85 to 371.60 ms for Mosaic, 820.46 to 793.96 ms for Harbor, and
975.90 to 902.59 ms for Lattice. The larger parent samples were host-noisy
(23.82 and 49.26 ms MAD), so this establishes no clock regression without
claiming the apparent large-workload speedup. Peak memory moved +0.3, -0.8,
+0.6, and -2.3 MiB respectively. The Lattice executable remains byte-identical
at `8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1364 followed the next release profile into recursive semantic identity
hashing and copying. A `StableDefinitionKey` used to copy its full module,
name, and optional owner payload into every recursive type/function key and
re-hash those strings at every query index, task-cache, and validation lookup.
The key now owns one `Arc`-shared immutable payload and computes a 128-bit
SHA-256 accelerator once when that payload is issued. Exact field equality is
still authoritative, `Ord` still compares the original fields in their
original order, and the runtime's keyed hash map still owns bucket placement;
a forced-collision regression proves the accelerator cannot conflate keys.

The fixed one-worker Lattice allocation probe reduced requested bytes from
3,371,985,007 to 2,917,750,583 (-13.47 percent). Issuing the shared payload
adds one allocation per distinct stable identity, so allocation calls moved
from 17,593,676 to 17,603,349 (+0.05 percent); the much smaller recursive key
copies repay that bounded setup cost in bytes and retained memory. A second
immediate current/parent ordinary-allocator comparison moved compiler medians
from 132.65 to 130.38 ms for Ruelex, 360.10 to 343.58 ms for Mosaic, 782.21 to
759.58 ms for Harbor, and 894.76 to 864.09 ms for Lattice. Peak memory fell on
every workload: 109.0 to 97.7 MiB, 261.7 to 229.8 MiB, 622.1 to 550.2 MiB, and
717.8 to 643.8 MiB respectively. Every query, reachability, and
CFG-materialization counter remained identical, and the Lattice executable
remains byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1365 made the largest remaining leaf phase measurable at the same standard
as query validation and CFG materialization. Provider-native body analysis
already increments exact production counters for each lookup, candidate query,
fact-family read, and durable materialization. The one-shot metrics boundary
and the versioned scaling report now snapshot those counters; measurement adds
no provider operation to the compile path.

The maintained cold baseline is:

| workload | name lookups | method candidates | identity facts | signature facts | const facts | durable materializations |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ruelex | 8,670 | 892 | 6,667 | 2,014 | 4,012 | 3,148 |
| Mosaic | 23,443 | 784 | 21,639 | 8,171 | 4,744 | 8,673 |
| Harbor | 48,756 | 476 | 42,672 | 16,706 | 7,836 | 17,374 |
| Lattice | 66,324 | 500 | 53,917 | 21,198 | 13,890 | 21,636 |

Import lookups, operator candidates, nominal-well-formedness reads, and
anonymous-nominal fact reads are all exactly zero on these four successful
workloads; producer and toolchain reads remain visible in the complete report.
The next bounded investigation should therefore begin with repeated name and
identity observation, not with an inactive provider family or linking.

The projection is measurement-neutral. Against the merged RUE-1364 report,
compiler medians moved 130.38 to 127.83 ms for Ruelex, 343.58 to 352.52 ms for
Mosaic, 759.58 to 738.34 ms for Harbor, and 864.09 to 862.39 ms for Lattice;
peak RSS moved +1.5, +0.6, +0.2, and -8.3 MiB respectively. This mixed movement
is ordinary run noise, not a claimed speedup or regression. The fixed
single-worker Lattice allocation probe remains effectively identical at
17,603,205 calls and 2,917,731,831 requested bytes, and the executable remains
byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1366 used that provider baseline to reject two tempting semantic shortcuts
before following a CPU profile. A request-local name-resolution cache removed
no query work because the 66,324 Lattice lookups are unique within that cache's
safe lifetime. Deriving a signature's stable definition key locally likewise
removed no measured query work and added almost exactly one allocation per
signature read; the existing identity query already returns a shared key at
negligible cost. Neither experiment was retained.

The subsequent symbolized Lattice sample instead placed standard SipHash at the
top of the collapsed compiler stack. Its dominant call paths were the sharded
typed-key memo index and the task-local typed query cache. Those two indexes now
use AHash with independent runtime-random keys; exact `Hash` plus `Eq` remains
authoritative, so collision safety and adversarial resistance are preserved.
Numeric runtime bookkeeping stays on its existing specialized or standard
hashers because the profile did not justify broadening the change.

The targeted SipHash leaf fell from 63 to 27 samples (-57 percent). Against the
merged RUE-1365 report, compiler-root medians moved from 127.83 to 124.91 ms for
Ruelex, 352.52 to 352.02 ms for Mosaic, 738.34 to 723.54 ms for Harbor, and
862.39 to 834.20 ms for Lattice. Peak memory moved -0.2, +0.6, -2.5, and +4.0
MiB respectively, which is mixed and within run noise. Every query,
reachability, CFG-materialization, and semantic-provider counter remained
identical. Lattice allocation counts are flat; requested bytes moved about
+0.06 percent, also within measurement noise. The executable remains
byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1367 closes the next measurement gap exposed by a rejected optimization:
the scaling report previously published validation and retention work but not
all query request outcomes. The compiler now makes one canonical projection of
cumulative query-runtime work and schema version ten publishes claims,
reuses, joins, completed bodies, red/green publications, cancellations, and
cycles. The report and retained-session runner share one saturating arithmetic
implementation, so adding a runtime field cannot silently make their deltas
disagree.

The fresh one-worker baseline records 8,403/68,041 claims/reuses for Ruelex,
18,602/191,452 for Mosaic, 32,674/382,634 for Harbor, and 39,092/494,426 for
Lattice. Every claimed body completes and publishes green in these fresh
probes; joins, cancellations, and cycles are zero. The counters are read only
at the observation boundary, so production query behavior and the previously
published deterministic work remain unchanged. Fresh-process timing and peak
memory remain mixed within run noise, and the Lattice executable remains
byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1368 used those request counters to remove one peer identity query from
every successful body-provider signature fact. The semantic-nucleus signature
projection now carries the shared stable definition key it already computes;
the consumer clones that pointer instead of requesting the identity projection
solely to recover the same key. Reconstructing the key at the consumer was
rejected because it replaced a query reuse with a fresh digest and allocation.

The fixed probes remove exactly 2,014, 8,171, 16,706, and 21,198 reuses from
Ruelex through Lattice, equal to the signature-fact counts. Claims, validation,
retention enforcement and scans, display identities, provider work,
reachability, and CFG materialization are unchanged. A reverse-order paired
run on the loaded local host moved compiler medians -10.3, +5.5, -10.9, and
-20.3 percent respectively; Mosaic's movement was smaller than its MAD, so the
clock result is mixed/neutral rather than a claimed speedup. Peak memory was
flat within 1.5 MiB, and the Lattice executable remains byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

RUE-1371 removes a duplicate retained-memory accounting walk at the CFG
boundary. A successful raw CFG computes and caches the exact logical charge of
its body-local symbol interner; the optimized CFG shares that append-only
interner and reuses the cached charge while its symbol universe is unchanged.
Accessor import can extend the shared interner, so that path detects actual
growth, refreshes the shared charge, and records the additional walk.

Scaling schema version eleven publishes this bookkeeping separately from CFG
materialization. The fixed one-worker probes now perform 216, 630, 1,359, and
1,280 interner scans across Ruelex through Lattice, visiting 3,544, 13,089,
32,191, and 41,882 entries and 588,364, 2,839,105, 8,418,806, and 10,887,143
UTF-8 bytes. The previous source path repeated every walk for the optimized
terminal, including CFGs whose symbol universe was unchanged. None of the
maintained workloads extends an interner during accessor import, so all three
measures are halved exactly across the curve; the accessor corpus separately
exercises the growth refresh. Directly alternating baseline/final Ruelex
samples are clock-neutral within host
dispersion, peak memory is flat, and maintained Harbor/Lattice probes are
neutral to faster. The Lattice executable remains byte-identical at
`8d7355dcda83780ef8a98aedfa0495a9d4745c5ed84375e0ae9a37d82eb361a9`.

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
