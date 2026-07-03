---
id: 0042
title: "Standard-library availability model (str/String split, prelude vs explicit std)"
status: proposed
tags: [stdlib, modules, strings, prelude, ergonomics, language-shape]
created: 2026-07-03
accepted:
implemented:
spec-sections: ["2 (lexical/literals)", "3.7 (String)", "10 (modules)"]
supersedes:
relates: ["ADR-0020", "ADR-0037", "RUE-315", "RUE-287", "RUE-6", "RUE-1"]
---

# ADR-0042: Standard-library availability model

> **Status: Proposed / for discussion (Steve + Dorian).** This ADR frames the
> problem, lays out the options with trade-offs, and records a recommendation.
> It does **not** ratify a decision. Tracking issue: RUE-315.

## Context

Writing the first real Rue program (`examples/first/stats.rue`, RUE-226) and
comparing it against Rust/Swift/Zig/Hylo exposed an incoherence: Rue currently
has **three different answers** to "how does a standard thing become usable in a
program."

1. **Blessed builtin types.** `String` is a synthetic struct the compiler
   injects before user code (ADR-0020). Always available; no import.
2. **Intrinsics.** `@parse_i64`, `@read_line`, `print`, `@to_string`,
   `@size_of`, … — always available, `@`-prefixed.
3. **Imported std-library types.** `Option`, `Result`, `Vec` live in `.rue`
   files and require an explicit `@import`.

So `String` is magic-available but `Option`/`Vec` are not; a program that reads
a line and parses an int gets `String` for free but must `@import` `Option` to
represent "no value." Extending error handling (RUE-6) by having `@parse_*`
return a *blessed* `Option` would add a **fourth** magic case, deepening the
split rather than resolving it.

The goal of this ADR is to pick **one uniform model** for standard-type
availability, rather than growing the ad-hoc tiers.

### The crux: string literals force an irreducible core

The reason `String` can't simply "become a library type like `Option`" is that
`"hello"` must have a type **before any import**. Something string-shaped is
irreducibly language-level. Every language draws this line somewhere:

- **Rust:** `&str` is a language primitive; `String`, `Option`, `Vec` are library
  types made ergonomic by an implicit **prelude**.
- **Zig:** string literals are `*const [N:0]u8` (a language primitive); everything
  else is explicitly `@import("std")`, no prelude.
- **Swift:** `String` is effectively built in; the whole stdlib is available with
  no imports.
- **C++:** literals are `const char*`; `std::string` is `#include`d.

For Rue, the natural core primitive is a **`str` slice** — a *borrowed* view a
literal produces. This is not a retrofit: a `str` is a **second-class borrow**
into static data (can't escape, can't be stored), which is exactly the access
model Rue already has (ADR-0037). The *owned, growable* `String` (ptr+len+cap,
heap, destructor) would then be a std-library type alongside `Option`/`Vec`.

"Back out the String change" (the phrasing that started this) ≈ **split `String`
into a `str` primitive (for literals) and a `String` std type (owned/growable)**,
so `str`(core) / `String`,`Option`,`Vec`(std) sit in coherent tiers.

## Decision drivers (the "various things to take into consideration")

- **Consistency** — the whole motivation: one tier, one rule, no per-type magic.
- **Explicitness** — Rue's stated value (explicit `@import`, no flat-namespace
  magic, privacy-everywhere: RUE-180/181). An implicit prelude is in tension
  with this; an explicit one-line `@import "std"` is not.
- **Ergonomics / ceremony** — without *some* convenience, "no prelude + String is
  a library type" means importing String/Option/Vec in nearly every file.
- **First impression / teaching** — what does hello-world look like? Does printing
  a string require an import?
- **`no_std` / freestanding** — Rue targets no-allocator scenarios. `String`/`Vec`
  need an allocator; `str`/`Option`/`Result` do not. Whatever the model, it must
  degrade cleanly to a freestanding subset (a prelude must be opt-out or layered).
- **comptime interaction** — `Option`/`Vec` are comptime *type functions*
  (`Option(T)`). Any prelude/bundle mechanism must inject/resolve comptime-generic
  names, not just monomorphic types.
- **Migration cost** — `String` is deeply wired as a builtin (lexer literals,
  sema injection, codegen, ABI, the spec's ch. 3.7/3.10, hundreds of tests).
  Splitting it into `str` + `String` is a large, careful change.
- **Intrinsic signatures** — `@parse_*`/`@read_line`/`print`/`@to_string` traffic
  in strings. If `String` moves to std, do these deal in `str` (core) or the std
  `String`? (Likely: they accept/produce `str`, and `String` derefs to `str`.)
- **ABI / representation** — `str` = (ptr, len) fat pointer; `String` =
  (ptr, len, cap) owned. Clarifies the current 2-word-vs-3-word confusion (see the
  spec-audit finding RUE-295).

## Options

### Option A — Everything blessed (uniform "up")

Make `Option`, `Result`, `Vec` compiler-injected builtins too (like `String`
today). The whole standard set is always available; no imports for std.

- **Pros:** uniform, zero ceremony, great first impression; smallest migration
  (extends the existing ADR-0020 injection).
- **Cons:** maximally *implicit* — the opposite of Rue's explicit stance; "the
  language and the stdlib are welded together"; `no_std` becomes "the compiler
  must know which builtins need an allocator"; keeps `String` magic (doesn't
  address the instinct to back that out). Injecting comptime-generic type
  functions as builtins is more machinery than a monomorphic `String`.

