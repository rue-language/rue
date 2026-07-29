---
id: 0063
title: "Parallel demand-driven incremental compilation"
status: accepted
tags: [architecture, compiler, incremental, parallelism, codegen, linker, performance]
feature-flag: null
created: 2026-07-18
accepted: 2026-07-18
implemented:
spec-sections: []
superseded-by:
supersedes: [0045, 0053]
amends: [0051]
amended-by: [0066]
relates: ["ADR-0050", "ADR-0052", "ADR-0055", "ADR-0058", "ADR-0061", "RUE-328", "RUE-648", "RUE-812", "RUE-1021", "RUE-1022", "RUE-1023", "RUE-1024", "RUE-1025", "RUE-1026", "RUE-1027", "RUE-1028", "RUE-1029", "RUE-1030", "RUE-1031", "RUE-1032", "RUE-1033", "RUE-1137"]
---

# ADR-0063: Parallel demand-driven incremental compilation

## Status

Accepted by Steve on 2026-07-18 after adversarial review in PR #1824. This is
an internal compiler and tooling design with no language-semantics or preview-
feature change. It authorizes the phased demand-driven compilation project
through fresh linking; incremental linker implementation still requires the
follow-up ADR described below.

Implementation is tracked by the Linear project **Parallel demand-driven
incremental compilation** under the RUE-648 epic. RUE-1021 through RUE-1033 are
the dependency-ordered phase issues.

This ADR supersedes ADR-0045's compiler-architecture rollout and
its exclusion of cross-request and cross-invocation incremental state. It
retains and restates ADR-0045's language-semantic rule that observable semantic
checking and emitted code are selected by explicit roots.

This ADR also supersedes ADR-0053's single-threaded, one-selected-key execution
boundary and its decision to end the query database before code generation.
ADR-0053 invariants 2 through 8 and 10 through 17 remain binding verbatim.
Invariant 1 is replaced by per-key memo nodes plus request-owned current
publication. Invariant 9 is modified so cancellation, dependency cycles, and
engine invariant violations remain non-terminal aborts, while an exact-key,
compatible duplicate joins the existing computation instead of aborting.
Invariant 18 is strengthened to require cold, reused, joined, one-worker,
many-worker, and codegen observations to be equivalent. Invariant 19 is
superseded: code generation becomes a retained query terminal while linking
remains fresh in the first implementation project. Publication identity,
bounded ownership, the single computation path, and the differential oracle
therefore remain explicit requirements rather than implied survivals.

This ADR amends ADR-0051. Compiler ownership of import recognition, candidate
precedence, canonical identity and outcomes, provenance, read policy, typed
observations, observation and accepted-read ledgers, and deterministic
diagnostics survives. Its closed-whole-graph fixed point, all-module parse abort,
and rule that semantic work begins only after a complete valid import graph are
replaced by batched demand fulfillment and validity of the rooted dependency
closure. Section 7 defines the amended protocol.

ADR-0045 and ADR-0053 carry reciprocal `superseded-by: 0063` metadata.
ADR-0051 remains accepted with `amended-by: 0063` because its import authority,
policy, and provenance rules survive while this ADR replaces its whole-graph
staging boundary.

## Summary

Rue will organize compilation as a revisioned graph of typed, memoized queries
from source input through per-function code generation. Observable compilation
is demand-driven from an explicit root set, normally the executable entry
point. Query results use stable semantic identities and request-independent
artifacts, so unchanged results can be retained across edits, evaluated in
parallel, and eventually persisted across processes.

The execution substrate will support immutable pinned input revisions,
per-key claim-or-join computation, exact dependency tracking, red/green change
propagation, cancellation, deterministic diagnostics, and bounded retention.
The first implementation may schedule queries serially, but it may not encode a
global mutable computation stack, a single selected key per family, or another
constraint that would require replacing the state model to run independent
queries in parallel.

The terminal compiler query artifact is a stable per-function `CodegenUnit`,
not serialized object-file bytes. A deterministic `ProgramImagePlan` collects
the reached units and runtime requirements. The initial implementation may
perform a fresh full internal link from that plan. A later incremental linker
can consume additions, removals, and changed unit fingerprints directly,
without changing frontend, semantic, CFG, or codegen query boundaries.

## Motivation and target

Rue already has useful pieces of incremental compilation:

- immutable parsed-module artifacts and per-module parse reuse;
- stable module and definition identities;
- a typed query graph with dependency stamps and retained attempts;
- demand-driven ordinary body analysis from `main`;
- canonical semantic body/type/value artifacts and fail-closed re-import;
- per-function durable CFG projections; and
- per-function native code generation on both supported architectures.

The pieces do not yet form a demand-driven pipeline. Import discovery reads the
complete transitive import graph. Parse, merge, RIR lowering, and declaration
resolution process every loaded module. Named destructors are implicit roots,
drop glue scans the complete type pool, semantic and optimized CFG artifacts
share one query terminal, and machine code/object/link work is performed afresh
for every executable request.

The current query engine is also deliberately serial. A family owns one selected
state, a second in-flight key is rejected, and active computation/cycle state is
tracked by query-family type rather than exact keyed node. `CompilerSession`
orchestration requires exclusive mutable access. Making those fields individually
thread-safe would preserve the serial ownership model while adding contention;
it would not produce a parallel query engine.

