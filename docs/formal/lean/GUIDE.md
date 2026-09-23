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

For example, `(Use-Move)` in the calculus (§5.1) says: a use of a
`fully-owned`, non-`Copy` place has the place's type and marks the place
`MovedOut`, removing every path under it. In Lean:

```lean
| useMove {Γ p en u T} :
    Γ[p.root]? = some en →
    en.st.get p.path = some u → u.fullyOwned = true →
    en.ty.atPath P.decls p.path = some T →
    T.mult P.decls ≠ .copy →
    noDtorPrefix P.decls en.ty p.path = true →
    declaredPrefix P.decls en.ty p.path = none →
    rootIdxOnly P.decls en.ty p.path = true →
    Typed P R Γ (.use p) T (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut)))
```

Premise by premise: the place's root binding exists and is entry `en`; Σ has a
state `u` for the path — which it has exactly when no *proper prefix* of the
path is `MovedOut`, so the lookup is (Owned-Base) §5.1 (`3.8:53`); `u` is
`fully-owned` (`3.8:26`: an aggregate with a hole may not be handed to a new
owner); the path reaches a declared field at every step and lands at type `T`;
`T`'s class is not `Copy`; no proper prefix of the path declares a destructor
(`3.9:34`, E0456); the use plan §4.2 records for the place is `Ordinary`,
which is `declaredPrefix … = none` — §5.1's "the (Use-Copy) and (Use-Move)
rules are read only with an `Ordinary` plan"; and `rootIdxOnly` is `3.8:68`'s
"element moves only at the root", read by walking `Ty.fieldAt` from the root's
declared type rather than by reading `.idx` versus `.proj` off the place, since
it is the type at a node that makes a step an index step. The conclusion marks
exactly `p`. The calculus's one remaining premise (`p not loaned`) concerns
loans, which are outside the current fragment; `INDEX.md` lists which rules and
sections are in and which are not.

## 2. The dynamics is a function

§6 gives a small-step machine: configurations `⟨H ; φ ; K ; e⟩` and a
reduction relation between them. `Dynamics.lean` gives the same dynamics as
a definitional interpreter:

```lean
def eval : Nat → Program → Store → Frame → Expr → EvalRes
def run (P : Program) (fuel : Nat) : EvalRes := eval fuel P [] ⟨[], []⟩ (.call 0 [])
```

`H` is §6.1's store — a list of cells, each `full c` for live contents or
`dead` for a retired allocation `†`. The contents `c` is a **tree**, because
§6.1's moved-out marker `⊘` may sit at any node of it and not only at the
root: a partial move writes `H[ℓ@π ↦ ⊘]` at exactly the sub-position it takes
(§6.3, §4.2), so a whole-place move is the special case `π = ε`. `φ` is §6.1's
frame:
the environment `ρ` (position `i` ↦ its location in `H`) and the scope record
`σ` (the cells this frame owes a drop, in creation order). `run` is §6.12's
top-level result: call the program's entry point, index `0`, with no
arguments. Instead of stepping once, `eval` runs the program to the end and
reports one of five outcomes:

| `EvalRes` | Meaning in §6 |
| --- | --- |
| `.ok H' v tr` | the machine halted normally with value `v`, final store `H'`, and drop trace `tr` |
| `.returned H' v tr` | an unwinding `return` handed `v` back (§6.9's (D-Return)); every enclosing form passes it on until a call boundary absorbs it |
| `.panic k tr` | the machine halted in a defined trap `↯κ` (§6.12) — `overflow`, `divZero`, `remZero`, `castOverflow` or `user` — carrying the observable output `tr` that ran before it |
| `.stuck w` | the machine refused: `w` names either a configuration §6 leaves undefined or a linear action the machine monitors (see below) |
| `.outOfFuel` | not a machine state at all: the interpreter's admission that it stopped early (see below) |

The trace `tr` is the list of everything the machine *did that a program can
see*, in order: `drop ℓ c` for a binding's drop (at scope exit, at `@drop`, or
when overwritten — it records the *contents* dropped, which after a partial
move is a tree with holes in it), `dropTemp v` for a discarded temporary,
`dtor s c` for a user destructor §6.11 ran, and `dbg v` for a `@dbg` (§6.12's
observable
output). The last two are the ones a Rue program prints; the first two mark
where a drop *starts*. This is the
fragment's image of the oracle interpreter's observable outcome, and it is
what the bridge compares against a native binary's stdout (`README.md`, "The
bridge corpus"). A trap carries it too, which is why `.panic` has a trace: a
trapping process prints what it printed and then exits 101.

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
two bindings share a location.

Per cell the agreement is now **recursive**, because both sides are trees. Σ's
state for a binding is `OwnSt` — `owned`, `movedOut`, or `fields [t₁ … tₖ]`
for a value some of whose fields have been moved out, which `explain/` and
§5e's drawing spell `Owned{ x0: MovedOut }` — and the cell holds
`Contents`, the same shape with §6.1's `⊘` admitted at any node.
`ContentsMatches` relates them path by path:

- an **`owned`** node holds a hole-free well-typed contents, i.e. a value;
- a **`movedOut`** node holds well-typed contents with **no live linear
  sub-value** in it — it need not hold `⊘`;
- a **`fields`** node holds the struct its type names, matched field by field,
  with a slot no partial move touched read as `owned`.

The second clause is the asymmetry, and it is deliberate. For an *affine*
`x`, after `if c { @drop(x) } else { () }` the §5.5 join marks `x` `MovedOut`
on both paths even though on the `else` path it is still live: the static
story is conservative, the dynamic story is exact, and the machine drops that
residue path-specifically at scope exit (§5.6, §6.7; `3.8:60`, and `3.8:73` is
the array-element form of the same rule). The corpus
case `cond_drop_affine` is exactly this program, and `partial_move_one_arm`
is it one field down. The invariant must allow that gap. What it must never
allow is a live *linear* value behind a `MovedOut` node, because then the
static leak check could pass while the machine reached `linearLeak`, and the
theorem would be false; where the two arms disagree on a path whose residue
still carries a linear value the join refuses outright (`3.8:50`, the corpus
cases `linear_half_consumed` and `join_linear_field_one_arm`), and
`ContentsMatches` records that refusal as an invariant.

Two lemmas turn that clause into the ones the proof uses.
`ContentsMatches.residualLinear_false` says the machine's leak monitor sees
exactly what §5.6's `residual-linear` computes — so after a partial move the
obligation is the *residue*'s on both sides, which is the RUE-1591 model and
what the compiler does. And `ContentsMatches.readAt`/`.writeAt` say that
navigating a path agrees on the two sides: wherever Σ has a state for the path
— which is wherever no proper prefix of it is `MovedOut`, (Owned-Base) §5.1 —
the store reaches a sub-position, and writing a new pair at that path leaves
the cell matched.

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
  ∀ fuel, Typed P R Γ e T Γ' → FrameMatches P.decls Γ φ H →
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
        ∨ (∃ k tr, run P fuel = .panic k tr)
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
after the `if` — and `main() -> int { let x = mk 5; (if c { @drop(x); return 0 } else { 5 }); @drop(x) }`
is derivable, runnable, accepted by the compiler, and rejected here (`mk 5`
being a struct literal and `@drop(x)` the discharge). That is
why a `reject` verdict in the bridge corpus is only trustworthy on shapes
where `check` is complete, and why the generator emits no `return`
(`Corpus.lean`, `Gen.lean`).

