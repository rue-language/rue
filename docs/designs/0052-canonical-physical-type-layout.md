---
id: 0052
title: Canonical Physical Type Layout
status: proposal
tags: [types, semantics, compiler, codegen, abi, memory]
feature-flag: aggregate_layout
created: 2026-07-14
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0052: Canonical Physical Type Layout

## Status

Proposal. Tracked by RUE-880. This record is a design gate, not authorization
to begin the migration.

## Summary

Rue will replace its eight-byte ABI-slot physical representation with one
canonical, target-aware type-layout authority. Physical memory layout, compiler
value decomposition, and function-call ABI classification will become separate
concepts. The first compact layout will use natural scalar sizes and alignments,
ascending array elements, declaration-order struct fields with padding, and a
straightforward tagged-enum representation. Field reordering and niche
optimization are explicitly deferred.

## Context

Rue currently uses flattened eight-byte slots for several different purposes:

- the internal representation of scalar and aggregate values;
- the physical size and alignment reported by `@size_of` and `@align_of`;
- struct field offsets and array element stride;
- typed allocation, pointer arithmetic, and pointer loads and stores;
- stack objects, parameters, and return values;
- aggregate equality, drop glue, and backend materialization.

The model was a useful bootstrap, but these concerns do not have to share a
representation. A narrow scalar may conveniently occupy a widened virtual
register while using one, two, or four bytes in memory. Likewise, the fact that
an aggregate has a particular physical representation does not determine how a
target calling convention passes it.

RUE-879 made the mismatch concrete. `StrBuf` uses packed bytes and byte-counted
length and capacity fields, but ordinary `ptr u8` operations stride, load,
store, allocate, and free in eight-byte slots. The preview-gated raw-byte
intrinsic family is a sound transition, but it must not become a peer aggregate
layout system.

RUE-288 already adopts an observe-versus-guarantee model for ordinary Rue
layout: compiler-mediated queries and field access observe the layout selected
for the compilation without promising a stable default representation. ADR-0040
requires ascending array layout and element-based pointer arithmetic. This ADR
preserves both decisions.

RUE-742 separately designs a guaranteed target C representation and C ABI.
Target C layout must be another representation computed by the canonical layout
authority, not an FFI-only calculator embedded in a backend.

## Decision

### One canonical physical-layout authority

The compiler will have one target-aware query for the physical layout of every
materializable type. Conceptually, its result contains at least:

```text
Layout {
    size
    alignment
    stride
    kind:
        Scalar
        Array { element_layout, count }
        Struct { field_offsets, padding_ranges }
        Enum { tag_layout, payload_offset, variants }
}
```

This is a conceptual contract, not a required Rust API. The implementation may
intern, split, or lazily compute the data as long as there is one canonical
computation path.

Every operation that observes or addresses memory must consume this authority,
including:

- `@size_of`, `@align_of`, and `@offset_of`;
- place projection and `@field_ptr`;
- array indexing and pointer stride;
- typed allocation, reallocation, and deallocation;
- stack and temporary object allocation;
- aggregate loads, stores, copies, moves, and drop glue;
- object constants and read-only data;
- both architecture-specific backends;
- the oracle, diagnostics, emitted IR/assembly presentation, and layout tooling.

No consumer may recompute a peer notion of field offsets, aggregate width, or
array stride from internal value fragments.

### Three independent representations

Rue will distinguish:

1. **Physical layout**: bytes, alignment, stride, field offsets, padding, and
   enum representation.
2. **Compiler value decomposition**: how AIR, CFG, MIR, register allocation, or
   scheduling represent a value internally.
3. **Call ABI classification**: whether arguments and returns use registers,
   stack locations, or indirect storage, including target register classes.

Internal values may continue to use widened scalars or slot-like fragments when
that simplifies analysis and code generation. Loads, stores, and marshaling
between internal values and memory must nevertheless follow the canonical
physical layout.

A separate canonical call-ABI classifier will eventually consume type layouts
and target ABI information. Calls, returns, exports, callbacks, and indirect
calls must share its result. Physical layout alone never certifies that a value
can be passed by value through a particular ABI.