The long-term performance objective is an interactive warm rebuild: an edit to
one reachable function should normally re-run only the queries whose observable
inputs changed, generate a replacement code unit, and update a runnable internal
link in tens of milliseconds on a representative project. This ADR does not set
a hardware-independent latency promise. It requires the architecture,
structural work metrics, and benchmark scenarios needed to pursue and measure
that class of result.

## Decision

### 1. Observable roots are separate from speculative computation

Every compilation request owns an explicit, ordered root set. An executable
request normally roots the target-specific `main` definition. A future
`check-all` request may add public or all declarations as semantic roots. Other
tools may request syntax, definition, semantic, CFG, or codegen artifacts
directly through supported session operations.

The transitive dependency closure of those roots determines:

- which semantic failures and warnings are observable for that request;
- which runtime functions, user functions, specializations, destructors, and
  drop-glue units enter the program image; and
- which codegen units are required before linking.

The scheduler may speculatively evaluate a pure query which is not yet in the
root closure. A speculative result may populate the memo database, but its
diagnostics, warnings, or emitted unit do not become observable until a rooted
query reads it. This separates language semantics from scheduling policy and
allows safe background work without turning `check-all` into the default build
mode.

Speculation may consume only inputs already present in its pinned revision. It
may not emit a host missing-input demand, trigger filesystem access or policy
checks, or enlarge the observation or accepted-read ledgers. A speculative
query which encounters a missing input parks or abandons without publishing a
failure. Only rooted work, or a future explicitly authorized cache-warming
request with its own read policy, may drive the external-input protocol.

Lexing and parsing remain whole-file operations after a module is demanded.
Therefore a demanded module must be syntactically valid as a whole. An
undemanded module is not read or parsed and contributes no syntax diagnostics.
Semantic checking, CFG construction, and code generation are per declaration or
per concrete function instance. Item-level error-tolerant parsing is separate
future work and is not required by this ADR.

### 2. Inputs are immutable revisions

The query database retains immutable input revisions. A compilation request is
a sequence of attempts. Each attempt pins exactly one revision for its lifetime.
Publishing a source edit, target/configuration edit, or host import observation
creates a successor revision; it does not mutate the inputs observed by an
already-running attempt. A request may continue in a successor attempt after
rooted missing-input fulfillment, but no individual query computation observes
two revisions. An explicit source or configuration update begins a new request;
a canceled request may be retried only as a new request.

Rooted missing-input fulfillment is round-based. The coordinator drains all
currently available work for one pinned attempt, accumulates and deduplicates
its frontier of demands, and asks the host to execute one compiler-produced
batch. The host publishes the batch in one successor revision and the request
continues with one successor attempt. A cold build therefore advances revisions
with import dependency depth, not once per imported module. Terminals from the
previous attempt are validated or red/green reused against the successor; an
unchanged input leaf carried into the successor does not become different merely
because its revision number advanced.

Input leaves are granular:

- one root/request configuration;
- one module source and its accepted-read provenance;
- one import observation;
- target, preview, optimization, ABI, and layout configuration; and
- compiler/schema implementation epochs where they affect retained artifacts.

A module query depends on its exact module source leaf, not on a whole-program
snapshot containing every source currently known to the host. Adding an
unrelated source therefore does not invalidate an already-parsed module.

Current-versus-last-good selection belongs to a request/session publication
layer over immutable revisions. It is not mutable state inside every query
algorithm.

#### 2.1 Filesystem observation authority

Ruled by Steve on 2026-07-26 (RUE-1137). This subsection defines when a new
rooted import-input request may validate retained terminals against its
predecessor. It is the input contract RUE-1023 refers to.

**The compatibility token is observation-regime identity, not request
identity.** A revision's compatibility slot carries a stable digest of the rules
governing reads — discovery epoch, project root, std root, read policy revision.
It changes when those rules change and *not* when a file changes. File changes
are per-leaf stamp changes, so a retained terminal remains validatable across an
ordinary edit; a regime change resets the epoch wholesale because the compiler
can no longer relate its retained observations to the new rules.

Per-request identity remains separate and is what a trusted-toolchain successor
delta must match. Conflating the two is what previously made warm reuse
unreachable: a per-request counter in the compatibility slot meant no
predecessor terminal was ever eligible for red/green validation, so every rooted
compilation recomputed every body irrespective of its dependencies.

**Two authority tiers may assert filesystem stability. Bare assertions may
not.**

- *Tier A (watched).* The host supplies a proof attesting an active watcher
  covering the accepted-read set. Content changes arrive as ordinary leaf
  updates.
- *Tier B (unwatched, default).* The compiler is its own authority via a
  re-observation sweep over the previous rooted closure's accepted-read set at
  request start.

Tier B is the correctness baseline and Tier A is a latency optimization over it.
Warm reuse is therefore available to unwatched hosts, including a plain CLI and
a future watch mode, rather than only to a watcher-equipped editor.

**The sweep compares content, not metadata.** For each entry: stat it; on any
size/mtime/identity mismatch re-read and re-hash; republish the leaf only when
the *content digest* differs. A metadata mismatch alone is not a change, because
editors and build tools routinely rewrite files without changing bytes.

**Unreliable timestamps fall back to hashing.** A stat whose mtime lies within
the filesystem's indistinguishable window of now must not be trusted: such a
timestamp cannot separate "written before we hashed it" from "written after."
Those leaves are content-hashed. This rule is not optional hardening — it is why
the preceding rule is sound.

