# The Rue Core Calculus

This is the keystone of the formal semantics: the small calculus that surface Rue
elaborates into, and the precise home of ownership, moves, borrows, drops, and
linear consumption. Read `README.md` first for how this relates to the surface
language and the prose spec.

Notation is deliberately plain ASCII so it can be transcribed into a proof
assistant or an interpreter without re-typesetting. Judgment forms are introduced
where they are first used. Paragraphs tagged **[open]** are modeling decisions
that a maintainer should confirm; everything else is a proposed commitment.

---

## 1. What the core is, in one paragraph

After elaboration (desugaring + comptime + monomorphization), a Rue program is a
set of first-order, fully-monomorphic function definitions over a small set of
types, with no `comptime`, no generics, and no method-call or `&&`/`||`/`while`
sugar. The core makes precise what it means to *run* such a program and what it
means for one to be *well-formed* — where "well-formed" includes the entire
ownership discipline that is Rue's memory-safety guarantee.

---

## 2. Abstract syntax

```
Types
  T ::= int(w, s)              -- integer of width w ∈ {8,16,32,64}, signedness s ∈ {signed, unsigned}
      | bool
      | unit
      | never                  -- the ! type; no values
      | S                      -- a (monomorphic) struct-type name
      | E                      -- a (monomorphic) enum-type name
      | [T; n]                 -- array of n ≥ 0 elements of type T

Type declarations
  D ::= struct S { f1: T1, ..., fk: Tk }             -- struct; multiplicity class assigned by §3
      | enum   E { K1(T̄1), ..., Kn(T̄n) }             -- enum (sum); variant Ki has payload tuple T̄i = Ti1..Ti_{ai} (ai ≥ 0)
  (an elaborated anonymous struct is just a struct name S with a generated identity;
   a discriminant-only, C-like enum is the ai = 0 case for every variant)

Places (lvalues — expressions that denote a location)
  p ::= x                      -- a local binding
      | p . f                  -- field projection
      | p [ e ]                -- array index (e a value expression of an int type)

Expressions
  e ::= lit                    -- integer / bool / unit literal
      | p                      -- a place used in VALUE context (see §4 — this is a USE)
      | e1 ⊕ e2                -- primitive arithmetic / bitwise op (operands are Copy scalars)
      | ⊖ e                    -- primitive unary op: neg / not / bitnot (operand a Copy scalar)
      | e1 ≟ e2                -- primitive equality compare (== / !=); operands are BORROWED, not moved (§4.1, 4.3:3f)
      | e1 ⋚ e2                -- primitive ordering compare (< > <= >=); operands are Copy scalars (4.3:1)
      | S { f1: e1, ..., fk: ek }        -- struct value construction
      | E :: K ( e1, ..., em )           -- enum value construction (variant K of E with payload e1..em; m = arity of K)
      | [ e1, ..., en ]        -- array value construction
      | g ( a1, ..., am )      -- call of function g with argument forms a_i (see below)
      | if e0 { e1 } else { e2 }
      | match e0 { pat1 => e1, ..., patk => ek }
      | let μ x = e1 ; e2      -- binding; scope of x is e2; μ ∈ {∅, mut} is the binding's mutability mark (§5)
      | e1 ; e2                -- sequence; e1's value is DISCARDED (well-formed unless it carries a linear value — §5.3, 3.8:64)
      | loop { e }             -- infinite loop; exited only by break
      | break                  -- exit the nearest enclosing loop, value unit
      | return e               -- exit the enclosing function with e's value
      | assign p = e           -- assignment (a statement form; value unit)

Patterns (a match arm's head; matched against an enum scrutinee)
  pat ::= E :: K ( x1, ..., xa )   -- variant pattern for variant K of arity a: binds the a payload components to fresh locals
                                   -- (a = 0 for a discriminant-only variant, written E::K)

Argument forms (how an argument is passed — the call-site mode)
  a ::= e                      -- by value: the parameter takes ownership (move) or a copy
      | inout p                -- by exclusive reference: an exclusive loan of place p
      | borrow p               -- by shared reference: a shared loan of place p

Function definitions
  F ::= fn g ( m1 x1: T1, ..., mk xk: Tk ) -> T { e }     -- m_i ∈ {∅, inout, borrow}; a by-value (m_i = ∅)
                                                          --   parameter may additionally carry the μ = mut mark,
                                                          --   binding exactly as a `let mut` local does

Programs
  P ::= D* F*                  -- with exactly one  fn main() -> (int(32,signed) | unit)
```

Notes on what is **absent by design** (lives in elaboration, `02-elaboration.md`):
`comptime`, comptime/`type` parameters, generics, method-call syntax, `Self`,
`&&`/`||` (→ `if`), `else if` (→ nested `if`), `while c { b }` (→
`loop { if c { b } else { break } }`), block syntax (→ `let`/`;` sequences),
`@import`/modules (→ a flat set of `F` after resolution), integer-literal *base*
(→ the value); and, closing the inventory gaps of RUE-1279:

- **surface shadowing** (`3.8:12/13`) → elaboration **α-renames** binders so
  every binding in a function body has a distinct name (the Barendregt
  convention); the core never sees a shadowed name, which is what licenses §6.7's
  never-restored environment entries;
- **const items** → comptime evaluation inlines each use as the resolved value;
  no core form remains;
- **repeat arrays** `[v; n]` → the surface restricts the element to a `Copy`
  type (E0905, verified against the compiler), so the form elaborates to
  `let t = v; [t, …, t]` — one evaluation of the operand, then `n`
  value-context *copies* (§4.2); no non-`Copy` instance exists to need an
  evaluate-once move semantics;
- **postfix `?`** → the enclosing-function early-return desugaring over the
  scrutinized enum (a `match` whose failure arm rebuilds the failure value and
  `return`s it, and whose success arm yields the payload binding). The
  desugaring is ownership-relevant — the scrutinee is *used* (§4.2) and the
  failure arm's `return` is a §5.7 divergence point — which is why it must be
  specified in `02-elaboration.md` rather than treated as transparent sugar;
- **`_` in payload-binding position** → a fresh, unnameable binding, governed
  by §5.6 exactly like its named siblings (an `Affine` payload it covers drops
  at the arm's end; a `Linear` one makes the arm ill-formed). RUE-1270 records
  a compiler divergence in this area (sibling drops skipped by a partial
  payload binding); the core states the fresh-binding rule;
- **no-argument `@panic()`** → the message form (§6.12, D-Panic) with the
  fixed message `panic` (verified: the compiler emits the bare word and exits
  101).

**[open]** Raw pointers and `unchecked` code (chapter 9) are
initially *out* of the core and added as a distinguished, clearly-marked
extension; their whole point is to step outside the guarantees the core proves,
so they are modeled separately rather than threaded through every rule.

**Buffer-backed container types are inside the machine; slice statics are not
yet.** The RUE-390 modeling decision is ratified (2026-07-14): the machine's
store is an **allocation store** (§6.1) — abstract allocations, not a
language-level heap — and the buffer-backed library types `ArrayBuf(T)` /
`StrBuf` are brought inside the proved perimeter by defining equations over it
(§6.13), so the §7 theorems now quantify over buffer cells as well
(conditional on the library obligations of §6.13.5). What remains outside the
*grammar* above: the surface slice forms (`[T]`, `str`, `Str(N)`) and the
mode-position compatibility relation `⊳` that creates views at call sites (the
external-call note in §6.9, string content equality in §6.4, the `⊳` examples
in §5.7). Those mentions remain *forward references* — but they now have
something to refer to: giving slices their §5 statics is the separate slices
work, designed against the view values §6.13.2 already defines.

---

## 3. The multiplicity lattice

Every type has a **class** describing its substructural behavior — how many times
a value of it may be used, and whether it may be discarded (dropped):

```
  class(T) ∈ { Copy, Affine, Linear }

  Copy    : may be used any number of times (contraction);  may be dropped (weakening).
  Affine  : may be used at most once (no contraction);       may be dropped (weakening).
  Linear  : must be used exactly once (no contraction);      must NOT be dropped (no weakening).
```

Assignment of the class:

```
  class(int(_,_)) = class(bool) = class(unit) = Copy
  class(never)    = Copy            -- vacuous: no values, so any class is sound; Copy is simplest
  class([T; n])   = Linear   if n > 0 and class(T) = Linear
                  = Affine   if n > 0 and class(T) = Affine
                  = Affine   if n = 0 and class(T) ≠ Copy    -- carries nothing (3.8:74): droppable, but NOT duplicable
                  = Copy     if class(T) = Copy      -- includes every n when T:Copy

  For a struct  S { f1: T1, ..., fk: Tk }  with declared attribute attr(S):
    let base = ⊔ { class(Ti) }            -- the join (Copy ⊑ Affine ⊑ Linear) over fields
    class(S) = Linear         if attr(S) = linear   OR base = Linear    -- infectious (3.8:58, 3.8:57)
             = Copy           if attr(S) = @copy    (well-formed only if base = Copy and S declares no destructor — 3.8:18, 3.9:31)
             = Affine         otherwise

  For an enum  E { K1(T̄1), ..., Kn(T̄n) }  with T̄i = Ti1..Ti_{ai}:
    class(E) = ⊔ { class(Tij) : 1 ≤ i ≤ n, 1 ≤ j ≤ ai }    -- join over EVERY payload component of EVERY variant (6.3:19)
             = Copy   when there are no payload components  -- discriminant-only ⇒ empty join ⇒ Copy (6.3:19, 3.8:2)
```

An enum has no `@copy`/`linear` attribute of its own: its class is exactly the
join of its variants' payload classes (`6.3:19`). It is `Copy` iff every payload
of every variant is `Copy` (a discriminant-only enum, whose join is empty, is the
degenerate `Copy` case); `Affine` if some payload is `Affine` and none `Linear`;
`Linear` if some payload is `Linear` — and a `Linear` enum must be consumed
exactly like a `Linear` struct (`6.3:19`, matching ADR-0039). The join is over
*all* variants because the active variant is not known statically; the class is
the type's worst case, while the *drop* at run time touches only the active
payload (§5.6, §6).

The lattice order is `Copy ⊑ Affine ⊑ Linear` (more restrictive is higher); `⊔`
is the least upper bound. **Infectiousness is just the join**: a struct is at
least as restrictive as its most-restrictive field. A `@copy` declaration is
well-formed only when the field join is already `Copy` (`3.8:18`) and the struct
declares no destructor (`3.9:31`); a `linear` declaration forces `Linear`
regardless of fields.

A zero-length array of a non-`Copy` element type is `Affine`, not `Copy`:
prose `3.8:74` grants it *droppability* only — its must-consume obligation is
vacuously satisfied because it holds no values — and says nothing about
duplicability. The compiler agrees: `let b = a; let c = a;` on an `[NC; 0]`
with `NC` non-Copy is a use-after-move error (E0205). (An earlier draft of
this table classified every `[T; 0]` as `Copy`; that over-granted contraction
— RUE-526.)

> This replaces the prose enumerations `3.8:2` (the Copy list, which includes
> discriminant-only enums), `3.8:3` (structs affine by default), `3.8:18/20`
> (`@copy` field constraint), `3.8:57/58` (carries-linear, infectious), and
> `6.3:19` (enum multiplicity = the payload join) with one lattice and one join.

There is no fourth class. Explicit duplication of an `Affine` value is an
ordinary function `g_dup : S -> S`, needing no directive and no lattice
change — the former `@handle` directive was retired on exactly this
observation (RUE-199).

---

## 4. THE KEYSTONE: places, values, and *use*

Rue's prose says a value is moved when it is "assigned to another variable,
passed as an argument, or returned" (`3.8:7`); that a linear value is consumed
when "passed, returned, or destructured" (`3.8:33`); and that Copy values may be
"used" repeatedly (`3.8:9`). These are three overlapping, individually incomplete
enumerations of one underlying notion. The core defines that notion once.

### 4.1 Place context vs. value context

Every occurrence of a place expression `p` in a program sits in exactly one of two
syntactic contexts:

- **Place context** — the occurrence denotes a *location*, and the value stored
  there is not consumed by appearing here. There are exactly these place
  contexts:
  - the target of an assignment: the `p` in `assign p = e`;
  - the base of a projection: the `p` in `p.f` and the `p` in `p[e]`;
  - the operand of a by-reference argument: the `p` in `inout p` and `borrow p`;
  - the operands of an equality compare: the `p` in `p ≟ e` and `e ≟ p` (`==`
    / `!=`). Equality **borrows** its operands — each operand place is read
    through a shared loan, its value inspected but not consumed, exactly as a
    `borrow p` argument (`4.3:3f`). An affine or linear operand is therefore
    still Owned afterward and its move obligation is undischarged; `let c = a;
    a == b` is well-formed. (Contrast: arithmetic/bitwise `⊕` operates only on
    `Copy` scalars, so *its* operands are ordinary value-context uses below —
    the copy-vs-borrow distinction is immaterial there.)
- **Value context** — every other occurrence. The occurrence must *produce a
  value*: operands of an arithmetic or bitwise `⊕` (always `Copy` scalars, so
  the use copies), the scrutinee of `if`/`match`, a struct-field or
  array-element initializer, a by-value argument `e`, the operand of `return`,
  the tail expression of a `let`/`;`-sequence, and so on.

### 4.2 Definition: *use*

> **Definition (use).** A **use** of a place `p` of type `T` is the appearance of
> `p` as an expression in *value context* (the `e ::= p` production of §2). Its
> effect depends only on `class(T)`:
>
> - if `class(T) = Copy`: the use **copies** — the value at `p` is duplicated;
>   `p` remains in whatever ownership state it had.
> - if `class(T) ∈ {Affine, Linear}`: the use **moves** (equivalently,
>   *consumes*) — the value at `p` is transferred out; `p` becomes `MovedOut`
>   (§5). A later use of `p`, or of any place under it, is ill-formed; and `p` is
>   not dropped at scope exit.
>
> A use of a *projection* `p.f` (or `p[c]` with `c` a constant) in value context
> is a **partial** move: it moves exactly the sub-place `p.f`, leaving sibling
> sub-places of `p` in their prior state (`3.8:22`). A use of a whole place moves
> the whole place — and requires the whole place: using an aggregate any of
> whose sub-places is already `MovedOut` is ill-formed (`3.8:26`; the
> `fully-owned` premise of §5.1).

Two static restrictions bound *which* projections may be partially moved:

- **No moves out of a destructor-bearing value** (`3.9:34`, E0456): a partial
  move of `p.f` (or `p[c]`) is ill-formed if the type of `p` — or of any
  enclosing place along the path — declares a user destructor. The destructor
  runs on the whole value at drop and would observe the hole. Borrowing such a
  sub-place, or moving the *whole* value, remains legal.
- **Element moves only at the root** (`3.8:68`, E0904): an index step `[c]`
  may appear in a moved path only applied directly to the root binding — `x[c]`
  or `x[c].f…` moves are legal (for constant `c`), but an array reached through
  any projection (`x.f[c]`), or indexed by a non-constant, cannot be moved out
  of. This is what keeps Σ's constant-index path tracking finite and decidable.

That is the entire notion. Everything the prose enumerated is now a corollary:

| Prose rule | Now a consequence of §4.2 |
|---|---|
| `3.8:5` use-after-move is an error | using `p` requires `p` Owned (§5); a moved `p` is MovedOut |
| `3.8:7` moved when assigned / passed / returned | all three are value contexts ⇒ uses ⇒ moves (for non-Copy) |
| `3.8:9/11` Copy used repeatedly / copied into params | Copy use = copy; ownership state unchanged |
| `3.8:22` field access is a partial move | the projection clause of §4.2 |
| `3.8:33` linear consumed when passed/returned/destructured | value-context use of a Linear place moves it |
| `3.8:53/54` reading a Copy field through a moved ancestor is an error | the base `p` of `p.f` must be Owned (§5, ProjRead) |
| `4.3:3f` equality borrows its operands | equality operands are *place context* (§4.1), not a value-context use ⇒ no move; a shared loan for the compare |

The prose paragraphs above should be **rewritten** to reference this single
definition rather than re-list contexts. (This is the concrete form of the
"implicit return" fix, applied to ownership: name the compositional rule, delete
the folk enumeration.)

### 4.3 The analogous fix for expression value (the "implicit return" item)

The same discipline resolves the original complaint. There is no "implicit
return." Instead, two compositional rules (dynamic semantics, §6):

- a `let x = e1 ; e2` sequence, and a bare `e1 ; e2`, **evaluate to the value of
  their tail** (`e2`);
- a function call **evaluates to the value its body evaluates to** (§6, Call/Ret).

A function "with no `return`" returns its body's value because *its body is an
expression that evaluates to a value* — not because of any implicit action. The
prose `6.1:4/5` and the block chapter `4.5` are rewritten to state these two
rules; the word "implicit" disappears.

---

## 5. Static semantics: typing with ownership

Ownership is flow-sensitive, so the type judgment threads an **ownership state**.

```
  Ownership state
    Σ : Path ⇀ { Owned, MovedOut }        -- absence of a path ⇒ Uninit (not yet bound / fully moved)
    Path ::= x | Path.f | Path[c]         -- static access paths; array index tracked only for constants c

  Type context
    Γ : x ⇀ (T, μ)     -- the declared type and mutability mark of each in-scope binding (both fixed at the binder;
                       --   Γ ⊢ x : T abbreviates the type component, and μ(x) reads the mark)

  Loan state (for exclusivity)
    Λ : set of currently-outstanding loans, each  ( root(p), {shared | exclusive} )
```

`Σ(p)` is the state recorded for the exact path `p`. A base path can be `Owned`
while one of its descendants is `MovedOut` after a partial move; rules that hand
an aggregate to another context therefore use the stronger predicate
`fully-owned(Σ,p)`, meaning `Σ(p)=Owned` and no path strictly under `p` is
`MovedOut`. A place `p` is **mutable** when its root is a `μ = mut` binding (a
`let mut` local or a `mut`-marked by-value parameter — the grammar's binding
mark, §2, which elaboration carries over from the surface `mut`), an `inout`
parameter, or a projection through either; unmarked bindings, unmarked
parameters, and `borrow` parameters are not mutable roots. The mark is
static-only: no §6 rule consults it, and it has no dynamic image — mutability
is a well-formedness discipline, not a runtime property. (RUE-1279: an earlier
draft's binding form had no mark, leaving this paragraph and (Assign)'s
mutability premise undefined for every elaborated program.)

The main judgment:

```
    Γ ; Σ ; Λ  ⊢  e  ⇒  T  ⊣  Σ'
```

read: "under bindings Γ, starting ownership Σ and loans Λ, expression `e` is
well-formed, has type `T`, and leaves ownership Σ'." (Λ is scoped to a single call
and does not change across `e`; it is threaded only so the use/assign rules can
consult it — see 5.4. It is written on the turnstile, not on the output, for that
reason.)

