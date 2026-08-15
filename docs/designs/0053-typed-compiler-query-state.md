---
id: 0053
title: Typed CompilerSession query state
status: superseded
tags: [architecture, compiler, incremental, tooling]
feature-flag: null
created: 2026-07-15
accepted: 2026-07-15
implemented:
spec-sections: []
superseded-by: 0063
relates: ["ADR-0050", "ADR-0051", "RUE-627", "RUE-720", "RUE-730", "RUE-812"]
---

# ADR-0053: Typed CompilerSession query state

## Status

Superseded by [ADR-0063](0063-parallel-demand-driven-incremental-compilation.md)
on 2026-07-18. ADR-0063 retains invariants 2 through 8 and 10 through 17,
modifies invariants 1, 9, and 18 for per-key parallel execution, and supersedes
invariant 19 by retaining code generation while keeping the first linker path
fresh. RUE-813 through RUE-818 remain the implemented foundation inherited by
the successor design.

## Summary

`CompilerSession` will remain the sole compiler orchestrator and will own one
small in-house typed query database. Every typed query request will have an
immutable key, explicit direct dependencies, and an immutable attempt record
containing its finished outcome, diagnostics, and work. Each query family will
expose the same storage states: unpublished, dirty, computing, success, and
failure. Cancellation will return a computing entry to dirty without
manufacturing a failure.

The current attempt and the last-good artifact will be separate typed
references. A failed or canceled attempt will never replace last-good state,
and last-good state will never be returned implicitly as the result of a failed
current request. Durable declaration, body, specialization, and CFG artifacts
may use last-good state only as validated inputs to the one canonical current
computation.

Rue will not adopt a general query library in this migration. The in-house
store is deliberately smaller than an execution framework: it owns state,
memoization, dependency validation, retention, and attempt publication, while
ordinary `CompilerSession` methods continue to own query order and call the
existing canonical phase functions.

## Context

The current source already establishes most of the semantic boundaries this
design must preserve:

- `CanonicalParseSession` retains a last-successful parsed baseline and does
  not advance it after syntax failure.
- import discovery distinguishes an open staging revision, a closed attempted
  revision, and a closed valid revision; only the closed valid artifact becomes
  the committed import authority;
- merge caches success and failure, while RIR currently caches only success;
- semantic and stable-definition queries search heterogeneous vectors using
  explicit input descriptors and canonical import graphs;
- diagnostic publication separately maintains latest, latest successful, and
  last-good semantic pointers plus a bounded cache;
- semantic dependency manifests and invalidation plans contain stable keys and
  fail-closed blockers, but also contain parallel completeness booleans;
- declaration, body, specialization, and CFG reuse retain last-successful
  candidates and atomically fall back to fresh work when validation fails; and
- `CompilerSessionWork` is mutated throughout lookup, execution, fallback,
  diagnostics, and retention paths.

These are useful policies, not a coherent state model. Correctness currently
depends on `finish_update` knowing which cache fields to clear, query methods
remembering which failures are memoized, diagnostic publication updating the
right generation pointers, and durable caches being advanced at exactly the
right success boundary. A new cache can be stale merely because an invalidation
checklist was not updated.

One part of the current behavior is intentional and must survive the migration:
`finish_update` clears revision-local merge, RIR, semantic, definition, and
manifest entries, but preserves the definition-shard, durable-declaration,
last-successful body, and last-successful CFG baselines. Those preserved values
are validated last-good candidates for a later current attempt, not stale
current results. Dependency-derived invalidation replaces the clearing
checklist without erasing this validated cross-revision reuse boundary.

ADR-0050 defines the stable semantic dependency and durable-artifact boundary.
ADR-0051 defines the staging and committed import-resolution boundary. This ADR
does not replace either decision. It gives their artifacts one storage and
attempt model.

## Decision

### One database, one orchestrator, one phase path

`CompilerSession` remains the public orchestration object. Its methods continue
to express the canonical demand graph:

```text
source inputs -> parse -> import plan/closure
                     |
closed parse + import graph -> merge -> RIR -> semantic/CFG
                                          \-> definitions

semantic + definitions + import graph -> dependency manifest
```

This diagram describes dependencies, not a second phase runner. Existing
canonical functions continue to compute artifacts. The session asks typed
stores for a reusable record or starts one computation, calls those functions,
and atomically publishes the result.

The session will own a `QueryDatabase` containing typed stores plus supporting
attempt indexes. It will not own raw result vectors, individual
diagnostic-generation fields, or a global list of downstream caches to clear.
Storage types do not call compiler phases and do not decide which query to
request next. Phase functions do not inspect or mutate stores.

The two current parse baselines used by direct publication and import staging
are transitional storage for two callers of the same canonical parse
algorithm. The final model has one parse query family keyed by exact snapshot
inputs. Committed compilation and discovery staging select different attempts;
they do not own different parsers or phase machines.

### Conceptual typed model

The following is a contract, not a required spelling of the Rust API:

```text
QueryFamily {
    Key: immutable + Eq + Hash
    Value: immutable
    Failure: immutable
    Work: immutable
    validate(key, dependencies, terminal outcome)
    retention policy
}

QueryStore<Q> {
    memos: Map<Q::Key, RetainedTerminal<Q>>
    selected: QueryState<Q>
}

InvalidationCause =
    LeafInputChanged { input, previous, current }
    DependencyStampChanged { dependency, previous, current }
    DependencyStampDisappeared { dependency, previous }

TerminalStamp<Q> =
    Success { value_publication, diagnostic_publication }
    Failure { failure_publication, diagnostic_publication }

QueryState<Q> =
    Unpublished { requested_key }
    Dirty {
        requested_key,
        invalidated_by: SortedDeduplicatedSet<InvalidationCause>,
        last_attempt?,
        last_good?
    }
    Computing {
        attempt_id,
        requested_key,
        cancellation,
        last_good?
    }
    Success {
        current_attempt,
        last_good
    }
    Failure {
        current_attempt,
        last_good?
    }

Attempt<Q> {
    id,
    key,
    dependencies,
    execution: Computed | Reused { origin_attempt } | Rejected,
    outcome:
        Success(value)
        | Failure(failure)
        | Aborted(Canceled | DuplicateInFlight | DependencyCycle),
    diagnostics,
    work
}
```

The memo table and selected state have different jobs. `memos` retains bounded
terminal records for multiple immutable keys. `selected` describes exactly one
key currently chosen by session orchestration and is the only value exposed by
ordinary current-result access. Selecting a different key does not delete the
other memos or turn them into current state.

Selection itself is not a query request. Selecting a key with a retained memo
whose dependencies validate installs that retained successful or failed
terminal attempt into `selected` without creating an attempt record. The next
actual query request observes Success or Failure and creates one reuse attempt.
Selecting an absent key installs Unpublished; selecting a retained but
unverified key installs Dirty with that terminal attempt available for
restoration. A request that selects and fetches in one API call performs these
selection/validation steps first and still records exactly one computed or
reused attempt for the request.

`invalidated_by` is a sorted, deduplicated set, not a single last writer. Cause
variants and their leaf/query identities have stable total ordering; previous
and current stamps are retained where applicable. Adding the same cause twice
is idempotent, independent causes are preserved, and successful restoration
removes exactly the causes it validates.

Every finished attempt record is immutable and externally observable. Only
Success and Failure are terminal query outcomes and receive a `TerminalStamp`;
Aborted attempts are history without a reusable stamp. A private computing
guard accrues dependency reads, diagnostics, and work; finishing the guard
freezes them into one attempt. Dropping or explicitly canceling the guard
freezes an aborted-canceled history record and makes the selected entry dirty.
It cannot publish a partial value.

Every request to a typed query family creates an attempt record, including a
memoized success or failure. A reuse attempt points to the terminal origin and
shares its immutable value, failure, and diagnostic batch, while recording its
own zero structural execution work and explicit reuse event. This separates the
artifact's provenance from the request that observed it and makes calls,
executions, reuses, failures, and cancellations derivable from attempt records.
The later one-shot executable adapter is deliberately not a memoized query in
M2 and therefore does not manufacture a query attempt record.

The concrete implementation may use specialized stores where that is clearer,
but they must implement this contract. It must not erase all families behind
`Any`, unchecked downcasts, string query names, or digest-only equality.

### Immutable keys and dependencies

A query key contains every intrinsic value that can change the query's answer.
Keys use typed equality over compiler-owned immutable descriptors. A digest may
accelerate lookup but is never sufficient evidence of equality.

A value, failure, or diagnostic publication stamp is a session-local opaque
publication identity allocated by its typed family store. On publication, the
store compares the new immutable value with the prior retained publication for
that key using the family's typed equality. `Arc::ptr_eq` is a legal fast path
for pointer-identical immutable values. Structural equality, a family-specific
exhaustive equality function, or another proof that implies typed equality is
required otherwise. A digest may reject equality cheaply, but a matching digest
never proves it. If the outcome and diagnostic batch are equal, the store
preserves their publication identities; otherwise it allocates new identities.
The success/failure variant plus those identities is the `TerminalStamp` read
by dependents.

Eviction has explicit consequences. A store may retain a bounded validation
tombstone containing the key, direct dependency stamps, and terminal stamp
after dropping the artifact. While those dependencies remain valid, already
retained dependents may validate against the tombstone, but a request for the
evicted artifact must recompute it. Because the old value is unavailable for
typed equality, that recomputation receives a fresh publication identity even
if a digest or external caller-owned `Arc` suggests equality; dependents are
conservatively dirtied. Evicting the tombstone makes the observed stamp
disappear and dirties dependents immediately. Tombstones count against their
family's documented retention bound.