**Absent leaves invalidate; they do not demand.** A retained terminal whose
already-observed input leaf is absent from the successor's published view is
invalid and recomputes. Missing-input demand is for leaves a computation has not
yet discovered, not for leaves a retained terminal already observed. Without
this distinction an import edit that removes a module aborts every dependent
instead of recomputing it. Cancellation, dependency cycles, and engine invariant
violations continue to propagate, since none of them indicate staleness.

**Remaining rulings.** Missing, denied, unreadable, ambiguous, and malformed
reads stay distinguishable typed observations, so any transition between them
invalidates the dependent closure; a read-policy change is a regime change and
still fails closed. Speculative work inherits no authority and drives no
re-observation. A canceled or abandoned request establishes no freshness, and
its successor re-sweeps any leaf an incomplete sweep did not cover. A regime
change makes prior terminals ineligible for validation but forces no eviction;
they age out under existing bounded retention, so a watcher restart does not
produce a retention cliff.

**Implementation status.** The compatibility token, absent-leaf invalidation,
and the default Tier B sweep are implemented. The CLI source loader re-observes
the previous rooted closure before starting a successor filesystem request,
reuses cached bytes only after a trustworthy metadata match, and hashes
metadata mismatches or too-recent mtimes. Tier A watcher proofs remain a later
latency optimization.

### 3. Query identity, computation, and publication

A logical query key identifies a semantic entity plus explicit configuration.
Examples include a module identity, stable definition key, concrete
specialization key, type-instance key, target, or optimization level. A logical
key never embeds terminal stamps from its dependencies. Dependency stamps are
recorded on observed graph edges.

A terminal content fingerprint is derived metadata over the canonical result
and the explicit dependency outputs which affect that result. It is never part
of memo identity, never replaces exact-key and dependency validation, and never
proves reuse by digest equality alone. Consumers such as `ProgramImagePlan` may
compare fingerprints to describe content deltas without changing the stable
logical node which owns attempt history.

The database allocates one logical memo node per `(query family, logical key)`
and may retain versioned attempts/results for that node. Requesting a node from
a pinned revision validates a compatible retained result or performs an atomic
claim-or-join operation:

- a valid terminal is reused;
- one task claims a dirty or unpublished key and computes it;
- another request for that exact in-flight key and compatible pinned inputs
  joins the computation;
- a different ready key may compute concurrently; and
- a request which would wait through a true dependency cycle receives the
  family-defined cycle result or a query-control abort.

Requests for the same logical key under incompatible revisions do not join
blindly. They may compute concurrently or reuse only after dependency validation
proves that the observed inputs are equivalent.

No database shard or query-state lock may be held while executing a query body
or requesting a dependency. Computation builds its value, diagnostics, work,
and observed dependencies privately and publishes them atomically after
checking that its pinned revision is still eligible for the requesting
publication.

Publication is red/green. If a recomputed query has an equal canonical outcome
and equal semantic diagnostic identities and payloads under its family-owned
comparison, its observable terminal stamp is preserved. Current source
locations are a separately stamped presentation projection and do not
participate in semantic terminal equality. Dependents are not invalidated merely
because an implementation ran again, an input revision number advanced, or an
unrelated source edit shifted diagnostic positions.

Attempt history, current/last-good behavior, deterministic failures,
cancellation, and fail-closed retained-artifact validation continue to follow
ADR-0053. Duplicate in-flight work changes from a rejected attempt to a joined
request. Cancellation of one waiter does not cancel shared work while another
live waiter or retained root still requires it.

### 4. Parallel execution is schedule-independent

The database and all retained query keys/records are `Send + Sync`. Query
algorithms consume immutable dependencies and return immutable `Arc` artifacts.
Shared mutable interners, globally appended type pools, discovery-order counters,
and request-local indices do not cross query boundaries.

The evaluator maintains task-local dependency stacks and a cross-task wait graph
for cycle detection. Stable output does not depend on worker order:

- module, definition, specialization, symbol, anonymous nominal, and string/data
  identities are derived from stable semantic inputs rather than allocation
  order;
- diagnostic presentation is sorted by stable module/source identity, source
  anchor, and producer-defined order after collection;
- reached codegen units and image atoms are sorted by stable identity before
  image planning; and
- reductions of work metrics are deterministic.

The runtime owns one concurrency budget. Query-level parallelism and parallel
algorithms inside a query must use structured permits from that budget rather
than nesting uncoordinated global thread pools. The first implementation may run
the same interfaces with one worker. Correctness and artifact identity must be
identical for one worker and many workers.

A parked joiner does not retain an execution permit needed by the computation
it awaits. The scheduler must either release the permit while parking or donate
the waiting worker to ready dependency work. It must make progress with a budget
of one and under adversarial claim, join, and dependency schedules; cross-task
cycle detection alone is not a progress guarantee.

Rue's existing process-global Rayon configuration and the CFG, optimization,
and backend `par_iter` paths are part of this migration. Before query-level
parallelism is enabled, they must execute through the same structured budget or
be serial inside a query. `configure_thread_pool` remains a supported facade
operation under ADR-0061, but its implementation becomes configuration of this
shared budget rather than authorization for an independent nested pool.

### 5. Stable identity and canonical artifacts are the interchange format