## 5. A worked example: `reinit`

The corpus case `reinit` (`Corpus.lean`) discharges a linear value, assigns a
new one back in, and discharges that. It exercises `(@Drop)`, `(Assign)` with
the `3.8:77` premise checked on the post-RHS state, reinitialization
(`3.8:55`), and the scope-exit leak check (§5.6).

### The program

Core syntax, as `Examples.reinit` writes it:

```lean
letIn true (resL (lit 1))
  (seq (drop (.var 0))
    (seq (assign (.var 0) (resL (lit 2)))
      (seq (drop (.var 0)) (lit 2))))
```

as the body of the entry function `f0`, over the fixture declarations
`Examples.structEnv` (`resL` is `mkStruct sLinear [·]`, the declared-`linear`
struct with one `i64` field and no destructor). Printed as Rue source by the
bridge — the program's struct declarations come first; only the one this case
uses is shown here, `explain/reinit.txt` has them all:

```rue
linear struct S2 { x0: i64 }

fn f0() -> i64 {
    {
        let mut v0: S2 = S2 { x0: 1 };
        {
            @drop(v0);
            {
                { v0 = S2 { x0: 2 }; };
                {
                    @drop(v0);
                    2
                }
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

`checkProgram` accepts, and `f0`'s body checks at `i64` with outgoing context
`[]`. The
derivation it certifies, read from the outside in, with the fused context
written as `[type, μ, state]` per binding (only one binding, `v0`):

| Step | Rule | Context in | Context out |
| --- | --- | --- | --- |
| `mkStruct sLinear [lit 1]` | `Typed.mkStruct`, (Struct-Intro) §5.8: one initializer per declared field, at the field's type | `[]` | `[]` |
| enter the `let` body | `Typed.letIn`, `(Let)`: the binder enters `Owned` | `[]` | `[S2, mut, Owned]` |
| `drop (.var 0)` | `(@Drop)` §5.3: `class(S2) = Linear` (§3: the declared attribute), and `@drop` is the one non-move discharge of a linear obligation (`3.9:39`) | `[S2, mut, Owned]` | `[S2, mut, MovedOut]` |
| `seq` discard | `(Seq)`: `unit` carries no linear value (`3.8:64`) | | |
| `mkStruct sLinear [lit 2]` | RHS of the assignment, typed first | `[…, MovedOut]` | `[…, MovedOut]` |
| `assign (.var 0) …` | `(Assign)`: `v0` is `mut`; on the post-RHS state `v0` is `MovedOut`, so the `3.8:77` premise holds; the subtree at the path becomes `Owned` (`3.8:55`) | `[…, MovedOut]` | `[S2, mut, Owned]` |
| inner `seq` discard | `(Seq)`: the assignment's `unit` carries no linear value | | |
| `drop (.var 0)` | `(@Drop)` again | `[…, Owned]` | `[…, MovedOut]` |
| leave the `let` body | §5.6's scope-exit check, folded into `Typed.letIn`: `residual-linear(Σ, v0, S2)` is `false` because `Σ(v0) = MovedOut`, so no linear value leaks | `[S2, mut, MovedOut]` | `[]` |

The premise that matters is in the `(Assign)` row. Had the first
`@drop(v0)` been omitted, the post-RHS state of `v0` would be
`Owned`, the `3.8:77` premise `Σ1(p) = MovedOut ∨ ¬carries_linear(T)` would
fail, and `check` would reject; that is the corpus case `linear_overwrite`,
which the compiler rejects with E0493 and the machine refuses with
`linearOverwrite`.

That premise is keyed on the destination's **type**, exactly as §5.2 writes it
and as `3.8:77` insists ("determined by the destination's *type* together with
the statically tracked move paths, never by a run-time drop flag"). It is the
one place in the fragment where the *residual* reading would be wrong. Where a
partial move sits under the target — `@drop(v0.x0)` on a carrier whose only
linear content is `x0`, then `v0 = S10{…}` — the residue carries nothing and
yet the assignment is still ill-formed, because the type still carries a linear
value; the compiler agrees (E0493), and the corpus case
`overwrite_past_partial_linear` pins it, with
`overwrite_field_past_partial_linear` the same shape one field step down.
§5.5's join and §5.6's leak check *are* keyed on the residue, because they ask
whether an obligation was **discharged** and the residue is the honest answer
there (`ownedJoinOk`, `residualLinear`). An overwrite discharges nothing —
that is precisely what `3.8:77` is about — so it reads the type
(`overwriteOk`). The two are not in tension, and the model is strictly stricter
than the residual reading at (Assign): both disjuncts of §5.2's premise imply
`residual-linear = false`, which is what keeps the machine's
`linearOverwrite` monitor unreachable from a program `check` accepts.

### The interpreter's run

`run` returns `.ok [dead] (.int 2) []`. Step by step, with the
store `H` as a list of cells indexed by location and `ρ` mapping position 0
to its location:

| Step | Store before | Effect | Store after | Trace |
| --- | --- | --- | --- | --- |
| `mkStruct sLinear [lit 1]`, (D-Struct) §6.5 | `[]` | a value `{ 1 }_S2`, no store effect | `[]` | |
| `(D-Let)`: mint a cell for `v0` | `[]` | allocate location 0, `ρ = [0]` | `[full S2 { 1 }]` | |
| `drop (.var 0)`, §6.11 | `[full S2 { 1 }]` | the glue runs — `S2` declares no destructor, so nothing is observable — and the cell's contents become `⊘` | `[full ⊘]` | `drop ℓ0 = S2 { 1 }` |
| `(D-Seq)` discard | | `unit` is `Copy`: no drop | `[full ⊘]` | |
| `mkStruct sLinear [lit 2]` | | a value | `[full ⊘]` | |
| `assign (.var 0)`, `(D-Assign)` | `[full ⊘]` | the position is `⊘`, so nothing is dropped; reinitialize | `[full S2 { 2 }]` | |
| inner `(D-Seq)` discard | | `unit` is `Copy`: no drop | `[full S2 { 2 }]` | |
| `drop (.var 0)`, §6.11 | `[full S2 { 2 }]` | the glue runs again; the contents become `⊘` | `[full ⊘]` | `drop ℓ0 = S2 { 2 }` |
| `(D-EndScope)`: retire `v0` | `[full ⊘]` | the contents are `⊘`, so nothing to drop; retire the cell | `[dead]` | |

No **observable** event is emitted: `S2` declares no destructor, so the two
`drop ℓ0` markers project to no stdout line (`Corpus.eventLine`), and the
scope exit finds a `⊘`. The printed program's only output line is therefore
the value, `2`, and the bridge expectation in `corpus.json` is exactly that:
`{"kind": "ok", "stdout": ["2"], "exit": 0}`.

Both tables above are generated for every corpus case: this one is
`explain/reinit.txt`, printed by `lake exe ruecore-explain reinit`.

### The theorem that covers it

`checkProgram` accepted, so by `checkProgram_sound` the program is
`ProgramTyped`, and `run_safe` applies (the initial frame invariant,
`FrameMatches D [] ⟨[], []⟩ []`, holds trivially: no bindings, no store, an
empty scope record). It promises, at every fuel: `outOfFuel`, a defined
panic, or `.ok` with a value of the entry function's declared return type.
The run above is the last case; `HasTy (.int .w64 .signed 2) (.int .w64
.signed)` holds because `2` is in `i64`'s range. The corollary this program illustrates is `no_linear_overwrite`: the
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
letIn false (resA (lit 3))
  (letIn false (resA (lit 4))
    (ret (lit 7)))
```

