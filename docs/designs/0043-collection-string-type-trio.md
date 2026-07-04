---
id: 0043
title: "The collection & string type trio: fixed / slice / growable"
status: accepted
tags: [strings, collections, slices, arrays, vec, allocators, stdlib]
feature-flag:
created: 2026-07-03
accepted: 2026-07-03
implemented:
spec-sections: ["3.7", "3.9"]
supersedes:
amends: [0020, 0035, 0041, 0042]
---

# ADR-0043: The collection & string type trio (fixed / slice / growable)

## Status

Accepted — **experimental**. Ratified by Steve on 2026-07-03. It amends
several accepted ADRs (0035, 0041) and supersedes others in part (0020, 0042) —
see "Relationship to prior ADRs". Adopted to be *tried* while the surface is
still small enough to change cheaply (the `Vec`→`ArrayBuf` and `String`→`StrBuf`
renames included); repealed via `superseded-by:` if it proves wrong.
Implementation is tracked in the ADR-0043 epic.

## Summary

Collections and strings share **one** three-rung structure — **fixed**,
**slice**, **growable** — and strings are simply the `u8`-with-UTF-8
specialization of it, plus literal syntax. There is no special `String` type;
`String` becomes `StrBuf`, an ordinary library type that is the growable rung.

|  | **Fixed** (comptime `N`, no heap, no allocator) | **Slice** (second-class view, runtime len) | **Growable** (heap, allocator) |
|--|--|--|--|
| **Collection** | `[T; N]` *(exists)* | `borrow [T]` / `inout [T]` *(new)* | `ArrayBuf(T)` *(rename of `Vec`)* |
| **String** | `Str(N)` | `borrow str` / `inout str` | `StrBuf` |

- **Slice** is the new keystone: a second-class fat pointer (`ptr` + runtime
  `len`), valid only in argument position, in shared (`borrow`) and exclusive
  (`inout`) forms straight off the access model (ADR-0037). It is the universal
  read/write interface — the `&[T]`/`&str` analog, minus lifetimes and minus
  `Cow`, *because* it cannot escape.
- **`str` is `[u8]` + the byte-string convention of ADR-0035.** `Str(N)` is
  `[u8; N]` + convention; `StrBuf` is `ArrayBuf(u8)` + convention. Strings are a
  *refinement* of byte collections, not a parallel hierarchy.
- **String literals are static-backed, first-class `str`.** `"hello" : str`,
  storable and `Copy`, because static data cannot dangle — so it is exempt from
  the second-class rule that governs local borrows.
- **Naming uses the `-Buf` suffix** consistently: `ArrayBuf` / `StrBuf`. The
  suffix teaches the model ("`Buf` = the growable heap version of a slice").
- **Fallible allocation is explicitly out of scope** (see Non-decisions).

## Context

Two questions that looked separate — "why is `String` special?" and "should Rue
have explicit allocators?" — are one question. `String` is blessed (a synthetic
struct injected in scope, ADR-0020) precisely because a literal `"hello"` must
produce something *owned*, and owning a heap string hides an allocation. **The
specialness is a hidden global allocator.** Deciding the allocator model decides
`String`'s status; they cannot be chosen independently.

Two further observations unlock the design:

1. **Rust's core/std coherence pain is overwhelmingly *trait* pain.** `str`
   (`core`) vs `String` (`alloc`), `to_owned` via `ToOwned`, orphan-rule
   friction — it is all mediated by traits crossing a crate boundary. Rue is
   comptime-monomorphization-first with inherent methods and **no traits yet**
   (traits are future work), so a core/std-style split is *cheaper here now*
   than the Rust experience implies — and much cheaper to design *before* traits
   than to retrofit after.
2. **Rue has no first-class borrows and no lifetimes** (ADR-0037, MVS-style). So
   Rust's `str`/`String` (borrowed *view* vs owned) does not port: every Rue
   string type must be an **owned value** or a **second-class view**. That
   removes the entire lifetime/`Cow`/"return `&str`" family of problems — the
   split is far cheaper than Rust makes it look.

## Decision

### 1. The trio, for collections

