---
id: 0040
title: "Array layout is ascending; @ptr_offset is standard pointer arithmetic"
status: implemented
tags: [layout, arrays, pointers, codegen, abi]
feature-flag:
created: 2026-07-03
accepted: 2026-07-03
implemented: 2026-07-03
spec-sections: ["3.6", "9"]
superseded-by:
---

# ADR-0040: Array layout is ascending; `@ptr_offset` is standard pointer arithmetic

## Status

Accepted. Ratified by Steve on 2026-07-03 ("we should be like every other
language here"). Resolves RUE-243. Supersedes the descending-local-array layout
that RUE-213 patched.

## Summary

Rue lays out array elements **ascending**: element 0 at the lowest address,
element `i` at `base + i * element_size` — the same convention as Rust, C, and
every mainstream ABI. Consequently `@ptr_offset(p, n)` is `p + n *
sizeof(pointee)` (a plain **add**), uniform for pointers of every origin (into a
local array, the heap, an mmap region, or an `@int_to_ptr` address). Array
indexing and pointer arithmetic therefore agree by construction, and neither is
special-cased on where the pointer points.

## Context

The reference implementation had been laying **local arrays out descending** in
the stack frame — element 0 at the *highest* address (RUE-213 uncovered this:
array indexing *subtracted* `index * size`). To make a raw pointer into such an
array walk in step with indexing, RUE-213 then made `@ptr_offset` *subtract*
too. That broke standard pointer arithmetic for every **non-array** pointer
(heap / mmap / `@int_to_ptr`): `@ptr_offset(base, 1)` moved *backward*, and
`-1` moved forward (RUE-243) — contradicting ADR-0028, which documents
`@ptr_offset(p, 1)` as *advancing* by `sizeof(pointee)`.

The tension was entirely self-inflicted. Rust, C, and every mainstream language
lay arrays out ascending, so array indexing *adds* and pointer offset *adds* —
they agree, and pointer arithmetic never depends on the pointee's origin. Rust
even makes ascending-contiguous array element layout a hard guarantee (only
struct field order is left to `repr`). The descending layout gave Rue a problem
nobody else has, and RUE-213's subtract treated the symptom rather than the
cause.

## Decision

- **Array element layout is ascending**: element `i` of `[T; N]` is at
  `base + i * size_of(T)`, element 0 at the array's lowest address. (This pins,
  for arrays, what 3.6 otherwise leaves implementation-defined; struct field
  layout remains implementation-defined.)
- **Array indexing** computes `base + index * size` (add).
- **`@ptr_offset(p, n)` is `p + n * sizeof(pointee)`** — standard pointer
  arithmetic, an unconditional add, identical for all pointer origins. Negative
  `n` moves toward lower addresses; positive `n` toward higher. Out-of-allocation
  offsets remain undefined behavior in `unchecked` code (ADR-0028), unchanged.
- **`@raw`, `@ptr_read`, `@ptr_write`** operate over the ascending layout.
- The RUE-213 descending-frame subtract is removed; its array-indexing
  correctness is preserved by the ascending model (indexing and offset both add).

## Consequences

### Positive
- Standard and coherent — matches Rust/C/every ABI; no surprises for anyone
  coming from another systems language.
- `@ptr_offset` needs no special case and no longer depends on where a pointer
  points; raw-pointer code (and RUE-1 / `Vec`, which walks pointers) behaves as
  written.
- Removes the RUE-213 subtract patch and the class of confusion it created.

### Negative
- A codegen layout change touching array-index lowering, `@raw`, `@ptr_offset`,
  and the aggregate-slot layout — mechanical but broad. Mitigated by the
  differential oracle now running in CI (#1059, RUE-205), which catches any
  behavioral regression, plus the release-mode CI job (#1060).
- Any code or test that (wittingly or not) depended on the descending layout
  must be updated; the ascending result is the correct one.

## Alternatives Considered

- **Keep descending arrays, make `@ptr_offset` add anyway, fix RUE-213 in the
  index lowering.** Rejected: leaves a non-standard array layout in place for no
  benefit, still surprising to everyone, and keeps two conventions in the codebase.
- **Document `@ptr_offset` as pointing into whatever layout the pointee lives
  in.** Rejected: leaks an implementation quirk into user-visible pointer
  semantics.