Printed:

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
destructor if either carried a *linear* field. A third check sits beside them
rather than inside them: `3.0:5` (E0483) forbids a declaration to contain
itself by value through any cycle of struct fields and enum payloads, and
`checkNoCycle` decides it for both layers at once. `class_unique` is the
statement those three buy — that the recorded class is determined rather than
free: on well-formed declarations of the same shapes, exactly one assignment
of classes satisfies §3's equations.

### The program, and what the checker demands

```lean
letIn false (mkStruct sOuter [lit 1, resA (lit 2)]) (lit 9)
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

## 5d. A fourth worked example: a trap, and the output that survives it

The corpus case `panic_after_drop` is the smallest program where the
*observable output* and the *trap* are both part of the answer. §6.12 halts
the program at a trap, but the process has already printed whatever it
printed, and `Outcome` — the thing the differential harness compares — is
exit status **and** stdout. So the machine's `.panic` carries a trace, and
the bridge compares it.

### The program

```lean
letIn false (resA (lit 7)) (seq (drop 0) (panic "boom"))
```

printed as

```rue
fn f0() -> i64 {
    {
        let v0: S1 = S1 { x0: 7 };
        {
            @drop(v0);
            @panic("boom")
        }
    }
}
```

`S1` declares a destructor (`drop fn S1(self) { @dbg(self.x0); }`), so its
drop is observable; the `@drop` discharges the binding explicitly, and the
`@panic` then abandons the program.

### What the checker demands, and what it does not

`@panic` is `never`-typed (§5.7, `3.4:2`), and (Sub-Never) admits it wherever
a value of any type is expected. `Typed.panic` folds both in exactly as
`Typed.ret` does: it concludes at **any** type and at any outgoing context of
the same skeleton, so `Ty` needs no `never` constructor and `HasTy` no case
for it — sound because `never` has no values (`3.4:1`).

The premise `Typed.ret` carries and `Typed.panic` does **not** is the
interesting half. §5.7 gives `return` the provenance `⊥_exit`, which "carries
the §5.6 scope-exit/drop obligation", so `Typed.ret` demands
`NoOwnedLinear`: no binding of the frame may still be `Owned` at a linear
type. `@panic` carries `⊥_panic`, which §5.7 exempts — "§5.6 performs no
scope-exit check or drop on that edge" — so `Typed.panic` demands nothing.
The two rules differ in one premise, and that premise is the whole difference
between an exit that unwinds and an exit that abandons.

| Node | Rule | Concludes |
| --- | --- | --- |
| `S1 { x0: 7 }` | (Struct-Intro) §5.8 | `⇒ S1` |
| `@drop(v0)` | (@Drop) §5.3 | `⇒ unit`, and `v0` becomes `MovedOut` |
| `@panic("boom")` | (Panic) §5.8 + (Sub-Never) §5.7 | `⇒ i64` — the type `check` picks is the enclosing return type, the same choice it makes for `return` (`Checker.lean`) |
| `@drop(v0); @panic(…)` | (Seq) §5.3 | `⇒ i64`; the discarded `unit` carries no linear value |
| `let v0 = …; …` | (Let) §5.3 + §5.6 | `⇒ i64`; `v0` is `MovedOut` at the body's end, so the leak check has nothing to ask |

### The run, step by step

| Step | Rule | Store before | Effect | Store after | Events |
| --- | --- | --- | --- | --- | --- |
| 1–2 | literal, (D-Struct) §6.5 | `[]` | the literal becomes `{ 7 }_S1` | `[]` | |
| 3 | `(D-Let) §6.7` | `[]` | mint `ℓ0` for `v0` | `[ℓ0 = S1 { 7 }]` | |
| 4 | `@drop` §6.11 | `[ℓ0 = S1 { 7 }]` | the glue runs — the destructor is the observable half — and the cell is marked `⊘` rather than retired, so the binding stays reinitializable (§6.8/§6.11) | `[ℓ0 = ⊘]` | `drop ℓ0 = S1 { 7 }`; `run drop fn S1(S1 { 7 })` |
| **5** | **`(D-Panic) §6.12`** | `[ℓ0 = ⊘]` | the configuration is abandoned: `↯user`. **No `endscope` runs** — the `let`'s (D-EndScope) never fires, and nothing unwinds σ, which is the dynamic face of §5.7's `⊥_panic` exemption | — | |
| 6 | `(Panic-Lift) §6.2` | | the trap is carried out of the suspended `main() → f0()` context: **no frame is popped**, `run-all-scope-drops` never runs, and the callee's open scopes go with the configuration | — | |

The result is `EvalRes.panic .user [drop ℓ0 …, dtor S1 …]` — the trap, and
the two events that had already happened. `Corpus.outLines` projects the
observable ones, so the exported expectation is

```json
{"kind": "panic", "panic": "user", "stdout": ["7"]}
```

and `crates/rue-oracle-diff` compares *both* halves: a native binary that
trapped with the right category but lost the destructor line is a
disagreement. Verified by hand before the expectation was written: the
compiled program prints `7` on stdout, `panic: boom` on stderr, and exits
101.

### Why the drop is the explicit one

Note which drop shows. The `@drop(v0)` at step 4 is the one that ran; the
binding's *scope exit* never happened, because step 5 abandoned the
configuration. Take the `@drop` away and the destructor never runs at all —
`panicPastAffine` in `Examples.lean` is that program, kernel-checked to an
empty trace, and the compiler does the same (a live destructor-bearing
binding prints nothing at a `@panic`). That is §5.7's exemption doing visible
work: a `return` in the same position would have unwound the frame and
printed the line through `run-all-scope-drops`, which is the previous worked
example.

## 5e. A fifth worked example: a float conversion, and the one float trap

Floats are in the core (§2, §5.8, §6.4), and they are the one construct whose
IEEE side the mechanization *assumes* rather than proves. The corpus case
`float_to_int_trap_inf` is where that boundary is easiest to see, because it
is the only trap a float program can reach.

### The program

```lean
fintrin (.floatToInt .w32 .signed) (binop .div (flE .w64 1 0) (flE .w64 0 0))
```

printed as

```rue
fn f0() -> i32 {
    { let t1c: f64 = (1.0 / 0.0); let t1k: i32 = @float_to_int(t1c); t1k }
}
```

The two binders are the printer supplying types nothing downstream names:
`@float_to_int` takes its result type from the *use* site (`3.12:17`) and
fixes nothing about its operand, exactly as `@intCast` does not
(`Print.lean`, "Integer typing"). A float literal would otherwise default to
`f64` (`3.12:8`) — which is what is wanted here, but not at `f32`.

### What the checker demands

| Node | Rule | Concludes |
| --- | --- | --- |
| `1.0`, `0.0` | (Lit) §5.8 at `float(64)` | `⇒ f64`. `3.12:9` *rounds*, so an inexact decimal like `0.1` denotes the nearest `f64` rather than being rejected; the one premise is `3.12:10`, which refuses a literal whose value rounds to an **infinity** at the width (`E0206`) — `RueCore.FloatLit.RoundsFinite`, an exact comparison against `max_{𝔽_w}` plus half an ulp. An underflow to zero is legal |
| `1.0 / 0.0` | (Float-Arith) §5.8 | `⇒ f64`. One `w` for both operands — `3.12:13` gives no implicit widening — and `BinOp.floatAdmits` is §5.8's "rejected by the absence of a rule" for `%` and the bitwise operators, written as a side condition because one constructor stands for (Float-Arith), (Float-Ord) and (Total-Cmp) |
| `@float_to_int(…)` | (Float-To-Int) §5.8 | `⇒ i32`. Whether the value *survives* is dynamic, not a typing question (`3.12:18`) |

### The run, step by step

| Step | Rule | Effect |
| --- | --- | --- |
| 1–2 | (Lit) §6.3 | each literal becomes `M.ofLit`'s datum — `3.12:9`'s rounding, which is the **model's**, not this module's |
| 3 | **`(D-Float-Arith) §6.4`** | `1.0 / 0.0 → +inf`. Not a trap: none of (D-Arith-Trap), (D-Div-Zero) or (D-Div-Overflow) is stated over a float redex, and `3.12:22` fixes the answer — a finite non-zero over a zero is the infinity of the xor sign |
| 4 | **`(D-Float-To-Int-Trap) §6.4`** | `+inf` is neither truncatable nor in range, so the conversion traps. The category is `↯overflow`, the one §6.12 already lists (`8.1:7`), which is why the compiler reports `integer overflow` here and not a float-specific message |

The exported expectation is

```json
{"kind": "panic", "panic": "overflow", "stdout": []}
```

and the compiled program exits 101 with `error: integer overflow` — checked
case by case, like every other.

### Where the assumption is, and where it is not

Step 4 is a **theorem**: `floatToInt_partition` (`RueCore/Float.lean`) says
the premises of `(D-Float-To-Int)` and `(D-Float-To-Int-Trap)` partition
`𝔽_w`, which is what §7 asks for and what keeps progress intact. It is
provable because §2 models a float as a *datum*, so truncation toward zero is
exact integer arithmetic.

Step 3 is an **assumption**: `FloatModel.div_by_zero`, `3.12:22` as §6.4
quotes it. `Examples.floatDivZeroToInt_traps` is the two steps together,
stated over an *arbitrary* `FloatModel` and proved from its laws — so the
witness is a claim about IEEE 754 rather than about this package's instance,
and it computes no float at all. That is also why it costs no axiom: Lean's
own `Float` is defined over an `opaque` constant, and a theorem that so much
as mentions one reports `Classical.choice`.

The laws are **structure fields**, not `axiom` declarations, so a theorem
that rests on one says so in its own statement and `TRUST.md` lists them in a
section of their own. `Float.exactOps`, the instance the corpus runs, is
constructive integer arithmetic; that it *satisfies* the laws is the residual
assumption, and it is checked by running the float corpus against the
compiler rather than proved.

## 5f. A sixth worked example: a partial move, drawn

The slice RUE-2231 adds is the one where both sides of the invariant stop
being flat, so this example draws them. The corpus case is
`partial_move_residue`, over the fixture declarations

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }     // the observation channel
struct S7 { x0: S1, x1: S1 }             // no destructor of its own
```

