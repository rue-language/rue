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
exception. ADR-0066 remains a proposal until both RUE-1090's measurement gate
and RUE-1092's prototype/adversarial review complete; RUE-1093 acceptance is
blocked by both. If RUE-1090 activates RUE-1091, that repair also blocks
acceptance and must include a rerun of the relevant RUE-1090 measurements plus
RUE-1092 sign-off on the repaired result; pre-repair evidence cannot satisfy the
final gate. Otherwise RUE-1091 is cancelled. The live specification and
executable cases still land atomically with RUE-1089, never with this draft.

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

## Proposed post-D1 compiler contract

This section records the RUE-1093 acceptance amendments. It remains
non-authoritative while this ADR has proposal status. Its purpose is to make
the remaining ADR-0063 work falsifiable before implementation resumes, rather
than allowing another aggregate adapter to become a second compiler pipeline.

### Artifact identity and change detection

The implementation keeps four concepts distinct:

1. A source identity names exact source bytes and their logical module mapping.
   It is input and provenance identity and may change for a semantically neutral
   edit.
2. A source-partition input fingerprint is versioned, collision-aware metadata
   for validating a particular raw semantic input. It is neither semantic
   equality nor an artifact key. The existing stable definition and import
   content fingerprints belong to this category.
3. An artifact has an exact stable key and a versioned, domain-separated
   fingerprint of canonical artifact content. Exact structural comparison is
   authoritative after a fingerprint match. Artifact fingerprints exclude raw
   spans, file-local IDs, diagnostics, rendered text, work metrics, and linker
   mode. Target or feature configuration appears only where it can change the
   artifact.
4. A diagnostic has a stable diagnostic identity and a separately stamped
   source position. Neither is an artifact key or artifact content.

The independently stamped artifact projections include definition identity,
nominal content, callable signature, reduced comptime value, body
implementation, body references, multiplicity, drop facts and plans, layout,
call ABI, optimized CFG, code-generation-unit content, and reachable
membership. A consumer observes the narrow projection that can change its
answer. It does not observe a whole declaration, semantic epoch, or source
partition merely because that aggregate was convenient to construct.

A type-level comptime expression depends downstream on its canonical reduced
value, while the producer body remains a transitive validation dependency of
the comptime-value query. Editing the producer to compute the same value may
recompute that query but leaves type, layout, and ABI artifacts green. Editing
it to compute a different value invalidates those dependents. Named constants
and nested comptime calls follow the same rule. Deterministic failure is a
separate terminal; cancellation publishes no terminal.

### Body locality, diagnostics, and reachability

The post-D1 semantic provider consists of an immutable, data-only declaration
base plus body-local mutable overlays. It does not retain or expose a cloneable
whole `Sema`, `BoundSema`, type pool, raw RIR handle, source span, or other
epoch-local ID as a query artifact. The registered body evaluator owns exact
body requests and publishes one atomic body transaction. Deterministic failure
retains the independently useful body references needed for reachability;
cancellation or incomplete work publishes neither a body terminal nor a
reference terminal.

RUE-1112's independent amendment to ADR-0038 establishes that language-owned
optionality and fallibility use the exact trusted producers
`\0rue-std/option.rue::Option` and `\0rue-std/result.rue::Result`. This ADR
records the incremental dependency boundary implied by that language decision;
it does not replace ADR-0038's normative error-handling record. Every fallible
intrinsic returns an exact specialization of the trusted `Option` in every
context. An expected type only checks compatibility; it never selects the
producer, so annotating the result with a user-defined lookalike is a type
error. `?` recognizes only exact specializations of those two trusted producer
families, never enum shape. An enclosing trusted `Option` may have a different
success payload from the operand. An enclosing trusted `Result` may likewise
have a different success payload, but its error type must exactly equal the
operand's error type. Lookalikes remain ordinary legal enums and receive no
fallibility sugar.

