---
id: 0055
title: Typed compiler-runtime ABI manifest
status: implemented
tags: [architecture, compiler, runtime, abi, codegen]
feature-flag: null
created: 2026-07-16
accepted: 2026-07-16
implemented: 2026-07-16
spec-sections: []
superseded-by:
relates: ["RUE-355", "RUE-629", "RUE-738", "RUE-783", "RUE-826", "RUE-827", "RUE-828", "RUE-829", "RUE-830", "RUE-831", "RUE-832", "RUE-833", "RUE-834"]
---

# ADR-0055: Typed compiler-runtime ABI manifest

## Status

Implemented by the M4 typed-runtime-ABI milestone on 2026-07-16. The canonical
manifest, typed compiler identities, shared call planning, runtime conformance,
strict embedded-archive validation, and production-source inventory guard are
all enforced in the current tree.

## Summary

Rue defines the compiler/runtime boundary once in a dependency-light
`rue-runtime-abi` crate. An exhaustive `RuntimeHelperId` identifies every
compiler-callable runtime helper. A typed manifest entry provides its
exported symbol, ordered parameters, explicit out-pointer or scalar return
shape, safety contract, return behavior, target availability, and calling
convention.

Compiler phases carry helper IDs and consume manifest signatures. Raw
symbol strings are produced only at display, object, archive, and link
boundaries. Runtime exports prove their Rust `extern "C"` types against
the same manifest, and embedded archives are checked against its required
exports before compiler publication.

Entry points, compiler-built memory routines, runtime-private functions,
platform signal shims, and compiler-generated Rue symbols are not callable
runtime helpers. They remain explicit, separately validated export classes.

## Context

Before this decision was implemented, the compiler/runtime contract was
distributed across independent sources:

- `rue-builtins` publishes raw `runtime_fn: &str`-style constants and prose
  signatures;
- semantic analysis constructs raw external-call names and layouts for string,
  character, formatting, parsing, allocation, I/O, panic, and assertion paths;
- codegen maps CFG operations to symbol literals and repeats parts of their
  logical ABI in value, call, allocation, and target lowering;
- x86-64 and AArch64 lowerers directly intern selected helper strings;
- `rue-runtime` declares Rust `extern "C"` exports and checks pointer unsafety
  by parsing its own source text; and
- `docs/runtime-abi.md` restates signatures manually and is already stale
  relative to the source.

The former archive smoke test proved only that an archive parsed and was
nonempty. It did not prove complete, unique, target-correct exports or agreement
between Rust function types and compiler call plans.

This duplication permits three failure classes:

1. a compiler and runtime rename disagree and fail only during linking;
2. both sides retain the same symbol but disagree on parameter order, width,
   pointer mode, or return convention and miscompile at runtime; and
3. one target archive omits or accidentally exports a helper while host-only
   tests remain green.

The `__rue_` prefix is not a sufficient identity. Compiler-generated functions,
drop glue, internal RIR intrinsics, runtime-private functions, and platform
shims also use reserved spellings. Prefix recognition therefore cannot replace
an exhaustive helper ID.

## Current ABI inventory and disposition

The live inventory is typed data in `crates/rue-runtime-abi`, not a table in
this ADR:

- `RuntimeHelperId::ALL` and `RuntimeHelperId::helper()` enumerate callable
  helpers and expose their canonical signatures and contracts;
- `ReservedExportId::ALL` and `ReservedExportId::export()` enumerate entry
  points, compiler-built memory routines, platform shims, and the ABI marker;
- `AggregateShapeId::ALL` owns explicit result-storage layouts; and
- `RUNTIME_ABI_VERSION` and `RUNTIME_ABI_VERSION_SYMBOL` own lockstep version
  metadata.

Runtime-private functions, internal callbacks, compiler-generated Rue symbols,
and compiler-internal intrinsics are not callable runtime helpers. Each carries
its own typed or mangled identity in its owning subsystem. An unclassified
externally visible reserved export is an archive-validation error.

## Decision

### A dependency-light Rust manifest crate

`rue-runtime-abi` is a leaf `no_std` crate. It performs no allocation or
initialization and depends on no compiler IR, semantic
type pool, codegen, linker, or runtime implementation crate. `rue-builtins`,
`rue-air`, `rue-cfg`, `rue-codegen`, `rue-compiler`, and `rue-runtime`
depend on it.

The canonical data is ordinary Rust declarations and `const` tables. A build
script, external schema parser, or generated checked-in source would introduce
a second tool and failure path without improving this fixed, small ABI. Macros
may remove repetition inside the crate, but their expansion is not a second
manifest.

The public conceptual model is:

```text
RuntimeHelperId
RuntimeHelper {
    id
    symbol
    parameters: &[AbiParameter]
    result: AbiResult
    safety: SafetyContract
    return_behavior: Returns | Never
    availability: TargetSet
    calling_convention: TargetC
}

AbiParameter {
    ty: AbiType
    mode: Value | ConstPointer | MutPointer | OutPointer(AggregateShape)
}

AbiResult = Void | Scalar(AbiType)
```

