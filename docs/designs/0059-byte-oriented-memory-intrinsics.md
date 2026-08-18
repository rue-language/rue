---
id: 0059
title: "Byte-oriented memory intrinsics: unify the two low-level families"
status: implemented
tags: [intrinsics, memory, pointers, allocator, bytes, semantics, stdlib, abi]
feature-flag: raw_bytes
created: 2026-07-17
accepted: 2026-07-17
implemented: 2026-07-20
spec-sections: ["4.13:5a", "9.2:7", "9.2:10", "9.2:11", "9.2:12", "9.2:13"]
superseded-by:
relates: ["RUE-937", "RUE-879", "RUE-971", "RUE-712", "ADR-0052", "ADR-0005", "ADR-0057"]
---

# ADR-0059: Byte-oriented memory intrinsics — one intentional set

## Status

Accepted. Ratified by Steve on 2026-07-17, resolving the RUE-937 design gate.
The open questions below are settled by the rulings recorded in
[Resolved at acceptance](#resolved-at-acceptance); implementation proceeds in
the phase order given, starting with Phase 1 (bulk primitives, RUE-937) and
Phase 2 (RUE-960). This record also serves as the first ADR of record for the
raw-byte family
(RUE-879), which shipped behind the `raw_bytes` preview gate without a dedicated
design document — its semantics have lived only in ADR-0052 (as transitional
substrate), ADR-0057 (as a consumer), the specification's §9.2, and the
`PreviewFeature::RawBytes.adr()` string. This document becomes that record and
proposes where the family goes next.

RUE-937 was filed as "Stabilize `raw_bytes`: remove the preview gate." The
maintainer direction on RUE-937 re-scoped it away from stabilization: rather
than freeze the preview byte family as-is, re-orient the older typed family to
be byte-oriented and deprecate the byte family, choosing "the right set of
intrinsics, not just whatever we happened to build in the past." This ADR is
that requested design.

## Summary

Rue has **two parallel low-level memory intrinsic families** that overlap and
neither of which is the intended surface. This ADR proposes to collapse them
into **one byte-oriented set**: a byte-and-alignment allocation family
(`@alloc`/`@realloc`/`@free` carrying `(size, align)` in bytes), the existing
pointee-typed scalar access pair (`@ptr_read`/`@ptr_write`) plus explicit
unaligned variants, two new bulk primitives (`@byte_copy` memcpy-shaped,
`@byte_set` memset-shaped) that replace the hand-rolled per-byte loops in std,
the element-scaled `@ptr_offset` (kept per ADR-0040), and the segregated
int↔pointer pair (`@ptr_to_int`/`@int_to_ptr`), all resting on the
`@size_of`/`@align_of` reflection substrate. The five `_bytes` intrinsics are
deprecated by folding into this set. Because both families already share the
`__rue_alloc`/`__rue_free`/`__rue_realloc` runtime helpers, this is a **surface
redesign, not a runtime ABI change**. The re-orientation of the typed family is
**sequenced behind ADR-0052 / RUE-971 canonical layout**: only once
`@size_of(u8)` reports 1 does the typed family become byte-correct, so the
deprecation cannot precede canonical layout landing. The `raw_bytes` gate is
**not stabilized** here; it is retargeted to govern the interim byte surface
until RUE-971 lands.

## Context

### Two families, neither intentional

Rue exposes two disjoint sets of memory intrinsics that do overlapping jobs:

- **Typed / slot family (RUE-1 slot era).** `@alloc`, `@free`, `@realloc`,
  `@ptr_read`, `@ptr_write`, `@ptr_offset`, `@ptr_to_int`, `@int_to_ptr`. These
  are pointee-typed and element-count-scaled: `@alloc(count)` reserves
  `count * @size_of(T)` bytes, `@ptr_offset(p, n)` strides `n * @size_of(T)`,
  `@ptr_read` moves a whole `T`. Nothing in their semantic analysis hard-codes a
  width — the analysis is written in terms of `@size_of(T)`. The "8" comes
  entirely from Rue's current flattened eight-byte-slot layout (ADR-0052), so
  today `@size_of(u8)` reports 8, `@alloc(n)` of `ptr mut u8` reserves `n*8`
  bytes, and `@ptr_offset(p_u8, 1)` strides 8. That is precisely why `StrBuf`
  could not pack bytes through this family.

- **Preview byte family (RUE-879, `--preview raw_bytes`).** `@alloc_bytes`,
  `@realloc_bytes`, `@free_bytes`, `@byte_read`, `@byte_write`. These are flat
  `u8`, physical-byte-counted, and byte-aligned. RUE-879 added them as an
  **expedient unblock for `StrBuf`**: the slot family could not express packed
  single-byte storage, and the layout migration (RUE-880) was not scheduled, so
  a byte-shaped family was bolted alongside rather than fixing layout first.

### Why neither is the "right set"

The maintainer direction on RUE-937 states the problem directly: the older
intrinsics are "shaped around the 8 byte fields rather than intentionally
designed," and the goal is "the right set of intrinsics, not just whatever we
happened to build in the past," surveying Rust and Zig — "Rust generally has the
right intrinsics here" and Zig "is very good at this kind of stuff." Concretely:

- The typed family is slot-shaped: byte-accurate only once RUE-971 lands, and
  even then it lacks bulk copy/set and an unaligned access mode.
- The byte family is `u8`-only and cannot express typed multi-unit moves (which
  `ArrayBuf` needs), has **no alignment parameter** (`@alloc_bytes` guarantees
  only alignment 1 — flagged as an open question by ADR-0052), and has **no
  null-pointer primitive** (its consumers reach into the *typed* family's
  `@int_to_ptr` for the null idiom).
- Every std consumer that copies bytes hand-rolls a one-byte-at-a-time `while`
  loop, because there is no bulk-copy primitive in either family.

### The dependency and consumer landscape

- **Shared runtime.** Both families lower to the same three runtime helpers;
  `RuntimeCallKind::helper()` maps `AllocTyped | AllocBytes → __rue_alloc`,
  `FreeTyped | FreeBytes → __rue_free`, `ReallocTyped | ReallocBytes →
  __rue_realloc`. There are no separate `__rue_alloc_bytes` symbols. The helpers
  already take `(size, align)`; typed calls compute `size = count * size_of(T)`
  and `align = align_of(T)` in codegen, byte calls pass `size` through with
  `align = 1`. Unifying the intrinsic surface therefore requires little runtime
  change.
- **Canonical layout (ADR-0052 / RUE-971).** ADR-0052 was ratified on
  2026-07-17; the migration is staged under the RUE-971 epic (children RUE-972
  through RUE-978). Its final phase (RUE-978) is literally "reassess the
  RUE-879 raw-byte family once ordinary `u8` and typed pointer operations use
  physical layout." The re-orientation this ADR proposes is the same endpoint
  ADR-0052 anticipates, and it presupposes canonical layout has landed or
  co-lands.
- **Trusted-std carve-out.** `require_preview` authorizes `raw_bytes` for files
  with standard-library provenance (by `file_id`, not path), and this carve-out
  is `RawBytes`-specific. `std/strbuf.rue`, `std/fs.rue`, `std/env.rue`, and
  `std/mem.rue` depend on it so that programs consuming std need no `--preview
  raw_bytes`.
- **ADR-0057 (File IO v0)** is a live consumer: `std.fs` marshals reads and
  writes through a temporary contiguous `@alloc_bytes` buffer, copying with
  byte-at-a-time loops, and uses `@ptr_to_int` for null checks and syscall
  addresses. Its migration intent already points at slices; this proposal must
  not strand it, so the interim byte surface stays available and `std.fs`
  re-points at the unified names on the same schedule as `StrBuf`.
- **ArrayBuf** is the typed-family consumer: it moves whole `T` values through
  `@ptr_read`/`@ptr_write` (multi-slot aggregates) and steps elements with
  element-scaled `@ptr_offset`. A pure `u8` byte family cannot serve it, so
  type-width read/write and element-scaled offset must survive the unification.

## Survey: Rust and Zig

Rust and Zig converge strikingly on the low-level memory surface; the survey
findings, distilled:

1. **Bytes are the unit at the primitive layer.** Rust `GlobalAlloc::alloc`
   returns `*mut u8` with size carried in `Layout`; Zig `rawAlloc(len, ...)`
   returns `[*]u8`. Element counts are sugar computed as `n * size_of(T)` on top.
2. **Alignment is first-class at allocation** — mandatory, and present on free.
   Rust carries it in `Layout { size, align }` (align a power of two); Zig passes
   an `Alignment` (log2 exponent) enum to every vtable method, `free` included.
   Neither language lets you allocate without stating alignment. Rue's
   `@alloc_bytes(size)` with no alignment is the one clear defect versus both.
3. **Free and realloc are sizeless: the caller returns `(size, align)` to the
   allocator.** Rust's layout-carrying `dealloc` and Zig's `memory: []u8` +
   `Alignment` `free` both let the allocator avoid per-block headers.
4. **Typed scalar read/write derive width from the pointee; aligned and
   unaligned are distinct.** Rust makes it two intrinsics
   (`read`/`read_unaligned`); Zig makes it one deref parameterized by
   `*align(N) T`. The *distinction* is non-negotiable — they compile differently
   and have different UB — but the *spelling* is a design choice.
5. **Bulk copy and bulk set are primitives, not loops.** Rust
   `copy_nonoverlapping` (memcpy) / `copy` (memmove) / `write_bytes` (memset);
   Zig `@memcpy` / `@memset`. Overlap is decided up front: a non-overlapping fast
   path plus a separate overlapping move.
6. **Endianness is library code over byte primitives**, never in the allocator
   or pointer surface. Rust puts it on integers (`to_le_bytes`); Zig in `std`
   (`writeInt(..., endian)`), always with an explicit endianness argument.
7. **Element-scaled and byte-granular pointer arithmetic are both real and
   distinct** — "next element" versus "advance N raw bytes."
8. **Int↔pointer conversion is a named, segregated escape hatch** kept apart
   from arithmetic and access (Rust `addr`/`with_addr` and the provenance pair;
   Zig `@intFromPtr`/`@ptrFromInt`), so the common path never silently launders
   an address into a pointer.

**The one divergence: alignment and access invariants in the type system vs. in
intrinsic names.** Zig folds alignment (`*align(N)`), pointer kind (`[*]`/`[]`),
and length into *types*, shrinking the builtin set to `@memcpy`/`@memset`/
`@ptrCast`/`@alignCast`/`@intFromPtr`/`@ptrFromInt` plus the `@sizeOf` family.
Rust encodes them in intrinsic *names* (`read_unaligned`) and wrapper structs
(`Layout`, `NonNull`). Zig's approach yields fewer primitives but demands a
richer pointer type system.

**Recommendation for Rue.** Rue has comptime type parameters but does not yet
have Zig's `*align(N)` pointer types. The pragmatic fit at Rue's stage is to
take **Zig's byte-oriented allocator ABI** and **Rust's explicit aligned/
unaligned intrinsic split**, deferring type-carried alignment until the pointer
type system grows. Endianness stays out of the intrinsic surface entirely —
binary format readers/writers are source-defined over `int ↔ [N]u8` conversion
plus `@byte_copy`.

## Decision

Adopt one byte-oriented intrinsic set. Signatures below use `ptr mut u8` /
`ptr const u8` for the byte surface and pointee-typed `ptr T` where width is
derived from `T`. All argument sizes and offsets in the allocation and bulk
families are **byte counts**; alignment is a byte count that must be a power of
two. Every intrinsic remains `checked`-gated (unchecked operations) exactly as
today.

### Allocation — byte size + explicit alignment, sizeless free

```text
@alloc(size: u64, align: u64)                          -> ptr mut u8
@realloc(p: ptr mut u8, old_size: u64, align: u64, new_size: u64) -> ptr mut u8
@free(p: ptr mut u8, size: u64, align: u64)            -> ()
```

`size`/`old_size`/`new_size` are bytes; `align` is a power-of-two byte count.
Null is returned on OOM; `size == 0` follows the existing zero rules. This is
the sizeless-allocator ABI (principles 1–3): the caller returns `(size, align)`
so the runtime stays header-free. The runtime helpers already accept
`(size, align)`, so this is an operand-plan and signature change only — no new
`__rue_*` symbol. Typed allocation becomes **source-defined sugar**: `ArrayBuf`
allocates `@alloc(count * @size_of(T), @align_of(T))`, exactly as Rust
`alloc(T, n)` and Zig `alignedAlloc` compute `n * size_of(T)` over the byte ABI.
This absorbs `@alloc_bytes`/`@realloc_bytes`/`@free_bytes` and supplies the
alignment parameter they lack — the defect ADR-0052 flagged.

### Typed scalar access — width from the pointee, aligned + unaligned

```text
@ptr_read(p: ptr const T | ptr mut T)                  -> T
@ptr_write(p: ptr mut T, value: T)                     -> ()
@ptr_read_unaligned(p: ptr const T | ptr mut T)        -> T          (new)
@ptr_write_unaligned(p: ptr mut T, value: T)           -> ()         (new)
```

**Reconciliation with the survey strawman.** The strawman spelled these
`@ptr_read(comptime T, p)`. Rue already has the *better* shape: `@ptr_read`
derives `T` from the pointee via HM inference (reconciled against the annotation
to avoid silent truncation, RUE-244), so no explicit type argument is needed.
Keep Rue's existing pointee-typed spelling. The aligned pair keeps its current
signatures; RUE-971 makes their width physically correct (`@size_of(T)` bytes
instead of a slot). The **new** `_unaligned` variants are the Rust-style
explicit escape for packed/parsed data, chosen over a `*align(N)` pointer type
because Rue lacks alignment-qualified pointers today. `@ptr_read`/`@ptr_write`
must survive because `ArrayBuf` moves whole multi-unit `T` values a `u8`-only
family cannot express.

### Bulk memory — the missing primitives

```text
@byte_copy(dst: ptr mut u8, src: ptr const u8, size: u64) -> ()      (new; non-overlapping)
@byte_set(dst: ptr mut u8, byte: u8, size: u64)           -> ()      (new; memset)
```

`@byte_copy` is memcpy-shaped: `dst` and `src` must not overlap. `@byte_set` is
memset-shaped. These replace **every** hand-rolled per-byte loop in std —
`StrBuf::copy_packed_bytes`, the `std.fs` read/write marshalling loops, and
`std.mem.swap` — which is the single most-requested missing operation. An
overlapping `@byte_move` (memmove) is deferred until a real overlapping use
appears (see Open Questions); none exists in std today.

### Pointer arithmetic — element-scaled, kept

```text
@ptr_offset(p: ptr T, offset: integer)                 -> ptr T
```

Kept as element-scaled (`addr + n * @size_of(T)`), unchanged in spelling.
ADR-0040 already ratified this as standard pointer arithmetic and `ArrayBuf`
depends on it. Post-RUE-971, at `T = u8` it strides one byte naturally, so
byte-granular walking is `@ptr_offset` on a `ptr u8`; no separate byte-granular
intrinsic is added at this stage (add a byte-granular variant later only on
demonstrated need).

### Int↔pointer — the segregated escape hatch, kept

```text
@ptr_to_int(p: ptr T)                                  -> u64
@int_to_ptr(addr: u64)                                 -> ptr mut T
```

Kept unchanged. These stay distinct from arithmetic and access (principle 8).
`@int_to_ptr` on a zero address remains the null-pointer idiom and
`@ptr_to_int(p) == 0` the null check — the byte family never had these, and the
unified set does not strand the consumers that borrow them. Spelled out, since
`@int_to_ptr(0)` as written does not compile twice over — the intrinsic is
unchecked (E1300 outside a `checked` block) and takes `u64`, which an untyped
`0` literal is not (E0702):

```rue
let zero: u64 = 0;
let null: ptr mut u8 = checked { @int_to_ptr(zero) };
```

`std/fs.rue` and `std/mem.rue` use exactly this form. Names are chosen so a later strict/exposed
provenance split does not force a rename of the common path.

### Reflection substrate — kept, made byte-accurate by RUE-971

`@size_of` and `@align_of` are the substrate the entire byte surface computes on
(typed allocation, buffer sizing, field displacement). They are unchanged in
spelling; RUE-971 makes them report physical bytes instead of slots, which is
what makes the whole re-orientation byte-correct. `@offset_of` can join when
aggregate layout stabilizes.

### Fate of every current intrinsic

All eight typed intrinsics and all five byte intrinsics — thirteen total:

| Current intrinsic | Family | Fate | Detail |
|---|---|---|---|
| `@alloc` | typed | **Re-shape** | Becomes byte + align: `@alloc(size, align)`, size in bytes. Absorbs `@alloc_bytes`. `ArrayBuf` computes `size = count*@size_of(T)`, `align = @align_of(T)` in source. |
| `@realloc` | typed | **Re-shape** | `@realloc(p, old_size, align, new_size)` in bytes. Absorbs `@realloc_bytes`. |
| `@free` | typed | **Re-shape** | `@free(p, size, align)` — sizeless-allocator ABI. Absorbs `@free_bytes`. |
| `@ptr_read` | typed | **Keep + re-semantics** | Signature kept (pointee-typed via HM). Width becomes physical `@size_of(T)` post-RUE-971, not an 8-byte slot. Gains `@ptr_read_unaligned`. |
| `@ptr_write` | typed | **Keep + re-semantics** | Same. Gains `@ptr_write_unaligned`. |
| `@ptr_offset` | typed | **Keep** | Element-scaled (ADR-0040). Becomes byte-exact stride post-RUE-971; strides 1 at `T = u8`. |
| `@ptr_to_int` | typed | **Keep** | Correct escape-hatch shape; unchanged. |
| `@int_to_ptr` | typed | **Keep** | Unchanged. Remains the null-pointer idiom for all consumers. |
| `@alloc_bytes` | preview | **Deprecate → fold** | Into re-shaped `@alloc`; gains the missing `align` parameter. |
| `@realloc_bytes` | preview | **Deprecate → fold** | Into re-shaped `@realloc`. |
| `@free_bytes` | preview | **Deprecate → fold** | Into re-shaped `@free`. |
| `@byte_read` | preview | **Deprecate → fold** | Subsumed by `@ptr_read` on a `ptr u8` (width 1 post-RUE-971); redundant once width derives from the pointee. |
| `@byte_write` | preview | **Deprecate → fold** | Subsumed by `@ptr_write` on a `ptr u8`. |
| — | new | **Add** | `@byte_copy`, `@byte_set`, `@ptr_read_unaligned`, `@ptr_write_unaligned`. |

Net: one byte-and-alignment allocation family, the pointee-typed scalar access
pair plus explicit unaligned variants, two bulk primitives, an element-scaled
offset, and a segregated int↔pointer pair, all over the `@size_of`/`@align_of`
substrate. Everything above — `StrBuf`, IO buffers, binary format readers,
endianness — stays source-defined, matching how Rust and Zig keep policy out of
the primitive surface.

## Sequencing

The re-orientation of the **typed** family is gated on ADR-0052 / RUE-971
canonical layout. Today `@size_of` reports slot-flattened sizes, so byte-correct
typed operations only make sense post-migration: until `@size_of(u8) == 1`, the
typed family still strides and allocates in eight-byte slots, and `StrBuf`/
`std.fs` cannot move onto it. "Deprecate the byte family" therefore **cannot
precede canonical layout landing**. the migration's final phase (RUE-978) already names this
reassessment; this ADR supplies its target shape.

**Can happen now, independent of RUE-971:**

- Add `@byte_copy` and `@byte_set` to the interim byte surface. They replace the
  hand-rolled per-byte loops in `std/strbuf.rue`, `std/fs.rue`, and `std/mem.rue`
  immediately and depend on no layout change — pure wins available today.
- Add the alignment parameter to the byte allocator (fix the `@alloc_bytes`
  alignment-1 defect that ADR-0052 flags), so the interim surface already carries
  the `(size, align)` contract the final `@alloc` requires.

**Must wait for RUE-971 (or co-land with its compact-representation phase):**

- Re-shaping `@alloc`/`@realloc`/`@free` from element counts to byte + align and
  folding the `_bytes` allocators into them.
- Making `@ptr_read`/`@ptr_write`/`@ptr_offset` byte-correct and folding
  `@byte_read`/`@byte_write` into `@ptr_read`/`@ptr_write`.
- Adding `@ptr_read_unaligned`/`@ptr_write_unaligned` (their contract is only
  meaningful once alignment is a real physical property).

**Disposition of the moving parts:**

- **The `raw_bytes` preview gate (RUE-937).** Not stabilized. RUE-937 is
  re-scoped from "remove the gate" to "deliver the unified set." The gate is
  retained and retargeted to govern the interim byte surface (including the new
  bulk primitives) and the unified set until RUE-971 lands and the surface is
  proven; it is then removed by the ADR-0005 stabilization procedure (drop
  `preview =` from spec tests, remove `require_preview()`, remove the
  `PreviewFeature::RawBytes` variant, set this ADR to `implemented`). Per the
  acceptance ruling, the surface stays gated until RUE-971 lands and is
  stabilized once, at the end — no partial early stabilization.
- **The `trusted_std_raw_bytes` carve-out.** Retained unchanged while the gate
  exists — `std/strbuf.rue`, `std/fs.rue`, `std/env.rue`, `std/mem.rue` keep
  authorization by std provenance, and the two tests in
  `std_internal_raw_bytes.toml` keep asserting it. It is removed together with
  the gate at stabilization.
- **Specification sweep.** The eventual sweep touches the intrinsic registry
  table row `4.13:5a` (rename/merge of the listed names) and the normative
  semantics in §9.2: `9.2:7` (`@ptr_offset` scaling), `9.2:10`–`9.2:13` (alloc/
  free/realloc redefined from element counts to physical bytes), and the entire
  gated raw-byte block `9.2:14a`–`9.2:14f` (folded in or removed) — in
  particular the `9.2:14a` clause that explicitly *preserves* element scaling is
  the sentence that must be rewritten. This ADR only names these paragraphs; it
  does not edit the specification (spec edits land with the implementing phases
  per AGENTS.md).

## Implementation Phases

Phases are ordered by the sequencing above. RUE-937 is the tracking issue.

- [x] **Phase 1: Bulk primitives (now, no RUE-971 dependency)** — RUE-937. Add
  `@byte_copy` and `@byte_set` to the `raw_bytes` surface; port
  `StrBuf::copy_packed_bytes`, the `std.fs` marshalling loops, and
  `std.mem.swap` off their per-byte loops; add spec coverage under the existing
  gate. Landed in PR #1750.
- [x] **Phase 2: Alignment on the byte allocator (now)** — RUE-960. Add
  the `align` parameter to the byte allocator, giving the interim surface the
  `(size, align)` contract; update the runtime operand plan (already
  `(size, align)`-shaped) and `std` call sites. Landed in PR #1752. The interim
  `_bytes` surface it shaped no longer exists — Phase 3 folded it away — but the
  `(size, align)` contract it established is the one the unified family carries.
- [x] **Phase 3: Byte-oriented allocation family (post-/co-RUE-971)** —
  RUE-961. Re-shape `@alloc`/`@realloc`/`@free` to byte + align; fold
  the `_bytes` allocators in; move `ArrayBuf` typed allocation to source-computed
  `@size_of`/`@align_of` sugar. Landed with the hard removal ruled at
  acceptance: `@alloc_bytes`/`@realloc_bytes`/`@free_bytes` no longer exist and
  every `std/` consumer moved in the same change. Typed allocation now lives in
  `std/rawbuf.rue`, the one place that turns `count` into
  `count * @size_of(T)` and casts the `ptr mut u8` block to `ptr mut T`.
  Two consequences worth recording: the allocation-size overflow trap moved out
  of codegen into the language's ordinary trapping multiply, and the differential
  oracle gained a lazy reinterpretation of an untouched byte block as cells of
  the first pointee viewed over it, which is how it keeps modeling typed
  containers now that no allocation carries an element type.
- [x] **Phase 3a: In-place resize and zeroed allocation** — RUE-968. Added
  `@resize(p, old_size, align, new_size) -> bool` (Zig's `Allocator.resize`:
  in-place only, the pointer never moves, refusal is `false`) and
  `@alloc_zeroed(size, align)`, both on the unified `(size, align)` ABI, with
  the new `__rue_resize` / `__rue_alloc_zeroed` runtime helpers. Pulled forward
  from Future Work ahead of the issue's wait-for-trigger note.
- [x] **Phase 3b: Overlapping bulk move** — RUE-964. Added
  `@byte_move(dst, src, size)`, the memmove-shaped sibling of `@byte_copy`
  (which stays memcpy-shaped, overlap undefined), over the new
  `__rue_byte_move` helper. Purely additive.
- [x] **Phase 4: Byte-correct typed access (post-RUE-971)** — RUE-962.
  `@ptr_read`/`@ptr_write`/`@ptr_offset` are physical-layout-driven,
  `@ptr_read_unaligned`/`@ptr_write_unaligned` exist, and `@byte_read`/
  `@byte_write` are folded into `@ptr_read`/`@ptr_write` (hard removal, no
  alias window). The `*align(N)` re-evaluation this phase required was made and
  declined; see the fold ruling under "Resolved at acceptance".
- [x] **Phase 5: Specification sweep** — RUE-963. Rewrote `4.13:5a` and
  `9.2:7`, `9.2:10`–`9.2:13`, and updated traceability. The `9.2:14a`–`9.2:14f`
  removal this phase was written to perform had already happened: Phase 3
  restructured that range, and the `14d`/`14e`/`14f` deletion is recorded under
  the `@byte_read`/`@byte_write` fold ruling below.
- [x] **Phase 6: Stabilize and de-gate** — RUE-937. Landed in PR #1829, done
  2026-07-20. Removed `preview =` from spec tests, the `require_preview()`
  sites, the `trusted_std_raw_bytes` carve-out, and `PreviewFeature::RawBytes`.
  This ADR's `status` and `implemented:` date follow that landing.

## Consequences

### Positive

- **One intentional surface.** A single set matching Rust/Zig convergence
  replaces two accidental families; the `u8`-only limitation, the missing
  alignment parameter, and the missing null primitive all disappear.
- **Bulk primitives available immediately.** `@byte_copy`/`@byte_set` land in
  Phase 1 with no RUE-971 dependency and delete every hand-rolled per-byte loop
  in std — a correctness and performance win before the layout migration.
- **No runtime ABI churn.** The shared `__rue_alloc`/`__rue_free`/`__rue_realloc`
  helpers already take `(size, align)`; this is a surface and operand-plan
  redesign, not a runtime change.
- **Alignment becomes a real contract.** The `(size, align)` allocation ABI
  satisfies ADR-0052's requirement that raw allocation for later typed access
  prove both size and alignment.
- **ADR-0057 is not stranded.** `std.fs` keeps a working byte surface throughout
  and re-points at the unified names on schedule; its slice migration is
  orthogonal.

### Negative

- **Gated on an unscheduled migration.** The typed re-orientation cannot land
  until the RUE-971 migration epic completes its compact-representation phase (RUE-974); the full payoff is deferred.
- **Std call-site churn.** `StrBuf`, `ArrayBuf`, `std.fs`, `std.env`, and
  `std.mem` all change call sites (typed allocation becomes source-computed byte
  sugar; per-byte loops become bulk calls).
- **Spec rewrite of live normative text.** The §9.2 raw-byte block and the
  element-scaling clauses must be rewritten, not merely extended.
- **Two-step deprecation window.** While the interim and unified surfaces
  coexist, the `raw_bytes` gate and its trusted-std carve-out persist longer
  than a simple stabilization would.

### Neutral

- Endianness stays out of the intrinsic surface by design; binary formats remain
  source-defined over `int ↔ [N]u8` plus `@byte_copy`.
- No change to the `checked {}` requirement; every intrinsic here stays
  unchecked-gated.

## Resolved at acceptance

The proposal's open questions, settled by the 2026-07-17 ratification:

- **Alignment spelling: plain `align: u64`, power-of-two, validated.** Sema
  rejects non-power-of-two constants at compile time; runtime values carry a
  checked-gate-consistent contract. A dedicated `Align`/`Layout` type (Zig's
  log2 `Alignment`, Rust's `Layout`) is deferred — the call-site shape does not
  change when a typed refinement arrives.
- **Deprecation shape: hard removal at fold-in, no alias window.** The `_bytes`
  names have four consumers, all in `std/`, all migrated in the same PR that
  folds them. Preview-gated user code is opted into instability by definition
  (ADR-0005); an alias window would double the spec surface for names being
  deleted.
- **Gate timing: single de-gate at the end.** The unified surface stays behind
  `raw_bytes` until RUE-971 lands and the surface is proven, then stabilizes
  once (Phase 6). Partial early stabilization of the bulk primitives would
  leave §9.2 describing a half-gated family, and the features users want reach
  them ungated through std via the trusted-std carve-out regardless.
- **Overlapping copy: deferred.** `@byte_copy` (non-overlapping) only; no std
  consumer overlaps today, and a later `@byte_move` (memmove) is purely
  additive. Superseded: `@byte_move` shipped in Phase 3b (RUE-964), exactly as
  the additive change this ruling anticipated.
- **Byte-access fold: accepted with the two-call nesting; `@ptr_offset` stays
  sugarless.** Ruled by Steve on 2026-08-10, closing Phase 4 (RUE-962). The
  single-byte pair folds by direct substitution:

  ```
  @byte_read(p, i)      →  @ptr_read(@ptr_offset(p, i))
  @byte_write(p, i, v)  →  @ptr_write(@ptr_offset(p, i), v)
  ```

  Both forms are equivalent because `@size_of(u8)` is `1` post-canonical-layout,
  so an element-scaled `@ptr_offset` over a `ptr u8` strides exactly one byte;
  the existing spec case
  `byte_access_agrees_with_typed_access_on_a_u8_pointer` proved the two spellings
  access the same byte before the fold. The substitution applies unchanged to
  `ptr const u8`, since both `@ptr_offset` and `@ptr_read` accept const
  pointers. The nesting is the accepted cost: no byte-granular `@ptr_offset`
  sugar and no offset operand on the access pair are added, keeping one
  element-scaled arithmetic intrinsic (9.2:7) and one typed access pair
  (9.2:6b/9.2:6d) rather than reintroducing a parallel byte-shaped surface —
  which is the very duplication this ADR exists to remove. Removal is hard, with
  no alias window, matching the `_bytes` allocator ruling above: `@byte_read` is
  now an unknown intrinsic (E0700). Spec rules `9.2:14d`/`14e`/`14f` are deleted
  and their coverage re-homed onto `9.2:6b`/`9.2:6d`.
- **Null primitive: keep the `@int_to_ptr` idiom.** A dedicated `@null_ptr`
  acquires design questions (typed or untyped result?) better answered by the
  future provenance work. See the spelled-out form above: it needs a `checked`
  block and a `u64`-typed zero.
- **Aligned/unaligned spelling: Rust-style `_unaligned` intrinsics, with a
  Phase 4 checkpoint.** Type-carried `*align(N)` alignment (Zig model) changes
  only the access split — the allocator still passes alignment explicitly even
  in Zig, and Rue's access surface is intrinsic-shaped rather than
  deref-shaped, so the ergonomic delta is small. Because the `_unaligned` pair
  is Phase 4 (post-RUE-971) and nothing is built on the split before then,
  Phase 4 explicitly re-evaluates `*align(N)` before implementing the
  intrinsics; a byte-granular `@ptr_offset` variant is likewise deferred to
  demonstrated need (element-scaled at `T = u8` covers byte walking
  post-RUE-971). **Checkpoint answered:** Phase 4 made the re-evaluation and
  declined `*align(N)`, shipping the `_unaligned` pair. Type-carried alignment
  remains open as its own design question on RUE-965, not as a debt of this ADR.

## Future Work

Explicitly out of scope for this ADR, for later designs:

- **`*align(N)` pointer types** folding the aligned/unaligned split into the
  type system (Zig model), retiring the `_unaligned` intrinsics — when Rue's
  pointer types grow richer (ties to ADR-0052's alignment-qualified-pointer open
  question).
- **Strict/exposed provenance split** on `@ptr_to_int`/`@int_to_ptr` as Rue's
  memory model formalizes.
- ~~**In-place-only resize** (`@resize` returning a success bool, Zig-style) for
  containers that manage their own copy.~~ Delivered in Phase 3a (RUE-968).
- ~~**`@offset_of`** once aggregate layout stabilizes under RUE-971.~~ Already
  present since RUE-301; verified against `@field_ptr` and in comptime constant
  positions under RUE-969.
- ~~**`@alloc_zeroed`** if the allocator can zero more cheaply than
  `@byte_set`.~~ Delivered in Phase 3a (RUE-968). The current allocator recycles
  arenas and so has no cheaper path than clearing the block, but the intrinsic
  makes the guarantee expressible and confines a future zero-page mapping to the
  runtime.
- **Endianness** stays library-level forever (`to_le_bytes`/`from_le_bytes`-style
  conversions over the byte primitives); no byte-order intrinsic.

## References

- RUE-937 — tracking issue (re-scoped from "stabilize `raw_bytes`" to this
  unification).
- RUE-879 — the transitional raw-byte intrinsic family (no prior ADR of record;
  this document becomes it).
- RUE-880 / RUE-971 / ADR-0052 — canonical physical type layout (design epic / implementation epic); its final phase reassesses
  the raw-byte family, and it flags the `@alloc_bytes` alignment defect.
- RUE-712 / ADR-0057 — File IO v0, a live consumer of the byte family and the
  trusted-std carve-out.
- ADR-0005 — preview-feature gating and stabilization procedure.
- ADR-0040 — ascending array layout and element-scaled `@ptr_offset` (preserved).
- ADR-0028 — unchecked code and raw pointers (the `checked {}` requirement).
- Rust `std::alloc::GlobalAlloc` and `Layout` (byte + power-of-two align,
  layout-carrying `dealloc`): <https://doc.rust-lang.org/std/alloc/trait.GlobalAlloc.html>,
  <https://doc.rust-lang.org/std/alloc/struct.Layout.html>
- Rust `core::ptr` (`read`/`write`/`read_unaligned`/`copy_nonoverlapping`/
  `write_bytes`): <https://doc.rust-lang.org/std/ptr/index.html>,
  <https://doc.rust-lang.org/std/ptr/fn.read_unaligned.html>
- Rust pointer methods and provenance (`add`/`byte_add`, `addr`/`with_addr`):
  <https://doc.rust-lang.org/std/primitive.pointer.html>
- Zig `std.mem.Allocator` (byte `len`, first-class `Alignment`, sizeless
  `free`): <https://github.com/ziglang/zig/blob/master/lib/std/mem/Allocator.zig>
- Zig language reference (`@memcpy`/`@memset`/`@intFromPtr`/`@ptrFromInt`/
  `*align(N)`): <https://ziglang.org/documentation/master/>
