---
id: 0038
title: "Error handling: sum types, Result/Option, and must-check via linearity"
status: accepted
tags: [error-handling, enums, sum-types, linearity, pattern-matching]
feature-flag:
created: 2026-07-02
accepted: 2026-07-02
implemented: 2026-07-03
spec-sections: ["3.5", "4.7", "6.3"]
superseded-by:
---

# ADR-0038: Error handling — sum types, Result/Option, must-check via linearity

## Status

Accepted. Ratified by Steve on 2026-07-02. Resolves RUE-6. Depends on enum
payloads (a prerequisite feature, tracked as a phase of RUE-6) and makes RUE-187
(intentional-destroy escape hatch) a required companion. Builds on ADR-0037
(access model), ADR-0013 (borrowing modes), and the multiplicity lattice
(docs/formal §3), and reuses ADR-0020 (comptime type functions).

**Stabilized (2026-07-03):** the `enum_payloads` preview feature — tuple-variant
payloads, tagged-union layout, and match-with-payload-bindings — is complete and
was stabilized (its `--preview enum_payloads` gate removed). Payload-carrying
enums take their multiplicity from the join of their variants' payloads
(spec 6.3:19/6.3:20), and the pre-payload blanket ownership rules were narrowed
to discriminant-only enums (RUE-294).

## Summary

Rue gets error handling with no new ownership machinery — it composes parts that
already exist:

1. **Enum payloads** (sum types) with **both** tuple variants (`Some(T)`) and
   struct variants (`Err { code: i32 }`).
2. **`Option`/`Result` are ordinary comptime-generic library enums**, not
   builtins — `fn Result(comptime T: type, comptime E: type) -> type { enum { Ok(T), Err(E) } }`.
3. **Pattern-match bindings inherit the scrutinee's access mode** (match
   ergonomics): `match e` consumes, `match borrow e` / `match inout e` bind by
   borrow / inout. No `ref`/`ref mut`, no lifetimes.
4. **`Result` is a *linear* type → an unhandled `Result` is a compile error**
   (must-check via linearity), strictly stronger than Rust's silence-able
   `#[must_use]` lint. `Option` is **not** must-use (it follows its payload's
   multiplicity).
5. **`?`** desugars to a match with early return; same-constructor propagation
   only (no `Try` trait); exact error type until traits exist.

## Context

Rue had no error-handling story: enums carried no data, so `Option`/`Result`
could not be expressed, and fallible intrinsics panicked or returned sentinels.
The design forces enum payloads, a generics mechanism, and propagation
ergonomics — and ADR-0008 already named "must-check results" as a motivating
use-case for linear types. The recurring result of this design pass: the parts
that *look* novel (payload ownership, must-use) are corollaries of the access
model (ADR-0037) and the multiplicity lattice, not new machinery.

## Decision

### 1. Enum payloads — both variant forms

Variants may carry data as **tuple variants** (`Some(T)`, `Err(E)`) or **struct
variants** (`Line { start: P, end: P }`). Layout is a **tagged union**: a
discriminant plus payload space sized to the largest variant. Layout is
**implementation-defined** (per the conformance taxonomy, spec 1.3:6), so niche
optimizations (e.g. `Option<ptr>` using the null representation) are permitted
later without a spec change.

### 2. Match bindings inherit the scrutinee's access mode

The scrutinee obeys the ordinary **"use" definition** (docs/formal §4.2), and
bindings inherit its mode — this is the faithful analog of Rust's 2018 match
ergonomics:

- `match e { Some(x) => … }` — bare match is a use of `e` in value context:
  copies if `e` is Copy, **moves** if Affine/Linear; bindings move out.
- `match borrow e { Some(x) => … }` — `x` binds by **borrow**, `e` intact.
- `match inout e { Some(x) => … }` — `x` binds by **inout**, mutate in place.

Binding mode is **uniform** (all bindings inherit the scrutinee mode; no
per-binding overrides initially) and you cannot move a field out of a
`borrow`-matched scrutinee. This is *cleaner than Rust*: the mode is stated once,
explicitly, on the scrutinee, so there is no inferred "default binding mode"
state machine (the part Rust had to revise in its 2024 edition).

