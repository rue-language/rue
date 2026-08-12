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

RUE-1372 removes repeated declaration-identity queries from durable body-fact
materialization. Resolving a stable definition already has to inspect each
syntax candidate's exact identity; the source now carries the winning identity
projection beside the candidate and consumes it when materializing constants,
nominals, and free functions. Signature queries and dynamic anonymous-nominal
registration remain on their existing paths.

The fixed one-worker probes reduce identity facts from 6,667 to 3,743 on
Ruelex, 21,639 to 10,321 on Mosaic, 42,672 to 20,036 on Harbor, and 53,917 to
24,837 on Lattice. Total declaration facts fall from 12,693 to 9,769, 34,554
to 23,236, 67,214 to 44,578, and 89,005 to 59,925 respectively. Query reuses
fall by the same 2,924, 11,318, 22,636, and 29,080 requests because the removed
identity lookups were compatible peer queries. Every other semantic-provider
counter and every query-validation counter is exactly unchanged.

Five directly alternating release pairs are clock-neutral: median paired
deltas are +0.08% on Ruelex, -1.49% on Harbor, and +0.43% on Lattice, all well
inside host dispersion. Median peak memory remains within 0.6%. Output sizes
and SHA-256 hashes are identical for every workload.

RUE-1373 removes equal-string allocations while reconstructing syntax
candidates from stable definition identities. Stable keys and named owners
already retain their immutable names in `Arc<str>`; candidate construction now
clones those pointers instead of allocating new payloads. Ordinary functions
benefit twice because their function and extern-function candidates share the
same name allocation. Candidate order, equality, hashing, query ownership, and
invalidation are unchanged.

The fixed one-worker Lattice allocation probe falls from 17,434,269 to
17,387,834 allocation calls (-46,435, or -0.27%) and from 2,917,592,451 to
2,916,549,779 requested bytes (-1,042,672). Every deterministic compiler-work
counter is byte-for-byte identical. Five directly alternating ordinary-release
pairs are clock-neutral: median paired deltas are +0.19% on Ruelex, -0.66% on
Harbor, and -0.51% on Lattice. Median peak RSS is flat to modestly lower, and
all output sizes and SHA-256 hashes are identical.

RUE-1374 removes whole-history snapshots from transactional lookup-root
publication. The lookup lease retains a bounded 4,096-key incarnation history
and recency index; both publication handoffs previously deep-cloned both trees
before every commit solely to support the rare abort path. Publication now
journals only the keys it refreshes or evicts and replays that journal backward
on abort. Root replacement, bounded FIFO eviction, rederivation accounting,
callback rollback, and retry semantics are unchanged.

The fixed one-worker Lattice allocation probe falls from 17,387,834 to
16,796,147 allocation calls (-591,687, or -3.40%) and from 2,916,549,779 to
2,723,643,949 requested bytes (-192,905,830, or -6.61%). Every deterministic
compiler-work counter and the emitted executable are identical. Five directly
alternating ordinary-release pairs are consistently faster, with a median
paired delta of -5.43%. Median peak RSS falls from 658,046,976 to 455,131,136
bytes (-193.52 MiB, or -30.84%).

RUE-1375 keeps typed lookup keys in the bounded incarnation history. The
published observation already owns a name/import key whose module and payload
strings are shared; the history previously formatted that key into a new
presentation string on every publication. The history and RUE-1374 rollback
journal now clone the typed key directly. Name and import families remain
distinct variants, and no diagnostic or query identity changes.

The fixed one-worker Lattice allocation probe falls from 16,796,147 to
16,567,637 allocation calls (-228,510, or -1.36%) and from 2,723,643,949 to
2,720,491,481 requested bytes (-3,152,468). Every deterministic compiler-work
counter and the emitted executable are identical. Twelve directly alternating
ordinary-release pairs are clock-neutral at a +0.16% median paired delta.
Median peak RSS moves from 456,433,664 to 457,940,992 bytes (+1.44 MiB, or
0.33%), within run dispersion.

RUE-1376 gives stable-definition ordering the same shared-allocation shortcut
as equality. Stable keys are propagated by cloning one immutable identity Arc;
ordered maps previously walked module, namespace, kind, name, and owner fields
even when comparing two handles to that exact allocation. Independently issued
keys still use the authoritative full-field order, including when their cached
hash accelerators collide.

The post-RUE-1375 Lattice profile placed `StableDefinitionKey::cmp` among the
hottest compiler leaves. Two independent directly alternating sets total 24
ordinary-release pairs: their median paired deltas are -3.84% and -2.62%, for a
combined -3.09%. Separate medians move from 1,399.815 to 1,374.492 ms. Median
peak RSS moves from 458,235,904 to 454,656,000 bytes (-3.41 MiB). Allocation
counts and requested bytes vary by less than 0.001% across repeated accounting
runs; every deterministic compiler-work counter and emitted executable is
identical.

RUE-1377 replaces the task-local and structured-batch validation-endorsement
trees with exact hash indexes. These sets only insert, extend, and test the
runtime-assigned `(incarnation, stamp, revision)` identity; no consumer uses
their order. Cold Lattice performs 745,097 authority probes, so ordered lookup
was avoidable work on one of the hottest query-runtime paths.

Across three post-RUE-1376 and three candidate samples,
`Task::validation_endorsement_authority_at` falls from 49 to 11 combined
top-of-stack samples (-78%). Two allocation-accounting comparisons agree: the
first moves from 16,567,861 to 16,553,066 calls (-14,795, or 0.09%) and from
2,720,425,017 to 2,713,507,845 requested bytes (-6,917,172, or 0.25%); the
reverse-order repetition differs by fewer than 20 calls and 20 KiB. Twenty-four
directly alternating ordinary-release pairs are clock-neutral at a +0.67%
median paired delta. Median peak RSS moves by +1.58 MiB (0.35%), within run
dispersion. Every deterministic compiler-work counter and the emitted
executable are identical.

RUE-1378 gives the request-local terminal-lease deduplication index the same
numeric-identity treatment. Cold Lattice makes 199,098 lease observations;
`TaskLeases` only inserts exact `(incarnation, stamp, revision)` identities and
tests whether each was new, so its ordered tree carried no semantic order.

Two allocation-accounting comparisons reduce calls by 10,887 and 10,823 and
requested bytes by 7,032,904 and 7,013,852 respectively. Sixteen directly
alternating ordinary-release pairs are clock-neutral at a -0.61% median paired
delta. Median peak RSS moves by +0.69 MiB (0.15%), within run dispersion. Every
deterministic compiler-work counter and the emitted executable are identical.
The adjacent structured-batch lease set was evaluated separately and retained
as an ordered tree: hashing it saved about 1,100 allocation calls but requested
about 6.7 MiB more memory, so it did not meet the non-regression boundary.

The next apparent comparison shortcut was also rejected. Adding shared-Arc
fast paths to `NodeIdentity` equality and ordering left 32 paired Lattice runs
clock-neutral (-0.27%), changed no deterministic work or allocation measure,
and did not reduce the broader string-comparison profile. Its median peak RSS
was repeatably about 3.0 MiB higher, so the source change was not retained.

RUE-1380 tested routing AIR projection helpers through the existing 64-entry
per-body type-validation cache. It preserved focused diagnostics, work, and
output, but removed no material allocation work. Sixteen paired Lattice runs
were 2.86% slower and median peak RSS rose by about 5.6 MiB, so the prototype
was rejected rather than treating locally simpler bookkeeping as a free win.

RUE-1381 follows the next measured query-runtime leaf into request-frame
dependency recording. A frame now retains its common single direct dependency
inline by runtime-unique node incarnation. The second distinct dependency
promotes to a boxed numeric hash index, which keeps wide-frame observation
expected-linear instead of introducing an unbounded linear search. Completed
frames sort once by the existing stable display identity before publishing the
immutable dependency array, preserving canonical order.

The targeted ordered-map insertion falls from 33 combined top-of-stack samples
to zero across three profiles. Two fixed one-worker allocation comparisons save
7,941 and 7,727 calls and 794,540 and 678,980 requested bytes respectively.
Sixteen directly alternating ordinary-release pairs are 1.59% faster; separate
medians move from 1,888.37 to 1,823.21 ms. Median peak RSS rises by 1.55 MiB
(0.35%), within dispersion. Every deterministic compiler-work counter and the
emitted executable remain identical.

