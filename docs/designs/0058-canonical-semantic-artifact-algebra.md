---
id: 0058
title: Canonical semantic artifact algebra
status: accepted
tags: [architecture, compiler, incremental, semantics, validation]
feature-flag: null
created: 2026-07-17
accepted: 2026-07-17
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0024", "ADR-0050", "ADR-0053", "RUE-720", "RUE-771", "RUE-845", "RUE-849", "RUE-850", "RUE-851"]
---

# ADR-0058: Canonical semantic artifact algebra

## Status

Accepted under RUE-845 on 2026-07-17. RUE-849 through RUE-851 implement the
additive migration after RUE-720 and RUE-771. This is an internal compiler
architecture decision. It does not change Rue language semantics, the
specification, or a preview feature.

## Summary

Rue will have one request-independent semantic artifact algebra, owned by
`rue-air` and parameterized by definition and module identity. The algebra is
the value used for live body export, compiler retention, validation, and
re-import. With `StableDefinitionKey` and `ModuleId` as its identity parameters,
it is the durable representation; durability is a property of the complete
versioned artifact envelope, not a second instruction or type enum.

Compact live AIR remains separate because it has a genuinely different layout:
epoch-local types and symbols, owner-bound instruction/place references, and
packed payload stores. Exactly two semantic adapters remain: live AIR to the
canonical artifact and the canonical artifact to fresh live AIR. They perform
relocation and fail-closed validation. There is no canonical-to-durable copy.

`StableDefinitionKey`, `StableDefinitionKind`, and
`StableDefinitionNamespace` become the only durable declaration identity
taxonomy. Syntax candidates and live semantic bindings retain narrower
snapshot-local views, but their correspondence to the stable taxonomy is
declared once at the binding boundary. Body export receives already joined
stable keys and never invents an intermediate textual identity taxonomy.

## Context

RUE-720 established sound, measured ordinary-body and CFG reuse through the
canonical semantic query. Its deliberately fail-closed implementation crosses
several structurally mirrored values. That machinery is present in current
source, not historical compatibility code. It is safe, but each new semantic
form can require changes in several exhaustive translations which do not add a
new boundary or invariant.

### Current representations

The complete current inventory is:

| Concern | Current owner | Representation | Why it exists |
| --- | --- | --- | --- |
| Compact live body | `rue-air/src/inst.rs` | `AirInstData`, `AirInst`, `AirPlace`, typed payload ranges, `Type`, `Spur`, nominal IDs, `InstRef` | Hot analysis and downstream traversal inside one AIR epoch. |
| Live export DTO | `rue-air/src/semantic_body.rs` | `SemanticBodyInstData<K, M>`, `SemanticBody<K, M>`, patterns, projections, places, anchors, warnings, specialization identity | Structured, request-independent relocation boundary. |
| Export-time identity | `rue-air/src/semantic_body.rs` | `SemanticBodyDefinitionIdentity`, `SemanticBodyDefinitionKind`, `SemanticBodyModuleIdentity` | Lets AIR describe identities before the compiler joins them to its stable universe. |
| Importable type/value DTO | `rue-air/src/semantic_import.rs` | `SemanticImportType<K, M>`, `SemanticImportConstValue<K, M>`, nominal kind, import epoch | Structured type/value relocation into a fresh type pool and symbol universe. |
| Retained body | `rue-compiler/src/durable_body.rs` | `DurableAirInstData`, `DurableAirInst`, `DurablePlace`, `DurablePattern`, `DurableProjection`, `DurableBodyAnchor`, `DurableSpecializationIdentity` | Compiler-owned last-successful body candidate. Its structure mirrors the export DTO. |
| Retained type/value | `rue-compiler/src/durable_semantics.rs` | `DurableType`, `DurableConstValue` | Stable-key/module form of the importable type/value DTO. |
| Versioned envelopes | `rue-compiler/src/durable_body.rs`, `durable_semantics.rs` | ordinary-body, specialized-body, and declaration payloads plus schema versions and exact fingerprints | Candidate authorization, compatibility, completeness, and publication. |
| Parsed candidate identity | `rue-compiler/src/definition_snapshot.rs` | `DefinitionKind`, `DefinitionNamespace`, `DefinitionNameKey`, snapshot-local occurrence ID | Presemantic ambiguity and duplicate handling. It intentionally cannot distinguish every semantic kind. |
| Live binding identity | `rue-air/src/sema/binding_manifest.rs` | `SemanticBindingKind`, `SemanticBindingNamespace`, textual owner/name and request-local span | Successful semantic binding result before compiler issuance. |
| Stable identity | `rue-compiler/src/bound_definitions.rs` | `StableDefinitionKind`, `StableDefinitionNamespace`, `StableDefinitionKey`, issuer-scoped `BoundDefinitionId` | Cross-revision identity and snapshot-isolated live authorization. |

