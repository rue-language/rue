---
id: 0024
title: Canonical Type Handle and Intern Pool
status: accepted
tags: [architecture, type-system, performance, parallelization]
feature-flag: null
created: 2026-01-02
accepted: 2026-07-16
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-766", "RUE-835", "RUE-836", "RUE-837", "RUE-838"]
---

# ADR-0024: Canonical Type Handle and Intern Pool

## Status

Accepted under RUE-766 on 2026-07-16. The representation convergence is not
implemented until RUE-835 through RUE-838 land. This is an internal compiler
architecture decision with no language-semantics, specification, or preview
feature change.

## Summary

`Type` is Rue's single authoritative compact live type handle at semantic and
runtime compiler phase boundaries within one semantic epoch. `TypeKind` is its
checked decoded view for pattern matching. The intern pool owns definitions,
structural-type canonicalization, and private typed storage identities; it does
not expose a second handle universe that independently encodes primitives and
composite indices.

The canonical `Type` encoding must distinguish runtime types, comptime-only
types, module values, never, and error recovery. Malformed or reserved bit
patterns are explicitly rejected by checked decoding. Structural interning
consumes and returns `Type`, and pool entries retain enough kind information to
validate that a handle is used with the correct definition category and entry
state in its owner-provided pool.

Durable semantic types remain a separate stable projection and import schema.
They deliberately do not reuse the request-local live handle encoding.

## Context

At acceptance time, the source has most of the intended architecture:

- `Type` in `crates/rue-air/src/types.rs` is a compact `u32` handle. It directly
  encodes primitive, composite, module, comptime, never, and error categories.
- `TypeKind` is the decoded, pattern-matchable view returned by checked decoding
  APIs.
- AIR and the surrounding compiler phases overwhelmingly exchange `Type`.
- `TypeInternPool` owns nominal definitions and canonical structural array and
  pointer entries. RUE-659 and RUE-660 established
  `FrozenTypeInternPool` as the backend-facing read-only projection after
  semantic construction finishes.
- `DurableType` and the semantic import/export schemas represent stable,
  request-independent types for reuse and invalidation.

One transitional representation remains: `InternedType` is a second public
`u32` type universe in `crates/rue-air/src/intern_pool.rs`. It has a different
primitive table from `Type`, omits comptime and module categories, and maps every
composite to an offset pool index. Recovering the composite category therefore
requires consulting pool storage. Public conversions and pool APIs allow both
representations to persist.

This duplication creates architectural ambiguity:

- a phase API can accidentally choose either handle without expressing a
  different ownership or lifetime boundary;
- primitive values can have different raw meanings depending on the wrapper;
- a bare composite index does not prove whether its entry is a struct, enum,
  array, or pointer;
- comptime-only, module, error, and malformed encodings cannot be described
  uniformly; and
- tests can validate each representation independently while missing divergence
  between them.

The compiler needs one live type identity, not a conversion protocol between
two request-local identities.

## Decision

### `Type` is the sole live compiler type handle

Every semantic and runtime compiler phase boundary uses `Type`. This includes
AIR, semantic definitions, structural interning, CFG construction, layout
queries, code generation, and compiler-session artifacts that remain within one
semantic epoch.

`Type` remains compact, copyable, hashable, and equality-comparable without
dereferencing the pool. Its raw representation is an implementation detail.
Callers construct values through named constants, typed constructors, and pool
APIs rather than synthesizing raw encodings.

No second public type may independently represent both primitives and composite
pool entries. Opaque category-specific IDs may remain public where `TypeKind`
or a category-specific API needs them. They are not peer representations of the
complete type universe. Raw pool positions, unchecked construction from those
positions, and unchecked extraction of positions from live handles remain
private to the pool implementation.

### `TypeKind` is the checked decoded view

`TypeKind` remains the exhaustive, pattern-matchable view of a `Type`. Decoding
must preserve all semantically meaningful categories:

- runtime primitives and composite types;
- comptime-only types;
- module values;
- never and error-recovery types.

`Type::try_kind` establishes **encoding validity** without consulting a pool. It
validates the tag and payload shape and returns no kind for malformed values,
overflowed payloads, and bit patterns reserved for future encoding categories.
An encoding-valid composite is not thereby valid in any particular pool or
semantic epoch.

Potentially untrusted or reconstructed raw values cross this checked decoding
API. Invariant-proven internal paths may use a convenience API that treats
invalid encoding as a compiler bug, but invalid bits must never silently decode
as a valid type. Encoding-reserved bit ranges are unrelated to the nominal
reservation lifecycle described below.