RUE-1382 removes the tree allocation from validation's recursion guard without
trading it for an unbounded linear scan. Each traversal now retains its first
eight active runtime incarnations inline and promotes once to a numeric hash set
when a genuinely deep dependency cone crosses that bound. The ordered tree had
no semantic consumer: the guard only inserts, tests membership, and removes.

Fixed one-worker cold Lattice performs 154,956 validation traversals and
614,202 node visits with zero active-cycle prunes. The candidate removes 137,381
allocator calls (0.83%) and 14,279,424 requested bytes (0.53%); the profiled
tree-insertion leaf under `validated_stamp` disappears. Sixteen directly
alternating ordinary-release pairs improve by 0.71% at the paired median.
Median peak RSS rises by 1.48 MiB (0.33%), within run dispersion. Every
deterministic compiler-work counter and the emitted executable remain
identical.

Two adjacent collection substitutions were measured and rejected. RUE-1383
changed task-local validation endorsement scopes from a mutex to a read/write
lock. Thirty-two alternating runs were clock-neutral, but the targeted
authority leaf grew by 16% across three profiles: the actual task-sharing shape
did not provide enough concurrent readers to repay the heavier uncontended
primitive. RUE-1384 changed the canonical anonymous-nominal registry from an
ordered tree to a hash table. Its recursive identity was more expensive to hash
than to compare in this registry; the combined 32-pair cold median regressed by
1.15%. Neither prototype was retained.

RUE-1385 instead removes the owned-value copy at the canonical anonymous
registry boundary. Registry entries are immutable request-local facts, and the
body identity pool may ask separately for shape, methods, type captures, and
value captures. The registry now returns an `Rc` handle, so those lookups share
the complete nominal and each consumer clones only its required projected
output.

Fixed one-worker Lattice saves 30,008 allocator calls (0.18%) and 4,469,456
requested bytes (0.17%). The durable `AnonymousNominalKey` clone leaf falls
from 105 to 65 combined samples (-38%). Sixteen alternating ordinary-release
pairs are clock-neutral at +0.14%; median peak RSS falls by 3.27 MiB (0.73%).
Every deterministic compiler-work counter and the emitted executable remain
identical.

RUE-1386 removes the adjacent recursive producer copy. Anonymous lookup needs
only to lend a function producer to `producer_body_facts`; it now borrows an
existing function key and owns only the synthetic definition-producer key that
must be constructed. The borrowed-or-owned value cannot escape the lookup.

Fixed one-worker Lattice saves another 14,125 allocator calls (0.09%) and
758,932 requested bytes. The durable `FunctionInstanceKey` clone leaf falls
from 48 to 32 combined samples (-33%). Sixteen alternating ordinary-release
pairs improve by 0.62% at the paired median. Median peak RSS rises by 1.54 MiB
(0.35%), within dispersion. Every deterministic compiler-work counter and the
emitted executable remain identical.

RUE-1387 removes the ordered-tree allocation from the common one-input query
frame. Request-local input bookkeeping now keeps zero or one identity inline
and promotes only a second distinct identity to the existing ordered map. Both
ordinary and aborted-prefix observations use the same accumulator, and
multi-input publication retains canonical `InputIdentity` order.

Fixed one-worker Lattice saves 979 allocator calls and 1,146,563 requested
bytes. Sixteen alternating ordinary-release pairs are clock-neutral on the
loaded profiling host; the paired median is -2.12%, inside very high run
dispersion. Median peak RSS moves +2.7 MiB (0.6%), also within dispersion.
Every deterministic compiler-work counter and the emitted executable remain
identical.

RUE-1388 reuses the same inline ordered accumulator for request-frame work
contributions. A single structural-work identity now aggregates in the frame
itself; a second distinct identity promotes to the existing ordered map, so
terminal publication keeps its canonical metric order. Direct, inherited, and
aborted-prefix contributions all share that path.

Fixed one-worker Lattice saves another 1,242 allocator calls and 61,452
requested bytes. Sixteen alternating ordinary-release pairs are clock-neutral
to modestly faster at -0.78% paired median despite one noisy outlier. Median
peak RSS moves +0.6 MiB, within dispersion. Every deterministic compiler-work
counter and the emitted executable remain identical.

RUE-1389 makes the body-reference publication representation explicit: every
summary is sorted and duplicate-free. Provider publication previously rebuilt
that canonical slice by inserting all old and newly selected references into a
fresh ordered tree, even though both inputs were already canonical. It now
merges the two ordered streams directly, reducing the merge from
O((n + m) log(n + m)) ordered insertion work to O(n + m). A debug assertion
guards the producer-side invariant at the publication boundary.

Fixed one-worker Lattice saves 6,076 allocator calls and 6,508,304 requested
bytes. Sixteen alternating ordinary-release pairs are clock-neutral on the
loaded host: the paired median is -2.49%, but the paired MAD is 11.10% and the
range is -27.68% to +54.74%. Median peak RSS falls by 2.4 MiB, within the wide
run dispersion. Every deterministic compiler-work counter and the emitted
executable remain identical.

RUE-1390 removes a redundant full-tree sweep from each body-scheduler
iteration. All pending insertion goes through `schedule_body_instance`, which
refuses visited bodies, while the one direct deferred-producer retry removes
its body from `visited` before reinserting it. The scheduler now asserts that
invariant in debug builds instead of filtering the complete pending frontier in
release builds. This removes a potential O(iterations × pending) comparison
path without changing scheduler order or query topology.

The body-scheduler `BTreeMap::ExtractIf` leaf disappears from three follow-up
profiles; the remaining `ExtractIf` samples belong to unrelated runtime
retention. Thirty-two alternating ordinary-release pairs are clock-neutral at
a -0.38% paired median, with 7.96% paired MAD on the loaded host. Median peak
RSS moves +1.53 MiB within dispersion, and repeated allocation-instrumented
runs are neutral. Every deterministic compiler-work counter and the emitted
executable remain identical.

RUE-1391 narrows the remaining standard-hasher cost to the body-local identity
pool. Its 17 durable-key, reverse-identity, poison, and signature registries now
use independently keyed AHash maps; unrelated provider, inference, query, and
test maps keep their existing hashers. These registries expose exact `Hash` +
`Eq` lookup, not iteration order, and the emitted-artifact hash remains exact.

Across three fixed one-worker Lattice profiles, the standard SipHash leaf falls
from 30--34 samples to 27--28, and the `BodyIdentityPool` paths disappear from
that leaf. Sixteen balanced alternating release pairs improve by 1.12% at the
paired median, with 1.14% paired MAD and a -5.62% to +4.55% range. Median peak
RSS falls by 2.17 MiB. Three allocation-accounting comparisons are neutral in
call count and add 0.17--0.41 MiB of requested bytes (at most 0.02%). Every
deterministic compiler-work counter and emitted executable byte remains
identical.

RUE-1392 removes the liveness solver's redundant convergence sweep for
forward-only control flow. With no back edge, instruction order is already a
topological order, so one reverse sweep visits every successor before its
predecessors and computes the exact fixed point. Looped MIR retains the
existing iterative solver. Focused work-accounting tests record one sweep for
acyclic control flow and three for the back-edge fixture.

Sixteen balanced alternating ordinary-release Lattice pairs improve by 0.61%
at the paired median, with 1.40% paired MAD and a -5.52% to +2.53% range. Median
peak RSS falls by 0.86 MiB, within dispersion. Three allocation-accounting
comparisons are neutral, and every published compiler-work counter and emitted
executable byte remains identical.

RUE-1393 keeps the shared instruction scheduler's bounded physical-register
facts inline. Each x86-64 and AArch64 MIR instruction reads or writes at most
three physical registers; a shared hard-bounded list now represents those
facts without a per-instruction heap allocation. The list panics on a fourth
entry rather than silently spilling if MIR grows. Static clobber tables are
borrowed directly instead of copied into another temporary vector.