**Λ is ambiently empty in the current core** (RUE-526 hygiene note): (Fn)
checks every body under `∅`, and (Call) builds `Λ_call` but discharges it
*locally* — by the consistency check and by rechecking `fully-owned` for every
loaned root against the state at call entry — rather than threading it into the
argument subexpression judgments. So the "not loaned in Λ" premises of
(Use-Copy)/(Use-Move)/(@Drop)/(Assign) can never fire today; they are retained
because they become load-bearing the moment any extension lets a loan span a
judgment (first-class references, §5.4's **[open]**, or argument-list
threading). This is not a soundness gap for the current language: loans begin
at call entry, *after* all argument evaluation (see (Call)), so a mutation or
overwrite of a to-be-loaned root inside a sibling argument completes before the
loan exists — the loan then observes the new, valid value (verified against the
compiler: the overwrite's drop glue runs during argument evaluation and the
callee reads the updated value through the borrow) — and a *move* of a loaned
root anywhere in the argument list is caught by the entry recheck (prose
`6.1:36`, E0208).

Function bodies are checked with an entry ownership state and parameter
discipline determined by the signature:

```
  g : fn (m1 x1:T1, ..., mm xm:Tm) -> Tr { e_body }
  Γ0 = [x1↦T1, ..., xm↦Tm]
  Σ0 = [xi↦Owned for every parameter xi]
  Γ0;Σ0;∅ ⊢ e_body ⇒ Tr ⊣ Σf
  for every parameter xi:
    mi = borrow  ⇒ xi and every path under xi is read-only, never moved out, and may be re-lent only as borrow
    mi = inout   ⇒ xi may be read, written, and forwarded, but never moved out
    mi = ∅       ⇒ xi is an owned by-value local subject to the ordinary use/drop rules
  ───────────────────────────────────────────────────────────────────────── (Fn)
  g is well-formed
```

The `borrow` and `inout` restrictions are **callee-polarity** rules: the caller
may not touch a lent place while the loan is live, while the callee may touch the
parameter only in the mode it received. They are not represented by inserting the
parameters into ambient `Λ`; doing so would make an `inout` parameter unreadable
by the ordinary `(Use-Copy)` premise.

### 5.1 Use and copy (the keystone, as a rule)

```
  class(Γ ⊢ p : T) = Copy         Σ(p) = Owned         p not exclusively loaned in Λ
  ───────────────────────────────────────────────────────────────────────── (Use-Copy)
  Γ ; Σ ; Λ  ⊢  p  ⇒  T  ⊣  Σ

  class(Γ ⊢ p : T) ∈ {Affine, Linear}    fully-owned(Σ, p)    p not loaned in Λ
  no proper prefix of p has a type that declares a destructor       -- 3.9:34 (E0456); moving the WHOLE value is fine
  any index step in p is a constant [c] applied directly to the root binding    -- 3.8:68 (E0904)
  ───────────────────────────────────────────────────────────────────────── (Use-Move)
  Γ ; Σ ; Λ  ⊢  p  ⇒  T  ⊣  Σ[ p ↦ MovedOut,  and every path strictly under p removed ]
```

