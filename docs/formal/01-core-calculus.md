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
      | e1 ≟ e2                -- primitive equality compare (== / !=); operands are BORROWED, not moved (§4.1, 4.3:3f)
      | S { f1: e1, ..., fk: ek }        -- struct value construction
      | E :: K ( e1, ..., em )           -- enum value construction (variant K of E with payload e1..em; m = arity of K)
      | [ e1, ..., en ]        -- array value construction
      | g ( a1, ..., am )      -- call of function g with argument forms a_i (see below)
      | if e0 { e1 } else { e2 }
      | match e0 { pat1 => e1, ..., patk => ek }
      | let x = e1 ; e2        -- binding; scope of x is e2
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
  F ::= fn g ( m1 x1: T1, ..., mk xk: Tk ) -> T { e }     -- m_i ∈ {∅, inout, borrow}

Programs
  P ::= D* F*                  -- with exactly one  fn main() -> (int(32,signed) | unit)
```

Notes on what is **absent by design** (lives in elaboration, `02-elaboration.md`):
`comptime`, comptime/`type` parameters, generics, method-call syntax, `Self`,
`&&`/`||` (→ `if`), `else if` (→ nested `if`), `while c { b }` (→
`loop { if c { b } else { break } }`), block syntax (→ `let`/`;` sequences),
`@import`/modules (→ a flat set of `F` after resolution), integer-literal *base*
(→ the value). **[open]** Raw pointers and `unchecked` code (chapter 9) are
initially *out* of the core and added as a distinguished, clearly-marked
extension; their whole point is to step outside the guarantees the core proves,
so they are modeled separately rather than threaded through every rule.

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
                  = Copy     if class(T) = Copy      -- includes every n when T:Copy
                  = Copy     if n = 0                -- a zero-length array carries nothing (3.8:74)

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

> This replaces the prose enumerations `3.8:2` (the Copy list, which includes
> discriminant-only enums), `3.8:3` (structs affine by default), `3.8:18/20`
> (`@copy` field constraint), `3.8:57/58` (carries-linear, infectious), and
> `6.3:19` (enum multiplicity = the payload join) with one lattice and one join.

*`@handle`* is not a fourth class: an `@handle` type is `Affine` (or `Linear`)
that additionally provides an explicit duplication operation `.handle()`. In the
core it is an ordinary Affine/Linear type plus a function `g_handle : S -> S`;
nothing about the class lattice changes. **[open]** Confirm we want `@handle` to
survive the formalization at all, or fold it into "any Affine type may define a
`clone`-like function" and drop the directive (a candidate simplification).

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
> the whole place.

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
    Γ : x ⇀ T          -- the declared type of each in-scope binding (immutable within a scope)

  Loan state (for exclusivity)
    Λ : set of currently-outstanding loans, each  ( root(p), {shared | exclusive} )
```

`Σ(p)` is the state recorded for the exact path `p`. A base path can be `Owned`
while one of its descendants is `MovedOut` after a partial move; rules that hand
an aggregate to another context therefore use the stronger predicate
`fully-owned(Σ,p)`, meaning `Σ(p)=Owned` and no path strictly under `p` is
`MovedOut`. A place `p` is **mutable** when its root is a `mut` local, an `inout`
parameter, or a mutable projection through either; immutable parameters and
`borrow` parameters are not mutable roots.

The main judgment:

```
    Γ ; Σ ; Λ  ⊢  e  ⇒  T  ⊣  Σ'
```

read: "under bindings Γ, starting ownership Σ and loans Λ, expression `e` is
well-formed, has type `T`, and leaves ownership Σ'." (Λ is scoped to a single call
and does not change across `e`; it is threaded only so the use/assign rules can
consult it — see 5.4. It is written on the turnstile, not on the output, for that
reason.)

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

  class(Γ ⊢ p : T) ∈ {Affine, Linear}    Σ(p) = Owned    p not loaned in Λ
  ───────────────────────────────────────────────────────────────────────── (Use-Move)
  Γ ; Σ ; Λ  ⊢  p  ⇒  T  ⊣  Σ[ p ↦ MovedOut,  and every path strictly under p removed ]
```

`Σ(p) = Owned` is the **no-use-after-move** premise (`3.8:5/24/53`): a use
requires the place currently owns its value. `Use-Move` records the consumption;
the `p not loaned` premise forbids moving a place that is currently borrowed
(`3.8`/exclusivity). Reading a place that is only *projected from* uses the
ProjRead rule instead:

```
  Σ(p) = Owned      -- the base must currently own its storage (3.8:53)
  ───────────────── (Owned-Base)         -- side condition used by p.f / p[e] in any context
