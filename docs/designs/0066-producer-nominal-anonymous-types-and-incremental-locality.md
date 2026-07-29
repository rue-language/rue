---
id: 0066
title: "Producer-nominal anonymous types and incremental locality"
status: implemented
tags: [types, semantics, comptime, incremental, performance, parallelism]
feature-flag: null
created: 2026-07-21
accepted: 2026-07-28
implemented: 2026-07-28
spec-sections: ["4.13:35", "4.13:44", "4.14:8", "4.14:15", "4.14:21-25", "4.15:3-4"]
superseded-by:
amends: [0025, 0029, 0063]
---

# ADR-0066: Producer-nominal anonymous types and incremental locality

## Status

Implemented. RUE-1089 landed the producer-nominal language cut and its
specification changes before this record was ratified. RUE-1092 then completed
the query-native body-analysis cut. RUE-1093 reconciled the record with those
implemented decisions on 2026-07-28.

This ADR amends the anonymous-type semantics introduced by ADR-0025 and
ADR-0029, and it specializes ADR-0063's stable-identity and per-body-query
contracts for anonymous types. Those ADRs retain their recorded status.

The earlier draft also proposed acceptance gates and a peer replacement
implementation sequence. Those proposals are not part of the ratified
decision. The provider-native locality and retention boundary that actually
landed is retained below; ADR-0063 remains the implementation-plan authority.
RUE-1028 has landed phase 7, and RUE-1029 through RUE-1033 own the remaining
phases 8 through 12.

## Summary

Anonymous `struct` and `enum` declaration expressions are producer-nominal.
Their identity comes from the selected declaration expression under its static
enclosing comptime specialization, rather than from structural comparison of
their fields, variants, methods, or bodies.

The compiler represents that identity with a stable producer key and analyzes
each reached body through the query database's provider-native body path.
Anonymous-type content and trusted standard-library type demands are exact
dependencies of the body that observes them. Whole-program semantic state is
not an authority for a body query.

## Context

Rue originally treated anonymous types structurally. Equal shapes from
different producers could converge on one representative, so reaching an
unrelated producer could change which method or destructor body represented an
already-reached type. That made language behavior non-local and required
cross-producer equivalence and restart coordination.

ADR-0063 requires stable logical identities, independently keyed semantic
bodies, exact `BodyReferences`, and one canonical query graph. Anonymous types
therefore need a single producer-owned identity that does not depend on the set
or order of other declarations.

`Option` and `Result` add a related requirement. They are ordinary
comptime-generic library enums under ADR-0038. Fallible intrinsics produce an
exact trusted `Option`, while `?` recognizes exact trusted `Option` or `Result`
identity rather than accepting any same-shape enum.

## Decision

### 1. Anonymous types are producer-nominal

Every anonymous `struct` or `enum` declaration expression denotes a
producer-nominal type. The producer is the selected anonymous declaration
expression, not every function that returns `type`.

Identity consists of that producer under its static enclosing comptime
specialization. Declared comptime arguments and enclosing generic or comptime
specializations distinguish specializations. Repeated evaluation of the same
producer under the same canonical specialization denotes the same type.
Different producers, definition-relative anchors, or specializations denote
different types even when their fields, variants, methods, and bodies are
identical.

Rue has no comptime loop evaluation that repeatedly selects an anonymous
declaration within one specialization. A future comptime-loop feature must
specify any additional identity dimension and its incremental-stability
consequences.

#### Forwarding preserves identity

A function that returns an existing type value does not mint another type.
`fn Id(comptime T: type) -> type { T }` returns `T`'s identity, and aliases
likewise preserve identity.

Within one source revision, evaluations with the same identity agree on type
content. A source edit may change that content and invalidate exact consumers;
content is not a second equality rule.

#### Anonymous declarations are not type annotations

An anonymous type declaration expression is legal as a comptime value and as a
type-constructor result. It is not legal directly, or nested inside another
type, in a type annotation. A user binds the resulting type value or exposes it
through a type constructor before using it in an annotation.

This was an intentional hard semantic cut without a preview gate. Supporting a
preview would have required both incompatible identity systems and preserved
the cross-producer machinery that this decision removes.

The normative language rules and examples are in specification rules 4.14:8,
4.14:15, 4.14:21, 4.14:22, 4.14:23a, and 4.14:25.

### 2. Stable identity is distinct from semantic equality

The compiler's logical key contains the anonymous declaration kind, producer
definition or specialization, definition-relative structural anchor, and
canonical specialization arguments. This key is compared exactly.

A digest is collision-aware indexing and presentation metadata. It does not
decide language equality. Source positions, allocation order, evaluation
order, and the set of unrelated declarations are not identity inputs.

Captured or external comptime values that affect a type's content are exact
dependencies of that content. They do not become dynamically selected pieces
of the identity tuple.

### 3. Trusted `Option` and `Result` follow exact producer identity

The standard `Option` and `Result` remain ordinary library-defined anonymous
enums. The producer-nominal rule has these consequences for ADR-0038's
error-handling design:

- fallible intrinsics return an exact trusted standard `Option`
  specialization;
- `?` accepts only an exact specialization of the trusted `Option` or `Result`
  producer; and
- a user-defined same-shape enum is an ordinary, distinct enum and receives no
  trusted behavior.

The exact intrinsic-result rules are specification rules 4.13:35 and 4.13:44;
the exact `?` legality rules are 4.15:3 and 4.15:4. This is a consequence of
producer-nominal identity, not a new error-handling decision: ADR-0038 remains
the owner of `Option`, `Result`, and propagation semantics.