- **Fixed:** `[T; N]` — value type, comptime length, no allocator. *(Exists.)*
- **Slice:** `borrow [T]` (shared) and `inout [T]` (exclusive) — a second-class
  fat-pointer view (`ptr` + runtime `len`), **argument-position only** (cannot be
  returned, stored, or otherwise escape — ADR-0037). Obtained by borrowing a
  fixed or growable value: `borrow a`, `borrow v`, `borrow v[i..j]`. `.len()` is
  the runtime length. This is the universal read/write interface over both other
  rungs (the deref-coercion analog, made explicit rather than magic).
- **Growable:** `ArrayBuf(T)` — the heap, allocator-backed, growing collection.
  This is `Vec` from ADR-0041, renamed (see naming, below); all of its mechanism
  (heap buffer via `@alloc`, amortized 2× growth, `drop fn` frees the buffer)
  is unchanged.

### 2. Strings are the `u8` + UTF-8 refinement of the trio

`str ≈ [u8]`, `Str(N) ≈ [u8; N]`, `StrBuf ≈ ArrayBuf(u8)`, each carrying the
**byte-string invariant of ADR-0035** (conventionally UTF-8, not
guaranteed-valid; byte indexing `s[i] -> u8` and never-trapping access; the
"trap, don't corrupt" discipline applies at the *decode* boundary — `chars()`
traps on invalid UTF-8; `len` is byte length). Strings add nothing to the type
system beyond this refinement plus literal syntax.

### 3. Literals and the one hard case

- **Literal syntax** exists for both rungs' *values*: `[1, 2, 3]` and `"hello"`.
  Only *arrays* get bracket **type** syntax (`[T; N]`, `[T]`) because an array is
  a structural type *constructor*; a string is a single concrete named
  refinement, so it gets names (`str`, `Str`, `StrBuf`). Both having literal
  syntax while only the constructor gets type-bracket syntax is the norm across
  languages, not an inconsistency.
- **`"hello" : str`, a static-backed first-class value.** It is storable, `Copy`,
  and reassignable (`let s = "hello"; s = "hi";` compiles) — the ergonomic
  default. The cost is that `str` has two provenances: **static** (literals →
  first-class) and **local** (a `borrow` of a `StrBuf`/`Str(N)` → second-class,
  argument-only). The compiler tracks this as a single bit, not a lifetime
  system — the one place Rue's no-lifetime purity gets a smudge, taken
  deliberately for `&str`-grade ergonomics.
- **Array literals land on the *fixed* rung** (`[1,2,3] : [T; N]`) while string
  literals land on the *slice* rung (`"hello" : str`). This asymmetry is
  principled: **string-literal data is always compile-time-static** (→ a natural
  first-class static `str`), whereas **array-literal elements may be runtime
  values** (`[x, y, z]`, → a fixed stack value). A *const* array literal getting
  a first-class static `[T]` slice, symmetric with `str`, is a nice-to-have, not
  load-bearing.

### 4. Naming

The growable rung uses the `-Buf` suffix in both hierarchies: **`ArrayBuf`** and
**`StrBuf`**. This is chosen over the familiar `Vec`/`String` because the suffix
*teaches the trio* — `str`→`StrBuf` reads as obviously related in a way Rust's
`str`→`String` never does (a known wart). The recognized cost is losing `Vec` as
a Schelling point; it is paid once, for permanent internal coherence. Mixing axes
(`Vec` + `StrBuf`) is explicitly rejected as the worst option.

### 5. Slicing returns a view

`s[a..b]` yields a **`borrow str`** (a view), and `a[i..j]` a **`borrow [T]`** —
not an owned copy. This revises ADR-0035, which specified `s[a..b] -> String`
(an owned copy) because slices did not exist when it was written. Byte indexing
`s[i] -> u8` is unchanged.

## Rationale

- **One mental model, applied twice.** A reader learns fixed/slice/growable once
  and knows both collections and strings. That is a deeper simplicity than "one
  `String` type"; the "one type" instinct is satisfied by *consistency*, since
  three collection forms are already accepted.
- **De-blessing dissolves ADR-0042 at the root.** `String` stops being special
  because it stops being an owned-with-hidden-allocator primitive; it is the
  growable rung, `StrBuf`, exactly like `ArrayBuf`. The three-tier incoherence
  (blessed builtin / intrinsics / imported) collapses to "primitives (`str`,
  slices) + library types (`StrBuf`, `ArrayBuf`, `Option`)."