```

### 5.2 Assignment and reinitialization

```
  Γ ; Σ ; Λ ⊢ e ⇒ T ⊣ Σ1       Γ ⊢ p : T       (mutability & loan side-conditions)
  p's prior value, if Owned and droppable, is dropped BEFORE the store (dynamic, §6)
  ─────────────────────────────────────────────────────────────────────────────── (Assign)
  Γ ; Σ ; Λ ⊢ (assign p = e) ⇒ unit ⊣ Σ1[ p ↦ Owned ]      -- reinitialization (3.8:55)
```

Assigning to a `MovedOut` place makes it `Owned` again (`3.8:55/56`). Assigning to
an already-`Owned` place drops the old value first (`3.8` overwrite-drop). Writing
*into* an array while any element is moved out is rejected by a side condition
(`3.8:72`).

### 5.3 Sequencing, discard, and the linear leak check

```
  Γ ; Σ ; Λ ⊢ e1 ⇒ T1 ⊣ Σ1         carries_linear(T1) = false        -- 3.8:64
  Γ ; Σ1 ; Λ ⊢ e2 ⇒ T2 ⊣ Σ2
  ─────────────────────────────────────────────────────────────────── (Seq)
  Γ ; Σ ; Λ ⊢ (e1 ; e2) ⇒ T2 ⊣ Σ2
```

Discarding a value whose type *carries a linear value* is ill-formed (`3.8:64` —
`carries_linear(T)` is `class(T) = Linear` lifted through the aggregates: the
field join for a struct, the element type for an array, and the **payload join
over all variants** for an enum (`6.3:19`) reaching Linear). Because `class` is
itself defined as exactly these joins (§3), `carries_linear(T) ⟺ class(T) =
Linear` for every type — including enums, whose class is the payload join.
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

- if `carries_linear(T)`: **ill-formed** — a linear value reached end of scope
  unconsumed (`3.8:32/62/66`). This is the must-use check.
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
and an infinite `loop { e }` (one with no reachable `break`). (Surface
`continue` elaborates to the loop's back-edge and is likewise never-typed; it is
not a distinct core form. `@panic(...)` is **not** a never form — it elaborates
to an ordinary call of type `unit`, matching `3.4:2`, which lists only these
control-transfer forms, and the compiler, which types a `@panic` expression at
`unit`.)

```
  Γ;Σ;Λ ⊢ e ⇒ T_ret ⊣ _        T_ret = the enclosing function's declared return type
  ─────────────────────────────────────────────────────── (Return)
  Γ;Σ;Λ ⊢ return e ⇒ never ⊣ ⊥

  ───────────────────────── (Break)      -- well-formed only inside a loop; hands unit to the loop
  Γ;Σ;Λ ⊢ break ⇒ never ⊣ ⊥

  Γ;Σ;Λ ⊢ e ⇒ unit ⊣ _        the loop body has no reachable `break`
  ─────────────────────────────────────────────────────── (Loop-Div)
  Γ;Σ;Λ ⊢ loop { e } ⇒ never ⊣ ⊥
