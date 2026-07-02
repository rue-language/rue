# The Rue Core Calculus

This is the keystone of the formal semantics: the small calculus that surface Rue
elaborates into, and the precise home of ownership, moves, borrows, drops, and
linear consumption. Read `README.md` first for how this relates to the surface
language and the prose spec.

Notation is deliberately plain ASCII so it can be transcribed into a proof
assistant or an interpreter without re-typesetting. Judgment forms are introduced
where they are first used. The five modeling decisions this rested on were
**ratified by Steve on 2026-07-02** and are now commitments (see §9); the most
consequential is the removal of `@handle` (§3).

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
      | [T; n]                 -- array of n ≥ 0 elements of type T

Struct declarations
  D ::= struct S { f1: T1, ..., fk: Tk }        -- multiplicity class assigned by §3
  (an elaborated anonymous struct is just a struct name S with a generated identity)

Places (lvalues — expressions that denote a location)
  p ::= x                      -- a local binding
      | p . f                  -- field projection
      | p [ e ]                -- array index (e a value expression of an int type)

Expressions
  e ::= lit                    -- integer / bool / unit literal
      | p                      -- a place used in VALUE context (see §4 — this is a USE)
      | e1 ⊕ e2                -- primitive binary op (arith, compare, bitwise)
      | S { f1: e1, ..., fk: ek }        -- struct value construction
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
(→ the value). Raw pointers and `unchecked` code (chapter 9) are **out of the
core** (ratified 2026-07-02): they are a distinguished, clearly-marked extension;
their whole point is to step outside the guarantees the core proves, so they are
modeled separately rather than threaded through every rule.

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
```

The lattice order is `Copy ⊑ Affine ⊑ Linear` (more restrictive is higher); `⊔`
is the least upper bound. **Infectiousness is just the join**: a struct is at
least as restrictive as its most-restrictive field. A `@copy` declaration is
well-formed only when the field join is already `Copy` (`3.8:18`); a `linear`
declaration forces `Linear` regardless of fields.

> This replaces the prose enumerations `3.8:2` (the Copy list), `3.8:3` (structs
> affine by default), `3.8:18/20` (`@copy` field constraint), and `3.8:57/58`
> (carries-linear, infectious) with one lattice and one join.

*`@handle` is removed* (ratified 2026-07-02). It was a vestigial intermediate:
the formalization showed it is not a distinct concept — an `@handle` type was
merely an Affine (or Linear) type that also provided an explicit duplication
operation `.handle()`, which is nothing more than an ordinary function
`S -> S`. The class lattice needs no `@handle`. If explicit duplication of an
Affine type is wanted later, it is just such a function, requiring no directive.
The surface feature (`3.8:40–49`), its `.handle()` requirement, and any compiler
support are to be retired; tracked separately from this document.

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
  - the operand of a by-reference argument: the `p` in `inout p` and `borrow p`.
- **Value context** — every other occurrence. The occurrence must *produce a
  value*: operands of `⊕`, the scrutinee of `if`/`match`, a struct-field or
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
`carries_linear(T)` is `class(T) = Linear` lifted through arrays/structs, i.e. the
field/element join reaching Linear). `let x = e1 ; e2` is like `Seq` but binds `x`
(with `x` Owned in Σ for `e2`) and imposes no discard check on `e1`.

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
(`Use-Move` premise) nor, for a shared loan, mutated. Loans are **second-class**
(ratified 2026-07-02): they exist only for the call's dynamic extent and cannot be
returned, stored, or outlive the call — this is what lets Rue omit lifetimes. If a
future first-class-reference feature is adopted (a deferred design question), this
section is where it would land, as an addition rather than a change.

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

### 5.6 Scope exit: the drop obligation and the leak check

When a binding `x: T` introduced by `let` (or a by-value parameter) leaves scope
in state `Owned`:

- if `carries_linear(T)`: **ill-formed** — a linear value reached end of scope
  unconsumed (`3.8:32/62/66`). This is the must-use check.
- else if `class(T) = Copy`: nothing happens (no drop).
- else (`Affine`, droppable, non-linear): a **drop** is scheduled (dynamic §6):
  the value's destructor, if any, runs, then its droppable fields/elements drop,
  in the order of `3.9` (declaration order for fields, ascending index for
  elements), skipping any sub-place that is `MovedOut`.

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
    H : Loc ⇀ StoredValue          -- the store: locations to values (scalars, struct/array aggregates)
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
  its fields/elements; **a `MovedOut` place is skipped** (this single skip is what
  makes double-free impossible — §7).
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
  defined panics. Types are preserved under reduction.

- **No use-after-move.** In a well-typed program, the Use/Move dynamic rule is
  never applied to a `MovedOut` place. *Because:* `Use-Move`/`Use-Copy` (§5.1)
  require `Σ(p) = Owned`, and preservation maintains the invariant that Σ
  faithfully tracks the store's initialization.

- **No double-free.** Every stored value's destructor runs at most once. *Because:*
  a move sets `p ↦ MovedOut`, and the Drop rule (§6) skips MovedOut places — so a
  value that was moved out of a place is dropped through its *new* owner, never
  again through the old one.

- **No use-after-drop / no leak of drops.** Every `Owned`, droppable,
  non-moved place is dropped exactly once, at the end of its scope, and never read
  afterward. *Because:* §5.6 schedules the drop and §6 executes it at frame pop,
  and Σ shows no path live past that point.

- **Linear values are consumed exactly once.** No value whose type carries a
  linear value reaches end of scope `Owned` (§5.6 rejects it) or is discarded
  (§5.3 rejects it) or is consumed on only some paths (§5.5 join rejects it);
  and no value is used twice (`Use-Move` consumes it). Hence exactly once.

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
| §3 multiplicity lattice | 3.8:1–3, 3.8:14/16/18/20, 3.8:30/32/37, 3.8:57/58, 3.8:74 |
| §4.2 definition of *use* | 3.8:5, 3.8:7, 3.8:9, 3.8:11, 3.8:22, 3.8:33, 3.8:53 |
| §4.3 expression/return value | 4.5:3 (→ value, not just type), 6.1:4/5, 4.9:1/7 |
| §5.2 assignment / reinit | 3.8:55/56, 3.8:72 |
| §5.3 discard leak check | 3.8:64/65 |
| §5.4 borrows / exclusivity | 6.1:14–33, 6.1:20, 6.1:30 |
| §5.5 branch join | 3.8:50/51, 3.8:73 |
| §5.6 scope exit: drop + leak | 3.8:32/62/66, 3.9 (drop order) |
| §6 overflow/bounds/div-zero panics | 3.1:6/13, 8.1, 8.2, 8.3, Appendix B |
| §7 soundness | the informal safety intent throughout ch. 3 and 8 |

---

## 9. Ratified modeling decisions (2026-07-02)

The five load-bearing modeling decisions, ratified by Steve on 2026-07-02:

1. **Comptime is elaboration, not core.** ✅ The runtime core is formalized first;
   comptime + monomorphization are a separate later layer (`02-elaboration.md`).
2. **Raw pointers / `unchecked` are out of the core (§2).** ✅ Modeled as a
   distinguished extension that explicitly steps outside the §7 guarantees, not
   threaded through every rule.
3. **`@handle` is removed (§3).** ✅ Confirmed vestigial — an Affine/Linear type
   plus an ordinary duplication function, no distinct concept. The surface
   feature and its compiler support are to be retired (tracked separately).
4. **Loans are strictly second-class (§5.4).** ✅ Loans never escape a call;
   first-class references remain a deferred, additive design question.
5. **Array ownership tracks only constant index paths (§5).** ✅ Matches
   `3.8:68/70`; dynamic-index moves are forbidden. Keeps the ownership analysis
   decidable without dependent types.

With these ratified, the keystone is the project's formal foundation. Remaining
work is breadth (per the extension rubric) and the executable interpreter (§6 →
RUE-50), not further foundational decisions.