- **Second-class slices fit the spine.** A slice is precisely a scoped,
  non-escaping capability — what ADR-0037's model is for. Rust must reconstruct
  this with lifetimes; Rue gets it structurally.
- **Explicit allocators become clean and additive later**, not forced now: an
  allocator is a scoped capability you `borrow`, so `fn build(borrow a: Allocator,
  …)` threads through existing machinery when we want it (see Non-decisions).

## Relationship to prior ADRs

- **ADR-0042 (Standard-library availability model) — SUPERSEDED (in part).** This
  ADR resolves the string half of 0042's incoherence by de-blessing `String`:
  everything above the primitives is an ordinary library type. The *availability
  mechanism* it left open (a prelude vs. explicit `@import` for library types
  like `StrBuf`/`Option`) is **still open** and tracked separately (RUE-315 /
  RUE-287); this ADR does not settle it.
- **ADR-0035 (String model — byte strings) — AMENDED.** Its byte-string
  invariant (conventionally-UTF-8, byte-indexed, decode-boundary trap, `len` =
  bytes) is **carried forward unchanged** and now applies to all three string
  rungs. Amendments: `String` is renamed **`StrBuf`**; the type gains two
  siblings (`str`, `Str(N)`); and **slicing returns a borrowed `str` view**
  rather than an owned copy.
- **ADR-0041 (Vec) — AMENDED.** `Vec` is renamed **`ArrayBuf`** and reframed as
  the growable rung of the collection trio. Its mechanism (raw-pointer heap
  buffer, 2× growth, `drop fn` free — the last now real via RUE-312) is
  unchanged.
- **ADR-0020 (Built-in types as synthetic structs) — SUPERSEDED (for `String`).**
  `String` is no longer a blessed synthetic struct injected into scope; `str`
  and slices are genuine primitives, `StrBuf` is a library type. Whether the
  synthetic-struct injection mechanism retains any use is out of scope here.
- **ADR-0037 (Exclusivity / access-point model) — BUILDS ON.** Slices are new
  second-class access points; `borrow`/`inout` slice forms are the shared/
  exclusive loans of that model. No change to 0037.
- **ADR-0028 (Unchecked code / raw pointers) — BUILDS ON.** `StrBuf`/`ArrayBuf`
  are safe method APIs over `@alloc`/`@realloc`/`@free` in `checked {}` blocks,
  exactly as ADR-0041 established.

## Consequences

- **New surface to build:** the slice type (`borrow [T]` / `inout [T]`), range
  slicing syntax (`x[a..b]`), and the static-vs-local provenance bit on `str`.
  This is foundational and should be built as *one* piece so arrays, strings, and
  buffers share it rather than each inventing a view.
- **`str`/`String`-grade ergonomics without the tax:** no lifetimes, no `Cow`, no
  "take `&str` return `String`" puzzles, because views cannot escape.
- **The one smudge:** `str` carries a static/local provenance distinction — a
  single-bit, two-point echo of a lifetime. Accepted for literal ergonomics.
- **Renames land as breaking changes** (`Vec` → `ArrayBuf`, `String` → `StrBuf`)
  while the surface is still small — cheapest to do now.
- **Sequencing:** the slice/trio design is the irreversible part and is the same
  work whether or not explicit allocators ever ship; do it before traits.

## Non-decisions (explicitly deferred)

- **Fallible allocation.** Returning `Result` from allocating methods (OOM /
  bare-mode handling) is a *different* question (allocation-failure, not type
  structure) and would tax the common path with `?` everywhere. Deferred; if
  adopted it is an *additive* capability on the growable rung
  (`try_push`/bare-mode-restricted methods), never the default. `Str(N)` needs no
  allocator by construction.
- **Explicit allocators.** Zig-style allocator parameters are compatible with
  this design (an allocator is a `borrow`ed capability) and are deferred as a
  future, additive choice — best designed once traits/interfaces exist, since the
  comptime-param vs. runtime-interface representation fork depends on them.
- **The availability mechanism** (prelude vs. explicit import) for library types
  — left open by ADR-0042 and still open (RUE-315 / RUE-287).
- **Const array literals as static `[T]` slices** — a symmetry nice-to-have, not
  required.
