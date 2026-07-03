---
id: 0037
title: "Exclusivity model: access-point-based, statically enforced (Hylo-style)"
status: accepted
tags: [ownership, exclusivity, borrows, semantics, principle]
feature-flag:
created: 2026-07-02
accepted: 2026-07-02
implemented:
spec-sections: ["3.8"]
superseded-by:
---

# ADR-0037: Exclusivity model — access-point-based, statically enforced

## Status

Accepted. Ratified by Steve on 2026-07-02. This is a foundational semantics
decision (it governs the whole law of exclusivity, not one feature), recorded as
its own ADR so it can be cited and repealed individually. It resolves the
load-bearing question in RUE-15 (method receivers) and states the global
discipline that complements ADR-0013 (borrowing modes) and formalization
decision 4 (loans strictly second-class, no lifetimes).

**Stabilized (2026-07-03):** the two preview features that rode on this access
model — borrow/inout method receivers (`method_receivers`, RUE-15) and built-in
`for` loops iterating in read/borrow mode (`for_loops`, RUE-220) — are complete
and were stabilized (their `--preview` gates removed).

## Summary

Rue's law of exclusivity is **access-point-based**, not span/region-based: an
access to a place has a *duration* (a read completes during expression
evaluation; an `inout`/`borrow` access lasts for the call or scope it is passed
to), and two accesses to the same object may not *overlap in time* if at least
one is a write. It is enforced **fully statically** — no runtime exclusivity
checks — which is possible because Rue's loans are strictly second-class and
**non-escaping** (a loan never outlives the call it is born in), so the compiler
can always see both ends of any potential overlap. A direct consequence:
patterns like `v.push(v.len())` are **allowed for free** (the read for the
argument finishes before the mutating access for the receiver begins), with none
of the two-phase-borrow machinery a span-based model requires.

## Context

Extending borrow/inout to method receivers (RUE-15) forced the underlying
question: when a receiver is mutably accessed while an argument reads the same
object — `v.push(v.len())` — is that a conflict? The answer depends on which
exclusivity model Rue uses.

- **Span/region-based (Rust).** A borrow is a *value* whose lifetime is a region
  of the program; exclusivity is checked over overlapping regions. `&mut v` for
  `push` is created before the arguments are evaluated, so `v.push(v.len())`
  overlaps and was rejected — Rust had to add **two-phase borrows** (2018) purely
  to permit it. This model requires lifetimes/regions.
- **Access-point-based (Swift, Hylo).** A borrow is a *scoped access*, not a
  value. A read access is instantaneous (produces its value and ends); an
  `inout` access lasts for the call. `v.append(v.count)` is fine because the read
  ends before the mutation begins — no overlap, no special machinery.

Two facts make the choice for Rue:

1. **Rue already chose second-class loans and no lifetimes** (formalization
   decision 4). Span-based exclusivity *is* the lifetime machinery — choosing it
   would reopen that decision and reintroduce a borrow checker. Access-point-based
   is the only model consistent with what is already settled.
2. **Hylo is the precedent, and it is exactly this design.** Hylo (Dave Abrahams,
   Dimitri Racordon) is a systems language whose parameter conventions
   `let`/`inout`/`sink`/`set` are Rue's `borrow`/`inout`/by-value(consume)/init
   modes, with **no lifetimes, no borrow checker, no first-class references**, and
   the law of exclusivity enforced **fully statically**. Its "mutable value
   semantics" is formalized with a published memory-safety + data-race-freedom
   result. Hylo takes `v.push(v.len())` for free and stays fully static — the
   existence proof for this model.

Swift's one wart — falling back to **runtime** exclusivity checks for cases it
cannot prove statically — comes entirely from *escaping* references (inout
aliasing through computed indices, escaping closures, globals, class properties).
Rue's non-escaping second-class loans preclude those, so Rue does **not** inherit
the runtime tax; it enforces the whole law statically.

## Decision

Adopt **access-point-based exclusivity, enforced fully statically.**

- An access to a place has a duration: a **read** is completed during evaluation
  of the expression that produces its value; an **`inout`** (exclusive) or
  **`borrow`** (shared) access lasts for the duration of the call or scope it is
  passed to.
- **Law of exclusivity:** two accesses to the same object may not overlap in time
  if at least one is a write (`inout`/mutation). Multiple simultaneous shared
  reads/`borrow`s are fine.
- **Allowed** (the whole point): `v.push(v.len())` and friends — an argument that
  merely *reads* the receiver, because the read access ends before the receiver's
  `inout` access begins.
- **Rejected** — genuine overlap: passing the receiver itself as an
  `inout`/`borrow` argument to its own `inout` method (`s.push(inout s)`); or an
  argument that yields a *live projection* into the object that is still open when
  the mutating access starts.
- Enforcement is **static**; there are no runtime exclusivity checks. This relies
  on loans being second-class and non-escaping (ADR-0013 / formalization
  decision 4).

## Consequences

### Positive
- Common patterns (`v.push(v.len())`, read-an-element-while-mutating-another via
  values) are ergonomic by default, with no two-phase-borrow special case.
- Fully static: no runtime exclusivity checks, no runtime traps for aliasing.
- Consistent with the already-ratified second-class-loans / no-lifetimes design;
  no borrow checker, no lifetime annotations.
- Well-precedented and formalizable — Hylo's mutable-value-semantics metatheory
  is a direct template for `docs/formal`.

### Negative
- **No first-class borrows.** A borrow cannot be stored in a struct, returned, or
  used to build reference-laden or self-referential data structures. This is the
  already-accepted cost of no lifetimes, not a new one. Patterns needing
  long-lived borrows use value semantics + `inout` instead.
- If Rue later needs container-projection ergonomics (`get_mut`-style access into
  a collection), it must adopt a lifetime-free mechanism for it — Hylo's
  **subscripts that `yield` a projection** (coroutine-style scoped borrow-return)
  are the precedent. Out of scope here; noted as the escape hatch.

## Alternatives Considered

- **Span/region-based (Rust + two-phase borrows).** Rejected: requires
  lifetimes/regions, contradicting the settled no-lifetimes decision, and needs
  dedicated machinery (two-phase borrows) just to permit `v.push(v.len())`.
- **Access-based with Swift-style runtime fallback.** Rejected: the runtime
  checks exist only to cover escaping references, which Rue's non-escaping loans
  rule out. Rue enforces statically instead — same ergonomics, no runtime tax.