Query results which outlive one local computation contain no request-local
`FileId`, raw `Span`, `Spur`, `InstRef`, live `Type`, nominal pool ID, AIR/CFG
offset owned by another artifact, or pointer identity.

The stable identity domain covers:

- named definitions and named nominal types;
- free functions, methods, associated functions, and destructors;
- concrete generic/comptime specializations with canonical type/value
  arguments;
- anonymous structs/enums and their methods/destructors, keyed by their stable
  producing definition or specialization, definition-relative structural
  anchor, and canonical arguments;
- synthesized drop glue keyed by canonical type instance; and
- runtime/compiler-provided symbols from the typed ABI manifest.

An identity anchor is a structural path relative to its stable producing
definition or specialization. It is not a module-absolute byte offset, line,
column, or raw span. Inserting whitespace, comments, or declarations outside
the producer therefore changes only current position metadata, not anonymous
nominal identity, symbols, codegen fingerprints, or image-plan entries.

ADR-0058's canonical semantic body/type/value algebra is the semantic query
interchange format. Compact live AIR, live type pools, parser interners, CFG
storage, and backend MIR remain optimized local representations. A query may
materialize a fresh local epoch from canonical dependencies, compute one
artifact, export its canonical result, and discard the local epoch. There is no
whole-reachable-program semantic epoch whose reconstruction gates downstream
parallel work.

### 6. Query and projection boundaries

The intended graph is conceptual; exact Rust names may differ:

```text
module source
  -> ParseModule
  -> ModuleIndex / ModuleRir
  -> LookupName / ResolveImport

stable definition or instance
  -> Declaration / Signature / ConstValue
  -> Body
       -> CanonicalBody
       -> BodyReferences

root set + BodyReferences
  -> Reachability
  -> demanded TypeFacts / Layout / CallAbi / DropGlue
  -> Cfg
  -> OptimizedCfg
  -> optional LoweredMir
  -> CodegenUnit

sorted reached CodegenUnits + runtime requirements
  -> ProgramImagePlan
  -> fresh internal link initially
  -> incremental internal link later
```

One query may publish multiple independently stamped projections when consumers
observe different facts. Required projections include at least:

- a body artifact separate from `BodyReferences`;
- declaration identity separate from signature/ABI facts;
- type identity/facts separate from physical layout;
- unoptimized CFG separate from optimization output; and
- codegen unit content separate from the program-wide reached-unit set.

A projection is produced by the canonical query and stored by the same database;
it is not a peer computation path or a manually synchronized cache.

Not every compiler pass becomes a query. A boundary is justified when an
artifact is independently reusable, has materially narrower invalidation than
its producer, or has a direct consumer such as `--emit mir`. Liveness,
peepholes, scheduling, verification, register allocation, and encoding may
remain internal stages of a `LoweredMir` or `CodegenUnit` computation until
measurement demonstrates an independent reuse boundary.

### 7. Import resolution is a demand-driven external-input protocol

The compiler continues to own import syntax recognition, candidate precedence,
logical identity, canonical outcomes, diagnostics, and the ordered demand plan
under ADR-0051. The host continues to own filesystem access and read policy and
executes only compiler-produced demand batches. Candidate provenance, typed
observation outcomes, the observation ledger, and the accepted-read manifest
remain separate canonical records.

Parsing a module records lazy module-binding/import-site values. Merely
encountering `const std = @import("std")` does not read the target. Looking up a
member through that binding requests its resolution. If the pinned revision has
no observation for the required candidate operation, the query reports a typed
missing-input demand rather than a compiler failure.

The request coordinator deduplicates all demands exposed by the current rooted
frontier. The host performs stable reads and policy checks for that ordered
batch, then publishes accepted, absent, denied, unreadable, ambiguous, or
canceled observations into one successor immutable revision. Work pinned to
the previous revision does not observe an in-place mutation. Candidate
precedence and provenance remain identical under serial and parallel
fulfillment.

All successor attempts used to fulfill one compilation request belong to one
immutable external-input discovery epoch. Completed observations may be carried
across those attempts and are validated as the same leaves. A later source
update starts a new epoch and re-executes observations unless the host supplies
the trustworthy filesystem/read-policy revision or watch token required by
ADR-0051; that token participates in every observation key. Suspension and
retry therefore cannot silently promote an old filesystem observation into a
new update epoch.

The database's canonical import records and their projections are the sole
resolution authority. A whole `CanonicalImportGraph` remains available as a
deterministic projection for diagnostics, dependency output, and compatibility,
but semantic work no longer waits for a complete transitive graph. Each rooted
branch proceeds only through demanded modules whose parse and relevant import
outcomes are valid. A syntax or import failure blocks that dependent branch and
the root request while unrelated, undemanded modules remain unread and cannot
fail the request. This replaces ADR-0051's closed-graph semantic gate and its
all-loaded-module parse abort; it retains fail-closed outcomes for every import
which the rooted closure actually observes.

The implementation may use suspension/joining or successor attempts. It may
not let semantic queries access the filesystem, let speculative work emit host
demands, let the host invent or reorder demand candidates, or maintain a second
import graph outside the canonical query database.

### 8. Semantic bodies and reachability are separate

A body query analyzes exactly one ordinary function, method, destructor, or
concrete specialization. It may request names, declarations, signatures,
constant values, type facts, and comptime results. It does not request ordinary
callee bodies.

