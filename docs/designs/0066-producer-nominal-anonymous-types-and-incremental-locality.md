---
id: 0066
title: "Producer-nominal anonymous types and incremental locality"
status: proposal
tags: [types, semantics, comptime, incremental, performance, parallelism]
feature-flag: null
created: 2026-07-21
accepted:
implemented:
spec-sections: ["4.14"]
superseded-by:
amends: [0025, 0029, 0063]
---

# ADR-0066: Producer-nominal anonymous types and incremental locality

## Status

Proposal, owned by RUE-1088. This ADR amends the anonymous-type semantics
introduced by ADR-0025 and ADR-0029 and the incremental architecture in
ADR-0063. Those ADRs remain accepted or implemented as recorded.

Steve and Dorian have approved the producer-nominal language decision, and
RUE-1089 is authorized to implement and land that decision before this broader
ADR is accepted. This is an explicit implementation-before-ADR-acceptance
exception. ADR-0066 remains a proposal until RUE-1090's measurement gate,
RUE-1091's activated repair, and RUE-1092's prototype/adversarial review
complete; RUE-1093 acceptance is blocked by them. The repair must include a
rerun of the relevant RUE-1090 measurements plus RUE-1092 sign-off on the
repaired result; pre-repair evidence cannot satisfy the final gate. The live
specification and executable cases still land atomically with RUE-1089, never
with this draft.

## Summary

Anonymous `struct` and `enum` declaration expressions are producer-nominal.
Their identity comes from the selected declaration expression under its static
enclosing comptime specialization, rather than from structural comparison of
their fields, variants, methods, or bodies. This makes nominal type identity
observable and makes incremental compilation local: a type computation has one
owner and one canonical result path.

The compiler architecture uses stable logical identity, exact dependencies, and
local projections. It removes cross-producer equivalence classes and their
representative/restart coordination. Performance acceptance requires structural
work counters as well as controlled latency, allocation, and memory measures.

## Context

The current live rules 4.14:8, 4.14:15, 4.14:21, and 4.14:25 define anonymous
types structurally. Current executable cases also demonstrate that structural
identity may select a stable representative for method and destructor bodies:
`crates/rue-spec/cases/expressions/comptime.toml` and
`crates/rue-spec/cases/types/destructors.toml` contain the cases named in the
migration inventory below. That behavior requires global cross-producer
comparison, representative selection, and invalidation/restart coordination.

It conflicts with a demand-driven, retained, parallel-ready query graph. An
unrelated anonymous declaration must not change which body represents an
already-reached type, or make a body scan an expanding universe of declarations.
The existing system also needs explicit ownership and eviction rules before
retention can safely span rooted demand.

## Decision

### 1. Observable language semantics

Every anonymous `struct` or `enum` declaration expression is
producer-nominal. The *producer* is the selected anonymous declaration
expression, not every function that returns `type`.

The identity of an anonymous type is determined by that selected producer under
its static enclosing comptime specialization. Declared comptime arguments and
enclosing generic or comptime specializations distinguish specializations.
Repeated evaluation of the same producer under the same canonical
specialization denotes the same type. A different producer,
definition-relative anchor, or specialization denotes a different
type even when fields, variants, method signatures, and bodies are identical.

Rue currently has no comptime loop evaluation that can repeatedly select an
anonymous declaration within one specialization. This ADR therefore adds no
iteration dimension to identity. A future comptime-loop feature must specify
its observable identity rule and incremental-stability consequences explicitly;
until then, the same producer under the same canonical specialization is one
type wherever it is evaluated.

A function that forwards an existing type does not mint another type. For
example, `fn Id(comptime T: type) -> type { T }` returns `T`'s identity. Aliases
also preserve identity. Within one source revision, evaluations with the same
identity must agree on type content. A source edit may change content and cause
its users to be rechecked; this is not a second identity rule.

An anonymous type declaration expression is legal as a comptime value and as a
type-constructor result. It is not legal directly, or nested inside another
type, in a type annotation. Users bind the resulting type or expose it through
a constructor. Result-typed/headless aggregate literals are future work; this
ADR adds no spelling that can construct an otherwise unnamed annotation type.

This is a hard semantic cut with no preview gate. That is an explicit
maintainer exception to the normal preview policy: a gate would require both
incompatible identity systems and retain the global machinery this change
removes. As a non-normative implementation acceptance criterion, a value from
a same-shape, different-producer anonymous type produces a deterministic type
mismatch diagnostic that identifies the expected and actual producer-derived
types without relying on allocation order.

### 2. Incremental identity and locality

Stable logical identity is an implementation concern, not language equality.
It contains the producer definition/specialization, its definition-relative
structural anchor, and canonical specialization context. A fingerprint is
derived, collision-aware metadata for indexing and validation; it never decides
semantic equality or substitutes for exact identity comparison.

Captured or external comptime values that affect a type's content are exact
query dependencies. They are not dynamically branch-dependent pieces of the
type's identity tuple. This preserves a stable owner while still invalidating
content users precisely.