When a semantic operation must materialize one of those types, it emits a
closed, typed request for the exact toolchain module, not an import of the std
root and not a shape search. An identity check may compare the trusted producer
key without loading or specializing a producer merely to reject a lookalike.
The query evaluator performs no filesystem I/O: the host validates and appends
demanded source through a narrowly verified successor snapshot, preserving
source-manifest authority and then resuming ordinary import discovery only for
imports introduced by that module. The demand is handled before a body
transaction begins and publishes no semantic failure terminal. A missing,
unreadable, or malformed demanded trusted module is a deterministic toolchain
installation/integrity failure, not a user-language alternative, fabricated
import failure, or structural fallback. Manifest denial remains a hermetic
build-configuration failure. These well-known semantic dependencies are not
runtime reachability edges.

An artifact query publishes either a successful artifact, a deterministic
failure with a canonical diagnostic batch, or no terminal for cancellation.
Successful semantic, CFG, code-generation, and image artifacts contain no
warnings or diagnostics. Diagnostic identity, current source position,
allow/filter policy, and rendering are separate projections. The outer
one-shot adapter may collect diagnostic batches after observing requested
artifacts, but no diagnostic participates in an artifact key or fingerprint.

`Reachability(RootSetKey)` is one database-owned traversal over typed,
independently stamped body-reference terminals. Its root provider is an
explicit narrow input. The current policy that roots every destructor or scans
all nominal definitions is transitional and must be deleted when keyed drop
facts and drop plans become authoritative. Body-reference edge classes are
versioned and typed; new semantic edge kinds are added explicitly rather than
hidden in a generic catch-all.

Reachability publishes a proof, a deterministically ordered member set, and
individually stamped membership projections. Recursive call components are
ordinary graph cycles: body queries do not request callee bodies. Additions may
use monotone expansion; after an edge or root deletion, recomputation from the
roots is the correctness baseline. Unchanged membership remains green.
Removing a member from one root set releases only the departing `RootSetKey`
closure's membership pin and withdraws the diagnostics owned by that
root-set/member relation. The physical terminal remains retained while any
other live root lease or explicit retention owner still reaches it, and becomes
reclaimable only after its last owner releases it. The removed membership may
ultimately contribute to a symbol removal after code-generation-unit and
complete-image-plan comparison; membership itself is not a symbol delta.
Schedule order may affect work order but not results, membership, diagnostics,
or specialization-depth witnesses.

The query runtime, not a session-local worklist or a phase-local Rayon loop,
owns structured child and batch scheduling under the configured concurrency
budget. No query holds a database lock while executing a child. The serial
coordinator remains only until the database-owned traversal proves parity, and
is then deleted rather than retained as a fallback path.

### Type, drop, code generation, and image boundaries

Multiplicity is target-independent and keyed by type identity. Ordinary
non-linear structs are affine even when all fields are copyable; only an
explicitly copyable type is `Copy`. Linear containment is infectious. An array
is `Copy` exactly when its element is `Copy`; it is `Linear` exactly when its
length is nonzero and its element is linear. A zero-length array of a linear
element is non-copyable but has a vacuous runtime drop obligation. Pointer
multiplicity does not depend on the pointee.

Declared-destructor identity and signature are separate from the destructor
body. Drop facts and ordered drop plans are separate artifacts. Merely forming
a type does not root its destructor or drop glue; reached operations that need
drop semantics observe the plan. Drop glue is a synthetic body and then an
ordinary code-generation unit. Target-independent multiplicity and drop facts
remain separate from target-specific layout and call ABI.

CFGs and code-generation units are keyed per function or synthetic body.
Backend parallelism consumes those independent units; it is not a second
whole-program scheduling authority. A deterministic `ProgramImagePlan`
contains the complete current reached symbols, data, and relocations; it has no
predecessor-dependent delta or retained linker state. The first linker may
still build a fresh image, but it must consume this plan. A consumer or
separately keyed comparison projection can compare two complete plans and
derive canonical additions, changes, and removals. That seam permits a later
incremental linker to apply the derived delta without another semantic, CFG,
or code-generation refactor.