The body producer publishes a canonical body outcome plus an independently
stamped `BodyReferences` projection containing the stable callable,
specialization, type, module, and glue dependencies discovered after
current-target comptime evaluation. After whole-module parsing succeeds,
`BodyReferences` is a total projection: a body with deterministic semantic
errors still publishes every positively resolved reference found during
error-tolerant analysis. Unresolved references produce diagnostics but no
invented edge. Cancellation, engine abort, or typed incompleteness publishes no
terminal reference projection, so reachability cannot mistake an interrupted
scan for an empty body. This policy keeps diagnostics in valid callees
observable even when their caller also fails. Ordinary call recursion is
therefore a legal cycle in the reachability graph, not a query dependency cycle.

`Reachability(RootSetKey)` is a database-owned query family, not a peer
coordinator or second call graph. Its evaluator expands a deterministic worklist
of `BodyReferences` projections from the root set, records every observed
projection stamp and canonical edge, deduplicates stable identities, and may
schedule independent frontier work in parallel. The result publishes a sorted
reached set plus independently stamped per-identity membership projections.
Downstream type, glue, codegen, and image-plan queries observe the memberships
they use rather than one opaque global reached-set stamp.

Additions may be maintained by monotone frontier expansion. When an observed
edge is removed, the baseline correctness algorithm re-derives membership from
the roots; it never retains a node merely because it was reached in an earlier
revision. If measurement shows that re-derivation misses the warm-edit budget,
the implementation may add predecessor support counts, tracing, or another
dynamic reachability algorithm behind the same query contract. Phase 7 must
measure edge addition and deletion separately and may not complete without a
documented latency/work gate. A recomputation whose reached memberships are
unchanged remains green even if one reference projection was recomputed.

True semantic cycles retain domain-specific handling. Recursive value constants,
illegal by-value type/layout cycles, and non-terminating specialization cycles
are diagnosed by their owning query families. Pointer/reference edges which do
not require pointee layout do not create false layout cycles.

### 9. Type facts, layout, ABI, and drop glue are demand-driven

Containment/ownership facts, physical layout, call ABI classification, and drop
glue become canonical typed queries over stable type instances and explicit
target/configuration inputs.

`TypeFacts` computes facts such as linearity and `needs_drop`. `Layout` consumes
only by-value component layouts required by ADR-0052. `CallAbi` consumes the
relevant signature, layouts, and target ABI. A consumer observes only the
projection it uses.

`DropGlue(TypeInstanceKey)` is requested when a reached semantic body or CFG can
destroy that exact type, or when another reached glue body refers to it. Named
and anonymous destructors become dependencies of the corresponding glue query;
they are not global roots. There is no scan over a complete compilation type
pool.

### 10. CFG, optimization, and interprocedural dependencies

`Cfg(FunctionInstanceKey)` consumes the canonical body and the precise type,
layout, symbol, string, warning, and implicit-cleanup projections needed to
construct a validated per-function CFG artifact.

`OptimizedCfg(FunctionInstanceKey, OptLevel)` consumes the unoptimized CFG and
optimization configuration. Changing optimization level does not invalidate
parsing, declarations, bodies, type facts, or the unoptimized CFG.

An ordinary caller depends on a callee's stable symbol, signature, and ABI, not
on its body. Editing a non-inlined callee body therefore does not invalidate the
caller's body, CFG, or codegen unit. An optimization such as inlining records an
explicit dependency on the callee body/optimized artifact and accepts the
corresponding invalidation.

CFG artifacts retain record-local references and canonical relocation domains
as established by the current durable CFG boundary. A local materialization may
use compact `Type` and string/symbol IDs, but those IDs do not become durable
query identity.

### 11. `CodegenUnit` is the terminal compiler query artifact

Backend work is per reached function instance. The initial reusable boundaries
are:

- `OptimizedCfg`;
- optionally `LoweredMir`, because tooling directly consumes pre-allocation MIR;
  and
- `CodegenUnit`, which includes allocation, scheduling, verification, peephole,
  and machine emission unless later measurements justify another boundary.

A `CodegenUnit` conceptually owns:

```text
CodegenUnit {
    identity and content fingerprint
    target, object format, ABI/layout/code-model epochs
    defined symbols and bindings
    referenced symbols
    text, read-only-data, writable-data, and BSS atoms
    atom alignment and permissions
    relocations and addends
}
```

Atom and symbol identities are stable across revisions when their semantic
identity is unchanged. Function-local strings and constants are normalized to
stable local atoms before publication. The internal linker consumes this typed
form directly in the eventual incremental path.

Object-file encoding is a projection from `CodegenUnit` for the system linker,
object presentation, and compatibility testing. The internal path does not need
to serialize each function to ELF/Mach-O and immediately parse it back.

The logical machine-code key is the stable `FunctionInstanceKey` plus explicit
target architecture/OS, code model, optimization mode, and relevant
ABI/layout/backend schema epochs. The optimized CFG, referenced ABI/layout
facts, runtime ABI manifest, and only the strings/data used by that unit are
recorded dependencies. Their canonical content contributes to the terminal
fingerprint but is not embedded in memo identity. Linker mode is neither a
logical key component nor a codegen dependency.

### 12. `ProgramImagePlan` preserves the incremental-linking seam