`fully-owned(Σ, p)` (§5 preamble) is the **no-use-after-move** premise
strengthened to the whole subtree (`3.8:5/24/26/53`): a use requires that `p`
owns its value *and* that no sub-place of `p` has been partially moved out —
handing an aggregate with a hole to a new owner is ill-formed (`3.8:26`; the
compiler's E0205 "use of partially moved value"). (Use-Copy) needs only
`Σ(p) = Owned`: every sub-place of a `Copy` type is itself `Copy`, so no
descendant can be `MovedOut`, and the two premises coincide there. The second
(Use-Move) premise is the destructor-field restriction and the third the
root-index restriction, both from §4.2. `Use-Move` records the consumption; the
`p not loaned` premise forbids moving a place that is currently borrowed
(`3.8`/exclusivity). Reading a place that is only *projected from* uses the
ProjRead rule instead:

```
  Σ(p) = Owned      -- the base must currently own its storage (3.8:53)
  ───────────────── (Owned-Base)         -- side condition used by p.f / p[e] in any context
```

### 5.2 Assignment and reinitialization

```
  Γ ; Σ ; Λ ⊢ e ⇒ T ⊣ Σ1       Γ ⊢ p : T       (mutability & loan side-conditions)
  Σ1(p) = MovedOut  ∨  ¬carries_linear(T)                  -- 3.8:77: no implicit linear drop-on-overwrite
  p's prior value, if Owned and droppable, is dropped BEFORE the store (dynamic, §6)
  ─────────────────────────────────────────────────────────────────────────────── (Assign)
  Γ ; Σ ; Λ ⊢ (assign p = e) ⇒ unit ⊣ Σ1[ p ↦ Owned ]      -- reinitialization (3.8:55)
```

Assigning to a `MovedOut` place makes it `Owned` again (`3.8:55/56`). Assigning to
an already-`Owned` place whose type does **not** carry a linear value drops the old
value first (`3.8` overwrite-drop). Overwriting an already-`Owned` place whose type
*does* carry a linear value is **ill-formed** (`3.8:77`): the second premise, checked
on the post-RHS state `Σ1`, admits the assignment only when `p` is `MovedOut` (the
reinitialization idiom, where there is nothing to drop) or `T` carries no linear
value. This closes the theorem-5 hole (RUE-387): without it, the overwrite-drop
would implicitly consume a linear value that the program never explicitly consumed.
The `Σ1` (rather than `Σ`) reading of the premise makes `p = e` legal when `e` itself
consumes `p` (`x = f(x)`), matching the RHS-first drop order. Writing *into* an array
while any element is moved out is rejected by a side condition (`3.8:72`); a runtime
index can never establish `Σ1(p) = MovedOut`, so a linear element assignment through
one is always rejected.

### 5.3 Sequencing, discard, and the linear leak check

```
  Γ ; Σ ; Λ ⊢ e1 ⇒ T1 ⊣ Σ1         carries_linear(T1) = false        -- 3.8:64
  Γ ; Σ1 ; Λ ⊢ e2 ⇒ T2 ⊣ Σ2
  ─────────────────────────────────────────────────────────────────── (Seq)
  Γ ; Σ ; Λ ⊢ (e1 ; e2) ⇒ T2 ⊣ Σ2
```

Discarding a value whose type *carries a linear value* is ill-formed (`3.8:64` —
`carries_linear(T)` is `class(T) = Linear` lifted through the aggregates: the
field join for a struct, the element type for an array **of nonzero length**
(a zero-length array carries nothing, `3.8:74` — which is why §3 classes it
`Affine`, droppable, when its element type is non-Copy), and the **payload join
over all variants** for an enum (`6.3:19`) reaching Linear). Because `class` is
itself defined as exactly these joins (§3), `carries_linear(T) ⟺ class(T) =
Linear` for every type — including enums, whose class is the payload join, and
zero-length arrays, whose class never reaches Linear.
`let x = e1 ; e2` is like `Seq` but binds `x` (with `x` Owned in Σ for `e2`) and
imposes no discard check on `e1`.

The `@drop(p)` intrinsic is the deliberate, visible discharge of a value's drop
obligation (`3.9`). For `Affine`/`Linear` operands it consumes the operand, runs
the same drop glue that scope exit would run, and leaves the source moved out so
no later scope-exit drop runs for that place. It is the only non-move operation
that can satisfy a linear obligation. For `Copy` operands, `@drop` has no drop
glue and no ownership effect.

```
  Γ ⊢ p : T        class(T)=Copy        Σ(p)=Owned
  ─────────────────────────────────────────────────────── (@Drop-Copy)
  Γ;Σ;Λ ⊢ @drop(p) ⇒ unit ⊣ Σ

  Γ ⊢ p : T        class(T)∈{Affine,Linear}        Σ(p)=Owned        p not loaned in Λ
  ─────────────────────────────────────────────────────── (@Drop)
  Γ;Σ;Λ ⊢ @drop(p) ⇒ unit ⊣ Σ[ p ↦ MovedOut, and every path strictly under p removed ]
```

### 5.4 Borrows and the law of exclusivity

A call evaluates its arguments and then, for the duration of the call, holds
*loans* on the places passed by reference. Within one call:

```
  For each argument a_i of call g(a1..am):
    a_i = e         : ordinary value context — Use-Copy / Use-Move on any places in e
    a_i = borrow p  : requires fully-owned(Σ,p); adds (root(p), shared)   to Λ_call
    a_i = inout p   : requires fully-owned(Σ,p); adds (root(p), exclusive) to Λ_call; p mutable

  Well-formed only if Λ_call is CONSISTENT:  a root may appear
    - any number of times as shared,  OR
    - exactly once as exclusive,
    - never both.                                            -- law of exclusivity (6.1:20, 6.1:30)
  At the concrete `(Call)` rule, every loaned root is rechecked against the final
  argument state at call entry.
```

While `(root(p), _) ∈ Λ`, `p` and every path under/over it may not be moved
(`Use-Move` premise) nor, for a shared loan, mutated. Loan consistency is
deliberately **root-granular**: a loan of any projection or subrange covers the
entire root, so two `inout` loans of disjoint fields or ranges of the same root
are ill-formed. This is a conservative static rule; future `split_at_mut`-style
APIs must express disjointness by producing distinct roots. Loans are
**second-class**: they exist only for the call's dynamic extent and cannot be
returned, stored, or outlive the call — this is what lets Rue omit lifetimes.
**[open]** The core models loans as strictly call-scoped; if a future
first-class-reference feature is adopted (a live ADR question, deferred), this
section is where it lands.

**Borrow of a value (elaboration, RUE-953).** The rule above is stated over
`borrow p` with `p` a place, and that is the whole of the core: surface syntax
also admits `borrow e` for an arbitrary `e`, and it is *elaborated away* before
these rules apply. Elaboration is a source-to-source function on the argument,
choosing between two forms:

```
  borrow e   with   e comptime-evaluable and infallible
      ⇝  borrow ℓ_e        where ℓ_e is e's immortal static allocation in H0 (§6.13.2)

  borrow e   otherwise
      ⇝  let x_fresh = e in  borrow x_fresh    -- x_fresh unnameable, scoped to the call
```

Neither form is a new rule. The promoted form loans an `H0` allocation, which
by §6.13.2 is minted before `main` and never retired: `Σ(ℓ_e) = Owned` holds
everywhere, no scope contains it, and §5.6 therefore schedules nothing for it —
that is the precise content of "no drop glue". The temporary form is an
ordinary `let` whose binding scope is the call, so §5.6 governs it unchanged:
if `class(T)` is droppable the drop is scheduled at the call's exit, and if
`residual-linear(Σ, x_fresh, T)` the program is **ill-formed** — which is the
right answer, since `x_fresh` is unnameable and so can never be consumed. The
must-consume rejection of `f(borrow make_token())` is thus a consequence of
§5.6, not an added premise.

The elaboration is a function of the *argument's syntax alone*: it commits to
the promoted form only on a value-independent, enumerated set of infallible
forms (literals, constants, and the total operators over them — notably not `/`
or `%`, whose traps are value-dependent). A value-dependent promotion rule is
unsound to weaken later, which is the history Rust's RFC 1414 → RFC 3027 records.

An **equality compare** `e1 ≟ e2` reads each place operand through the same
kind of shared loan, scoped to the compare rather than a call: it requires
`Σ(p) = Owned`, takes a `(root(p), shared)` loan for the compare's duration,
and leaves Σ unchanged (no move — §4.1, `4.3:3f`). Two shared reads are always
consistent, so an operand may even appear on both sides (`a == a` is
well-formed). This is why comparing an affine or linear value does not
discharge its move obligation. This eager-read model keeps the left operand's
temporary shared loan from overlapping evaluation of the right operand. If future
slice/view equality defers reading either side, it must add an explicit
loan-extent rule; otherwise an expression such as `v[0..2] == g(inout v)` could
dangle after `g` reallocates. The executable oracle's current copy-in/copy-out
model masks that class and is not evidence that deferred view equality is sound.

### 5.5 Control flow and the branch join

`if` and `match` type each arm under the *same* incoming Σ and must **reconcile**
their outgoing states:

```
  Γ;Σ;Λ ⊢ e0 ⇒ bool ⊣ Σ0
  Γ;Σ0;Λ ⊢ e1 ⇒ T ⊣ Σ1        Γ;Σ0;Λ ⊢ e2 ⇒ T ⊣ Σ2
  Σ' = join(Σ1, Σ2)      -- see below
  ─────────────────────────────────────────────────────── (If)
  Γ;Σ;Λ ⊢ if e0 { e1 } else { e2 } ⇒ T ⊣ Σ'
```

`join` must agree on every path that *carries a linear value*: if a linear-
carrying place is MovedOut on one branch and Owned on the other, the program is
ill-formed (`3.8:50` — a linear value consumed on only some paths). For non-linear
(merely Affine/Copy) places the join takes `MovedOut` if either branch moved it
(the value is conservatively considered gone), and the dynamic semantics uses a
per-path drop flag so the runtime drops it on exactly the paths that did not
(`3.8:73`). A branch ending in `return`/`break` has outgoing state `⊥` (diverged)
and is excluded from the join (`3.8:51`); its *type* is `never`, which
(Sub-Never), §5.7, coerces to the sibling arm's type `T`, so a diverging arm
still satisfies the (If)/(Match) same-type premise.

`match` is the elimination form for enums, and its arms join exactly as `if`'s do.
Typing the scrutinee is a value-context **use** of it (§4.2): a move-typed enum is
*consumed* by the match, and each arm's variant pattern binds that variant's
payload components as fresh Owned locals — this is the "destructured" consumption
context of `3.8:33` and `6.3:17`. The scrutinee must be covered exhaustively (one
arm per variant of `E`), so exactly one arm's payload is live in any run:

```
  Γ;Σ;Λ ⊢ e0 ⇒ E ⊣ Σ0          E = enum { K1(T̄1), ..., Kn(T̄n) }   with T̄i = Ti1..Ti_{ai}
  arms are exhaustive: exactly the variants K1..Kn, arm i headed by pat_i = Ki(x_{i1}, ..., x_{i,ai})  (fresh locals)
  for each i:   Γ, x_{i1}:Ti1, ..., x_{i,ai}:Ti_{ai} ;  Σ0[ x_{ij} ↦ Owned ] ;  Λ   ⊢ ei ⇒ T ⊣ Σi'
  Σ' = join(Σ1, ..., Σn)      -- restricted to paths live before the match; each pattern local x_{ij} leaves scope at arm end (§5.6)
  ─────────────────────────────────────────────────────────────────────────────────────── (Match)
  Γ;Σ;Λ ⊢ match e0 { K1(x̄1) => e1, ..., Kn(x̄n) => en } ⇒ T ⊣ Σ'
```

Typing `e0 ⇒ E` moves the scrutinee out when `class(E) ∈ {Affine, Linear}` and
copies it when `class(E) = Copy` (the (Use-Move)/(Use-Copy) split of §5.1),
exactly as for any other place. The payload locals `x_{ij}` are ordinary Owned
bindings and are governed by §5.6 at the arm's end: a `Linear`-carrying payload
that an arm neither moves nor consumes is a leak error, and an `Affine` payload it
drops (once) — this is the formal content of "binding a variant's payload moves it
out; a moved-out payload runs its destructor exactly once when its binding leaves
scope" (`6.3:17`). Diverging arms (`return`/`break`) contribute `⊥` and are
excluded from the join, as with `if`.

The core `match` is deliberately the **canonical form only**: an enum
scrutinee, exactly one arm per variant, no wildcards, no guards. Everything
else the surface `match` chapter (4.7) allows is an **elaboration obligation**
(`02-elaboration.md`), stated here so the gap is not silent (RUE-526): a
wildcard or `_` arm duplicates into one arm per remaining variant; several
patterns targeting one body duplicate the body; surface first-match ordering
is resolved *by* that duplication (each core variant arm is the textually
first surface arm covering it); an integer or bool scrutinee elaborates to
nested `if` chains on `≟` (bool and integer matches are exhaustiveness-checked
at the surface, and an integer match's residual arm is the `_` the surface
requires); and a zero-arm `match` on an uninhabited scrutinee is `never`-typed
at the surface and does not reach the core (no `E` here is uninhabited — every
core enum has at least one variant). The (Match) rule below therefore never
needs an ordering or overlap side condition.

Enum construction is the dual introduction form, evaluated like a struct literal:
each payload argument is a value-context use, and the result owns the tag and the
supplied payload.

```
  E = enum { ..., Kj(T̄j), ... }  with T̄j = Tj1..Tj_{aj}
  Γ;Σ;Λ ⊢ e1 ⇒ Tj1 ⊣ Σ1     Γ;Σ1;Λ ⊢ e2 ⇒ Tj2 ⊣ Σ2     ...     Γ;Σ_{aj-1};Λ ⊢ e_{aj} ⇒ Tj_{aj} ⊣ Σaj
  ─────────────────────────────────────────────────────────────────────────────── (Enum-Intro)
  Γ;Σ;Λ ⊢ E::Kj(e1, ..., e_{aj}) ⇒ E ⊣ Σaj
```

### 5.6 Scope exit: the drop obligation and the leak check

When a binding `x: T` introduced by `let` (or a by-value parameter) leaves scope
in state `Owned`:

- if `residual-linear(Σ, x, T)` (below): **ill-formed** — a linear value
  reached end of scope unconsumed (`3.8:32/62/66`). This is the must-use check.
- else if `class(T) = Copy`: nothing happens (no drop).
- else (`Affine`, droppable, non-linear): a **drop** is scheduled (dynamic §6):
  the value's destructor, if any, runs, then its droppable *contents* drop,
  skipping any sub-place that is `MovedOut`. The contents depend on the type:
  - a **struct** drops its droppable fields in declaration order (`3.9`);
  - an **array** drops its droppable elements in ascending index order (`3.9`);
  - an **enum** drops the payload of its **active** variant only — the variant
    selected by the run-time discriminant — running that payload's drop glue
    exactly once, and nothing for a discriminant-only active variant (`6.3:20`).
    Which variant is active is a *dynamic* fact, so this drop cannot be unrolled
    statically the way a struct's fields can; §6 reads the tag and drops the one
    live payload. A payload already moved out (e.g. through a `match` binding,
    §5.5) left the enum `MovedOut` and is skipped here, so it is never dropped
    twice (`6.3:20`).

The leak check is keyed on the **residual** ownership state, not on the
binding's *type* alone (RUE-526): after a partial move, the linear obligation
attaches to whatever linear content is still present. `carries_linear(T)` at
the binding's type would over-reject the legal idiom of consuming exactly the
linear part of an infectious carrier (`let h = Holder { t: token, n: 0 };
consume(h.t);` — `class(Holder) = Linear` by the join, yet prose and compiler
both let the non-linear residue drop). Precisely:

```
  residual-linear(Σ, p, T) =
    false                                            if Σ(p) = MovedOut (or p is under a MovedOut prefix)
    true                                             if T is a struct with attr(T) = linear      -- the obligation is the VALUE's (3.8:74)
    ∃ field f:  residual-linear(Σ, p.f, T_f)         if T = struct { …, f: T_f, … }
    ∃ tracked element [c]:  residual-linear(Σ, p[c], T')   ∨  (untracked residue carries linear)
                                                     if T = [T'; n], n > 0
    class(T) = Linear                                if T is an enum (payload paths are not statically tracked)
    false                                            if T is a scalar ([T'; 0] has no elements: false)
```

An `Owned` binding with **no** residual linear content but *some* residual
droppable content falls to the third bullet: its scope-exit drop walks the
value skipping every `MovedOut` sub-place, exactly as §6.11's `⊘`-skip does
dynamically. Note the `attr(T) = linear` case: a **declared**-`linear` struct's
obligation belongs to the value itself, not its contents (`3.8:74` — "must be
consumed regardless of what its fields hold"; the empty
`linear struct MustUse` of `3.8:75` is the motivating case), so a husk whose
linear field was moved out is still ill-formed to drop. The compiler currently
diverges on exactly that husk case — it keys purely on residual content and
accepts the drop (RUE-614); the core states the prose rule.

Parameters passed `inout`/`borrow`, and a destructor's own `self`, are exempt from
the must-consume and drop obligations here (`3.8:62`): the caller (resp. the drop
glue) owns them.

> This section is the precise content behind "implicitly drops"/"goes out of
> scope": end-of-scope is where drop *and* the linear leak check happen, driven by
> the ownership state Σ, not by syntax.

### 5.7 Divergence and never-coercion

Prose `3.4` gives Rue its **single** type coercion: the never type `!`
(`never` here) coerces to any type. The core realizes this as one subsumption
rule, plus typing rules that give the diverging expression forms type `never`.

The expressions that *have* type `never` are those that transfer control away
instead of yielding a value to their context (`3.4:1/2`): `return e`, `break`,
and an infinite `loop { e }` — one whose body **syntactically contains no
`break` targeting it**, the same purely syntactic classification the prose
(`4.8:21`) and the compiler use; reachability of a `break` that is present is
not consulted. (Surface `continue` elaborates to the loop's back-edge and is
likewise never-typed; it is not a distinct core form. `@panic(...)` **is** a
never form — it elaborates to a diverging call of type `!`, matching `3.4:2`
(which lists it among the control-transfer forms) and the compiler's HM, AIR,
and CFG contracts, which all type a `@panic` expression at `!`; its *dynamics*
are the `↯user` trap of §6.12. This resolves the former RUE-512 question in
favour of `!`-typing: `@panic` participates in never-coercion (Sub-Never)
exactly like `return`, so it may inhabit any value context. `@assert` is **not**
a never form — it returns on the success path and is typed `unit`.)

```
  Γ;Σ;Λ ⊢ e ⇒ T_ret ⊣ _        T_ret = the enclosing function's declared return type
  ─────────────────────────────────────────────────────── (Return)
  Γ;Σ;Λ ⊢ return e ⇒ never ⊣ ⊥

  ───────────────────────── (Break)      -- well-formed only inside a loop; hands unit to the loop
  Γ;Σ;Λ ⊢ break ⇒ never ⊣ ⊥

  Γ;Σ;Λ ⊢ e ⇒ unit ⊣ _        e contains no `break` targeting this loop (syntactic — 4.8:21)
  ─────────────────────────────────────────────────────── (Loop-Div)
  Γ;Σ;Λ ⊢ loop { e } ⇒ never ⊣ ⊥
```

`break` yields no value to its *own* context, so its type is `never`; the "value
unit" of the grammar (§2) is what it hands to the enclosing loop, not the type of
the `break` expression. The outgoing state `⊥` ("diverged") is exactly the `⊥`
that §5.5's join excludes: a branch ending in one of these forms contributes no
ownership state to the merge.

A `loop` that *is* exited by a `break` — the complement of (Loop-Div)'s
syntactic premise, and the target of every elaborated `while` — is `unit`-typed
(`4.8:21`: type `()` even when every `break` is unreachable; the classification
is the same purely syntactic one as (Loop-Div)'s). Its ownership story has two
halves: the **back edge**, which re-enters the body, and the **break edges**,
which exit it.

```
  Γ;Σ;Λ ⊢ e ⇒ unit ⊣ Σ_back        e contains a break targeting this loop (syntactic — 4.8:21)
  Σ_back = Σ  on every path rooted outside the loop            -- back-edge invariance
  Σ_exit = join( Σ_b1, ..., Σ_bk )                             -- the k at-break states (below), §5.5's join
  ─────────────────────────────────────────────────────── (Loop-Break)
  Γ;Σ;Λ ⊢ loop { e } ⇒ unit ⊣ Σ_exit
```

- **Back-edge invariance.** The body is typed once, under the entry state `Σ`,
  and its fall-through state `Σ_back` must equal `Σ` on every path rooted
  outside the loop. That makes `Σ` a fixpoint of the body by *requirement*
  rather than by iteration — one typing pass covers every iteration, with no
  dataflow limit construction. The premise is what rejects a move of an outer
  binding that the body does not restore before the back edge (use-after-move
  on the second iteration; the compiler agrees — E0205) while admitting the
  move-then-reassign idiom, since (Assign) restores `Owned` (§5.2). Paths
  rooted *inside* the loop are exempt: a loop-local binding's scope ends
  within the iteration, so it does not survive to be compared.
- **The at-break states.** Each of the `k` occurrences of `break` targeting
  this loop contributes the ownership state `Σ_bi` in force where it fires,
  restricted to paths rooted outside the loop — read (Break) as *delivering*
  `unit` at `Σ_bi` to its innermost enclosing loop while its own context sees
  `never ⊣ ⊥`. The loop's outgoing state is §5.5's `join` over exactly these
  delivered states: a linear-carrying path must agree across every break
  (`3.8:50` — consumed on only some exits is ill-formed), and a merely
  Affine/Copy path moved on some break but not another joins to `MovedOut`
  conservatively, with the dynamic per-path drop flag of `3.8:73` dropping it
  on exactly the exits that did not move it. Loop-local bindings live at a
  break are not part of any `Σ_bi`; their §5.6 obligations are discharged at
  the break itself, where their scopes end (dynamically, §6.10's unwind).
- **Compiler divergence (RUE-1293).** The compiler enforces the back-edge
  premise but currently computes the loop-exit state from the entry state
  instead of joining the break-edge states, so it accepts a post-loop use of
  a value moved on a break path — an accepted use-after-move, and an
  observable double-drop when the type has drop glue. The core states the
  rule the prose join discipline (`3.8:50/51`) requires; the compiler is the
  artifact in the wrong (RUE-305).

A `never`-typed expression is accepted wherever a value of any type is expected —
this is the coercion, stated as **subsumption on the bottom type** (`3.4:3/4`):

```
  Γ;Σ;Λ ⊢ e ⇒ never ⊣ Σ'
  ───────────────────────── (Sub-Never)      -- for any type T
  Γ;Σ;Λ ⊢ e ⇒ T ⊣ Σ'
```

Because `never` has no values (`3.4:1`), this coercion is vacuously sound: there
is no run-time value to convert, so re-typing a diverging expression at `T`
cannot misclassify any value. It also creates no ownership obligation: `never` is
zero-sized (`3.4:9`) and §3 sets `class(never) = Copy`, so a `never`-typed
expression has nothing to move, drop, or leak, and (Sub-Never) leaves `Σ'`
untouched.

(Sub-Never) is what makes §5.5's (If)/(Match) admit a diverging arm while their
premises still demand a single common type `T`. In
`if c { 5 } else { return 0 }` the `else` arm has type `never`, which
(Sub-Never) re-types to `i32` to meet the `then` arm; the whole `if` is `i32`,
and since the `else` arm's outgoing state is `⊥` the branch join is just the
`then` arm's state. When *every* arm diverges (`3.4:6`, e.g.
`if c { return 1 } else { return 0 }`), the principal type is `never`, which
(Sub-Never) then coerces to whatever the surrounding context needs (there, the
function's `i32` return type). This is the only coercion **on values**: every
other value-typing rule demands exact type identity. Mode-position compatibility
for by-reference arguments is a separate relation on places and views and does
not create a stored value of another type. Its symbol, used in §2's note and
§6.13.2, is defined to exactly its current extent (RUE-1279 — previously used
but never introduced): **`⊳` relates the type of a place passed by reference to
the parameter type it may satisfy**. Over the core grammar of §2 — which has no
slice types — `⊳` is the identity relation, `T ⊳ T`, and every (Call)/(Fn) rule
already assumes exactly that. The non-identity instances are the slice-statics
work's to define, enumerated so the deferral is not silent: `[T; N] ⊳ [T]`,
`Str(N) ⊳ str`, `StrBuf ⊳ str`, and static string storage `⊳ str` — each minting
a `view⟨A | o, k⟩` (§6.13.2) at the by-ref argument position rather than
converting any stored value.

### 5.8 Leaf, operator, aggregate, and call forms

§5.5 gave the rules for the branching and enum forms (`if`/`match`, enum
intro/elim) and §5.7 the diverging forms (`return`/`break`, (Loop-Div)) together
with the break-exited loop ((Loop-Break), RUE-1278). This subsection completes
the statics with the remaining **non-diverging** expression forms of §2 —
literals, primitive arithmetic/bitwise, equality compare, struct and array
construction, and calls — closing the "thin statics" gap (RUE-308).
None of these adds ownership machinery beyond the *use* discipline of §4.2 and
the loan discipline of §5.4; they are collected here so §5 covers every §2
expression form, not because they introduce anything new. Each threads the
ownership state Σ left-to-right through its subexpressions (evaluation order
`4.0`). The diverging forms are **not** restated here (they live in §5.7), and
`match`/enum construction are **not** restated here (they live in §5.5).

**Literals.** A literal denotes a fresh `Copy` value of its own type and reads no
place, so Σ is unchanged.

```
  lit is an integer / bool / unit literal of type T       -- T ∈ { int(w,s), bool, unit }
  ─────────────────────────────────────────────────────── (Lit)
  Γ;Σ;Λ ⊢ lit ⇒ T ⊣ Σ
```

An integer literal's width and signedness are fixed *before* the core: elaboration
has already resolved the surface "an integer literal defaults to `i32` unless the
context requires another type" (`4.1:3`) into a concrete `int(w, s)`, so the core
sees only the resolved type (`4.1:2` for integers, `4.1:5` for `true`/`false`,
`4.1:7` for `()`).

**Primitive arithmetic / bitwise `⊕`.** Both operands share one integer type and
the result has that same type; the operands are `Copy` scalars, so each is an
ordinary value-context use that *copies* (§4.2) and the only effect on Σ is
whatever uses the operand subexpressions themselves perform.

```
  Γ;Σ;Λ ⊢ e1 ⇒ int(w,s) ⊣ Σ1     Γ;Σ1;Λ ⊢ e2 ⇒ int(w,s) ⊣ Σ2     ⊕ ∈ { +,-,*,/,%, &,|,^,<<,>> }
  ─────────────────────────────────────────────────────────────────────────────────────── (Arith)
  Γ;Σ;Λ ⊢ e1 ⊕ e2 ⇒ int(w,s) ⊣ Σ2
```

The same-type/same-result-type shape is `4.2:1`. Overflow, division-by-zero, and
remainder-by-zero are *dynamic* panics (§6), not typing errors. (Equality `≟` is
a separate rule below because, unlike `⊕`, it *borrows* rather than copies its
operands.)

**Unary operators `⊖`.** The three unary forms are typed like one-operand
`⊕`: the operand is a `Copy` scalar, used by value (a copy), and the result
has the operand's type — `bool` for `not`. Negation demands a *signed*
operand (`4.2:6`; rejecting `neg` on unsigned is `4.2:14`); `not` demands
`bool` (`4.4:2`); `bitnot` any integer (`4.3a:3/4`). Their dynamics are in
§6.4 (`neg` traps on `min_T`; `not`/`bitnot` are total).

```
  Γ;Σ;Λ ⊢ e ⇒ int(w,signed) ⊣ Σ'                Γ;Σ;Λ ⊢ e ⇒ bool ⊣ Σ'            Γ;Σ;Λ ⊢ e ⇒ int(w,s) ⊣ Σ'
  ───────────────────────────── (Neg)           ───────────────────── (Not)      ─────────────────────────── (BitNot)
  Γ;Σ;Λ ⊢ neg e ⇒ int(w,signed) ⊣ Σ'            Γ;Σ;Λ ⊢ not e ⇒ bool ⊣ Σ'        Γ;Σ;Λ ⊢ ~ e ⇒ int(w,s) ⊣ Σ'
```

**Ordering compare `⋚`.** Ordering works **only on integers** (`4.3:5`;
ordering a bool, string, unit, or aggregate is rejected, `4.3:6`), so unlike
`≟` there is no borrow subtlety: both operands are `Copy` scalars and their
occurrences are ordinary value-context uses (copies), exactly as for `⊕`.

```
  Γ;Σ;Λ ⊢ e1 ⇒ int(w,s) ⊣ Σ1     Γ;Σ1;Λ ⊢ e2 ⇒ int(w,s) ⊣ Σ2     ⋚ ∈ { <, >, <=, >= }
  ─────────────────────────────────────────────────────────────────────────── (Ord)
  Γ;Σ;Λ ⊢ e1 ⋚ e2 ⇒ bool ⊣ Σ2
```

This closes the citation from prose `4.3:1`/`4.3:5`, which referenced the §6.4
ordering dynamics before these static rules existed (RUE-526).

**Equality compare `≟`.** Comparison of two values of the same type yields `bool`.
Unlike `⊕`, a place appearing **directly** as an operand is read through a
call-scoped *shared loan* and is **not** moved, even when its type is `Affine` or
`Linear` — this side condition overrides the default (Use-Move) of §4.2 for the
operand position, exactly the equality-compare loan of §5.4 (`4.3:3f`).

```
  Γ;Σ;Λ ⊢ e1 ⇒ T ⊣ Σ1     Γ;Σ1;Λ ⊢ e2 ⇒ T ⊣ Σ2     ≟ ∈ { ==, != }
  a place operand p is read through a (root(p), shared) loan and is NOT moved  (§4.1, §5.4, 4.3:3f)
  ─────────────────────────────────────────────────────────────────────── (Eq)
  Γ;Σ;Λ ⊢ e1 ≟ e2 ⇒ bool ⊣ Σ2
```

The result is always `bool` (`4.3:1`); equality is defined for scalars, `unit`,
strings, and the aggregate types — structs, arrays, enums (`4.3:2`) — and
recurses structurally without ever consuming an operand. Because a place operand
leaves Σ untouched, its move obligation is undischarged: `let c = a; a ≟ b` is
well-formed, and two shared reads are always consistent, so `a == a` is too
(§5.4). Σ2 reflects only the uses performed *inside* compound operand
subexpressions (e.g. `f() == g()`), never a move of a directly-named operand
place.

**Struct construction.** Each field initializer is a value-context use of the
declared field type — a move for a non-`Copy` field, a copy for a `Copy` one
(§4.2) — and the result owns every field.

```
  S = struct { f1: T1, ..., fk: Tk }        all k fields supplied, each exactly once  (3.6:5, 3.6:6, 3.6:15)
  Γ;Σ;Λ ⊢ e1 ⇒ T1 ⊣ Σ1     Γ;Σ1;Λ ⊢ e2 ⇒ T2 ⊣ Σ2     ...     Γ;Σ_{k-1};Λ ⊢ ek ⇒ Tk ⊣ Σk
  ─────────────────────────────────────────────────────────────────────────────── (Struct-Intro)
  Γ;Σ;Λ ⊢ S { f1: e1, ..., fk: ek } ⇒ S ⊣ Σk
```

Initializers are typed in the order written and Σ is threaded through them,
matching the source-order evaluation of `3.6:16`/`4.0:9` even though the stored
value places each field in its *declaration* slot (`3.6:9`). A well-formed
literal supplies every field exactly once (`3.6:5`, `3.6:6`); a surface literal
written field-out-of-order (`3.6:15`) is presented here in declaration order by
elaboration without loss of generality. The result owns all fields, so
`class(S)` is the field join of §3.

**Array construction.** All `n` elements share one element type `T`, and the
result has type `[T; n]`.

```
  Γ;Σ;Λ ⊢ e1 ⇒ T ⊣ Σ1     Γ;Σ1;Λ ⊢ e2 ⇒ T ⊣ Σ2     ...     Γ;Σ_{n-1};Λ ⊢ en ⇒ T ⊣ Σn      n ≥ 0
  ─────────────────────────────────────────────────────────────────────────────── (Array-Intro)
  Γ;Σ;Λ ⊢ [ e1, ..., en ] ⇒ [T; n] ⊣ Σn
```

Every element is a value-context use of `T` (`3.5:2` — one shared element type),
typed left-to-right with Σ threaded, and the array owns all `n` elements;
`class([T; n])` is given by §3 (`3.5:1` for the type form). The empty array `[]`
(`n = 0`) is the `Copy`, zero-sized `[T; 0]` and uses nothing.

**Call.** A call's type is the callee's return type; its arguments are checked
against the parameter list, each in its call-site mode. By-value arguments are
value-context uses that thread Σ; by-reference arguments take a call-scoped loan
and leave Σ unchanged, per §5.4.

```
  g : fn ( m1 x1:T1, ..., mm xm:Tm ) -> Tr        -- the (monomorphic) signature of g
  the m argument forms a1..am match the m parameters in count and mode  (4.10:3)
  for each i, threading Σ left-to-right (Σ0 = Σ):
      a_i = e          (mi = ∅):       Γ;Σ_{i-1};Λ ⊢ e ⇒ Ti ⊣ Σi                          -- by value: value-context use, §4.2
      a_i = borrow p   (mi = borrow):  fully-owned(Σ_{i-1},p);            Σi = Σ_{i-1};  add (root(p), shared)    to Λ_call   -- §5.4
      a_i = inout p    (mi = inout):   fully-owned(Σ_{i-1},p), p mutable; Σi = Σ_{i-1};  add (root(p), exclusive) to Λ_call   -- §5.4
  Λ_call is CONSISTENT  (law of exclusivity, §5.4)
  for every (r, _) in Λ_call: fully-owned(Σm,r)       -- loans begin at call entry, after all argument evaluation
  ─────────────────────────────────────────────────────────────────────────────── (Call)
  Γ;Σ;Λ ⊢ g ( a1, ..., am ) ⇒ Tr ⊣ Σm
```

The result type is the callee's return type `Tr` (`4.10:5`); the argument count
must match the parameter count (`4.10:3`) and each argument's type its parameter
(`4.10:4`). A by-value argument moves a non-`Copy` value into the parameter and
copies a `Copy` one (§4.2), recording that in Σ; a `borrow`/`inout` argument
leaves Σ unchanged and instead contributes a loan to `Λ_call`, which must satisfy
the law of exclusivity (§5.4). Those loans begin at call entry, not at the
syntactic point where their argument forms are checked, so the final `Σm` must
still fully own every loaned root. This rejects a same-argument-list loan plus
consuming or partial move of the same root while preserving access-point
evaluation patterns where a read finishes before the `inout` access begins.
The three views agree on this rule (the RUE-523 reconciliation recorded by
RUE-526): the entry recheck here is prose `6.1:36`, which the compiler
enforces as E0208 — the core stated the rule first, the prose and compiler
followed. The
loans are second-class — released when the call returns — so the outgoing Σm
carries only the moves performed by the by-value arguments. Because the core is
fully monomorphic (§1), `g` names a single concrete signature: there is no
overload or generic instantiation to resolve at the call.

**Accessor calls (ADR-0062, preview).** A *read accessor* is a method of the
form

```
  A.f : fn ( borrow self : A, x1:T1, ..., xk:Tk ) -> borrow T { e_guard ; yield p_y }
```

whose body is well-formed iff every non-diverging exit is the single trailing
`yield` of a place `p_y` rooted at the receiver parameter — a projection chain
`self.f…[e]…` (possibly through a nested accessor call), whose guards `e_guard`
either diverge (trap, `@panic`) or fall through, with an empty post-`yield`
continuation. Value parameters are by-value (prose `6.6:4`–`6.6:7`). A call
produces a **borrowed place**, not a value:

```
  Γ;Σ;Λ ⊢ receiver place p ⇒ A     fully-owned(Σ, p)
  for each i, threading Σ left-to-right (Σ0 = Σ):
      Γ;Σ_{i-1};Λ ⊢ e_i ⇒ Ti ⊣ Σi                       -- by-value guard inputs, §4.2
  A.f is a well-formed read accessor with element type T
  add (root(p), shared) to Λ_expr    -- extent: the enclosing FULL EXPRESSION, not the call
  Λ_expr ∪ Λ_call of every call in that extent is CONSISTENT   (law of exclusivity, §5.4)
  ─────────────────────────────────────────────────────────────────────── (Accessor-Call)
  Γ;Σ;Λ ⊢ p.f(e1, ..., ek) ⇒ borrowed-place T ⊣ Σk
```

The **full expression** of an occurrence (RUE-1279 — the loan extent above,
previously undefined) is the largest enclosing §2 expression that is not itself
a proper subexpression of another: walk outward from the occurrence and stop at
the first *sequencing position* — the bound expression `e1` of
`let μ x = e1; e2`, the discarded `e1` of `e1; e2`, the right-hand side of
`assign`, the operand of `return`, the scrutinee of an `if`/`match`, or an
entire arm body, loop body, or function body. The expression occupying that
position is the full expression; an accessor loan taken anywhere inside it
lives until precisely that expression has reduced to a value (or been
discarded by an unwinding form, §6.9/§6.10). This is the same granularity at
which §6.7's temporaries die, so the loan cannot outlive any storage it
depends on.

The result is usable in **place contexts only**: it may be read (a `Copy`-shaped
read; reading out an owning value would mint a second owner and is rejected —
the same argument as the RUE-651 `get` gate), projected further, passed as a
`borrow` argument, or compared (§5.4's compare loan). It may **not** be
returned, stored, bound by a `let`, or captured in an aggregate — each escape
would let the loan outlive its extent (prose `6.6:9`–`6.6:11`). Unlike `(Call)`'s
loans, which are discharged at the call, the accessor loan joins the enclosing
full expression's loan set `Λ_expr` — the same extent generalization the §5.4
equality-compare paragraph anticipates — so an exclusive use of `root(p)`
anywhere in that extent is inconsistent (`use(v.get_ref(i), g(inout v))` is the
canonical rejection). This is the first construct that makes the dormant
"loaned in Λ" premises of §5.1/§5.2 observable within a single judgment; no new
§7 theorem shapes are introduced — the result's extent is bounded by its
expression, so second-classness, view-intact, loan-extent-nesting, and
handle-uniqueness quantify over it unchanged.

Dynamically an accessor call is not a `(D-Call)`: the call reduces **by the
accessor's inlined body** — the guards run in the caller (and may trap, §6.12)
and the redex is then replaced by the projected place itself, `(ℓ, π·π_y)` for
a user accessor over §6.9's by-ref place plumbing (a library accessor over the
allocation store would yield `view⟨A | o, k⟩`, §6.13.2 — deferred to the std
phase, RUE-1017). No call frame is pushed and no calling convention for
"returning a place" exists; that absence is the RUE-1012 forward-compatibility
contract.

---

## 6. Dynamic semantics (small-step)

This section gives the small-step operational semantics for **every** core form
of §2. It is the paper form of `crates/rue-oracle` — the executable reference
interpreter that runs a core program and produces its exit code, its `@dbg`
output, its panics, and its drop trace, and is differential-tested against the
compiler on the RUE-50 corpus. Each rule group below cites the oracle function
or CFG-instruction arm that realizes it (in `crates/rue-oracle/src/lib.rs`;
cited by *name*, not line number, so the citations survive the oracle growing —
RUE-526), so the paper semantics and the
executable semantics are one artifact read two ways: **the thing that governs the
spec is the thing we can run.** Where a rule and the interpreter disagree, one of
them is a bug (RUE-305) — that is the point of pinning both.

> The oracle interprets over the compiler's typed **CFG**, not directly over the
> §2 surface AST; the CFG is that AST after control flow is made explicit
> (`if`/`match`/`loop` become `Branch`/`Switch`/`Goto`, `let`/`;` become straight-
> line SSA). The reduction below is over the §2 forms; the correspondence is the
> obvious one, noted per group. Any place where the two could observably differ
> is a differential-test obligation.

### 6.1 The machine configuration

```
  Allocation ids A ∈ AllocId                 -- abstract allocation identities (the RUE-390 ruling: allocations, not addresses)
  Locations      ℓ ∈ Loc ⊂ AllocId           -- binding allocations: one single-cell allocation per live let-binding or by-value parameter
  Cell contents  c ::= v | ⊘                 -- ⊘ = uninitialised / moved-out (the dynamic image of Σ's absence/MovedOut, §5)
  Allocations    a ::= [ c1, …, cn ]         -- live: n ≥ 0 cells (a binding allocation always has exactly one)
                     | †                      -- dead: the identity is spent, its storage gone, and it is never reused (§6.13)
  Store          H : AllocId ⇀ a             -- the allocation store
  Values         v ::= n_T                    -- a scalar integer n of type T = int(w,s), with min_T ≤ n ≤ max_T
                     | b                       -- b ∈ { true, false }
                     | ⟨⟩                      -- unit
                     | { v1, …, vk }_S         -- a struct-S value (fields in declaration order)
                     | [ v1, …, vn ]           -- an array value (elements in ascending index)
                     | Kj⟨ v1, …, va ⟩         -- an enum value: variant tag Kj (0-based index j) + payload v1..va (a = 0 ⇒ just the tag)
                     | buf⟨A⟩                  -- an owned buffer handle: the opaque identity of a buffer allocation (§6.13)
                     | view⟨A | o, k⟩          -- a second-class view of k cells of allocation A starting at cell o (§6.13)
  Environment    ρ : Var ⇀ Loc × Path        -- per frame: each in-scope binding → its root cell and a projection path
                                             --   (RUE-1279: declared here once, not silently widened at §6.9. Every
                                             --   by-value binding has path ε, and ρ(x) = ℓ abbreviates ρ(x) = (ℓ, ε)
                                             --   throughout §6.3–§6.8; only §6.9's by-ref parameter bindings carry a
                                             --   non-ε path, composed under any further projection)
  Scope record   s = [ℓ1, …, ℓq]             -- cells owed a drop at this scope's exit, in creation order (dropped newest-first)
  Frame          φ = ⟨ ρ ; σ ⟩ ,  σ = [s1, …, sr]   -- a stack of r ≥ 1 open scopes; σ's top is the innermost scope
  Control stack  K ::= halt                    -- bottom: nothing pending; a returned value is the program result
                     | ret(E, φ) · K           -- a caller suspended in evaluation context E (§6.2), frame φ, awaiting a callee's value
                     | loopβ(e_body, φ) · K    -- a loop boundary: its body e_body and the frame φ to resume; break unwinds to here
  Config         C ::= ⟨ H ; φ ; K ; e ⟩       -- active frame φ evaluating expression e
                     | ↯κ                        -- halted in a trap of category κ ∈ { overflow, div-zero, rem-zero, bounds, user } (exit 101)
                     | ✓n                        -- halted normally with process exit code n
```

The reduction relation is `C → C'`. The store `H` is global (locations never
alias across frames except through the by-reference sharing of §6.9); `φ` and `K`
carry the per-call control state the sketch attributed to `K`. This matches the
oracle's `Interp`/`Frame` types: a `Frame` is `φ`, its
`locals`/`params` vectors are the cells reachable through `ρ`, and its evaluation
proceeds block-by-block exactly as `E`-decomposition proceeds redex-by-redex.

The store's allocations come in two kinds, distinguished only by how they are
minted (the RUE-390 ruling: stack storage and dynamically allocated buffers
are both abstract allocations, separated only where the semantics requires
it). **Binding allocations** `ℓ ∈ Loc` are minted by `(D-Let)`, `(D-Match)`,
and `(D-Call)`, always hold exactly one cell, and are retired by the scope
exit that drops them; `H(ℓ) = c` and `H[ℓ ↦ c]` throughout §6.3–§6.11
abbreviate the one-cell forms `H(ℓ) = [c]` and `H[ℓ ↦ [c]]`, so every rule in
those sections reads unchanged. **Buffer allocations** are minted, read,
written, resized, and retired only by the machine operations and container
defining equations of §6.13; no §2 expression form touches one directly. A
fresh identity is one not in `dom(H)`; dead allocations stay in the domain as
`†`, so an identity is never reused — which is what makes a stale `buf⟨A⟩` or
`view⟨A | o, k⟩` permanently dead rather than accidentally valid again.

`n_T` records the value's integer type because overflow, comparison signedness,
and bitwise width all depend on it; the oracle carries the same information out of
band on each CFG instruction's `ty` field. A discriminant-only enum value
`Kj⟨⟩` is stored as its bare tag (the oracle's `Value::Int` tag); a
payload-carrying `Kj⟨v1..va⟩` as the tagged aggregate (`Value::Aggregate`, RUE-285).

Four **scope helpers** on frames, used by the rules below, all defined in terms of
the drop relation `drop(H, ℓ)` of §6.11 (which is itself a no-op on a `⊘` or
`Copy` cell, so these fold harmlessly over non-droppable bindings):

```
  drop-retire(H, ℓ)              = drop(H, ℓ) [ℓ ↦ †]              -- scope-exit teardown of one binding: run its drop, then retire it
  push-scope(⟨ρ;σ⟩)              = ⟨ρ; [] :: σ⟩                    -- open a fresh, empty innermost scope
  run-scope-drops(H, ⟨ρ; s::σ⟩) = drop-retire the cells of s newest-first, yielding H'; result frame ⟨ρ; σ⟩   -- close ONE (innermost) scope
  run-all-scope-drops(H, φ)      = iterate run-scope-drops until φ has no open scopes (whole-frame teardown, on return)
  unwind-drops(H, φ', φ)         = run-scope-drops repeatedly on φ' until its open-scope stack equals φ's (break: down to a boundary)
```

A scope gains a cell to drop when a `let` (§6.7) or a `match` arm (§6.6) binds one;
`inout`/`borrow` parameters are deliberately never recorded (§6.9), which is how
they escape the drop obligation (§5.6). Retiring the binding allocation after
its drop is the RUE-390 change: a scope-exited cell's identity is dead, so any
residual access to it — which a well-typed program never performs (§7,
no-use-after-drop) — is *stuck* rather than silently readable. Overwrite-drop
(§6.8) and `@drop` (§6.11) do **not** retire: the binding stays live and
reinitializable there.

### 6.2 Evaluation order: contexts, search, and panic propagation

Evaluation is left-to-right, inheriting the prose order `4.0:3–9` verbatim (the
same order §5 threads Σ through). This is fixed by a grammar of single-hole
**evaluation contexts** `E`, whose hole marks the one subexpression reduced next:

```
  E ::= [·]
      | E ⊕ e  | v ⊕ E                                   -- arithmetic / bitwise / shift operands, left then right
      | E ≟ e  | v ≟ E                                   -- equality operands (the operand is BORROWED, not moved — §6.3)
      | ⊖ E                                              -- unary neg / not / bitnot
      | S{ f1=v1, …, f_{i-1}=v_{i-1}, fi=E, f_{i+1}=e, … }  -- struct field i (earlier fields already values)
      | [ v1, …, v_{i-1}, E, e_{i+1}, … ]                -- array element i
      | Kj( v1, …, v_{i-1}, E, e_{i+1}, … )              -- enum payload component i
      | E . f  | E [ e ]  | v [ E ]                       -- projection base, then index
      | g( v̄, …, E, … )                                  -- a by-VALUE call argument (a by-ref arg is a place, not reduced — §6.9)
      | if E { e1 } else { e2 }                          -- scrutinee
      | match E { … }                                    -- scrutinee
      | let x = E ; e2                                   -- bound expression (e2 not entered until E is a value)
      | E ; e2                                           -- discarded expression
      | assign p = E                                     -- right-hand side (p's index subexpressions reduce first, below)
      | return E
      | endscope(ℓ̄) in E                                 -- the administrative scope-close form of §6.7 (RUE-1277: without
                                                         --   this context, a let's body could never take a step)
```

A place `p` used in value context (the `e ::= p` production) is a redex once its
index subexpressions are values; the contexts `E[e]`/`v[E]` reduce those indices
left-to-right first (as `resolve_path` does). The two structural
rules that drive every reduction:

```
  ⟨ H ; φ ; K ; r ⟩ → ⟨ H' ; φ' ; K' ; e' ⟩          -- r is a redex reduced by a §6.3–§6.11 rule
  ───────────────────────────────────────────────────────────────── (Search)
  ⟨ H ; φ ; K ; E[r] ⟩ → ⟨ H' ; φ' ; K' ; E[e'] ⟩

  a redex rule fires ↯κ                                -- a trap (§6.12)
  ─────────────────────────────────── (Panic-Lift)     -- for any context E
  ⟨ H ; φ ; K ; E[r] ⟩ → ↯κ
```

(Search) also carries `Call`/`Return`/`break` steps that rewrite `φ`/`K`; those
appear below with the frame explicit. (Panic-Lift) is why a trap anywhere
abandons the whole configuration: a panic is not a value and no context can
consume it, so it propagates to the top and halts (`Interp::run`).

`return v` and `break` have the same *shape* of behavior — no context can
consume them, so they discard the context around them — but unlike a panic
they unwind **with drops**: `(D-Return)`/`(D-Return-Value)` (§6.9) and
`(D-Break)` (§6.10) fire on `E[return v]` / `E[break]` for any context `E`,
discarding `E` (including any pending `endscope` markers inside it) and
running the discarded scopes' drops from the frame's scope records σ instead.
That is why §6.7 registers every binding's cell in σ *as well as* in its
`endscope` marker: the marker is the normal-path close, and σ is what survives
when an unwinding form throws the marker away (RUE-1277).

### 6.3 Literals and the use of a place (copy / move)

A literal is already a value; it takes no step except to *be* one. Its width and
signedness were resolved by elaboration (§5.8), so the machine stores the concrete
`n_T` / `b` / `⟨⟩` (`Const`/`BoolConst`).

Using a place `p` in value context is the operational side of §4.2 / §5.1. Let
`p` resolve, under ρ, to a root cell `ℓ` and an evaluated projection path `π`
(field indices and already-reduced array indices, §6.2); write `H(ℓ)@π` for the
sub-value reached by following `π` into `H(ℓ)`, and `H[ℓ@π ↦ ⊘]` for the store
with that sub-position replaced by the moved-out marker. Reading navigates the
stored aggregate exactly as `place_read` does.

```
  ρ(root(p)) = ℓ      H(ℓ)@π = v      class(T) = Copy           -- T the type of p (§5)
  ─────────────────────────────────────────────────────────────────── (D-Use-Copy)
  ⟨ H ; φ ; K ; p ⟩ → ⟨ H ; φ ; K ; v ⟩                         -- the cell is left untouched

  ρ(root(p)) = ℓ      H(ℓ)@π = v      class(T) ∈ {Affine, Linear}
  ─────────────────────────────────────────────────────────────────── (D-Use-Move)
  ⟨ H ; φ ; K ; p ⟩ → ⟨ H[ℓ@π ↦ ⊘] ; φ ; K ; v ⟩               -- whole- or partial-place move; source becomes ⊘
```

`(D-Use-Move)` writes `⊘` at exactly the sub-position moved (the whole cell for a
whole-place use, one field/element for a projection — the *partial move* of
§4.2), so the later scope-exit drop of `ℓ` (§6.11) skips it and cannot free it a
second time. In a **well-typed** program `H(ℓ)@π` is never `⊘` when a use fires —
that is the no-use-after-move theorem (§7); the oracle nonetheless treats a read
of an absent cell as a hard error (`get_local`) so a violation is
caught rather than masked. The oracle achieves the `⊘`-marking *statically* — the
compiler's drop elaboration omits the drop of a moved place, so no runtime marker
is needed (`run_drop`'s note); the paper machine marks it
dynamically. The two are observably identical: the same values are dropped the
same number of times.

Equality operands are the exception (§4.1, §5.4, `4.3:3f`): the operand place is
**read through a shared loan and not moved**, so even an `Affine`/`Linear`
operand steps by `(D-Use-Copy)` in the `E ≟ e` / `v ≟ E` positions, leaving its
cell `Owned`. This is the dynamic image of the equality-borrow side condition; the
oracle simply reads both operands without disturbing storage (`cmp`).

### 6.4 Primitive operators

All operands are `Copy` scalars, already reduced to `n_T` (or `b`) by §6.2.

**Arithmetic `+ - *` and unary `neg`** compute over ℤ and **trap on overflow** —
Rue arithmetic never wraps (`3.1:6/13`). Let `n1 ⊕_ℤ n2` be the exact integer
result:

```
  min_T ≤ (n1 ⊕_ℤ n2) ≤ max_T          ⊕ ∈ { +, -, * }
  ───────────────────────────────────────────────────── (D-Arith)
  (n1)_T ⊕ (n2)_T  →  (n1 ⊕_ℤ n2)_T

  (n1 ⊕_ℤ n2) < min_T   or   (n1 ⊕_ℤ n2) > max_T
  ───────────────────────────────────────────────────── (D-Arith-Trap)
  (n1)_T ⊕ (n2)_T  →  ↯overflow
```

`(D-Arith)`/`(D-Arith-Trap)` are `arith` + `range_check`: the interpreter computes in `i128` (wide enough that no host
overflow precedes the range check) and traps when the result leaves `[min_T,
max_T]`. `neg` is the unary case: `neg (min_T)_T → ↯overflow` because `-min_T >
max_T` for a signed `T` (`Neg`).

**Division and remainder `/ %`** add two extra traps before the range check
(`divmod`):

```
  n2 ≠ 0    ¬(s = signed ∧ n1 = min_T ∧ n2 = -1)      q = n1 quot n2 (truncated toward zero)
  ───────────────────────────────────────────────────────────────────────────── (D-Div)
  (n1)_{int(w,s)} / (n2)_{int(w,s)}  →  (q)_{int(w,s)}

  (n2)_T = 0_T                                    ───────────────────────── (D-Div-Zero)
                                                  (n1)_T / (n2)_T → ↯div-zero

  s = signed    n1 = min_T    n2 = -1             ─────────────────────────── (D-Div-Overflow)
                                                  (n1)_T / (n2)_T → ↯overflow
```

`%` is identical with `q` the truncated remainder `n1 rem n2`, trapping
`↯rem-zero` on a zero divisor and `↯overflow` on `min_T % -1` (the hardware
`idiv` faults there even though the mathematical remainder is 0 — `divmod`'s remainder arm).

**Comparison `≟` and the ordering compares `< > <= >=`** yield `bool`
(`cmp`). Scalars compare by their integer value, respecting
signedness (the value `n_T` already carries the sign). Only `==`/`!=` may reach an
aggregate (ordering on aggregates is a §5 type error); there they compare
**structurally** — a struct field-by-field, an array element-by-element, an enum
same-tag-and-equal-payload, recursing into nested aggregates (RUE-285) — and a
each canonical text rung (`str`, `Str(N)`, or `StrBuf`) by its byte content (`4.3:2`):

```
  v1 ≈ v2  ⟺  v1 and v2 are structurally equal      (scalars by value; aggregates componentwise; strings by content)
  ─────────────────────────────────────────────────────────────────────────────────── (D-Eq)
  v1 == v2 → (v1 ≈ v2)                v1 != v2 → ¬(v1 ≈ v2)
```

**Bitwise `& | ^ ~` and shifts `<< >>`** operate on the `w`-bit two's-complement
representation and never trap (`bitop`/`shift`). Write `β_w(n)` for
the `w`-bit pattern of `n` and `val_{w,s}(β)` for its reinterpretation at
signedness `s`:

```
  ⊛ ∈ { &, |, ^ }        β = β_w(n1) ⊛_bits β_w(n2)
  ───────────────────────────────────────────────────── (D-Bit)
  (n1)_{int(w,s)} ⊛ (n2)_{int(w,s)} → ( val_{w,s}(β) )_{int(w,s)}

  (n1)_{int(w,s)}  ~  → ( val_{w,s}( ¬β_w(n1) ) )_{int(w,s)}       -- bitwise complement (BitNot)
```

Shifts mask the shift amount modulo the operand width `w` (`4.3a:10`); `>>` on a
signed type is arithmetic (sign-replicating), on an unsigned type logical
(`shift`):

```
  k = amt mod w        β = β_w(n) shifted left by k, masked to w bits
  ───────────────────────────────────────────────────────────────── (D-Shl)
  (n)_{int(w,s)} << (amt)_T → ( val_{w,s}(β) )_{int(w,s)}

  k = amt mod w        β = ( arithmetic-if-signed / logical-if-unsigned ) right shift of β_w(n) by k
  ───────────────────────────────────────────────────────────────── (D-Shr)
  (n)_{int(w,s)} >> (amt)_T → ( val_{w,s}(β) )_{int(w,s)}
```

`not` on `bool` is `not true → false`, `not false → true` (`Not`).

### 6.5 Aggregate introduction and projection

A struct, array, or enum literal is a redex once **all** its components are values
(the `E` contexts of §6.2 reduce them left-to-right, threading `H`). It steps to
the corresponding aggregate value, owning every component (`StructInit`/
`ArrayInit`):

```
  ────────────────────────────────────────────────── (D-Struct)
  S{ f1=v1, …, fk=vk } → { v1, …, vk }_S

  ────────────────────────────────────────────────── (D-Array)
  [ v1, …, vn ] → [ v1, …, vn ]                       -- (n ≥ 0; the empty array [] is the zero-sized [T;0])
```

Projection in value context is subsumed by the place-use rules of §6.3 (a
projection `p.f` / `p[e]` is a place); an **array index is bounds-checked** at the
moment the path is navigated, and an out-of-range or negative index traps
(`resolve_path`/`place_read`):

```
  0 ≤ i < n
  ───────────────────────────────── (D-Index)         -- [v0,…,v_{n-1}] @ [i] = vi (then §6.3 copies or moves)
  reading [ v0, …, v_{n-1} ][ i ] yields vi

  i < 0   or   i ≥ n
  ───────────────────────────────── (D-Index-Trap)
  reading [ v0, …, v_{n-1} ][ i ] → ↯bounds
```

### 6.6 Enum introduction and the `match` elimination

Enum construction evaluates its payload left-to-right (§6.2) and builds the tagged
value; a discriminant-only variant is just its tag (`EnumVariant`):

```
  ────────────────────────────────────────────────── (D-Enum-Intro)
  E::Kj( v1, …, va ) → Kj⟨ v1, …, va ⟩                -- a = 0 ⇒ the bare tag Kj⟨⟩
```

`match` first reduces its scrutinee to an enum value `Kj⟨v1,…,va⟩` (a
value-context **use** of the scrutinee, §5.5: a move for a non-`Copy` enum — its
source cell became `⊘` by §6.3 — a copy otherwise). The tag `Kj` selects the one
covering arm (exhaustiveness, §5.5, guarantees exactly one), which **binds the
payload components to fresh cells** and reduces the arm body in a new scope owning
those cells; the other arms never come into being (`Terminator::Switch` +
`EnumPayloadGet`):

```
  arm j is  Kj(x1, …, xa) => ej        ℓ1..ℓa fresh        ρ' = ρ[ x1↦ℓ1, …, xa↦ℓa ]
  H' = H[ ℓ1↦v1, …, ℓa↦va ]           φ = ⟨ρ; s::σ⟩        φ' = ⟨ρ'; (s ++ [ℓ1,…,ℓa])::σ⟩
  ────────────────────────────────────────────────────────────────────────────────── (D-Match)
  ⟨ H ; φ ; K ; match Kj⟨v1,…,va⟩ { …, Kj(x1..xa) => ej, … } ⟩  →  ⟨ H' ; φ' ; K ; endscope([ℓ1,…,ℓa]) in ej ⟩
```

The payload cells are bound exactly as a `let` binds one (§6.7): appended to the
innermost scope record *and* owed to the arm's `endscope` marker, so their drops
run **when the arm's body becomes a value** — the arm's end, `6.3:17`'s timing —
rather than at some later frame pop, and an unwinding `return`/`break` inside
the arm still finds them in σ after discarding the marker (RUE-1277; an earlier
draft pushed a whole scope here and never closed it on the value path). The
scrutinee value is **consumed** by the match: its payload now lives in the
`ℓi`, so it is not dropped again, and each `xi` is an ordinary `Owned` binding
governed by §6.11 at the arm's end (a `Linear` payload the arm neither moves nor
consumes is the leak the statics already rejected; an `Affine` one is dropped
once, newest-first with its sibling bindings by `(D-EndScope)`). This is the
operational content of "binding a variant's payload moves it out; a moved-out
payload runs its destructor exactly once when its binding leaves scope"
(`6.3:17`, `6.3:20`). `if` is the two-armed boolean special case:

```
  ─────────────────────────────────────── (D-If-T)          ─────────────────────────────────────── (D-If-F)
  if true { e1 } else { e2 } → e1                            if false { e1 } else { e2 } → e2
```

`if`'s arms are entered directly (they open scopes for their own `let`-bindings by
§6.7); the boolean scrutinee is `Copy`, so no drop attends the branch itself.

### 6.7 `let`, sequencing, and scope-exit drop

`let x = v ; e2` allocates a fresh cell for `x`, binds it, and reduces the body in
a scope that **owes `x` a drop**. That debt is recorded in **two places at
once**, and the redundancy is load-bearing (RUE-1277): the cell is appended to
the frame's innermost open scope record `s` — so the frame-level unwinding of
`return` (§6.9) and `break` (§6.10) can find and drop it — *and* the body is
wrapped in the administrative runtime form `endscope(ℓ̄) in e` (not a §2 surface
form), which is the normal-path close: it runs the drops of exactly the cells
`ℓ̄` when `e` has become a value, and removes them from the scope record so no
later exit drops them again.

```
  ℓ fresh        φ = ⟨ρ; s::σ⟩
  ─────────────────────────────────────────────────────────────────── (D-Let)
  ⟨ H ; φ ; K ; let x = v ; e2 ⟩ → ⟨ H[ℓ↦v] ; ⟨ρ[x↦ℓ]; (s ++ [ℓ])::σ⟩ ; K ; endscope([ℓ]) in e2 ⟩

  φ = ⟨ρ; s::σ⟩        ℓ1,…,ℓq is a suffix of s (see below)
  ─────────────────────────────────────────────────────────────────── (D-EndScope)     -- ℓ̄ dropped-and-retired newest-first (§6.1)
  ⟨ H ; φ ; K ; endscope([ℓ1,…,ℓq]) in v ⟩ → ⟨ drop-retire(H, ℓq) ; …; drop-retire(H, ℓ1) ; ⟨ρ; (s minus ℓ1..ℓq)::σ⟩ ; K ; v ⟩
```

where `drop(H, ℓ)` is the drop relation of §6.11 (a no-op on a `⊘` or `Copy`
cell). The suffix side condition is an invariant, not a check the machine
performs: cells are appended to the innermost record in creation order, an
`endscope` closes the most recently created ones, and nothing between a
binding's creation and its `endscope` can leave a *younger* cell in the same
record (a nested `let`'s or `match`'s marker closes before the enclosing one by
expression nesting; a nested `loop` pushes and — by `(D-Loop-Iter)`/
`(D-Break)` — fully pops its *own* scope records; a call runs in its own
frame). Nested `let`s nest their `endscope`s, so cells are dropped in **reverse
declaration order** (RAII) — the innermost/newest binding first.

The environment `ρ[x↦ℓ]` is never restored when the binding dies, and no rule
needs it to be: elaboration **α-renames** binders so every binding in a
function body has a distinct name (the Barendregt convention — surface
shadowing, `3.8:12/13`, is resolved *by renaming* before the core, like every
other surface-to-core translation in §2's absent-by-design list). A dead
binder's name is therefore never looked up again — Γ-scoping (§5) already
rejects any occurrence outside the binder's `e2` — so the stale `ρ` entry is
unobservable. A bare sequence
`e1 ; e2` evaluates `e1` to a value and **discards** it; because §5.3 guarantees
`e1` carries no linear value, the discarded temporary is simply dropped (a no-op
for a `Copy` value) and control passes to `e2`:

```
  ─────────────────────────────────────────────────────────── (D-Seq)
  ⟨ H ; φ ; K ; v1 ; e2 ⟩ → ⟨ drop(H, v1) ; φ ; K ; e2 ⟩       -- drop the discarded temporary, then continue
```

(The oracle realizes both via the compiler's explicit `Drop` CFG instructions,
which its elaboration inserts at exactly these scope/temporary boundaries and the
interpreter executes with `run_drop`; `drop(H, v)` on an already-
owned temporary value is `drop` on a cell whose contents is `v` and never `⊘`.)

### 6.8 Assignment: overwrite-drop and reinitialisation

`assign p = v` stores `v` into the cell/sub-position `p` denotes. If that position
currently holds an `Owned` droppable value, it is **dropped first** (overwrite-
drop, §5.2, `3.8:55`); reinitialising a `⊘` (moved-out) position drops nothing.
The result is `⟨⟩` and the position becomes `Owned` (`place_write`):

```
  ρ(root(p)) = ℓ      H(ℓ)@π = c      H1 = ( drop(H, c-at-ℓ@π) if c ≠ ⊘ else H )      H2 = H1[ ℓ@π ↦ v ]
  ─────────────────────────────────────────────────────────────────────────────────────────────────── (D-Assign)
  ⟨ H ; φ ; K ; assign p = v ⟩ → ⟨ H2 ; φ ; K ; ⟨⟩ ⟩
```

(The compiler elaborates the overwrite-drop as an explicit `Drop` emitted before
the store, so the oracle's `place_write` needs only to overwrite — the drop
instruction ran first; consistent with `3.8` overwrite-drop.) By the (Assign)
premise (§5.2, `3.8:77`), whenever the dropped value `c ≠ ⊘` here has a type that
carries a linear value the program was rejected statically — so this rule only ever
drops non-linear (`Affine`/`Copy`) contents on overwrite, and a linear position it
reaches is necessarily `⊘` (nothing to drop). No linear value is implicitly dropped
at an assignment.

### 6.9 Calls, parameters, and return

A call `g(a1, …, am)` evaluates its arguments left-to-right (§6.2). A by-value
argument reduces to a value `vi` that is **moved or copied into** the parameter
cell (per §4.2 — the source place, if any, was already marked `⊘` by §6.3). A
by-reference argument `inout p` / `borrow p` is **not** reduced to a value:
instead the parameter cell *is* the argument place's cell — the callee and caller
share storage for the call's dynamic extent, so a write through an `inout`
parameter is visible to the caller on return (`6.1:18`), and a `borrow` parameter
is read-only. Let `g` be `fn g(m1 x1:T1, …, mm xm:Tm) -> Tr { e_body }`:

```
  for each i:  arg a_i  is  either  a value v_i (by-value: fresh cell ℓ_i, H'(ℓ_i)=v_i, parameter path ε)
                          or  a by-ref place p_i resolving to root cell ℓ_i = ρ(root(p_i)) and projection path π_i
  ρ_g = [ xi↦(ℓ_i, ε) for by-value params; xi↦(ℓ_i, π_i) for by-ref params ]
  φ_g = ⟨ ρ_g ; [ [by-value ℓ_i only] ] ⟩        -- by-ref params owe NO drop (§5.6)
  ────────────────────────────────────────────────────────────────────────────────────────────── (D-Call)
  ⟨ H ; φ ; K ; E[ g(a1,…,am) ] ⟩ → ⟨ H' ; φ_g ; ret(E, φ)·K ; e_body ⟩
```

The callee's entry scope owes a drop **only** for the by-value parameter cells;
`inout`/`borrow` parameters are owned by the caller and are exempt (§5.6,
`3.8:62`). A by-ref binding carries both the root cell and the projection path
that was passed; reading or writing parameter `xi` therefore reaches exactly the
field/element argument, not the caller's whole root cell. By-value bindings are
the special case with projection path `ε`; earlier notation such as
`ρ(root(p)) = ℓ` refers to the first component of this binding and composes the
stored parameter path with the source projection `π`. When the body reduces to a
value `v`, the frame is popped: its open scopes' drops run (freeing the by-value
params and any still-live locals), and `v` is handed back to the suspended caller
context:

```
  ────────────────────────────────────────────────────────────────────────── (D-Return-Value)
  ⟨ H ; φ_g ; ret(E, φ)·K ; v ⟩ → ⟨ run-all-scope-drops(H, φ_g) ; φ ; K ; E[v] ⟩

  K_g = loopβ(_,_)·…·loopβ(_,_)·ret(E, φ)·K        -- zero or more loop boundaries, discarded with the frame
  ────────────────────────────────────────────────────────────────────────── (D-Return)
  ⟨ H ; φ_g ; K_g ; E'[return v] ⟩ → ⟨ run-all-scope-drops(H, φ_g) ; φ ; K ; E[v] ⟩

  K_main = loopβ(_,_)·…·loopβ(_,_)·halt
  ────────────────────────────────────────────────────────────────────────── (D-Return-Main)
  ⟨ H ; φ_main ; K_main ; E'[return v] ⟩ → ⟨ run-all-scope-drops(H, φ_main) ; φ_main ; halt ; v ⟩
```

`(D-Return-Value)` is the "a function evaluates to the value its body evaluates
to" rule of §4.3 — there is no implicit action, the body simply *is* an
expression that reduced to `v`. `(D-Return)` is the explicit form, and it fires
with `return v` **in any evaluation context `E'`** (RUE-1277 — the unwinding
analogue of (Panic-Lift), §6.2): `let y = (return 100); …` and
`1 + (if c { return 0 } else { 2 })` — the `3.4:5`/`3.4:6b` shapes — reduce by
discarding `E'`, every pending `endscope` marker inside it included, along with
any loop boundaries the frame pushed onto `K`. The drops those markers would
have run are not lost: every bound cell is also registered in the frame's scope
records (§6.7), and `run-all-scope-drops` walks exactly those records, so an
early return runs every live binding's drop, newest-first per scope
(`3.9:18` — verified against the compiler: an early return with two live
destructor-bearing locals runs both destructors, then the caller's). The frame
is then discarded and `v` handed to the suspended caller context (the oracle's
`Terminator::Return`, with the compiler having placed the pre-return `Drop`s).
`(D-Return-Main)` is the same firing at the bottom of the stack, where there is
no suspended caller: the result configuration is the value configuration that
§6.12's (Result-Ok) consumes. (Without these context forms, a nested `return`
was a *stuck* configuration — D-Return required the frame's whole expression to
be `return v`, which after one reduction under a `let` it never again was.) The oracle models `inout` by
**copy-in / copy-out** rather than true sharing — it copies the argument in, runs
the callee, then copies each `inout` parameter's final value back into the caller
place (the copy-in/copy-out note on `call`). Under the law of exclusivity (§5.4) an `inout`
place is unaliased for the call's duration, so copy-out is observably identical to
the shared-cell rule above; the paper machine takes the sharing form because it is
simpler to state and the two agree exactly on well-typed programs.

A call whose callee is an intrinsic with no core body (e.g. `@dbg` or
`@to_string`) reduces by the intrinsic's defining equation rather than by
`(D-Call)`; these are elaboration-level primitives, and the oracle dispatches them
directly (`@dbg` appends its argument's rendering
to the observable output). The core-form call rule above governs
every user function.

### 6.10 `loop` and `break`

`loop { e }` pushes a loop boundary and enters the body in a fresh scope; when the
body reduces to a value (necessarily `⟨⟩`, discarded), its scope drops run and the
loop **re-enters** its body — so a value's storage from one iteration is reclaimed
before the next, exactly as a `let` inside the loop body drops each turn (in the
oracle the loop is `Goto`/`Branch`/`Switch` back-edges, and the
compiler places a `Drop` on the back-edge that `run_drop` executes each turn).
`break` unwinds to the nearest loop boundary, running the drops of every scope it
discards, and the whole `loop` yields `⟨⟩`:

```
  ─────────────────────────────────────────────────────────────────── (D-Loop-Enter)
  ⟨ H ; φ ; K ; loop { e } ⟩ → ⟨ H ; push-scope(φ) ; loopβ(e, φ)·K ; e ⟩

  ─────────────────────────────────────────────────────────────────── (D-Loop-Iter)
  ⟨ H ; φ' ; loopβ(e, φ)·K ; v ⟩ → ⟨ run-scope-drops(H, φ') ; push-scope(φ) ; loopβ(e, φ)·K ; e ⟩

  ─────────────────────────────────────────────────────────────────── (D-Break)      -- unwinds scopes down to the loop boundary
  ⟨ H ; φ' ; loopβ(e, φ)·K ; E'[break] ⟩ → ⟨ unwind-drops(H, φ', φ) ; φ ; K ; ⟨⟩ ⟩
```

Like `(D-Return)`, `(D-Break)` fires with `break` **in any evaluation context
`E'`** (RUE-1277): `1 + (if c { break } else { 2 })` reduces by discarding
`E'` — pending `endscope` markers included — and running the discarded
bindings' drops from the scope records that §6.7 registered them in, via
`unwind-drops`. The innermost loop boundary is necessarily the top of `K`
(a `break` in a callee would be ill-formed, §5.7, and any inner loop the body
entered pushed — and by exiting, popped — its own boundary above this one).

`unwind-drops(H, φ', φ)` runs the scope-exit drops of every scope open in `φ'`
that is not already open in the enclosing `φ`. A `loop` containing no `break`
never fires `(D-Break)` and so runs forever — its static type is `never` (§5.7,
`Loop-Div`), consistent with its never yielding a value to its context. A loop
with one or more `break`s is (Loop-Break)'s `unit`-typed form (§5.7), and every
`break` fires the same `(D-Break)` regardless of which one it is. (There is no
value-carrying `break` anywhere: `break expr` is a compile-time error at the
surface, `4.8:22`.)

### 6.11 Drop

`drop(H, ℓ)` and `drop(H, c)` are the operational core of Rue's memory safety.
Dropping a cell holding `⊘` — a moved-out or uninitialised position — does
**nothing** (this single skip is what makes double-free impossible, §7). Otherwise
the value's user destructor, if any, runs **first**, then its droppable *contents*
drop in `3.9` order (`run_drop`):

```
  drop(H, ⊘)                       = H                                   -- moved-out / uninitialised: skip
  drop(H, n_T) = drop(H, b) = drop(H, ⟨⟩) = H                            -- scalars are Copy: nothing to drop
  drop(H, { v1,…,vk }_S)           = drop*( H , [v1,…,vk] )              -- S declares NO destructor: fields in DECLARATION order
  drop(H, { v1,…,vk }_S)           = drop*( H1[ℓ↦†] , [c1,…,ck] )        -- S declares a destructor: see the construction below
  drop(H, [ v1,…,vn ])             = drop*( H , [v1,…,vn] )              -- elements in ASCENDING index order
  drop(H, Kj⟨ v1,…,va ⟩)           = drop*( H , [v1,…,va] )              -- ONLY the ACTIVE variant Kj's payload (6.3:20)
```

where `drop*(H, [c1,…,cm])` folds `drop` over the list left-to-right. The
destructor case is a **nested machine run** — the formal shape of "the
destructor runs as an ordinary call" (RUE-1279; earlier drafts typed `dtor_S`
as a store function `H → H` while *saying* it could step and trap, with no
definition connecting the two):

```
  S declares  drop fn S(self) { e_dtor }        ℓ fresh
  ⟨ H[ℓ ↦ {v1,…,vk}_S] ; ⟨ [self↦(ℓ,ε)] ; [[]] ⟩ ; halt ; e_dtor ⟩  →*  ⟨ H1 ; _ ; halt ; ⟨⟩ ⟩
  H1(ℓ) = { c1, …, ck }_S               -- the RESIDUAL fields: ⊘ wherever the destructor moved one out
```

The destructor body runs in its own frame whose single scope record is
**empty**: `self` is exempt from the drop obligation (§5.6 — otherwise
dropping `self` would re-run the destructor, an infinite regress), so the
nested run's frame pop drops only the destructor's own locals. Afterward the
*residual* cell contents `c1,…,ck` — not the original `v1,…,vk` — drop in
declaration order, so a field the destructor consumed is `⊘` and skipped,
never dropped twice; the scratch cell `ℓ` is then retired. `drop` and `→` are
thus defined by **mutual recursion**, as the least pair of relations closed
under all the rules of §6; a `drop` whose nested run diverges makes the
enclosing configuration diverge, and one whose nested run traps `↯κ` makes the
enclosing configuration `↯κ` ((Panic-Lift) extends through the nesting). A
destructor is permitted to have no observable effect, but need not. For a **library
container** `S` — `StrBuf`, an `ArrayBuf(T)` instance — the destructor is a
source-defined `drop fn` whose body contains unchecked code, so in the model
it steps by the type's defining drop equation instead (§6.13.3: drop the live
buffer cells in ascending index order, skipping `⊘`, then retire the
allocation — never "no observable effect", which was the `@free`-as-no-op
vacuity RUE-390 closed). The **enum** case reads the runtime tag `Kj` to
recurse into the *active* variant's payload only: an inactive variant's payload
has no storage, and a discriminant-only active variant (`a = 0`) drops nothing
(`run_drop`'s enum arm). A payload already moved out by a `match` binding (§6.6) left the
enum place `⊘`, so it is skipped here and never dropped twice.

Because a destructor is a normal function, dropping can itself step the machine
(and can even trap — a destructor that overflows halts with `↯overflow`, exactly
as the oracle would, since `run_drop` calls back into `call`). Drops are therefore
sequenced, not atomic; `endscope`/`return`/`break`/overwrite all expand to `drop`
applications in the orders fixed above.

The surface intrinsic `@drop(p)` is the explicit version of the same relation.
For a non-`Copy` place resolving to `ℓ@π`, it runs `drop(H, H(ℓ)@π)`, writes `⊘`
back to `ℓ@π`, and returns `⟨⟩`; for a `Copy` place it returns `⟨⟩` without
changing the store. Thus an explicit drop consumes a linear value exactly once
and suppresses the later scope-exit drop through the original place.

### 6.12 Traps and the top-level result

The five trap categories — `overflow` (arithmetic, `neg`, `min_T / -1`),
`div-zero`, `rem-zero`, `bounds` (a negative or out-of-range array index), and
`user` (an explicit `@panic`) — each abandon the configuration to `↯κ` and halt
the program with the panic exit code of Appendix B (101), regardless of
surrounding context (§6.2, Panic-Lift). They are **total, deterministic, and
observable**: an alternate compiler must reproduce the same trap on the same
input (`3.1:6/13`, `8.1`–`8.3`).

The `user` category is `@panic`'s defining equation (RUE-526 — previously the
paper machine had no rule for it). `@panic` is an intrinsic (§6.9's
external-call note): it evaluates its message operand, appends
`panic: <message>` to the observable output, and abandons the configuration:

```
  ────────────────────────────────────────── (D-Panic)
  @panic(v_msg)  →  ↯user                    -- after emitting "panic: v_msg" to the observable output
```

Statically `@panic(…)` is a diverging `!`-typed call (§5.7); dynamically it
yields no value at all — `↯user` is lifted past every context by
(Panic-Lift). Verified against the compiler: a `@panic` prints its message and
exits 101, indistinguishable from the four machine traps at the process
boundary. (The RUE-512 typing question is settled at §5.7 in favour of `!`; this
dynamic equation is unchanged — `!`-typing only sharpens the static side,
letting `@panic` inhabit any value context via never-coercion.) The top-level
result is fixed by running `main` from the **initial configuration**, whose
three components are constructed from the elaborated program `P` (RUE-1279 —
previously named by the rules below but never built):

```
  H0     = one live allocation per distinct string literal occurring in P,
           minted before main and never retired (§6.13.2), and nothing else
  e_main = the body of P's unique  fn main
  φ_main = ⟨ ∅ ; [[]] ⟩            -- main takes no parameters: an empty environment
                                   --   and one open, empty scope record
```

```
  ⟨ H0 ; φ_main ; halt ; e_main ⟩ →* ⟨ H ; φ_main ; halt ; v ⟩
  ────────────────────────────────────────────────────────────── (Result-Ok)
  program result = ✓( n mod 256 )        where v = n_{int(32,signed)}, or ✓0 if v = ⟨⟩

  ⟨ H0 ; φ_main ; halt ; e_main ⟩ →* ↯κ
  ────────────────────────────────────────────────────────────── (Result-Panic)
  program result = ✓101
```

`main`'s returned `i32` is masked to a byte for the process exit code, and a
`unit`-returning `main` exits 0 (`Interp::run`); any trap exits 101
(`Interp::run`). These two rules, plus the observable `@dbg` output accumulated
during reduction and the exact runtime diagnostic emitted by a trap, are
precisely the `Outcome` (`exit_code`, `stdout`, `stderr`, `panic`) the differential
harness compares against the compiled binary (RUE-50). Both executions retain
stderr only up to the same fixed bound and reject overflow or a truncated native
prefix, so bounded observation cannot manufacture agreement.

The interpreter implementing this whole relation is the executable oracle
(`crates/rue-oracle`; README § "The executable oracle" / RUE-50). Every rule group
above names the function that realizes it, so a change to either must be mirrored
in the other or the differential tests will diverge — which is the mechanism that
keeps the paper semantics and the running semantics one artifact.

### 6.13 The allocation store: buffers, views, and container defining equations

This section is the ratified RUE-390 modeling decision (maintainer ruling,
2026-07-14): the machine models **abstract allocations**, not a language-level
heap. §6.1's store already gives every binding a single-cell allocation; this
section adds the multi-cell **buffer allocations** that back `ArrayBuf(T)` and
`StrBuf`, the machine operations on them, the two value forms that name them,
and the defining equations of the library container types over those
operations. What it deliberately does **not** add: a malloc-style heap, a
global allocator, an allocation algorithm, arenas or pages, address
arithmetic, or a reclamation policy. Those are runtime/library concerns (the
allocator design is RUE-878); an allocation here is an identity plus cells
plus liveness, nothing more. The payoff is stated in §7: use-after-free
becomes a *progress violation* — the safety theorems become falsifiable, and
provable, for exactly the types real programs use (the vacuity RUE-390
reported).

#### 6.13.1 Machine operations on buffer allocations

```
  mint(H, n)        = (A, H[A ↦ [⊘, …, ⊘]])      where A ∉ dom(H); n ≥ 0 cells, all ⊘
  H(A).i            = ci                           iff H(A) = [c0, …, c_{m-1}] and 0 ≤ i < m      -- cell read
  H[A.i ↦ c]                                       defined under the same conditions               -- cell write
  retire(H, A)      = H[A ↦ †]                    iff H(A) is live
  realloc(H, A, n') = (A', H2[A ↦ †])             where H(A) = [c0, …, c_{m-1}] is live,
                                                   (A', H1) = mint(H, n'),
                                                   H2 = H1[A'.i ↦ ci  for 0 ≤ i < min(m, n')]
```

Three commitments, each load-bearing:

- **Partiality is stuckness, not a trap.** An operation on a `†` allocation, or
  outside a live allocation's cells, has no rule — the configuration is
  **stuck**, deliberately distinct from the defined `↯` traps of §6.12. A trap
  is defined behavior an alternate compiler must reproduce; a stuck
  configuration is a state a well-typed program must never reach, which is
  precisely what gives §7's progress theorem content over buffers. The checked
  containers below establish every operation's precondition themselves (each
  equation bounds-checks against its own `len` before touching a cell, and
  traps `↯bounds` at *its* boundary), so their uses of these operations never
  stick — that unreachability is now theorem content, not vacuity.
- **Identities are never reused.** `mint` freshness is `A ∉ dom(H)`, and dead
  allocations remain in the domain as `†`, so no fresh identity ever collides
  with a retired one. A dangling handle or view is permanently dead — it cannot
  come back to life aliasing an unrelated later allocation. (This is the
  abstract form of provenance; concrete address reuse is the runtime's
  business, invisible here.)
- **`realloc` moves identity.** Growth mints a *fresh* allocation, copies the
  preserved cells, and retires the old identity (the ruling's stated initial
  model). Every stale copy of the old handle, and every view into it, is dead
  the moment the container grows — the dangling-after-realloc class that §5.4's
  deferred-view-equality warning describes is stuck here, not silently
  readable.

#### 6.13.2 Buffer handles and views

Two §6.1 value forms name allocations:

- `buf⟨A⟩` — an **owned buffer handle**. It is opaque: no §2 expression form
  and no §6.3–§6.11 rule operates on it; it exists only as the abstracted
  pointer field inside a library container's header struct (the `ptr mut T` of
  `std/arraybuf.rue`, the `ptr mut u8` of `std/strbuf.rue`'s header),
  and only the defining equations below touch the allocation it names. Every
  library container type declares a destructor, so its class is `Affine` (§3,
  never `@copy`) and core code cannot duplicate a header — and with it a
  handle — by (Use-Copy); handle uniqueness inside the *library* is obligation
  (O1) of §6.13.5.
- `view⟨A | o, k⟩` — a **second-class view**: cells `o … o+k-1` of allocation
  `A`. Views are the model's slices (`borrow [T]` / `inout [T]` / `str` —
  ADR-0043's second-class fat pointer, `ptr` + runtime `len`, abstracted to
  identity + offset + length). A view is created at a by-ref argument position
  by the mode-position compatibility relation `⊳` (§5.7's examples) and lives
  exactly as long as the call's loan (§5.4): it cannot be returned, stored, or
  otherwise escape, so its loan root — the container place it was created
  from — outlives it by construction. Reading through a view is `H(A).(o+i)`
  for `i < k`, with the range check performed by the view's own accessors (the
  `__rue_str_byte_at` equation of §6.13.4 traps `↯bounds` first); under the
  str ruling (RUE-386) a `Str(N)`/`StrBuf` borrow in `str` position *is* such
  a view, so that ruling and this model line up one-to-one.