The encoding layout is centralized in one implementation. Primitive constants,
tags, masks, limits, construction, and decoding may not be repeated in the
intern pool or in consumers.

### The pool owns typed storage identity, not a second type universe

The intern pool stores one kind-tagged entry for every pool-backed type. Its
private storage may use typed pool indices or entry IDs for efficient lookup,
but a raw position alone is not sufficient type metadata. Looking up a struct
as an array, an enum as a pointer, or any out-of-range entry must fail through a
checked boundary or be reported as an internal invariant violation on a path
that has already established the kind.

Pool-aware validation establishes that an encoding-valid composite is **valid
in a pool/epoch** relative to its authoritative owner. It checks that the entry
is in range in the provided pool, has the encoded category, is in a state legal
for the requested operation, and contains representable children. APIs that
receive imported, reconstructed, or otherwise unproven values are fallible. A
bare `TypeKind` result never substitutes for these checks.

A naked compact `Type` cannot prove which pool produced it. Live phase
artifacts therefore pair their `Type` values with one authoritative pool as an
owner and boundary invariant. Pool lookup can validate the bits against that
provided pool, but it cannot distinguish foreign bits that coincidentally name
the same range and category. APIs must not carry a bare live `Type` across
semantic epochs. Explicit cross-epoch transfer uses durable export/import (or
an explicitly stamped boundary wrapper if one independently exists); M5 does
not introduce a branded replacement handle.

Nominal types retain nominal identity. Structural types are canonicalized by
their structural key. Those keys contain canonical `Type` children, so equality
and deduplication do not pass through another primitive/composite encoding.

Accordingly, structural pool APIs consume and return `Type`:

```text
intern_array(element: Type, len) -> Type
intern_ptr_const(pointee: Type) -> Type
intern_ptr_mut(pointee: Type) -> Type
```

The exact Rust spelling may differ, but callers do not convert through
`InternedType` before or after interning.

### Runtime structural children are validated

Arrays and raw pointers are runtime structural types. The pool applies these
representation-level rules before inserting or locating a structural entry:

| Child category | Array/pointer child |
| --- | --- |
| Runtime primitive | Allowed |
| Complete nominal or structural type in the owner-provided pool | Allowed |
| Declared nominal shell in the owner-provided pool | Allowed so recursive type graphs can be formed |
| `Never` | Allowed; source-position legality is a semantic check outside the interner |
| `Module` | Rejected |
| `ComptimeType` | Rejected |
| `Error` | Allowed as a canonical recovery child so sema can continue |
| Private anonymous reservation | Rejected |
| Malformed, wrong-kind, or out-of-range value | Rejected |

The interner decides representability, not full language legality. In
particular, semantic analysis decides whether `Never` is permitted in a given
source position.

A structural graph containing `Error` is recovery-only. Error results may
retain it for diagnostics, and semantic output may still freeze on an error
path. Successful durable export, layout, backend access, and compilation reject
any graph containing `Error`. Freeze itself does not reject `Error`.

Checked pool and durable-import APIs report failure rather than panicking on
rejected children. Durable imports fail closed if any recursive child cannot
become a representable live type in the target owner-provided pool.

### Nominal construction has explicit entry states

Nominal construction has three relevant pool-entry states:

1. **Reserved.** A private anonymous-construction token and entry. It is not
   issued as a live `Type` and is not legal in structural keys or type graphs.
2. **Declared.** A named nominal shell with canonical identity but no completed
   definition. It may be issued as `Type` and referenced by structural keys and
   type graphs. This permits legal recursive declarations such as pointers to a
   nominal whose fields are still being gathered.
3. **Complete.** A nominal with its finished definition.

Reservation is not a `Type` bit category and not a `TypeKind` variant. Declared
and completed nominals retain the same slot identity. Definition and layout
reads distinguish their pool-entry state rather than treating a shell as an
empty definition.

The legal transitions are `Reserved -> Complete` for anonymous construction and
`Declared -> Complete` for ordinary named declarations. Each transition happens
at most once. A completed entry can never be overwritten by another completion.
Duplicate-name and wrong-kind checks apply to both transitions.

Structural interning and type-graph construction may read the identity and
category of a declared shell. Definition, layout, durable export, backend, and
successful-compilation reads require completion. Reserved entries reject all
ordinary reads. Freeze rejects every remaining reserved or declared entry so
backend consumers receive a complete nominal universe.