Names and exact Rust spelling may differ, but the information and ownership may
not. Explicit out pointers are parameters, not inferred platform sret
registers. `StrBufResult`, `OptionStrBufResult`, and integer option results are
named aggregate shapes whose slot types and order are declared once.

`AbiType` describes the physical C-boundary scalar, not a Rue semantic type:
the initial set includes exact signed and unsigned integer widths, the
canonical boolean word used by the current ABI, and opaque byte/result
pointers. Aggregate layout, source type identity, and target register
assignment remain outside the manifest.

### Stable identity, lookup, and string boundaries

`RuntimeHelperId` is exhaustive and cannot be constructed from an arbitrary
string. The manifest provides:

- `RuntimeHelperId::ALL` or an equivalent complete iterator;
- total ID-to-entry lookup;
- checked symbol-to-ID lookup for archive and linker boundaries;
- `Display` or `symbol()` for emitted diagnostics and relocations; and
- validation that IDs and exported symbols are unique.

Compiler IR and planning structures carry `RuntimeHelperId`. Conversion to a
symbol occurs only when formatting debug output, interning a target MIR
relocation, writing an object, inspecting an archive, or linking. User external
functions and compiler-generated Rue functions retain their existing distinct
symbol identities.

Source builtin and intrinsic owners map their typed operations to
`RuntimeHelperId`. The leaf crate does not depend on their enums and does not
store source-language strings. Exhaustive mapping tests in those owning crates
prove that each applicable builtin or intrinsic selects a real helper and uses
the canonical signature.

### Signature and call-plan ownership

The manifest owns logical C-boundary facts:

- ordered scalar and pointer parameters;
- explicit out-pointer aggregate shape;
- scalar or void result;
- never-returning behavior;
- pointer mutability and required validity;
- helper availability; and
- the target C calling-convention class.

The manifest does not own:

- Rue source types or type inference;
- CFG values, slots, places, or aggregate materialization;
- x86-64 or AArch64 registers and stack offsets;
- target instruction selection, flags, encodings, relocations, or clobber
  register sets; or
- object/archive parsing.

Existing shared call planning remains the canonical physical assignment path.
It consumes a manifest signature plus materialized operands. Target adapters
derive registers, stack locations, and caller-saved clobbers from
`TargetC`; per-helper register lists are forbidden unless a helper uses a
genuinely different convention. Width and extension are explicit manifest
facts and are never inferred from a Rue semantic type or symbol name.

### Safety and checked requirements

Each helper declares a `SafetyContract` sufficient to distinguish:

- no pointer preconditions;
- readable pointer/length input;
- writable allocation or result storage;
- allocation layout requirements;
- concrete enum discriminants supplied by the caller; and
- unconditional trap/termination.

This metadata is a compiler/runtime invariant and diagnostic aid, not a new
source-language effect system. AIR validates compiler-constructed operands
against the manifest's types and modes. Runtime conformance requires
`unsafe extern "C"` whenever raw-pointer validity is a caller obligation.

### Target availability and shims

The initial callable helper set is available on all three runtime targets:
x86-64 Linux, AArch64 Linux, and AArch64 macOS. The manifest represents
availability explicitly so a later target-specific helper cannot silently
appear universal.

Entry points, signal trampolines, startup functions, and compiler-built memory
routines use separate typed export records. Runtime conformance and archive
validation check them, but they cannot be selected as `RuntimeHelperId`.

### Runtime conformance

Each Rust export proves its function type against the manifest at compile time.
Manifest-generated wrappers and function-pointer assertions share the same row.
Source-text parsing is not part of this contract because it cannot prove
parameter order, result type, cfg applicability, or a single implementation.

For each target, conformance proves:

1. every applicable helper has exactly one implementation;
2. the declaration macro or wrapper owns the expected unmangled export
   attribute and symbol mapping;
3. the Rust C function type matches the ordered manifest signature;
4. pointer-bearing exports have the required unsafe contract; and
5. separately classified shims satisfy their own typed record.

The compiler independently inspects every embedded archive. Compile-time type
assertions/declaration ownership and archive symbol validation are
complementary: the former proves Rust types and the intended export spelling,
while the latter proves the artifact actually contains that spelling.

### Versioning

Compiler and runtime remain lockstep components of one Rue release. The
manifest exposes a monotonically increasing integer ABI version. Any
incompatible symbol, signature, mode, aggregate shape, safety, or availability
change increments it.

Each runtime archive exports one metadata object symbol named
`__rue_runtime_abi_vN`, where `N` is the decimal manifest version. It is an
unmangled, retained, one-byte read-only static whose value is zero; the version
is encoded only in the symbol name. Archive validation checks that the data
byte is the zero sentinel, not a version payload. It is not callable and is not
a `RuntimeHelperId`.

Object readers compare normalized linker names: Mach-O's platform-added leading
underscore is removed in the same way as other parsed symbols before matching.
Archive validation requires exactly the expected marker, rejects any other
`__rue_runtime_abi_v*` marker, and rejects multiple definitions before compiler
publication. Compiler/runtime compatibility across released revisions is not
promised.