A deterministic `ProgramImagePlan` contains the sorted reached `CodegenUnit`
identities/fingerprints, entry point, target/object format, runtime ABI/archive
identity, and required runtime symbols. It contains no diagnostics, warnings,
source positions, or external system-toolchain state. Final user-visible
warnings belong to the executable request adapter's diagnostic projection under
section 13; changing warning text or position cannot create a linker delta.

The first implementation rebuilds the internal executable from the complete plan
on every root request. This is the intentional project boundary: frontend
through code generation is demand-driven and memoized, while internal linking
remains fresh. The adapter may project ordinary object bytes from each
`CodegenUnit` and invoke the current linker. Direct typed-unit ingestion belongs
to the follow-up linker work and does not change the plan boundary.

A later stateful internal linker may compare plans and retain:

- symbol definitions, addresses, and winning weak/strong bindings;
- per-atom output placement with reserved growth capacity;
- reverse relocation indexes from symbol/atom to patch sites;
- runtime archive symbol indexes and selected members;
- free slots, tombstones, and compaction thresholds; and
- the previously published executable image or patchable output mapping.

It can replace, add, or remove changed atoms, patch affected relocation sites,
and atomically publish a new executable. Growth beyond reserved capacity,
layout-policy changes, runtime changes, unsupported relocations, or excessive
fragmentation may fall back to a deterministic full link. The system-linker path
is not expected to meet the warm incremental-link latency target.

The incremental linker requires a follow-up ADR before implementation. That ADR
may choose placement, slack, indirection/stub, compaction, signing, and output
publication policies without changing `CodegenUnit` or frontend query identity.

### 13. Diagnostics, work, and determinism belong to requests

Every query attempt freezes its own semantic diagnostic/warning identities and
payloads plus a separately stamped current-position projection and structural
work record. The semantic batch owns diagnostic identity, severity, message,
notes, and producer order; it excludes module-absolute offsets, lines, columns,
and raw spans. A root request publishes only batches observed through its
dependency closure and joins them with position projections from the request's
pinned attempt. Reused and joined results retain origin provenance without
duplicating logical diagnostics.

Execution order never determines presentation order. The request adapter merges
and sorts observed batches using stable source identity, current source
positions, and producer order. A whitespace-only position shift updates rendered
locations without reddening semantic, CFG, codegen, or image-plan terminals.
Parallel failure does not race to choose the one user-visible error;
family-defined collection and presentation rules remain deterministic.

Structural metrics distinguish requested, computed, reused, joined, canceled,
speculative, red-equivalent, invalidated, and evicted work. Phase counts prove
that an edit did not parse, analyze, build, optimize, or code-generate unrelated
entities. Wall time alone is not evidence of incremental correctness.

### 14. Retention, persistence, and long-lived service use

The memo database is memory-budgeted. Retention policy accounts for artifact
bytes, dependency/reverse-dependency pins, active waiters, current roots,
last-good roots, and persistent-cache eligibility. It is not a fixed count of
terminals shared by an entire query family.

The first implementation may retain only in-process typed values. Logical keys,
canonical artifact envelopes, schema epochs, content fingerprints, and
validation rules must nevertheless be request-independent so a future
persistent cache does not require redefining query identity.

Persistent storage, a filesystem watcher, and a daemon/LSP workspace service are
separate implementation phases. They reuse the same immutable revision and memo
database contracts. A persistent codec may store only the canonical owned forms
authorized by ADR-0058 and this ADR; live compiler handles and Rust memory layout
are never serialized.

### 15. One compiler graph and supported facade

`CompilerSession` remains the supported compiler facade from ADR-0061, but it
evolves from an exclusively mutable phase orchestrator into a handle over the
revision/input owner, concurrent memo database, scheduler, and immutable
published artifact views. Exact public method shapes are handled under
ADR-0061's facade rules.

Batch compilation, `--emit` presentation, benchmarks, the oracle, a future LSP,
and a future incremental linker request artifacts through the same graph.
Presentation queries project canonical artifacts; they do not call free backend
helpers or run a second lowering/regalloc/emission path.

The implementation may use an in-house engine, Salsa, or another substrate only
if one database owns query state. A migration may not keep the current store as a
peer cache/state machine behind a second query library. Substrate selection must
preserve Rue's attempt history, diagnostic provenance, current/last-good roots,
host import protocol, cancellation, red/green semantics, and differential
oracle.

## Implementation Phases

Tracked in Linear under the RUE-648 epic. The dependency order is:

- [ ] **Phase 0: Runtime prototype and benchmark gate.** Prove exact-key
  claim-or-join, different-key parallel execution, red/green propagation,
  deterministic diagnostics, cancellation/revision isolation, exact cycle
  handling, bounded retention, and progress with one permit and adversarial
  claim/join/dependency schedules on representative query shapes. Compare an
  in-house evolution with a query-library prototype if substrate choice remains
  open. — RUE-1022
- [ ] **Phase 1: Revisioned keyed database and compatibility shim.** Introduce
  immutable revisions, per-key nodes, joined waiters, task-scoped dependency
  recording, and single-worker scheduling beneath a compatibility shim over the
  selected-state API. Do not require all query families to change call
  discipline in one diff. — RUE-1021 (blocked by RUE-1022)
- [ ] **Phase 2: Source and import inputs.** Publish per-module source leaves,
  fulfill rooted missing import observations in deduplicated frontier batches,
  prohibit speculative host demands, and preserve canonical read policy,
  precedence, provenance, and discovery-epoch reuse across successor attempts.
  — RUE-1023 (blocked by RUE-1021)
