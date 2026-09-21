# Reading RueCore without Lean

A guide for a Rue contributor who can read the core calculus
(`../01-core-calculus.md`) and has never opened a Lean file. It explains what
each Lean artifact *is* in the calculus's own terms, walks one program from Rue
source through the checker and the interpreter to the theorem that covers it,
and says how to run things yourself. The companion `INDEX.md` (generated) maps
every calculus rule and every §2 syntactic form to the declaration that
mechanizes it, and `README.md` has the build commands and the file map.

If what you want is not to *read* the mechanization but to decide whether to
*believe* it, start at section 7, "Validating this in thirty minutes": it uses
the two generated reports, `DIGEST.md` (every statement) and `TRUST.md` (every
statement's axioms), and says at each step what a defect would look like.

The one idea to hold on to: **the mechanization is the calculus, typed into a
proof assistant.** Nothing here is a second semantics. Where the two texts
differ in shape (a judgment becomes a datatype, a reduction relation becomes a
function), this guide says exactly how the shapes correspond, and the
correspondence is what the review checkpoints of the project check.

## 1. A judgment is an inductive type

The calculus writes the typing judgment as

```
Γ ; Σ ⊢ e ⇒ T ⊣ Σ'
```

"under type environment `Γ` and ownership state `Σ`, expression `e` has type
`T` and leaves the ownership state as `Σ'`". The calculus's full form also
carries the loan set `Λ` (`Γ ; Σ ; Λ ⊢ e ⇒ R ⊣ Σ'`, where `R` may be a place
type); the fragment has no borrows, and `Λ` is ambiently empty in the current
core (§5 preamble), so the mechanization omits it, as `Statics.lean` says.
Each rule of §5 is a horizontal bar: premises above, conclusion below, the
rule's name in parentheses.

`Statics.lean` writes the same judgment as

```lean
inductive Typed (P : Program) (R : Ty) : Ctx → Expr → Ty → Ctx → Prop
```

Read `Typed P R Γ e T Γ'` as the judgment above. `P` is the top-level function
environment that §5.8's (Call) looks a callee's signature up in, and `R` is the
enclosing function's declared return type, which §5.7's (Return-Value) checks a
`return` operand against; the calculus fixes both for a function body, and the
mechanization carries them as parameters of the whole judgment for the same
reason. Four things are different in shape and identical in content:

- **`Γ` and `Σ` are fused.** A `Ctx` is a list of entries, one per binding in
  scope, and each entry carries both the fixed part (`ty`, the `μ` mark) and
  the flowing part (`st`, `Owned` or `MovedOut`). The calculus keeps them as two
  environments; the mechanization keeps them as one list, because they are
  indexed by the same bindings. `Typed.skel_preserved` is the proof that every
  rule leaves the fixed part alone, which is what lets the calculus write `Γ`
  once and thread only `Σ`.
- **Variables are positions, not names.** `use 0` is the innermost binding,
  `use 1` the one outside it (de Bruijn indices). The calculus reaches the
  core through elaboration, and elaboration resolves names; the mechanization
  starts after that step. The printer (`Print.lean`) gives position `i` the
  name `v<depth>` when it prints Rue source, so `v0` is the outermost binder.
- **Each rule is a constructor.** `Typed.useMove` *is* `(Use-Move)`: its
  arguments are the rule's premises, its result is the rule's conclusion, and
  its doc-comment names the rule and the prose paragraph it encodes. A
  program `e` is well-typed exactly when a value of type `Typed P R [] e T Γ'`
  exists for some `T` and `Γ'`, which is what "there is a derivation" means.
- **`never` is folded into one rule.** §5.7 types `return e` at `never` and
  lets (Sub-Never) coerce it to whatever the context needs, with a divergent
  outgoing state `⊥`. `Ty` has no `never`, because a `never` value does not
  exist (`3.4:1`) and so nothing is ever typed at it dynamically; instead
  `Typed.ret` concludes at *any* type and at *any* outgoing context of the
  same skeleton, which is exactly what `never` and `⊥` license a context to
  assume. `INDEX.md` records (Sub-Never) as mechanized at that one form.

For example, `(Use-Move)` in the calculus (§5.1, whole bindings only) says: a
use of an owned, non-`Copy` place has the place's type and marks the place
`MovedOut`. In Lean:

```lean
| useMove {Γ i en} :
    Γ[i]? = some en → en.st = .owned → en.ty.mult ≠ .copy →
    Typed P R Γ (.use i) en.ty (Γ.set i (en.setSt .movedOut))
```

Premise by premise: binding `i` exists and is entry `en`; it is `Owned`; its
class is not `Copy`; conclusion: the use has type `en.ty` and the outgoing
context is `Γ` with binding `i` re-marked `MovedOut`. The calculus's other
premises (`fully-owned`, the destructor-field and root-index restrictions, `p
not loaned`) concern projections and loans, which are outside the current
fragment; `INDEX.md` lists which rules and sections are in and which are not.

## 2. The dynamics is a function

§6 gives a small-step machine: configurations `⟨H ; φ ; K ; e⟩` and a
reduction relation between them. `Dynamics.lean` gives the same dynamics as
a definitional interpreter:

```lean
def eval : Nat → Program → Store → Frame → Expr → EvalRes
def run (P : Program) (fuel : Nat) : EvalRes := eval fuel P [] ⟨[], []⟩ (.call 0 [])
```

`H` is §6.1's store (a list of cells: `full v`, the moved-out marker `moved`
for `⊘`, or `dead` for a retired allocation `†`) and `φ` is §6.1's frame:
the environment `ρ` (position `i` ↦ its location in `H`) and the scope record
`σ` (the cells this frame owes a drop, in creation order). `run` is §6.12's
top-level result: call the program's entry point, index `0`, with no
arguments. Instead of stepping once, `eval` runs the program to the end and
reports one of five outcomes:

| `EvalRes` | Meaning in §6 |
| --- | --- |
| `.ok H' v tr` | the machine halted normally with value `v`, final store `H'`, and drop trace `tr` |
| `.returned H' v tr` | an unwinding `return` handed `v` back (§6.9's (D-Return)); every enclosing form passes it on until a call boundary absorbs it |
| `.panic k` | the machine halted in a defined trap `↯κ` (§6.12): `overflow` or `divZero` |
| `.stuck w` | the machine refused: `w` names either a configuration §6 leaves undefined or a linear action the machine monitors (see below) |
| `.outOfFuel` | not a machine state at all: the interpreter's admission that it stopped early (see below) |

The drop trace `tr` is the list of every drop the machine performed, in
order: `drop ℓ v` for a binding's drop (at scope exit, at `@drop`, or when
overwritten) and `dropTemp v` for a discarded temporary. This is the
fragment's image of the oracle interpreter's observable outcome, and it is
what the bridge compares against a native binary's stdout (`README.md`, "The
bridge corpus").

A `Violation` is a refusal: `useAfterMove` (reading a `⊘` cell),
`useAfterDrop` (touching a `†` cell), `linearLeak` (a scope exit or a frame
unwind on a live linear value), `linearOverwrite` (`3.8:77`),
`linearDiscard` (`3.8:64`),
plus `unbound` and `typeConfusion` for ill-scoped or ill-typed input. §7's
memory-safety bullets each say that one of these never happens. The three
linear refusals are monitors the machine adds (§6's rules would drop the
value and rely on §5 to have forbidden it); the other four are §6's own
stuck states. That is why `eval` is a model of §6 on the programs `check`
accepts, and only there (`Dynamics.lean`).

Why a function rather than the relation: a function can be *run*, so every
semantic question about a fragment program is answerable by execution, and a
total function always returns one of the outcomes above, so progress becomes
the single statement "never `.stuck`", which is what the theorem in section 4
proves. What the function owes the relation is an adequacy lemma (they agree
on every program), owed by RUE-2289 and required before the mechanization
gates anything (`../03-metatheory.md`, "How to read a theorem here").

### What the fuel is, and why the theorem quantifies over it

A function in Lean must terminate, and Lean must be able to see why. Before
calls, it could: every recursive step of `eval` ran on a *subexpression*, and
expressions are finite. A call breaks that. The body of the callee is not a
subexpression of the call — and a recursive function calls itself, so no
amount of looking at the program's syntax bounds how far the machine goes. A
Rue program can loop forever, and `eval` must not.

So `eval` carries a **fuel**: a number, one unit of which every step spends,
and when it reaches zero the interpreter stops and says `.outOfFuel`. That is
not a state of §6's machine and it is not a claim about the program; it is the
interpreter reporting that *it* gave up. Unspent fuel costs nothing, so a
bound far larger than any program needs is free (`Examples.lean` runs
everything at 200, the corpus exporter at 100 000).

The theorems then say: *for every* fuel bound, a well-typed program's result
is a well-typed value, an unwinding return, a defined panic, or `outOfFuel` —
never a violation. Read carelessly, the last disjunct looks like a loophole:
an interpreter that answered `.outOfFuel` immediately would satisfy the
theorem too, and say nothing. Two lemmas close it.

- **`fuel_mono`**: if some bound produced an answer other than `outOfFuel`,
  every larger bound produces *that same answer*. So raising the bound never
  changes a result; there is one outcome, and a sufficient bound finds it.
- **`no_masking`**: if some bound reached a violation, then every bound that
  answers at all reaches that same violation. So no choice of fuel can hide a
  violation behind exhaustion.

Together: for a program that completes at some bound, "for every fuel" is a
statement about that program's one real outcome, and a violation cannot be
traded away by choosing the fuel badly. `Examples.lean` shows the two sides
concretely — `run countdown 16` is `outOfFuel`, `run countdown 17` is the
value, and `fuel_mono` proves every larger bound agrees.

One caveat belongs with `return` rather than with fuel, and it is the one
place these theorems say less than "never a violation" sounds like. A
by-value argument's value sits in no cell and no scope record until
`mintParams` gives it one, so if a *later* argument of the same call unwinds
by `return`, (D-Return) discards it with the evaluation context and no drop
and no monitor fires — a linear value can be consumed zero times without any
of the five violations. That is the calculus as written and what the compiler
does, not a modelling slip; `Dynamics.lean`'s "Pending arguments" section
states it, `Examples.lean`'s `linearLostAtCallArg` and `affineLostAtCallArg`
are the kernel-checked witnesses, and closing it is RUE-2316.

## 3. What `Matches` says, and why it is asymmetric

Every safety proof carries an invariant relating the static story to the
dynamic one. Here it is `FrameMatches D Γ φ H` (`Soundness.lean`), with `D`
the program's struct declarations — what `class(T)` and a value's drop are
read against. It has two
halves. The first is `Matches D Γ ρ H`: for each binding, the cell its location
holds agrees with its context entry, locations are inside the store, and no
two bindings share a location. Per cell, `CellMatches` says:

- a statically `Owned` entry holds a live, well-typed value;
- a statically `MovedOut` entry holds either the moved-out marker **or a live,
  well-typed, non-linear value**.

The second clause is the asymmetry, and it is deliberate. For an *affine*
`x`, after `if c { @drop(x) } else { () }` the §5.5 join marks `x` `MovedOut`
on both paths even though on the `else` path it is still live: the static
story is conservative, the dynamic story is exact, and the machine drops that
residue path-specifically at scope exit (§5.6, §6.7; `3.8:73` is the
array-element form of the same rule). The corpus case `cond_drop_affine` is
exactly this program. The invariant must allow that gap. What it must never
allow is a live *linear* value behind a `MovedOut` entry, because then the
static leak check could pass while the machine reached `linearLeak`, and the
theorem would be false; for a linear `x` the join refuses outright (`3.8:50`,
the corpus case `linear_half_consumed`), and `Matches` records that refusal
as an invariant.