Three fixed one-worker Lattice probes save 462,033--462,367 allocator calls
(2.83%) and 3.44--3.62 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.31% paired median, with 1.87%
paired MAD and a -10.12% to +5.07% range. Median peak RSS moves +2.31 MiB within
dispersion. Every published compiler-work counter and emitted executable byte
remains identical.

RUE-1394 applies each completed block schedule to the MIR instruction vector in
place. The scheduler now records the dense old-index-to-new-index permutation
while it reads the unchanged blocks, then realizes its cycles with swaps. This
removes the second full-width instruction vector, every instruction clone, and
the adapter's `Inst: Clone` requirement without changing dependency discovery,
priority, barriers, or tie-breaking.

Three fixed one-worker Lattice probes keep allocation calls neutral while
reducing requested bytes by 8.29--8.45 MiB. Sixteen balanced alternating
ordinary-release pairs are clock-neutral on an unusually noisy host at a
-0.83% paired median, with 5.99% paired MAD and a -19.89% to +30.97% range.
Median peak RSS moves +3.05 MiB within dispersion. Every published
compiler-work counter and emitted executable byte remains identical.

RUE-1395 keeps the common scheduler dependency-edge lists inline. A direct
one-worker AArch64 Lattice graph-shape probe counted 11,860 scheduled blocks
and 203,330 MIR nodes: 93.6% of incoming degrees and 88.6% of outgoing degrees
are at most two. Each shared scheduler node now carries two inline indices for
each direction while uncommon higher-degree lists retain the same spillable
representation and edge-linear scheduling behavior.

Three fixed one-worker allocation probes save 390,689--391,167 calls
(2.46%) and 8.52--8.83 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a +0.19% paired median, with 2.98%
paired MAD and a -20.15% to +8.39% range on the loaded host. Median peak RSS
falls by 0.41 MiB within dispersion. Every published compiler-work counter and
emitted executable byte remains identical.

RUE-1397 narrows the remaining durable-key comparison leaf in body-local
semantic materialization. `SemanticImportEpoch` already sorts exact nominal
and callable facts before assigning local IDs, but then stored those two
stable-to-local joins in ordered trees used only for lookup. Independently
keyed, pre-sized AHash maps now provide expected constant-time joins; sorted
input remains the sole authority for deterministic local ID and symbol order.
Small builtin/module registries and compact reverse joins remain unchanged.

Three fixed one-worker allocation probes save 4,582--4,994 calls and
1.55--1.79 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a +0.50% paired median, with 3.31%
paired MAD and a -18.33% to +11.91% range on the loaded host. Median peak RSS
falls by 6.54 MiB, within dispersion. Across three follow-up profiles,
`StableDefinitionKey::cmp` falls from 17/17/8 top-of-stack samples to 5/0/0.
Every published compiler-work counter and emitted executable byte remains
identical.

RUE-1398 keeps the common dependency-reader lists in the shared instruction
scheduler inline. Dependency construction records every instruction that has
read a physical register or the condition flags since their last write so a
later write cannot move above those readers. These transient lists now carry
two instruction indices inline while uncommon longer runs retain the same
spillable representation and dependency order.

Three fixed one-worker allocation probes save 122,297--122,347 calls (0.79%)
and 2.32--2.48 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a +0.06% paired median, with 0.84%
paired MAD and a -12.03% to +10.39% range. Median peak RSS moves +1.56 MiB
within dispersion. Every published compiler-work counter and emitted
executable byte remains identical.

RUE-1399 walks instruction-scheduler basic blocks directly instead of first
materializing every block start in a temporary vector. Each barrier closes the
current block, whose non-barrier interior is scheduled immediately while the
original MIR remains unchanged; the final non-barrier suffix follows the same
path. The completed dense permutation is still applied only after every block
has been inspected.

Three fixed one-worker allocation probes save 4,896--5,265 calls and
0.49--0.83 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.43% paired median, with 0.82%
paired MAD and a -3.71% to +9.47% range. Median peak RSS falls by 0.73 MiB
within dispersion. Every published compiler-work counter and emitted
executable byte remains identical.

RUE-1400 reuses the instruction scheduler's per-register map storage across
basic blocks in one function. Each dependency graph still clears every writer
and reader fact before inspecting a block, so no dependency crosses a control-
flow barrier; only the hash-table allocation survives for the next block. The
test-only graph adapter retains its fresh-tracker behavior.

Three fixed one-worker allocation probes save 39,285--39,441 calls and
7.96--8.11 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.21% paired median, with 1.25%
paired MAD and a -11.30% to +12.37% range. Median peak RSS falls by 2.01 MiB
within dispersion. Every published compiler-work counter and emitted
executable byte remains identical.

RUE-1401 reuses the instruction scheduler's dense dependency-graph storage
across basic blocks in one function. Retained node slots reset their incoming
and outgoing edges, priority, and latency before reuse; the edge-deduplication
table is likewise filled with its empty sentinel for every block. The storage
lifetime remains bounded by one function, and scheduler policy is unchanged.

Four fixed one-worker allocation probes save 35,752--36,052 calls and
11.89--12.05 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.68% paired median, with 1.20%
paired MAD and a -5.44% to +2.36% range. Median peak RSS falls by 2.62 MiB
within dispersion. Every published compiler-work counter and emitted
executable byte remains identical.

RUE-1402 reuses the instruction scheduler's list-scheduling storage across
basic blocks in one function. The output order, remaining-dependency counts,
and ready heap are logically cleared before each block, while their backing
allocations remain available until that function's scheduling completes.

Four fixed one-worker allocation probes save 28,390--28,683 calls and
2.60--2.74 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.68% paired median, with 1.13%
paired MAD and a -10.96% to +5.83% range. Median peak RSS falls by 0.68 MiB
within dispersion. Every published compiler-work counter and emitted
executable byte remains identical.

RUE-1403 gives ordinary CFG optimization a no-accessor path. When the optimized
key has no accessor dependencies, immutable strings, local atoms, codegen
metadata, warnings, and destructor targets remain shared instead of being
cloned into mutable collections that cannot change. CFG and optimization
ownership, query granularity, failure behavior, and the accessor path remain
unchanged.

Four fixed one-worker allocation probes save 74,544--74,624 calls and
15.68--15.86 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs have a favorable but still clock-neutral -0.95% paired
median, with 1.42% paired MAD and a -4.92% to +6.84% range. Median peak RSS
falls by 15.02 MiB. Every published compiler-work counter and emitted
executable byte remains identical.

RUE-1409 makes liveness dataflow storage reflect control-flow shape. Acyclic
MIR retains only live-in rows, derives live-at in forward order while every
successor row is still unmodified, and materializes live-out only when debug
presentation requests it. Cyclic MIR still publishes both tables, but derives
live-out once after live-in reaches its fixed point instead of rewriting both
tables on every sweep.

Four fixed one-worker allocation probes save 108,574--108,897 calls and
14.94--15.15 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.50% paired median, with 1.22%
paired MAD and a -5.08% to +14.67% range. Median peak RSS falls by 10.91 MiB.
Every published compiler-work counter and emitted executable byte remains
identical.

RUE-1411 shares one immutable `BodyQueryKey` payload across the independently
stamped body-projection families. The first family miss formats its diagnostic
identity into that shared payload; later body-input, canonical-body,
transaction, reference, anonymous-production, source, bundle, CFG, and codegen
memo nodes retain the same `Arc<str>` instead of repeating the deep formatting
and string allocation. Typed key equality remains the sole memo authority.

Four fixed one-worker allocation probes save 66,845--67,191 calls and
13.61--13.83 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a +0.21% paired median, with 0.54%
paired MAD and a -12.43% to +28.94% range on the loaded host. Median peak RSS
falls by 7.52 MiB. The display-identity counters remain logically unchanged:
they count the memo nodes and key bytes named by the query graph, not duplicate
physical string allocations. Every other published compiler-work counter and
emitted executable byte remains identical.

RUE-1412 completes each body-local semantic epoch's nominal universe behind
one type-pool transaction. The constructor previously opened one transaction
per struct or enum; because every rollback snapshot deep-clones the growing
pool, local materialization repeated an increasing prefix of type metadata for
every nominal. The existing single-operation completion APIs remain
transactional, while the unpublished constructor now has one atomic batch
boundary for every shape and destructor assignment.