Dependencies are exact reads of other terminal query outcomes or explicit leaf
inputs. Each edge records a typed query identity and the immutable terminal
stamp observed by the attempt. A terminal stamp covers either the successful
value or the deterministic failure and always includes the attached
diagnostic/warning batch stamp, including the empty batch. Attempt ID, work,
timing, and retention position never participate. The complete direct edge set
is frozen at terminal publication. The store validates both the requesting key
and every dependency stamp before reuse. An untracked source of semantic
variation is a correctness bug.

A deterministic failure is therefore a first-class stable dependency. An
incomplete dependency manifest, diagnostic projection, or other fail-closed
consumer may reuse after observing the same failed terminal stamp. Success to
failure, failure to success, a changed failure payload, or a changed diagnostic
batch changes the terminal stamp. Failure by itself is not an invalidation
event.

Leaf dependencies include source identity and bytes, root module, logical and
physical source metadata where relevant, presentation order where relevant,
captured import-discovery context and observations, target, preview features,
optimization level, and any future explicit compiler option. Linker choice is
not a semantic dependency; it is a one-shot executable request input represented
by `LinkInputDescriptor`.

Dependencies, not broad source-generation numbers, determine validity. A query
may retain a record across a session update when all declared inputs and
observed terminal stamps are unchanged. Dirty, computing, and canceled states
have no reusable current terminal stamp. They make a dependent unverified and
therefore dirty before it can be returned. If the dependency subsequently
publishes the same observed terminal stamp, including the same deterministic
failure, a dirty dependent with its prior terminal attempt retained reinstates
that attempt without re-execution or a new attempt record. If the observed stamp
changes or disappears through cancellation, supersession, or eviction without a
retained validation stamp, the dependent remains dirty and must recompute before
publication.

Dependency recording occurs at the typed store boundary. A new query family
must declare its key and dependencies; callers do not add its field name to an
invalidation switch.

### State transitions

The allowed transitions are:

| From | Event | To | Publication rule |
| --- | --- | --- | --- |
| Any selected state | Select a different key with a retained terminal whose dependencies validate | Retained Success or Failure | Install the retained terminal as selected. Selection creates no attempt; a following request records reuse. |
| Any selected state | Select a different key with a retained but unverified terminal | Dirty | Retain that terminal and record the deterministic invalidation causes. |
| Any selected state | Select a different key with no retained terminal | Unpublished | Select the key without publishing an attempt. |
| Unpublished | request | Computing | Create a fresh attempt guard. |
| Dirty with a retained successful or failed terminal attempt | Own key is unchanged and validation restores every observed terminal stamp | Prior Success or Failure | Clear invalidation and reinstate the retained terminal attempt. This is validation, not a request: create no attempt, diagnostics, or work. |
| Dirty | Request arrives and retained-terminal restoration is unavailable | Computing | Preserve last-good; create a fresh attempt guard. |
| Success or Failure | exact key and valid dependencies | Success or Failure | Publish a reuse attempt referencing the origin. |
| Success or Failure | An observed terminal stamp changed or disappeared | Dirty | Record the invalidating edge or leaf input. A different own key uses the selection rows above. |
| Computing | A second request targets the same selected key | Computing | Reject the second request, publish an `Aborted(DuplicateInFlight)` attempt, and leave the original guard unchanged. M2 never joins. |
| Computing entries in a dependency cycle | Re-entry reaches an active key | Dirty | Abort the affected guards with `DependencyCycle`, preserve last-good, and publish no terminal stamp. |
| Computing | computation succeeds | Success | Atomically publish value, diagnostics, work, and dependencies; advance last-good. |
| Computing | computation fails | Failure | Atomically publish failure, diagnostics, work, and dependencies; preserve last-good. |
| Computing | canceled or superseded | Dirty | Publish only `Aborted(Canceled)` attempt history; preserve last-good. |

“Valid dependencies” includes pointer-independent equality with a previously
observed deterministic failure stamp. It excludes any dependency whose current
state is unpublished, dirty, computing, or canceled.

Restoration is legal only when the dirty entry still retains a successful or
failed terminal attempt for the same own key and every edge equals the terminal
stamp observed by that attempt. A canceled last attempt, an evicted terminal
attempt, an own-key change, a missing stamp, or any changed stamp makes
restoration unavailable. After restoration, the next query request follows the
ordinary Success/Failure reuse row and creates exactly one reuse attempt. The
validation event itself is not a query request and therefore does not violate
the universal attempt-record rule. A lookup that encounters Dirty always runs
this validation first; it starts Computing only when restoration fails.

M2 is single-threaded at the query-orchestration boundary. A second in-flight
request for the selected key is always rejected; joining is not implemented.
Re-entering an active key through dependencies aborts the cycle. Duplicate,
cycle, foreign-key/revision, and violated-store-invariant errors are query-engine
control failures: they are non-memoizable `Aborted` attempts, produce no
terminal stamp, never become a dependency, and never advance last-good. An
ordinary deterministic `InvalidCompilerInput` produced by a correctly keyed
compiler query may still be a memoizable Failure. No current family gains an
implicit cycle-recovery or fixed-point policy from this ADR.