The second half is about the frame's **scope record** σ, and it is one
equation: σ read newest-first *is* the environment ρ. §6.1 keeps both books —
ρ says where a binding lives, σ says it is owed a drop.

Be clear about what that equation costs to prove here: **nothing**. Every
frame the interpreter builds — the callee's at a call, the extended one inside
a `let` body — builds σ and ρ from the same list, so in this fragment σ holds
no information ρ does not and the equation is definitional. It is stated as an
invariant for two reasons. It is what the teardown proofs consume: `run-all-
scope-drops` walks σ, and because σ is ρ, `Matches` (every cell live or moved
out, no two bindings sharing one) applies to the walk — which is why §7's
no-use-after-drop bullet is a consequence of the invariant here rather than a
fact about closed expressions, and why no unwind touches a `†` cell or retires
one twice. And it is the clause that stops being free when `Frame.scope`
becomes the *stack* §6.1 actually specifies: §6.6's `match` arms and §6.10's
loops push and pop scopes independently of the binder chain, and then σ and ρ
are two books a slice has to keep in step. That is the shape the RUE-1277
redundancy was raised for; this fragment does not have it, so nothing here
should be read as having checked it.

A third piece, `Untouched ρ H H'`, carries frame *locality*: the store only
grows, and every already-allocated cell that ρ does not name keeps its
contents. A callee's parameter cells are minted above the caller's whole
store, so the caller's bindings are outside the callee's ρ and its agreement
survives the call untouched. That is what lets the proof step over a call
without knowing anything about the callee but its signature.

## 4. The theorem, and what `check_sound` buys

```lean
theorem soundness (hwf : WfProgram P) :
  ∀ fuel, Typed P R Γ e T Γ' → FrameMatches P.structs Γ φ H →
    EvalOk T R Γ' φ H (eval fuel P H φ e)
```

`EvalOk` is a predicate on the result rather than a disjunction of
existentials, which is what lets the proof discharge §6.2's operand search
once and reuse it at every form. Read out, it says: on `.ok`, the value has
the expression's type, the outgoing context's agreement is restored, and the
frame's neighbours are untouched; on `.returned`, the value has the enclosing
function's return type and the neighbours are untouched; on `.panic` and
`.outOfFuel`, nothing; and on `.stuck`, **`False`** — which is the whole
point. That is progress and preservation in one statement (§7, first bullet).

Over a whole program, `run_safe` says it in the shape a reader wants:

```lean
theorem run_safe (hwf : WfProgram P) (h0 : P[0]? = some fd) (hp : fd.params = []) :
  ∀ fuel, run P fuel = .outOfFuel
        ∨ (∃ k, run P fuel = .panic k)
        ∨ (∃ H v tr, run P fuel = .ok H v tr ∧ HasTy v fd.ret)
```

and the named corollaries (`no_use_after_move`, `no_linear_leak`, …) each
restate "never `.stuck` with this particular violation" for one §7 bullet.

The theorem quantifies over derivations. To apply it to a *program* you need
to know a derivation exists, and `Checker.lean` is how you find out: `check P
R Γ e` is the §5 rules run as an algorithm, returning the type and outgoing
context or rejecting, and `checkProgram P` lifts that to (Fn) §5.8 for every
function plus the entry point's empty parameter list. `check_sound` and
`checkProgram_sound` prove that every acceptance is backed by a real
derivation — the second produces exactly the `ProgramTyped` hypothesis the
program theorems take. So the pipeline for any program is: run
`checkProgram`; if it accepts, `run_safe` applies and the §7 guarantees hold
for it; if it rejects, the program is outside the theorem, and `run` usually
shows which refusal it would have reached (the corpus prints that when there
is one; a rejection can also be a plain type error, such as an out-of-range
literal, which `eval` runs without complaint). Completeness, that every
derivable program is accepted, is **false**, not open: `check` is
deliberately narrower than the rule at `return`, and `Checker.lean`'s "what
completeness costs" gives the two counterexamples. One is contrived
(`1 + return true` in a `bool`-returning function, which the rule re-types
and the algorithm does not). The other is not: a `return` arm of an `if`
contributes its post-operand state to §5.5's join, where §5.7 excludes a
diverging arm's state entirely, so a binding that arm moved out is unusable
after the `if` — and `main() -> int { let x = mk 5; (if c { @drop(x); return 0 } else { 5 }); consume(x) }`
is derivable, runnable, accepted by the compiler, and rejected here. That is
why a `reject` verdict in the bridge corpus is only trustworthy on shapes
where `check` is complete, and why the generator emits no `return`
(`Corpus.lean`, `Gen.lean`).

## 5. A worked example: `reinit`

The corpus case `reinit` (`Corpus.lean`) moves a linear value out of a
binding, assigns a new one back in, and consumes that. It exercises
`(Use-Move)`, `(Assign)` with the `3.8:77` premise checked on the post-RHS
state, reinitialization (`3.8:55`), and the scope-exit leak check (§5.6).

### The program

Core syntax, as `Examples.reinit` writes it:

```lean
letIn true (resL (intLit 1))
  (seq (consume (use 0))
    (seq (assign 0 (resL (intLit 2)))
      (consume (use 0))))
```

as the body of the entry function `f0`, over the fixture declarations
`Examples.structEnv` (`resL` is `mkStruct sLinear [·]`, the declared-`linear`
struct with one `i64` field and no destructor). Printed as Rue source by the
bridge — the program's struct declarations come first; only the one this case
uses is shown here, `explain/reinit.txt` has them all:

```rue
linear struct S2 { x0: i64 }
fn consume_S2(s: S2) -> i64 { s.x0 }

fn f0() -> i64 {
    {
        let mut v0: S2 = S2 { x0: 1 };
        {
            let t2: i64 = consume_S2(v0);
            {
                { v0 = S2 { x0: 2 }; };
                consume_S2(v0)
            }
        }
    }
}

fn main() -> i32 {
    let result: i64 = f0();
    @dbg(result);
    0
}
```

### The checker's derivation