Four fixed one-worker Lattice allocation probes save 119,411--119,633 calls
and 36.31--36.48 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.38% paired median, with 0.71%
paired MAD and a -4.86% to +1.35% range. Median peak RSS falls by 1.07 MiB.
Every published compiler-work counter and emitted executable byte remains
identical.

RUE-1414 validates each durable type-export graph once at its checked root.
Projection previously called the checked export boundary recursively for every
array, pointer, and slice child, so a nested structural chain revalidated every
remaining suffix and accumulated quadratic validation work in its depth. The
validated projection helper now walks that immutable graph once after the root
boundary has established completeness and pool ownership; nominal and module
export joins still fail closed independently.

The maintained Lattice graph is shallow enough that four fixed one-worker
allocation probes move within run-to-run dispersion, with a median reduction
of 256 calls and 0.10 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.22% paired median, with 0.56%
paired MAD and a -1.31% to +5.92% range. Median peak RSS moves +1.48 MiB within
dispersion. Every published compiler-work counter and emitted executable byte
remains identical. The architectural result is the linear validation bound,
not a claimed clock improvement on the current corpus.

RUE-1419 makes each body-pool anonymous identity own one successful registry
publication. Anonymous structs must publish a recursive shell before resolving
their fields, while anonymous enums publish after resolving their payloads;
each minting arm now performs its required registration, and the shared success
path no longer clones and rehashes the complete producer key only to replace
the same forward entry. Rollback and poisoning remain in the shared failure
path.

Four fixed one-worker allocation probes save 42,241--42,492 calls and
1.86--2.06 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are favorable but clock-neutral at a -0.97% paired
median, with 1.43% paired MAD and a -2.94% to +1.96% range. Median peak RSS
moves +0.71 MiB within dispersion. Every published compiler-work counter and
emitted executable byte remains identical.

RUE-1420 batches the two task-local work-counter updates that advance together
for every dependency inspected by retained-terminal validation. Each traversal
accumulates its exact attempted dependency prefix and publishes that prefix to
`dependency_observations` and `registry_probes` once at exit, including early
returns and unwinding, instead of performing two atomic read-modify-write
operations per edge. Fixed one-worker cold Lattice inspects 614,202 dependency
edges, so the common complete traversal removes roughly 1.23 million atomic
updates without weakening the counter contract.

Four fixed one-worker allocation probes are neutral, with a paired median of
+28 calls and +0.02 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.01% paired median, with 0.65%
paired MAD and a -3.19% to +1.54% range. Median peak RSS moves +1.02 MiB within
a 2.51 MiB paired MAD. Every published compiler-work counter and emitted
executable byte remains identical.

RUE-1421 separates candidate-root proof from descendant retention authority.
An exact task-local endorsement can prove a retained candidate root current,
but a published fallback pin set deliberately cannot prove that root's direct
inputs and dependency edges. Candidate selection previously scanned every
fallback anyway, only to send both `Borrowed` and `Missing` through the same
ordinary validation path. It now tests exact task-local authority alone, while
recursive certificate validation retains the full fallback-aware lookup where
borrowed authority can actually skip work.

Four fixed one-worker allocation probes are neutral, with paired medians of
-38 calls and -0.02 MiB of requested bytes. Sixteen balanced alternating
ordinary-release pairs are clock-neutral at a -0.38% paired median, with 0.90%
paired MAD and an outlier-sensitive -11.27% to +6.36% range. Median peak RSS is
unchanged at -0.03 MiB paired. Every published compiler-work counter and
emitted executable byte remains identical.

RUE-1422 derives four validation totals from their independent outcome
counters instead of storing and updating duplicate task-local atomics.
`traversals`, `node_visits`, `memo_misses`, and `registry_probes` remain public
and byte-identical; snapshot and transfer compute their exact sums with
saturating arithmetic. An outcome guard records successful, dirty, error, and
unwind exits exactly once, so every started traversal still contributes one
outcome. This removes four atomic fields (32 bytes) from every task accumulator
and 1,116,430 redundant atomic updates from the fixed cold Lattice build:
154,956 traversal totals, 614,202 node-visit totals, 55,737 memo-miss totals,
154,244 successful-root registry totals, and 137,291 non-empty dependency-batch
registry totals.

Four paired allocation probes are allocation-count neutral and save
2.90--3.11 MiB of requested bytes. Sixteen balanced alternating ordinary-
release pairs are clock-neutral at a -0.45% paired median, with 1.90% paired
MAD; median peak RSS falls 2.26 MiB and retired instructions fall 0.08%.
Every published compiler-work counter and emitted executable byte remains
identical.

RUE-1424 specializes hashing for the retained-pin index's semantic-stamp keys.
Every key is a fixed-width `(node incarnation, stamp)` pair: the runtime assigns
the unique incarnation, and retention bounds limit the historical stamps held
for each node. A private hasher mixes both components while exact key equality
remains authoritative; caller-controlled typed query maps retain their
randomized hashers.

Eight balanced fixed one-worker cold Lattice pairs reduce retired instructions
by a 0.565% paired median with 0.028% paired MAD. Clock moves -0.97% with 0.95%
paired MAD, while peak RSS is neutral at +0.02% with 0.30% paired MAD. Four
allocation-accounted pairs are count-neutral and save 0.16 MiB requested at
the median. Every compiler-work counter, emitted-output metric, and executable
byte remains identical.

RUE-1423 gives each provider body request one success cache for named-import
identity closures. An explicit in-progress state breaks recursive type cycles;
the complete state is published only after every nested field succeeds, so
repeated signatures no longer re-query and re-materialize a nominal and its
recursive fields. A failed outer walk clears the request-local cache, preserving
retry and error behavior without retaining partial cycle members.

On fixed one-worker cold Lattice this removes 2,425 semantic-provider
materializations, 4,850 declaration-fact reads, and 9,696 query reuses. Eight
balanced alternating release pairs reduce retired instructions by a 0.212%
paired median with 0.055% paired MAD. Clock is neutral at the paired median;
peak RSS is neutral at -0.014% with 0.142% paired MAD. Four allocation-accounted
pairs save a median 21,951 allocations and 1.99 MiB of requested bytes. Source
metrics, emitted-output metrics, and executable bytes remain identical.

RUE-1425 derives validation-endorsement probe totals from their disjoint hit
and miss outcomes. A successful authority lookup previously updated the total
probe counter and then the hit counter, even though every active lookup has
exactly one outcome. Task-local accumulation now records only that outcome;
snapshot and transfer preserve the public probe total by summing hits and
misses with saturating arithmetic.

Fixed one-worker cold Lattice performs 745,097 endorsement probes, including
546,386 hits that bypass validation. The change therefore removes exactly
546,386 redundant task-local atomic updates. Sixteen balanced alternating
release pairs are clock-neutral at a 0.00% paired median with 1.46% paired MAD;
retired instructions are neutral at +0.038% with 0.023% paired MAD, and peak
RSS is neutral at +0.36% with 0.38% paired MAD. Four allocation-accounted pairs
are neutral at median deltas of +48 calls and +0.06 MiB requested. Every
published compiler-work counter, emitted-output metric, and executable byte
remains identical.

RUE-1426 derives validation-demand totals from disjoint execution outcomes.
The task accumulator records reuse, compute, join, and abort directly; a small
unwind-safe guard records the exceptional case where a retired demand or
unwind produces no query result. Snapshot and transfer sum those categories
to preserve the public count of demands issued without a second hot-path
update.

Fixed one-worker cold Lattice issues 55,737 validation demands, all answered
by retained-terminal reuse, so the change removes exactly 55,737 redundant
task-local atomic updates. Sixteen balanced alternating release pairs are
clock-neutral at a +0.18% paired median with 0.90% paired MAD. Retired
instructions are neutral at -0.005% with 0.049% paired MAD, and peak RSS is
neutral at +0.10% with 0.22% paired MAD. Four allocation-accounted pairs are
neutral at median deltas of -63 calls and -0.01 MiB requested. Every published
compiler-work counter, emitted-output metric, and executable byte remains
identical.