RUE-835 implements the three entry states. RUE-836 implements the two transition
APIs and operation-specific reads. RUE-837 tests legal recursion, both
single-completion transitions, completed-entry overwrite rejection,
operation-specific shell access, reserved-entry rejection, and freeze rejection
of either incomplete state.

### Mutation and read ownership stay phase-scoped

Semantic construction owns creation, registration, interning, and completion
of live types. Once the semantic type universe is complete,
`FrozenTypeInternPool` remains the shared read authority for CFG and backend
consumers. Consolidating type handles does not create a peer mutation path in
later phases.

RUE-659 and RUE-660 already established this ownership and frozen-read
boundary. This decision preserves it rather than reopening it.

### Durable types are an external stable schema

`DurableType` and semantic import/export types remain owned, stable projections.
They use logical module and definition identities plus recursive structural
data; they do not expose or persist `Type` bits, pool positions, nominal IDs,
or request-local interner values.

Import validates a durable value and constructs or locates the corresponding
live `Type` in the current semantic epoch through fallible pool-aware APIs.
Export validates both encoding validity and validity in the source pool/epoch
before projecting into the supported durable algebra. Declared shells,
recovery-only `Error` graphs, unsupported categories, and invalid structural
states fail closed. The durable schema can evolve independently of the live
encoding.

## Required invariants

The implementation is complete only while both validation contracts hold.

Encoding contract:

1. `Type` is the only public primitive-or-composite live type handle.
2. Every encoding-valid `Type` has exactly one `TypeKind`; malformed and
   encoding-reserved values have none.
3. Construction and decoding share one canonical encoding definition.
4. Runtime, comptime-only, module, never, and error categories cannot be
   accidentally conflated.

Pool/owner contract:

5. Every live phase artifact pairs its `Type` values with one authoritative
   pool; naked bits are not a cross-epoch transfer format.
6. Pool-aware reads validate range, encoded category, entry state, and
   operation-specific child requirements in the owner-provided pool.
7. Structural interning accepts runtime types, `Never`, recovery `Error`, and
   declared nominal shells; it rejects module, comptime, reserved, malformed,
   wrong-kind, and out-of-range children.
8. Reserved anonymous entries are never issued as `Type`. Declared nominal
   shells may be issued and referenced recursively, but definition/layout/
   durable/backend reads require completion.
9. Each reserved or declared entry completes at most once, completed entries
   cannot be overwritten, and freeze rejects either incomplete state.
10. Recovery-only `Error` graphs may survive error output and freeze but cannot
    enter successful durable output, layout, backend work, or compilation.
11. Durable type identity contains no live handle encoding or pool index.
12. Phase APIs do not require bidirectional `Type`/`InternedType` conversion.

## Implementation sequence

The M5 work proceeds in dependency order:

1. **RUE-835 — centralize the canonical encoding and entry states.** Define one
   source of truth for tags, primitives, malformed and encoding-reserved
   patterns, checked construction, and decoding. Make pool entries retain
   explicit kind metadata, and represent reserved anonymous entries, declared
   nominal shells, and completed definitions distinctly.
2. **RUE-836 — migrate pool and phase APIs.** Change structural interning,
   lookup, keys, and consumers to accept and return `Type`. Add the
   `Reserved -> Complete` and `Declared -> Complete` transitions,
   operation-specific entry-state checks, the exact structural representability
   rules, and fallible durable-import validation. Preserve the owner/pool
   boundary without introducing a branded replacement handle. Use raw pool
   positions only inside the pool; retain opaque category-specific IDs where
   the typed public API needs them.
3. **RUE-837 — prove the representation.** Add round-trip and uniqueness
   properties, malformed/corrupt encoding tests, the exact child-representability
   matrix, recursive declared-shell coverage, both single-completion
   transitions, overwrite rejection, operation-specific incomplete-state
   failures, recovery-`Error` success-boundary rejection, structural
   deduplication, durable cross-epoch round trips, and concurrent interning
   coverage.
4. **RUE-838 — remove the transitional universe.** Delete `InternedType`,
   bidirectional conversions, duplicate primitive tables, migration
   scaffolding, and exports that expose raw pool implementation details. Update
   this ADR's context, checklist, status, and generated index to describe the
   completed architecture.

This is a sequential dependency chain, not parallel work. Linear relations must
record `RUE-836 blockedBy RUE-835` and `RUE-837 blockedBy RUE-836`. The migrated
API supplies the final surface for RUE-837, and RUE-838 removes compatibility
code only after those properties pass.

## Ownership boundaries