The compiler does not satisfy any phase by adding a query beside an aggregate
adapter that still owns the computation. Each phase includes a source-inventory
deletion guard naming the whole-program scan, aggregate builder, coordinator,
or peer scheduler that becomes illegal when the new keyed path is accepted.

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
| Retention leases | Force pressure eviction before and after rooted completion, cancellation, and supersession | Live root closures remain pinned; released/speculative terminals reclaim; accounting has no unowned pin; warm result/diagnostics equal fresh after eviction | A live closure evicts, a released pin remains, accounting leaks, or eviction changes observable output |
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
7. RUE-1091: conditional shared-base or narrow-epoch repair if RUE-1090's
   decision rule fires, including a rerun of the relevant RUE-1090 structural
   measurements and RUE-1092 prototype/adversarial sign-off on the repaired
   result; otherwise cancel it.
8. RUE-1092: vertical prototype and adversarial review.
9. RUE-1093: accept ADR-0066 only after RUE-1090 and RUE-1092, and RUE-1091
   when activated, complete; record the accepted contracts and replan the
   remaining ADR-0063 work.
10. RUE-1028: make typed body references, database-owned reachability, and
    structured runtime scheduling authoritative; delete the session-local
    reachability coordinator. RUE-1095, RUE-1099, and RUE-1111 are design
    prerequisites.
11. RUE-1029: publish per-type nominal content, reduced comptime values,
    multiplicity, drop facts and plans, layout, and call ABI; delete the
    whole-pool layout and destructor/glue scans. RUE-1095, RUE-1097, and
    RUE-1101 are design prerequisites.
12. RUE-1030: publish per-function and synthetic-body CFG artifacts; delete the
    aggregate CFG builder and its whole-semantic-output input.
13. RUE-1031: publish independent code-generation units for both targets and
    delete whole-program backend parallel loops as an ownership boundary.
14. RUE-1032: assemble a complete deterministic `ProgramImagePlan` and provide
    a separately keyed comparison that derives additions, changes, and
    removals; keep fresh linking as the complete plan's first consumer while
    preserving the incremental-link delta seam.
15. RUE-1033: delete the remaining aggregate adapters, peer schedulers, and
    compatibility routes, then restore every RUE-1083 performance check that
    was temporarily stubbed during the regression.

The specification amendment may be drafted and reviewed in parallel with
RUE-1085 through RUE-1087, but live specification and executable-case edits
land atomically with RUE-1089. RUE-1092 may run in parallel with RUE-1090 after
RUE-1089; acceptance awaits both required gates.
If RUE-1091 activates, its post-repair measurement and review evidence replaces
the corresponding pre-repair evidence for acceptance.

The RUE-1095, RUE-1097, RUE-1099, RUE-1101, and RUE-1111 decisions are accepted
as explicit prerequisites before their owning implementation phase begins.
They are not deferred cleanup: raw-input versus artifact fingerprints,
comptime-value dependencies, reachability deletion semantics, multiplicity and
drop ownership, and diagnostic terminals determine the query keys and
invalidation edges. Beginning implementation without those rulings would make
another cross-compiler rewrite likely.

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
- [ADR-0038: Error handling: sum types, Result/Option, and must-check via
  linearity](0038-error-handling-sum-types-result-must-check.md)
- [ADR-0063: Parallel demand-driven incremental compilation](0063-parallel-demand-driven-incremental-compilation.md)
- `docs/spec/src/04-expressions/14-comptime.md`
- `crates/rue-spec/cases/expressions/comptime.toml`
- `crates/rue-spec/cases/types/destructors.toml`
- `docs/benchmarks/rue-1086-caldera-baseline.json` (RUE-1086 raw Caldera
  provenance, reference-host configuration, and runner samples)