RUE-1428 memoizes successful anonymous-method endpoint installation within one
provider body request. Durable type resolution already interns recursive
nominal shells in the request's type pool; once every method signature for one
compact owner type has been installed, later encounters can reuse those facts
instead of repeatedly re-issuing its identity, formatting and interning its
callable names, cloning member keys, allocating parameter ranges, and probing
the endpoint tables. Failed walks are not cached, so retry and error behavior
remain unchanged.

Sixteen balanced fixed one-worker cold Lattice pairs reduce retired
instructions by a 1.674% paired median with 0.052% paired MAD. Clock is neutral
at a -1.03% paired median with 1.76% paired MAD, while peak RSS is neutral at
+0.20% with 0.32% paired MAD. Four allocation-accounted pairs save
336,987--337,377 allocations and 24.46--24.61 MB of requested bytes. Every
published compiler-work counter, emitted-output metric, source metric, and
executable byte remains identical.

RUE-1429 certifies an immutable frozen type pool once. The successful semantic
boundary already requires every pool entry and reachable structural edge to be
complete and free of recovery types, but layout and ABI queries subsequently
re-walked each requested type graph. Freezing now validates the whole universe
with one shared visit set in O(types + edges); successful pools need only check
the compact root handle on later queries. Recovery pools retain the exact deep
root validation path, so one invalid entry does not taint unrelated roots.

Sixteen balanced fixed one-worker cold Lattice pairs reduce retired
instructions by a 0.474% paired median with 0.062% paired MAD. Clock is neutral
at a -1.01% paired median with 2.02% paired MAD, and peak RSS is neutral at
-0.11% with 0.65% paired MAD. Four allocation-accounted pairs are neutral,
with deltas of -32 to +319 calls and -0.01 to +0.13 MB requested. Every
published compiler-work counter, emitted-output metric, source metric, and
executable byte remains identical.

RUE-1430 carries exact callable type fragments with the canonical semantic-
signature projection that already parses them. Body-local materialization
previously queried the same declaration candidate again and rebuilt a parser
and interner to recover dependent parameter/result syntax that cannot be
reconstructed from reduced comptime placeholders. Durable function and method
facts now transport the canonical fragments directly; raw declaration syntax
remains an internal dependency of the semantic-signature query instead of a
peer dependency of every consuming body.

Cold one-worker Lattice removes 2,738 declaration-identity fact reads, 5,476
query reuses, and 2,738 redundant signature parser invocations. Sixteen
balanced release pairs reduce retired instructions by a 2.262% paired median
with 0.043% paired MAD and compile clock by 2.20% with 0.63% paired MAD. Peak
RSS is neutral at -0.22% with 0.34% paired MAD. Four allocation-accounted pairs
save 478,839--479,274 allocations and 109.31--109.61 MB requested. Every other
published compiler-work counter, emitted-output metric, source metric, and
executable byte remains identical.

RUE-1431 derives flattened ABI slot widths once alongside the canonical
by-value containment graph. Complete struct, enum, and array entries already
have enough alignment padding for the four-byte width, so frozen consumers
replace repeated recursive aggregate walks with O(1) reads without enlarging a
retained type entry or adding a side table. Provisional semantic construction
keeps the recursive fallback until containment metadata is complete.

Sixteen balanced fixed one-worker cold Lattice pairs reduce retired
instructions by a 0.369% paired median with 0.089% paired MAD. Compile clock is
neutral at -0.68% with 1.78% paired MAD. Peak RSS is neutral at +0.18% with
0.45% paired MAD; the baseline and candidate medians are 403.5 and 404.7 MiB
with cross-pair outliers larger than that difference. Four allocation-accounted
pairs are neutral at -38 to +153 calls and -0.07 to +0.05 MB requested. Every
published compiler-work counter, emitted-output metric, source metric, and
executable byte remains identical.

RUE-1432 carries each anonymous nominal's exact body-local materialization name
with the durable fact that establishes its identity. Full and opaque fact
selection share that immutable name, so body materialization no longer repeats
the full structural Debug formatting for every selected local fact. The name is
a derived cache rather than semantic identity: equality, ordering, and hashing
retain the pre-cache field set, and the cached bytes are included in retained-
charge accounting. This preserves the historical spelling without deciding
the separate canonical anonymous-symbol question tracked by RUE-1295.

Sixteen balanced fixed one-worker cold Lattice pairs reduce retired
instructions by a 1.485% paired median with 0.057% paired MAD. Compiler clock
moves -1.35% with 1.37% paired MAD, while peak RSS falls 2.45% with 0.25%
paired MAD. Four allocation-accounted pairs save 113,732--114,528 allocations
and 39.46--39.70 MB of requested bytes. Every published compiler-work counter,
emitted-output metric, source metric, and executable byte remains identical.

RUE-1433 derives the semantic provider's declaration-fact aggregate from its
four disjoint fact-family counters. Identity, signature, type/well-formedness,
and const/comptime reads already partition the public total exactly; storing
and updating a fifth aggregate atomic duplicated that work. Drop/copy metadata
now classifies its backing nominal-signature read explicitly, and a focused
differential covers that composite path as well as the exact sum invariant.

Fixed one-worker cold Lattice removes exactly 52,337 redundant atomic updates.
Sixteen balanced release pairs are clock-neutral at a -0.82% paired median with
0.92% paired MAD. Retired instructions are neutral at -0.020% with 0.029%
paired MAD, and peak RSS is neutral at -0.10% with 0.24% paired MAD. Four
allocation-accounted pairs are neutral at -170 to -72 calls and -0.03 to
+0.06 MB requested. Every published compiler-work counter, emitted-output
metric, source metric, and executable byte remains identical.

RUE-1434 keeps repeated semantic-nucleus reads inside one body request on the
terminal that request has already observed. The exact provider remains the
canonical query consumer on every miss; a fixed eight-entry direct-mapped
cache merely reuses the immutable terminal for an immediately repeated key.
Deterministic keyed hashing makes the work result repeatable, and the inline
slots keep the cache's memory bounded independently of body size.

An unbounded request-local map was rejected despite removing 30,526 Lattice
query reuses because it added about 34 MB of requested allocation traffic. The
bounded form removes exactly 19,486 query-runtime reuses. Sixteen balanced
release pairs are clock-neutral at +0.51% with 1.46% paired MAD; retired
instructions are neutral at -0.046% with 0.035% paired MAD, and peak RSS is
neutral at -0.16% with 0.34% paired MAD. Four allocation-accounted comparisons
are neutral at -211 to +274 calls and -0.20 to +0.00 MB requested. Semantic-
provider counters, every compiler-work counter other than the removed reuses,
source/output metrics, and executable bytes remain identical.

RUE-1435 sizes that same bounded cache from the maintained scaling curve rather
than leaving its initial capacity as a guess. Moving from eight to 16 slots
removes another 835, 1,452, 2,855 and 3,622 query-runtime reuses from Ruelex
through Lattice. Thirty-two slots remove 1,237, 2,079, 4,144 and 7,561 reuses
relative to eight, but twelve alternating Lattice pairs raise peak RSS by a
consistent 0.54% paired median with 0.24% paired MAD. Doubling again to 64
removes only another 349, 437, 800 and 1,551 reuses respectively. Both larger
capacities are rejected: 32 crosses the no-regression memory boundary and 64
has sharply diminishing work reduction per retained inline slot.

Twelve alternating fixed one-worker Lattice comparisons of eight versus 16
slots are clock-neutral at a 0.00% paired median with 1.57% paired MAD; retired
instructions are neutral at -0.021% with 0.029% paired MAD, and peak RSS is
neutral at -0.22% with 0.51% paired MAD. Four allocation-accounted pairs are
neutral at median deltas of +200 calls and +0.10 MB requested. The complete
capacity curve preserves semantic-provider counters, source/output metrics,
and executable bytes. The selected 16-slot cache remains request-scoped,
allocation-free, deterministic and bounded independently of body size; misses
still cross the canonical query path.