A **static string literal** is an immortal live allocation: the initial store
`H0` (§6.12) contains one live allocation per distinct literal, minted before
`main` and never retired. `"hello" : str` is `view⟨A_lit | 0, 5⟩`, and its
`Copy`, storable, cannot-dangle status (ADR-0043's static-backed exemption
from the second-class rule) is literal: no reduction exists that could kill
`A_lit`.

One scope cut, stated so it is not silent: the equations below mint views only
over **buffer** allocations. A view of a *fixed stack array* (`borrow a[i..j]`
with `a : [T; N]`) still rides §6.9's by-ref place mechanism (a `(ℓ, π)`
binding), because a binding allocation holds its array as one structured cell,
not as `N` cells. Unifying the two — either by giving array-holding bindings
cell-vector allocations or by adding a path component to views — is the
slice-statics work this machinery was sequenced to unblock, deliberately not
decided here.

#### 6.13.3 `ArrayBuf(T)`: representation, invariant, and defining equations

`ArrayBuf(T)` is an ordinary source-defined library type (`std/arraybuf.rue`,
per ADR-0043 — not a compiler builtin), but its method bodies contain
`checked {}` blocks over the raw intrinsics, which are **outside the core**
(§2). The model therefore gives each public method a **defining equation**:
the same device §6.9 uses for intrinsics with no core body, applied at the
container's public boundary. The real body must refine its equation —
obligation (O4) of §6.13.5. An `ArrayBuf(T)` instantiation's values are