There is one canonical computation path. The implementation removes
cross-producer structural equivalence classes, stable-min representatives,
alias-collapse restarts, and their related coordination. A query for one entity
may not do `O(universe)` work. Input-derived aggregates are permitted only as
memoized, narrow, independently stamped projections. Runtime criteria include
body-local lookup memoization, hashed typed-key lookup, and lazy display
identity; formatting or inspection must not make semantic lookup scan the type
universe.

Retention leases make retained demand bounded and explicit. The first rooted
observation acquires the root lease; its transitive dependency closure inherits
pins; publication promotes the observed terminals while that lease is live; and
completion, cancellation, or supersession releases the lease and all inherited
pins. Speculative terminals receive no root lease and are evictable. Pressure
reclamation evicts unleased, superseded, or speculative terminals according to a
deterministic policy before it may reject a request. The accounting invariant is
that every retained terminal is either owned by one or more live lease closures,
or is explicitly reclaimable; no released lease leaves an unaccounted pin.
Forced eviction is followed by the same warm-versus-fresh parity oracle as an
ordinary edit.

The current source uses a transitional fixed `BODY_QUERY_MEMO_RETENTION` cap of
65,536 in `crates/rue-compiler/src/revisioned_query_database.rs`; its source
comment identifies exact rooted membership as the RUE-1028 replacement. RUE-1087
replaces that cap-based safety argument with the lease protocol above.

Specialization overflow uses a stabilized minimum-depth/quiescence rule and a
deterministic witness. It introduces neither breadth-first barriers nor
schedule-dependent thresholds. The current counting convention is specialization
instantiation edges from a root at depth zero: `chain(63)` materializes 64
specializations and succeeds; `chain(64)` would materialize 65 and fails.
`session::tests::reused_specializations_consume_the_persistent_round_budget`
in `crates/rue-compiler/src/session.rs` constructs `source(63)` and
`source(64)` across a reused session. D1 and the prototype retain explicit
boundary cases with that convention.

The query APIs are parallel-ready: correctness is independent of evaluation
order; they have no global exclusive traversal state; no lock is held while a
query body or dependency executes; and the design remains compatible with the
configured concurrency budget. A warm-session result and diagnostics must equal
those from a fresh session after every benchmark edit.

Code generation exposes independently keyed per-function, data, and symbol
artifacts plus a changed-symbol set. Initial linking may remain fresh, but this
is the seam at which an incremental linker consumes a delta without another
frontend or semantic refactor.

### 3. Measurement and decision gate

Measurement is a first-class acceptance surface. Every benchmark records
structural counters and performs separate controlled latency, allocation-count,
and peak-memory runs. The counting allocator is never enabled for timing runs.
The eventual north-star is approximately 45 ms pre-link warm incremental
compilation on a defined reference host and corpus; it is a project target, not
a language guarantee.

**Recorded prediction (pre-implementation, 2026-07-21):** hashed typed-key lookup plus D1 machinery deletion will not materially reduce the O(bodies × declarations) per-body installation/projection/endpoint term (~62% of cold wall time at Caldera scale). **Decision rule:** after D1 lands, if the scaling harness shows per-body install/project/endpoint work still increasing with unrelated-declaration count, the shared-base or narrow-epoch repair proceeds; an incidental wall-time improvement without flat per-body counters is not success.

The 62% figure in that recorded prediction is a pre-implementation hypothesis,
not repository-proven evidence in this ADR. Before RUE-1090 uses it, RUE-1086
attaches the raw Caldera artifact, command, base commit, reference host and
configuration, and samples that establish its provenance. RUE-1086 has attached
that machine-readable record at
`docs/benchmarks/rue-1086-caldera-baseline.json`; it separates the 62%
install/project/endpoint share (the figure the gate reads) from the distinct
~85% total per-body setup share, so the two are attributed rather than
conflated.

### 4. Activated per-body context repair

RUE-1090 observed the predicted non-flat structural term after the
producer-nominal and trusted-standard-library cuts. With a fixed body count,
per-body declaration projection, installation, and endpoint work still grows
with unrelated declarations. RUE-1091 is therefore required; wall-time changes
cannot cancel it.

The current source has two independent causes which the repair must remove:

1. A cold `BodyTransaction` calls `analyze_body_query`, which creates a fresh
   semantic epoch, prepares every declaration shell, projects and installs every
   durable declaration, issues the complete definition/token universe, and
   installs every endpoint before analyzing one body.
2. Each body observes aggregate module declaration sets and accepted import
   topology. Those edges preserve correctness for negative and qualified
   lookups, but they invalidate unrelated bodies whenever any declaration in an
   observed module or the aggregate topology changes.

Fixing only the first cause improves cold work but leaves broad incremental
invalidation. Fixing only the second leaves the cold
`O(declarations × bodies)` reconstruction. RUE-1091 completes only when both
are gone.

#### Authoritative facts and body-local state

The existing semantic-nucleus and exact lookup query terminals are the
authoritative immutable base. RUE-1091 may put a private, data-only recipe cache
in front of those terminals, but it does not introduce a whole-program semantic
epoch or another source of truth.