### Option B — `str` core + std library + **auto-prelude** (uniform "down", implicit)

Introduce a `str` primitive (literals); move `String`/`Option`/`Result`/`Vec` to
std; a fixed **prelude** set is implicitly in scope every file (Rust's model).

- **Pros:** uniform + ergonomic + clean core/std split; `str`-as-second-class-borrow
  fits the spine; matches the most popular model (Rust), so it's familiar.
- **Cons:** reintroduces the *implicit* availability Steve pushed back on
  (2026-07-03); "what's in the prelude" becomes a bikeshed + a compatibility
  surface; `no_std` needs a prelude-opt-out (`#![no_prelude]`-equivalent).

### Option C — `str` core + std library + **explicit `@import "std"` bundle** (uniform "down", explicit) — *recommended*

Same core/std split as B, but availability is a **single explicit import** per
file (`@import "std"` or `use std`) that brings the standard set into scope. No
implicit prelude.

- **Pros:** uniform (nothing magic above the `str` core); **explicit** (satisfies
  Rue's stance — you can always see where names come from); ergonomic (one line,
  not five); `no_std` is just "don't import std" (or import a `core` subset);
  answers RUE-6 with no new magic (`@parse_*` returns the imported std
  `Option`/`Result`); closest to Zig, which Rue already resembles on comptime.
- **Cons:** one line of ceremony per file (mild); requires the module system to
  resolve a `std` bundle name (a small addition); still a large migration to split
  `String`.

### Option D — `str` core + std library + **per-type explicit imports** (uniform "down", no bundle)

Same split; no bundle — you `@import` each std type you use.

- **Pros:** maximally explicit + uniform; simplest mechanism (no bundle concept).
- **Cons:** real ceremony (`@import` String, Option, Vec, … every file); the
  friction the first program already hit, made the permanent state.

### Option E — Status quo (three tiers)

Keep `String` blessed, `Option`/`Vec` imported, intrinsics magic. Do nothing.

- **Pros:** zero migration.
- **Cons:** the incoherence that motivated this ADR; every new std type forces an
  ad-hoc "blessed or imported?" call; `@parse`-returns-Option adds a fourth tier.

## Recommendation

**Option C** — the `str`/`String` split + a single explicit `@import "std"`
bundle. It resolves the incoherence uniformly, keeps Rue explicit (its stated
value), stays ergonomic, degrades cleanly to `no_std`, and the `str`-as-a-
second-class-borrow primitive is a genuine spine fit rather than a bolt-on. It
also gives RUE-6 a clean answer (`@parse_*` returns the imported std
`Option`/`Result`, no new magic).

The main cost is the `String` → `str`+`String` migration; that is real but
independently valuable (it also fixes the 2-word-vs-3-word `String`
representation confusion, RUE-295, and clarifies intrinsic signatures).

If the one-import-per-file ceremony proves annoying in practice, Option B
(auto-prelude) is a small, reversible delta *on top of* C — so C-now,
B-if-needed-later is a low-risk path that doesn't foreclose either.

## Open questions (for the Steve + Dorian discussion)

1. **Prelude vs explicit bundle** (B vs C) — the core ergonomics/explicitness call.
2. **Is `str` a distinct primitive**, or does the literal produce an owned
   `String` from a minimal always-present `core` (blurring A and C)?
3. **`no_std` layering** — is there a `core` (str/Option/Result, no allocator)
   separate from `std` (String/Vec, allocator)?
4. **Intrinsic signatures** — do `@parse_*`/`print`/`@to_string` speak `str` or
   `String`? (Proposed: `str` in, with `String` deref-to-`str`.)
5. **Migration staging** — introduce `str` first (literals unchanged in behavior),
   then move `String` to std, then wire the bundle? Or all at once behind a flag?

## Consequences

- Reopens ADR-0020 (builtin-types-as-structs) for `String` specifically; `Vec`
  and future collections stay library types either way.
- Unblocks a coherent RUE-6 (error handling) design.
- Large but stageable migration; the `str` primitive is the keystone and the
  riskiest single step (touches the lexer, sema, codegen, ABI, and ch. 2/3.7 of
  the spec).