- [ ] **Phase 3: Module syntax/RIR queries.** Parse and lower demanded modules,
  provide stable definition/name/import indexes, and keep whole-program views as
  thin projections. — RUE-1024 (blocked by RUE-1023)
- [ ] **Phase 4: Complete stable semantic identity.** Cover named definitions,
  specializations, anonymous nominals, methods/destructors,
  definition-relative structural anchors, and synthesized entities with
  schedule- and position-independent keys. — RUE-1025 (blocked by RUE-1024)
- [ ] **Phase 5: Declaration and comptime queries.** Move shells, signatures,
  constants, type constructors, method lookup, and domain-specific cycle
  handling behind keyed canonical queries. — RUE-1026 (blocked by RUE-1025)
- [ ] **Phase 6: Per-body semantics and projections.** Analyze one body per
  query, publish canonical bodies and independently stamped `BodyReferences`,
  including deterministic references from failed bodies, and remove the
  whole-program mutable Sema epoch as an authority. — RUE-1027 (blocked by
  RUE-1026)
- [ ] **Phase 7: Reachability and parallel scheduling.** Implement
  database-owned reachability with per-identity memberships, correct edge
  deletion, and separate addition/deletion measurement gates; handle legal call
  SCCs; move existing Rayon CFG/backend parallelism onto the shared budget; and
  prove progress plus one-worker/many-worker equivalence. — RUE-1028 (blocked by
  RUE-1027)
- [ ] **Phase 8: Type/layout/ABI/drop queries.** Replace full-pool scans and
  destructor roots with demand-driven facts, layouts, call ABI, and glue. —
  RUE-1029 (blocked by RUE-1028)
- [ ] **Phase 9: CFG and optimization queries.** Publish per-function
  unoptimized and optimized CFG artifacts with precise layout, warning, string,
  symbol, and interprocedural dependencies. — RUE-1030 (blocked by RUE-1029)
- [ ] **Phase 10: MIR and `CodegenUnit` queries.** Query both native backends per
  reached function, normalize link atoms, migrate all backend presentation to
  the canonical path, and retain only justified backend boundaries. — RUE-1031
  (blocked by RUE-1030)
- [ ] **Phase 11: `ProgramImagePlan` and fresh-link adapter.** Aggregate stable
  typed units, project the current per-function objects, invoke the existing
  fresh internal/system linker paths, and establish the delta/fingerprint
  contract for follow-up direct and incremental internal linking. — RUE-1032
  (blocked by RUE-1031)
- [ ] **Phase 12: Compatibility and performance completion.** Generalize the
  cold-versus-reused oracle, add multi-worker determinism and cancellation
  schedules and source-position-shift cases, delete the selected-state
  compatibility shim and peer cache state, enforce memory budgets, and publish
  warm edit-to-codegen and edit-to-runnable baselines. — RUE-1033 (blocked by
  RUE-1032)

Each family migrates through the compatibility shim one at a time and must pass
the cold-versus-reused differential oracle before the next family moves. Each
phase is additive until its replacement is proven. The superseded whole-program
cache/path is deleted in the phase that establishes the canonical query path;
the selected-state shim is deleted in Phase 12 and compatibility code does not
remain as a peer compiler graph.

## Acceptance and validation

The implementation is complete through code generation only when tests prove:

- importing and using one std submodule does not read, parse, analyze, or
  generate code for unrelated std modules, including under speculative
  execution;
- a cold import build publishes at most one successor input revision per
  demand-frontier round rather than one revision per imported module;
- an unreachable declaration contributes no semantic diagnostic or codegen
  unit, including under speculative query execution;
- current-target comptime selection contributes references only from the chosen
  branch;
- ordinary recursive calls terminate reachability without query-cycle errors;
- deterministic semantic failures retain every positively resolved body
  reference, while canceled or incomplete scans publish no reference terminal;
- adding and removing call edges updates exact per-identity reachability and
  satisfies the Phase 7 work/latency gate;
- true const, type-layout, import, and specialization cycles are deterministic;
- editing an unreachable source invalidates no rooted downstream terminal;
- editing a callee implementation preserves ordinary callers unless an
  interprocedural optimization explicitly observed that implementation;
- changing optimization preserves syntax, declarations, bodies, layouts, and
  unoptimized CFGs;
- named/anonymous destructors and glue are generated only for reached types;
- inserting whitespace or comments above definitions changes no stable
  identity, semantic terminal, codegen fingerprint, codegen unit, or image-plan
  entry; only current diagnostic/source-position projections may change;
- a one-worker budget and adversarial claim/join/dependency schedules make
  progress without permit-starvation deadlock;
- one-worker and many-worker requests produce identical diagnostics, reached
  identities, CFGs, codegen atoms, relocations, and executable bytes;
- cold, reused, joined, recovered-failure, canceled, and evicted schedules are
  differentially equivalent to a fresh session;
- retained bytes and dependency pins stay within configured memory budgets;
- a single-function warm edit measures exact phase work and latency through
  replacement `CodegenUnit` and through the fresh internal link; and
- `--emit` and tooling views consume the same terminals as normal compilation.

The future incremental-linker project additionally requires exact add/change/
remove plan tests, stable-address and growth cases, reverse-relocation patching,
runtime archive changes, deterministic full-link fallback, atomic publication,
and warm edit-to-runnable benchmarks on every supported target.