RUE-1436 makes a validation certificate's retention invariant eager. A node
previously rescanned its retained-attempt deque on every certificate hit to
prove that the certified terminal was still present. All attempt removal now
passes through one helper that clears the certificate exactly when its backing
terminal leaves, so the hot certificate lookup becomes O(1). A focused
regression removes an older terminal before the certified terminal and proves
that invalidation is neither premature nor omitted.

Fixed one-worker cold Lattice records 558,465 validation-certificate memo hits
and 55,149 retention-proof reacquisition misses. Both outcomes previously
completed the retained-attempt scan, so the change removes at least 613,614
scans; current-revision certificates already missing their terminal were
included in the broader certificate-miss counter and can only raise that total.
Sixteen balanced alternating release pairs reduce retired instructions by a
0.171% paired median with 0.060% paired MAD. Compiler clock moves -1.03% with
1.03% paired MAD, and peak RSS is neutral at -0.60% with 0.74% paired MAD. Four
allocation-accounted pairs are neutral at a median delta of +44 calls and -0.05
MB requested. Every published compiler-work counter, source/output metric, and
executable byte remains identical.

RUE-1437 applies the body-local hashing policy to `ProviderBodyHost`'s
request-local exact-lookup registries. RUE-1391 deliberately limited the first
step to `BodyIdentityPool`; the remaining standard SipHash profile leaf now
includes provider import-nominal registration and anonymous-endpoint lookup.
Public/output collections and trait-exposed maps remain unchanged. The
converted registries do not use iteration order as a semantic input: exported
token vectors are immediately reconstructed as lookup maps, while referenced
definitions and values are consumed as canonicalized dependency collections.

Sixteen balanced alternating fixed one-worker cold Lattice pairs reduce
retired instructions by a 0.572% paired median with 0.049% paired MAD. Compiler
clock moves -1.05% with 1.01% paired MAD, and cycles move -1.29% with 0.54%
paired MAD. Peak RSS is neutral at +0.23% with 0.31% paired MAD. Four
allocation-accounted pairs are neutral at median deltas of +76 calls and
-0.03 MB requested. Every published compiler-work counter, source/output
metric, and executable byte remains identical.

RUE-1438 extends the runtime-owned retained-identity hasher from semantic
`(incarnation, stamp)` authority to the adjacent exact-terminal
`(incarnation, stamp, revision)` deduplication index. Every component is minted
by the runtime, exact equality remains authoritative, and caller-controlled
query-key maps stay randomized. This closes the published `RetainedPinSet`
counterpart to RUE-1378's request-local task lease index without adding another
hashing policy.

A randomized AHash prototype reduced instructions by the same amount but moved
peak RSS +0.43% at the paired median, so it was rejected. Sixteen balanced
alternating fixed one-worker cold Lattice pairs with the existing zero-sized
runtime hasher reduce retired instructions by a 0.498% paired median with 0.049%
paired MAD. Compiler clock moves -1.05% with 1.04% paired MAD, cycles move
-0.76% with 0.67% paired MAD, and peak RSS moves -0.52% with 0.23% paired MAD.
Four allocation-accounted pairs are neutral at a median -82 calls and save
0.15 MB of requested bytes. Every compiler-work counter,
source/output metric, and executable byte remains identical.

RUE-1439 tested replacing `RetainedPinSet`'s remaining runtime-identity set
with the exact empty / one-runtime / mixed-runtime state its consumers need.
Across two independent sets of 16 balanced alternating fixed one-worker cold
Lattice pairs, the candidate reduced retired instructions by a 0.145% paired
median but increased peak RSS by a repeatable 0.60% paired median with 0.30%
paired MAD. The representation change was rejected under the no-regression
gate.

RUE-1440 keeps the fail-closed set representation but applies the query
runtime's existing runtime-owned `u64` hasher to it. Sixteen balanced
alternating fixed one-worker cold Lattice pairs reduce retired instructions by
a 0.068% paired median with 0.044% paired MAD. Compiler clock, cycles, and peak
RSS are neutral at 0.00%, +0.07%, and +0.09% paired medians respectively. Four
allocation-accounted pairs are neutral at a median +7 calls and save 0.13 MB of
requested bytes. Every compiler-work counter, source/output metric, and
executable byte remains identical.

RUE-1441 tested lasso's supported keyed-AHash feature for every shared string
interner. Sixteen balanced alternating fixed one-worker Lattice pairs reduced
retired instructions by 1.562% and compiler clock by 2.11% at the paired
median. Twenty-four Harbor pairs confirmed a 1.630% instruction reduction, but
also raised peak RSS by a repeatable 0.42% paired median with 0.20% paired MAD.
Allocation accounting and every compiler-work/output invariant were neutral.
The feature was rejected under the no-regression memory boundary; the measured
speed/memory tradeoff remains available for a future explicit policy decision.

RUE-1442 tested preallocating the stable semantic-symbol encoder after cold
samples found repeated `String` growth in that path. Both tested capacities
were counterproductive. A 128-byte start regressed retired instructions by
0.5332% (MAD 0.0596%) and RSS by 0.2792% (MAD 0.6203%) over 16 balanced Lattice
pairs; 64 bytes regressed instructions by 0.7550% (MAD 0.0580%) and RSS by
1.2447% (MAD 0.3614%) over 12 pairs. Both preserved work and output, but the
existing formatting-led growth remains the better cold-compile tradeoff.

RUE-1443 removes one heap allocation from every registered validation
traversal. Validation proof states now remain inline in each task's lexical
stack; structured batch children link to their parent task and propagate
unregistered or retryable state through that acyclic ancestry. This preserves
the existing nested-batch proof semantics while avoiding shared
`Arc<AtomicU8>` state on ordinary traversals. Against the exact RUE-1440 parent,
20 balanced Lattice pairs reduced retired instructions by 0.2067% (MAD 0.0313%)
with neutral wall time, cycles, and RSS. Four allocation-accounting pairs
removed a median 155,870.5 allocation calls and 5,286,240 requested bytes.
Compiler work, source metrics, emitted-output metrics, and executable digest
remained exact.

RUE-1444 applies the same retained-identity hasher to the temporary maps and
visited set built by terminal-cone promotion. Those structures contain only
runtime-owned incarnation, semantic-stamp, and revision tuples, but previously
paid SipHash while selecting and walking every published root cone. Sixteen
balanced Lattice pairs reduced retired instructions by 1.7378% (MAD 0.0521%),
compiler clock by 1.5903% (MAD 1.5903%), and cycles by 1.2335% (MAD 0.9286%);
RSS was neutral. Four allocation-accounting pairs were neutral at a median
-33.5 calls and -27,530 requested bytes. Compiler work, source metrics,
emitted-output metrics, and executable digest remained exact.
RUE-1445 replaces the instruction scheduler's per-class physical-register hash
maps with lazily grown dense vectors indexed by each backend's compiler-owned
`repr(u8)` register identity. The vectors retain storage across basic blocks,
while register classes remain disjoint and every dependency edge and schedule
is unchanged. Sixteen balanced Lattice pairs reduced retired instructions by
0.9496% (MAD 0.0319%), compiler clock by 1.0582% (MAD 1.0582%), cycles by
0.7317% (MAD 0.7106%), and RSS by 0.3146% (MAD 0.4213%). Four
allocation-accounting pairs removed a median 17,737 calls while requested bytes
were neutral at +143,128. Compiler work, source metrics, emitted-output metrics,
and both AArch64 and x86-64 executable digests remained exact.

RUE-1446 keeps each task's first eight nested registered-validation proof states
inline. RUE-1443 removed shared state from each traversal, but the replacement
`Vec<u8>` still allocated its first tiny buffer once a task validated anything;
the inline stack preserves the same lexical proof, locking, batch-parent, and
deep spill behavior. Sixteen balanced Lattice pairs reduced retired
instructions by 0.2373% (MAD 0.0602%); compiler clock, cycles, and RSS were
neutral. Allocation accounting removed a median 94,580.5 calls and 709,786
requested bytes. Compiler work, source metrics, emitted-output metrics, and
executable digest remained exact.

RUE-1447 tested reading the final validation-proof byte once instead of taking
the same task-local mutex separately for the registered-only and retryable
results. Across 32 balanced Lattice pairs, retired instructions and clock were
neutral, while peak RSS increased by a 0.9028% paired median with 0.5174%
paired MAD. Allocation accounting, compiler work, source/output metrics, and
the executable digest were neutral. The candidate was rejected under the
no-regression memory boundary.