```
  { h ; len ; cap }_ArrayBuf(T)        h ::= buf⟨A⟩ | null        len, cap : u64
```

(`null` abstracts the no-allocation empty state — `@int_to_ptr(0)` in the
source.) The **representation invariant** `Inv` holds at every method
boundary — entry and exit of every defining equation, and at the destructor:

```
  Inv({ h ; len ; cap }):
    h = null      ⇒  len = cap = 0
    h = buf⟨A⟩    ⇒  H(A) is live with exactly cap cells;
                     cells 0 … len-1 are values of T (not ⊘);
                     cells len … cap-1 are ⊘
    class(T) ≠ Linear            -- the RUE-388 instantiation gate (@require_droppable, E0499)
```

Mid-equation the invariant may be broken (a grow is mid-flight between `mint`
and the header update); it must be re-established on exit — obligation (O2).
The `⊘` in cells `len … cap-1` is the RUE-390 "⊘-skip extension": per-cell
initializedness now exists for dynamically allocated elements, and the
destructor's skip below has a `⊘` to write for a buffer element exactly as
§6.11's has for a stack cell.

The defining equations: `self` is an `inout` place for the mutators, so
header updates write the caller's cell per §6.9's sharing rule. Each equation
is the model of the corresponding `std/arraybuf.rue` body (its `checked {}`
blocks over `@alloc`/`@realloc`/`@ptr_read`/`@ptr_write`/`@ptr_offset` and
friends):