A declaration recipe contains only owned, stable values: stable definition and
module identities, durable types and comptime values, declaration shape,
visibility, language-item metadata, and other typed facts already suitable for
a query result. It contains no `Sema`, `BoundSema`, `DeclarationShells`, live or
borrowed RIR, `Spur`, compact AIR type or nominal IDs, spans, file-local IDs,
mutable namespace or endpoint tables, or whole-revision stamp.

Definition, module, body-owner, and endpoint recipes carry stable semantic
identities and owned payloads only. Issuer-scoped tokens are never shared:
materializing a recipe mints the consuming overlay's local token and records its
stable-to-local mapping. This prevents a cached endpoint from carrying an ID
issued by a different body epoch.

The recipe cache is a physical optimization, not a dependency authority. An
entry is selected only after the requesting body observes the exact current
query terminal for that fact. Cache identity includes the query family, logical
key, and exact terminal incarnation/stamp. An entry cannot make a stale terminal
valid, and a body never records an edge to a complete cache/base fingerprint.
Entries inherit the lifetime of their query terminal or are independently
evictable; they do not create unleased retained roots.

The cache container is created once in constant work and fills on exact demand;
it does not enumerate the declaration universe at construction. Concurrent
misses for one exact recipe claim or join one construction, while unrelated
recipe keys may build concurrently. No cache shard lock spans recipe
construction or a nested query request. Cache accounting distinguishes an
ordinary first build from rebuilding an equal-stamp fact whose prior terminal
incarnation was evicted and rederived. The latter is correctness-neutral but is
reported separately as retention-induced recipe thrash.

Each body evaluation creates one task-owned `BodySemanticOverlay`. The overlay
owns all compact type/nominal/parameter IDs, inference variables, generated and
anonymous types, local strings, errors, dependency events, well-known
`Option(payload)` materializations, and mutable AIR construction state. It
imports an exact recipe lazily when the body first consumes that fact and keeps
a body-local stable-fact-to-local-ID map so repeated reads within the body are
constant-time. No local ID, mutable pool, or inferred state is shared between
two bodies or retained in a recipe.

Successful body publication converts the overlay result back to the existing
durable body artifact. Compact overlay IDs do not cross the query boundary.
Producer-owned anonymous nominals continue to arrive through their exact
producer terminals and are materialized in the consuming overlay. A
body-specific trusted-standard-library demand likewise remains local to the
demanding overlay. Conversion sorts diagnostics by the existing stable
diagnostic ordering before publication; task schedule, provider observation
order, compact-ID allocation order, cache hits, and warm reuse cannot change
observable diagnostic order. The warm/fresh ordered-diagnostics oracle is the
executable proof of that invariant.

Program-level definitions, references, and dependency manifests continue to
aggregate published body artifacts. Their cost is `O(published bodies and
reported facts)`, and those consumers do not attach a declaration-universe or
aggregate-topology edge back to each body.

This representation makes future parallelism a property of ownership rather
than locking: query terminals and recipes are immutable and shareable; overlays
are disjoint; no global interner or type pool is mutated by body analysis; and
no cache or query-runtime lock is held while a body or nested dependency query
executes.

#### Exact provider boundary

AIR body analysis receives a narrow provider interface instead of a complete
declaration slice or prepared semantic epoch. The compiler implementation of
that interface runs inside the `BodyTransaction` query context. Every provider
read both returns the value used by analysis and records the corresponding
query edge before the body terminal can publish.

The provider exposes typed operations for:

- exact unqualified and qualified name lookup, including empty and ambiguous
  results, visibility filtering, method candidates, and operator candidates;
- exact declaration identity, signature, constant/comptime result, nominal
  well-formedness, anonymous-nominal facts, language-item identity, and
  drop/`@copy` metadata;
- exact module-binding or import resolution for only the paths consulted by the
  lookup, including absent, rejected, and ambiguous results; and
- exact producer-body and trusted-toolchain facts already required by the body.

Each parsed module publishes one immutable name-index input, built once in
`O(module declarations)` for that module revision. The index maps namespace and
name to the stable candidate set and the visibility/kind metadata needed by
resolution; building it does not enumerate other modules or bodies. Name lookup
is keyed by the consulted module, namespace, and name and reads that index in
expected `O(1)` work. Its canonical result includes all candidates needed to
distinguish success, absence, ambiguity, visibility, and kind. After a module
edit, validation rebuilds the module index once and re-evaluates only retained
lookup terminals against that module. Validation fan-out is therefore bounded
by the distinct retained lookups consulted against the edited module, never by
the number of declarations or bodies in the program. Equal lookup output
preserves its stamp and leaves body consumers green; adding a candidate for the
queried name changes the lookup result, including the negative-to-positive
case, and invalidates exactly its consumers.