`SemanticBodyInstData` and `DurableAirInstData` are variant-for-variant mirrors
apart from the deliberate absence of unresolved `CallGeneric` in durable data.
Their child records mirror in the same way. `SemanticImportType` and
`DurableType`, and `SemanticImportConstValue` and `DurableConstValue`, likewise
have the same recursive shape after substituting stable identity parameters.

### Current joins and translations

Every current join is also part of the design inventory:

1. `DefinitionSnapshot` records syntax candidates. Binding joins semantic
   manifest entries to their current occurrences and issues
   `BoundDefinitionId`s only after successful binding.
2. `bound_definitions.rs` hand-maps `SemanticBindingKind` and namespace to the
   stable kind/namespace and separately maps back to syntax kinds for occurrence
   validation.
3. AIR body export in `sema/semantic_body_export.rs` decodes packed live AIR,
   exports live types, symbols, nominals, modules, strings, spans, warnings,
   instructions, places, and specializations into the structured DTO.
4. `canonical_semantic.rs` builds textual-identity-to-stable-key maps, including
   repeated exhaustive stable-kind to export-kind conversions, then maps the
   exported body's generic keys.
5. `durable_body.rs` converts that stable-key DTO into the parallel durable body
   enum and converts every nested `SemanticImportType` and const value into the
   parallel durable type/value enums. Unsupported forms reject the candidate.
6. Candidate selection joins owner, exact input fingerprints, target/features,
   direct dependency records, completeness evidence, and artifact schema
   version against the current stable-definition universe.
7. Durable projection converts the retained body and nested durable types back
   into `SemanticBody<StableDefinitionKey, ModuleId>` and validates all record
   references, source order, anchors, strings, kinds, signatures, and dependency
   endpoints before installation.
8. `SemanticImportEpoch` resolves stable definitions and modules, reconstructs
   fresh pool-owned `Type`, nominal IDs, symbols, and strings, and atomically
   builds an owner-bound live AIR body. Failure falls through to ordinary
   analysis.
9. Declaration reuse independently converts between `SemanticImportType` and
   `DurableType`, then projects and joins stable declaration shells before
   constructing a fresh semantic import epoch.

The live/structured boundary and the structured/live boundary do real work.
The DTO/durable/DTO translations do not: they primarily rename the same shape.
The textual export identity is also avoidable once stable issuance is already
available to the canonical semantic request.

## Decision

### One parameterized artifact algebra

`rue-air` owns a public, request-independent algebra parameterized by key and
module identity. Existing `SemanticBody`, `SemanticBodyInstData`,
`SemanticImportType`, `SemanticImportConstValue`, specialization, pattern,
place, projection, anchor, warning, and call-argument records evolve into that
algebra rather than being replaced by another peer model.

The durable specialization is conceptually:

```rust
type DurableType = SemanticImportType<StableDefinitionKey, ModuleId>;
type DurableConstValue = SemanticImportConstValue<StableDefinitionKey, ModuleId>;
type DurableBody = SemanticBody<StableDefinitionKey, ModuleId>;
type DurableSpecialization =
    SemanticSpecializationIdentity<StableDefinitionKey, ModuleId>;
```

Names may change during migration. The invariant is that these are aliases or
thin newtypes over one recursive algebra, never parallel recursive enums with
element-by-element conversions. Thin newtypes are permitted only to express an
artifact state such as `Validated<T>` or to prevent confusing ordinary and
specialized envelopes. A newtype cannot repeat the inner variants.

The canonical body uses record-local dense `u32` references. Those references
are relative to the body artifact and are checked before indexing. They are not
live `InstRef`, `PlaceId`, AIR payload offsets, or pool indices. Strings are
owned values or indices into a record-local owned string table. Types contain
only structural forms, stable definition keys, logical module identities, and
declaration-scoped generic parameter numbers.

The compiler owns the complete retained envelope. It contains schema identity,
owner, exact input/dependency fingerprints, target and preview inputs where
applicable, completeness evidence, and the canonical algebra value. A bare
canonical body is transport data, not an authorized cache hit.

### Compact live AIR remains a separate layout

`AirInstData` remains distinct. Its compact payload schemas, owner-bound
references, epoch-local `Type`, `Spur`, and nominal IDs are appropriate inside
one successful semantic attempt and deliberately invalid across epochs.
Aliasing the live representation to the retained algebra would either serialize
request-local handles or inflate every live instruction with owned stable data.

There are exactly two body-shape translations:

```text
live AIR --export/relocate--> canonical artifact
canonical artifact --validate/relocate/build--> fresh live AIR
```