### 4. Body analysis is query-native and provider-owned

The canonical body transaction is keyed by a stable function instance and
semantic configuration. The query database owns its evaluator. The evaluator
obtains exact raw body, lookup, declaration, anonymous-producer, and trusted
toolchain facts through registered queries, and invokes rue-air's
provider-native body analyzer.

Each body analysis owns its mutable inference and compact type state. It
publishes a durable body result, diagnostics, and independently queryable
`BodyReferences`. A body-specific anonymous producer or trusted
standard-library demand is materialized only for the body that observes it.

Provider lookups record the exact positive, negative, ambiguous, visibility,
and import results the body observes. A published semantic root atomically
hands those exact terminal pins to the session-held
`PublishedRootLookupLease`; its successor replaces the prior set. Aborted,
canceled, and merely speculative attempts are not promoted. This retention
mechanism preserves the last published dependency set without making the lease
or its cache a semantic authority.

The production body evaluator does not call the whole-program
`analyze_body_query` adapter and does not receive `SharedDeclarationBase` or a
bound body epoch. Whole-program consumers compose the provider-queried body
artifacts; the retired whole-program body analyzer remains only as an explicit
test oracle, never as a peer production authority.

This is the locality boundary added to ADR-0063:

- one stable owner computes each producer-nominal identity;
- body facts are consumed through exact provider observations;
- unrelated declarations do not participate in an already-resolved anonymous
  type's identity;
- query result and diagnostic order are independent of scheduling and warm
  reuse; and
- warm and fresh sessions agree after edits.

Database-owned reachability landed under ADR-0063 phase 7 and RUE-1028.
Type/layout/ABI/drop queries, CFG queries, codegen units, image planning, and
compatibility cleanup remain ADR-0063 phases 8 through 12, tracked by RUE-1029
through RUE-1033. This ADR does not define a peer phase plan.

## Acceptance evidence

| Contract | Current authority |
| --- | --- |
| Different producers are distinct; the same producer and specialization are reused | Specification 4.14:8, 4.14:21, and `crates/rue-spec/cases/expressions/comptime.toml` |
| Methods and destructors remain producer-local | Specification 4.14:15 and `crates/rue-spec/cases/types/destructors.toml` |
| Forwarding preserves identity and anonymous declarations are rejected in annotations | Specification 4.14:22, 4.14:23a, 4.14:25, and the corresponding comptime specification cases |
| Stable identity is insensitive to unrelated edits, file-id allocation, and cold/warm execution | `crates/rue-compiler/src/producer_nominal_acceptance_tests.rs` |
| Fallible intrinsics return trusted `Option`; `?` recognizes only trusted `Option`/`Result` and rejects lookalikes | Specification 4.13:35, 4.13:44, and 4.15:3-4; `producer_nominal_acceptance_tests.rs`; the `result_try` execution case in `crates/rue-cli-tests/src/main.rs`; and `crates/rue-ui-tests/cases/diagnostics/result_try_errors.toml` |
| The production body transaction uses the provider-native analyzer and excludes the retired whole-context body path | `crates/rue-compiler/src/revisioned_query_database.rs` and `per_body_query_boundary_is_stable_independent_and_cache_free` in `api_inventory.rs` |
| Fixed reached bodies retain flat body work as unrelated declarations grow; unrelated declaration edits keep bodies green | `crates/rue-compiler/src/scaling_harness.rs` |
| Phase 7 reachability and the remaining incremental phases have one canonical owner | ADR-0063; RUE-1028 (landed) and RUE-1029 through RUE-1033 (remaining) |

## Consequences

### Positive

- Type equality and body ownership are deterministic and local.
- Same-shape, different-producer types cannot silently exchange method,
  destructor, or trusted error-handling behavior.
- Stable keys survive unrelated source edits and evaluation-order changes.
- Body analysis composes with ADR-0063's retained, parallel query graph without
  a whole-program semantic epoch as a peer authority.

### Costs

- Programs that relied on structural interchange between separately declared
  anonymous types must share one producer or add an explicit conversion.
- Producer identity and exact trusted-standard demands are durable compiler
  contracts that tests and future query phases must preserve.
- A future comptime-loop feature must define how repeated declaration selection
  affects identity.

## Rejected alternatives

- **Structural anonymous-type equality.** It requires global cross-producer
  comparison and makes body selection depend on unrelated declarations.
- **Stable representative selection.** A deterministic representative is still
  a non-local representative and retains invalidation and restart coordination.
- **Call-site nominal identity.** Repeated calls to the same constructor would
  create incompatible types and make generic library types unusable.
- **Treat every `type`-returning function as a producer.** Forwarding functions
  would mint identities despite selecting no declaration expression.
- **Recognize `Option` or `Result` by shape.** User lookalikes would acquire
  privileged intrinsic and `?` behavior.
- **Keep a complete semantic context as the body-query boundary.** That creates
  a second authority beside ADR-0063's independently keyed body graph.

## References

- ADR-0025: Compile-Time Execution
- ADR-0029: Anonymous Struct Methods
- ADR-0038: Error Handling
- ADR-0063: Parallel Demand-Driven Incremental Compilation
- RUE-1089: producer-nominal semantic cut
- RUE-1092: query-native body-analysis cut
- RUE-1093: ADR reconciliation
- RUE-1028: landed ADR-0063 phase 7 reachability
- RUE-1029 through RUE-1033: remaining ADR-0063 implementation phases