### Initial compact native layout

The first migration will deliberately use a simple representation:

- fixed-width integer storage uses its physical byte width and target natural
  alignment;
- booleans receive a documented byte representation and validity rule;
- pointers use the selected target's pointer size and alignment;
- an array's elements are ascending and separated by the element stride;
- struct fields are placed in declaration order at offsets satisfying their
  alignment, with explicit interior and tail padding;
- enums use an explicit tag plus storage sufficient for the largest variant
  payload;
- zero-sized types receive one coherent size, alignment, addressability, and
  array-stride rule.

The exact scalar alignment table, enum tag selection, zero-sized-type rule, and
the relationship between size and stride remain part of the proposal review.
They must be specified before acceptance rather than inherited accidentally
from a backend or host compiler.

### Alignment is a pointer guarantee

The pointer design must be able to express the alignment guaranteed by a
pointer value. A guarantee may be weakened without a check. Strengthening it
must require either static proof or an explicit assertion that is checked in
safety-enabled execution.

This ADR does not select syntax for alignment-qualified pointers or the checked
cast. It does require that:

- a pointer derived from byte storage does not silently acquire the alignment
  of an unrelated destination type;
- an unaligned field does not manufacture an ordinary aligned borrow or
  pointer;
- unaligned access is explicit and performed by value;
- raw allocation intended for later typed access accepts or otherwise proves
  both size and alignment.

The long-term allocator primitive should therefore operate on a layout or an
equivalent `(size, alignment)` contract. Typed allocation derives that contract
from the canonical type layout. RUE-879's `@alloc_bytes` family must retain a
documented alignment guarantee until it is replaced or stabilized.

### Padding and byte validity are explicit concerns

Padding is not part of a value's semantic equality, hashing, or validity. Safe
code must not accidentally export stale stack or heap contents through padding.
The accepted design must specify when padding is initialized or canonicalized
and whether raw byte observation sees it.

Layout compatibility alone does not imply that arbitrary bytes form a valid
value. Booleans, enums, pointers, ownership-bearing values, and future refined
types may reject bit patterns. A safe conversion from bytes must either target a
type for which every representation is valid or validate the representation.
Equal size and alignment are insufficient justification for a safe bit cast.

The proposal favors deterministic zero padding on construction and a safe byte
export operation that canonicalizes padding, while leaving unrestricted object
representation access unchecked. Acceptance requires a cost and semantics
review, particularly for partial field writes, whole-value copies, concurrency,
and drop-bearing aggregates.

### Representation contracts remain distinct

Ordinary native layout remains implementation-defined under RUE-288. Layout
queries observe the selected representation; they do not freeze it across
compiler versions or targets.

Other representation intents are separate contracts:

- target C representation and C ABI are owned by RUE-742;
- transparent wrappers require a future explicit ruling;
- packed bit representations and portable wire formats require explicit bit
  and byte ordering rather than inheriting native endianness;
- frozen or resilient cross-module layout requires a future ABI-evolution
  design.

These modes must share the canonical layout authority while retaining their
own guarantees and eligibility rules.

## Implementation Phases

Implementation is intentionally unscheduled. After this ADR is accepted,
RUE-880 requires a fresh source and call-site audit before actionable children
are filed. The expected capability sequence is:

- [ ] **Canonical query** — introduce target-aware layout data while preserving
  current behavior.
- [ ] **Memory consumers** — route layout intrinsics, places, pointer stride,
  typed allocation, constants, and aggregate memory operations through it.
- [ ] **Compact representation** — adopt natural scalar and aggregate physical
  layout on x86-64 and AArch64.
- [ ] **Stack and value separation** — remove physical-layout dependence on
  compiler value fragments.
- [ ] **Call ABI classification** — introduce a canonical native classifier
  separate from target C classification.
- [ ] **Language integration** — update the specification, preview gating,
  traceability, oracle, UI/CLI coverage, and layout presentation.
- [ ] **Transitional cleanup** — reassess the RUE-879 raw-byte family once
  ordinary `u8` and typed pointer operations use physical layout.

Linear identifiers will be assigned only after acceptance and the fresh audit.