A failure is a deterministic terminal compiler answer for exact inputs, may be
memoized, and may be observed by dependent queries. Cancellation means no
answer was computed, has no terminal stamp, and is never memoized as a failure.
An explicit import observation with status `Cancelled` remains an ordinary
closed input under ADR-0051 and can produce a deterministic import failure.
That is distinct from canceling the compiler query attempt itself.

### Current attempt and last-good state

`current_attempt` means the latest terminal attempt for the currently selected
request, whether successful or failed. `last_good` means the most recent
retained successful attempt accepted by that family's compatibility policy.
They are never aliases by convention; their relationship is represented by the
state variant.

Normal query access returns only the current attempt's outcome. A semantic
failure cannot silently return the previous semantic output, and an import
failure cannot expose the previous graph as the graph for the attempted
revision. Tooling that intentionally wants stale-but-useful data must call an
explicit `last_good` accessor and receives its original key and revision.

Success advances last-good only after the complete family success boundary.
For semantic analysis that boundary includes CFG construction and the warning
and dependency information that belongs to the output. A declaration, body,
specialization, CFG, diagnostic, or projection failure does not advance the
corresponding last-good pointer.

### Query-family coverage

The initial database covers these families and inputs:

| Family | Key and direct dependencies | Success / failure and last-good rule |
| --- | --- | --- |
| Parse | Exact source snapshot identity and syntax presentation mode; leaf module source, FileId epoch, logical/physical metadata, root, and diagnostic order inputs | `ParsedProgram` or ordered syntax errors. Successful exact-snapshot programs are last-good; failure never advances the baseline. |
| Import plan and closure | Parsed attempt, immutable `ImportDiscoveryContext`, policy version, accepted-read manifest, plan, and complete observation ledger | Immutable plan/closed `CanonicalImportGraphOutput` or typed discovery/validation errors. Only closed valid closure advances the committed last-good graph. |
| Merge | Parsed attempt and presentation ordering used for diagnostics | `CanonicalMergedProgram` or merge errors. Both exact success and failure are memoizable. Definition-shard reuse is a validated last-good input, not hidden mutable state. |
| RIR | Successful merged attempt | `CanonicalRirOutput` or lowering failure. Exact failures are memoizable even though the current implementation only retains success. |
| Definitions | RIR, merged definition snapshot, accepted import graph, target, and preview features | `BoundDefinitionSet` or complete binding/issuance failure. Stable IDs remain request-local outputs backed by stable definition keys. |
| Semantic | RIR, merged definitions, accepted import graph, target, preview features, and optimization level at the CFG boundary | `CanonicalSemanticOutput` or complete preparation/body/CFG failure. Linker choice is excluded. Exact option variants are distinct records. |
| Dependency manifest | Successful or deterministically failed semantic/definition terminal stamps, accepted import graph, and stable source inputs | A typed complete/incomplete manifest. An unchanged failed dependency may reuse the same incomplete manifest; last-good candidates are usable only when their typed completeness state permits it. |
| Durable declaration/body/specialization reuse | Stable definition keys, versioned fingerprints, target/features, exact direct dependency inputs, and schema version | Immutable candidates selected from a successful semantic attempt. Projection/import is atomic; rejection is current-attempt work and falls through to the same canonical analysis. |
| CFG reuse | Stable body and specialization input, consumed layout/type domains, target, optimization level, and schema version | Immutable CFG candidate from a successful semantic attempt. Remap/validation failure rebuilds that function and cannot partially publish. |
| Derived diagnostic projection, only when needed | Terminal stamps of all producing attempts plus presentation inputs; an import projection additionally includes context, plan, ledger, and accepted reads | One immutable projected batch attached to the projection attempt. A projection is a query only when it computes a genuine derived answer; it never owns or republishes another attempt's attached batch. |

Last-good compatibility is family-specific and never inferred from recency:

- a whole parsed program, import plan/closure, merge, RIR, semantic output,
  definition set, manifest, or diagnostic projection is a current memo only for
  its exact typed key and dependency stamps;
- parse may reuse individual module payloads across program keys only when the
  module source identity, FileId epoch, logical identity, and required physical
  metadata validate; target and optimization are irrelevant;
- merge may offer the preserved definition-shard baseline across source
  revisions only through canonical merge's stable shard validation; target and
  optimization are irrelevant;
- durable declarations require the same root, target, preview features, and
  exact stable definition fingerprints. Optimization and linker choice are
  irrelevant;
- durable ordinary and specialized bodies require their stable owner or
  specialization identity, target, preview features, exact owner and direct
  dependency fingerprints, completeness, and schema. Optimization and linker
  choice are irrelevant;
- durable CFGs additionally require the same target, optimization level,
  stable body/specialization input, consumed layout/type domains, and schema;
  an optimization change rejects CFG reuse without rejecting a compatible body;