### The program

```rue
fn f0() -> i64 {
    {
        let v0: S7 = S7 { x0: S1 { x0: 1 }, x1: S1 { x0: 2 } };
        {
            let v1: S1 = v0.x0;      // a PARTIAL move: 3.8:22
            { @drop(v1); 9 }
        }
    }
}
```

`v0.x0` is a place — `Place.proj (Place.var 0) 0` in the core — and using it
in value context moves *exactly* that field. Everything the slice is about is
visible in three snapshots.

### Before the partial move

Σ's state for `v0` and the contents of its cell, side by side:

```
  Σ(v0)                      H(ℓ0)
  Owned                      S7 { S1 { 1 }, S1 { 2 } }
```

`Owned` with nothing after it is a claim about the whole subtree: `Σ(v0) =
Owned` and no path under `v0` is `MovedOut`, which is `fully-owned(Σ, v0)` (§5
preamble). The cell holds the matching hole-free tree, which is
`ContentsMatches`'s `owned` clause.

### After it

(Use-Move) §5.1 marks exactly `v0.x0` and removes every path under it; §6.3's
(D-Use-Move) writes `H[ℓ0@[0] ↦ ⊘]` at exactly the same position:

```
  Σ(v0)                           H(ℓ0)
  Owned{ x0: MovedOut }           S7 {
    ├─ x0  MovedOut   ← v0.x0         ⊘,
    └─ x1  Owned      ← v0.x1         S1 { 2 }
                                  }
                                  H(ℓ1) = S1 { 1 }    ← v1, the moved value
```

`Owned{ x0: MovedOut }` is one state written one way: it is exactly how
`explain/partial_move_residue.txt` renders it, and `OwnSt.fields [.movedOut,
.owned]` is how `Statics.lean` builds it — a node that still owns its storage,
with one field taken out from under it. A slot the brace leaves out is `Owned`,
which is why `x1` needs no mention there.

Three things follow, and each is a premise somewhere:

* `Σ(v0)` is still `Owned` — the node is a field record, not `MovedOut` — so
  `v0.x1` is readable (`3.8:53`, the `copy_through_partial` case does exactly
  this with a `Copy` sibling) and `@drop(v0)` is legal (§5.3 asks only
  `Σ(p) = Owned`; the `drop_field_then_whole` case);