### Documentation

`docs/runtime-abi.md` consumes the canonical classification. Handwritten
sections explain target calling conventions and invariants but do not restate
an unverified peer signature table.

## Implementation record

| Issue | Scope | Dependency |
| --- | --- | --- |
| RUE-827 — Replace compiler-internal intrinsic strings with a typed enum | Added exhaustive RIR identity; desugaring produces it and inference/sema consume it; source-name collision is rejected. | RUE-826 |
| RUE-828 — Add canonical runtime helper IDs and typed signatures | Added `rue-runtime-abi`, complete helper and ABI-version metadata, validation, and display. | RUE-826 |
| RUE-829 — Migrate builtin runtime mappings | Replaced builtin raw helper names and signature restatements with typed mappings. | RUE-828 |
| RUE-830 — Migrate directly lowered semantic calls | AIR calls use manifest records, validate operands, and preserve typed helper identity through AIR-to-CFG lowering. | RUE-828 |
| RUE-831 — Migrate runtime helper emission | Shared call planning and both target backends consume typed CFG helper calls; symbols appear only at MIR relocation/display boundaries. | RUE-828 |
| RUE-832 — Add runtime-side conformance | Rust exports prove their types, own export attributes, and emit the target archive's ABI-version marker. | RUE-828 |
| RUE-833 — Validate embedded runtime exports | Every embedded target archive is checked for required, duplicate, stale, and wrong-target exports and ABI version. | RUE-829, RUE-830, RUE-831, RUE-832 |
| RUE-834 — Remove remaining raw literals | String conversion is boundary-only; the inventory guard and current architecture documentation are repository gates. | RUE-827, RUE-833 |

The slices were delivered in dependency order: RUE-827 and RUE-828 established
typed identities; RUE-829 through RUE-832 migrated consumers and runtime
definitions; RUE-833 added artifact validation; and RUE-834 completed the
inventory and documentation cleanup.

The implemented contract does not redesign Rue's source semantics, native ABI,
aggregate layout, or target matrix. An ABI disagreement is a bug or a
separately accepted change, not a reason to make the manifest match one
accidental consumer.

## Validation obligations

The repository enforces:

- manifest uniqueness, exhaustive lookup, and aggregate-shape tests;
- focused builtin, intrinsic, semantic-call, call-plan, and cross-backend tests;
- compile-time runtime signature checks for all three targets;
- mutated archive tests for missing, duplicate, stale-version, misspelled, and
  wrong-target exports, parameterized by the expected target;
- native runtime/CLI execution on supported CI hosts;
- cross-target lowering, assembly, and object validation; native archive
  validation in each of the three target CI jobs; and
- a final production-source inventory that forbids raw helper literals outside
  canonical tables, runtime definitions, display/link boundaries, and tests.

The generated-oracle isolation rule and full-suite serialization rule in
`AGENTS.md` apply. Host execution alone is not sufficient evidence.
M4 does not embed all target runtime archives into one compiler or enable
foreign-target executable linking. Each compiler build continues to embed and
validate its target-matching runtime archive; the three native CI jobs
collectively cover all supported archives. Per-target embedding and
cross-target executable linking remain owned by RUE-600 and M9.

## Consequences

### Positive

- Runtime helper renames and signature changes become compile-time or
  publication-time failures instead of link/runtime surprises.
- Builtins, sema, CFG/call planning, both backends, runtime exports, archive
  validation, and documentation consume one contract.
- An exhaustive helper ID prevents raw strings and prefix heuristics from
  creating accidental ABI identities.
- Target-specific instruction and calling-convention facts remain visible in
  their correct owners.

### Negative

- The leaf crate and conformance machinery add deliberate structure around the
  compiler/runtime boundary.
- Every helper addition or incompatible change updates the manifest,
  implementation, validation, and ABI version together.

### Neutral

- This decision does not stabilize the runtime ABI across Rue releases.
- It does not change source-language semantics or introduce a preview feature.
- It does not genericize target MIR, registers, encoders, or object formats.

## Rejected alternatives

### Keep `docs/runtime-abi.md` as the contract

Prose is useful explanation but cannot enforce Rust function types, archive
contents, exhaustive compiler mappings, or target availability.

### Generate the manifest by parsing runtime Rust source

Source parsing duplicates Rust's type system, is cfg-fragile, and cannot prove
that compiler call construction consumes the same data. Runtime declarations
instead type-check against explicit shared Rust data.

### Use exported symbols as IDs

Strings permit fabrication, collide with other reserved symbol classes, and do
not force exhaustive matches. Symbols are a boundary representation of a typed
ID, not the compiler's identity.

### Put the manifest in `rue-builtins`, `rue-air`, or `rue-runtime`

Each choice creates an inverse dependency or blurs source semantics with the
binary contract. A leaf crate keeps the runtime freestanding and lets all
consumers share the same declarations.

## Future work

- Stabilizing a runtime ABI across compiler releases.
- Foreign runtime implementations or dynamic runtime negotiation.
- Target C ABI and general foreign-function signature classification.
- Canonical physical type layout under ADR-0052.