- a target change rejects declaration, body, specialization, and CFG candidates
  whose compatibility includes target. A preview-feature change likewise
  rejects declaration/body candidates unless their exact typed key explicitly
  proves independence in a future design; and
- incomplete manifests and derived diagnostic projections may reuse a stable
  deterministic-failure terminal stamp, but never cross a changed source,
  import, presentation, target, preview, or optimization component present in
  their typed key.

Thus “last-good” is a selector over retained successful candidates plus an
explicit compatibility predicate, not permission to reuse the newest success
under a different key.

Semantic and CFG may be separate internal families or one semantic family with
typed subrecords during the additive migration. Either representation must keep
the existing single semantic-to-CFG computation path and the success boundary
above. RUE-812 does not authorize a peer body analyzer, CFG builder, frontend,
or presentation-selected phase path.

### Backend, object, and link remain one-shot adapters in M2

M2's typed database ends at the successful semantic/CFG artifact. Machine-code
generation, object construction, and linking are not memoized query families in
RUE-813 through RUE-818. `CompilerSession::executable` and
`executable_in_compile_scope` remain root operation adapters that:

1. require the session's closed-valid committed snapshot;
2. request current RIR and semantic/CFG terminal outcomes from the typed
   database;
3. pass the current artifacts and full `CompileOptions` to
   `compile_with_session` and the existing backend; and
4. call machine-code generation, object construction, and linking on every
   executable request,
   returning its success or failure directly without session memoization.

Post-decision note (RUE-1518): `CompilerSession::executable` was later deleted
from the stable facade (no production caller). The adapter contract above is
carried by `compile_snapshot` and the crate-private compile-scope adapters.

`LinkInputDescriptor::from_compile_options` is the canonical stable description
of the request's source/resolution/target/features/optimization/linker option
identity (`CodegenInputDescriptor` plus `StableLinkerInput`). The current
adapter does not need to instantiate or store that descriptor for correctness
because it performs no backend reuse: it passes the same underlying snapshot,
semantic functions, type pool, strings, RIR symbol interner, optimization
level, target, linker mode, and warnings directly to the backend on every call.
RUE-813 uses the descriptor to label and compare executable request sequences.
The embedded runtime and compiler/backend implementation are fixed by the
running compiler binary. A system linker and its ambient toolchain are external
execution inputs, which is an additional reason not to cache link results under
the current descriptor alone.

This boundary keeps executable observations covered without pretending they
are stored query outcomes. Cold and reused frontend sessions produce equivalent
backend inputs under the invariants below, and both execute codegen, object
construction, and link afresh. RUE-813 compares emitted/object/executable hashes
with the internal linker. System-linker byte equivalence is outside the bounded
deterministic corpus unless the harness explicitly pins that external
toolchain; in all cases its success or failure is the fresh root-operation
result, never a reused session value. Backend work, link failures, and link
diagnostics remain direct root operation output and are not indexed by
`DiagnosticAttemptStore`.

RUE-815 and RUE-818 do not migrate or cache this one-shot tail; their issue
scopes do not include it. Adding backend, object, or link memoization later
requires a separate tracked design/issue that introduces typed families and
keys for semantic/CFG terminal stamps, codegen settings, object-format inputs,
runtime archive identity, linker identity/version, and all relevant external
toolchain inputs. It must not add a cache directly to the adapter.

### Import discovery is an external-input protocol, not another query engine

ADR-0051's fixed-point protocol remains necessary because the host must perform
filesystem operations between compiler steps. `CompilerSession` coordinates
that protocol using the same parse, import-plan, and import-closure stores:

- an open discovery revision owns a computing closure attempt and immutable
  plan revisions;
- each staging snapshot is an ordinary parse query attempt in the same parse
  store used by committed compilation;
- a closed attempted revision is the closure attempt's failure and diagnostics;
- a closed valid revision is its success and is eligible to become committed
  last-good; and
- query cancellation returns the closure entry to dirty, while an observed
  host cancellation is a terminal failure input as described above.

The current `ImportDiscoveryRevisionStatus` may remain as an adapter during
migration, but final storage state must not duplicate `Open`, `ClosedAttempted`,
and `ClosedValid` alongside an independently mutable query state. Domain
accessors project those names from computing, failure, and success.

### Diagnostics and work belong to attempts

Every typed query attempt contains exactly one immutable diagnostic/warning
batch, which may be empty. Diagnostics are not mutable side effects of cache
lookup. A computation builds its ordered batch and freezes it with its terminal
attempt. A reuse attempt references the origin batch. Presentation order is
part of the producer key wherever it changes that batch.

`DiagnosticAttemptStore` is a supporting retention/index component, not a query
family and not a second owner of diagnostic values. It retains `Arc<Attempt<_>>`
references, indexes their attached batches by attempt identity and stage, and
owns the explicit selectors for latest attempted, latest successful, and
last-good semantic diagnostics. It has no unpublished/dirty/computing state,
does not evaluate dependencies, and never computes or copies a batch. Evicting
an index/history reference cannot alter a query result. Caller-owned `Arc`
values remain valid but cease to be discoverable through the index after
eviction.