* `fully-owned(Σ, v0)` is now **false**, so `let v2 = v0` has no derivation —
  the aggregate has a hole and (Use-Move) may not hand it to a new owner
  (`3.8:26`; the compiler's E0205, the `partial_then_whole` case);
* had `S7` declared a destructor, the move would have been rejected before any
  of this (`3.9:34`, E0456; the `partial_under_dtor` case), because a
  destructor runs on the whole value and would meet the `⊘`.

### At scope exit

`v1`'s scope ends first: `@drop(v1)` already marked it, so its `endscope`
drops nothing. Then `v0`'s scope ends, and §6.11's walk runs on the *cell
contents*, skipping every `⊘`:

```
  drop(ℓ0) = drop(S7 { ⊘, S1 { 2 } })
           = (S7 declares no destructor)
             drop(⊘)  ++  drop(S1 { 2 })
           = []       ++  [dtor S1 (S1 { 2 })]
```

So the trace is `1` (from the `@drop(v1)`) then `2` (from the residue), and
the moved field is dropped **once**, by its new owner. That single skip is
§7's double-free argument, and `dropContents_struct_events`
(`Soundness.lean`) is it in closed form: the destructor's event, then the
fields' events concatenated in declaration order, a moved-out field
contributing none.

### What the checker demanded

`check` reads the same three snapshots: `en.st.get p.path` finds the state for
`v0.x0` (and returns `none` — a rejection — when a *proper prefix* of the path
is `MovedOut`, which is (Owned-Base) §5.1 in one lookup);
`en.ty.atPath P.decls p.path` types the place; `u.fullyOwned` is `3.8:26`;
`noDtorPrefix` is `3.9:34`. Its acceptance is a `Typed` derivation
(`check_sound`), so `soundness` applies and `run` cannot reach a `Violation` —
in particular not the `useAfterMove` a second drop of `S1 { 1 }` would be.

`example : ProgramTyped (prog tI64 partialMoveResidue) := checkProgram_sound (by rfl)`
and the pinned trace beside it in `Examples.lean` are the kernel-checked form
of this paragraph, and `scripts/rue exec` on the printed program prints
`1`, `2`, `9`.

## 5g. A seventh worked example: a declared-linear destructure, drawn

The slice RUE-2236 adds is the one where a use of a *field* consumes something
other than that field, so this example draws the transition. The corpus case is
`destructure_residue_order`, over

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }        // the observation channel
linear struct S15 { x0: S1, x1: i64, x2: S1 }
```

### The program

```rue
fn f0() -> i64 {
    {
        let v0: S15 = S15 { x0: S1 { x0: 1 }, x1: 5, x2: S1 { x0: 2 } };
        {
            @dbg(10);
            { let v1: i64 = v0.x1;       // a DESTRUCTURE: 3.8:33
              { @dbg(20); v1 } }
        }
    }
}
```

`v0.x1` is an `i64` — a `Copy` place. An ordinary use of it would copy and
change nothing. It does neither, and §4.2 says why: "a declared-linear
destructure plan is the central override; it consumes the selected enclosing
place even when `T` is `Copy`".

### Selecting the plan

Elaboration computes §4.2's `dl(Γ, p)` from the root's declared type and the
path, and `declaredPrefix` (`Syntax.lean`) is that function:

```
  declaredPrefix D S15 [1]
    = some ([], [1])          -- π_d = ε,  π_s = [x1]
```

`π_d` is the **longest proper prefix** whose type is a struct declared
`linear`. Here that is the empty path — `v0` itself — so the consumed place `d`
*is* the binding. Where the chain runs deeper the answer is the innermost one:
`declaredPrefix D S13 [0, 0]` is `some ([0], [0])`, so `y.x0.x0` consumes
`y.x0` and leaves `y` alone (`3.8:33`, the `destructure_two_levels` case). And
where no prefix carries the attribute the answer is `none`, which is §5.1's
`Ordinary` plan and the premise (Use-Copy)/(Use-Move) carry.

### The residue gate

Before anything can be destroyed, §5.1 asks `¬ linear-residue(S, π_s)`:

```
  residue(S15, [1])          = [ x0 : S1,  x2 : S1 ]      -- declaration order
  linear-residue(S15, [1])   = false                       -- neither is Linear
```

`linearResidue` (`Syntax.lean`) computes it on the **types**, one step at a
time: every unselected member is retained, the selected one is recursed into.
At an **array** step the members are the `n` elements in ascending index order
(§5.1 states the two clauses together), so a destructure at `x.arr[0]` retains
`arr[1]`, then whatever follows `arr` in the declaration — which is the order
`drop*` then destroys them in (`Examples.destructureThroughIndex`). Had `x2`
been declared `linear`, the access itself would be the error —
"the `linear-residue` premise rejects the access before any residue can be
silently dropped" — which is `3.8:60` and the compiler's E0474
(`destructure_linear_residue`). Had the recursion needed to go a level deeper
to find it, it would have: `Examples.destructureNestedLinearResidue` puts the
`linear` field one plain-struct step below the selected one, and the
`checkProgram … = false` example over it in `Examples.lean` pins that (probe
d22). It is a kernel-checked witness rather than a corpus case, so it has no
`explain/` file.

### The ownership transition

The rule's Σ effect is (Use-Move)'s, taken at `d` rather than at `p`:

```
  before                              after
  Σ(v0)   Owned                       Σ(v0)   MovedOut
  H(ℓ0)   S15 {                       H(ℓ0)   ⊘
            S1 { 1 },                         (and ℓ1 = 5, the selected leaf,
            5,          ← v0.x1                once (D-Let) mints it)
            S1 { 2 }
          }
```

The two sides move together, which is `ContentsMatches`: `MovedOut` on the Σ
side, `⊘` on the store side, at the one path `π_d`. Nothing marks `v0.x1`,
because `v0.x1` is not what was consumed.

### The residue's drops, in order

§6.3 runs `split` and then `drop*`, and the order is the traversal's:

```
  split(S15 { S1 { 1 }, 5, S1 { 2 } }, [1]) = ( 5 , [ S1 { 1 }, S1 { 2 } ] )
  drop*(H, [ S1 { 1 }, S1 { 2 } ])          = [ dtor S1 (S1 { 1 })
                                              , dtor S1 (S1 { 2 }) ]
  then H[ℓ0 ↦ ⊘]