## Consequences

### Positive

- Narrow values and aggregates use representation-exact memory without
  constraining the compiler's internal register representation.
- `@size_of`, pointer arithmetic, allocation, field access, and backend memory
  operations agree by construction.
- Native and target C layouts can coexist without peer layout engines.
- Calling-convention work can evolve independently from object representation.
- Alignment mistakes can become explicit proof or checked-assertion failures
  instead of latent undefined behavior.
- Padding, byte validity, and serialization become designed language concerns
  rather than accidental consequences of a host ABI.

### Negative

- The migration is cross-cutting and changes semantics observed by unchecked
  code, allocation, tests, runtime boundaries, and emitted IR.
- Both backends need matching narrow load/store, addressing, stack, and
  marshaling coverage.
- Keeping internal widened values requires explicit packing and unpacking at
  memory and ABI boundaries.
- Deterministic padding or validated byte conversion may impose costs that need
  measurement and optimization.
- Existing slot-oriented tests and transitional runtime interfaces will need
  careful staged updates.

## Open Questions

- What exact size and alignment does each scalar have on every supported target?
- Does `size` include tail padding, or does Rue expose a distinct storage
  extent and array stride in the style of Swift?
- What are the size, alignment, stride, and addressability rules for zero-sized
  types and arrays of them?
- How is the initial enum tag width selected, and which invalid discriminants
  are forbidden?
- What padding initialization or canonicalization guarantee balances security,
  determinism, concurrency, and code-generation cost?
- Which types, if any, may safely expose their object representation as bytes?
- What syntax expresses pointer alignment, explicit checked strengthening, and
  unaligned access?
- What alignment does `@alloc_bytes` currently guarantee, and what long-term
  layout-based allocator surface replaces or subsumes it?
- Which parts of the current native calling convention should be preserved
  during migration, and which aggregates should temporarily pass indirectly?
- Does the migration use the existing `aggregate_layout` preview name or a
  different incremental gate selected during acceptance?

## Future Work

- Field reordering for native aggregates — RUE-881.
- Niche optimization for enums and optional-like values — RUE-882.
- Guaranteed target C layout and C ABI — RUE-742 and RUE-745.
- Transparent representations.
- Packed integer/bit representations with explicit bit ordering.
- Portable wire representations with explicit endianness.
- Frozen versus resilient cross-module layout.
- ABI layout fingerprints and link-time disagreement detection.
- C++ ABI compatibility, if ever desired.

## Prior Art

- Zig tracks alignment in pointer types, explicitly checks alignment
  strengthening, and separates ordinary, `extern`, and integer-backed `packed`
  structs: <https://ziglang.org/documentation/master/#struct>.
- Andrew Kelley's alignment example motivated the explicit checked-cast
  requirement: <https://andrewkelley.me/post/unsafe-zig-safer-than-unsafe-rust.html>.
- Rust separates default, C, packed/aligned, primitive-tagged, and transparent
  representations, and distinguishes layout from call ABI:
  <https://doc.rust-lang.org/reference/type-layout.html>.
- Ada record representation clauses allow exact component byte and bit ranges
  with compiler-checked legality:
  <https://www.adaic.org/resources/add_content/standards/05aarm/html/AA-13-5-1.html>.
- Swift exposes size, alignment, and stride separately and uses resilient versus
  frozen layout for ABI evolution:
  <https://developer.apple.com/documentation/swift/memorylayout>.
- WG14's padding analysis documents unspecified padding, bytewise comparison,
  and information-leak hazards in C:
  <https://www.open-std.org/jtc1/sc22/wg14/www/docs/n2012.htm>.

## References

- RUE-880 — foundational design epic
- RUE-881 — deferred field-reordering design
- RUE-882 — deferred niche-optimization design
- RUE-288 — observe-versus-guarantee layout model
- RUE-742 / RUE-745 — target C layout and FFI implementation
- RUE-879 — transitional raw-byte memory intrinsics
- RUE-738 — native and platform call-ABI audit
- RUE-8 / ADR-0028 — unchecked code and raw pointers
- ADR-0040 — ascending array layout and standard pointer arithmetic