Lookup families expect one logical terminal per distinct consulted
`(module, namespace, name)` or import-path key, rather than per declaration or
per body occurrence. On semantic-root publication—either success or a
deterministic failure—the compiler promotes the request's exact set of observed
lookup-terminal pins into a session-held `PublishedRootLookupLease`. Negative,
ambiguous, rejected-import, and other deterministic-failure terminals observed
by that published root are included. Promotion acquires no terminal by revision
or family-wide approximation: the request lease remains live until the same
exact pins have transferred, the new set replaces the prior published root
atomically, and only then does batched release enforce retention. An attempt
that aborts or is canceled before publishing a root, and a merely speculative
validation that no published root observes, is never promoted. Thus
edit/error/fix loops retain their last deterministic dependency set while the
current exact lookup working set may grow beyond the configured historical
floor under RUE-1087's grow-with-pressure-and-meter policy, but it cannot be
evicted merely because a large program consults more names than the floor.
Historical incarnations remain
subject to bounded FIFO retention, and unleased logical nodes with no retained
terminal are reclaimable. Pressure metrics report retained logical keys,
terminals, evictions, protected growth, and re-derivations after eviction. A
forced-pressure test exceeds both the current-root working set and historical
retention floor, then publishes a successor and revisits hot positive, negative,
ambiguous, qualified, and import keys: current-root keys remain warm, superseded
cold keys may rederive once, no request-to-root handoff loses a pin, speculative
keys remain evictable, and the released root's unneeded entries return to the
configured historical bound.

The aggregate `module_declaration_sets` loop and aggregate accepted-topology
input are removed from `BodyTransaction` once the exact provider is complete.
Positive semantic references continue to observe their exact semantic-nucleus
terminals. Exact negative, ambiguous, qualified, and import-path observations
are recorded during resolution rather than reconstructed from only the
successful body artifact. A failed or absent module binding is a first-class
terminal result and dependency edge; if a later edit makes that path resolve,
its stamp changes and invalidates exactly the consumers of that failed lookup.

Post-hoc dependency replay is not an accepted provider boundary: analysis may
not read an untracked complete namespace and attach narrower edges afterward.
Likewise, a lexical pre-scan of a body is not proof of semantic dependency
completeness because imports, comptime evaluation, generics, and type-directed
resolution may add or reject lookups. The typed provider call that supplies the
semantic fact is the dependency observation.

A type-level boundary enforces provider completeness. After the production cut,
the rue-air body analyzer can receive only the provider capability, the selected
body/producer inputs, and body-local configuration. No complete merged program,
declaration slice, namespace table, prepared `Sema`/`BoundSema`, aggregate
module-declaration set, or accepted-topology view is reachable through its
types. Rue-air remains independent of the query runtime: the compiler-side
provider implementation owns the `BodyTransaction` query context and returns
owned typed facts through the rue-air trait. Method and operator resolution,
visibility, language items, drop/`@copy`, imports, comptime, anonymous types,
producer bodies, and trusted-toolchain facts have no side channel around that
trait.

A body does not adopt a complete per-revision recipe-base terminal. Even an
exact-terminal adoption capability would make that aggregate terminal a real
body dependency, so a declaration-set change would still invalidate every
adopter. Exact-terminal adoption remains suitable for one immutable artifact
which a successor extends, such as the parse predecessor; it is not a substitute
for exact fact edges when consumers observe different declaration subsets.

#### Query and invalidation invariants

`BodyQueryKey` remains the stable function instance plus explicit semantic
configuration. It does not contain a source revision, complete declaration-set
fingerprint, recipe-cache generation, or dependency stamp. Dependency stamps
remain observed graph edges.

For a body terminal to publish or validate green, it observes:

- its exact raw body and body-level configuration;
- every exact name/import lookup result used by the attempt, including failures;
- every exact declaration/comptime/anonymous/producer fact materialized by its
  overlay; and
- no whole-program declaration or import-topology substitute.

Consequently:

- adding an unrelated declaration recomputes zero previously green bodies;
- changing a declaration signature or value invalidates exactly the bodies
  whose observed facts changed, followed by their ordinary dependent cone;
- editing one body invalidates only that body transaction; later artifacts
  recompute only when their exact published body input changes, and unrelated
  body-semantic terminals remain green; and
- a negative lookup becoming positive, or a positive lookup becoming ambiguous
  or absent, invalidates every and only consumer of that exact lookup.

Removal, rename, and arbitrary source edits are not forced through the
strictly-additive trusted-toolchain successor-overlay protocol. They publish an
ordinary immutable input revision/generation with the changed exact module
source leaf. The logical lookup keys survive that generation boundary: each
affected lookup terminal validates or recomputes against the replacement module
index, preserves its stamp when its exact result is equal, and changes its stamp
when the named candidate set changes. Body terminals then validate against
those exact stamps. No additive declaration overlay and no adopted whole-base
terminal is required to reuse bodies across a removal or rename.

Deterministic failure terminals retain the same exact observations as successful
ones. A failed revision therefore does not discard unrelated body terminals or
turn the next successful revision into a cold compile. Cancellation publishes
no partial overlay or dependency set. Eviction and recomputation preserve the
warm-versus-fresh artifact, diagnostic, reference, and producer-identity oracle.