```
  new()                     →  { null ; 0 ; 0 }                                  -- no allocation until first push
  with_capacity(0)          →  { null ; 0 ; 0 }
  with_capacity(n), n > 0   →  { buf⟨A⟩ ; 0 ; n }         where (A, H') = mint(H, n)

  len(borrow self)          →  self.len        capacity → self.cap        is_empty → self.len == 0

  reserve(inout self, additional):
    required = len + additional
    required ≤ cap  →  ⟨⟩                                   -- enough spare: no effect
    required > cap:    cap' = grow(cap, required);          -- amortized doubling: double from max(cap, 4) until ≥ required
                       (A', H') = ( mint(H, cap')           if h = null
                                  | realloc(H, A, cap')     if h = buf⟨A⟩ );     -- the old identity dies (§6.13.1)
                       h ↦ buf⟨A'⟩;  cap ↦ cap'   →  ⟨⟩

  push(inout self, x):
    reserve(self, 1);  let buf⟨A⟩ = h;                      -- h ≠ null after reserve: cap ≥ 1
    H[A.len ↦ x];  len ↦ len + 1                           →  ⟨⟩

  pop(inout self):
    len = 0   →  None
    len > 0:     v = H(A).(len-1);  H[A.(len-1) ↦ ⊘];  len ↦ len - 1   →  Some(v)
                                                            -- the element MOVES out: exactly one owner (RUE-651)
  get(borrow self, i):          -- well-formed only for trivially-droppable T (E0711, RUE-651)
    i < len   →  Some(H(A).i)                               -- a COPY; the cell is untouched
    i ≥ len   →  None

  set(inout self, i, x):
    i ≥ len   →  ⟨⟩                                         -- out of bounds: ignored (the source's contract)
    i < len:     drop(H, H(A).i);  H[A.i ↦ x]   →  ⟨⟩       -- old element dropped first (RUE-646): no leak

  clear(inout self):
    for i = 0 … len-1 ascending:  drop(H, H(A).i);  H[A.i ↦ ⊘]
    len ↦ 0                                       →  ⟨⟩     -- capacity kept

  free(inout self):
    clear's element drops;  retire(H, A) if h = buf⟨A⟩;
    h ↦ null;  len ↦ 0;  cap ↦ 0                  →  ⟨⟩     -- early release; the later destructor is then a no-op

  drop (the §6.11 library-container destructor):
    for i = 0 … len-1 ascending:  drop(H, H(A).i), skipping any ⊘
    retire(H, A) if h = buf⟨A⟩
```