RUE-1448 tested applying the query runtime's retained-identity hasher to its
remaining runtime-owned exact-terminal sets. Replacing all four task, batch,
and lexical sets increased peak RSS by a 0.9922% paired median (0.5932% MAD)
over 16 balanced Lattice pairs, with neutral instructions and clock. Keeping
the batch lease tree and task lease hash set unchanged reduced allocator
requests by 3.10 MB but still increased peak RSS by a repeatable 1.2662%
(0.5270% MAD), while instructions and clock remained neutral. Both variants
preserved every compiler-work counter, source/output metric, and executable
byte, but were rejected under the no-regression memory boundary.

RUE-1449 tested replacing the stable symbol encoder's formatted constant
version prefix with a direct string copy. The direct copy lost the formatter's
useful incidental starting capacity: across 16 balanced Lattice pairs it added
a median 41,662 allocation calls and 2.27 MB of requested bytes, increased
retired instructions by 0.1688% (0.0497% MAD), and increased peak RSS by
1.2218% (0.2941% MAD). Compiler clock was neutral and every work,
source/output, and executable invariant remained exact. The candidate was
rejected.

RUE-1327 makes body-local provider module publication linear in the number of
modules. `BodyIdentityPool` now owns exact forward and reverse module/file
indexes, assigns implicit ids with a monotonic cursor, and incrementally
publishes each logical path before recursive nominal minting can render a
qualified symbol. This replaces both the per-provider reverse scan and the
repeated clone-and-replace of the complete path map. A 256-module regression
mixes provider-assigned and pool-assigned ids, exercises immediate destructor
symbol spelling, and verifies that an id reused for another path fails before
minting a nominal shell.

Against the exact RUE-1446 parent, 16 balanced fixed one-worker cold Lattice
pairs reduced compiler clock by a 1.3303% paired median (0.4267% MAD) and cycles
by 1.1103% (0.6636% MAD). Retired instructions and RSS were neutral at -0.0644%
and -0.3196% respectively. Four allocation-accounting pairs removed a median
22,420.5 calls and 1,045,227 requested bytes. Every published compiler-work
counter, source/output metric, and executable byte remained identical.

RUE-1450 gives each semantic body-fact request a small direct-mapped cache for
exact name-lookup terminals. Repeated provider calls now probe with the borrowed
module, namespace, and name, so a hit avoids allocating another owned query key
and crossing the query runtime. The first lookup still records the terminal as
an exact dependency; later hits reuse that already-observed edge. Fixed hash
seeds make collisions and the resulting work count reproducible, while exact
key equality remains authoritative. The maintained cold Lattice curve removed
33,023 query reuses at 8 slots, 35,063 at 16, 39,537 at 32, and 40,655 at 64;
32 is the measured knee before retained state doubles for little additional
work reduction.

Against the exact merged RUE-1327 parent, 16 balanced fixed one-worker cold
Lattice pairs reduced retired instructions by a 0.3993% paired median (0.0467%
MAD). Compiler clock, cycles, and peak RSS were neutral at -0.3782%, -0.3138%,
and +0.0416% respectively. Four allocation-accounting pairs removed a median
39,717.5 calls and 964,574 requested bytes. Query-runtime reuses fell exactly
from 405,868 to 366,331; every other compiler-work counter, source/output
metric, and executable byte remained identical.

RUE-1452 records terminal-lease attempts as disjoint unique and duplicate
outcomes. A duplicate previously incremented the public attempt total and then
incremented the duplicate outcome, even though the total is their exact sum.
Snapshots, task transfer, and aggregate reset now derive the unchanged public
total, matching the validation runtime's traversal, memo, endorsement, and
demand counters. Cold Lattice therefore removes exactly 6,363 redundant
task-local atomic updates from its 199,098 lease observations.

Sixteen balanced fixed one-worker cold Lattice pairs were neutral: compiler
clock +0.1361% paired median (0.5372% MAD), retired instructions -0.0111%
(0.0657% MAD), cycles +0.0703% (0.6177% MAD), and peak RSS +0.2496%
(0.3077% MAD). Four allocation-accounting pairs were also neutral at a median
+198 calls and +37,008 requested bytes. Every published compiler-work counter,
source/output metric, and executable byte remained identical.

RUE-1453 reuses one full-width scratch bitset inside backend liveness dataflow.
The fixed-point transfer previously built `live_out` in one scratch allocation,
cloned its complete register width into a second scratch allocation for every
instruction and sweep, then immediately mutated that clone with defs and uses.
Building the successor union directly in the reusable `live_in` scratch removes
the second allocation and the per-row clone without changing the fixed-point
order, acyclic fast path, or live-set projections.

Against the exact RUE-1452 candidate, 16 balanced fixed one-worker cold Lattice
pairs reduced retired instructions by a 0.2255% paired median (0.0491% MAD).
Compiler clock and cycles were neutral at +0.0000% (1.0485% MAD) and -0.1220%
(0.6329% MAD). Peak RSS improved by 0.4168% (0.2528% MAD), and peak footprint
improved by 0.2856% (0.1430% MAD). Every published compiler-work counter,
source/output metric, and executable byte remained identical.

RUE-1454 bounds exact-root indexing during registered terminal-cone promotion
by the requested roots rather than by every lease held by the publishing task.
Promotion still performs its required linear lease scan to build the
stamp-equivalent dependency index, but it now seeds exact identities from the
roots and fills only those matches during that scan. This removes an exact-key
hash-table entry for every unrelated task lease while preserving exact root
selection, fallback dependency substitution, and fail-closed missing roots.

Against the exact RUE-1453 candidate, 16 balanced fixed one-worker cold Lattice
pairs were neutral for compiler clock (+0.0000%, 0.0000% paired MAD), retired
instructions (+0.0061%, 0.0293% MAD), cycles (-0.1512%, 0.4381% MAD), and peak
RSS (-0.1984%, 0.2794% MAD). Peak footprint improved by 0.7468% (0.2660%
MAD). Four allocation-accounted pairs were neutral in allocation count and
removed 4.38--4.48 MB of requested bytes. Every published compiler-work
counter, source/output metric, and executable byte remained identical.

RUE-1455 skips strict family-retention passes which are already converged: the
live terminal count is at or below the family bound and the publication
watermark is already at the strict next-publication threshold. A stale
geometric watermark still enters the ordinary pass so later publication cannot
hide above the bound. The convergence predicate is one non-generic out-of-line
helper rather than repeated code in every monomorphized query family.

Cold one-worker Lattice removes exactly 9,793 no-op retention passes, reducing
`retention_enforcements` from 11,261 to 1,468 while preserving all 31,549
retention scan-entry visits. Across 32 balanced release pairs, compiler clock
is neutral at +0.0000% paired median (1.0989% MAD), retired instructions at
-0.0085% (0.0609% MAD), cycles at +0.2444% (0.9928% MAD), peak RSS at +0.0060%
(0.3161% MAD), and peak footprint at +0.0601% (0.1746% MAD). Four
allocation-accounted pairs are neutral. Every other compiler-work counter,
source/output metric, and executable byte remains identical.

RUE-1456 carries stable definition and owner names through the durable body
lookup seam as shared immutable handles. The compiler's `StableDefinitionKey`
already owns both names in `Arc<str>`; returning a fresh `String` made
provider-native body materialization allocate and copy equal text only to pass
it immediately into interning, endpoint registration, or diagnostics.

Four allocation-accounted cold Lattice pairs remove 5,943--6,518 allocation
calls. Requested bytes are neutral, ranging from 112,143 fewer to 42,873 more.
Across 16 balanced release pairs, compiler clock is neutral at -0.8376% paired
median (0.7210% MAD), retired instructions at -0.0307% (0.0593% MAD), cycles at
-0.4515% (0.8319% MAD), peak RSS at +0.2450% (0.4045% MAD), and peak footprint
at -0.0580% (0.1881% MAD). Every compiler-work counter, source/output metric,
and executable byte remains identical.

