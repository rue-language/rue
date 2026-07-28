# Query runtime prototype

RUE-1022 tested the execution substrate required by ADR-0063 before any
production `CompilerSession` query family depends on it. The selected substrate
is the dependency-free `rue-query` crate. Phase 1 should integrate the existing
typed compiler families with this crate; it must not keep the current selected-
key executor as a peer database or add another compiler path.

## Decision

Rue needs attempt semantics which are more specific than a generic task pool:

- each request pins one immutable input revision, while compatibility is an
  explicit validation claim rather than equality of revision counters;
- a typed family atomically claims or joins an exact logical key, retains both
  successes and deterministic failures, and publishes cancellation only as a
  non-terminal abort;
- current and last-good selection remains a request/session concern above the
  immutable terminal store;
- dependency observations, semantic diagnostics, structural work, red/green
  stamps, and bounded retention are one atomic publication contract;
- query tasks share one permit budget, including nested dependency work, and a
  parked joiner releases its permit so a queued owner can run with a budget of
  one; and
- imports and other external inputs remain compiler-produced demands. Query
  bodies never acquire filesystem authority, and a pinned attempt cannot see an
  observation published into a successor revision.

The prototype implements these mechanics directly with standard-library
mutexes, condition variables, atomics, and typed family tables. A query body
runs with no runtime, family, or node lock held. `QueryContext` is deliberately
neither `Send` nor `Sync`: one task owns one dependency stack and one permit.
Parallelism comes from independent tasks, not concurrent mutation of a task.

Salsa is not vendored in Rue. Introducing it for this phase would create a
second database beside `CompilerSession` before proving how Rue's attempted and
last-good terminals, round-based import revisions, semantic/presentation
diagnostic split, shared-work cancellation, red/green equality, and protected
bounded retention map onto it. The focused in-house crate is therefore the
smaller Phase 1 integration surface. This is a substrate decision, not
authorization to preserve two query authorities: production families move to
`rue-query`, after which their superseded selected-state execution machinery is
removed.

## Prototype contract

`QueryRuntime` owns the permit budget, cross-task wait graph, task identities,
and structural metrics. `QueryFamily<K, V>` owns exact `K`-keyed memo nodes and
bounded FIFO attempts. `QueryRuntime::request_registered` creates a task pinned
to a `Revision`; `QueryContext::query_registered` requests typed dependencies
in that same task and revision. A registered evaluator is immutable family
policy and receives a non-owning reference to its own family, so recursive
families do not capture themselves. Weak family handles express cross-family
back edges without ownership cycles. The closure-per-request entry points are a
compatibility path for transitional families which do not register evaluators.

Validation demands a dirty registered dependency from its recorded exact key
under the requesting task before accepting or rejecting the parent terminal.
No node or family lock is held across that demand. This lets an equal child
recomputation preserve its red stamp and validate an entire ancestor chain from
a root-only request. Every nested demand also freezes a unique immutable
computed/reused/joined/aborted lifecycle in the top-level request ledger,
including its terminal origin, dependency/input/work prefix, and abort.

Memo lookup uses exact family-local `QueryKey::eq`; its stable identity is only
display text and may collide. It is never a hash or memo authority. The runtime
also assigns each retained exact node incarnation an opaque session-local
generation. Dependency observations include that generation so evicting and
recreating an exact node cannot repeat `(node, stamp)` and falsely validate an
old dependent. The generation is query-control metadata; it does not enter a
canonical compiler artifact or user-visible ordering.

Same-key compatible attempts share one terminal. Incompatible revisions may
compute concurrently. Wait edges are exact task-to-owner edges annotated with
the awaited logical node. A cycle is reported only when adding an edge closes a
path; lack of an execution permit is not a cycle. A task which joins while it
owns a permit releases it before parking and reacquires it before returning to
its body. Nested work by the same task reuses its existing permit.

Publication sorts diagnostics and reduces work by stable identity. Red equality
compares only the family-owned terminal kind, canonical success/failure outcome,
and semantic diagnostic identities and payloads. Dependency and input
observations are provenance used to validate whether recomputation is required;
they do not make equal semantics green. Presentation positions, revision
numbers, attempt order, and structural work are also excluded. A red
recomputation publishes current positions and work while preserving its prior
terminal stamp.

Cancellation is cooperative. Canceling a waiter removes only that waiter. If a
computing owner is canceled, its attempt is removed without publication; a live
waiter wakes, claims, and computes. Panics also remove the in-flight claim before
unwinding.

Retention protects computations, parked waiters, explicit terminal pins, and
explicit current/last-good revision pins. Protected entries may temporarily
exceed the configured limit. The last departing waiter or pin reruns eviction.
Empty nodes are reclaimed after their last active user leaves, so unique-key
churn is bounded. Failures use the same validation, pinning, and eviction rules
as successes. Family and runtime tokens prevent a same-named foreign family
from pinning a terminal it does not own.

## Reproduction and evidence

Run the deterministic adversarial suite and the repository unit suite with:

```text
./buck2 test //crates/rue-query:rue-query-test
scripts/rue quick
```

The focused tests cover exact-key one/many joins, independent-key overlap,
incompatible revisions, a same-task cycle, a cross-task cycle, the adversarial
one-permit queued-owner schedule both with and without a true cross-task cycle,
cancellation of both waiter and owner,
retained failures, terminal/revision pins, zero-retention active waiters,
unique-key churn, eviction/recreation stamp ABA, foreign pins, red publication,
and one-worker/many-worker diagnostic/work equivalence. They use barriers,
channels, and structural counters instead of latency assertions.

The retired prototype runner produced the following historical samples on an
Apple Silicon macOS 26.5.1 development host with Rust 1.92.0:

```json
{"schema":1,"keys":64,"workers":1,"cold_micros":191,"reuse_micros":42,"red_micros":131,"join_micros":55,"checksum":6240,"join_stamp":1,"claims":129,"joins":1,"reuses":64,"green_publications":65,"red_publications":64,"peak_active_bodies":1,"retained_terminals":129,"retained_nodes":65,"evictions":0}
{"schema":1,"keys":64,"workers":8,"cold_micros":418,"reuse_micros":153,"red_micros":177,"join_micros":190,"checksum":6240,"join_stamp":1,"claims":129,"joins":8,"reuses":64,"green_publications":65,"red_publications":64,"peak_active_bodies":5,"retained_terminals":129,"retained_nodes":65,"evictions":0}
```

The structural evidence is the contract: checksum, claims, reuses, green/red
publication counts, retained ownership, and evictions are identical across the
worker counts. The hot-key phase records one join per configured joining worker,
and every waiter observes `join_stamp` 1 from the single owner computation. The
many-worker run also overlapped independent bodies. The microsecond fields are
descriptive samples, not a platform promise. This tiny workload is
synchronization-heavy and is not a claim that more workers improve its elapsed
time.

Phase 1 must map compiler family equality and attempt/current/last-good
publication onto this API, then run the existing differential oracle against
the integrated path. Later phases should replace caller-created worker threads
with Rue's structured scheduler and migrate existing Rayon work into the same
permit budget without changing the claim/join or publication contract.