```

so the trace is `1` then `2`, **at the access** rather than at scope exit, and
the run table in `explain/destructure_residue_order.txt` shows both events on
the one `(D-Use-Declared-Linear) §6.3` row. Where the selected path passes
through a nested struct, the recursion appends the nested residue before the
later sibling (`destructure_nested_residue`); where the form is `@drop` rather
than a use, §6.11 then drops the selected leaf *after* the residue
(`drop_declared_residue_first`).

### What the checker demanded, and what the proof gives back

`check` computes `declaredPrefix` first and only then looks anything up:
`en.st.get π_d` and `u.fullyOwned` are `fully-owned(Σ, d)` at the *consumed*
place (`3.8:26`), `linearResidue` is the gate above, `noDtorPrefix` over the
**whole** path is `3.9:34` read at "every enclosing value, including `d`"
(E0456, `destructure_under_dtor`), and `en.ty.atPath … p.path` is the leaf's
type, which is what the rule concludes at.

On the proof side the case is `useDeclared` in `soundness`, and it rests on
three lemmas: `ContentsMatches.declaredPlan_eq`, that the plan the machine
reads off the store is the plan the rule selected; `splitResidue_ok`, that
`split` never fails on a hole-free well-typed aggregate and that every retained
subtree is non-`Linear`; and `dropResidue_events`, that the residue's trace is
§6.11's events concatenated in the traversal's order. The `⊘` at `π_d` is then
re-established exactly as (Use-Move)'s is.

## 5h. An eighth worked example: a `match`, and the two drops it does not do

Enums are the slice RUE-2320 adds and RUE-2325 generates, and the thing to
watch is not the branch — that is `if` again — but the *payload*. The corpus
case is `enum_match_affine`, over

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }     // the observation channel
enum E0 { K0(S1), K1 }                   // class(E0) = Affine, through S1
```

`class(E0)` is the join over **every** payload component of **every** variant
(`6.3:19`), not over the variant a value happens to hold: the active variant is
a run-time fact, so §3 cannot read it. Here the join is `class(S1) = Affine`,
so `E0` is `Affine` and a use of an `E0` place is a move.

### The program

```rue
fn f0() -> i64 {
    {
        let v0: E0 = E0.K0(S1 { x0: 1 });
        {
            let v1: i64 = match v0 {
                E0.K0(v1) => { @dbg(10); 5 },
                E0.K1     => { 6 },
            };
            { @dbg(20); v1 }
        }
    }
}
```