RUE-1458 makes rooted program-image validation the single producer of each
unit's stable encoded identity. Fresh plan construction previously encoded
every callable again, copied every already-shared defined symbol, and rebuilt
the same identity/symbol sets a third time after raw inputs had passed their
full duplicate, entry-point, and export checks. The retained-plan delta API
keeps its independent fail-closed plan validation because its inputs do not
come from that private fresh-construction path.

Four allocation-accounted cold Lattice pairs remove 8,362--8,758 allocation
calls and 680,862--823,390 requested bytes. Across 16 balanced release pairs,
the `program_image_plan` phase improves by 7.3521% at the paired median (0.6639%
MAD). End-to-end compiler clock is neutral at -0.0765% (1.3312% MAD), retired
instructions at -0.0455% (0.0786% MAD), cycles at +0.3026% (0.8219% MAD), peak
RSS at +0.1173% (0.3600% MAD), and peak footprint at -0.2005% (0.1693% MAD).
Every compiler-work counter, source/output metric, and executable byte remains
identical.

RUE-1459 makes each immutable object projection own its stable content digest.
Fresh program-image assembly previously serialized 1,280 SHA-256 operations
over 3,011,805 already-retained object bytes on cold Lattice, then repeated
that same work whenever a retained session assembled another no-edit plan.
Computing the digest beside object serialization lets the query graph retain
it, distributes cold hashing with object production, and removes hashing from
the single-threaded aggregation tail. Export-thunk and lazy runtime-archive
digests keep their existing ownership and domains.

Across 16 balanced fixed one-worker cold Lattice pairs, the
`program_image_plan` phase improves by 86.6184% at the paired median (0.5715%
MAD). End-to-end compiler clock is neutral at -0.1217% (4.2710% MAD), retired
instructions at -0.1234% (0.1090% MAD), cycles at +0.2814% (2.0873% MAD), and
peak RSS at -0.0873% (0.3057% MAD). Sixteen default-worker pairs independently
improve the plan phase by 86.6063% (0.8512% MAD); whole-compiler clock remains
host-noisy, while cycles improve by 1.6730% (0.6974% MAD). Every compiler-work
counter, source/output metric, and executable byte remains identical.

RUE-1460 keeps the canonical shared defined-symbol owner from
`CfgCodegenDomain` in each `CodegenUnit`. The evaluator previously copied that
symbol into its backend product and then allocated an equal `Arc<str>` even
though the canonical owner remained in scope. Four allocation-accounted cold
Lattice pairs remove 1,144--1,390 allocation calls and 34,292--270,796 requested
bytes. Across 16 balanced fixed one-worker release pairs, end-to-end compiler
clock is neutral at +0.9866% (2.9942% MAD), the `codegen_unit` phase at -0.6757%
(1.7548% MAD), retired instructions at -0.0266% (0.0972% MAD), cycles at
+0.5352% (1.7221% MAD), and peak RSS at -0.5406% (0.5881% MAD). Every
compiler-work counter, source/output metric, and executable byte remains
identical.

RUE-1461 lets codegen-unit fingerprinting borrow the canonical defined symbol
and generated machine code directly. The evaluator previously rebuilt a
`FunctionBackendProduct` solely to replace its name before hashing, allocating
and copying one equal `String` per unit even though both borrowed inputs were
already in scope. Four allocation-accounted cold Lattice pairs remove
1,131--1,318 allocation calls and 155,234--231,202 requested bytes. Across 16
balanced fixed one-worker release pairs, end-to-end compiler clock is neutral
at +2.5015% (3.4540% MAD), the `codegen_unit` phase at +3.3730% (5.3584% MAD),
retired instructions at +0.0470% (0.0875% MAD), cycles at +0.6602% (2.0821%
MAD), peak RSS at -0.0821% (0.6671% MAD), and peak footprint at -0.2669%
(0.1964% MAD). Every compiler-work counter, source/output metric, and
executable byte remains identical.

RUE-1462 gives both provider-backed anonymous-method paths one stack-buffered
spelling for synthetic parameter names. They previously allocated a temporary
`String` for every `arg{index}` immediately before interning it; the interner
needs that text only for the duration of the lookup. Four allocation-accounted
cold Lattice pairs remove 78,369--78,726 allocation calls and
339,360--664,096 requested bytes. Across 16 balanced fixed one-worker release
pairs, end-to-end compiler clock is neutral at +0.3667% (3.7385% MAD), retired
instructions at -0.0781% (0.2808% MAD), cycles at +0.8649% (3.7935% MAD), peak
RSS at +0.6267% (0.4606% MAD), and peak footprint at +0.2693% (1.5085% MAD).
Every compiler-work counter, source/output metric, and executable byte remains
identical.

RUE-1463 makes structured-batch validation leases use unordered exact-identity
membership, matching ordinary task leases. The identity components are assigned
by the runtime and no consumer observes their order, so the previous tree paid
ordered comparisons solely as an implementation accident. Across 16 balanced
fixed one-worker cold Lattice pairs, retired instructions improve by 0.1030%
(0.0690% MAD); end-to-end clock is neutral at -0.8874% (2.6105% MAD), cycles at
-0.5005% (1.2074% MAD), peak RSS at -0.7588% (1.0378% MAD), and peak footprint
at -1.1603% (1.2188% MAD). Four allocation-accounted pairs remove 890--1,292
allocation calls while hash-table capacity adds 6.57--6.75 MB of cumulative
requested bytes (0.28% of the run); the peak-memory comparisons above remain
neutral. Every compiler-work counter, source/output metric, and executable byte
remains identical.

RUE-1464 reuses the owned struct-symbol buffer when constructing provider
member callable names. The canonical spelling helper previously allocated an
owner string, formatted a second `Owner.method` / `Owner::method` string, and
immediately dropped the owner. Extending that buffer in place preserves the
single spelling path while removing the redundant intermediate allocation.
Four allocation-accounted cold Lattice pairs remove 112,908--113,474
allocation calls and 5.05--5.30 MB of requested bytes. Across 16 balanced
fixed one-worker pairs, retired instructions improve by 0.4428% (0.1301%
MAD); clock is neutral at -0.1323% amid substantial host noise, cycles at
+0.1649%, peak RSS at -0.6822%, and peak footprint at -0.0373%. Every
compiler-work counter, source/output metric, and executable byte remains
identical.

RUE-1465 defers each immutable object projection's durable content digest until
a caller compares program-image plans. Fresh linking consumes the retained
object bytes directly, so eagerly hashing 1,280 objects and 3,011,805 bytes on
cold Lattice prepared identity for only the dormant later-linking seam. The
projection still owns and memoizes the same domain-separated SHA-256 digest;
ordinary exact object equality and fresh plan construction do not force it,
while plan delta comparison does.

Across 16 balanced fixed one-worker cold Lattice pairs, retired instructions
improve by 0.5993% (0.0651% MAD) and cycles by 0.2315% (0.7333% MAD).
End-to-end clock is neutral at -0.1895% (1.4135% MAD), peak RSS at +0.3510%
(0.3469% MAD), and peak footprint at +0.8275% (0.3708% MAD). Four
allocation-accounted pairs are neutral at +101--320 calls (under 0.003%) and
-3.3--118.6 KB requested bytes. Every compiler-work counter, source/output
metric, and executable byte remains identical.

RUE-1466 removes the redundant `live_out` set-bit walk from cyclic MIR
live-range construction shared by both backends. The transfer equation
`live_in = uses ∪ (live_out - defs)` proves that every live-out register is
already either live-in or defined at the same instruction, and those two inputs
were already extending the range. Retained per-instruction liveness and debug
views keep their live-out table; only the duplicate range pass disappears.

Across 16 balanced fixed one-worker cold Lattice pairs, retired instructions
improve by 0.1051% (0.0868% MAD). End-to-end clock is neutral at +0.7974%
(2.6807% MAD), cycles at +0.4182% (1.5224% MAD), peak RSS at -0.0454%
(0.3189% MAD), and peak footprint at -0.0619% (0.5094% MAD). Four
allocation-accounted pairs are neutral, as expected for a set-bit iteration
reduction. Every compiler-work counter, source/output metric, and executable
byte remains identical.

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