### 3. Enum multiplicity and drop

An enum's multiplicity is the **join** of its variants' payload multiplicities
(the lattice's infectiousness rule): a `Linear` payload makes the enum `Linear`.
Dropping an enum drops the **active** variant's payload (the discriminant selects
which). A linear enum must be exhaustively consumed. No new rules — the
multiplicity lattice applied to a tagged union.

### 4. Option/Result as comptime-generic library enums

`Option` and `Result` are **library types**, defined with the comptime
type-function mechanism already used for anonymous structs (ADR-0020), not
privileged builtins:

```rue
fn Option(comptime T: type) -> type { enum { Some(T), None } }
fn Result(comptime T: type, comptime E: type) -> type { enum { Ok(T), Err(E) } }
```

Sum types are thus first-class for users, not a compiler privilege.

### 5. `Result` is linear (must-check); `Option` is not

The library defines **`Result` as a linear enum**. Ignoring a `Result` is then
an **E0406-family compile error** — the existing unconsumed-linear check — not a
lint. Unhandled errors become impossible, not merely discouraged. A linear
`Result` is satisfied by exactly the three ways to consume it:

1. **`match`** it (handle),
2. **`?`** it (propagate),
3. **explicitly discard** it (deliberate ignore) — this is **RUE-187**'s
   intentional-destroy escape hatch, which is therefore *required* by this ADR so
   that ignoring an error is possible but always *visible*.

**`Option` is not must-use**: it follows its payload's multiplicity (Copy payload
→ Copy `Option`), because a missing value is not the safety event an unhandled
error is. (A weak advisory lint remains available later.)

### 6. The `?` operator

`r?` desugars to `match r { Ok(v) => v, Err(e) => return Err(e) }`. It **consumes
`r`** (so it satisfies the linear-`Result` obligation) and reconstitutes the
enclosing function's `Result` (threading linearity to the caller). Constraints,
both from having no traits yet:

- **Same-constructor propagation only** — `Result`-`?` in a `Result`-returning
  function, `Option`-`?` in an `Option`-returning one. No `Try`-trait
  unification.
- **Exact error type** — Rust's `?` widens via `From`; without traits, the
  propagated error type must equal the enclosing function's. Error-type widening
  is deferred until traits exist (tracked as a follow-up).

## Consequences

### Positive
- Unhandled errors are **compile errors**, not silence-able lints — a concrete
  improvement over Rust, achieved by reusing linearity.
- No new concepts: payload ownership is the access model, must-use is linearity,
  generics are comptime type functions, match bindings are mode inheritance.
- Sum types (and `Option`/`Result`) are user-definable, not privileged.
- Match ergonomics without Rust's default-binding-mode sharp edges.

### Negative
- Every `Result` must be consumed (`match`/`?`/discard) — intended, but stricter
  than a lint; makes RUE-187 a required companion.
- No error-type widening in `?` until traits land.
- Enum payloads are a sizable prerequisite feature (parser, RIR/AIR, layout,
  match-with-bindings, drop under the affine model).

## Alternatives Considered

- **`Result` as a `#[must_use]` lint (Rust).** Rejected: weaker; silence-able.
  Linearity gives a hard guarantee for free.
- **`Option` also must-use.** Rejected: a missing value is not an unhandled
  error; forcing it adds friction without a safety payoff.
- **Builtin `Option`/`Result` (ADR-0020 style, like `String`).** Rejected in
  favor of comptime library enums, keeping sum types first-class for users.
- **`Try`-trait `?` unification / `From` widening now.** Deferred: both need
  traits, which Rue does not have yet.

## Implementation phases (RUE-6)

1. **Enum payloads** (sum types) — the prerequisite: tuple + struct variants,
   tagged-union layout, match-with-bindings (mode inheritance), multiplicity
   join + drop of the active variant. Gated behind `enum_payloads` preview.
2. **`Option`/`Result` library types** — once (1) + comptime-generic enums work;
   define `Result` linear.
3. **`?` operator** — same-constructor, exact-error-type.
4. **Companion:** RUE-187 (intentional-destroy) — required for (2).
5. **Deferred (needs traits):** `?` error-type widening / a `Try`-like
   abstraction.