| Concern | Authority |
| --- | --- |
| Live compiler type identity and decoding | `Type` and `TypeKind` |
| Composite definitions and structural canonicalization | `TypeInternPool` |
| Backend-facing immutable type metadata | `FrozenTypeInternPool` |
| Raw pool positions and unchecked construction/extraction | Private pool implementation |
| Opaque category-specific IDs used by `TypeKind` or typed category APIs | Public only at those typed boundaries |
| Stable cross-revision type identity | `DurableType` and import/export schemas |
| Orchestration and artifact lifetime | `CompilerSession` |

No consumer may reproduce a responsibility from another row merely for
presentation, compatibility, or convenience.

## Non-goals

This decision does not:

- change Rue language type semantics or syntax;
- prescribe persistent serialization of the live `Type` encoding;
- make live `Type` values stable across compiler requests or pool epochs;
- replace `DurableType`, stable definition keys, or semantic import/export
  schemas;
- redesign the semantic mutation ownership or frozen-pool sharing boundary
  established by RUE-659 and RUE-660;
- add new type categories, generic semantics, layout rules, or codegen behavior;
  or
- require compiler source changes in RUE-766 itself.

## Consequences

### Benefits

- Phase signatures communicate one type identity consistently.
- O(1) live type equality is preserved without parallel primitive tables.
- Structural interning cannot erase composite kind information.
- Checked decoding and corruption tests cover the same representation used by
  production phases.
- The pool's public surface becomes smaller and its storage mechanics remain
  replaceable.
- Durable incremental artifacts remain independent of request-local allocation
  and representation choices.

### Costs and risks

- Migrating keys and APIs touches semantic and pool code broadly even though
  language behavior is unchanged.
- A centralized encoding change can have wide impact; property tests and
  corruption tests must land before compatibility scaffolding is deleted.
- Compact encodings have finite tag and payload space. Constructors must reject
  overflow rather than truncate IDs into a different valid type.
- Concurrent structural interning must preserve one canonical result under
  contention.
- Explicit private pool-entry reservation state may reveal construction paths
  that currently read placeholder definitions. Those paths must establish
  completion rather than weakening the invariant.

## Completion criteria

ADR-0024 returns to `implemented` only when:

- [ ] RUE-835, RUE-836, RUE-837, and RUE-838 are merged.
- [ ] Public compiler phase APIs use `Type` for live type values.
- [ ] `InternedType` and its public conversions and exports no longer exist.
- [ ] Primitive, tag, malformed/encoding-reserved pattern, construction-limit,
      and decoding logic has one authoritative implementation.
- [ ] Encoding tests distinguish encoding-valid values from malformed or
      encoding-reserved values without requiring a pool.
- [ ] Live phase artifacts retain their authoritative pool, and no API treats
      naked `Type` bits as a cross-epoch transfer format.
- [ ] Pool-aware reads check range, stored category, entry state, and
      operation-specific child requirements in the owner-provided pool without
      claiming to detect coincidentally equal foreign bits.
- [ ] Reserved anonymous entries are private and unissued as `Type`; declared
      nominal shells may be issued and used in recursive structural keys/type
      graphs.
- [ ] `Reserved -> Complete` and `Declared -> Complete` preserve slot identity,
      happen at most once, reject completed-entry overwrite, and enforce
      duplicate-name and wrong-kind checks.
- [ ] Definition, layout, durable, backend, and successful-compilation reads
      reject declared shells; ordinary reads reject reserved entries; freeze
      rejects either incomplete state.
- [ ] Structural interning consumes and returns `Type`, enforces the child
      legality matrix, and remains deterministic under concurrency.
- [ ] Property and corruption tests cover encoding validity, pool/epoch
      validity under an authoritative owner, all encoding categories, every
      structural child category, recursive declared shells, and malformed,
      wrong-kind, out-of-range, reserved, and declared cases.
- [ ] Recovery structural types containing `Error` are canonical for diagnostics
      and may survive freeze on error paths, but successful durable export,
      layout, backend work, and compilation reject them.
- [ ] A source and public-API inventory proves durable types contain no live
      bits, pool indices, nominal IDs, or unchecked conversion path.
- [ ] Durable projection/import tests perform successful cross-epoch round
      trips and prove malformed or illegal imports fail closed.
- [ ] The focused type/pool tests and Rue's required compiler validation suites
      pass.
- [ ] RUE-838 updates this ADR's acceptance-time context, checklist, status, and
      generated README index to record the verified completed architecture.

## References

- [ADR-0050: Stable semantic dependency manifests](0050-semantic-dependency-manifest.md)
- [ADR-0053: Typed CompilerSession query state](0053-typed-compiler-query-state.md)