```

`break` yields no value to its *own* context, so its type is `never`; the "value
unit" of the grammar (§2) is what it hands to the enclosing loop, not the type of
the `break` expression. The outgoing state `⊥` ("diverged") is exactly the `⊥`
that §5.5's join excludes: a branch ending in one of these forms contributes no
ownership state to the merge. (A `loop` that *is* exited by a `break` yields
`unit` at the break's state; formalizing multi-`break` loop ownership is
orthogonal to coercion and left to a future loop section.)

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
for by-reference arguments is a separate relation on places and views (for
example `[T;N] ⊳ [T]`, `Str(N) ⊳ str`, and static string storage `⊳ str`) and
does not create a stored value of another type.

### 5.8 Leaf, operator, aggregate, and call forms

§5.5 gave the rules for the branching and enum forms (`if`/`match`, enum
intro/elim) and §5.7 the diverging forms (`return`/`break`/`loop`). This
subsection completes the statics with the remaining **non-diverging** expression
forms of §2 — literals, primitive arithmetic/bitwise, equality compare, struct
and array construction, and calls — closing the "thin statics" gap (RUE-308).
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
evaluation patterns where a read finishes before the `inout` access begins. The
loans are second-class — released when the call returns — so the outgoing Σm
carries only the moves performed by the by-value arguments. Because the core is
fully monomorphic (§1), `g` names a single concrete signature: there is no
overload or generic instantiation to resolve at the call.

---

## 6. Dynamic semantics (small-step)

This section gives the small-step operational semantics for **every** core form
of §2. It is the paper form of `crates/rue-oracle` — the executable reference
interpreter that runs a core program and produces its exit code, its `@dbg`
output, its panics, and its drop trace, and is differential-tested against the
compiler on the RUE-50 corpus. Each rule group below cites the oracle function
that realizes it (`crates/rue-oracle/src/lib.rs`), so the paper semantics and the
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
  Locations      ℓ ∈ Loc                    -- one storage cell per live let-binding or by-value parameter
  Cell contents  c ::= v | ⊘                 -- ⊘ = uninitialised / moved-out (the dynamic image of Σ's absence/MovedOut, §5)
  Store          H : Loc ⇀ c
  Values         v ::= n_T                    -- a scalar integer n of type T = int(w,s), with min_T ≤ n ≤ max_T
                     | b                       -- b ∈ { true, false }
                     | ⟨⟩                      -- unit
                     | { v1, …, vk }_S         -- a struct-S value (fields in declaration order)
                     | [ v1, …, vn ]           -- an array value (elements in ascending index)
                     | Kj⟨ v1, …, va ⟩         -- an enum value: variant tag Kj (0-based index j) + payload v1..va (a = 0 ⇒ just the tag)
  Environment    ρ : Var ⇀ Loc               -- per frame: each in-scope binding → its cell
  Scope record   s = [ℓ1, …, ℓq]             -- cells owed a drop at this scope's exit, in creation order (dropped newest-first)
  Frame          φ = ⟨ ρ ; σ ⟩ ,  σ = [s1, …, sr]   -- a stack of r ≥ 1 open scopes; σ's top is the innermost scope
  Control stack  K ::= halt                    -- bottom: nothing pending; a returned value is the program result
                     | ret(E, φ) · K           -- a caller suspended in evaluation context E (§6.2), frame φ, awaiting a callee's value
                     | loopβ(e_body, φ) · K    -- a loop boundary: its body e_body and the frame φ to resume; break unwinds to here
  Config         C ::= ⟨ H ; φ ; K ; e ⟩       -- active frame φ evaluating expression e
                     | ↯κ                        -- halted in a trap of category κ ∈ { overflow, div-zero, rem-zero, bounds } (exit 101)
                     | ✓n                        -- halted normally with process exit code n
```

The reduction relation is `C → C'`. The store `H` is global (locations never
alias across frames except through the by-reference sharing of §6.9); `φ` and `K`
carry the per-call control state the sketch attributed to `K`. This matches the
oracle's `Interp`/`Frame` (`lib.rs:132`, `lib.rs:141`): a `Frame` is `φ`, its
`locals`/`params` vectors are the cells reachable through `ρ`, and its evaluation
proceeds block-by-block exactly as `E`-decomposition proceeds redex-by-redex.

