---
id: 0062
title: "Place-returning borrow accessors: projection reads of owned elements"
status: accepted
tags: [ownership, borrows, collections, accessors, stdlib, formal-semantics]
feature-flag: borrow_accessors
created: 2026-07-18
accepted: 2026-07-18
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-662", "RUE-286", "RUE-651", "RUE-646", "RUE-1012", "RUE-1013", "RUE-219", "RUE-390", "ADR-0037", "ADR-0043"]
---

# ADR-0062: Place-returning borrow accessors — projection reads of owned elements

## Status

Accepted — ratified by Steve on 2026-07-18. Direction and syntax were both
decided in the 2026-07-18 design session: build the restricted
place-returning form now, with the **S2 surface** (`fn` + `-> borrow T` +
`yield` body — see §"The syntax decision"), conditional on (a) it remaining
forward-compatible with full coroutine accessors (RUE-1012 tracks that
general form) and (b) nothing shipping that the formal §7 lemmas cannot be
extended to cover. RUE-662 is the phase 0/1 implementing issue.

## Summary

Rue gains **accessors that yield a second-class borrow of a projection of
their receiver**: `v.get_ref(i)` produces a borrow of element `i` in place —
no copy, no move-out — checked by the existing law-of-exclusivity loan
machinery and scoped to the enclosing full expression. The accessor body is
restricted to **pure projections**: every non-diverging path must yield a
place derived from the receiver, so the construct compiles by inlining to
address computation, introducing **no new lowering shape, no coroutine, and
no ABI**. This closes the "cannot read an owned element without moving it
out" gap (RUE-662) that dogfooding showed forces every tree-shaped Rue
program into flat-arena/ECS form (RUE-286), while preserving the two
load-bearing model decisions it collides with: second-class borrows
(ADR-0037) and no-move-out-of-collections.

## Context

### The capability gap, with dogfood evidence

RUE-651 closed a real soundness hole: `ArrayBuf(T).get` performed a by-copy
`@ptr_read`, so reading a drop-glue element created an aliasing second owner
of that element's buffer — a double-free. The sound stopgap (E0711) rejects
by-copy reads of drop-glue elements entirely: owned elements can be
`push`ed, `pop`ped, and dropped, but never *read in place*.

The dogfooding campaign then measured what that costs. Per the running
record on RUE-286:

- `struct Node { kids: ArrayBuf(Node) }` compiles, but the tree can only be
  consumed (`pop`), never traversed — an owned-children AST is impossible.
- rueparse (1,898 lines) and the RUE-942 stage-1 frontend (1,890 lines) were
  both independently forced into flat POD `ArrayBuf(Node)` arenas with `u64`
  child indices and `(start, len)` runs in shared pools.
- Running conclusion from the maintainer's own notes: the E0711 gate is "the
  defining architectural constraint for tree-shaped programs"; "every
  non-trivial data structure in Rue today is written the ECS way, not the
  ADT way."