#### Structural accounting and acceptance

RUE-1121 lands the acceptance rows before the mechanism. Existing
`PerBodyDeclarationContextWork` fields keep their meanings; work is not renamed
from install/project/endpoint into “base” work to make the ratio appear flat.
The repair adds separate counters for:

- recipe entries built, reused, represented, and evicted;
- equal-stamp recipes rebuilt solely because the terminal incarnation was
  evicted and rederived;
- recipe-cache/base containers created and reused;
- overlays created;
- exact provider facts observed and overlay facts materialized;
- body-local type/parameter/endpoint units created; and
- any clone-from-template probe, including copied units, allocations, and peak
  memory.

Every counter is incremented at the operation it measures. A zero-valued
predecessor-work counter needs a mechanical test which makes predecessor
iteration, hashing, comparison, or cloning fail if attempted; an
always-zero field alone is not evidence.

The normal and 10,000-declaration RUE-1090 matrices must report exact-flat
per-body prepare/shell/project/install/endpoint work as unrelated declarations
grow. Cold total work must be
`O(declarations represented once + Σ body-local facts)`. The edit rows assert
the exact recomputed body set and require warm work to be strictly below an
equivalent fresh analysis. Timing, allocation counting, and peak-memory runs
remain separate.

The harness's tracked envelopes deliberately reject intermediate values between
their known-bad witness and repaired target. A mergeable migration slice must
therefore either preserve the current witnessed behavior for a row or complete
the linked counter and invalidation repair needed to reach its target. A
partially narrowed production path cannot weaken, skip, or silently retune the
envelope merely to land.

#### Migration and deletion sequence

The production cut proceeds in dependency order:

1. Land the mechanism-independent RUE-1121 acceptance rows.
2. Introduce owned declaration recipes and the body-local overlay with focused
   conversion, failure, cancellation, and two-overlay isolation tests.
3. Route name, import, declaration, comptime, anonymous, and trusted-toolchain
   reads through the exact provider in focused and test-only differential
   adapters. These adapters are not a selectable production body-analysis path.
4. In one production slice, make `analyze_body_query` consume only the body key,
   exact provider, selected producer facts, and body-local configuration;
   remove its complete merged-program, declaration-shell, durable-declaration,
   and endpoint-universe inputs; and delete the per-body
   prepare/project/install/issue-all-definitions/install-all-endpoints path plus
   aggregate module/topology observations. The slice includes all RUE-1121
   counter and exact-invalidation targets, so it moves each tracked envelope
   directly from its known-bad witness to its repaired target.
5. In that same slice, add the named source guard
   `body_analysis_has_no_whole_program_context_path`. It inspects the production
   body-analyzer signature and implementation and rejects complete-context types
   or calls to the retired prepare/project/install/endpoint and aggregate
   module/topology symbols. This is an enduring capability guard, not a
   transitional old/new-path assertion.
6. Rerun both RUE-1090 matrices, the RUE-1121 invalidation rows, differential
   oracles, forced eviction/cancellation tests, schedule permutations, and the
   Caldera budget before RUE-1092 sign-off.

A test-only differential adapter may evaluate the old and new implementations
during the cut, but it cannot ship as a second production body-analysis path.
Each mergeable implementation slice either preserves the one production path or
deletes the replaced path in the same slice.

## Staged specification amendment

This section is exact proposed replacement text. Each replacement includes all
prose and examples owned by that rule; D1 replaces that complete content while
retaining intervening section headings and the rule marker itself. It is
deliberately not applied to `docs/spec` in this ADR because the current compiler
and its executable cases still implement structural semantics. The D1 compiler
cut applies these complete blocks, updates traceability, and converts the
inventory below in the same change.

### Replace rule 4.14:8

> Each anonymous struct declaration expression denotes a producer-nominal type.
> Its identity is the selected declaration expression under its static enclosing
> comptime specialization. Declared comptime arguments and enclosing
> specializations distinguish anonymous type specializations. Repeated evaluation of the same
> declaration expression under the same canonical specialization denotes the
> same type. A different declaration expression or specialization denotes a
> different type, regardless
> of equal fields, method signatures, or method bodies.
>
> ```rue
> fn make_point1() -> type { struct { x: i32, y: i32 } }
> fn make_point2() -> type { struct { x: i32, y: i32 } }
>
> fn main() -> i32 {
>     let P1 = make_point1();
>     let P2 = make_point2();
>     let p1: P1 = P1 { x: 10, y: 20 };
>     let p2: P2 = p1;  // ERROR: P1 and P2 have different producers
>     p2.x + p2.y
> }
> ```
>
> Anonymous structs produced by different declaration expressions or
> specializations are different types and are not assignable to each other,
> including when their fields are equal.

### Replace rule 4.14:15