`checkProgram` accepts, and `f0`'s body checks at `int` with outgoing context
`[]`. The
derivation it certifies, read from the outside in, with the fused context
written as `[type, μ, state]` per binding (only one binding, `v0`):

| Step | Rule | Context in | Context out |
| --- | --- | --- | --- |
| `mkStruct sLinear [intLit 1]` | `Typed.mkStruct`, (Struct-Intro) §5.8: one initializer per declared field, at the field's type | `[]` | `[]` |
| enter the `let` body | `Typed.letIn`, `(Let)`: the binder enters `Owned` | `[]` | `[S2, mut, Owned]` |
| `use 0` | `(Use-Move)`: `class(S2) = Linear` (§3: the declared attribute), so the use moves | `[S2, mut, Owned]` | `[S2, mut, MovedOut]` |
| `consume (…)` | `Typed.consume`, the fragment's whole-value elimination | `[…, MovedOut]` | `[…, MovedOut]` |
| `seq` discard | `(Seq)`: an `int` carries no linear value (`3.8:64`) | | |
| `mkStruct sLinear [intLit 2]` | RHS of the assignment, typed first | `[…, MovedOut]` | `[…, MovedOut]` |
| `assign 0 …` | `(Assign)`: `v0` is `mut`; on the post-RHS state `v0` is `MovedOut`, so the `3.8:77` premise holds; `v0` becomes `Owned` (`3.8:55`) | `[…, MovedOut]` | `[S2, mut, Owned]` |
| inner `seq` discard | `(Seq)`: the assignment's `unit` carries no linear value | | |
| `use 0` | `(Use-Move)` again | `[…, Owned]` | `[…, MovedOut]` |
| `consume (…)` | `Typed.consume`, type `int` | | |
| leave the `let` body | §5.6's scope-exit check, folded into `Typed.letIn`: the residual state is `MovedOut`, so no linear value leaks | `[S2, mut, MovedOut]` | `[]` |

The premise that matters is in the `(Assign)` row. Had the first
`consume_S2(v0)` been omitted, the post-RHS state of `v0` would be
`Owned`, the `3.8:77` premise `Σ1(p) = MovedOut ∨ ¬carries_linear(T)` would
fail, and `check` would reject; that is the corpus case `linear_overwrite`,
which the compiler rejects with E0493 and the machine refuses with
`linearOverwrite`.

### The interpreter's run

`run` returns `.ok [dead] (.int 2) []`. Step by step, with the
store `H` as a list of cells indexed by location and `ρ` mapping position 0
to its location:

| Step | Store before | Effect | Store after | Trace |
| --- | --- | --- | --- | --- |
| `mkStruct sLinear [intLit 1]`, (D-Struct) §6.5 | `[]` | a value `{ 1 }_S2`, no store effect | `[]` | |
| `(D-Let)`: mint a cell for `v0` | `[]` | allocate location 0, `ρ = [0]` | `[full S2 { 1 }]` | |
| `use 0`, `(D-Use-Move)` | `[full S2 { 1 }]` | the value moves out; the cell becomes `⊘` | `[moved]` | |
| `consume` | | the first field, `1`, an `int` | `[moved]` | |
| `(D-Seq)` discard | | an `int` is `Copy`: no drop | `[moved]` | |
| `mkStruct sLinear [intLit 2]` | | a value | `[moved]` | |
| `assign 0`, `(D-Assign)` | `[moved]` | the cell is `⊘`, so nothing is dropped; reinitialize | `[full S2 { 2 }]` | |
| inner `(D-Seq)` discard | | `unit` is `Copy`: no drop | `[full S2 { 2 }]` | |
| `use 0`, `(D-Use-Move)` | `[full S2 { 2 }]` | move out again | `[moved]` | |
| `consume` | | payload `2` | `[moved]` | |
| `(D-EndScope)`: retire `v0` | `[moved]` | the cell is `⊘`, so nothing to drop; retire it | `[dead]` | |

No drop event is ever emitted (both values were consumed, so every cell was
`⊘` at every drop point), so the trace is empty and the printed program's only
output line is the value, `2`. The bridge expectation in `corpus.json` is
exactly that: `{"kind": "ok", "stdout": ["2"], "exit": 0}`.

Both tables above are generated for every corpus case: this one is
`explain/reinit.txt`, printed by `lake exe ruecore-explain reinit`.

### The theorem that covers it

`checkProgram` accepted, so by `checkProgram_sound` the program is
`ProgramTyped`, and `run_safe` applies (the initial frame invariant,
`FrameMatches D [] ⟨[], []⟩ []`, holds trivially: no bindings, no store, an
empty scope record). It promises, at every fuel: `outOfFuel`, a defined
panic, or `.ok` with a value of the entry function's declared return type.
The run above is the last case; `HasTy (.int 2) .int` holds because `2` is in
bounds. The corollary this program illustrates is `no_linear_overwrite`: the
assignment in the middle is the very shape `3.8:77` guards, and the theorem
says the guard is never needed at run time for a program the checker accepts,
because the checker has already demanded the `MovedOut` state that makes the
overwrite-drop a no-op.

## 5b. A second worked example: an early `return` and its unwind

The corpus case `return_past_affine` (`Corpus.lean`) is the shape RUE-1277
added `(D-Return)`'s "in any evaluation context `E'`" for: a `return` under
two open `let` scopes, each holding a live affine resource. It is the smallest
program where the frame's **scope record** σ, rather than the pending
`endscope` markers, is what runs the drops.

### The program

```lean
letIn false (resA (intLit 3))
  (letIn false (resA (intLit 4))
    (ret (intLit 7)))