A separate diagnostic query is allowed only for a genuine derived projection
whose answer combines terminal outcomes or adds presentation inputs not owned
by a producing query. For example, the canonical import site projection may
join a closed import terminal outcome to parser-owned occurrences. That
projection depends on the import terminal stamp whether it is success or
deterministic failure, owns its newly computed batch as part of its own attempt,
and is then indexed like every other attempt. Its batch is stored once in the
attempt's diagnostic field; the projection's success value is unit or projection
metadata rather than a second copy of the batch. A batch is never simultaneously
owned as an attached producer batch and as the result of a generic
“diagnostics” query. Direct syntax, merge, semantic, and CFG diagnostics stay
attached to their producing attempts.

Structural work is value-owned by the computing guard and reduced into one
attempt work record on every exit, including failure and abort. At attempt
publication, the metrics component first adds calls, executions, reuses,
failures, aborts, and structural work to monotonic lifetime counters. Only then
may bounded attempt history evict records. Lifetime counters therefore never
depend on retained history and never decrease.

Retained-record counts, retained bytes, protected current/last-good records,
dependency pins, tombstones, and cache sizes are gauges projected from the
stores after eviction. They may rise or fall and are not reconstructed as
lifetime work. Removing metrics collection cannot change keys, dependency
recording, state transitions, outcomes, diagnostic selection, or retention.
Existing benchmark schema remains stable unless RUE-817 explicitly versions
it.

### Bounded retention

Each family declares a deterministic retention policy. At minimum it specifies
the maximum memoized terminal records, maximum attempt-history records, and
which current and last-good records are protected. Insertion order is the
default eviction order unless a family documents another deterministic rule.

Retention is part of storage policy, never semantic validity. Eviction may
cause recomputation but cannot change the answer. A terminal record still
needed by a retained dependent is pinned, or eviction first dirties/removes the
dependent so no dangling validation edge survives. Immutable terminal stamps may
remain after artifact eviction only in the bounded validation tombstone defined
above, whose still-valid direct dependencies are sufficient to validate the
edge without reconstructing the artifact. Stores expose gauges for retained
records, pinned dependencies, bytes where measurable, and evictions.

There is no unbounded side vector of failures, work records, dependency
manifests, or query keys. Current and last-good protection counts against a
documented bound or a separately documented constant-size protected set.

### Dependency-derived invalidation

Publishing a changed leaf input or a changed/disappeared terminal stamp marks
its reverse dependents dirty. Publication of a terminal outcome equal under the
family's declared equality preserves the stamp and permits dependents with a
retained same-key terminal attempt to reinstate that attempt without executing
or recording a request. This applies equally to repeated success and repeated
deterministic failure. Dirty/computing/canceled dependencies remain non-reusable
until a matching terminal stamp is published. Invalidation and verification
traverse declared reverse edges in deterministic order.

Broad conservative edges are permitted during migration. For example, merge
may initially depend on the entire parsed program even when per-module edges
could be more precise. Missing an edge is never permitted. Precision can
improve additively without changing orchestration.

`finish_update` will ultimately publish the parse attempt and select the new
request. It will not name merge, RIR, semantic, definition, diagnostic,
manifest, body, or CFG stores. This invalidates revision-local current
selections and memos by dependency consequence while intentionally retaining
the validated last-good definition-shard, durable-declaration, body,
specialization, and CFG baselines that current `finish_update` preserves. Those
candidates remain non-current and must pass the family compatibility rules
above inside the next semantic attempt. Durable reuse then follows ADR-0050's
stable dependency manifests; it does not bypass the query dependency graph.

### Cold and reused observational equivalence

For a root request key, define the semantic observation as:

```text
success or failure kind
artifact contents and stable identities
warnings and diagnostics in specified order
canonical import outcomes and accepted provenance
dependency manifest and completeness state
emitted IR/object/executable bytes or hashes where requested
```

It intentionally excludes pointer identity, attempt IDs, cache-retention
position, timing, and work counters. Work must accurately distinguish computed
and reused execution; it is not expected to be equal.

Cold and reused observations are equivalent by construction because:

1. reuse requires exact typed key equality and validation of every frozen
   dependency edge against its observed success or deterministic-failure
   terminal stamp;
2. cached success and failure return the same immutable terminal outcome and
   diagnostic batch as the originating computation;
3. durable declaration, body, specialization, and CFG reuse are inputs to the
   canonical semantic computation, not alternate published result paths;
4. every durable import validates the complete current projection atomically
   and otherwise falls back to fresh work;
5. failure and cancellation preserve last-good state without exposing it as a
   current result; and
6. eviction changes only whether work is repeated.

The one-shot backend tail preserves the same observation because both cold and
reused frontend executions feed equivalent current semantic/CFG and
`LinkInputDescriptor` inputs into a fresh backend/object/link execution. M2
makes no backend cache-reuse claim.