> Method definitions are content of the producer-nominal anonymous struct type
> selected by their enclosing declaration expression. Fields, method names,
> signatures, declaration order, and method bodies do not make two different
> anonymous struct declaration expressions the same type. `Self` in each method
> denotes that enclosing producer-nominal type.
>
> ```rue
> fn A() -> type {
>     struct { x: i32, fn get(self) -> i32 { self.x } }
> }
>
> fn B() -> type {
>     // Different from A(): B's declaration expression is a distinct producer.
>     struct { x: i32, fn get(self) -> i32 { self.x } }
> }
>
> fn C() -> type {
>     // Also different from A() and B(), independently of this signature change.
>     struct { x: i32, fn get(self) -> i64 { @intCast(self.x) } }
> }
> ```

### Replace rule 4.14:21

> Each anonymous enum declaration expression denotes a producer-nominal type.
> Its identity is the selected declaration expression under its static enclosing
> comptime specialization. Declared comptime arguments and enclosing
> specializations distinguish anonymous type specializations. Repeated evaluation of the same
> declaration expression under the same canonical specialization denotes the
> same type. A different declaration expression or specialization denotes a
> different type, regardless
> of equal variant names or payload types.
>
> ```rue
> fn Option(comptime T: type) -> type { enum { Some(T), None } }
>
> fn main() -> i32 {
>     let A = Option(i32);
>     let B = Option(i32);
>     let x: A = A.Some(10);
>     let y: B = x;  // OK: A and B select the same producer and specialization
>     match y { B.Some(n) => n, B.None => 0 }
> }
> ```

### Replace rule 4.14:22

> A comptime function whose declared return type is `type` is a *type
> constructor* (equivalently, a *generic type*). Its body evaluates at compile
> time to any comptime type value. When evaluation selects an anonymous struct
> declaration expression or anonymous enum declaration expression, that
> expression denotes the producer-nominal type defined by rules 4.14:8 and
> 4.14:21. When evaluation returns an existing type value, it preserves that
> type's identity. Calling a type constructor is *type-function application*;
> the call is evaluated at compile time and reduces to that concrete type.
> Because application is comptime evaluation, every argument must be
> compile-time known (rule 4.14:6), and each `type`-typed argument must be
> supplied by a `comptime` parameter or another type value.
>
> In *value position*, the reduced type is an ordinary compile-time type value:
> it may be bound with `let` and then used as the path of a struct-literal
> expression (`P { … }`), a method call, or an associated-function call
> (`P.origin()`), exactly as in rules 4.14:7 through 4.14:13.
>
> ```rue
> fn Option(comptime T: type) -> type { enum { Some(T), None } }
>
> fn main() -> i32 {
>     let O = Option(i32);        // type-function application in value position
>     let x: O = O.Some(42);
>     match x { O.Some(n) => n, O.None => 0 }
> }
> ```
>
> A type constructor may forward an existing type value without minting another
> type:
>
> ```rue
> fn Id(comptime T: type) -> type { T }
> fn Pair(comptime T: type) -> type { struct { first: T, second: T } }
>
> fn main() -> i32 {
>     let P = Pair(i32);
>     let Q = Id(P);
>     let p: P = P { first: 20, second: 22 };
>     let q: Q = p;  // OK: Q preserves P's identity
>     q.first + q.second
> }
> ```

### Replace rule 4.14:25

> Type-function application monomorphizes each canonical specialization
> independently. When evaluation selects an anonymous struct or enum declaration
> expression, that expression under the application's static enclosing comptime
> specialization determines the resulting type identity. Different canonical
> arguments or enclosing specializations select distinct specializations; equal
> canonical specializations select the same type wherever evaluated. A function
> that returns an existing type value, rather than selecting an anonymous
> declaration expression, preserves the returned type's identity. Aliases also
> preserve identity.
>
> Distinct producer-nominal specializations do not converge merely because
> their contents are equal. Recursive instantiation that selects a new
> specialization at each step remains subject to the specialization-depth limit
> in rule 4.14:18.
>
> ```rue
> fn Pair(comptime T: type) -> type { struct { first: T, second: T } }
>
> fn produce() -> Pair(i32) {
>     let P = Pair(i32);
>     P { first: 10, second: 5 }
> }
>
> fn consume(p: Pair(i32)) -> i32 {  // same producer and specialization
>     p.first + p.second
> }
>
> fn main() -> i32 {
>     consume(produce())  // 15
> }
> ```

### Add rule 4.14:23a, immediately after rule 4.14:23

> An anonymous struct or enum declaration expression may not appear directly or
> nested within a type annotation. This restriction applies to `let`, parameter,
> return, field, array-element, and pointer-pointee annotations. A type
> constructor call remains permitted in those positions provided its argument
> expressions do not themselves contain an anonymous declaration expression.
> This containment test is syntactic: it examines the spelling of the annotation
> and its argument expressions, not the type values to which those expressions
> evaluate. Anonymous declaration expressions remain permitted as comptime
> values and as type-constructor results; a program that needs to use one in an
> annotation first binds the type value or names it through a type constructor.
> Value-position and path-head uses described by rules 4.14:22 and 4.14:23 are
> not type annotations; they remain governed by the path-head grammar.

The new rule uses the `4.14:23a` identifier to keep the existing generated
traceability stable; the D1 change adds its normative test coverage.

