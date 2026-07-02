---
id: 0036
title: "Behavior classification preference: prefer the most-defined category"
status: accepted
tags: [spec, conformance, safety, principle]
feature-flag:
created: 2026-07-02
accepted: 2026-07-02
implemented:
spec-sections: ["1.3"]
superseded-by:
---

# ADR-0036: Behavior classification preference

## Status

Accepted. Ratified by Steve on 2026-07-02. This is a **design preference / spec-
authoring principle**, not a normative rule of the specification — it guides how
*we* classify a new behavior when extending the spec, not what a conforming
implementation must do. It is recorded as its own ADR (per the "ADR-per-
principle" decision, RUE-201) so it can be cited individually and repealed
individually via `superseded-by:` if Rue's taste changes.

## Summary

When the specification assigns a program behavior to one of the four conformance
categories (undefined / unspecified / implementation-defined / erroneous — see
spec 1.3), Rue **prefers the most-defined category available**, and **confines
undefined behavior to `unchecked` operations for which a check is genuinely
impractical** (would require changing a value's representation, e.g. fat
pointers, or would otherwise defeat the purpose of `unchecked`). This is a
preference that admits case-by-case judgment, not an absolute rule.

## Context

Rue's whole personality is "trap, don't corrupt, and never invoke undefined
behavior in safe code": integer overflow traps, out-of-bounds indexing traps,
a live-unreachable block traps, a moved value is a compile error. The
conformance taxonomy (spec 1.3, adopted from C/C++/Rust) gives four homes for
behavior, and the question is *how opinionated Rue is about pushing behaviors
toward the defined end*.

The tension is that the cost of "define/trap instead of leaving undefined" is not
uniform:

- Some hazards are checkable for **free** — a null-pointer dereference already
  faults in hardware; it can be defined-to-trap at no cost.
- Some are **not checkable without changing the representation** — an
  out-of-bounds raw-pointer access has no bounds to check against unless raw
  pointers become fat pointers, which defeats the point of `unchecked`. These are
  genuinely undefined.

So a blanket "always trap in `unchecked`" rule would be wrong (it would gut
`unchecked`), and a blanket "`unchecked` is C-style UB" rule would be more lax
than Rue's character warrants. The preference below captures the intent without
over-committing to either extreme.

## Decision

**Prefer the most-defined category available; confine undefined behavior to
`unchecked` operations where a check is genuinely impractical.**

Concretely, when classifying a behavior:

1. If the result can be fully specified, it is **defined** (state it).
2. If it is a checkable error, prefer to **trap** it as a defined panic (this is
   why overflow/bounds/div-zero are *defined*, not erroneous) — or, where a
   defined-but-wrong result plus a recommended diagnosis is the better fit, mark
   it **erroneous**.
3. Reserve **undefined** for `unchecked` operations whose validity cannot be
   checked without changing a type's representation or otherwise defeating the
   purpose of `unchecked` (out-of-bounds / dangling / type-punned raw-pointer
   access).
4. Free or already-happening checks (e.g. a hardware trap) should be *defined*,
   not undefined, even inside `unchecked`.

The per-`unchecked`-operation classifications are worked out as chapter 9 (raw
pointers / unchecked code) is written; this ADR is the lens applied there.

## Consequences

### Positive
- Keeps Rue's "no surprises" character consistent all the way to the unsafe
  boundary: you get the fast path, and where you're wrong you find out (trap),
  except where finding out is genuinely impossible.
- Gives a single, citable rule for classifying future behaviors — the rail a
  contributor (or a lesser model) follows so the taxonomy stays consistent.

### Negative
- It is a *preference*, so it still requires judgment per behavior (deliberately —
  the alternative absolutes are both wrong).
- Some `unchecked` operations remain genuinely undefined; Rue does not promise to
  trap everything in `unchecked`.

## Alternatives Considered

- **"Always trap in `unchecked`."** Rejected: would require fat pointers /
  representation changes and defeat the purpose of `unchecked`.
- **"`unchecked` is C-style undefined behavior, full stop."** Rejected as the
  *default lean*: more lax than Rue's character; free/cheap checks should be
  defined.
- **No stated preference (classify ad-hoc).** Rejected: without a written lens,
  classifications drift and become inconsistent — exactly the kind of leak the
  spec-quality rebuild (RUE-201) is closing.