RUE-813 supplies the adversarial differential oracle for these invariants. The
oracle is required even though the model makes the intended proof local: it
tests that family keys and dependency declarations are complete.

### In-house store instead of a query library

Rue will implement this design in-house for M2.

Salsa is the credible library alternative. It provides a database, memoized
tracked functions, dependency tracking, revisions, durability, accumulators,
parallel snapshots, cycle handling, cancellation, and configurable per-query
LRU capacity. Those facilities align with ordinary pure compiler queries.

The mismatch is at Rue's current migration boundary:

- import discovery is a compiler/host transaction with open staging work,
  revision-labeled attempted failure, and separately committed last-good state;
- Rue must retain and expose failed and canceled structural work, exact
  diagnostic provenance, and explicit current-versus-last-good selectors;
- ADR-0050 durable artifacts perform validated cross-epoch projection and
  atomic fallback inside semantic analysis rather than ordinary memo reuse;
- current APIs use owned `Arc` artifacts and explicit stable descriptors while
  Salsa adoption would also require database-lifetime and input-model changes;
- Salsa's documented LRU policy does not itself provide Rue's protected
  current/last-good selectors, attempt-history bounds, or dependency-pin
  accounting, so a separate Rue attempt store would still be required; and
- the six additive implementation issues are primarily an ownership refactor,
  so replacing the execution substrate at the same time would obscure whether
  behavior changes came from the state model or the library migration.

A thin in-house store needs typed maps, attempt guards, dependency stamps,
reverse edges, deterministic retention, and metrics projection. It does not
need a macro system, generalized parallel snapshot runtime, automatic cycle
recovery, persistence, or a new scheduler. That smaller scope has lower
migration risk while meeting the required semantics.

This is not a permanent rejection of Salsa. Reconsideration requires evidence
after RUE-818: a prototype must preserve import attempt/commit behavior,
attempt-attached diagnostics and failed work, bounded retention, durable
projection, and the RUE-813 oracle without keeping the in-house database as a
peer state machine. A library wrapper that leaves all existing cache and
generation fields in place is not adoption.

## Invariants

The implementation and differential oracle must enforce these invariants:

1. **One current state:** each selected family request is represented by exactly
   one `QueryState` variant.
2. **Memo/selection separation:** retained per-key memos do not become current
   until orchestration selects and validates their key.
3. **Immutable identity:** a finished attempt's key, dependencies, outcome,
   diagnostics, and work never change.
4. **Publication identity:** typed equality preserves publication stamps;
   pointer equality may prove equality quickly, but a digest match never does.
5. **Exact reuse:** reuse requires equal typed keys and valid dependency stamps;
   a hash match alone is insufficient.
6. **Terminal failure is evidence:** an unchanged deterministic failure stamp is
   a reusable dependency; failure alone never dirties a dependent.
7. **Atomic publication:** no partial artifact, dependency set, diagnostic
   batch, or work record becomes a terminal success or failure.
8. **Failure fidelity:** deterministic failures may be memoized only for their
   exact keys and dependencies.
9. **Abort fidelity:** cancellation, duplicate in-flight requests, dependency
   cycles, and query-engine invariant violations publish no terminal stamp, are
   never memoized, and never advance last-good state.
10. **Last-good isolation:** normal current-result access never substitutes a
   last-good artifact after failure, dirtiness, or cancellation.
11. **Single diagnostic owner:** every query diagnostic/warning batch belongs to
   exactly one producing attempt; the non-query index only retains references.
12. **Work provenance:** every computation, fallback, failure, abort, and
   reuse contributes to exactly one attempt-scoped work record.
13. **Dependency closure:** every retained memo can explain validity through
    typed direct dependencies to explicit leaf inputs.
14. **Deterministic invalidation causes:** Dirty retains every sorted,
    deduplicated cause until validation removes that exact cause.
15. **Fail-closed incompleteness:** an incomplete dependency surface carries a
    typed reason and cannot be consumed as complete.
16. **Bounded ownership:** every session-owned history has a deterministic
    bound; retained dependency pins and protected records are measurable.
17. **Single computation path:** reused subartifacts enter the same canonical
    parse/merge/RIR/semantic/CFG consumers and publication boundary as cold
    subartifacts.
18. **Observational equivalence:** equal root request keys produce equal
    semantic observations whether computed cold, memo-reused, or reconstructed
    with durable subartifact reuse.
19. **Uncached executable tail:** every executable request runs
    backend/object/link from the current semantic/CFG artifact and exact link
    descriptor; M2 retains no backend result that could become stale.

## Implementation Phases

The migration is additive. Each phase must keep `CompilerSession` as the sole
orchestrator, keep the existing canonical phase functions, and pass the cold
versus reused oracle before the next ownership boundary moves.

