---
id: 0039
title: "`@drop`: the intentional-destroy intrinsic for linear (and affine) values"
status: accepted
tags: [linearity, ownership, intrinsics, destructors]
feature-flag:
created: 2026-07-02
accepted: 2026-07-02
implemented:
spec-sections: ["3.9"]
superseded-by:
---

# ADR-0039: `@drop` — intentional-destroy intrinsic

## Status

Accepted. Ratified by Steve on 2026-07-02. Resolves RUE-187. Required companion
to ADR-0038 (a linear `Result` is discharged by `match` / `?` / explicit
discard — this is the discard). Builds on the multiplicity model (docs/formal
§3) and the destructor-self-consume exemption added in #1002.

## Summary

`@drop(x)` is a compiler intrinsic that runs `x`'s drop glue (its destructor
plus the recursive field/variant drops) and **discharges `x`'s consumption
obligation**. It works on affine values (early, deterministic cleanup) and on
linear values (early cleanup *and* satisfying the must-consume rule); on a Copy
value it is a no-op. It is memory-safe (no `unchecked` required). After `@drop(x)`,
`x` is consumed — reusing it is E0205.

## Context

#1002 tightened linear enforcement: unconsumed linear parameters, discarded
linear temporaries, and bare arrays of linear values are all rejected
(E0406/E0478). That tightness creates the need for a deliberate escape hatch —
"yes, destroy this, I mean it" — that does not go through the value's normal
consuming protocol. ADR-0038 sharpens the need: a linear `Result` must be
`match`ed, `?`-propagated, or explicitly discarded, and the discard needs
syntax.

The decisive technical point resolves the "intrinsic vs library convention"
question in the issue: **a library consuming function cannot destroy a linear
value.** Passing a linear value to `fn sink(comptime T: type, x: T) {}`
discharges the *caller's* obligation but leaves `sink` with an unconsumed linear
parameter — `sink` itself fails E0406. The library function only *relocates* the
obligation. The only thing that can actually destroy a linear value is the
**destructor blessing** (#1002's `is_destructor` exemption — "the one blessed
place a linear value dies"). Therefore the escape hatch must be a primitive that
carries that blessing: an intrinsic, not a library function.

## Decision

- **`@drop(x)` is a compiler intrinsic** that runs `x`'s full drop glue
  (destructor + recursive field/variant drops) and discharges its consumption
  obligation. It is "the drop that `linear` suppresses, invoked by hand" — the
  same mechanism as affine scope-end drop and destructor invocation, triggered
  explicitly.
- **It applies to any droppable value:**
  - *linear* — runs the glue and satisfies the must-consume rule;
  - *affine* — runs the glue early (deterministic cleanup before scope end, e.g.
    releasing a lock);
  - *Copy* — no-op (may warn as pointless).
- **Destructor-carrying types are allowed through it.** Running a destructor is
  always safe and well-defined, and the destructor is the type's sanctioned
  *default* cleanup. A type needing a non-default path (e.g. commit vs rollback)
  exposes explicit protocol methods (`txn.commit()`); `@drop` invokes the
  default. The hatch is not restricted to payload-free "pure linear" markers.
- **Safe.** Drop glue is memory-safe, so `@drop` requires no `unchecked` context.
- **Visible.** The `@drop(x)` marker at the discard site is the visibility the
  design wants — ignoring a value is possible but never silent.

### Deferred: `@forget` / `@leak`

Consuming a value *without* running its destructor — an intentional resource leak
(Rust's `mem::forget`) — is memory-safe but rarer and more dangerous. It is **not**
part of this ADR; add a distinct, more-alarming `@forget`/`@leak` intrinsic when a
concrete need (e.g. FFI ownership transfer) appears. `@drop` is the escape hatch
needed now.

## Consequences

### Positive
- A single, uniform discharge mechanism that reuses existing drop glue — no new
  runtime concept.
- Serves affine early-drop too (deterministic cleanup), so it is generally
  useful, not a linear-only oddity.
- Satisfies ADR-0038's "explicit discard" of a linear `Result`.
- The obligation-discharge is syntactically visible.

### Negative
- A linear resource can be `@drop`ped via its default destructor even where a
  specific protocol path was intended — mitigated by exposing explicit protocol
  methods for non-default cleanup.
- No intentional-leak capability until a separate `@forget`/`@leak` is added.

## Alternatives Considered

- **Library consuming function (documentation-only hatch).** Rejected: cannot
  destroy a linear value — it relocates the obligation and fails E0406 internally.
  Only the destructor blessing can destroy a linear value.
- **Per-type explicit destructor calls only (Austral-style).** Rejected as less
  ergonomic: a generic `@drop` covers `Result` and other composite linear values
  without each type hand-rolling a destroy entry point.
- **Include `@forget` now.** Deferred — rarer, more dangerous, no concrete need
  yet.