```

Printed (prelude omitted):

```rue
fn f0() -> i64 {
    {
        let v0: S1 = S1 { x0: 3 };
        {
            let v1: S1 = S1 { x0: 4 };
            return 7
        }
    }
}
```

### What the checker demands

`Typed.ret` (`(Return-Value)` §5.7) has three premises. The operand is checked
at the enclosing function's declared return type — `7 ⇒ i64`, and `i64` is
`f0`'s return type. The outgoing state is free (that is `⊥`), restricted only
to the same skeleton. And the third is the one that matters here:
`NoOwnedLinear Γ₁` — **no binding of the frame is still `Owned` at a linear
type**. That is §5.6's obligation, taken frame-wide because a `return` ends
every open scope of the frame at once (`3.8:62`, and (Fn) §5.8's second
clause).

Here Γ₁ at the `return` is `[v1: S1 = Owned, v0: S1 = Owned]`. Both
are *affine*, not linear, so the premise holds and the program is accepted:
an affine value reaching an exit is dropped, which is legal and observable.
Change either to `S2` and the premise fails — that is the corpus case
`return_past_linear`, which the compiler rejects with E0406 and the machine
refuses with `linearLeak`.

### The unwind, step by step

The frame at the `return` is `φ = ⟨ρ ; σ⟩` with `ρ = [ℓ1, ℓ0]` (innermost
binder first) and `σ = [ℓ0, ℓ1]` (creation order). The `let` cases built σ by
appending, so σ reversed is ρ — the invariant of section 3.

| Step | Rule | Store before | Effect | Store after | Events |
| --- | --- | --- | --- | --- | --- |
| 1 | `(D-Call) §6.9` (push the frame) | `[]` | `f0` takes no arguments, so no parameter cell is minted; `σ = []` | `[]` | |
| 2–4 | literal, (D-Struct) §6.5, `(D-Let) §6.7` | `[]` | mint `ℓ0 = S1 { 3 }`; `ρ = [ℓ0]`, `σ = [ℓ0]` | `[ℓ0 = S1 { 3 }]` | |
| 5–7 | literal, (D-Struct) §6.5, `(D-Let) §6.7` | `[ℓ0 = …]` | mint `ℓ1 = S1 { 4 }`; `ρ = [ℓ1, ℓ0]`, `σ = [ℓ0, ℓ1]` | `[ℓ0 = S1 { 3 }, ℓ1 = S1 { 4 }]` | |
| 8 | literal | | the operand `7` becomes a value | | |
| **9** | **`(D-Return) §6.9` (unwind the frame)** | `[ℓ0 = S1 { 3 }, ℓ1 = S1 { 4 }]` | `run-all-scope-drops(H, φ)` walks `σ` **newest-first**: drop-retire `ℓ1`, then `ℓ0` | `[ℓ0 = †, ℓ1 = †]` | `drop ℓ1 = S1 { 4 }`; `run drop fn S1(S1 { 4 })`; `drop ℓ0 = S1 { 3 }`; `run drop fn S1(S1 { 3 })` |
| 10 | inner `(D-EndScope)` — did not run | | the `return` discarded the evaluation context, the pending `endscope` markers with it; the result travels out unchanged | | |
| 11 | outer `(D-EndScope)` — did not run | | same | | |
| 12 | `(D-Return-Main) §6.9` (absorb) | `[ℓ0 = †, ℓ1 = †]` | the call boundary turns the unwound `return` into the call's value; `f0` is the bottom of the stack, so this firing is (D-Return-Main) — at an inner call the same row is (D-Return)'s hand-off, which is why the generated table labels it with both | | |

Two things are worth pausing on. **The drops run once, not twice.** Steps 10
and 11 are the `endscope`s that the normal path would have run; they see a
`.returned` result and pass it on, because `eval` sequences a `let` body with
`andThen`, which only continues on a value. Their cells were already retired
at step 9 — and if either had tried again, `drop-retire` would have found a
`†` cell and the machine would have refused with `useAfterDrop`. The σ
invariant is exactly what proves it cannot.

**The order is newest-first, and it is observable.** The trace is
`drop ℓ1` then `drop ℓ0`, each followed by the destructor `S1` declares, so
the printed program prints `4`, then `3`, then its value `7`. Two spec rules
are at work and it is worth keeping them apart: `3.9:18` says *that* a
`return` drops every live binding of every enclosing scope, and `3.9:4` says
*in what order* — "reverse declaration order (last declared, first dropped)".
The bridge compares both against the real binary.
`explain/return_past_affine.txt` is this table generated, with the store at
every row.

## 5c. A third worked example: a struct, its class, and §6.11's drop order

The corpus case `struct_nested_dtor_drop` is the smallest program where a
struct's *fields* matter: a destructor-bearing struct holding a
destructor-bearing struct, dropped at scope exit. It is where (Struct-Intro)
§5.8, §3's join, and §6.11's order are all visible at once.

### The declarations

Two of the fixture declarations (`Examples.structEnv`) are in play. As the
printer writes them:

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }
struct S5 { x0: i64, x1: S1 }
drop fn S5(self) { @dbg(self.x0); }
```

In the core they are `StructDecl` records: `S1` has no attribute, one `int`
field, a destructor, and records `class(S1) = Affine`; `S5` has no attribute,
fields `[int, S1]`, a destructor, and records `class(S5) = Affine`.

**Why `Affine`, and why that is checked rather than asserted.** §3 says
`class(S)` is the join of the field classes lifted by the declared attribute.
For `S5` the field join is `Copy ⊔ class(S1) = Copy ⊔ Affine = Affine`, and
no attribute lifts `Affine` to `Affine` (`3.8:3`: structs are affine by
default). `WfStructs` (`Statics.lean`) is that equation, one conjunct per
declaration, and `checkStructs` decides it — so the class a declaration
*records* is never taken on trust. Two more conjuncts matter here:
`3.8:18`/`3.9:31` would reject `@copy` on either of these (their joins are
not `Copy`, and they have destructors), and `3.9:44` would reject the
destructor if either carried a *linear* field. `struct_class_unique` is the
statement that the recorded class is determined rather than free: on an
environment whose fields name only earlier declarations, at most one
assignment of classes satisfies §3's equation.

### The program, and what the checker demands

```lean
letIn false (mkStruct sOuter [intLit 1, resA (intLit 2)]) (intLit 9)
```

printed as

