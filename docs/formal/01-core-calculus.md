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
      | e1 ; e2                -- sequence; e1's value is DISCARDED (must be unit or a leak error)
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
             = Copy           if attr(S) = @copy    (well-formed only if base = Copy — 3.8:18)
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
well-formed only when the field join is already `Copy` (`3.8:18`); a `linear`
declaration forces `Linear` regardless of fields.

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

The main judgment:

```
    Γ ; Σ ; Λ  ⊢  e  ⇒  T  ⊣  Σ'
```

read: "under bindings Γ, starting ownership Σ and loans Λ, expression `e` is
well-formed, has type `T`, and leaves ownership Σ'." (Λ is scoped to a single call
and does not change across `e`; it is threaded only so the use/assign rules can
consult it — see 5.4. It is written on the turnstile, not on the output, for that
reason.)

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

### 5.4 Borrows and the law of exclusivity

A call evaluates its arguments and then, for the duration of the call, holds
*loans* on the places passed by reference. Within one call:

```
  For each argument a_i of call g(a1..am):
    a_i = e         : ordinary value context — Use-Copy / Use-Move on any places in e
    a_i = borrow p  : requires Σ(p)=Owned; adds (root(p), shared)   to Λ_call
    a_i = inout p   : requires Σ(p)=Owned; adds (root(p), exclusive) to Λ_call; p mutable

  Well-formed only if Λ_call is CONSISTENT:  a root may appear
    - any number of times as shared,  OR
    - exactly once as exclusive,
    - never both.                                            -- law of exclusivity (6.1:20, 6.1:30)
```

While `(root(p), _) ∈ Λ`, `p` and every path under/over it may not be moved
(`Use-Move` premise) nor, for a shared loan, mutated. Loans are **second-class**:
they exist only for the call's dynamic extent and cannot be returned, stored, or
outlive the call — this is what lets Rue omit lifetimes. **[open]** The core
models loans as strictly call-scoped; if a future first-class-reference feature is
adopted (a live ADR question, deferred), this section is where it lands.

An **equality compare** `e1 ≟ e2` reads each place operand through the same
kind of shared loan, scoped to the compare rather than a call: it requires
`Σ(p) = Owned`, takes a `(root(p), shared)` loan for the compare's duration,
and leaves Σ unchanged (no move — §4.1, `4.3:3f`). Two shared reads are always
consistent, so an operand may even appear on both sides (`a == a` is
well-formed). This is why comparing an affine or linear value does not
discharge its move obligation.

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
and is excluded from the join (`3.8:51`).

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

---

## 6. Dynamic semantics (small-step) *(sketch — shape fixed, rules being filled in)*

The machine configuration:

```
  Config  =  ⟨ H ; K ; e ⟩
    H : Loc ⇀ StoredValue          -- the store: locations to values (scalars, struct/array aggregates, tagged enum values ⟨Kj; payload⟩)
    K : evaluation context / control stack   -- holds pending frames, loop and function boundaries,
                                                and the per-scope list of drop obligations
    e : the expression being reduced
  Env  :  x ⇀ Loc                  -- carried per frame in K; binds each live local to its storage
```

Reduction `⟨H;K;e⟩ → ⟨H';K';e'⟩` is small-step and left-to-right per §4.0-order of
the prose (`4.0:3–9`), which the core inherits verbatim. The load-bearing,
Rue-specific rules — the ones an alternate compiler and a proof both need
pinned — are:

- **Use/Move (read a place in value context).** Reads the aggregate at the
  place's location. For a moved place this rule is *unreachable* in a well-typed
  program (that is the safety theorem, §7); the interpreter still asserts
  `Owned`-ness dynamically to serve as the oracle.