In `pop`/`get`/`set`/`clear`, `A` names the handle's allocation implicitly:
the guard (`len > 0`, resp. `i < len`) plus `Inv`'s `h = null ⇒ len = 0`
forces `h = buf⟨A⟩` on every arm that touches a cell. The remaining methods
(`get_or`/`pop_or`, `first`/`last`, `index_of`/`contains`, `swap`/`reverse`,
`with_capacity`'s non-zero arm via `reserve`, the `from_str`/`extend_from`
bridges) are compositions of the equations above plus ordinary core
evaluation; they add no new machine operation. Notes, each carrying a
citation:

- **`pop` writes `⊘`** where the source merely decrements `len` past the slot:
  observably identical (the cell is beyond the new `len` either way, and `Inv`
  reclassifies it), but the `⊘` states *why* the destructor will not drop it —
  the returned value is now the element's sole owner. This is `3.9`'s
  exactly-once discipline, mechanized for heap elements.
- **`get` copies**, which is why it is gated to trivially-droppable `T` at the
  surface (E0711, RUE-651): a by-copy read of a drop-glue element would create
  a second owner of that element's own buffer — in the model, two values
  holding the same inner `buf⟨A⟩`, violating (O1) and double-freeing at drop.
  The equation makes the aliasing visible. The borrow-returning read that
  would lift the gate (`get_ref`, RUE-662) must produce a §5.4-governed loan,
  not a value.