### Corpus migration inventory

The current corpus was inspected at 2026-07-21. The following success cases
flip to deterministic type-mismatch failures because they assign between
different producers: `anon_struct_structural_equality`
(`comptime.toml`, rule 4.14:8),
`anon_struct_structural_equality_with_methods`,
`anon_struct_method_order_is_not_structural`, and
`anon_struct_nested_method_signature_types_are_structural` (rule 4.14:15).

`anon_enum_structural_reuse` remains a success: both sides select the same
`Option(i32)` producer and canonical specialization. It is renamed and
reworded as same-producer reuse. D1 adds a different-producer, same-shape enum
assignment mismatch case. `instantiation_identity_is_structural_across_functions`
(rule 4.14:25) also remains a success only because both annotations select the
same producer and canonical specialization; it is renamed to state that basis.

`anon_struct_different_fields_different_types` and
`anon_struct_different_field_types` are successful coexistence/distinction
cases, not mismatch failures. They remain successes with names and descriptions
that state producer-local coexistence. D1 adds explicit assignments between
their bindings, and a same-shape different-producer struct assignment, as
compile-fail mismatch cases.

The representative-dependent cases are removed and replaced by producer-local
coverage: `late_specialization_replaces_anonymous_method_with_stable_representative`,
`late_specialization_retracts_stale_anonymous_method_error`,
`late_specialization_does_not_root_abandoned_nested_destructor`,
`structurally_equal_anon_destructor_uses_stable_representative`, and
`late_specialization_replaces_anonymous_destructor_with_stable_representative`.
They occur in `crates/rue-spec/cases/expressions/comptime.toml` and
`crates/rue-spec/cases/types/destructors.toml`.

`alternating_specialization_method_recursion_is_bounded` in
`crates/rue-cli-tests/cases/lazy_specialization_references.toml` changes from a
success to an E1200 specialization-depth failure. Structural representative
convergence currently masks the unbounded
`runaway(n) -> Wrapper(n).go() -> runaway(n + 1)` chain. Under
producer-nominal identity, each `Wrapper(n)` specialization is distinct, so the
chain reaches the existing deterministic depth limit rather than converging by
shape.

The existing negative cases `anon_enum_monomorphized_distinct` and
`instantiation_distinct_arguments_are_distinct_types` remain negative coverage,
but their descriptions stop claiming structural identity. D1 also adds a
forwarding success case for `fn Id(comptime T: type) -> type { T }`, proving
that `Id(T)` preserves `T`'s identity rather than minting another producer. The live
`type_constructor_in_annotation_position`,
`type_constructor_in_return_and_param_position`, and
`type_constructor_in_field_and_array_position` cases remain valid: they apply a
constructor, not an anonymous declaration expression directly. No current
executable case uses a direct or nested anonymous declaration expression in an
annotation; D1 adds both direct and nested rejection cases.

## Prototype plan and falsifiable acceptance matrix

The prototype uses canonical query APIs and a two-dimensional generated corpus.
It does not implement or test unification, structural equivalence, stable
representatives, or restart behavior; those mechanisms are out of scope.

| Scenario | Controlled change | Required observation | Falsifier |
| --- | --- | --- | --- |
| Producer identity | Same producer/specialization; then producer or declared argument differs | First pair has one identity; each changed dimension has a distinct identity and mismatch | Equal shape makes distinct producers assignable, or same producer splits |
| Repeated evaluation | Evaluate one producer repeatedly under one canonical specialization without a comptime loop | Every evaluation has one identity; the warm/fresh result agrees | Evaluation count or order splits the identity |
| Forwarding constructor | `Id(T) -> type { T }` forwards an anonymous type | Returned value has `T`'s identity | `Id` adds an owner identity |
| Captured comptime content | Edit an external/captured comptime input used in a producer body | Content and exact dependents invalidate without identity churn | Captured value enters identity or stale content survives |
| Lookup invalidation | Exercise positive, negative, and ambiguous name lookup, then edit each candidate set | Only the recorded lookup projection invalidates; outcome/diagnostic matches fresh | Unrelated declaration invalidates lookup, or a relevant edit does not |
| Depth stabilization | Run the current `source(63)`/`source(64)` convention in fresh and reused sessions | 63 materializes 64 specializations and succeeds; 64 would require 65 and has the deterministic overflow witness | Boundary, count, or witness depends on schedule or reuse |
| Local work | Independently vary declarations `D`, reached bodies `B`, and unreachable declarations/bodies `U` | Report one-time declaration/index work separately; total rooted work is `O(D + ΣB)`; normalized per-reached-body install/project/endpoint work stays flat as `D` grows; increasing `U` adds zero rooted semantic/codegen work | Total work follows `O(D×B)`, a normalized body counter grows with `D`, or `U` produces rooted semantic/codegen work |
| Lookup-index validation | Edit one module while independently varying other modules, bodies, declarations, and the retained lookup count against the edited module | Rebuild that module's index once; revalidate each retained lookup against it in expected `O(1)`; unchanged results preserve stamps; validation fan-out equals only the retained lookups against that module | One lookup scans declarations, another module contributes work, fan-out follows program size, or an equal result changes stamp |
| Provider capability guard | Inventory the rue-air body-analyzer signature and reachable calls after the production cut | Only the typed provider, selected body/producer facts, and body-local configuration are reachable; all named semantic fact families pass through provider calls | Any complete program/table/epoch capability or semantic side channel remains reachable |
| Retention leases | Force pressure eviction before and after rooted completion, cancellation, and supersession | Live root closures remain pinned; released/speculative terminals reclaim; accounting has no unowned pin; warm result/diagnostics equal fresh after eviction | A live closure evicts, a released pin remains, accounting leaks, or eviction changes observable output |
| Lookup retention pressure | Exceed the lookup family's historical floor with positive, negative, ambiguous, qualified, failed-import, and speculative keys; publish success, deterministic failure, then a fixed successor; revisit hot and superseded keys | Each published request's exact pin set hands off atomically to `PublishedRootLookupLease`; success and deterministic failure remain warm while current; speculative and superseded cold keys reclaim or rederive at most once; released entries return to the historical bound; thrash is metered | A current failure key is evicted, a speculative key becomes rooted, handoff opens a birth-eviction window, retained memory never falls after supersession, or rederivation is invisible |
| Differential oracle | After every edit run reused and fresh sessions over canonical queries | Artifacts, diagnostics, and changed sets agree | Any warm/fresh disagreement |
| Codegen seam | Edit one function, one data item, then one symbol reference | Per-function/data/symbol artifacts identify exactly the changed-symbol set | Whole-program artifact is the only observable delta |
| Schedule audit | Permute dependency evaluation order and review API locking/traversal state | Same result; no global traversal state or lock spans execution | Output, cycle result, or witness changes with schedule |