Both are exhaustive because the layouts and identity domains genuinely differ.
Export may reject a form whose stable meaning cannot be represented. Import
validates before committing any owner state. Removing the middle durable copy
means a semantic instruction is described once in the canonical algebra and
handled only at these two necessary layout boundaries.

The canonical instruction declarations and their structural traversal metadata
will live together. The implementation may use a local declarative macro when
that removes repeated body-wide visitors for references, keys, types, strings,
and validation. It must not introduce a build-time generator, procedural macro,
serialized tag ABI, or shared erased IR crate. The endpoint is source-reviewable
Rust in `rue-air`: adding a canonical instruction cannot require a matching enum
or rename-only match in `rue-compiler`.

### Stable identity is canonical only after binding

The three identity stages are not all mirrors:

- `DefinitionKind`/`DefinitionNamespace` remain syntax-candidate categories.
  `Const` cannot yet be split into a value constant versus module binding, and
  methods are not top-level parsed candidates. Snapshot occurrence IDs remain
  explicitly snapshot-local.
- the live binding manifest may retain its request-local spans and textual
  owner data, but its semantic kind and namespace use the stable taxonomy or a
  declarative projection of that taxonomy. The compiler owns the one mapping
  from the narrower parsed category to the final stable category.
- `StableDefinitionKind`, `StableDefinitionNamespace`, and
  `StableDefinitionKey` are the only categories stored in reusable artifacts.

Body export must receive resolver callbacks or a read-only table which maps
live function, nominal, and module handles directly to `StableDefinitionKey`
and `ModuleId`. `SemanticBodyDefinitionKind`,
`SemanticBodyDefinitionIdentity`, and `SemanticBodyModuleIdentity` are removed.
The exporter may not mint or infer stable identities. The current bound set is
the sole authority, and missing, ambiguous, wrong-kind, or foreign-issuer
resolution is an export failure.

`BoundDefinitionId` remains issuer-scoped for snapshot isolation. Only its
owned `StableDefinitionKey` crosses into a retained artifact. A retained key is
never sufficient to authorize a live entity by itself; import joins it against
the exact current issuer's bound set and obtains the fresh live ID.

### Validation and fail-closed publication

Validation is one canonical traversal over the algebra. It checks:

- instruction and place references are in range and obey ordering rules;
- source-order vectors are permutations with the expected arity;
- string references address the record-local table;
- anchors are ordered, bounded, and relocate within the owner's current body;
- stable keys exist exactly once in the current bound universe and have the
  required kind and namespace;
- nominal variants/fields, callable signatures, generic parameters, parameter
  modes, slots, and specialization arguments are structurally valid;
- all type and const-value recursion is finite and supported;
- warnings and dependency/completeness evidence satisfy the envelope policy;
  and
- every envelope fingerprint, target/feature input, and version matches.

Validation returns an owning typestate such as `ValidatedCanonicalBody`; import
consumes that value. An unchecked canonical value is never installed. Validation
and relocation may share a traversal, but mutation is staged and publication is
atomic. Any unknown, malformed, unsupported, stale, or unjoinable form records
the measured fallback reason and runs the existing ordinary path. Cache data is
never a compiler error for otherwise valid source.

### Version compatibility and invalidation

Each retained artifact envelope carries a schema identifier with a major and
minor version. The initial canonical algebra is major 1, minor 0. Versions are
part of the artifact key and are checked before decoding or joining.

- A major version changes when a variant is added or removed, a field changes
  meaning or requiredness, reference/index semantics change, identity or
  relocation rules change, validation becomes stricter in a way old data may
  violate, or canonical encoding changes incompatibly. Readers reject every
  other major version and rebuild.
- A minor version changes only for an additive envelope field or metadata whose
  absence has a specified conservative default. A reader accepts at most the
  exact current minor and explicitly listed older minors. There is no implicit
  best-effort compatibility.
- Compiler builds include an implementation/schema epoch in persistent cache
  namespaces. Until a persistent codec is designed, retained artifacts remain
  in-process typed values and version checks still exercise candidate
  invalidation across session revisions.
- Ordinary bodies, specialized bodies, declaration semantics, and CFGs keep
  separate envelope version constants because their authorization inputs differ.
  They all reference the same canonical semantic algebra version rather than
  versioning copies of its instruction/type variants.
- Changing target, preview features, owner input fingerprint, direct dependency
  fingerprint, stable-key join, warning/completeness policy, or required
  relocation context invalidates the affected candidate even when its algebra
  version is unchanged.

No migration code attempts to deserialize unknown tags, preserve raw enum
discriminants, or reinterpret old request-local indices. Incompatibility is a
normal measured cache miss.

### Serialization boundary

This ADR does not add persistent serialization. A future codec must be a thin
encoding of a validated canonical artifact envelope and must use explicit tags,
lengths, bounds, and version headers. Rust enum discriminants and memory layout
are not a file format.