The early recommendations on RUE-286 ("keep the model, solve it with library
APIs") predate this evidence and are refuted by it: no library API can be
designed around the inability to *return* a borrow — that is a language
capability.

### Why the fix is cheap now

Rust's answer (`Vec::get -> Option<&T>`) needs first-class references and
lifetimes — rejected by ADR-0037, whose second-class borrows buy Rue the
no-lifetimes model. Hylo and Swift solve the same problem differently:
subscripts/accessors that **yield a projection** scoped to the accessing
expression. That projection is *already* what a Rue second-class borrow is;
the missing piece is only the construct for a method to hand one out.

Two things landed in 2026-07 that make the semantics essentially free:

- **The allocation store** (RUE-390, `docs/formal/01-core-calculus.md`
  §6.13): a yielded element borrow *is* `view⟨A | i, 1⟩` over the store, and
  the §7 lemmas the design leans on — view-intact, loan-extent nesting,
  handle-uniqueness — are already stated.
- **By-ref place plumbing**: the machine already passes projected places
  into calls as `(ℓ, π)` bindings (§6.9). A place-returning accessor is the
  same value flowing *out* of a call instead of in.

## Decision

### 1. The construct

An **accessor** is a method whose result is a second-class borrow of a
projection of its borrowed receiver. Phase 1 is read-only:

```rue
// Phase 1 (shared):    receiver borrow self  →  shared result borrow
// Phase 2 (exclusive): receiver inout self  →  exclusive result inout
fn get_ref(borrow self, i: u64) -> borrow T { … }
```

(The concrete surface is S2 — the `yield`-body form of §"The syntax
decision". The semantics below are syntax-independent.)

**Body restriction (what "place-returning" means).** Every non-diverging
exit of the accessor body must yield a **place derived from the receiver
parameter**: a projection chain (`self.f`, `self.kids[i]`, a nested
place-returning accessor call), guarded by arbitrary *checking* code that
either diverges (trap, `@panic`) or falls through to the yield. No code may
run after the yield — this form has an empty post-yield continuation, which
is precisely what makes it the degenerate case of a coroutine accessor
(RUE-1012) and what makes it compile to nothing.

Two tiers satisfy the restriction:

- **User-code accessors**: the yielded place is a syntactic projection of
  `self`. Checked entirely by the ordinary place/loan rules.
- **Library accessors** (the `ArrayBuf.get_ref` case): the yielded place is
  computed in a `checked {}` block (`@ptr_offset(self.buf, i)` after a
  bounds check). Outside the core by design; carries the §6.13.5 refinement
  obligations (O1–O4), with the defining equation
  `get_ref(borrow self, i): i < len → yield view⟨A | i, 1⟩; else ↯bounds`.

### 2. Loan semantics

At a call site `v.get_ref(i)`:

- The result is a **second-class borrow**. It may be used in place contexts
  — read in an expression, projected further (`v.get_ref(i).name`), passed
  as a `borrow` argument, compared with `==` — and in phase 2, assigned
  through. It may **not** be returned, stored, bound by a plain `let`,
  captured, or otherwise escape.
- Its **loan root is `root(v)`** and its mode is the receiver's mode
  (shared for `borrow self`). The loan joins `Λ` for the **enclosing full
  expression** — the same extent discipline as the equality-compare loan of
  §5.4, generalized. Root-granularity is unchanged by this ADR (RUE-997's
  path-granular refinement composes with it independently: once loans carry
  paths, an accessor result's loan carries the yielded projection path).
- Calling an accessor requires of the receiver exactly what passing it as a
  `borrow`/`inout` argument requires today (`fully-owned`, mutability for
  phase 2, consistency in `Λ_call`).

This closes the dangling cases structurally. `use(v.get_ref(i), g(inout v))`
is rejected by exclusivity — the shared loan on `v`'s root conflicts with
the `inout` loan — which is the same both-sides closure the formal §6.13.3
notes describe for realloc: the loan rules forbid the shape whose dynamic
counterpart the allocation store makes stuck.

### 3. Lowering: inlining, no ABI

Accessors are **required-inlineable**. A call `v.get_ref(i)` compiles by
inlining the body at the call site: the guards run (and may trap), and the
yielded place becomes an ordinary projected place / address computation in
the caller — for `ArrayBuf`, a bounds check plus `self.buf + i·size(T)`,
exactly what a trusting caller would write by hand. **No calling convention
for "returning a place" is introduced.** This is the load-bearing
forward-compatibility commitment: when RUE-1012 lifts the body restriction,
coroutine accessors can adopt whatever two-part call shape they need
without any shipped ABI constraining them, because place-returning
accessors never had one.

(Whether the implementation literally inlines pre-CFG or reuses the
ADR-0049 splice machinery at a mandatory threshold is an implementation
choice inside phase 1, not a semantic commitment.)

### 4. What this deliberately does not add

- **No `Option(borrow T)`**: an option *containing* a loan is a stored
  borrow; it stays impossible. The non-trapping peek idiom is
  bounds-check-then-access — which this ADR makes sufficient, because the
  access now yields a borrow rather than forcing a copy or move.
- **No relaxation of ADR-0037**: nothing here lets a loan outlive its
  originating expression. The no-lifetimes and no-`Pin` stories are intact.
- **No coroutine accessors** — deferred to RUE-1012, with the
  forward-compatibility contract recorded there and in §3 above.
- **No `for`-loop integration** — the iteration model (RUE-219) may later
  specify itself over accessors; nothing here presumes it.

### 5. The E0711 gate stays; the idioms change

By-copy `get`/`get_or` remain rejected for drop-glue `T` (the RUE-651
soundness argument is unchanged). `get_ref` becomes the sanctioned read
path; `pop` remains the sanctioned ownership-transfer path. The std phase
(below) lifts the arena forcing: owned-children trees become traversable
(`node.kids.get_ref(i)`), nested grids peekable (`grid.get_ref(i).get_ref(j)`).

## The syntax decision (decided: S2)

Three candidate surfaces were weighed; **S2 was chosen** (Steve, 2026-07-18):

**S1 — return-position marker, expression body.**
```rue
fn get_ref(borrow self, i: u64) -> borrow T {
    if i >= self.len { @panic("index out of bounds"); }
    checked { @place(@ptr_offset(self.buf, i)) }
}
```
Ordinary `fn`, ordinary body; the `-> borrow T` return type is what marks
it. Most familiar; smallest parser change. Risk: the body reads as
"returning a value", and when RUE-1012 arrives, "code after `return`" is
inexpressible — coroutine accessors would need a different body form
anyway, weakening the one-construct story.

**S2 — `-> borrow T` signature plus `yield` body (chosen).**
```rue
fn get_ref(borrow self, i: u64) -> borrow T {
    if i >= self.len { @panic("index out of bounds"); }
    yield checked { @place(@ptr_offset(self.buf, i)) };
}
```
Same signature as S1, but the body hands out the projection with `yield`,
restricted today to "every non-diverging path ends in exactly one `yield`
of a place." Reads as what it is (a projection handed out, not a value
returned); RUE-1012 then becomes *purely* "lift the restriction on where
`yield` may appear and what may follow it" — same declaration, same
signature, same checker rule. Cost: one new keyword/body form now.

**S3 — distinct accessor/subscript declaration form (Hylo/Swift-shaped).**
```rue
subscript(borrow self, i: u64) -> borrow T {
    …; yield …;
}
```
Reserves the most future room (named accessors, `v[i]`-integration, read
and modify blocks under one declaration) at the cost of a whole new
declaration form and a bigger grammar/spec footprint — likely more than
phase 1 needs, and `v[i]` sugar can be layered on S1/S2 later.

**Why S2 won.** It buys the honest reading and the cleanest RUE-1012
upgrade path for the price of one keyword, without S3's grammar footprint.
(S2's `yield` is also the natural home for the RUE-219 generator
conversation if iteration later builds on accessors. S3's `v[i]` subscript
sugar can still be layered on top later without disturbing this choice.)

## Semantics (formal amendment, lands with phase 1)

`docs/formal/01-core-calculus.md` gains, as part of the phase 1 change:

- **Statics**: an (Accessor-Call) rule in §5.8 — the result is a *borrowed
  place*, usable in place contexts only; the call adds `(root(v), mode)` to
  `Λ` with extent = the enclosing full expression; accessor bodies are
  well-formed iff every non-diverging exit yields a place rooted at the
  receiver parameter. A well-formedness corollary: the result participates
  in `Λ_call` consistency exactly as a `borrow p` argument does today.
- **Dynamics**: `v.get_ref(i)` reduces, by the accessor's (inlined) body,
  to the projected place — `(ℓ, π·πacc)` for user accessors, the defining
  equation's `view⟨A | o, k⟩` for library accessors (§6.13.2 machinery,
  unchanged).
- **Soundness**: no new theorem shapes. Second-classness is preserved
  because the result's extent is bounded by its expression; the §7
  view-intact, loan-extent-nesting, and handle-uniqueness lemmas already
  quantify over the values involved. The E0711 lift for accessor reads is
  justified by the result being a loan, not an owner.

## Implementation Phases

Tracked in Linear under the RUE-1015 epic:

- [x] **Phase 0: Ratify syntax; spec + grammar + preview gate scaffolding** — RUE-662
- [x] **Phase 1: Read accessors** (`borrow self` → shared result; parser,
      sema/AIR loan checking, mandatory inlining, formal-core amendment,
      spec coverage) — RUE-662
- [x] **Phase 2: Mutable accessors** (`inout self` → exclusive result;
      assignment through the result) — RUE-1016
- [ ] **Phase 3: std adoption** (`ArrayBuf.get_ref` + E0711 diagnostic
      pointing at it; grid/deque/intmap accessors; owned-children tree
      example replacing an arena) — RUE-1017 (blocked by RUE-662)
- [ ] **Phase 4: preview-gate removal** after dogfooding — RUE-1018
      (blocked by RUE-1016 and RUE-1017)

Each phase follows the `docs/designs/0005-preview-features.md` layer
checklist under the single `borrow-accessors` flag.

## Consequences

### Positive

- Owned-element collections become indexable/peekable structures; the
  ADT-vs-ECS forcing documented on RUE-286 is lifted.
- No new lifetime machinery, no ABI, no lowering shape; the checker change
  is one loan rule.
- The formal story is already built (RUE-390); the amendment is additive.
- Forward-compatible by construction with RUE-1012 coroutine accessors.

### Negative

- A new body form (under S2) and a new kind of expression result ("borrowed
  place") that tooling, diagnostics, and the spec must learn.
- Mandatory inlining couples accessor cost to call-site count; pathological
  accessor chains inline multiplicatively (bounded in practice by projection
  depth; worth a diagnostic if abused).
- Expression-scoped extent will eventually feel tight (`let b = v.get_ref(i)`
  is rejected); widening to statement scope is deliberately deferred until
  dogfooding demands it, to keep phase 1's checker rule minimal.

## Open Questions

- Whether `v[i]` on `ArrayBuf` should become sugar for the accessor in
  phase 3 or stay a separate place form until the subscript question
  (S3 territory) is decided.
- Diagnostic quality: E0711's message should redirect to `get_ref` once it
  exists; the escape-rejection diagnostics need the same care E0432/E0427
  are getting under RUE-953.

## Future Work

- RUE-1012: full coroutine/yielding accessors (lazy, cached, composite
  projections; `_modify`-style write-back).
- RUE-1013: signature-level partial borrows — composes with accessor calls
  once loans carry paths.
- RUE-219: whether iteration specifies itself over accessors.
- Statement-scoped accessor results (`let borrow`), if dogfooding demands.

## References

- RUE-662 (design gate), RUE-286 (evidence record), RUE-651 (E0711 gate),
  RUE-646 (droppable elements), RUE-1012 / RUE-1013 (research follow-ups).
- ADR-0037 (access-point exclusivity; second-class borrows), ADR-0043 (the
  collection trio), ADR-0049 (inlining splice), `docs/formal/01-core-calculus.md`
  §5.4 / §6.13 / §7 (loan machinery, allocation store, lemmas).
- Hylo subscripts/projections; Swift SE-0474 / SE-0507 (`read`/`modify`,
  `yielding borrow`/`yielding mutate`).
- Rust: RFC 1414 (static promotion, for the RUE-953 sibling ruling),
  "Generalized Partial Borrows" (internals, 2025-05), Niko Matsakis's
  view-types posts (babysteps) — for the RUE-997/RUE-1013 sibling track.