```rue
fn f0() -> i64 {
    {
        let v0: S5 = S5 { x0: 1, x1: S1 { x0: 2 } };
        9
    }
}
```

(Struct-Intro) §5.8 types the initializers **in declaration order**, threading
Σ left to right, each at its declared field type — which is the same
`TypedArgs` judgment (Call) §5.8 uses for an argument list, because the two
rules impose the same left-to-right discipline. `S1 { x0: 2 }` is itself a
(Struct-Intro) node, so the derivation nests:

| Node | Rule | Concludes |
| --- | --- | --- |
| `S5 { x0: 1, x1: S1 { x0: 2 } }` | (Struct-Intro) §5.8 | `⇒ S5` |
| ⟶ `1` | (Lit) §5.8 | `⇒ i64`, at field `x0`'s declared type |
| ⟶ `S1 { x0: 2 }` | (Struct-Intro) §5.8 | `⇒ S1`, at field `x1`'s declared type |
| ⟶ ⟶ `2` | (Lit) §5.8 | `⇒ i64` |
| `let v0 = …; 9` | (Let) §5.3 + §5.6 | `⇒ i64`, and the leak check passes: `class(S5) = Affine`, not `Linear` |

The leak check is the one place the class is read: §5.6 rejects a binding
still `Owned` at a `Linear` type. `S5` is `Affine`, so scope exit may drop it
— and the machine then has to.

### The drop, step by step

| Step | Rule | Store before | Effect | Store after | Events |
| --- | --- | --- | --- | --- | --- |
| 1–4 | literals, (D-Struct) §6.5 | `[]` | the inner literal becomes `{ 2 }_S1`, then the outer `{ 1, { 2 }_S1 }_S5` — a redex only once **all** its components are values | `[]` | |
| 5 | `(D-Let) §6.7` | `[]` | mint `ℓ0` for `v0` | `[ℓ0 = S5 { 1, S1 { 2 } }]` | |
| 6 | literal | | the body's `9` | | |
| **7** | **`(D-EndScope) §6.7` → `drop-retire` → §6.11** | `[ℓ0 = S5 { 1, S1 { 2 } }]` | the cell holds a live non-`Linear` value, so the monitor lets it through and §6.11's walk runs: **the user destructor of `S5` first**, then the fields in **declaration order** — `x0` is an `int` and drops nothing, `x1` is an `S1` and runs *its* destructor | `[ℓ0 = †]` | `drop ℓ0 = S5 { 1, S1 { 2 } }`; `run drop fn S5(…)`; `run drop fn S1(S1 { 2 })` |
| 8 | `(D-Return-Value) §6.9` | | the frame pops with an empty record | | |

So the printed program prints `1` (the outer destructor), then `2` (the
inner), then its value `9` — and that is the bridge expectation
`{"kind": "ok", "stdout": ["1", "2", "9"], "exit": 0}`. It is also what the
compiler does: this order was checked by hand against a native binary before
the slice was written.

Three claims in that row are theorems rather than observations.
`dropValue_struct_events` is §6.11's order in closed form: dropping a
well-typed struct value emits its destructor's event, when its declaration
has one (`3.9:28`), followed by the concatenation of its fields' events in
declaration order (`3.9:13`), each field's given by the same form recursively
— so "outer first, then the fields in order" is one equation rather than a
reading of two induction steps. (`dropValue_order` and `dropValues_order` are
those steps; `dropEvents` in `Dynamics.lean` is the order written as a
function, and `dropValue_events` is the equation saying the machine's walk is
it.) `dropValue_ok` says the walk never refuses on a well-typed value. And
`StructDecl.Wf.field_not_linear` says a declaration whose class is not
`Linear` has no `Linear` field — which is why the leak monitor at step 7 can
look at the value's own class and never inside it, and why RUE-2237's
"dropped exactly once" has a walk of known shape to quantify over.

### More worked examples

Every corpus case is a smaller worked example: its printed source begins
with a comment naming the case, the rules it exercises, and its expected
outcome, and `corpus.json` (from `scripts/rue lean`, or `lake exe
ruecore-corpus`) holds all of them. Slice issues of the "Formal core mechanization" project add one
worked example each to this section as they land.

## 6. Running things yourself

- **Build and check everything.** `scripts/rue lean` builds the package
  through Buck with the pinned toolchain, re-checks the compiled modules with
  `leanchecker`, and prints the axioms report. `lake build` in this directory
  does the build alone, with `elan` fetching the same pinned toolchain.
- **Run a program.** Open `RueCore/Examples.lean`. Each `#eval run p demoFuel`
  line runs a program; `lake build`'s log prints the result next to the line
  number, and an editor with the Lean extension shows it inline. Change a
  program, rebuild, and watch the outcome change. `#eval checkProgram p` runs
  the checker the same way.
- **Read a kernel-checked fact.** `example : run returnPastLinear demoFuel =
  .stuck .linearLeak := by rfl` is not a test that ran once; it is a statement
  the kernel verified when the file compiled. Every refusal and trap the
  fragment can reach has such a witness (`Examples.lean`, `Corpus.lean`), and
  so does the fuel boundary (`run countdown 16` versus `17`).
- **Read the reports.** `DIGEST.md` is every theorem's statement and every
  definition those statements are written in terms of; `TRUST.md` is every
  theorem's axioms. Both are committed and both are regenerated by `lake exe
  ruecore-digest` (`--trust` for the second), so a reviewer's check is to
  regenerate and diff. Section 7 walks the whole path.
- **Read `#print axioms`.** The trust boundary of a Lean proof is the list of
  axioms it depends on, which is what `TRUST.md` tabulates. The Buck build
  also writes the raw listing (`axioms.txt` beside `trust.md`), for each of
  the theorems the target trusts (`BUCK`, `root//:lean-ruecore`), a line
  like

  ```
  'RueCore.soundness' depends on axioms: [propext, Quot.sound]
  ```

  Two different things can appear there. `sorryAx` means a proof was left
  unfinished, and `Lean.ofReduceBool` (what `native_decide` introduces) means
  a result the kernel did not verify itself; either would be a hole. Lean's
  three standard axioms, `propext`, `Quot.sound`, and `Classical.choice`,
  are all kernel-checked assumptions of the logic; this project's policy is
  to use only the first two (constructive proofs, no classical choice), and
  any axiom the package declared itself would be an assumption to review.
  The Buck build fails on anything outside its allowed set
  (`toolchains/lean/defs.bzl`), and so does `ruecore-digest --trust`, which
  applies the same policy to *every* theorem rather than to the ones `trust`
  names.