The harness records cold and warm observations separately. It holds corpus,
host configuration, request roots, revision sequence, worker budget, and cache
state constant for each comparison. Timing, allocation-counting, and peak-memory
jobs are separate invocations. The reference-host/corpus record accompanies the
first benchmark implementation before the 45 ms target is evaluated.

## Workstream sequence

1. RUE-1085: hashed typed-key lookup and lazy display identity.
2. RUE-1086: minimal scaling harness, warm-versus-fresh oracle, and Caldera
   artifact provenance.
3. RUE-1087: retention leases.
4. RUE-1088: this ADR draft and staged specification wording.
5. RUE-1089: D1 producer-nominal semantics implementation, live normative spec
   and executable-case update, and deletion of structural
   equivalence/representative/restart machinery.
6. RUE-1090: measurement against the recorded prediction.
7. RUE-1091: activated data-only recipe-base and body-local-overlay repair,
   including exact body lookup dependencies, a rerun of the relevant RUE-1090
   structural measurements, and RUE-1092 prototype/adversarial sign-off on the
   repaired result.
8. RUE-1092: vertical prototype and adversarial review.
9. RUE-1093: accept ADR-0066 only after RUE-1090, RUE-1091, and RUE-1092
   complete; then resume the remaining RUE-1028 work.

The specification amendment may be drafted and reviewed in parallel with
RUE-1085 through RUE-1087, but live specification and executable-case edits
land atomically with RUE-1089. RUE-1092 may run in parallel with RUE-1090 after
RUE-1089; acceptance awaits both required gates.
RUE-1091's post-repair measurement and review evidence replaces the
corresponding pre-repair evidence for acceptance.

## Consequences

### Positive

- Anonymous type identity is predictable from source and static specialization.
- Same-shape declarations cannot silently share methods, destructors, or errors.
- Query ownership, invalidation, retention, and future parallel execution have
  local invariants rather than global representative coordination.
- Fine-grained artifacts provide an incremental-link extension point.

### Costs

- Source programs that rely on structural interchangeability must share a
  binding or constructor deliberately.
- The hard cut updates language behavior without a compatibility gate.
- D1 is not accepted merely by compiling: it must satisfy the matrix and the
  structural measurement gate.

## Rejected alternatives

Keeping structural equality behind a preview gate is rejected because it would
keep two incompatible identity systems and the global machinery under removal.
Using fingerprints as semantic identity is rejected because hashes are derived
metadata and collision handling cannot define the language. Optimizing the old
representative model before D1 is rejected because it delays the deletion that
establishes producer ownership.

## References

- [ADR-0025: Compile-Time Execution](0025-comptime.md)
- [ADR-0029: Anonymous Struct Methods](0029-anonymous-struct-methods.md)
- [ADR-0063: Parallel demand-driven incremental compilation](0063-parallel-demand-driven-incremental-compilation.md)
- `docs/spec/src/04-expressions/14-comptime.md`
- `crates/rue-spec/cases/expressions/comptime.toml`
- `crates/rue-spec/cases/types/destructors.toml`
- `docs/benchmarks/rue-1086-caldera-baseline.json` (RUE-1086 raw Caldera
  provenance, reference-host configuration, and runner samples)