- **Growth is identity death** (§6.13.1's `realloc`): a view into the old
  buffer held by an enclosing call would now be stuck — but §5.4's exclusivity
  already rejects that shape (`v[0..2] == g(inout v)`): the `inout` loan
  needed to reach `push` conflicts with any live view of `v`. The model and
  the loan rules close the same hole from opposite sides.
- **The destructor's `⊘`-skip** covers mid-equation states and future move-out
  APIs (a `swap_remove`, a `take_at`) as well as `pop`'s retired slot; under
  `Inv` at boundaries the skip fires only past `len`, so drop glue runs
  exactly once per live element, ascending (`3.9` order, RUE-646).
- **Capacity policy** (`grow`: double from `max(cap, 4)` until ≥ required)
  mirrors the source's current contract and is observable only through
  `capacity()`; a policy change is a library change that amends the equation,
  not a soundness matter. `StrBuf`'s floor is 16 (§6.13.4).
- **Resource exhaustion is outside the model.** `mint`/`realloc` are total
  here; the real bodies fail fast — a byte-size arithmetic overflow or a null
  allocation is `@panic("out of memory")` — per ADR-0043's explicit
  fallible-allocation non-decision. This is a stated (O4) refinement escape:
  the implementation may abort where the model allocates, and nothing else.

#### 6.13.4 `StrBuf`, `Str(N)`, and `str`

`StrBuf` is the `u8` refinement of the trio's growable rung plus the
byte-string convention (ADR-0043; the RUE-386 two-types ruling), and like
`ArrayBuf` it is **source-defined** (`std/strbuf.rue`) over the byte-oriented
unchecked intrinsics (`@alloc`/`@realloc`/`@free`/
`@byte_read`/`@byte_write`/`@byte_copy`, ADR-0059), so the same
defining-equation device applies at its public boundary. Its value is
`{ h ; len ; cap }_StrBuf` over `u8` cells, with one representation twist the
source pins in its header comment: **`cap = 0` is the non-owning state.**

```
  Inv({ h ; len ; cap }_StrBuf):
    cap > 0   ⇒  h = buf⟨A⟩, H(A) live with exactly cap cells, cells 0 … len-1 bytes, cells len … cap-1 ⊘
                 (the value OWNS A: it retires A at free/drop)
    cap = 0   ⇒  h = null and len = 0                          -- new()
              ∨  h names an immortal static-literal allocation with ≥ len live byte cells (§6.13.2)
                 (NON-owning: free/drop must not retire it — a literal-backed value)
```

The `cap = 0` literal-backed state is why a `StrBuf` built from a literal is
safe to store indefinitely: the allocation it does not own is one of `H0`'s
immortal literals, which no reduction can retire. Mutation of such a value
first performs **literal promotion** (`grow`'s `cap = 0` arm): mint a fresh
allocation, copy the live cells, and only then write — the non-owned
allocation is never written through and never retired. Equations, as `u8`
instances of §6.13.3 with these deltas:

- `grow(self, additional)` is `reserve` with the promotion arm: `cap = 0`
  mints and copies (never `realloc`s an allocation it does not own); `cap > 0`
  is §6.13.3's `realloc` arm. The doubling floor is 16.
- `push`/`append_byte` appends one raw byte; a byte ≥ `0x80` may make the
  content invalid UTF-8, which the byte-string model permits (ADR-0035) —
  strictness lives at the decode boundary only.
- `push_str(self, other)` **consumes** `other` (a by-value `Self` — its
  header's move obligation discharges into the call, §4.2) and appends its
  live cells; `append_borrowed`/`append_str`/`append_bytes` are the
  non-consuming forms over a `borrow` loan. `concat` mints a third allocation
  and copies both operands' cells.
- `clone`/`copy` **mint** a fresh allocation and copy the live cells — always,
  even for an empty or literal-backed value — a deep copy with a new
  identity, never a second handle to the same allocation. Likewise every
  cross-container bridge (`from_str`, `from_bytes`, `to_bytes`,
  `from_byte_range`): the source's own contract line — "no bridge aliases or
  transfers either container's private allocation" — is obligation (O1)
  stated in the library's voice.
- The trapping index form `s[i]` (`StrBuf` or a `str` view) checks `i < len`
  (resp. `i < k` for `view⟨A | o, k⟩`) and traps out of range exactly like
  array indexing (§6.5; ADR-0035's byte indexing); the machine cell read it
  then performs is in range by construction, so it never sticks. (The source
  spells the trap `@panic("index out of bounds")` pending a source-accessible
  bounds-trap primitive — its comment tracks that; observably it is the
  canonical bounds diagnostic. The Option-returning `byte_at` is the
  non-trapping companion.)
- `clear` drops nothing (`u8` is `Copy`) and resets `len` to 0, keeping the
  capacity. `free` retires only an owned allocation (`cap > 0`) and resets to
  `new()`'s state; the `drop fn` likewise retires only when `cap > 0` — the
  non-owning arm of `Inv` is what makes that conditional correct.
- Equality (§6.4's `≈`) on each canonical text rung compares **content** — the
  live cells in order (`equals_borrowed`) — never allocation identity: two
  distinct allocations with equal bytes are `≈`-equal (`4.3:2`).
- The UTF-8 **decode family** (`char_scalar`, `char_next`, and their `_lossy`
  variants, still runtime calls dispatched by the oracle) is deliberately
  **not pinned here**: its strict forms introduce a trap category (invalid
  UTF-8) that §6.12's taxonomy does not yet carry, so its equations belong to
  a string-decode amendment of their own. A cut, stated rather than silent.

`Str(N)` is `[u8; N]` plus the convention — a fixed array, already fully
inside §6.3–§6.11; a `Str(N)` borrow in `str` position becomes a view per
§6.13.2 (RUE-386).

#### 6.13.5 The library refinement obligation (RustBelt-style)

The §7 theorems quantify over core programs whose container-method calls step
by the equations above. The real implementations — the `checked {}` blocks of
`std/arraybuf.rue` and `std/strbuf.rue` over the raw and byte intrinsics —
are **unchecked code, outside the core by design** (§2): the core type system
does not verify them, and no amount of core soundness can. They carry instead
a stated proof obligation, discharged per method at the library boundary (by
review today; by mechanized verification when `03-metatheory.md` exists), in
exactly the position RustBelt gives `Vec`'s unsafe internals:

- **(O1) Unique handle.** No operation fabricates or duplicates a live buffer
  identity: at every method boundary, distinct live container values hold
  distinct allocations, and no other value holds any. This is the invariant
  that extends exclusivity (§7) to buffer roots: a buffer's cells are
  reachable only through its one owning header, so §5.4's root-granular loans
  on the header place cover the cells, and the root-separation lemma extends
  to allocations.
- **(O2) Boundary invariant.** Every method entered on a representation
  satisfying `Inv` re-establishes `Inv` at exit — and at every call it makes
  back into user code (element drop glue must observe the container
  mid-teardown only through values it owns).
- **(O3) Footprint.** A method touches only its own allocation(s) and its
  arguments — never another allocation, live or dead.
- **(O4) Refinement.** The method's observable behavior — result value, header
  effect, traps, `@dbg` output, and the cells' contents — equals its defining
  equation's.

A violation inside a checked block is a **library bug**, not a refutation of
the core theorems; conversely, the theorems say nothing about a program that
adds new unchecked code without discharging the same four obligations. This
conditionality is the ruling's Rust/RustBelt-style separation, stated rather
than hidden: the abstract machine names every addressable allocation and
proves safety over them, while allocators and container internals remain
replaceable abstractions with proof obligations.

#### 6.13.6 Oracle correspondence and the differential obligation

Today the oracle dispatches only the residual true builtins — `@to_string`
(both signednesses), the trapping `str` byte index, and the UTF-8 decode
family (`string_builtin`, with `preflight_string_builtin` enforcing the
modeled signatures) — over immutable byte-content values, a copy-in/copy-out
realization that, as with `inout` (§6.9), is observably identical to the
shared-allocation form *given* exclusivity and (O1). The container methods
themselves are ordinary CFG bodies to the oracle, and every intrinsic they
rest on is a registered **model gap** (`Intrinsic::Allocate` / `Reallocate` /
`Free` / `PointerRead` / `PointerWrite` / `ByteRead` / `ByteWrite` /
`ByteCopy` in the oracle's gap registry) — so `ArrayBuf` and `StrBuf`
programs currently fall outside differential coverage entirely. The
differential-test obligation this section creates: interpret the container
methods *at their public boundary, by these equations* — the same dispatch
strategy `string_builtin` uses for the residual builtins — rather than by
their raw bodies; the gap registry names the exact dispatch points to
replace. Until that lands, the §6.13.3/§6.13.4 equations are validated
against the compiler by the spec/CLI suites only, and the
equations-vs-source correspondence rests on review — a gap recorded here so
it is not mistaken for coverage.

---

## 7. Soundness — what we get to state, and then prove

With §5 (statics) and §6 (dynamics) precise, Rue's guarantees become *theorems*
rather than hopes. Stated now; proved in `03-metatheory.md`.

- **Type safety (progress + preservation).** A well-typed core program does not
  get stuck: it either reduces, halts with a value, or halts with one of the
  defined panics. Types are preserved under reduction. For `match`, progress rests
  on **exhaustiveness** (§5.5): a well-typed enum value carries one of the
  variants `K1..Kn`, and the arms cover exactly those, so some arm always
  matches — a `match` is never stuck on an uncovered tag.

- **No use-after-move.** In a well-typed program, the Use/Move dynamic rule is
  never applied to a `MovedOut` place. *Because:* `Use-Move`/`Use-Copy` (§5.1)
  require `Σ(p) = Owned`, and preservation maintains the invariant that Σ
  faithfully tracks the store's initialization.

- **No double-free.** Every stored value's destructor runs at most once. *Because:*
  a move sets `p ↦ MovedOut`, and the Drop rule (§6) skips MovedOut places — so a
  value that was moved out of a place is dropped through its *new* owner, never
  again through the old one. For an **enum** this holds through two mechanisms at
  once: the Drop rule recurses into the *active* variant's payload only, so an
  inactive variant's (non-existent) payload is never freed; and a payload moved
  out by a `match` binding leaves the enum place `MovedOut`, so the arm's binding
  becomes the sole owner and the scope-exit drop of the scrutinee is skipped
  (`6.3:20`). Neither the tag switch nor the match can free a payload twice.
  For **buffer cells** the same shape holds one level down (§6.13.3): `pop`
  and the teardown loops write `⊘` as they move or drop cells, the
  destructor's walk skips `⊘`, and `retire` is defined only on a live
  allocation — a second `free`, or a destructor after `free`, meets the
  `h = null` reset or sticks, and never frees twice. §6.13.1's never-reused
  identities close the aliasing half: a dead identity cannot come back as
  somebody else's allocation.

- **No use-after-drop / no leak of drops.** Every `Owned`, droppable,
  non-moved place is dropped exactly once, at the end of its scope, and never read
  afterward. *Because:* §5.6 schedules the drop, §6 executes it at the scope's
  close — the binding's `endscope` on the normal path, or the σ-walk of a
  `return`/`break` unwind or frame pop on the early-exit paths (§6.7, §6.9,
  §6.10; each cell is dropped by exactly one of these, since `endscope`
  un-registers what it drops and an unwind discards the markers whose cells it
  drops) — and Σ shows no path live past that point. Scope exit now also *retires* the
  binding's allocation (§6.1, `drop-retire`), so a read past the drop is
  **stuck** rather than silently possible — this bullet, too, is now
  falsifiable rather than structural. For buffers: every minted allocation is
  retired exactly once, by its unique owner's `free`, grow, or destructor
  (§6.13.3 under (O1)), so buffer storage neither leaks nor outlives its
  owner.

- **No use-after-free** *(new with RUE-390)*. In a well-typed program, under
  the §6.13.5 obligations, no reduction applies a §6.13.1 machine operation to
  a dead allocation: every `H(A)` an equation touches is live. *Because:*
  views are second-class — a view exists only inside a call whose loan covers
  the container place it was created from (§5.4), and while that loan is live
  the owner can be neither moved (Use-Move's loan premise) nor reached by a
  `free`/grow/destructor (exclusivity) — so no reduction that retires an
  allocation can fire under a live view; and handles are unique (O1), so no
  *other* place can reach the allocation to kill it. Previously this theorem
  was unstatable: `@free` was modeled as a no-op and buffer cells were not in
  `H` at all (the RUE-390 vacuity). Now a use-after-free is a **stuck
  configuration**, which progress forbids — the claim is falsifiable, and the
  oracle obligation of §6.13.6 is what will falsify it mechanically if the
  library or the equations are wrong.

- **Linear values are consumed exactly once.** No value whose type carries a
  linear value reaches end of scope `Owned` (§5.6 rejects it) or is discarded
  (§5.3 rejects it) or is consumed on only some paths (§5.5 join rejects it) or is
  **overwritten by an assignment while still live** (§5.2's (Assign) premise
  rejects it, `3.8:77` — the RUE-387 hole: without this premise the overwrite-drop
  of §6.8 would consume the old linear value implicitly); and no value is used
  twice (`Use-Move` consumes it). Hence exactly once. This
  now covers **enums**: `class(E)` is the payload join (§3), so a `Linear`-payload
  enum is itself `Linear` and `carries_linear(E)` holds (§5.3); letting it reach
  end of scope unconsumed is rejected exactly as for a linear struct (`6.3:19`),
  and consuming it by a `match` that binds and consumes the linear payload
  discharges the obligation (§5.5). Symmetrically, an `Affine`-payload enum is
  dropped exactly once at scope exit via its active payload (§5.6), unless a
  `match` already moved that payload out.

- **Exclusivity / no aliased mutation.** At any point in a well-typed reduction,
  no location is reachable through both a live exclusive loan and any other live
  access. *Because:* §5.4's `Λ` consistency admits either one exclusive or many
  shared loans of a root, never both, and loans are second-class (do not escape
  the call). This is the data-race-freedom precondition and the MVS invariant.
  Buffer cells inherit the guarantee through (O1): they are reachable only
  through their unique owning header, so a loan of that root is a loan of the
  cells — no new rule is needed, which is precisely why the RUE-390 ruling
  wanted allocations abstract.

The eventual metatheory proof also owes these explicit lemmas:

- **Loan/drop non-interference.** No live loan root may be in a scope record that
  is being dropped or in an overwrite target whose old contents are being
  dropped. This is what prevents drop glue from invalidating storage reachable
  through a live by-ref parameter.
- **Loan-extent nesting.** If a callee forwards a by-ref parameter as another
  loan, the inner loan's extent is dominated by the outer loan's call extent.
  This makes root-granular checking compose transitively across calls.
- **Root separation.** Distinct roots occupy disjoint storage in the tree store.
  Exclusivity is stated per root, so the proof must justify that different roots
  cannot alias the same mutable location.
- **View-intact.** For the lifetime of a loaned root, there is no `⊘` beneath the
  loaned place. This follows from `fully-owned` at loan creation plus the
  no-move-while-loaned premise, and it is the invariant the §6.13.2 view rules
  rely on when runtime indices prevent per-element ownership tracking.
- **Handle-uniqueness preservation.** Reduction preserves (O1): every live
  buffer allocation is named by exactly one live handle (plus, transiently,
  the views its loans justify). The §6.13 equations must be audited to
  preserve it (`clone` mints, `realloc` retires, `free` nulls the header) —
  and this lemma is the formal hook the RUE-388 lift (linear elements via
  container/element multiplicity propagation) will be designed against, per
  the ruling.

These seven are the memory-safety-without-GC claim, decomposed. Note which of
them was *unprovable* against the prose spec until now: all of them, because
each rests on "use", "moved", "consumed", "dropped" being defined — which
§3–§6 finally do. And note which were **vacuous for the flagship collection
types** until the allocation store: the double-free, use-after-drop, and
use-after-free bullets (RUE-390) — now stated over buffer cells, with their
conditionality on the library obligations made explicit in §6.13.5 rather
than hidden.

---

## 8. Traceability: prose paragraphs this core subsumes

For the spec-traceability discipline (`crates/rue-spec`), the correspondence so
far. As breadth is filled in, each new rule adds its citation. The §6 rows below
also correspond, function-for-function, to `crates/rue-oracle` — the executable
witness of the dynamic semantics (RUE-50), cited inline in each §6 rule group.

| Formal notion | Prose paragraphs it formalizes / replaces |
|---|---|
| §3 multiplicity lattice | 3.8:1–3, 3.8:14/16/18/20, 3.8:30/32/37, 3.8:57/58, 3.8:74, 3.9:31, 6.3:19 |
| §4.2 definition of *use* (+ §5.1 premises) | 3.8:5, 3.8:7, 3.8:9, 3.8:11, 3.8:22, 3.8:26, 3.8:33, 3.8:53, 3.8:68, 3.9:34 |
| §4.1/§5.4 equality borrows its operands | 4.3:3f |
| §5.5 match / enum elim + intro | 6.3:17, 3.8:33 (destructure), 4.7 (match) |
| §5.8 leaf/operator/aggregate/call statics | 4.1:2/5/7, 4.2:1/6/14, 4.3:1/2/5/6, 4.3a:3/4, 4.4:2, 3.6:5/6/15/16, 3.5:1/2, 4.10:3/4/5/7, 6.1:36 |
| §5.8 (Accessor-Call) + accessor body WF (preview, ADR-0062) | 6.6:2–6.6:12 |
| §5.6 enum drop (active payload) | 6.3:20 |
| §4.3 expression/return value | 4.5:3 (→ value, not just type), 6.1:4/5, 4.9:1/7 |
| §5.2 assignment / reinit | 3.8:55/56, 3.8:72, 3.8:77 |
| §5.3 discard leak check / explicit `@drop` | 3.8:64/65, 3.9:37–39 |
| §5.4 borrows / exclusivity | 6.1:14–35, 6.1:20, 6.1:30 |
| §5.5 branch join | 3.8:50/51, 3.8:73 |
| §5.6 scope exit: residual leak check + drop | 3.8:32/62/66, 3.8:74, 3.9 (drop order) |
| §5.7 divergence + never-coercion; (Loop-Div)/(Loop-Break) loop typing and the break-edge join | 3.4:1/2/3/4/6/6a/8, 3.4:9, 4.8:21, 3.8:50/51/73 |
| §6.2 evaluation order (contexts, left-to-right) | 4.0:3–9 |
| §6.3 dynamic use: copy vs. move; equality borrows | 3.8:5/7/22, 4.3:3f |
| §6.4 operator dynamics: arith/div/mod, compare, bitwise/shift | 4.2:1, 4.3:1/2, 4.3a:10, 3.1:6/13 |
| §6.5 aggregate intro + projection (bounds) | 3.5:2, 3.6:16, 4.11:14, 4.12:9, 8.2 |
| §6.6 enum intro + match dynamics | 6.3:17, 4.7:16 |
| §6.7/§6.8 let/seq/scope-drop (σ registration + `endscope`), assignment overwrite-drop; α-renamed shadowing | 4.5:3, 3.8:12/13, 3.8:55/64, 3.9 |
| §6.9 call / return / inout copy-out; nested-`return` unwind with scope drops | 6.1:4/5/18, 4.9:1/7, 3.4:5/6b, 3.9:18 |
| §6.10 loop / break dynamics | 4.8:18/21/22, 3.4:2 |
| §6.11 drop relation (active enum payload; skip moved; explicit `@drop`) | 3.9, 6.3:20 |
| §6.12 overflow/bounds/div-zero/`@panic` traps + exit code | 3.1:6/13, 4.13:5b, 8.1, 8.2, 8.3, Appendix B |
| §6.13 allocation store: handles, views, container equations | 3.7, 3.9 (drop order), 4.3:2 (string content equality); design citations: the RUE-390 ruling, ADR-0035/0041/0043, the RUE-386 str ruling, the RUE-388 linear-element gate |
| §7 soundness | the informal safety intent throughout ch. 3 and 8 |

---

## 9. Immediate open decisions for a maintainer

Collected from the **[open]** tags, for the design conversation before this is
locked:

1. **Comptime as elaboration (README).** Confirm the runtime core is formalized
   first and comptime/monomorphization is a separate later layer. (Recommended.)
2. **Raw pointers / `unchecked` out of the core initially (§2).** Model chapter 9
   as a marked extension that explicitly steps outside the §7 guarantees, rather
   than threading it through every rule. (Recommended.) The RUE-390 ruling
   keeps this: §6.13 models buffers as abstract allocations reached only
   through container defining equations — raw pointers themselves stay outside
   the core, and the containers' unchecked internals carry the §6.13.5
   obligations instead.
3. **Loans strictly second-class (§5.4).** Confirm loans never escape a call in
   the core; first-class references remain a deferred design question.
4. **Array index paths (§5).** Ownership tracks only *constant* index paths
   (matching `3.8:68/70`); dynamic-index moves are forbidden. Confirm this stays
   as the core rule (it is what keeps the ownership analysis decidable without
   dependent types).