- **Find the rule.** `INDEX.md` lists every labeled rule of the calculus's
  §5 and §6, and every alternative of its §2 grammar, with the declaration
  that mechanizes it or *not yet mechanized*. Start there when the question is
  "where is `(If)`?", "is `(Call)` covered yet?", or "does the fragment have
  arrays?".

## 7. Validating this in thirty minutes

The path for a reader who knows type systems or proof assistants and wants to
decide whether to believe the mechanization without trusting whoever wrote it.
Six steps, each with what a defect would look like. Nothing here requires
reading a proof.

**1. Build it yourself (five minutes warm; the first run downloads the
toolchain).**

```bash
scripts/rue lean
```

Buck fetches the SHA-pinned Lean toolchain — about 17,500 files and 2.7 GB
unpacked (`toolchains/lean/defs.bzl`), so the first run is a download and the
five minutes are the ones after it — builds the package, re-checks the
compiled modules with the toolchain's own `leanchecker` (an independent
re-verification of the `.olean`s, not a replay of the build), runs the reports,
and prints the trust report. *A defect looks like:* the build failing, which
means what is committed does not compile — no result below is worth anything
until it does. `lake build` in `docs/formal/lean` is the same check without
Buck, using `elan` and the same pin (`scripts/validate-lean-toolchain-pin.py`
holds the two pins equal).

**2. Read the trust report (two minutes).**