- [ ] **Phase 1: Differential oracle** — RUE-813. Land the reusable fresh-versus-
  reused request-sequence harness before changing cache ownership. Cover exact
  success, failure and recovery, last-good/current separation, relocation,
  root/target/options changes, incomplete manifests, eviction, and deliberately
  injected stale import/semantic/diagnostic faults. Compare the freshly executed
  backend/object/link outputs under exact `LinkInputDescriptor` inputs; do not
  add a backend cache to create a reuse scenario.
- [ ] **Phase 2: Diagnostic attempt store** — RUE-814. Introduce immutable
  attempt-attached diagnostic batches and move bounded retention and selectors
  behind one non-query index. Adapt existing query methods to publish once,
  identify any genuine derived projections explicitly, and remove the
  individual session diagnostic fields only after parity holds. The index never
  owns a second copy or query state for a batch.
- [ ] **Phase 3: Typed query stores** — RUE-815. Introduce the shared typed store
  contract and migrate import, semantic, and definition lookup/insertion without
  changing reuse policy. Terminal stamps cover both success and deterministic
  failure. Parse, merge, and RIR may be adapted through the same contract in
  this phase where necessary, but no query algorithm moves into the store and no
  second orchestrator is introduced.
- [ ] **Phase 4: Typed dependency completeness** — RUE-816. Replace manifest
  completeness booleans and blocker combinations with exhaustive complete or
  incomplete states carrying the data valid in each case. Preserve fail-closed
  planning and current presentation intentionally.
- [ ] **Phase 5: Attempt-scoped work** — RUE-817. Make computation guards return
  immutable work on every terminal or aborted path and derive session metrics
  from attempt events. Preserve or explicitly version benchmark JSON.
- [ ] **Phase 6: Dependency-derived invalidation** — RUE-818. Record typed
  dependency edges and reverse edges at store boundaries, begin conservatively,
  switch publication to dependency validation, and then remove imperative
  downstream clearing. Extend typed stores to the remaining parse, merge, RIR,
  manifest, and durable reuse records needed to ensure update code names no
  downstream implementation. Do not expand this phase into backend/object/link
  caching.

With this design accepted, RUE-816 and RUE-817 may proceed subject to their
tracker dependencies. RUE-814 and RUE-815 still wait for the RUE-813 oracle and
any other tracker blockers; RUE-818 waits for RUE-814 through RUE-817. The phase
issues may use temporary adapters around current fields. An adapter must have
one owner and a scheduled removal in the same phase; it must not become a second
independently invalidated cache or phase machine.

## Consequences

### Positive

- Invalid state combinations become unrepresentable at store and manifest
  boundaries.
- Failure, cancellation, diagnostics, work, and last-good retention have one
  explicit provenance model.
- New queries declare dependencies instead of extending a global invalidation
  checklist.
- Existing stable durable reuse remains usable without becoming a peer
  frontend.
- Query storage can be tested independently while orchestration remains simple
  and visible in `CompilerSession`.

### Negative

- Rue owns a small amount of incremental infrastructure and must test its
  dependency, cancellation, and eviction behavior.
- Attempt records and dependency edges add memory overhead that must be bounded
  and measured.
- Recording one attempt for every cache hit makes reuse allocation and retention
  churn explicit; implementations may compact representation, but may not erase
  the logical attempt or its lifetime metrics before publication.
- The additive migration temporarily requires adapters around heterogeneous
  current caches.
- Conservative initial edges may recompute more than the final model until
  precision improves.

### Neutral

- This ADR changes compiler ownership and incremental correctness, not Rue
  language semantics, so it has no preview feature or specification change.
- It does not promise persistent or cross-process cache compatibility.
- It does not make FileId, Span, interner IDs, AIR references, CFG references,
  or pointer identity durable keys.
- It does not require asynchronous or parallel query execution.

## Future Work

- Persistent serialization and cross-process schema/version policy.
- A scheduler for parallel independent queries, if profiling justifies it.
- Joining duplicate in-flight requests if query execution becomes parallel;
  M2 rejects them.
- More precise per-module and per-definition dependency edges after the
  conservative graph is proven.
- Re-evaluating Salsa or another library against the post-RUE-818 model and the
  differential oracle.
- Stable editor position/reference indexes and filesystem watcher integration.

## References

- ADR-0050: Stable semantic dependency manifests
- ADR-0051: `CanonicalImportGraph` as the sole import-resolution authority
- RUE-812: Define a typed query-state model for `CompilerSession`
- RUE-813: Add a cold-versus-reused session differential oracle
- RUE-814: Extract diagnostic attempts and retention from `CompilerSession`
- RUE-815: Encapsulate import, semantic, and definition caches in typed stores
- RUE-816: Replace dependency-manifest completeness booleans with typed states
- RUE-817: Separate query work accounting from execution state
- RUE-818: Replace imperative clearing with dependency-derived invalidation
- [Salsa overview](https://salsa-rs.github.io/salsa/overview.html)
- [Salsa database and runtime](https://salsa-rs.github.io/salsa/plumbing/database_and_runtime.html)
- [Salsa tuning, retention, and cancellation](https://salsa-rs.github.io/salsa/tuning.html)