- **Drop.** When a scope's frame is popped (or a place is overwritten, or a loop
  body iterates), the drop obligations recorded for that scope run: for each
  Owned droppable place, in `3.9` order, run its destructor then recursively drop
  its contents — a struct's fields and an array's elements, or, for an **enum**,
  *only the payload of the variant its stored tag names* (`6.3:20`); **a
  `MovedOut` place is skipped** (this single skip is what makes double-free
  impossible — §7). The enum case reads the tag from the store `H` to choose the
  one payload to recurse into: an inactive variant's payload has no storage to
  free, and a discriminant-only active variant drops nothing.
- **Match / variant.** `E::Kj(v1, ..., va)` is a stored value carrying tag `Kj`
  and its payload aggregate. `match v { ..., Kj(x1, ..., xa) => ej, ... }` reads
  the tag, selects the arm for `Kj`, **binds `x1..xa` to the payload components**
  (moving them out of the enum for a move-typed payload, copying for a Copy one),
  and reduces `ej`; the other arms and their bindings never come into being. The
  scrutinee value is consumed by the match, so it is not dropped again — the
  moved-out payload is now owned by the arm's locals, which drop (or must be
  consumed) at the arm's end per §5.6. Construction `E::Kj(e1, ..., ea)` evaluates
  the payloads left-to-right and stores the tagged aggregate.
- **Call / Return.** A call evaluates arguments (moving/copying by-value ones,
  binding by-reference ones to the caller's locations via the loan), pushes a
  frame binding parameters, and reduces the body; the call **evaluates to the
  value the body evaluates to**; `return e` discards intervening frames up to the
  function boundary and yields `e`'s value. (This is §4.3 made operational.)
- **inout / borrow.** The parameter's location *is* the argument place's location
  (no copy); writes through an `inout` parameter are visible to the caller after
  return (`6.1:18`); a `borrow` parameter's location is read-only for the callee.
- **Overflow / bounds / div-zero.** Arithmetic that overflows the operands' type,
  an out-of-range index, and division/remainder by zero each step to a **panic**
  configuration that halts with the exit code of Appendix B (`3.1:6/13`,
  `8.1`–`8.3`). These are total, deterministic, and observable — an alternate
  compiler must reproduce them exactly.

The interpreter implementing this relation is the executable oracle (README §
"The executable oracle" / RUE-50).

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

These six are the memory-safety-without-GC claim, decomposed. Note which of them
was *unprovable* against the prose spec until now: all of them, because each rests
on "use", "moved", "consumed", "dropped" being defined — which §3–§6 finally do.

---

## 8. Traceability: prose paragraphs this core subsumes

For the spec-traceability discipline (`crates/rue-spec`), the correspondence so
far. As breadth is filled in, each new rule adds its citation.

| Formal notion | Prose paragraphs it formalizes / replaces |
|---|---|
| §3 multiplicity lattice | 3.8:1–3, 3.8:14/16/18/20, 3.8:30/32/37, 3.8:57/58, 3.8:74, 6.3:19 |
| §4.2 definition of *use* | 3.8:5, 3.8:7, 3.8:9, 3.8:11, 3.8:22, 3.8:33, 3.8:53 |
| §4.1/§5.4 equality borrows its operands | 4.3:3f |
| §5.5 match / enum elim + intro | 6.3:17, 3.8:33 (destructure), 4.7 (match) |
| §5.6 enum drop (active payload) | 6.3:20 |
| §4.3 expression/return value | 4.5:3 (→ value, not just type), 6.1:4/5, 4.9:1/7 |
| §5.2 assignment / reinit | 3.8:55/56, 3.8:72 |
| §5.3 discard leak check | 3.8:64/65 |
| §5.4 borrows / exclusivity | 6.1:14–33, 6.1:20, 6.1:30 |
| §5.5 branch join | 3.8:50/51, 3.8:73 |
| §5.6 scope exit: drop + leak | 3.8:32/62/66, 3.9 (drop order) |
| §6 overflow/bounds/div-zero panics | 3.1:6/13, 8.1, 8.2, 8.3, Appendix B |
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