The codec may serialize only owned strings, logical module identities, stable
definition keys, structural type/value forms, record-local references, anchors,
fingerprints, and policy/version metadata. It must never serialize `Spur`,
`FileId`, `Span`, `Type`, nominal IDs, `InstRef`, place IDs, AIR payload ranges,
type/string pool offsets, `BoundDefinitionId` issuer pointers, or iteration
positions in request-local tables.

### Measured fallback remains observable

The existing work counters remain semantically meaningful. The migration keeps
attempt, version rejection, stable-join rejection, structural validation
rejection, unsupported export/import, successful reuse, and ordinary fallback
counts. Removing rename-only conversion work may remove those counters only
after benchmarks and differential tests no longer consume them. Cold and reused
compilation must continue to produce identical public semantics, diagnostics,
CFGs, and emitted bytes.

## Implementation Phases

- [ ] **Canonical body storage — RUE-849.** Retain
  `SemanticBody<StableDefinitionKey, ModuleId>` and its specialization identity
  directly inside versioned body envelopes; delete `DurableAirInstData` and all
  mirrored child records and rename-only conversions. Centralize structural
  validation and preserve differential/reuse benchmarks.
- [ ] **Canonical identity taxonomy — RUE-850.** Make stable kind/namespace the
  only durable taxonomy, declare the parsed-to-stable binding join once, pass
  stable-key/module resolvers into body export, and remove textual body identity
  kinds and their repeated conversions.
- [ ] **Canonical type/value projection — RUE-851.** Replace recursive
  `DurableType`/`DurableConstValue` mirrors with aliases or thin state newtypes
  over `SemanticImportType<StableDefinitionKey, ModuleId>` and its const value;
  centralize import/export/validation traversal for declarations, bodies, and
  specializations.
- [ ] **Guardrail and compatibility completion — RUE-851.** Add source/API
  inventories forbidding the removed peer enums and request-local carriers,
  malformed artifact tests, explicit version rejection tests, and a test-only
  schema visitor proving every canonical variant participates in validation.

The phases are additive until each replacement is proven. No phase introduces
a second frontend, semantic query, or body analyzer. Temporary adapters are
deleted in the phase which makes them redundant, not retained as compatibility
APIs.

## Consequences

### Positive

- Live export, retained bodies, validation, and re-import share one semantic
  vocabulary without forcing compact live AIR to become an owned wire format.
- Adding an instruction, type form, const value, or stable identity does not
  require an unrelated compiler-owned mirror and two rename-only translations.
- Stable-key joins happen at one authority boundary, preserving snapshot
  isolation and making wrong-kind failures easier to audit.
- Schema compatibility, cache invalidation, unsupported forms, and future
  serialization constraints are explicit and fail closed.
- Existing measured incremental fallback and cold-versus-reused equivalence
  remain the proof boundary.

### Negative

- The canonical algebra becomes a deliberately stable internal contract, so
  changes require version and compatibility review even before persistence.
- `rue-air` exposes generic transport records whose durable specialization is
  chosen by `rue-compiler`; API guardrails are required to prevent consumers
  from treating unchecked transport data as installed AIR.
- Live-to-canonical and canonical-to-live exhaustive adapters remain. They are
  necessary because compact live AIR and durable artifacts have different
  layouts and identity lifetimes.
- The staged migration temporarily carries old and new representations, so each
  phase must delete its superseded conversion before completion.

## Rejected Alternatives

### Serialize compact live AIR

Rejected. Live AIR contains epoch-local handles, owner-bound references, and
packed-store positions. Making those stable would leak request-local interners
or turn the hot representation into a durable object graph.

### Keep separate live-export and durable enums, generated from one schema

Rejected as the endpoint. Generation would prevent variant drift but preserve
the unnecessary copy and invite layout divergence. Declarative definitions are
useful for visitors over one canonical enum, not for manufacturing peer enums.

### Move the algebra into a new shared crate

Rejected for now. `rue-air` already owns the import/export semantics and the
compiler already depends on it. A new crate would add an ownership boundary
without breaking a dependency cycle or establishing a more canonical owner.

### Use unversioned Rust typed values until persistence arrives

Rejected. In-process retention already crosses source revisions, and explicit
version rejection is part of the fail-closed contract. Deferring versions would
make the first persistent codec define policy accidentally.

## References

- [ADR-0024: Canonical Type Handle and Intern Pool](0024-type-intern-pool.md)
- [ADR-0050: Stable semantic dependency manifests](0050-semantic-dependency-manifest.md)
- [ADR-0053: Typed CompilerSession query state](0053-typed-compiler-query-state.md)
- [Body analysis and CFG incrementality audit](../notes/body-analysis-cfg-incrementality-audit.md)