That is the core form; the printed program is the same with `@dbg`'s operand
bound to a `let` (`Print.lean`'s typed blocks, and RUE-2336). It prints `10`,
`1`, `20`, then the value `5`. The `1` is `S1`'s destructor, and **where** it
falls is the whole example: between the `10` and the `20`, at the *arm's* end,
not at `v0`'s scope exit and not twice.

### What (Match) demands

(Match) §5.5 has four premises, and the mechanization is each of them
literally:

* the scrutinee is typed first, at the enum type, and whatever typing it did to
  Σ is what the arms start from. `class(E0)` is not `Copy`, so `v0` is typed by
  (Use-Move) §5.1 — the `match` **consumes** it, because a scrutinee is a
  value context and a use of a move-type place there moves it (`3.8:7`,
  `3.8:76`, `6.3:17`; the declared-`linear` destructure of `3.8:33` is a
  different rule, and `E0`'s class here is `Affine`);
* exhaustiveness is `arms.length = ed.variants.length`, with arm `j` the arm
  for variant `j`. There is no coverage search and no ordering side condition,
  because the core form has no wildcards and no guards — those are elaboration
  obligations §5.5 states (`4.7:9`, `4.7:10`). `exhaustive_arm_exists`
  (`Soundness.lean`) is the one line progress needs: a variant index the
  declaration has is an index the arm list has;
* every arm is typed from the **same** post-scrutinee state, under its own
  variant's payload locals (`armCtx`), and all arms at one type. An arm is a
  branch, not a step in a sequence, so Σ is not threaded from arm to arm;
* at the arm's end the payload locals leave scope under §5.6, and what is left
  after popping them is that arm's contribution to `Σ' = join(Σ1, …, Σn)`.

### Σ, at the four points that matter

`Σ` is read here exactly as `explain/enum_match_affine.txt` renders it — a
binder, its type, and its ownership state.

```
  before the match         [v0: E0 = Owned]
  Σ0, after the scrutinee  [v0: E0 = MovedOut]          ← (Use-Move) §5.1
  inside arm K0            [v1: S1 = Owned, v0: E0 = MovedOut]
  inside arm K1            [v0: E0 = MovedOut]           ← no payload to bind
```

The payload local enters `Owned` and **unmarked**: §2 gives a pattern binding
no `μ`, so nothing may assign to one, and the compiler's parser rejects `mut`
there. Arm `K0` pops its one local at its end, arm `K1` pops none, and the two
outgoing states are then joined:

```
  Σ1 = [v0: E0 = MovedOut]        (arm K0, after popping v1)
  Σ2 = [v0: E0 = MovedOut]        (arm K1)
  Σ' = join(Σ1, Σ2) = [v0: E0 = MovedOut]
```

`join(Σ1, …, Σn)` is unordered in the calculus and a **left fold** of the
binary join here (`Ctx.joinAll`): the arms in declaration order, starting from
the first arm's state. The binary join is proved commutative
(`OwnSt.join_comm`) and associative (`OwnSt.join_assoc`) over states that are
shapes of their types, so `Ctx.joinAll_perm` says the fold's arm order does not
matter. Well-formedness is a premise rather than a consequence of the judgment,
because (Return) and (Panic) conclude at any context (RUE-2340).

### The run, step by step

The row numbers are `explain/enum_match_affine.txt`'s.

```
  [5]  (D-Let) §6.7        let v0 = E0.K0⟨S1 { 1 }⟩ at ℓ0
                           store  [ℓ0 = E0.K0⟨S1 { 1 }⟩]
  [6]  (D-Use-Move) §6.3   v0
                           store  [ℓ0 = ⊘]                        ← the scrutinee moved out
  [7]  (D-Match) §6.6      bind E0.K0's payload to [ℓ1]
                           store  [ℓ0 = ⊘, ℓ1 = S1 { 1 }]
  [9]  (Dbg)               @dbg prints 10
  [12] (D-EndScope) §6.6   endscope([ℓ1])
                           events >> drop ℓ1 = S1 { 1 }; run drop fn S1(S1 { 1 })
  [15] (Dbg)               @dbg prints 20
  [19] (D-EndScope) §6.7   endscope([ℓ0])                          ← ℓ0 is ⊘: nothing drops
```

Row [7] is (D-Match): the tag `K0` selects the one covering arm, the payload
components are bound to **fresh cells**, and those cells are appended to the
innermost scope record *and* owed to an `endscope` marker around the arm's
body — exactly as (D-Let) §6.7 binds one. That is why row [12] falls where it
does: the drops run when the arm's body becomes a value, which is `6.3:17`'s
timing, and not at some later frame pop. It is also why an unwinding `return`
inside an arm still finds them in σ (`enum_return_past_payload`).

### The two drops that do not happen

Row [19] is the point. `v0`'s scope ends with its cell at `⊘`, so
(D-EndScope) drops **nothing** there — and that is not an optimization, it is
what keeps the payload from being dropped twice:

* §6.11's enum case reads the run-time tag and recurses into the **active**
  variant's payload only (`6.3:20`). An inactive variant's payload has no
  storage, and a discriminant-only active variant drops nothing at all.
  `enum_drop_unmatched` holds one of each: the `K0` value's active payload
  does drop, and prints its `1`, while the `K1` binding beside it drops
  nothing;
* a payload a `match` binding already moved out left the enum place `⊘`, and
  the walk skips every `⊘`. So the destructor at row [12] is the only one, and
  `S1 { 1 }` is destroyed exactly once, by the owner the arm gave it.

An enum has no destructor of its own to run before either (§3 gives it no
`drop fn`; where one is written the compiler reports `[E0417]: unknown type
'E0' in destructor` — destructor lookup does not see enums at all, so the
diagnostic is about the name rather than about enums), so the payload's
destructor is the entire observation channel at an enum drop.

### What the checker demanded

`check` reads the same four premises: `check` on the scrutinee, which yields
the enum type and `Σ0`; `arms.length = ed.variants.length`; `checkArms`, which
runs every arm from `Γ₀` under `armCtx` at the type `firstArmTy` fixed and
requires `NoResidualLinear` over the entries the arm pops; and `Ctx.joinAll`.
Its acceptance is a `Typed` derivation (`check_sound`), so `soundness` applies
and `run` cannot reach a `Violation`.

Change one thing and each premise answers in turn. Make the payload `linear`
and leave it, and the §5.6 check at the arm's end is the leak
(`enum_arm_leaks_payload`, E0406). Consume the enum in one arm of an `if`
only, and the join has `MovedOut` against `Owned` at a `Linear` type
(`enum_match_one_arm`, E0443). Match it twice, and the second scrutinee is the
use of a moved-out place (`enum_matched_twice_moving`, E0205). Make the
scrutinee a field of a struct, and the move is `3.8:22`'s partial one, whose
sibling still drops at scope exit (`enum_match_projection`, and
`enum_holder_partial_then_drop` for the explicit drop of the residue).

`example : ProgramTyped (enumProg tI64 enumMatchAffine) := checkProgram_sound (by rfl)`
in `Examples.lean` is the kernel-checked form of this paragraph, and
`scripts/rue exec` on the printed program prints `10`, `1`, `20`, `5`.

## 5i. A ninth worked example: the right-hand side before the index

The corpus case `array_dyn_write_rhs_first` is the smallest program where the
order of an assignment's operands is observable. `5.2:14` is normative: "the
right-hand side `expression` is evaluated first … any index subexpressions
appearing in the target … are evaluated after the right-hand side, in source
order", and §6.2's `assign p = E` context says the same (RUE-305 corrected an
earlier comment there that had it the other way round).

### The program

```lean
letIn true (aiPair 1 2 3 4)
  (seq (indexWrite (.var 0) [call 1 [lit 1]] [[0]] (call 2 [lit 9]))
    (seq (dbg (lit 20)) (lit 7)))
```

`f1` is `id`, which prints its argument and returns it; `f2` is `mk`, which
prints its argument and builds an `S1` of it. `indexWrite p idx πs e` is the
place `p`, then one dynamic step per index in `idx`, each followed by the
constant path at the same position of `πs` — here one step, then `.x0`. It
prints as

```rue
let mut v0: [S8; 2] = [S8 { x0: S1 { x0: 1 }, x1: 2 }, S8 { x0: S1 { x0: 3 }, x1: 4 }];
{ v0[{ let t3i0: i64 = f1(1); t3i0 }].x0 = f2(9); };
```

Each index is a typed block in place, so the printed statement keeps the
surface's own order.

### What the checker demands

`Typed.indexWrite` types the right-hand side **first**, at the leaf type `S1`,
and threads Σ from it into the index list (`TypedArgs` at integer types). Then
it reads the array `v0` on the post-operand state: `fully-owned` there, and
`assignArrayOk` above it (nothing to check, `v0` is the root). The last premise
is (Assign) §5.2's `Σ1(p) = MovedOut ∨ ¬carries_linear(T)` at the leaf, and a
place under a runtime index is never `MovedOut`, so it is `class(S1) ≠ Linear`.

### The run, step by step

| Step | What happens | Events |
| --- | --- | --- |
| 1 | the right-hand side `mk(9)` runs and builds `S1 { 9 }` | `@dbg 9` |
| 2 | the index `id(1)` runs | `@dbg 1` |
| 3 | `dynPlace` resolves `v0[1].x0` to the constant path `[1, 0]`, bounds-checking `1 < 2` | |
| 4 | §6.8's overwrite-drop of the old leaf `S1 { 3 }` | `drop`, `dtor S1 { 3 }` |
| 5 | the store writes `S1 { 9 }` at `[1, 0]`, and the array back into `ℓ0` | |
| 6 | `@dbg(20)`, then the scope exit drops `v0` ascending | `20`; `1`, `9` |

So stdout is `9 1 3 20 1 9 7`, and the compiler prints the same.

Had the index been out of range, step 3 would trap, and the `S1 { 9 }` built
at step 1 would never be dropped: a trap runs no drops (§6.12), and the
compiler does the same (`array_dyn_write_trap_negative`). Had the index
`return`ed instead, the value would be lost the same way, which is RUE-2316's
pending-value edge (`../03-metatheory.md`).

## 5j. A tenth worked example: an element moved out, and the rest dropped ascending

The array slice (RUE-2235) adds one thing the struct examples cannot show: a
place whose step is an **index**, and a residue that §6.11 walks by position
rather than by field. The corpus case is `array_elem_move_rest_ascending`,
over the fixture declaration

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }     // the observation channel
```

### The program

```rue
fn f0() -> i64 {
    {
        let v0: [S1; 3] = [S1 { x0: 1 }, S1 { x0: 2 }, S1 { x0: 3 }];
        {
            let v1: S1 = v0[1];          // an ELEMENT move: 3.8:68
            { @drop(v1); { @dbg(20); 7 } }
        }
    }
}
```

In the core, `v0[1]` is `Place.idx (Place.var 0) 1` (`Examples.arrayElemMove`).
Its path is `[1]`: `Place.path` does not tell an index step from a field step,
and neither does anything that navigates by it — `Ty.fieldAt` reads the step
as a field slot at a struct type and as a constant index at an array type. So
the element move goes through **the same rules** as `v0.x1` would, and the
only question the array adds is whether the index may be moved out of at all.

### The path rules, premise by premise

`use (v0[1])` is a use in value context at a non-`Copy` type, so the rule is
(Use-Move) §5.1, read under §4.2's `Ordinary` plan (`[S1; 3]` is not a struct
declared `linear`, so `declaredPrefix` is `none`). Its premises, as `check`
reads them:

| Premise | Here | Where |
| --- | --- | --- |
| `Γ ⊢ v0[1] : S1` | `[S1; 3]` stepped at `1 < 3` is `S1`; an index `≥ 3` would have no type, which is `7.1:9`'s compile-time bounds check (E0902) | `Ty.atPath` |
| `Σ(v0[1]) = Owned`, `fully-owned` | nothing under `v0` has moved yet | `OwnSt.get`, `fullyOwned` |
| no proper prefix declares a destructor (`3.9:34`) | the only proper prefix is `v0`, and an array declares none — `S1`'s destructor is the **leaf**'s, which is allowed | `noDtorPrefix` |
| an index step only off the root (`3.8:68`) | the step is the first off the root binding | `rootIdxOnly` |

The last one is the array's own. `h.arr[1]`, an element of an array reached
through a field, and `a[1][0]`, an element of an element, both fail it, and the
compiler reports both as E0904 (`Examples.arrayElemMoveThroughField`,
`arrayElemMoveNestedIndex`): `3.8:68` tracks an element move only where the
index is applied directly to the binding.

### Σ and the store, before and after

Before the move, `v0` is `Owned` and its cell holds three whole elements:

```
  Σ(v0)                      H(ℓ0)
  Owned                      [S1 { 1 }, S1 { 2 }, S1 { 3 }]
```

(Use-Move) marks exactly `v0[1]`, and (D-Use-Move) §6.3 writes `⊘` at exactly
the same position:

```
  Σ(v0)                                H(ℓ0)
  Owned{ x0: Owned, x1: MovedOut }     [
    ├─ [0]  Owned      ← v0[0]           S1 { 1 },
    ├─ [1]  MovedOut   ← v0[1]           ⊘,
    └─ [2]  Owned      ← v0[2]           S1 { 3 }
                                       ]
                                       H(ℓ1) = S1 { 2 }    ← v1, the moved element
```

`Owned{ x0: Owned, x1: MovedOut }` is how `explain/array_elem_move_rest_ascending.txt`
renders the state, and it names positions the way it names fields: `x1` is
position `1`, here index `1`. The slot the brace leaves out, index `2`, is
`Owned`. This is `3.8:73`'s per-element drop flag, and it is the whole of it:
there is no separate flag, only the path's `MovedOut`.

Three things follow, each a premise somewhere:

* `v0[0]` and `v0[2]` are still readable and movable (`3.8:68`: sibling
  elements remain usable; `OwnSt.get`
  finds `Owned` at both), and `v0[1]` is not (E0205);
* `fully-owned(Σ, v0)` is now **false**, so `let v2 = v0` is refused (`3.8:70`,
  `7.1:45`, E0205; `Examples.arrayWholeAfterElemMove`), and so is a read at a
  **dynamic** index, which could name the hole (`3.8:70`; E0205 for a read,
  E0480 for a write, `Examples.arrayDynWriteAfterElemMove`);
* a write into the array — `v0[1] = S1 { 9 }`, at the hole as much as at a
  sibling — is refused (`3.8:72`, `7.1:46`, E0480; `Examples.arrayElemReinit`):
  an element write does not give back per-element ownership, and the
  recovery is the whole-array assignment (`Examples.arrayWholeReinit`).

### The residue's drops, in order

`@drop(v1)` runs `S1 { 2 }`'s destructor where it stands: `2`. Then `@dbg(20)`
prints `20`. Then `v1`'s scope ends (already `MovedOut`, nothing to drop) and
`v0`'s, and §6.11's walk runs on the **cell contents**, elements in ascending
index order, skipping every `⊘`:

```
  drop(ℓ0) = drop([S1 { 1 }, ⊘, S1 { 3 }])
           = (an array has no `drop fn` of its own; dropping it drops its
              elements in index order, 3.9:14–15)
             drop(S1 { 1 })  ++  drop(⊘)  ++  drop(S1 { 3 })
           = [dtor S1 (S1 { 1 })] ++ [] ++ [dtor S1 (S1 { 3 })]
```

So stdout is `2`, `20`, `1`, `3`, then `main`'s `7`, and the compiler prints
the same. The moved element is dropped once, by its new owner; the residue
drops `1` before `3` because `3.9:15` orders an array's elements by index.
`dropContents_array_events` (`Soundness.lean`) is the walk in closed form: no
event of the array's own, then each element's events, concatenated in order.

### What the checker demanded, and what the proof gives back

`check` accepted by reading exactly the four rows above, so `check_sound` turns
the acceptance into a `Typed` derivation and `soundness` applies: `run` cannot
reach `useAfterMove` (the `⊘` at `[1]` is never read) or `useAfterDrop` (the
walk skips it). The kernel-checked form is the pair of examples beside
`arrayElemMove` in `Examples.lean`: `checkProgram … = true` and the pinned trace

```lean
[.drop 1 (cA 2), .dtor sAffine (cA 2), .dbg (v64 20),
 .drop 0 (.array (.struct sAffine) [cA 1, .hole, cA 3]),
 .dtor sAffine (cA 1), .dtor sAffine (cA 3)]
```

which is the drop above, event for event, `.hole` being the `⊘`.

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
side, and the pairs that differ; last a tally of the shape

```text
  cases: <n> (<n-k> agree, <k> disagree)
  checker <-> compiler: …
  lean <-> oracle: …
  lean <-> native: …
  oracle <-> native: …
```

**Expect five disagreements, and expect a non-zero exit.** Each one is seeded
deliberately and stays until its issue is decided or fixed:
`destructure_ancestor_dropped` (RUE-2335, a spec decision: the compiler reports
E0406 where the model accepts); and `array_dyn_write_after_destructure_via_field`
(RUE-2341, a compiler defect: it accepts a write below a dynamic index that
`3.8:72` forbids, then double-drops and leaks); `array_elem_self_assign`
(RUE-2346, a decision: the model refuses `a[0] = a[0]` under `3.8:72` and the
compiler accepts it since RUE-228); and `array_zero_length_field_dyn_read`
(RUE-2345, a compiler defect: a dynamic-index read from a zero-length array
field is an internal error where the model traps with `bounds`). Two more were red until
RUE-2344: `array_dyn_write_after_field_move`, a write below a dynamic index
into the moved array field `h.x0`, which the compiler now refuses with E0205 as
the model does; and `array_write_after_destructure_via_field`, the constant
twin of the RUE-2341 case, which the compiler now refuses because the write's
base `h.arr[0]` is consumed — with E0205 where `3.8:72` says E0480, a code the
bridge does not compare and RUE-2341 still owns. The fifth,
`i64_min_times_neg1`, is `min_T * -1` at `i64`. §6.4's (D-Arith-Trap), `3.1:6`
and `8.1:3` make it an overflow trap and the model traps; the compiler's
constant folder wraps it and the program exits 0. It is narrow — only `i64`,
only `*`, only with two literal operands; `{ let a: i64 = min; a * -1 }` and
the same through two calls both trap — so it is the folder, and it is a
compiler defect: RUE-2318. `cond_drop_affine` is the precedent: it first made the
compiler report an internal error instead of a verdict (`E9000`, a
CFG-verification failure on the conditionally dropped affine residue), so the
oracle, which shares that frontend, could not run it either. That was
RUE-2290, found by this bridge and fixed, and the case stays as the
regression signal.

*A defect looks like:* any **other** case disagreeing.
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