`TRUST.md`, which `scripts/rue lean` just printed from the build's own output.
It lists every theorem in the `RueCore` namespace with the axioms
`Lean.collectAxioms` says its proof depends on, the number of proofs resting on
`sorryAx`, and the axioms the package declares itself. Today: 98 theorems, no
axiom anywhere outside `propext` and `Quot.sound`, no `sorryAx`, no declared
axiom. *A defect looks like:* a `sorryAx` (an unfinished proof), a `Lean.ofReduceBool` (a
`native_decide` the kernel did not check), a `Classical.choice` (allowed by
Lean, outside this project's constructive policy), or a package-declared axiom
that assumes the thing being proved. A grep for `sorry` would not find the
first of those if it were hidden behind a macro; the axiom list would.
Regenerate it with `lake exe ruecore-digest --trust` and diff against the
committed copy — there is no CI gate on that diff yet (RUE-2241 tracks it;
nothing in CI runs the Lean build until ADR-0097's gate is met), so the
reviewer is the gate.

**3. Read the digest (ten minutes).**

`DIGEST.md`, likewise generated (`lake exe ruecore-digest`) and likewise worth
regenerating and diffing — regenerating it also runs the two checks the file
claims for itself, that every `RueCore` constant it prints has an entry of its
own and that every declaration the generated `INDEX.md` names survived its
filter, and a miss is a message on stderr and a non-zero exit rather than a
quietly wrong file. It opens with the fragment boundary — how many of the
calculus's §5/§6 rules and §2 syntactic forms have a core image at all, quoted
from `INDEX.md` — then gives every theorem's statement as Lean elaborated it,
then every definition those statements are written in terms of, in dependency
order — with its body where the body is short enough to read, so `Ty.mult`
(which is `class(T)`, and so is what makes `res linear` linear) and
`Ctx.join` (§5.5, a premise of `Typed.ite`) can be read rather than taken on
their signatures. Read `soundness` first and satisfy yourself you can state it
in one sentence; then read the corollaries, which should say nothing
`soundness` does not. *A defect looks like:* a theorem that quantifies over
less than you expected (a hypothesis that makes it vacuous, a `Γ` fixed to
`[]` where the claim should be general), a corollary that is not an instance
of the main theorem, a definition in the dependency list whose doc-comment
describes something other than what its signature says, or a definition whose
body makes the claims about it trivial — a `Ty.mult` that answered `.copy`
everywhere, or a `Ctx.join` that answered `none`, would leave every linearity
corollary true and empty.

**4. Read `Matches`, and hold it against §7 (five minutes).**

The one place a soundness proof can quietly cheat is its invariant: an
invariant strong enough to be unprovable is caught by the kernel, but one too
weak to mean anything is not. `FrameMatches` (in `DIGEST.md`, or
`Soundness.lean`) is this proof's invariant, and its `Matches` half is what
§7's no-use-after-move bullet names in words — "preservation maintains the invariant that Σ faithfully tracks the
store's initialization". Check the two directions of `CellMatches` against that
phrase, as section 3 above spells them out: an `Owned` entry holds a live
well-typed value; a `MovedOut` entry holds the moved-out marker *or* a live
well-typed **non-linear** value. *A defect looks like:* that second clause
dropping the word `non-linear`. Then a live linear value could sit behind a
`MovedOut` entry, the scope-exit leak check could pass over it, and
`no_linear_leak` would be false — and the proof would still go through, because
the invariant would no longer rule the case out. The asymmetry is deliberate
and §5.5's join is why (section 3); the missing restriction would not be.

Then read the other half, `FrameMatches.record`: the frame's scope record,
reversed, *is* its environment. *A defect looks like:* that clause weakened to
an inclusion, or dropped. Then a cell could sit in the record twice, or in the
record after its `endscope` retired it, and a `return`'s unwind would
drop-retire it a second time — the `useAfterDrop` that `no_use_after_drop` now
rests on. Section 5b's step 9 is the walk that clause protects.

Fuel is the other place to look. The theorems say "for every fuel", and
`outOfFuel` satisfies them for free, so check that `fuel_mono` and
`no_masking` are in `DIGEST.md` and say what section 2 says they say. *A
defect looks like:* either one missing, or stated with a hypothesis that makes
it vacuous — `no_masking` with `n = m`, say.

**5. Run the three-way bridge (three minutes).**

The theorems are about `eval` and `check`, not about the compiler. What ties
the two together is the bridge: every corpus case is printed as a Rue program,
and the compiler, the reference oracle, and the native binary are run on it and
compared against what the mechanization says (`README.md`, "The bridge
corpus"). The consumer lands with RUE-2228.

```bash
scripts/rue lean-bridge
```

It prints one line per case — the case's name, the verdict (`accept(i64)`,
`reject`), and `agree` or `DISAGREE` with the number of disagreeing pairs,
each carrying the diagnostic code where a program was refused — then, for
every case that disagrees, the printed Rue program, the four views side by
side, and the pairs that differ; last a tally. On the 34 seed cases it ends

```text
  cases: 34 (34 agree, 0 disagree)
  checker <-> compiler: 0
  lean <-> oracle: 0
  lean <-> native: 0
  oracle <-> native: 0
```

and exits zero. It was not always green: `cond_drop_affine` first made the
compiler report an internal error instead of a verdict (`E9000`, a
CFG-verification failure on the conditionally dropped affine residue), so
the oracle, which shares that frontend, could not run it either. That was a
real compiler defect, RUE-2290, found by this bridge and fixed; the case
stays as the regression signal. *A defect looks like:* any case disagreeing.
A disagreement is a defect in one of the four views — the mechanization, the
compiler, the oracle, or the printed program — and which one is a question the
case's `explain/<case>.txt` rendering (section 5's tables, `lake exe
ruecore-explain <case>`) is meant to answer.

**6. Spot-check two statements against the calculus (five minutes).**

`INDEX.md` maps every labeled rule to the declaration that mechanizes it. Pick
two and read the calculus and the Lean side by side.

- `(Assign)`, §5.2, against `Typed.assign`. The calculus's premises: the target
  is `mut`; the RHS is typed first; and `3.8:77`'s overwrite premise
  (`Σ1(p) = MovedOut ∨ ¬carries_linear(T)`) is checked on the state *after*
  the RHS. The constructor should have one argument per premise, with the
  `Γ₁[i]?` lookup — the post-RHS state — feeding the disjunction. *A defect
  looks like:* that lookup being `Γ[i]?`, the pre-RHS state, which would accept
  a program that overwrites a live linear value the RHS had not yet consumed.
- `(D-Return)`, §6.9, against `eval`'s `ret` arm. The rule discards the
  evaluation context `E'` — every pending `endscope` marker inside it
  included — and runs `run-all-scope-drops(H, φ)` instead, over every live
  binding of every enclosing scope (`3.9:18`) in reverse declaration order
  (`3.9:4`). The arm should evaluate the operand, then walk the frame's scope
  record, then return `.returned`, which every enclosing form passes on until
  a `call` absorbs it. *A defect looks like:* the arm running the *innermost*
  scope only (then an early return two scopes deep would leak the outer
  binding, against `3.9:18`), or the record walked oldest-first (against
  `3.9:4`, which the bridge's stdout comparison in step 5 would catch on
  `return_past_affine`), or `.returned` being absorbed somewhere other than a
  call boundary (then a `return` would stop at the nearest `let`). What the
  rule does *not* cover is the value of an argument already evaluated when a
  sibling argument returns; that is the RUE-2316 carve-out in section 2, and
  `Examples.lean` has the witnesses.
- `(D-Let)`/`(D-EndScope)`, §6.7, against `eval`'s `letIn` arm. The machine
  mints a fresh cell for the binder, runs the body, then at scope exit inspects
  that cell: a live linear value is `linearLeak` and the machine stops there; a
  live affine value is dropped, its event appended to the trace, and the cell
  retired (`dead`); a live copy value or a `⊘` cell drops nothing and the cell
  is retired just the same. A `†` cell cannot arise here — §6.7 mints the
  binder's own cell and this arm is where it retires it — and the machine
  refuses one (`useAfterDrop`), as it refuses an unbound index. *A defect looks
  like:* the retire being omitted (then a use after scope exit would read a
  stale value instead of refusing), or the affine drop event being emitted in
  the wrong order relative to the body's own trace, which is exactly what the
  bridge's stdout comparison in step 5 would catch.

What thirty minutes does **not** buy: the adequacy lemma tying this executable
dynamics to §6's reduction relation is owed by RUE-2289 and not proved here
(section 2); the fuel is this interpreter's device and has no counterpart in
§6, so `fuel_mono`/`no_masking` are about `eval`, not about the paper machine;
and the rules and forms marked *not yet mechanized* in `INDEX.md` are outside
every theorem above. The fragment boundary in step 3 is not a
formality; it is most of what the reports are for.

## 8. Writing a doc-comment that the index can read

Every top-level declaration in a rule-bearing module (`Syntax`, `Statics`,
`Dynamics`, `Soundness`, `Checker`, `Print`, `Explain`, `CorpusMain`, and any
new module;
`instance`s, `example`s, and constructors without a doc-comment of their own
are exempt) has a doc-comment citing what it mechanizes: a rule label exactly as the calculus writes it
(`(Use-Move)`, `(D-Let)`, `(@Drop)`), a section (`§5.5`), or a prose
paragraph (`3.8:73`). A declaration that mechanizes nothing on its own (an
inversion lemma, a printing helper) says `(helper)` instead. A module whose
declarations are programs rather than rules says `xref: examples` in its
module docstring. `scripts/validate-lean-xref-index.py` enforces this and
regenerates `INDEX.md` with `--write`; `README.md` has the full convention.
The same script holds the §2 forms table, which maps each alternative of the
calculus's grammar to the `Expr`/`Ty` constructors that mechanize it; a slice
that gives a form its first core image updates that row in the same change.