## Consequences

### Positive

- Foreground work scales with demanded program structure rather than loaded
  library size.
- Independent module, declaration, body, CFG, and codegen work can run in
  parallel without changing identity or diagnostics.
- Red/green projections prevent implementation-only edits from causing broad
  downstream invalidation.
- Stable codegen atoms create a direct path to a stateful incremental internal
  linker and avoid object encode/parse churn.
- The same canonical artifacts support batch compilation, interactive tooling,
  future persistence, and performance oracles.
- Off-target code remains outside the observable semantic closure without
  forbidding safe speculative cache warming.

### Negative

- The query engine becomes a concurrent revisioned system with difficult
  cancellation, wait-cycle, eviction, and publication invariants.
- Per-entity canonicalization and validation add overhead to cold builds; query
  granularity and projections must be benchmarked early.
- Stable anonymous/specialization/data identities become reviewed internal
  contracts rather than incidental allocation results.
- Function-local semantic/CFG epochs require deliberate relocation at phase
  boundaries and may initially duplicate small type/symbol tables.
- Deterministic parallel diagnostics and work accounting require explicit
  collection rather than mutation during traversal.
- An eventual incremental linker adds persistent mutable placement state and
  must retain a reliable full-link fallback.

### Neutral

- This ADR does not change Rue runtime semantics, source syntax, or the meaning
  of comptime selection.
- It does not require item-level parsing, a particular query library, persistent
  serialization in the first implementation, or incremental system linking.
- It does not require every compiler pass to become a query.
- A serial scheduler remains a supported execution configuration and the first
  implementation step.

## Rejected alternatives

### Add locks to the current `CompilerSession`

Rejected. The current selected-family state, global mutable graph/attempt
orchestration, and request-local semantic universe are serial ownership
boundaries. Locks would add contention and cross-thread deadlock hazards without
providing immutable revisions or per-key joining.

### Reconstruct one reachable whole-program semantic epoch

Rejected as the canonical downstream boundary. Re-importing every reached type
and body after an edit creates an O(reachable-program) serial barrier and makes
compact allocation order a broad invalidation source. Local computation epochs
remain useful inside one query.

### Make body queries depend on callee bodies

Rejected. It turns legal source recursion into query cycles and invalidates
callers on ordinary callee implementation edits. Body reference projections and
the database-owned reachability query express the real dependency.

### Make every backend pass a query

Rejected without measurement. Fine-grained memo tables can cost more than the
passes and complicate concurrency. `OptimizedCfg`, optional `LoweredMir`, and
`CodegenUnit` are the initial consumer/reuse boundaries.

### Use serialized object files as the incremental linker identity

Rejected for the internal path. Object formats obscure stable atom identity and
require encoding/parsing work. They remain necessary compatibility projections
for system tools.

### Implement incremental linking in the first migration

Rejected. The frontend/query/codegen ownership migration is already large. A
fresh linker consuming stable `ProgramImagePlan` values proves the boundary;
stateful placement and patching follow under a dedicated ADR and project.

### Preserve ADR-0045's within-invocation-only scope

Rejected. It cannot express the desired warm long-lived compiler or eventual
persistent cache/link state. Observable root-driven semantics are retained while
the architectural scope expands.

## Open Questions

- Which execution substrate best satisfies Rue's attempt/diagnostic/import and
  current/last-good requirements after the Phase 0 comparison: an evolution of
  the in-house database, Salsa, or another typed query runtime?
- Which initial memory budget and eviction policy should gate the representative
  project benchmark?
- Which fixed warm-edit benchmark corpus and host normalization should define
  the first latency budgets without turning one machine's number into a language
  promise?
- Is `LoweredMir` independently valuable enough to retain as a memo terminal, or
  should MIR presentation be a projection/debug recomputation from
  `OptimizedCfg` until measurements justify it?

These questions affect implementation and measured policy, not the accepted
identity, revision, dependency, `CodegenUnit`, or linking-seam decisions.

## Future Work

- Stateful incremental internal linking under a follow-up ADR.
- Persistent cache codec and cross-process cache namespace policy.
- Workspace daemon, filesystem watcher, and LSP request integration.
- Speculative scheduling policy and idle-time cache warming.
- Distributed or remote query execution, if local artifact stability and
  serialization make it useful.
- More precise interprocedural optimization projections as inlining and other
  whole-program optimizations mature.

## References

- [ADR-0045: Lazy semantic analysis](0045-lazy-semantic-analysis.md)
- [ADR-0050: Stable semantic dependency manifests](0050-semantic-dependency-manifest.md)
- [ADR-0051: Canonical import resolution authority](0051-canonical-import-resolution-authority.md)
- [ADR-0052: Canonical physical type layout](0052-canonical-physical-type-layout.md)
- [ADR-0053: Typed compiler query state](0053-typed-compiler-query-state.md)
- [ADR-0055: Typed compiler-runtime ABI manifest](0055-typed-runtime-abi-manifest.md)
- [ADR-0058: Canonical semantic artifact algebra](0058-canonical-semantic-artifact-algebra.md)
- [ADR-0061: Supported compiler facade](0061-supported-compiler-facade.md)
- [Body analysis and CFG incrementality audit](../notes/body-analysis-cfg-incrementality-audit.md)
- [Compiler session architecture completion audit](../notes/canonical-query-completion-audit.md)
