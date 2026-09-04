# Compact physical layout is now the default (ADR-0052)

*2026-07-19. RUE-987. The compact native physical layout ratified in ADR-0052
stops being a `--preview aggregate_layout` opt-in and becomes Rue's one and only
memory representation. This is an observable change: `@size_of`, `@align_of`, and
`@offset_of` now report smaller, natural values, and code that hardcoded the old
eight-byte-slot layout breaks.*

## What changed, observably

Until now every materialized value used a flattened **eight-byte ABI slot**
layout: every scalar occupied 8 bytes at 8-byte alignment, structs were packed
one field per slot with no padding, arrays strode by 8 bytes per element, and
every enum tag was a full 8-byte slot. That was always a placeholder. ADR-0052
ratified the **compact native layout**, and RUE-987 makes it the default:

- **Scalars take their natural LP64 width and alignment.** `@size_of(u8)` is now
  `1` (was `8`); `@size_of(i16)` is `2`, `i32` is `4`, `i64`/`u64`/pointers stay
  `8`. `@align_of` follows: `@align_of(i8)` is `1`, `@align_of(i32)` is `4`.
- **Structs are laid out in declaration order at natural alignment, with
  interior and tail padding.** `struct Padded { a: u8, b: i32, c: u8 }` is now
  `12` bytes (was `24`): `a@0`, padding `[1,4)`, `b@4`, `c@8`, tail padding
  `[9,12)`, alignment `4`. A struct's alignment is its widest field's.
- **Arrays stride by the compact element size.** `[i32; 3]` is `12` bytes (was
  `24`); `[u8; 4]` is `4` (was `32`).
- **Enum tags narrow to the smallest sufficient unsigned integer** (`u8` for up
  to 256 variants, then `u16`, then `u32`) and the payload is placed at the
  maximum variant alignment. A discriminant-only enum is one byte.
- **Zero-sized types** keep size `0`, alignment `1`, stride `0`.

Padding bytes are deterministically zeroed on construction (ADR-0052 ruling 5),
so a freshly built aggregate has no indeterminate bytes in its physical image.

## The `@ptr_offset` u8 stride change

A raw pointer to a narrow scalar now strides by that scalar's compact size.
`@ptr_offset(p, 1)` on a `ptr mut u8` advances **one** byte, not eight; on a
`ptr mut i16` it advances two, on a `ptr mut i32` four. Walking a heap
`[u8; N]`/`[i16; N]`/`[i32; N]` buffer now uses the compact stride, and narrow
loads/stores extend with the correct sign (signed integers sign-extend, `u8`,
`bool`, and unsigned integers zero-extend). This is the same authority the
compile-time queries report, so `@offset_of` + `@ptr_offset` navigation and a
direct field access still agree.

Note that this is the *physical* memory model. Rue's internal
value-decomposition — the slot-shaped model that frames, vregs, and the call ABI
use (ADR-0052 representation 2) — is unchanged; `@ptr_offset` over a pointer to a
whole aggregate still strides by that aggregate's slot-shaped storage size, not
its packed compact size.

## What breaks

**Any code that assumed the eight-byte-slot layout breaks.** Concretely:

- Hardcoded byte offsets or sizes (`8`, `16`, `24`, …) computed by hand for
  struct fields, array elements, or enum payloads are now wrong. Use
  `@offset_of` / `@size_of` instead of literals.
- Raw-pointer walks that advanced by a hardcoded 8 bytes per element now
  over-stride narrow buffers. Use `@ptr_offset`, which strides correctly.
- `@size_of` / `@align_of` / `@offset_of` return smaller values; any comparison
  or arithmetic pinned to the old numbers must be re-golded.

Programs that only used `@size_of` / `@offset_of` for navigation — never
hardcoding the slot numbers — keep working unchanged; that was the point of
exposing those intrinsics (RUE-288 / RUE-301). `StrBuf`, `str`/`Str(N)` views,
and any aggregate built entirely from eight-byte leaves are *slot-identical*:
their compact and slot layouts coincide, so they are byte-for-byte unaffected.

## The `--preview aggregate_layout` flag is gone

The preview feature is retired. `--preview aggregate_layout` is now a plain
unknown-preview error listing the remaining features (`test_infra`, `c_ffi`,
`non_exhaustive_enums`, `test_declarations`).
There is nothing to opt into: the compact layout is simply how Rue lays out
memory.

## What did *not* change

The layout *guarantees* are exactly as before (specification 3.6/4.13/9.2): a
type's memory layout remains **implementation-defined**. `@size_of`,
`@align_of`, and `@offset_of` are well-defined observations of the layout the
implementation chose; the concrete numbers they report are documented
observations, not language guarantees. RUE-987 changes those observed numbers
from the slot values to the compact values; it does not add or remove any
guarantee.

## Heterogeneous enums marshal too

An enum whose per-variant payloads disagree in layout — placing fields at
conflicting byte offsets or widths, e.g. `Result(SockAddr, NetworkError)` — has
no single variant-independent physical image, but it is **not** refused. Such an
enum (and any array of, struct containing, or enum nesting one) marshals through
a pointer and across a call by dispatching on its runtime tag and running the
active variant's own slot→byte map (RUE-1037). A store zeroes the full image
extent first, so bytes outside the active variant are deterministic with no
residue after a variant overwrite. This is the shape that `std.net`'s
`Result`-returning surface (`TcpListener.local_addr`) uses, and it now compiles
and runs under the default layout.

## The one remaining refusal: raw pointers into frame aggregates

One construct is refused rather than laid out. Taking a raw pointer with
`@raw` / `@raw_mut` / `@field_ptr` into a **frame-resident** non-slot-identical
aggregate — or indexing such a frame array through a place — is refused with a
construct-level diagnostic. The stack frame stores an aggregate slot-shaped (one
eight-byte slot per leaf, the unchanged value-decomposition model, RUE-975),
while `@ptr_read` / `@ptr_write` / `@ptr_offset` address memory by its packed
compact image, so a raw pointer into a frame aggregate would stride across
mismatched field and element layouts. Allocate the aggregate on the heap with
`@alloc`, whose storage *is* the compact image, to round-trip it through a raw
pointer. That refusal is about the construct, not about any preview flag; a
slot-identical aggregate (all eight-byte leaves) is unaffected because its frame
and compact images coincide.