`n_T` records the value's integer type because overflow, comparison signedness,
and bitwise width all depend on it; the oracle carries the same information out of
band on each CFG instruction's `ty` (`lib.rs:415`). A discriminant-only enum value
`Kj⟨⟩` is stored as its bare tag (the oracle's `Value::Int` tag, `lib.rs:565`); a
payload-carrying `Kj⟨v1..va⟩` as the tagged aggregate (`lib.rs:559`, RUE-285).

Four **scope helpers** on frames, used by the rules below, all defined in terms of
the drop relation `drop(H, ℓ)` of §6.11 (which is itself a no-op on a `⊘` or
`Copy` cell, so these fold harmlessly over non-droppable bindings):

```
  push-scope(⟨ρ;σ⟩)              = ⟨ρ; [] :: σ⟩                    -- open a fresh, empty innermost scope
  run-scope-drops(H, ⟨ρ; s::σ⟩) = drop the cells of s newest-first, yielding H'; result frame ⟨ρ; σ⟩   -- close ONE (innermost) scope
  run-all-scope-drops(H, φ)      = iterate run-scope-drops until φ has no open scopes (whole-frame teardown, on return)
  unwind-drops(H, φ', φ)         = run-scope-drops repeatedly on φ' until its open-scope stack equals φ's (break: down to a boundary)
```

A scope gains a cell to drop when a `let` (§6.7) or a `match` arm (§6.6) binds one;
`inout`/`borrow` parameters are deliberately never recorded (§6.9), which is how
they escape the drop obligation (§5.6).

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
```

A place `p` used in value context (the `e ::= p` production) is a redex once its
index subexpressions are values; the contexts `E[e]`/`v[E]` reduce those indices
left-to-right first (as `resolve_path` does, `lib.rs:858`). The two structural
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
consume it, so it propagates to the top and halts (`Interp::run`, `lib.rs:172`).

### 6.3 Literals and the use of a place (copy / move)

A literal is already a value; it takes no step except to *be* one. Its width and
signedness were resolved by elaboration (§5.8), so the machine stores the concrete
`n_T` / `b` / `⟨⟩` (`Const`/`BoolConst`, `lib.rs:417`).

Using a place `p` in value context is the operational side of §4.2 / §5.1. Let
`p` resolve, under ρ, to a root cell `ℓ` and an evaluated projection path `π`
(field indices and already-reduced array indices, §6.2); write `H(ℓ)@π` for the
sub-value reached by following `π` into `H(ℓ)`, and `H[ℓ@π ↦ ⊘]` for the store
with that sub-position replaced by the moved-out marker. Reading navigates the
stored aggregate exactly as `place_read` does (`lib.rs:892`).

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
of an absent cell as a hard error (`get_local`, `lib.rs:845`) so a violation is
caught rather than masked. The oracle achieves the `⊘`-marking *statically* — the
compiler's drop elaboration omits the drop of a moved place, so no runtime marker
is needed (`run_drop`'s note, `lib.rs:344`); the paper machine marks it
dynamically. The two are observably identical: the same values are dropped the
same number of times.

Equality operands are the exception (§4.1, §5.4, `4.3:3f`): the operand place is
**read through a shared loan and not moved**, so even an `Affine`/`Linear`
operand steps by `(D-Use-Copy)` in the `E ≟ e` / `v ≟ E` positions, leaving its
cell `Owned`. This is the dynamic image of the equality-borrow side condition; the
oracle simply reads both operands without disturbing storage (`cmp`, `lib.rs:762`).

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

`(D-Arith)`/`(D-Arith-Trap)` are `arith` + `range_check` (`lib.rs:713`,
`lib.rs:1037`): the interpreter computes in `i128` (wide enough that no host
overflow precedes the range check) and traps when the result leaves `[min_T,
max_T]`. `neg` is the unary case: `neg (min_T)_T → ↯overflow` because `-min_T >
max_T` for a signed `T` (`Neg`, `lib.rs:484`).

**Division and remainder `/ %`** add two extra traps before the range check
(`divmod`, `lib.rs:727`):

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
`idiv` faults there even though the mathematical remainder is 0 — `lib.rs:748`).

**Comparison `≟` and the ordering compares `< > <= >=`** yield `bool`
(`cmp`, `lib.rs:762`). Scalars compare by their integer value, respecting
signedness (the value `n_T` already carries the sign). Only `==`/`!=` may reach an
aggregate (ordering on aggregates is a §5 type error); there they compare
**structurally** — a struct field-by-field, an array element-by-element, an enum
same-tag-and-equal-payload, recursing into nested aggregates (RUE-285) — and a
`String` by its byte content (`4.3:2`):

```
  v1 ≈ v2  ⟺  v1 and v2 are structurally equal      (scalars by value; aggregates componentwise; strings by content)
  ─────────────────────────────────────────────────────────────────────────────────── (D-Eq)
  v1 == v2 → (v1 ≈ v2)                v1 != v2 → ¬(v1 ≈ v2)
```

**Bitwise `& | ^ ~` and shifts `<< >>`** operate on the `w`-bit two's-complement
representation and never trap (`bitop`/`shift`, `lib.rs:794`). Write `β_w(n)` for
the `w`-bit pattern of `n` and `val_{w,s}(β)` for its reinterpretation at
signedness `s`:

```
  ⊛ ∈ { &, |, ^ }        β = β_w(n1) ⊛_bits β_w(n2)
  ───────────────────────────────────────────────────── (D-Bit)
  (n1)_{int(w,s)} ⊛ (n2)_{int(w,s)} → ( val_{w,s}(β) )_{int(w,s)}

  (n1)_{int(w,s)}  ~  → ( val_{w,s}( ¬β_w(n1) ) )_{int(w,s)}       -- bitwise complement (BitNot, lib.rs:489)
```

Shifts mask the shift amount modulo the operand width `w` (`4.3a:10`); `>>` on a
signed type is arithmetic (sign-replicating), on an unsigned type logical
(`shift`, `lib.rs:810`):

```
  k = amt mod w        β = β_w(n) shifted left by k, masked to w bits
  ───────────────────────────────────────────────────────────────── (D-Shl)
  (n)_{int(w,s)} << (amt)_T → ( val_{w,s}(β) )_{int(w,s)}

  k = amt mod w        β = ( arithmetic-if-signed / logical-if-unsigned ) right shift of β_w(n) by k
  ───────────────────────────────────────────────────────────────── (D-Shr)
  (n)_{int(w,s)} >> (amt)_T → ( val_{w,s}(β) )_{int(w,s)}
```

`not` on `bool` is `not true → false`, `not false → true` (`Not`, `lib.rs:488`).

### 6.5 Aggregate introduction and projection

A struct, array, or enum literal is a redex once **all** its components are values
(the `E` contexts of §6.2 reduce them left-to-right, threading `H`). It steps to
the corresponding aggregate value, owning every component (`StructInit`/
`ArrayInit`, `lib.rs:518`):

```
  ────────────────────────────────────────────────── (D-Struct)
  S{ f1=v1, …, fk=vk } → { v1, …, vk }_S

  ────────────────────────────────────────────────── (D-Array)
  [ v1, …, vn ] → [ v1, …, vn ]                       -- (n ≥ 0; the empty array [] is the zero-sized [T;0])
```

Projection in value context is subsumed by the place-use rules of §6.3 (a
projection `p.f` / `p[e]` is a place); an **array index is bounds-checked** at the
moment the path is navigated, and an out-of-range or negative index traps
(`resolve_path`/`place_read`, `lib.rs:869`, `lib.rs:897`):

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
value; a discriminant-only variant is just its tag (`EnumVariant`, `lib.rs:559`):

```
  ────────────────────────────────────────────────── (D-Enum-Intro)
  E::Kj( v1, …, va ) → Kj⟨ v1, …, va ⟩                -- a = 0 ⇒ the bare tag Kj⟨⟩
```

`match` first reduces its scrutinee to an enum value `Kj⟨v1,…,va⟩` (a
value-context **use** of the scrutinee, §5.5: a move for a non-`Copy` enum — its
source cell became `⊘` by §6.3 — a copy otherwise). The tag `Kj` selects the one
covering arm (exhaustiveness, §5.5, guarantees exactly one), which **binds the
payload components to fresh cells** and reduces the arm body in a new scope owning
those cells; the other arms never come into being (`Switch` + `EnumPayloadGet`,
`lib.rs:268`, `lib.rs:579`):

```
  arm j is  Kj(x1, …, xa) => ej        ℓ1..ℓa fresh        ρ' = ρ[ x1↦ℓ1, …, xa↦ℓa ]
  H' = H[ ℓ1↦v1, …, ℓa↦va ]           φ' = push-scope(φ with ρ', owing [ℓ1,…,ℓa])
  ────────────────────────────────────────────────────────────────────────────────── (D-Match)
  ⟨ H ; φ ; K ; match Kj⟨v1,…,va⟩ { …, Kj(x1..xa) => ej, … } ⟩  →  ⟨ H' ; φ' ; K ; ej ⟩
```

The scrutinee value is **consumed** by the match: its payload now lives in the
`ℓi`, so it is not dropped again, and each `xi` is an ordinary `Owned` binding
governed by §6.11 at the arm's end (a `Linear` payload the arm neither moves nor
consumes is the leak the statics already rejected; an `Affine` one is dropped
once). This is the operational content of "binding a variant's payload moves it
out; a moved-out payload runs its destructor exactly once when its binding leaves
scope" (`6.3:17`, `6.3:20`). `if` is the two-armed boolean special case:

```
  ─────────────────────────────────────── (D-If-T)          ─────────────────────────────────────── (D-If-F)
  if true { e1 } else { e2 } → e1                            if false { e1 } else { e2 } → e2
```

`if`'s arms are entered directly (they open scopes for their own `let`-bindings by
§6.7); the boolean scrutinee is `Copy`, so no drop attends the branch itself.

### 6.7 `let`, sequencing, and scope-exit drop

`let x = v ; e2` allocates a fresh cell for `x`, binds it, and reduces the body in
a scope that **owes `x` a drop**. To make scope exit a reduction step, the machine
uses one administrative runtime form, `endscope(ℓ̄) in e` (not a §2 surface form),
which runs the drops of cells `ℓ̄` when `e` has become a value:

```
  ℓ fresh
  ─────────────────────────────────────────────────────────────────── (D-Let)
  ⟨ H ; ⟨ρ;σ⟩ ; K ; let x = v ; e2 ⟩ → ⟨ H[ℓ↦v] ; ⟨ρ[x↦ℓ];σ⟩ ; K ; endscope([ℓ]) in e2 ⟩

  ─────────────────────────────────────────────────────────────────── (D-EndScope)     -- ℓ̄ dropped newest-first
  ⟨ H ; φ ; K ; endscope([ℓ1,…,ℓq]) in v ⟩ → ⟨ drop(H, ℓq) ; …; drop(H, ℓ1) ; φ ; K ; v ⟩
```

where `drop(H, ℓ)` is the drop relation of §6.11 (a no-op on a `⊘` or `Copy`
cell). Nested `let`s nest their `endscope`s, so cells are dropped in **reverse
declaration order** (RAII) — the innermost/newest binding first. A bare sequence
`e1 ; e2` evaluates `e1` to a value and **discards** it; because §5.3 guarantees
`e1` carries no linear value, the discarded temporary is simply dropped (a no-op
for a `Copy` value) and control passes to `e2`:

```
  ─────────────────────────────────────────────────────────── (D-Seq)
  ⟨ H ; φ ; K ; v1 ; e2 ⟩ → ⟨ drop(H, v1) ; φ ; K ; e2 ⟩       -- drop the discarded temporary, then continue
```

(The oracle realizes both via the compiler's explicit `Drop` CFG instructions,
which its elaboration inserts at exactly these scope/temporary boundaries and the
interpreter executes with `run_drop`, `lib.rs:701`; `drop(H, v)` on an already-
owned temporary value is `drop` on a cell whose contents is `v` and never `⊘`.)

### 6.8 Assignment: overwrite-drop and reinitialisation

`assign p = v` stores `v` into the cell/sub-position `p` denotes. If that position
currently holds an `Owned` droppable value, it is **dropped first** (overwrite-
drop, §5.2, `3.8:55`); reinitialising a `⊘` (moved-out) position drops nothing.
The result is `⟨⟩` and the position becomes `Owned` (`place_write`, `lib.rs:911`):

```
  ρ(root(p)) = ℓ      H(ℓ)@π = c      H1 = ( drop(H, c-at-ℓ@π) if c ≠ ⊘ else H )      H2 = H1[ ℓ@π ↦ v ]
  ─────────────────────────────────────────────────────────────────────────────────────────────────── (D-Assign)
  ⟨ H ; φ ; K ; assign p = v ⟩ → ⟨ H2 ; φ ; K ; ⟨⟩ ⟩
```

(The compiler elaborates the overwrite-drop as an explicit `Drop` emitted before
the store, so the oracle's `place_write` needs only to overwrite — the drop
instruction ran first; consistent with `3.8` overwrite-drop.)

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

  ────────────────────────────────────────────────────────────────────────── (D-Return)
  ⟨ H ; φ_g ; ret(E, φ)·K ; return v ⟩ → ⟨ run-all-scope-drops(H, φ_g) ; φ ; K ; E[v] ⟩
```

`(D-Return-Value)` is the "a function evaluates to the value its body evaluates
to" rule of §4.3 — there is no implicit action, the body simply *is* an
expression that reduced to `v`. `(D-Return)` is the explicit form: `return v`
discards the intervening scopes up to the function boundary, running their drops,
and yields `v` (the oracle's `Terminator::Return`, `lib.rs:240`, with the
compiler having placed the pre-return `Drop`s). The oracle models `inout` by
**copy-in / copy-out** rather than true sharing — it copies the argument in, runs
the callee, then copies each `inout` parameter's final value back into the caller
place (`lib.rs:640`, `lib.rs:657`). Under the law of exclusivity (§5.4) an `inout`
place is unaliased for the call's duration, so copy-out is observably identical to
the shared-cell rule above; the paper machine takes the sharing form because it is
simpler to state and the two agree exactly on well-typed programs.

A call whose callee is a builtin with no core body (e.g. a `String` method, or
`@dbg` / `@to_string`) reduces by the builtin's defining equation rather than by
`(D-Call)`; these are elaboration-level primitives, and the oracle dispatches them
directly (`string_builtin`, `lib.rs:307`; `@dbg` appends its argument's rendering
to the observable output, `lib.rs:667`). The core-form call rule above governs
every user function.

### 6.10 `loop` and `break`

`loop { e }` pushes a loop boundary and enters the body in a fresh scope; when the
body reduces to a value (necessarily `⟨⟩`, discarded), its scope drops run and the
loop **re-enters** its body — so a value's storage from one iteration is reclaimed
before the next, exactly as a `let` inside the loop body drops each turn (in the
oracle the loop is `Goto`/`Branch`/`Switch` back-edges, `lib.rs:247`, and the
compiler places a `Drop` on the back-edge that `run_drop` executes each turn).
`break` unwinds to the nearest loop boundary, running the drops of every scope it
discards, and the whole `loop` yields `⟨⟩`:

```
  ─────────────────────────────────────────────────────────────────── (D-Loop-Enter)
  ⟨ H ; φ ; K ; loop { e } ⟩ → ⟨ H ; push-scope(φ) ; loopβ(e, φ)·K ; e ⟩

  ─────────────────────────────────────────────────────────────────── (D-Loop-Iter)
  ⟨ H ; φ' ; loopβ(e, φ)·K ; v ⟩ → ⟨ run-scope-drops(H, φ') ; push-scope(φ) ; loopβ(e, φ)·K ; e ⟩

  ─────────────────────────────────────────────────────────────────── (D-Break)      -- unwinds scopes down to the loop boundary
  ⟨ H ; φ' ; loopβ(e, φ)·K ; break ⟩ → ⟨ unwind-drops(H, φ', φ) ; φ ; K ; ⟨⟩ ⟩
```

`unwind-drops(H, φ', φ)` runs the scope-exit drops of every scope open in `φ'`
that is not already open in the enclosing `φ`. A `loop` with no reachable `break`
never fires `(D-Break)` and so runs forever — its static type is `never` (§5.7,
`Loop-Div`), consistent with its never yielding a value to its context.
(Multi-`break` and `break e` value-carrying loops are elaborated to this shape;
formalizing their ownership join is the deferred loop-section work noted in §5.7.)

### 6.11 Drop

`drop(H, ℓ)` and `drop(H, c)` are the operational core of Rue's memory safety.
Dropping a cell holding `⊘` — a moved-out or uninitialised position — does
**nothing** (this single skip is what makes double-free impossible, §7). Otherwise
the value's user destructor, if any, runs **first**, then its droppable *contents*
drop in `3.9` order (`run_drop`, `lib.rs:350`):

```
  drop(H, ⊘)                       = H                                   -- moved-out / uninitialised: skip
  drop(H, n_T) = drop(H, b) = drop(H, ⟨⟩) = H                            -- scalars are Copy: nothing to drop
  drop(H, { v1,…,vk }_S)           = drop*( dtor_S(H, {v̄}_S) , [v1,…,vk] )   -- run S's destructor (if any), then fields in DECLARATION order
  drop(H, [ v1,…,vn ])             = drop*( H , [v1,…,vn] )              -- elements in ASCENDING index order
  drop(H, Kj⟨ v1,…,va ⟩)           = drop*( H , [v1,…,va] )              -- ONLY the ACTIVE variant Kj's payload (6.3:20)
```

where `drop*(H, [c1,…,cm])` folds `drop` over the list left-to-right, and
`dtor_S` runs `S`'s destructor as an ordinary call (§6.9) if `S` declares one
(skipping it, and the whole field recursion, for a **builtin** `S` such as
`String`, whose destructor *is* its entire drop glue and has no observable effect
in the model — `lib.rs:356`). The **enum** case reads the runtime tag `Kj` to
recurse into the *active* variant's payload only: an inactive variant's payload
has no storage, and a discriminant-only active variant (`a = 0`) drops nothing
(`lib.rs:388`). A payload already moved out by a `match` binding (§6.6) left the
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

The four trap categories — `overflow` (arithmetic, `neg`, `min_T / -1`),
`div-zero`, `rem-zero`, and `bounds` (a negative or out-of-range array index) —
each abandon the configuration to `↯κ` and halt the program with the panic exit
code of Appendix B (101), regardless of surrounding context (§6.2, Panic-Lift).
They are **total, deterministic, and observable**: an alternate compiler must
reproduce the same trap on the same input (`3.1:6/13`, `8.1`–`8.3`). The top-level
result is fixed by running `main`:

```
  ⟨ H0 ; φ_main ; halt ; e_main ⟩ →* ⟨ H ; φ_main ; halt ; v ⟩
  ────────────────────────────────────────────────────────────── (Result-Ok)
  program result = ✓( n mod 256 )        where v = n_{int(32,signed)}, or ✓0 if v = ⟨⟩

  ⟨ H0 ; φ_main ; halt ; e_main ⟩ →* ↯κ
  ────────────────────────────────────────────────────────────── (Result-Panic)
  program result = ✓101
```

`main`'s returned `i32` is masked to a byte for the process exit code, and a
`unit`-returning `main` exits 0 (`Interp::run`, `lib.rs:165`); any trap exits 101
(`lib.rs:172`). These two rules, plus the observable `@dbg` output accumulated
during reduction, are precisely the `Outcome` (`exit_code`, `stdout`, `panic`) the
differential harness compares against the compiled binary (RUE-50).

The interpreter implementing this whole relation is the executable oracle
(`crates/rue-oracle`; README § "The executable oracle" / RUE-50). Every rule group
above names the function that realizes it, so a change to either must be mirrored
in the other or the differential tests will diverge — which is the mechanism that
keeps the paper semantics and the running semantics one artifact.

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

- **No use-after-drop / no leak of drops.** Every `Owned`, droppable,
  non-moved place is dropped exactly once, at the end of its scope, and never read
  afterward. *Because:* §5.6 schedules the drop and §6 executes it at frame pop,
  and Σ shows no path live past that point.

- **Linear values are consumed exactly once.** No value whose type carries a
  linear value reaches end of scope `Owned` (§5.6 rejects it) or is discarded
  (§5.3 rejects it) or is consumed on only some paths (§5.5 join rejects it);
  and no value is used twice (`Use-Move` consumes it). Hence exactly once. This
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
  no-move-while-loaned premise, and it is the invariant future slice/view rules
  need when runtime indices prevent per-element ownership tracking.

These six are the memory-safety-without-GC claim, decomposed. Note which of them
was *unprovable* against the prose spec until now: all of them, because each rests
on "use", "moved", "consumed", "dropped" being defined — which §3–§6 finally do.

---

## 8. Traceability: prose paragraphs this core subsumes

For the spec-traceability discipline (`crates/rue-spec`), the correspondence so
far. As breadth is filled in, each new rule adds its citation. The §6 rows below
also correspond, function-for-function, to `crates/rue-oracle` — the executable
witness of the dynamic semantics (RUE-50), cited inline in each §6 rule group.

| Formal notion | Prose paragraphs it formalizes / replaces |
|---|---|
| §3 multiplicity lattice | 3.8:1–3, 3.8:14/16/18/20, 3.8:30/32/37, 3.8:57/58, 3.8:74, 3.9:31, 6.3:19 |
| §4.2 definition of *use* | 3.8:5, 3.8:7, 3.8:9, 3.8:11, 3.8:22, 3.8:33, 3.8:53 |
| §4.1/§5.4 equality borrows its operands | 4.3:3f |
| §5.5 match / enum elim + intro | 6.3:17, 3.8:33 (destructure), 4.7 (match) |
| §5.8 leaf/operator/aggregate/call statics | 4.1:2/5/7, 4.2:1, 4.3:1/2, 3.6:5/6/15/16, 3.5:1/2, 4.10:3/4/5/7 |
| §5.6 enum drop (active payload) | 6.3:20 |
| §4.3 expression/return value | 4.5:3 (→ value, not just type), 6.1:4/5, 4.9:1/7 |
| §5.2 assignment / reinit | 3.8:55/56, 3.8:72 |
| §5.3 discard leak check / explicit `@drop` | 3.8:64/65, 3.9:31–33 |
| §5.4 borrows / exclusivity | 6.1:14–35, 6.1:20, 6.1:30 |
| §5.5 branch join | 3.8:50/51, 3.8:73 |
| §5.6 scope exit: drop + leak | 3.8:32/62/66, 3.9 (drop order) |
| §5.7 divergence + never-coercion | 3.4:1/2/3/4/6/8, 3.4:9 |
| §6.2 evaluation order (contexts, left-to-right) | 4.0:3–9 |
| §6.3 dynamic use: copy vs. move; equality borrows | 3.8:5/7/22, 4.3:3f |
| §6.4 operator dynamics: arith/div/mod, compare, bitwise/shift | 4.2:1, 4.3:1/2, 4.3a:10, 3.1:6/13 |
| §6.5 aggregate intro + projection (bounds) | 3.5:2, 3.6:16, 8.2 |
| §6.6 enum intro + match dynamics | 6.3:17, 4.7 |
| §6.7/§6.8 let/seq/scope-drop, assignment overwrite-drop | 4.5:3, 3.8:55/64, 3.9 |
| §6.9 call / return / inout copy-out | 6.1:4/5/18, 4.9:1/7 |
| §6.10 loop / break dynamics | 4.9 (loop), 3.4:2 |
| §6.11 drop relation (active enum payload; skip moved; explicit `@drop`) | 3.9, 6.3:20 |
| §6.12 overflow/bounds/div-zero panics + exit code | 3.1:6/13, 8.1, 8.2, 8.3, Appendix B |
| §7 soundness | the informal safety intent throughout ch. 3 and 8 |

---

## 9. Immediate open decisions for a maintainer

Collected from the **[open]** tags, for the design conversation before this is
locked:

1. **Comptime as elaboration (README).** Confirm the runtime core is formalized
   first and comptime/monomorphization is a separate later layer. (Recommended.)
2. **Raw pointers / `unchecked` out of the core initially (§2).** Model chapter 9
   as a marked extension that explicitly steps outside the §7 guarantees, rather
   than threading it through every rule. (Recommended.)
3. **`@handle` (§3).** Keep it as a directive, or simplify to "an Affine type may
   define an explicit duplication function" and retire the directive? (A
   candidate pre-1.0 simplification the formalization surfaces.)
4. **Loans strictly second-class (§5.4).** Confirm loans never escape a call in
   the core; first-class references remain a deferred design question.
5. **Array index paths (§5).** Ownership tracks only *constant* index paths
   (matching `3.8:68/70`); dynamic-index moves are forbidden. Confirm this stays
   as the core rule (it is what keeps the ownership analysis decidable without
   dependent types).
