# Reading RueCore without Lean

This guide is for a Rue contributor who reads the core calculus
(`../01-core-calculus.md`) and has never opened a Lean file. It explains each
Lean artifact in the calculus's own terms, works programs from Rue source
through the checker and the interpreter to the theorem that covers them, and
says how to run things yourself.

The one idea to hold on to: **the mechanization is the calculus, typed into a
proof assistant.** It is not a second semantics. Where the two texts differ in
shape (a judgment becomes a datatype, a reduction relation becomes a
function), this guide says how the shapes correspond.

## How to read this guide

- **To understand the mechanization**, read sections 1–4 in order: how a
  judgment, the machine, the invariant and the theorem look in Lean. Then read
  example 1, which takes one program through all four. The other examples
  each show one more idea; the table at the top of section 5 says which.
- **To decide whether to believe it**, go straight to section 7, "Validating
  this in thirty minutes". It uses the two generated reports, `DIGEST.md`
  (every statement) and `TRUST.md` (every statement's axioms), says at each
  step what a defect would look like, and points back into sections 2–5 where
  it needs them.
- **To run something yourself**, section 6. **To add a declaration**, section
  8.

`INDEX.md` (generated) maps every calculus rule and every §2 syntactic form to
the declaration that mechanizes it, or says *not yet mechanized*. `README.md`
has the build commands and the file map.

"Section 3" means a section of this guide; `§5.1` is a section of the
calculus, and `3.8:26` is a paragraph of the prose specification.

## 1. A judgment is an inductive type

The calculus writes the typing judgment as

```
Γ ; Σ ⊢ e ⇒ T ⊣ Σ'
```

"under type environment `Γ` and ownership state `Σ`, expression `e` has type
`T` and leaves the ownership state as `Σ'`". The calculus's full form also
carries the loan set `Λ`; the fragment has no borrows, and `Λ` is ambiently
empty in the current core (§5 preamble), so `Statics.lean` omits it. Each rule
of §5 is a horizontal bar: premises above, conclusion below, the rule's name
in parentheses.

`Statics.lean` writes the same judgment as

```lean
inductive Typed (P : Program) (R : Ty) : Ctx → Expr → Ty → Out → Prop
```

Read `Typed P R Γ e T Ω` as the judgment above, with §5.3's outgoing result
`Ω` for `Σ'`: `Ω` (`Out`) is a normal outgoing state `some Σ'`, or `none` for
§5.7's `⊥` when evaluation never continues past `e`, together with the edge
deliveries `Δ` a later rule reads a state from. (The fragment records only
`⟨break, Σ⟩` deliveries, each the whole context in force at its `break`,
which the enclosing loop reads; a `return` discharges its obligation where it
fires.) `P` is the program: its
functions are where (Call) §5.8 looks up a callee's signature, and its
declarations (`P.decls`) are what types are read against. `R` is the enclosing
function's declared return type, which (Return-Value) §5.7 checks a `return`
operand against. The calculus fixes both for a function body, so the
mechanization makes them parameters of the whole judgment.

Four things differ in shape and not in content:

- **`Γ` and `Σ` are one list.** A `Ctx` has one entry per binding in scope,
  and each entry carries both the fixed part (`ty`, and the `μ` mark `mu`) and
  the flowing part (`st`, the ownership state). The calculus keeps two
  environments indexed by the same bindings; the mechanization keeps one.
  `Typed.skel_preserved` proves that every rule leaves the fixed part alone,
  which is what lets the calculus write `Γ` once and thread only `Σ`.
- **Variables are positions, not names.** `use 0` is the innermost binding,
  `use 1` the one outside it (de Bruijn indices). Elaboration resolves names
  before the core, and the mechanization starts after that step. When
  `Print.lean` prints Rue source it names each binder `v<depth>`, counting
  from the outermost, so `v0` is the outermost binder.
- **Each rule is a constructor.** `Typed.useMove` *is* (Use-Move): its
  arguments are the rule's premises, its result is the rule's conclusion, and
  its doc-comment names the rule and the prose paragraph it encodes. A program
  `e` is well-typed exactly when a value of type `Typed P R [] e T Ω` exists
  for some `T` and `Ω`; that is what "there is a derivation" means.
- **`never` is folded into the rules that produce it.** §5.7 types
  `return e` at `never` and lets (Sub-Never) coerce it to whatever the context
  needs, with a divergent outgoing state `⊥`. `Ty` has no `never`: a `never`
  value does not exist (`3.4:1`), so nothing is ever typed at it dynamically.
  Instead `Typed.ret`, `Typed.panic`, `Typed.brk`, `Typed.loopDiv` and the
  `-Bottom` rules §5.7 types at `never` conclude at *any* type, and at `⊥`. The `⊥` itself is in the
  judgment: a branch joins only the arms that continue, and nothing is typed
  past a diverging subexpression, as §5.3's (Strict-Bottom), (Seq-Bottom) and
  (Let-Bottom) say. `INDEX.md` records (Sub-Never) at the rules that fold it
  in.

For example, (Use-Move) §5.1 says: a use of a `fully-owned`, non-`Copy` place
has the place's type and marks the place `MovedOut`, removing every path under
it. In Lean:

```lean
| useMove {Γ p en u T} :
    Γ[p.root]? = some en →
    en.st.get p.path = some u → u.fullyOwned = true →
    en.ty.atPath P.decls p.path = some T →
    T.mult P.decls ≠ .copy →
    noDtorPrefix P.decls en.ty p.path = true →
    declaredPrefix P.decls en.ty p.path = none →
    rootIdxOnly P.decls en.ty p.path = true →
    Typed P R Γ (.use p) T ⟨some (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut))), []⟩
```

Premise by premise:

1. `Γ[p.root]? = some en`: the place's root binding exists, with entry `en`.
2. `en.st.get p.path = some u`: Σ has a state `u` for the path. It has one
   exactly when no *proper prefix* of the path is `MovedOut`, so this lookup
   is (Owned-Base) §5.1 (`3.8:53`).
3. `u.fullyOwned = true`: the place is `fully-owned` (`3.8:26`: an aggregate
   with a hole may not be handed to a new owner).
4. `en.ty.atPath … = some T`: the path reaches a declared field at every step
   and lands at type `T`.
5. `T.mult P.decls ≠ .copy`: `T`'s class is not `Copy`.
6. `noDtorPrefix … = true`: no proper prefix of the path declares a destructor
   (`3.9:34`, E0456).
7. `declaredPrefix … = none`: the use plan §4.2 records for the place is
   `Ordinary`. §5.1 reads (Use-Copy) and (Use-Move) only with an `Ordinary`
   plan.
8. `rootIdxOnly … = true`: `3.8:68`'s "element moves only at the root". It
   walks `Ty.fieldAt` from the root's declared type rather than reading `.idx`
   versus `.proj` off the place, because it is the type at a node that makes a
   step an index step.

The conclusion marks exactly `p`, and its `Ω` is `⟨some Σ', []⟩`: the use
continues, and makes no delivery. The calculus's one remaining premise,
`p not loaned`, concerns loans, which are outside the fragment; `INDEX.md`
lists which rules and sections are in.

## 2. The dynamics is a function

§6 gives a small-step machine: configurations `⟨H ; φ ; K ; e⟩` and a
reduction relation between them. `Dynamics.lean` gives the same dynamics as a
definitional interpreter:

```lean
def eval (M : FloatOps) : Nat → Program → Store → Frame → Expr → EvalRes
def run (M : FloatOps) (P : Program) (fuel : Nat) : EvalRes :=
  eval M fuel P [] { env := [], scope := [] } (.call 0 [])
```

`M` supplies float arithmetic; it matters only for float programs (example
10). The `Nat` is the fuel, explained below.

- `H` is §6.1's store: a list of cells, each `full c` for live contents or
  `dead` for a retired allocation `†`. The contents `c` is a **tree**, because
  §6.1's moved-out marker `⊘` may sit at any node of it, not only at the root:
  a partial move writes `H[ℓ@π ↦ ⊘]` at exactly the sub-position it takes
  (§6.3, §4.2), and a whole-place move is the case `π = ε`.
- `φ` is §6.1's frame: the environment `ρ` (position `i` ↦ its location in
  `H`) and the scope record `σ` (the cells this frame owes a drop, in creation
  order).
- `run` is §6.12's top-level result: call the entry point, function `0`, with
  no arguments.

Rather than take one step, `eval` runs the program to the end and reports one
of six outcomes:

| `EvalRes` | Meaning in §6 |
| --- | --- |
| `.ok H' v tr` | the machine halted normally with value `v`, final store `H'`, and trace `tr` |
| `.returned H' v tr` | an unwinding `return` handed `v` back (§6.9's (D-Return)); every enclosing form passes it on until a call boundary absorbs it |
| `.broke H' sc tr` | an unwinding `break` (§6.10's (D-Break)), carrying the scope record `sc` of the frame it fired in; every enclosing form passes it on until its loop catches it and drops the cells the body still owed |
| `.panic k tr` | the machine halted in a defined trap `↯κ` (§6.12) — `overflow`, `divZero`, `remZero`, `castOverflow`, `bounds` or `user` — carrying the trace `tr` of what ran before it |
| `.stuck w` | the machine refused: `w` names either a configuration §6 leaves undefined or a linear action the machine monitors (see below) |
| `.outOfFuel` | not a machine state at all: the interpreter's admission that it stopped early (see below) |

The trace lists, in order, everything the machine did that a program can see:

- `drop ℓ c`, a binding's drop (at scope exit, at `@drop`, when
  overwritten, or a retained subtree of a declared-linear destructure's
  residue). It records the *contents* dropped, which after a partial move is
  a tree with holes in it.
- `dropTemp v`, a discarded temporary.
- `consume c`, the shell a `match` or a destructure consumes: every member
  already moved out or dropped, so no drop of its own runs (RUE-2427).
- `dtor s c`, a user destructor §6.11 ran.
- `dbg v`, a `@dbg`: §6.12's observable output.

The last two are what a Rue program prints; the first two mark where a drop
starts, and `consume` where a value's life ends without one. The trace is the fragment's image of the oracle interpreter's
observable outcome, and the bridge compares it against a native binary's
stdout (`README.md`, "The bridge corpus"). A trap carries a trace too, because
a trapping process prints what it printed and then exits 101.

**Every aggregate value has an identity.** §6.1 gives values none: two
`S1 { 1 }` are the same term. To say *which* value a drop was of, a struct,
enum or array literal **mints** one when it is built, and the value carries it
from then on — `S1 { 1 }#3` in a step table, `.struct s 3 [...]` in Lean. The
identity is the store's next index, reserved by appending a `†` slot that no
binding ever names (§6.1: "a fresh identity is one not in `dom(H)`"), so the
stores in the tables below have `†` slots between the bindings, and a
binding's location is the next index after them. Nothing in `eval` branches on
an identity, and the printer and the corpus never print one, so the bridge
compares exactly what it compared before; the step tables below print `#n`
only for the reader's benefit. What it buys is section 4's last theorem,
`no_double_free`: in a checked program's trace, no identity has its
destructor run twice, and no identity appears twice among the
`drop`/`dropTemp` free events.

A `Violation` is a refusal: `useAfterMove` (reading a `⊘` cell),
`useAfterDrop` (touching a `†` cell), `linearLeak` (a scope exit or a frame
unwind reaching a live linear value), `linearOverwrite` (`3.8:77`),
`linearDiscard` (`3.8:64`), `ownedUnderCopy` (an owned value put under a
`Copy` node, which a copy would duplicate), and `unbound` and `typeConfusion`
for ill-scoped or ill-typed input. Each of §7's memory-safety bullets says
that one of these never happens. The three linear refusals and
`ownedUnderCopy` are monitors the machine adds: §6's rules would drop the
value, or build the aggregate, and rely on §5 to have forbidden it. The other
four are §6's own stuck states. So `eval` is a model of §6 on the programs `check`
accepts, and only there (`Dynamics.lean`).

Why a function rather than the relation? A function can be *run*, so every
semantic question about a fragment program can be answered by executing it.
And a total function always returns one of the six outcomes, so progress
becomes the single statement "never `.stuck`", which section 4's theorem
proves. What the function owes the relation is an adequacy lemma (the two
agree on every checked program), required before the mechanization gates
anything (`../03-metatheory.md`, "How to read a theorem here"). Its first
half, soundness, is proved (`eval_sound`, below), and so is its second half,
completeness modulo fuel (`eval_complete`).

**Two presentations of one dynamics.** The relation exists too:
`Step.lean` defines `Step`, §6's `C → C'` itself, one constructor per §6
rule with the rule's name in its doc-comment, over a configuration built from
the same store and frame `eval` uses. The two are kept for different jobs.
`Step` is what §6 *says*, so a reader checks it against the calculus rule by
rule, and §7's own phrasing ("no reduction sequence reaches a stuck
configuration") is a statement about it. `eval` is what can be *run* and
*proved about*: the safety theorem is a fuel induction over it, and the bridge
compares its results with the compiler's. Neither alone is enough: a theorem
about `eval` says nothing about §6 unless the two agree, and `Step`, though
it runs (`stepN` takes its steps, and `letAddProgram_runs` and the `demo_`
theorems run whole programs through it), is not what the bridge runs against
the compiler, and it does not carry the safety proof as cheaply. The adequacy
theorems (RUE-2289's parts 2 and 3) are the bridge between them. Besides
them, `Step`'s own theorems are the cheap ones — it is deterministic, a
finished configuration takes no step, and a stuck one is stuck on one of §6's
four violations, never on one of `eval`'s four monitors
(`../03-metatheory.md`). Where `Step` departs from §6's text, and the
metatheory row and `Step.lean`'s module docstring give the same list:

- §6.2's evaluation context `E` and §6.1's stack `K` are one list of frames,
  so the rule that searches into a context is two constructors, one entering
  the hole and one plugging a value back in;
- `endscope` pops its cells off the frame by count, because bindings are de
  Bruijn indices where §6.7 relies on α-renaming;
- the loop boundary sits above its context's frames, because §6.10's
  `loopβ(e, φ)` records no context;
- `push-scope` is one scope record read by length, so (D-Loop-Iter) and
  (D-Break) drop the cells past the loop's record;
- the use plan is recovered from the store rather than read off `μ`;
- a destructor is one trace event rather than a nested run;
- aggregate introduction mints a value identity by reserving a `†` slot, as
  `eval` does, so the two presentations keep one store;
- `Config.init` calls the entry point, so (D-Return-Main) is (D-Return)
  reaching its `call` frame;
- a dynamic read, `@drop` at a dynamic place, or repeat operand that is not
  `Copy` is stuck, the premise of the rule each cites.

On programs `check` rejects, `Step` follows §6 where `eval` does not: `@drop`
of a `⊘` place is §6.11's no-op where `eval` refuses it.

**Soundness: what `eval` answers, §6 reaches.** `Adequacy.lean` proves the
first adequacy theorem. For a program `check` accepts, `eval_sound` says three
things: `run` is never `.stuck` (that is `no_violation`); if it answers a
value, §6.12's initial configuration reaches, by `Step`, the terminal
configuration holding that value, with the same store and the same trace; and
if it panics, `Step` reaches the same panic after the same trace. The proof is
a simulation, `Sim`, read off each of `eval`'s outcomes: the expression in
focus under *any* context reaches the context's hole with the value, or the
panic, or (for an unwinding `return` or `break`) the nearest caller or loop.
Each `andThen` in `eval` becomes one enter step, the operand's run, and one
plug step. A surprise: the simulation needs no typing at all (`run_sim` holds
on every program), because every place `eval` and `Step` differ is a refusal
on `eval`'s side, and a refusal promises nothing. Typing only fixes the
domain, by ruling `.stuck` out. `letAddProgram_sound` is the theorem at work:
the same `→*` derivation `letAddProgram_runs` found by stepping, obtained from
`run`'s answer alone.

**Completeness: what §6 reaches, `eval` answers.** The converse needs no second
simulation, only a count. If `eval` runs out of `fuel` on an expression, `Step`
has a run of exactly `fuel` steps from it (`eval_steps_of_outOfFuel`): every
unit of fuel `eval` spends is paid for by a step. `Step` is deterministic, so a
run that reaches an end in `n` steps has no longer run beside it. At any fuel
above `n`, then, `run` cannot be out of fuel, and whatever it answers,
soundness places at that same end. On a checked program, that makes
`eval_complete`: `run` answers §6's value or panic, with the same store and
trace, at every fuel past the length of §6's run. It also makes
`never_stuck_iff`: "`run` is never `.stuck`" is equivalent to "every
configuration `Step` reaches reduces or has halted", which is §7's own
phrasing. And it makes `eval_diverges_iff`: `run` is out of fuel at every fuel
exactly when §6's run never ends. On a program `check` rejects, `eval` may
refuse where §6 carries on. `dropMoved_refused` is `@drop` of a moved-out
place, which is §6.11's no-op and `eval`'s `useAfterMove`.

### One program, traced both ways

Why keep two presentations of one dynamics at all? §6 is written as a
relation, so the relation is the thing a reader can hold against the
calculus rule by rule, and §7's promises ("does not get stuck", "types are
preserved under reduction") are sentences about it. The interpreter is what
can be run against the compiler, and what the safety proof is an induction
over. Adequacy is what lets a result about one be read as a result about the
other. Here is one corpus program, `affine_scope_drop`, in both (the
corpus prelude's other declarations omitted):

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }
fn f0() -> i64 {
    {
        let v0: S1 = S1 { x0: 7 };
        1
    }
}
```

`explain/affine_scope_drop.txt` renders `eval`'s run in seven rows, one per
evaluated node, premises first. `Step` takes twelve steps from §6.12's
initial configuration to `✓1`, and `affineScopeDrop_both_ways`
(`Adequacy.lean`) writes them out, one constructor each. The two columns
line up like this, with `K` the frame stack below the focus (the top frame
first) and `call` the entry point's `ret(E, φ)`:

| Step | Constructor | §6 rule | What changes | `explain/` row |
| --- | --- | --- | --- | --- |
| 1 | `callEnter` | (Search) §6.2 | focus on `f0()`'s empty argument list | [1] |
| 2 | `call` | (D-Call) §6.9 | push `call`; focus on the body | [1] |
| 3 | `letEnter` | (Search) §6.2 | push `let v0 = □; 1`; focus on `S1 { x0: 7 }` | [4] |
| 4 | `structEnter` | (Search) §6.2 | focus on the initializer list | [3] |
| 5 | `argsPush` | (Search) §6.2 | push `S1{ □ }`; focus on `7` | [3] |
| 6 | `intLit` | §6.3 | `7` is a value | [2] |
| 7 | `argsPlug` | (Search) §6.2 | pop `S1{ □ }`; the list holds `7` | [3] |
| 8 | `mkStruct` | (D-Struct) §6.5 | store `[ℓ0 = †]`: mint `#0`; value `S1 { 7 }#0` | [3] |
| 9 | `letBind` | (D-Let) §6.7 | store gains `ℓ1 = S1 { 7 }#0`; push `endscope([ℓ1])`; focus on `1` | [4] |
| 10 | `intLit` | §6.3 | `1` is a value | [5] |
| 11 | `endScope` | (D-EndScope) §6.7 | `ℓ1` dropped and retired; trace gains `drop ℓ1`, then `S1`'s destructor | [6] |
| 12 | `callReturn` | (D-Return-Value) §6.9 | pop `call`; the frame's record is empty; `✓1` | [7] |

Two things differ, and neither is a disagreement:

- Five of the twelve steps are (Search): entering a subexpression, or
  plugging a value back into its context. `eval` does these by recursion, so
  they have no row of their own. Each unit of fuel `eval` spends is paid for
  by at least one step, either an enter step or the rule step that puts the
  next subexpression in focus, which is why `eval_steps_of_outOfFuel` can
  count fuel in steps.
- `explain/` lists a node after its premises, so (D-Let)'s row [4] follows
  the struct literal's rows it waited for. `Step` interleaves the same work:
  step 3 enters the `let`, and step 9 is its rule firing.

The end states are equal: the same store (`[†, †]`), the same value (`1`),
and the same trace. `affineScopeDrop_both_ways` proves exactly that, and also
proves `check` accepts the program. The adequacy theorems say that this
agreement is not special to one program: on every program `check` accepts,
`eval`'s value or panic is §6's (`eval_sound`), §6's is `eval`'s at every
fuel past the run's length (`eval_complete`), and `outOfFuel` at every fuel
is §6 running forever (`eval_diverges_iff`).

**What that buys: §7 in its own terms.** The safety theorem is proved once,
over `eval`. Adequacy then carries it to `Step`:

- `step_progress`: every configuration §6 reaches from a checked program's
  initial one reduces or has halted, so none is stuck.
- `step_preservation`: every configuration §6 reaches is typed at the entry
  point's return type. A step from a typed configuration lands on a typed
  one — equivalently, `step_progress` plus `step_value_typed`; the one-step
  half holds on every program.
- `step_type_safety`: at every horizon `n`, §6 has run `n` steps, or has
  halted with a value of the declared type, or with a defined panic.

"Typed" here is `Config.SafeAt`, a *semantic* typing: nothing the
configuration reaches is stuck, and every value it halts with has the type.
Preserving it is immediate. The work is in showing the initial configuration
has it (`init_safeAt`), and that is `run_safe` carried over by
`eval_complete`. A syntactic typing of configurations would be a second
safety proof, one case per `Step` constructor, and the mechanization does not
claim one. For `affine_scope_drop`, `step_value_typed` reads off that the `1`
§6 halts with is an `i64`.

Off the checked domain, the two presentations can part. `dropMoved_refused`
is a program `check` rejects, `@drop` of a moved-out binding: §6.11 makes it a
no-op and `Step` reaches `✓0`, while `eval` refuses it with `useAfterMove`.

### Fuel, and why the theorems quantify over it

Lean accepts a function only if it can see that the function terminates.
Recursion on subexpressions is enough for most of `eval`, but not for a call:
the callee's body is not a subexpression of the call, and a recursive function
calls itself, so no amount of looking at the program's syntax bounds how far
the machine goes. A Rue program can loop forever; `eval` must not.

So `eval` carries **fuel**: a number, one unit of which every step spends.
When it reaches zero the interpreter stops with `.outOfFuel`. That is not a
state of §6's machine and not a claim about the program; it is the
interpreter reporting that *it* gave up. Unspent fuel costs nothing, so a
bound far larger than any program needs is free (`Examples.lean` runs its
demos at `demoFuel`, 400; the corpus exporter uses 100,000).

The theorems then say: *for every* fuel bound, a well-typed program's result
is a well-typed value, an unwinding return, a defined panic, or `outOfFuel`,
and never a violation. The last disjunct looks like a loophole: an
interpreter that always answered `.outOfFuel` would satisfy the theorem and
say nothing. Two lemmas close it:

- **`fuel_mono`**: if some bound produces an answer other than `outOfFuel`,
  every larger bound produces that same answer. Raising the bound never
  changes a result: a program has one outcome, and a sufficient bound finds
  it.
- **`no_masking`**: if some bound reaches a violation, every bound that
  answers at all reaches that same violation. No choice of fuel can hide a
  violation behind exhaustion.

So for a program that completes at some bound, "for every fuel" is a
statement about its one real outcome. `Examples.lean` shows both sides:
`run demoOps countdown 16` is `outOfFuel`, `run demoOps countdown 17` is the
value, and `fuel_mono` proves every larger bound agrees. A program that does
*not* complete is the other side: every turn of a `loop` spends fuel, so
`loop { () }` is `outOfFuel` at every bound (`infiniteLoop_outOfFuel`), and
the corpus, which exports only completed runs, leaves it out.

The two lemmas close the loophole from `eval`'s side. Adequacy closes it from
§6's side. On a checked program, `run` is out of fuel at every bound exactly
when §6's reduction never ends (`eval_diverges_iff`). And when §6's run does
end, every bound past its length finds the end (`eval_complete`).

### The one edge no monitor covers

There is one place where the theorems say less than "never a violation"
suggests, and it concerns `return` and `break`, not fuel. A by-value
argument's value sits in no cell and no scope record until `mintParams` gives
it one. If a *later* argument of the same call unwinds by `return` or `break`,
(D-Return) §6.9 or (D-Break) §6.10 discards the earlier value with the
evaluation context. No drop runs and no monitor fires,
so a linear value is consumed zero times without any violation. That is the
calculus as written and what the compiler does, not a modelling slip.
`Dynamics.lean`'s "Pending values" section states it, `Examples.lean`'s
`linearLostAtCallArg`, `affineLostAtCallArg` and `linearLostAtBreakArg` (the
`break` case) are kernel-checked witnesses, and closing it is RUE-2316.

## 3. The invariant: what `Matches` says, and why it is asymmetric

Every safety proof carries an invariant relating the static story to the
dynamic one. Here it is `FrameMatches D Γ φ H` (`Soundness.lean`), where `D`
is the program's declarations, against which `class(T)` and a value's drop
are read. It has two fields, `store` and `record`, and the proof also carries
a third fact, `Untouched`.

**`store`: `Matches D Γ ρ H`.** For each binding, the cell at its location
agrees with its context entry; every location is inside the store; and no two
bindings share a location.

The per-cell agreement is recursive, because both sides are trees. Σ's state
for a binding is an `OwnSt`: `owned`, `movedOut`, or `fields [t₁ … tₖ]` for a
value some of whose fields or elements have been moved out (the explainer
output and the drawing in example 5 write this `Owned{ x0: MovedOut }`).
The cell holds `Contents`: the same shape, with `⊘` admitted at any node.
`ContentsMatches` relates the two node by node:

- an **`owned`** node holds hole-free, well-typed contents, that is, a value;
- a **`movedOut`** node holds well-typed contents with **no live linear
  sub-value** in it; it need not hold `⊘`;
- a **`fields`** node holds the struct its type names, matched field by field,
  or the array its type names, matched element by element; a slot no move
  touched reads as `owned`.

The `movedOut` clause is the asymmetry, and it is deliberate. For an *affine*
`x`, after `if c { @drop(x) } else { () }` the §5.5 join marks `x` `MovedOut`
on both paths, although on the `else` path it is still live. The static story
is conservative and the dynamic story is exact: the machine drops that
residue path-specifically at scope exit (§5.6, §6.7; `3.8:60`, and `3.8:73`
for array elements). The corpus case `cond_drop_affine` is this program, and
`partial_move_one_arm` is the same one field down. The invariant must allow
that gap.

What it must never allow is a live *linear* value behind a `MovedOut` node.
Then the static leak check could pass while the machine reached
`linearLeak`, and the theorem would be false. Where two arms disagree on a
path whose residue still carries a linear value, the join refuses outright
(`3.8:50`; the corpus cases `linear_half_consumed` and
`join_linear_field_one_arm`), and `ContentsMatches` records that refusal as an
invariant.

Two lemmas turn the clause into what the proof uses:

- `ContentsMatches.residualLinear_false`: the machine's leak monitor sees
  exactly what §5.6's `residual-linear` computes. After a partial move the
  obligation is the *residue*'s on both sides, which is the residue-keyed
  obligation model (RUE-1591) and what the compiler does.
- `ContentsMatches.readAt` and `ContentsMatches.writeAt`: navigating a path
  agrees on the two sides. Wherever Σ has a state for the path (wherever no
  proper prefix of it is `MovedOut`, (Owned-Base) §5.1), the store reaches a
  sub-position, and writing a matching state and contents there leaves the
  cell matched.

**`record`: `φ.scope.reverse = φ.env`.** The frame's scope record, read
newest-first, *is* its environment. §6.1 keeps both books: ρ says where a
binding lives, σ that it is owed a drop.

In this fragment the equation costs **nothing** to prove. Every frame the
interpreter builds (the callee's at a call, the extended one inside a `let`
body) builds σ and ρ from the same list, so the equation holds by definition.
It is stated as an invariant for two reasons:

- The teardown proofs consume it. `run-all-scope-drops` walks σ, and because
  σ is ρ, `Matches` applies to the walk: every cell is live or moved out, and
  no two bindings share one. That is why §7's no-use-after-drop bullet follows
  from the invariant rather than from a fact about closed expressions, and why
  no unwind touches a `†` cell or retires one twice.
- It stops being free when `Frame.scope` becomes the *stack* §6.1 specifies,
  where scopes are pushed and popped independently of the binder chain and σ
  and ρ are two books to keep in step. That is the shape for which the
  calculus keeps σ beside ρ (RUE-1277). This fragment does not have it:
  §6.6's `match` arm appends its payload cells to the one record, and a loop
  remembers the record's length at its entry instead of pushing one, so a
  `break` hands the loop the record it fired in and the loop drops the cells
  past that length. The equation stays definitional; what the loop's proof
  uses is `Matches.unwindPrefix`, the arm's own teardown lemma.

**`Untouched ρ H H'`** carries frame *locality*: the store only grows, and
every allocated cell that ρ does not name keeps its contents. A callee's
parameter cells are minted above the caller's whole store, so the caller's
bindings are outside the callee's ρ and their agreement survives the call.
That lets the proof step over a call knowing nothing about the callee but its
signature.

## 4. The theorem, and what `check_sound` buys

```lean
theorem soundness (M : FloatModel) (hwf : WfProgram P) :
    ∀ fuel, Typed P R Γ e T Ω → FrameMatches P.decls Γ φ H →
      EvalOk P.decls T R Ω.norm Ω.brk φ H (eval M.toFloatOps fuel P H φ e)
```

(implicit arguments omitted). `M` is any float model that satisfies the IEEE
laws the mechanization assumes (example 10), so the theorem holds for each
of them.

`EvalOk` is a predicate on the result rather than a disjunction of
existentials, which lets the proof discharge §6.2's operand search once and
reuse it at every form. It says:

- on `.ok`, the normal outgoing state `Ω.norm` is some `Σ'`, the value has the
  expression's type, the invariant holds at `Σ'`, and the cells outside the
  frame are untouched — and when `Ω` is `⊥` there is no `.ok` at all, so an
  expression the rules type as divergent never completes normally;
- on `.returned`, the value has the enclosing function's return type `R`, and
  the cells outside the frame are untouched;
- on `.broke`, the `break` fired at one of `Ω`'s delivered states, in the
  frame with the loop body's still-open bindings on top (`BrokeOk`);
- on `.panic` and `.outOfFuel`, nothing;
- on `.stuck`, **`False`**, which is the whole point.

That is progress and preservation in one statement (§7, first bullet).
Over §6's `Step`, its whole-program consequence is `step_progress` and
`step_value_typed`, with `step_preservation` packaging the two as a semantic
configuration typing, derived from this theorem by adequacy. The
per-expression invariant `FrameMatches` has no `Step`-side statement.

Over a whole program, `run_safe` says it in the shape a reader wants:

```lean
theorem run_safe (M : FloatModel) (hwf : WfProgram P)
    (h0 : P.fns[0]? = some fd) (hp : fd.params = []) (fuel : Nat) :
    run M.toFloatOps P fuel = .outOfFuel
      ∨ (∃ k tr, run M.toFloatOps P fuel = .panic k tr)
      ∨ (∃ H v tr, run M.toFloatOps P fuel = .ok H v tr ∧ HasTy P.decls v fd.ret)
```

The named corollaries (`no_use_after_move`, `no_linear_leak`, …) each restate
"never `.stuck` with this particular violation" for one §7 bullet.

**No double free** is the one bullet about the trace rather than the result
(`Trace.lean`):

```lean
theorem no_double_free (M : FloatModel) (h : ProgramTyped P) (fuel : Nat) :
    (∀ w, run M.toFloatOps P fuel ≠ .stuck w) ∧
      (∀ a, (freedIds P.decls (run M.toFloatOps P fuel).trace).count a ≤ 1) ∧
      (∀ a, (dtorIds (run M.toFloatOps P fuel).trace).count a ≤ 1)
```

In words: the run is never refused, and in its trace no identity has its
destructor run twice, and no identity appears twice among the
`drop`/`dropTemp` free events. `freedIds` reads the non-`Copy` identities out
of each `drop`/`dropTemp` marker's tree, and `dtorIds` the identity of each
value a destructor ran on. A `Copy` value is duplicated freely and frees
nothing, so neither counts it. A declared-linear destructure's residue is
dropped under a `drop` marker per retained subtree, and a `match` or a
destructure records the shell it consumes with a `consume` event (RUE-2427),
so `freedIds` sees every way an owned value's life ends.

The proof is a **conservation law**, `eval_conserves`, by the same fuel
induction as `soundness`: what the final store, the result and the trace own,
counted as a multiset, is at most what the initial store owned plus what was
minted, and minted identities are store indices, each once. It needs no typing
derivation. It needs **copy closure**, that nothing owned sits under a `Copy`
node, and the machine enforces that with its fourth monitor. Typing enters
through the first conjunct, `no_violation`: a checked program never reaches
the monitor, so its trace is the whole run's. `dupProgram_step_double_free` is
the ill-typed program that shows the monitor is doing real work: §6's
relation, which has no monitor, runs one destructor twice on one identity.

**From a program to the theorem.** The theorem quantifies over derivations.
To apply it to a *program* you need to know a derivation exists, and
`Checker.lean` is how you find out. `check P R Γ e` runs the §5 rules as an
algorithm, returning the type (possibly `never`) and `Ω`, or rejecting.
`checkProgram P` lifts that to (Fn) §5.8 for every function, plus the entry
point's empty parameter list. `check_sound` and `checkProgram_sound` prove
that every acceptance is backed by a real derivation; the second produces the
`ProgramTyped` hypothesis the program-level theorems take.

So for any program, run `checkProgram`:

- If it accepts, `run_safe` applies and the §7 guarantees hold for the
  program.
- If it rejects, the program is outside the theorem. `run` often shows which
  refusal the program would have reached, and the corpus prints it when there
  is one. A rejection can also be a plain type error, such as an out-of-range
  literal, which `eval` runs without complaint.

**Completeness is false, at a few shapes.** Not every derivable program is
accepted. `check` carries §5.7's `⊥`, so a diverging arm contributes nothing
to a join and a `@panic` past a live linear binding reaches no scope exit, as
the rules say. What it does not do is accept a `never` operand where it reads
a type off one: an operator's left operand, a cast or intrinsic operand,
`@dbg`, a repeat form, an index expression (`a[return 8]`). `(return 1) + 2`
is derivable — §5.3's (Strict-Bottom) types it at the operator's own type,
whatever integer type the `return` is coerced to — and `check` refuses it
rather than guess. `Checker.lean`'s module docstring lists every shape.
Nothing a reader would write turns on them.

**And an acceptance says less about dead code.** The `-Bottom` rules type
nothing past a diverging subexpression, so `return 1; (1 + true)` is accepted
here: the tail is unreachable and §5.3 does not check it. The compiler rejects
it, which §5.3 allows ("the surface checker may still … report ordinary errors
in unreachable source"). So for a program with syntax after a `return` or
`@panic`, an `accept` verdict does not mean the compiler must accept, and the
corpus has no such case.

Before RUE-2368 the checker had no `⊥` and was narrower: it refused a `return`
arm of an `if` or a `match` whose sibling kept a binding the arm moved, a
`match` whose first arm diverges, a `@panic` arm beside an arm that consumed a
linear binding, and a `@panic` past a live linear binding. The compiler
accepts all five, and the corpus seeds them now (`if_return_arm_affine`,
`match_return_arm_linear`, `match_never_first_arm`, `if_panic_arm_linear`,
`panic_past_linear`). The generator still emits neither `return` nor `@panic`
(`Gen.lean`).

### The three trace theorems, one sentence each

Three theorems read the drop trace rather than the result. Each is here in
one plain sentence, beside its Lean statement and a corpus program whose
trace shows it. Every explain rendering (`explain/<case>.txt`) ends with an
**identity ledger**: one line per owned identity, with the step that minted
it, the steps that ended it and the steps whose destructor ran on it. That
makes each claim something you can look at, not only something proved.

**No double free** (`Trace.lean`). *In a checked program's run, no value is
ended twice and no value's destructor runs twice.*

```lean
theorem no_double_free (M : FloatModel) (h : ProgramTyped P) (fuel : Nat) :
    (∀ w, run M.toFloatOps P fuel ≠ .stuck w) ∧
      (∀ a, (freedIds P.decls (run M.toFloatOps P fuel).trace).count a ≤ 1) ∧
      (∀ a, (dtorIds (run M.toFloatOps P fuel).trace).count a ≤ 1)
```

Witness: `partial_move_residue` (example 5). `v0.x0`'s value `#0` is moved
out and dropped through its new binding at row 10 of its explain table. The
scope exit at row 14
drops the residue `S7 { ⊘, S1 { 2 }#1 }#2`, and the walk skips the `⊘`, so
`#0` is not dropped again. The ledger's *ended* column has one entry for
each of `#0`, `#1` and `#2`.

**Exactly once** (`TraceExact.lean`). *Every owned value a well-typed
evaluation holds is, when the evaluation ends normally or unwinds, still in a
cell, part of the result, or ended exactly once: on the normal path or on
the unwind path, never both and never neither.*

```lean
theorem drop_exactly_once (M : FloatModel) (h : ProgramTyped P)
    (hp : P.pendingSafe = true) (ht : Typed P R Γ e T Ω)
    (hfm : FrameMatches P.decls Γ φ H) (hcc : StoreCC P.decls H)
    (he : e.pendingSafe = true) :
    (∀ w, eval M.toFloatOps fuel P H φ e ≠ .stuck w) ∧
      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) ∧
      Tidy φ H (eval M.toFloatOps fuel P H φ e)
```

`Exact` is the count equation over the identities the evaluation starts
with. `Tidy` says every cell the evaluation allocated has been retired.
`rest_exactly_once` is the same for the values a form's leading operands
produce, which is where a `let`'s initializer or a discarded `S { .. };`
ends. The carve-outs are a trap, which ends nothing (§6.12), and
`pendingSafe`, which excludes RUE-2316's abandoned operand.

Witness: `return_past_affine` (example 2). The `return` unwinds past two
live bindings. The σ-walk drops each once (`#2` and `#0`, row 9), and no
`endscope` runs for either: the ledger has one end per identity, and all of
them are on the unwind path.

**Drop order** (`TraceOrder.lean`). *Within a value, every destructor runs
inside the drop of the value that owns it, in §6.11's order; across cells,
every step tears its cells down newest first.*

```lean
theorem drop_order (M : FloatModel) (h : ProgramTyped P) (fuel : Nat) :
    (∀ w, run M.toFloatOps P fuel ≠ .stuck w) ∧
      Blocks P.decls (run M.toFloatOps P fuel).trace ∧
      ∀ C C', Steps M.toFloatOps P Config.init C → Step M.toFloatOps P C C' →
        ∃ evs, C'.trace = C.trace ++ evs ∧ NewestFirst (dropLocs evs)
```

The two halves need two presentations.

- **Within a value**, the statement is over `eval`'s trace. `Blocks` is a
  grammar: a trace is a sequence of `@dbg` lines, consumptions, and drop
  markers, each marker followed by exactly §6.11's walk of what it names
  (`dropEvents`). That is the destructor first, then the fields in
  declaration order, an array's elements ascending, and an enum's active
  payload only. A destructor event has no other place in the grammar.
- **Across cells**, the statement is over §6's `Step`, because the order
  comes from the scope record, which the trace does not show. Every scope
  record of every reachable configuration is in location order, which is
  registration order (`reachable_ordered`). So a teardown, which walks its
  record backwards, drops strictly newest first. Every other step drops
  sub-positions of one cell only.

Witnesses: `struct_nested_dtor_drop` (example 4) for the first half. `S5`'s
destructor (`#1`) runs before its field's (`#0`), both inside the one
`drop ℓ2` block. `return_past_affine` for the second: the σ-walk's one step
drops `ℓ3`, then `ℓ1` (`returnPastAffine_newestFirst`). In the ledger these
show as the ends `[9.1]` and `[9.2]`, the first and second end of row 9. `TraceOrder.lean` also pins ten
order-witnessing corpus cases through the theorems' projections, and has two
results the statement rejects:

- `fieldsSwapped_rejected`: a struct's two field destructors swapped;
- `unorderedRecord_rejected`: a frame whose record is out of location order
  takes a real step that drops oldest first. The invariant is what rules it
  out.

### Loops, briefly

`loop e` and its nullary `break` (§5.7, §6.10) add three things a reader
should know where to find; example 11 works through them.

- **The loop-head state.** A loop body is typed once, at the state in force
  at the loop head on every turn: the entry state joined with the state the
  body leaves at its back edge. That state is on both sides of its own
  definition, so the rule takes it as a premise, `LoopHead`, and the checker
  finds it by iterating from the entry state until it stops changing
  (`headIter`). `loop_reassign_then_move` is the seed where the head is not
  the entry state.
- **The back edge in the proof.** A turn that completes re-enters the loop
  at its head, and the same derivation types the loop there again
  (`LoopHead.reenter`, which rests on the join absorbing a second copy of the
  back-edge state), so `soundness`'s fuel induction takes every later turn.
- **The exits.** A `break` delivers the whole context in force where it fires
  (`Typed.brk`). The loop splits it: the bindings the body opened are dropped
  at the exit (`NoResidualLinear` over `Ctx.loopLocals`, and §6.10's unwind at
  run time), and the rest is joined over every exit (`Ctx.outsideLoop`,
  `3.8:80`). A linear binding consumed at one exit and not another is §5.5's
  join failure, E0443 (`loop_linear_one_exit`).

## 5. Worked examples

Each example is one corpus case (`Corpus.lean`), chosen to show one idea.
Read example 1 first: it takes a program through the checker, the
interpreter and the theorem in full. The others show only what is new.

| Example | Corpus case | What it teaches |
| --- | --- | --- |
| 1 | `reinit` | reading a derivation and a run end to end; the overwrite premise of (Assign) |
| 2 | `return_past_affine` | an early `return` unwinds the frame's scope record, newest first |
| 3 | `panic_after_drop` | a trap keeps the output already printed and runs no drops |
| 4 | `struct_nested_dtor_drop` | a struct's class, and §6.11's outer-then-fields drop order |
| 5 | `partial_move_residue` | a partial move: Σ and the store as trees |
| 6 | `destructure_residue_order` | a declared-linear destructure consumes the enclosing place |
| 7 | `array_elem_move_rest_ascending` | an element move, and the rest dropped in index order |
| 8 | `enum_match_affine` | a `match` consumes its scrutinee; where the payload drops |
| 9 | `array_dyn_write_rhs_first` | an assignment evaluates its right-hand side before its index |
| 10 | `float_to_int_trap_inf` | the one float trap, and where the IEEE assumption sits |
| 11 | `loop_move_every_path_breaks`, `loop_linear_one_exit` | a loop's head state, its back edge and its exits: RUE-1615's shape and RUE-1614's |

Every example has the same parts: **The program**, **What the checker
demands**, **The run**, and, where the proof needs something new, **What the
proof needs**.

**Conventions.**

- The struct declarations are the fixture `Examples.structEnv` (`S0`–`S10`),
  and later examples add enum and destructure fixtures. Each example shows
  only the declarations it uses.
- `explain/<case>.txt` (`lake exe ruecore-explain <case>`) is the full,
  generated version of each example: the whole printed program, every
  derivation node with its Σ, and every run step with the store. Where a table
  here numbers its rows, the numbers are that file's.
- The printer binds a `@dbg` operand to a typed `let`
  (`{ let t4g: i64 = 20; @dbg(t4g) }`); the programs here write it `@dbg(20)`.
- Σ is written the way the explainer writes it: `[v0: S2 mut = Owned]`, one
  entry per binding, innermost first.

**How every example ends.** Every example here is accepted, and the chain is
the same each time: `checkProgram` accepts, `checkProgram_sound` turns the
acceptance into `ProgramTyped`, and `run_safe` applies. So, for every float
model satisfying the laws, `run` reaches no `Violation`. The runs shown here
use `demoOps` (`Float.exactOps`); that it is such a model is the one
assumption example 10 names, and a program with no floats never exercises it.
Most examples have kernel-checked forms in `Examples.lean`, beside the
program's definition: the acceptance (`checkProgram_sound (by rfl)` or
`checkProgram … = true`) and the run's pinned trace. The bridge compares each
printed program's output with the compiled binary's.

Every other corpus case is a smaller worked example. Its printed source
begins with a comment naming the case, the rules it exercises and its
expected outcome; `corpus.json` (from `scripts/rue lean`, or `lake exe
ruecore-corpus`) holds all of them, and `explain/` renders each.

### Example 1: `reinit`, reinitializing a linear binding

The program discharges a linear value, assigns a new one back in, and
discharges that. It exercises (@Drop), (Assign) with the `3.8:77` premise
checked on the post-RHS state, reinitialization (`3.8:55`), and the
scope-exit leak check (§5.6).

#### The program

Core syntax, as `Examples.reinit` writes it:

```lean
letIn true (resL (lit 1))
  (seq (drop (.var 0))
    (seq (assign (.var 0) (resL (lit 2)))
      (seq (drop (.var 0)) (lit 2))))
```

This is the body of the entry function `f0`. `resL e` is `mkStruct sLinear
[e]`, a literal of `S2`: the declared-`linear` struct with one `i64` field and
no destructor. The bridge prints it as:

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

#### What the checker demands

`checkProgram` accepts, and `f0`'s body checks at `i64` with outgoing context
`[]`. The derivation it certifies, read in evaluation order:

| Step | Rule | Σ in | Σ out |
| --- | --- | --- | --- |
| `mkStruct sLinear [lit 1]` | `Typed.mkStruct`, (Struct-Intro) §5.8: one initializer per declared field, at the field's type | `[]` | `[]` |
| enter the `let` body | `Typed.letIn`, (Let): the binder enters `Owned` | `[]` | `[v0: S2 mut = Owned]` |
| `drop (.var 0)` | (@Drop) §5.3: `class(S2) = Linear` (§3: the declared attribute), and `@drop` is the one non-move discharge of a linear obligation (`3.9:39`) | `Owned` | `MovedOut` |
| `seq` discard | (Seq): `unit` carries no linear value (`3.8:64`) | | |
| `mkStruct sLinear [lit 2]` | the assignment's right-hand side, typed first | `MovedOut` | `MovedOut` |
| `assign (.var 0) …` | (Assign) §5.2: `v0` is `mut`; on the post-RHS state `v0` is `MovedOut`, so the `3.8:77` premise holds; the subtree at the path becomes `Owned` (`3.8:55`) | `MovedOut` | `Owned` |
| inner `seq` discard | (Seq): the assignment's `unit` carries no linear value | | |
| `drop (.var 0)` | (@Drop) again | `Owned` | `MovedOut` |
| leave the `let` body | §5.6's scope-exit check, folded into `Typed.letIn`: `residual-linear(Σ, v0, S2)` is `false` because `Σ(v0) = MovedOut`, so nothing leaks | `[v0: S2 mut = MovedOut]` | `[]` |

The premise that matters is in the (Assign) row. Without the first
`@drop(v0)`, the post-RHS state of `v0` would be `Owned`, the premise
`Σ1(p) = MovedOut ∨ ¬carries_linear(T)` would fail, and `check` would reject.
That is the corpus case `linear_overwrite`, which the compiler rejects with
E0493 and the machine refuses with `linearOverwrite`.

The premise reads the destination's **type**, as §5.2 writes it and as
`3.8:77` insists ("determined by the destination's *type* together with the
statically tracked move paths, never by a run-time drop flag"). This is the
one place in the fragment where reading the *residue* would be wrong. After
`@drop(v0.x0)` on a carrier whose only linear content is `x0`, the residue
carries nothing, yet `v0 = S10{…}` is still ill-formed because the type still
carries a linear value. The compiler agrees (E0493), and the corpus cases
`overwrite_past_partial_linear` and `overwrite_field_past_partial_linear`
(one field down) pin it.

§5.5's join and §5.6's leak check do read the residue (`ownedJoinOk`,
`residualLinear`), because they ask whether an obligation was
**discharged**. An overwrite discharges nothing, so it reads the type
(`overwriteOk`). At (Assign) the model is therefore strictly stricter than the
residual reading: both disjuncts of §5.2's premise imply
`residual-linear = false`, which keeps the machine's `linearOverwrite`
monitor unreachable from a program `check` accepts.

#### The run

`run` returns `.ok` with value `2`, final store `[dead, dead, dead]`, and the
two `drop ℓ1` events as its trace. Step by step, with the store `H` as a list
of cells indexed by location and `ρ` mapping position 0 to its location. Each
struct literal mints its value's identity (section 2), which reserves a `†`
slot, so `v0`'s cell is location 1, not 0:

| Step | Store before | Effect | Store after | Trace |
| --- | --- | --- | --- | --- |
| `mkStruct sLinear [lit 1]`, (D-Struct) §6.5 | `[]` | a value `{ 1 }_S2` with identity `#0`, reserving slot 0 | `[dead]` | |
| (D-Let): mint a cell for `v0` | `[dead]` | allocate location 1, `ρ = [1]` | `[dead, full S2 { 1 }#0]` | |
| `drop (.var 0)`, §6.11 | `[dead, full S2 { 1 }#0]` | the drop glue runs (`S2` declares no destructor, so nothing is observable), and the contents become `⊘` | `[dead, full ⊘]` | `drop ℓ1 = S2 { 1 }#0` |
| (D-Seq) discard | | `unit` is `Copy`: no drop | `[dead, full ⊘]` | |
| `mkStruct sLinear [lit 2]` | | a value with identity `#2`, reserving slot 2 | `[dead, full ⊘, dead]` | |
| `assign (.var 0)`, (D-Assign) §6.8 | `[dead, full ⊘, dead]` | the position is `⊘`, so nothing is dropped; reinitialize | `[dead, full S2 { 2 }#2, dead]` | |
| inner (D-Seq) discard | | `unit` is `Copy`: no drop | `[dead, full S2 { 2 }#2, dead]` | |
| `drop (.var 0)`, §6.11 | `[dead, full S2 { 2 }#2, dead]` | the glue runs again; the contents become `⊘` | `[dead, full ⊘, dead]` | `drop ℓ1 = S2 { 2 }#2` |
| (D-EndScope): retire `v0` | `[dead, full ⊘, dead]` | the contents are `⊘`, so nothing to drop; retire the cell | `[dead, dead, dead]` | |

The two drops name two identities, `#0` and `#2`: the reinitialization put a
new value in the old place, and each value was freed once, which is what
`no_double_free` counts.

No event is **observable**. `S2` declares no destructor, so the two
`drop ℓ1` events project to no stdout line (`Corpus.eventLine`), and the
scope exit finds a `⊘`. The printed program's only output line is the value,
`2`, and the bridge expectation in `corpus.json` is exactly that:
`{"kind": "ok", "stdout": ["2"], "exit": 0}`.

#### What the proof needs

The initial invariant, `FrameMatches D [] ⟨[], []⟩ []`, holds trivially: no
bindings, no store, an empty scope record. `run_safe` then gives, at every
fuel, `outOfFuel`, a defined panic, or `.ok` with a value of the entry
function's declared return type. The run above is the last case:
`HasTy D (.int .w64 .signed 2) (.int .w64 .signed)` holds because `2` is in
`i64`'s range.

The corollary this program illustrates is `no_linear_overwrite`. The
assignment in the middle is the shape `3.8:77` guards, and the theorem says
the guard is never needed at run time for a program the checker accepts,
because the checker has already demanded the `MovedOut` state that makes the
overwrite-drop a no-op.

### Example 2: `return_past_affine`, an early `return` unwinds the frame

A `return` under two open `let` scopes, each holding a live affine resource.
This is the smallest program in which the frame's **scope record** σ, rather
than the pending `endscope` markers, runs the drops: the shape for which
(D-Return) §6.9 says "in any evaluation context `E'`" (RUE-1277).

#### The program

```lean
letIn false (resA (lit 3))
  (letIn false (resA (lit 4))
    (ret (lit 7)))
```

`resA e` is a literal of `S1`, the affine struct whose destructor prints its
field (`drop fn S1(self) { @dbg(self.x0); }`). Printed:

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

#### What the checker demands

`Typed.ret`, (Return-Value) §5.7, has three premises:

- the operand is checked at the enclosing function's declared return type:
  `7 ⇒ i64`, and `i64` is `f0`'s return type;
- `NoResidualLinear P.decls Γ₁`: **no binding of the frame still carries a
  residual linear value** after the operand. This is §5.6's obligation, taken
  frame-wide because a `return` ends every open scope of the frame at once
  (`3.8:62`, and (Fn) §5.8's second clause);
- the outgoing context is free (that is `⊥`), restricted only to the same
  skeleton.

Here Σ at the `return` is `[v1: S1 = Owned, v0: S1 = Owned]`. Both bindings
are *affine*, so the premise holds and the program is accepted: an affine
value reaching an exit is dropped, which is legal and observable. Make either
one `S2` and the premise fails. That is the corpus case `return_past_linear`,
which the compiler rejects with E0406 and the machine refuses with
`linearLeak`.

#### The run

At the `return` the frame is `φ = ⟨ρ ; σ⟩` with `ρ = [ℓ3, ℓ1]` (innermost
binder first) and `σ = [ℓ1, ℓ3]` (creation order). Each `let` appended to σ,
so σ reversed is ρ: the `record` invariant of section 3. The even locations
are the `†` slots the two struct literals reserved for their identities,
`#0` and `#2` (section 2); nothing binds them.

| Row | Rule | Store before | Effect | Store after | Events |
| --- | --- | --- | --- | --- | --- |
| 1 | (D-Call) §6.9 (push the frame) | `[]` | `f0` takes no arguments, so no parameter cell is minted; `σ = []` | `[]` | |
| 2–4 | literal, (D-Struct) §6.5, (D-Let) §6.7 | `[]` | mint identity `#0` at `ℓ0`, then `ℓ1 = S1 { 3 }#0`; `ρ = [ℓ1]`, `σ = [ℓ1]` | `[ℓ0 = †, ℓ1 = S1 { 3 }#0]` | |
| 5–7 | literal, (D-Struct) §6.5, (D-Let) §6.7 | `[ℓ0 = †, ℓ1 = …]` | mint identity `#2` at `ℓ2`, then `ℓ3 = S1 { 4 }#2`; `ρ = [ℓ3, ℓ1]`, `σ = [ℓ1, ℓ3]` | `[ℓ0 = †, ℓ1 = S1 { 3 }#0, ℓ2 = †, ℓ3 = S1 { 4 }#2]` | |
| 8 | literal | | the operand `7` becomes a value | | |
| **9** | **(D-Return) §6.9 (unwind the frame)** | `[…, ℓ1 = S1 { 3 }#0, …, ℓ3 = S1 { 4 }#2]` | `run-all-scope-drops(H, φ)` walks `σ` **newest-first**: drop-retire `ℓ3`, then `ℓ1` | `[ℓ0 = †, ℓ1 = †, ℓ2 = †, ℓ3 = †]` | `drop ℓ3 = S1 { 4 }#2`; `run drop fn S1(S1 { 4 }#2)`; `drop ℓ1 = S1 { 3 }#0`; `run drop fn S1(S1 { 3 }#0)` |
| 10 | inner (D-EndScope): does not run | | the `return` discarded the evaluation context, and the pending `endscope` markers with it; the result travels out unchanged | | |
| 11 | outer (D-EndScope): does not run | | the same | | |
| 12 | (D-Return-Main) §6.9 (absorb) | `[ℓ0 = †, …, ℓ3 = †]` | the call boundary turns the unwound `return` into the call's value. `f0` is the bottom of the stack, so this is (D-Return-Main); at an inner call the same row is (D-Return)'s hand-off, which is why the generated table labels it with both | | |

**The drops run once, not twice.** Rows 10 and 11 are the `endscope`s the
normal path would have run. They see a `.returned` result and pass it on,
because `eval` sequences a `let` body with `andThen`, which continues only on
a value. Their cells were already retired at row 9; had either tried again,
`drop-retire` would have found a `†` cell and the machine would have refused
with `useAfterDrop`. The `record` invariant is what proves it cannot.

**The order is newest-first, and it is observable.** The trace is
`drop ℓ3`, then `drop ℓ1`, each followed by `S1`'s destructor, so the printed
program prints `4`, then `3`, then its value `7`. Two spec rules are at work:
`3.9:18` says *that* a `return` drops every live binding of every enclosing
scope, and `3.9:4` says *in what order* ("reverse declaration order (last
declared, first dropped)").

### Example 3: `panic_after_drop`, a trap keeps its output and unwinds nothing

The smallest program in which the *observable output* and the *trap* are
both part of the answer. §6.12 halts the program at a trap, but the process
has already printed whatever it printed, and `Outcome`, the thing the
differential harness compares, is exit status **and** stdout. So the
machine's `.panic` carries a trace.

#### The program

```lean
letIn false (resA (lit 7)) (seq (drop (.var 0)) (panic "boom"))
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

#### What the checker demands

`@panic` is `never`-typed (§5.7, `3.4:2`). `Typed.panic` folds (Sub-Never) in
exactly as `Typed.ret` does (section 1): it concludes at any type, and at `⊥`.

The difference from `return` is one premise. §5.7 gives `return` the
provenance `⊥_exit`, which "carries the §5.6 scope-exit/drop obligation", so
`Typed.ret` demands `NoResidualLinear` (example 2). `@panic` carries
`⊥_panic`, which §5.7 exempts ("§5.6 performs no scope-exit check or drop on
that edge"), so `Typed.panic` demands nothing. That one premise is the whole
difference between an exit that unwinds and an exit that abandons. (`check`
accepts a `@panic` past a live linear binding too, since it carries `⊥`
(RUE-2368); `panic_past_linear` is that case.)

| Node | Rule | Concludes |
| --- | --- | --- |
| `S1 { x0: 7 }` | (Struct-Intro) §5.8 | `⇒ S1` |
| `@drop(v0)` | (@Drop) §5.3 | `⇒ unit`, and `v0` becomes `MovedOut` |
| `@panic("boom")` | (Panic) §5.8 + (Sub-Never) §5.7 | `⇒ never ⊣ ⊥`: `check` returns `never`, which (Sub-Never) lets stand as the function's `i64` |
| `@drop(v0); @panic(…)` | (Seq) §5.3 | `⇒ never ⊣ ⊥`; the discarded `unit` carries no linear value |
| `let v0 = …; …` | (Let) §5.3 + §5.6 | `⇒ never ⊣ ⊥`; the body has no normal exit, so no scope-exit check is read (`explain/panic_after_drop.txt` renders `body ⇒ never, exit ⊥`) |

#### The run

| Row | Rule | Store before | Effect | Store after | Events |
| --- | --- | --- | --- | --- | --- |
| 2–3 | literal, (D-Struct) §6.5 | `[]` | the literal becomes `{ 7 }_S1` with identity `#0`, reserving `ℓ0` | `[ℓ0 = †]` | |
| 4 | (D-Let) §6.7 | `[ℓ0 = †]` | mint `ℓ1` for `v0` | `[ℓ0 = †, ℓ1 = S1 { 7 }#0]` | |
| 5 | `@drop` §6.11 | `[ℓ0 = †, ℓ1 = S1 { 7 }#0]` | the glue runs, with the destructor as its observable half, and the cell is marked `⊘` rather than retired, so the binding stays reinitializable (§6.8, §6.11) | `[ℓ0 = †, ℓ1 = ⊘]` | `drop ℓ1 = S1 { 7 }#0`; `run drop fn S1(S1 { 7 }#0)` |
| **6** | **(D-Panic) §6.12** | `[ℓ0 = †, ℓ1 = ⊘]` | the configuration is abandoned with `↯user` | | |
| 7–8 | (D-Seq), then the `let`'s (D-EndScope) | | both pass the trap on. The body did not complete, so the scope never closes and nothing unwinds σ: the dynamic face of §5.7's `⊥_panic` exemption | | |
| 9 | (Panic-Lift) §6.2 | | the trap is carried out of the suspended `main() → f0()` context: **no frame is popped**, `run-all-scope-drops` never runs, and the callee's open scopes go with the configuration | | |

The result is `EvalRes.panic .user [drop ℓ1 …, dtor S1 …]`: the trap, and the
two events that had already happened. `Corpus.outLines` projects the
observable ones, so the exported expectation is

```json
{"kind": "panic", "panic": "user", "stdout": ["7"]}
```

and `crates/rue-oracle-diff` compares *both* halves: a native binary that
trapped with the right category but lost the destructor line disagrees. The
compiled program prints `7` on stdout and `panic: boom` on stderr, and exits
101.

The drop that shows is the explicit one. The binding's *scope exit* never
happened, because row 6 abandoned the configuration. Take the `@drop` away
and the destructor never runs at all: `panicPastAffine` in `Examples.lean` is
that program, kernel-checked to an empty trace, and the compiler does the same.
A `return` in the same position would have unwound the frame and printed the
line, as in example 2.

### Example 4: `struct_nested_dtor_drop`, a struct's class and §6.11's drop order

The smallest program in which a struct's *fields* matter: a
destructor-bearing struct holding a destructor-bearing struct, dropped at
scope exit.

#### The program

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }
struct S5 { x0: i64, x1: S1 }
drop fn S5(self) { @dbg(self.x0); }

fn f0() -> i64 {
    {
        let v0: S5 = S5 { x0: 1, x1: S1 { x0: 2 } };
        9
    }
}
```

In the core the body is `letIn false (mkStruct sOuter [lit 1, resA (lit 2)])
(lit 9)`, and the declarations are `StructDecl` records that each carry their
class: `class(S1) = Affine`, `class(S5) = Affine`.

#### What the checker demands

**The recorded class.** §3 says `class(S)` is the join of the field classes,
lifted by the declared attribute. For `S5` that is
`Copy ⊔ class(S1) = Copy ⊔ Affine = Affine`, and no attribute lifts it
(`3.8:3`: structs are affine by default). `WfStructs` (`Statics.lean`) checks
every recorded class against this equation, alongside §3's other declaration
rules, and `class_unique` shows the recorded class is determined rather than
free.

**The initializers.** (Struct-Intro) §5.8 types them **in declaration
order**, threading Σ left to right, each at its declared field type. It uses
the same `TypedArgs` judgment as (Call) §5.8 does for an argument list,
because the two rules impose the same left-to-right discipline:

| Node | Rule | Concludes |
| --- | --- | --- |
| `S5 { x0: 1, x1: S1 { x0: 2 } }` | (Struct-Intro) §5.8 | `⇒ S5` |
| ⟶ `1` | (Lit) §5.8 | `⇒ i64`, at field `x0`'s declared type |
| ⟶ `S1 { x0: 2 }` | (Struct-Intro) §5.8 | `⇒ S1`, at field `x1`'s declared type |
| ⟶ ⟶ `2` | (Lit) §5.8 | `⇒ i64` |
| `let v0 = …; 9` | (Let) §5.3 + §5.6 | `⇒ i64`, and the leak check passes: `class(S5) = Affine`, not `Linear` |

The leak check is the one place the class is read: §5.6 rejects a binding
still `Owned` at a `Linear` type. `S5` is `Affine`, so scope exit may drop
it, and the machine then must.

#### The run

| Row | Rule | Store before | Effect | Store after | Events |
| --- | --- | --- | --- | --- | --- |
| 2–5 | literals, (D-Struct) §6.5 | `[]` | the inner literal becomes `{ 2 }_S1` with identity `#0`, then the outer `{ 1, { 2 }_S1 }_S5` with `#1`, a redex only once **all** its components are values; each identity reserves a `†` slot | `[ℓ0 = †, ℓ1 = †]` | |
| 6 | (D-Let) §6.7 | `[ℓ0 = †, ℓ1 = †]` | mint `ℓ2` for `v0` | `[…, ℓ2 = S5 { 1, S1 { 2 }#0 }#1]` | |
| 7 | literal | | the body's `9` | | |
| **8** | **(D-EndScope) §6.7 → `drop-retire` → §6.11** | `[…, ℓ2 = S5 { 1, S1 { 2 }#0 }#1]` | the cell holds a live non-`Linear` value, so the monitor lets it through and §6.11's walk runs: **`S5`'s destructor first**, then the fields in **declaration order**: `x0` is an `int` and drops nothing, `x1` is an `S1` and runs *its* destructor | `[…, ℓ2 = †]` | `drop ℓ2 = S5 { 1, S1 { 2 }#0 }#1`; `run drop fn S5(…#1)`; `run drop fn S1(S1 { 2 }#0)` |
| 9 | (D-Return-Value) §6.9 | | the frame pops with an empty record | | |

So the program prints `1` (the outer destructor), then `2` (the inner), then
its value `9`: the bridge expectation
`{"kind": "ok", "stdout": ["1", "2", "9"], "exit": 0}`.

#### What the proof needs

Three claims in row 8 are theorems rather than observations:

- `dropContents_struct_events` is §6.11's order in closed form: dropping a
  well-typed struct emits its destructor's event, when its declaration has
  one (`3.9:28`), followed by its fields' events concatenated in declaration
  order (`3.9:13`), each field's given by the same form recursively. A
  moved-out field (`⊘`) contributes none.
- `dropContents_ok` says the walk never refuses on well-typed contents.
- `StructDecl.Wf.field_not_linear` says a declaration whose class is not
  `Linear` has no `Linear` field. That is why the leak monitor at row 8 can
  look at the value's own class and never inside it.

### Example 5: `partial_move_residue`, a partial move, drawn

A partial move is where both sides of the invariant stop being flat, so this
example draws them.

#### The program

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }     // the observation channel
struct S7 { x0: S1, x1: S1 }             // no destructor of its own

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

`v0.x0` is a place, `Place.proj (Place.var 0) 0` in the core, and using it in
value context moves *exactly* that field.

#### What the checker demands

(Use-Move) §5.1, read by `check` as in section 1: `en.st.get p.path` finds
the state for `v0.x0` (and returns `none`, a rejection, when a *proper prefix*
is `MovedOut`); `en.ty.atPath` types the place; `u.fullyOwned` is `3.8:26`;
`noDtorPrefix` is `3.9:34`. Before the move, Σ and the cell agree on a
hole-free tree (`ContentsMatches`'s `owned` clause):

```
  Σ(v0)                      H(ℓ3)
  Owned                      S7 { S1 { 1 }#0, S1 { 2 }#1 }#2
```

(`ℓ0`–`ℓ2` are the `†` slots the three literals reserved for their
identities, `#0`–`#2`; section 2.) (Use-Move) marks exactly `v0.x0` and removes
every path under it, and (D-Use-Move) §6.3 writes `H[ℓ3@[0] ↦ ⊘]` at the same
position:

```
  Σ(v0)                           H(ℓ3)
  Owned{ x0: MovedOut }           S7 {
    ├─ x0  MovedOut   ← v0.x0         ⊘,
    └─ x1  Owned      ← v0.x1         S1 { 2 }#1
                                  }#2
                                  H(ℓ4) = S1 { 1 }#0    ← v1, the moved value
```

The moved value keeps its identity, `#0`: it changed owners, not identity.

`Owned{ x0: MovedOut }` is how the explainer writes `OwnSt.fields [.movedOut,
.owned]`: a node that still owns its storage, with one field taken out from
under it. A slot the brace leaves out is `Owned`.

Three things follow, and each is a premise somewhere:

- `Σ(v0)` is still `Owned` (the node is a field record, not `MovedOut`), so
  `v0.x1` is readable (`3.8:53`; the `copy_through_partial` case) and
  `@drop(v0)` is legal (§5.3 asks only `Σ(p) = Owned`; the
  `drop_field_then_whole` case);
- `fully-owned(Σ, v0)` is now **false**, so `let v2 = v0` has no derivation:
  the aggregate has a hole and (Use-Move) may not hand it to a new owner
  (`3.8:26`; the compiler's E0205, the `partial_then_whole` case);
- had `S7` declared a destructor, the move would have been rejected
  (`3.9:34`, E0456; the `partial_under_dtor` case), because a destructor runs
  on the whole value and would meet the `⊘`.

#### The run

`v1`'s scope ends first. `@drop(v1)` already marked it, so its `endscope`
drops nothing. Then `v0`'s scope ends, and §6.11's walk runs on the *cell
contents*, skipping every `⊘`:

```
  drop(ℓ3) = drop(S7 { ⊘, S1 { 2 }#1 }#2)
           = (S7 declares no destructor)
             drop(⊘)  ++  drop(S1 { 2 }#1)
           = []       ++  [dtor S1 (S1 { 2 }#1)]
```

So the output is `1` (from `@drop(v1)`), `2` (from the residue), then `9`,
and the moved field is dropped **once**, by its new owner: the trace's
destructors name `#0` and `#1`, once each, which is what `no_double_free`
counts. That skip is §7's
double-free argument, and it is `dropContents_struct_events` from example 4.
By the chain in the section 5 introduction, `run` cannot reach the
`useAfterMove` a second drop of `S1 { 1 }` would be.

### Example 6: `destructure_residue_order`, a declared-linear destructure

Here a use of a *field* consumes something other than that field.

#### The program

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }        // the observation channel
linear struct S15 { x0: S1, x1: i64, x2: S1 }

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

`v0.x1` is an `i64`, a `Copy` place. An ordinary use would copy it and change
nothing. This one does neither, and §4.2 says why: "a declared-linear
destructure plan is the central override; it consumes the selected enclosing
place even when `T` is `Copy`".

#### What the checker demands

**The plan.** Elaboration computes §4.2's `dl(Γ, p)` from the root's declared
type and the path, and `declaredPrefix` (`Syntax.lean`) is that function:

```
  declaredPrefix D S15 [1]
    = some ([], [1])          -- π_d = ε,  π_s = [x1]
```

`π_d` is the **longest proper prefix** whose type is a struct declared
`linear`. Here that is the empty path, so the consumed place `d` is `v0`
itself. Where the chain runs deeper the answer is the innermost one:
`declaredPrefix D S13 [0, 0]` is `some ([0], [0])`, so `y.x0.x0` consumes
`y.x0` and leaves `y` alone (`3.8:33`, the `destructure_two_levels` case).
Where no prefix carries the attribute the answer is `none`: §5.1's
`Ordinary` plan.

**The residue gate.** Before anything is destroyed, §5.1 asks
`¬ linear-residue(S, π_s)`:

```
  residue(S15, [1])          = [ x0 : S1,  x2 : S1 ]      -- declaration order
  linear-residue(S15, [1])   = false                       -- neither is Linear
```

`linearResidue` (`Syntax.lean`) computes it on the **types**: every
unselected member is retained, and the selected one is recursed into, at any
depth. At an **array** step the members are the elements in ascending index
order. Had `x2` been declared `linear`, the access itself would be the error:
`3.8:60` and the compiler's E0474 (`destructure_linear_residue`).

**The rest of `check`.** `en.st.get π_d` and `u.fullyOwned` are
`fully-owned(Σ, d)` at the *consumed* place (`3.8:26`); `noDtorPrefix` over
the **whole** path is `3.9:34` read at "every enclosing value, including `d`"
(E0456, `destructure_under_dtor`); and `en.ty.atPath … p.path` is the leaf's
type, which the rule concludes at. The rule's Σ effect is (Use-Move)'s, taken
at `d` rather than at `p`: `Σ(v0)` becomes `MovedOut`. Nothing marks `v0.x1`,
because `v0.x1` is not what was consumed.

#### The run

§6.3 runs `split` and then `drop*`, and the order is the traversal's:

```
  split(S15 { S1 { 1 }#0, 5, S1 { 2 }#1 }#2, [1]) = ( 5 , [ S1 { 1 }#0, S1 { 2 }#1 ] )
  drop*(H, [ S1 { 1 }#0, S1 { 2 }#1 ])            = [ drop ℓ3 = S1 { 1 }#0, dtor S1 (S1 { 1 }#0)
                                                    , drop ℓ3 = S1 { 2 }#1, dtor S1 (S1 { 2 }#1) ]
  consume S15 { ⊘, ⊘, ⊘ }#2
  then H[ℓ3 ↦ ⊘]
```

(`v0` is at `ℓ3` because the three literals reserved `ℓ0`–`ℓ2` for their
identities; section 2.) Each retained subtree is a sub-position of `ℓ3`
being dropped, so its drop starts with a `drop ℓ3` marker, exactly as
`@drop(v0.x0)` would record it. The consumed aggregate's own identity, `#2`,
runs no drop of its own — everything it held has been handed on or dropped —
and the `consume` event records that its life ends here, with every member
`⊘` (RUE-2427). The trace's markers end `#0`, `#1` and `#2` once each (the
`dtor` events name `#0` and `#1` too, as the destructors that ran), and no
marker ends any of them again.

`MovedOut` on the Σ side and `⊘` on the store side, at the one path `π_d`,
is `ContentsMatches` again. The output is `10`, `1`, `2`, `20`, then the
value `5`: the residue drops **at the access** rather than at scope exit, and
`explain/destructure_residue_order.txt` shows both drops and the
consumption on its one (D-Use-Declared-Linear) §6.3 row. Where the selected path passes through a
nested struct, the nested residue comes before the later sibling
(`destructure_nested_residue`); where the form is `@drop` rather than a use,
§6.11 drops the selected leaf *after* the residue
(`drop_declared_residue_first`).

#### What the proof needs

The case is `useDeclared` in `soundness`, and it rests on three lemmas:

- `ContentsMatches.declaredPlan_eq`: the plan the machine reads off the store
  is the plan the rule selected;
- `splitResidue_ok`: `split` never fails on a hole-free, well-typed
  aggregate, and every retained subtree is non-`Linear`;
- `dropResidue_events`: the residue's trace is §6.11's events concatenated in
  the traversal's order.

### Example 7: `array_elem_move_rest_ascending`, an element move and the rest dropped in index order

Arrays add a place whose step is an **index**, and a residue that §6.11 walks
by position rather than by field.

#### The program

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }     // the observation channel

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

In the core, `v0[1]` is `Place.idx (Place.var 0) 1` (`Examples.arrayElemMove`),
and its path is `[1]`. Nothing that navigates by a path tells an index step
from a field step: `Ty.fieldAt` reads the step as a field slot at a struct
type and as a constant index at an array type. So the element move goes
through **the same rules** as the partial move of example 5.

#### What the checker demands

(Use-Move) §5.1 with the premises of section 1. The one the array adds is
`rootIdxOnly`, `3.8:68`: an element move is tracked only where the index is
applied directly to the binding. `v0[1]` passes. `h.arr[1]` (an element of an
array reached through a field) and `a[1][0]` (an element of an element) fail
it, and the compiler reports both as E0904
(`Examples.arrayElemMoveThroughField`, `arrayElemMoveNestedIndex`).

After the move, Σ and the cell look like example 5's, one index down:

```
  Σ(v0)                                H(ℓ4)
  Owned{ x0: Owned, x1: MovedOut }     [
    ├─ [0]  Owned      ← v0[0]           S1 { 1 }#0,
    ├─ [1]  MovedOut   ← v0[1]           ⊘,
    └─ [2]  Owned      ← v0[2]           S1 { 3 }#2
                                       ]#3
                                       H(ℓ5) = S1 { 2 }#1    ← v1, the moved element
```

(`ℓ0`–`ℓ3` are the four identities' reserved slots: three elements and the
array; section 2.)

The explainer names positions the way it names fields: `x1` is index `1`.
This is `3.8:73`'s per-element drop flag, and it is the whole of it: there is
no separate flag, only the path's `MovedOut`. Two consequences are the
array's own:

- an access at a **dynamic** index could name the hole, so it is refused
  (`3.8:70`; E0205 for a read, E0480 for a write,
  `Examples.arrayDynWriteAfterElemMove`);
- a write into the array, `v0[1] = S1 { 9 }`, is refused whether it targets
  the hole or a sibling (`3.8:72`, `7.1:46`, E0480;
  `Examples.arrayElemReinit`): an element write does not give back
  per-element ownership, and the recovery is the whole-array assignment
  (`Examples.arrayWholeReinit`).

#### The run

`@drop(v1)` runs `S1 { 2 }`'s destructor where it stands: `2`. Then
`@dbg(20)` prints `20`. Then `v1`'s scope ends (already `MovedOut`), then
`v0`'s, and §6.11's walk runs on the **cell contents**, elements in ascending
index order, skipping every `⊘`:

```
  drop(ℓ4) = drop([S1 { 1 }#0, ⊘, S1 { 3 }#2]#3)
           = (an array has no `drop fn` of its own; dropping it drops its
              elements in index order, 3.9:14–15)
             drop(S1 { 1 }#0)  ++  drop(⊘)  ++  drop(S1 { 3 }#2)
           = [dtor S1 (S1 { 1 }#0)] ++ [] ++ [dtor S1 (S1 { 3 }#2)]
```

So stdout is `2`, `20`, `1`, `3`, then `main`'s `7`. The pinned trace beside
`arrayElemMove` in `Examples.lean` is this, event for event, with `.hole` for
the `⊘`:

```lean
[.drop 5 (cA 1 2), .dtor sAffine (cA 1 2), .dbg (v64 20),
 .drop 4 (.array (.struct sAffine) 3 [cA 0 1, .hole, cA 2 3]),
 .dtor sAffine (cA 0 1), .dtor sAffine (cA 2 3)]
```

`cA i n` is `S1 { n }` with identity `i`.

#### What the proof needs

`dropContents_array_events` (`Soundness.lean`) is the walk in closed form: no
event of the array's own, then each element's events, concatenated in index
order.

### Example 8: `enum_match_affine`, a `match` and the two drops it does not do

With enums, the thing to watch is not the branch (that is `if` again) but the
*payload*.

#### The program

```rue
struct S1 { x0: i64 }
drop fn S1(self) { @dbg(self.x0); }     // the observation channel
enum E0 { K0(S1), K1 }                   // class(E0) = Affine, through S1

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

`class(E0)` is the join over **every** payload component of **every** variant
(`6.3:19`), because the active variant is a run-time fact §3 cannot read.
Here that is `class(S1) = Affine`, so a use of an `E0` place is a move.

The program prints `10`, `1`, `20`, then the value `5`. The `1` is `S1`'s
destructor, and **where** it falls is the whole example: at the *arm's* end,
not at `v0`'s scope exit and not twice.

#### What the checker demands

(Match) §5.5 has four premises, and `check` reads each one:

- **The scrutinee is typed first**, and the arms start from the Σ it leaves.
  `class(E0)` is not `Copy`, so `v0` is typed by (Use-Move) §5.1: the `match`
  **consumes** it, because a scrutinee is a value context (`3.8:7`, `3.8:76`,
  `6.3:17`).
- **Exhaustiveness** is `arms.length = ed.variants.length`, with arm `j` the
  arm for variant `j`. There is no coverage search and no ordering side
  condition, because the core form has no wildcards and no guards; those are
  elaboration obligations (`4.7:9`, `4.7:10`). `exhaustive_arm_exists` is the
  one line progress needs.
- **Every arm is typed from the same post-scrutinee state** (`checkArms`),
  under its own variant's payload locals (`armCtx`), all at the type
  `firstArmTy` fixed. An arm is a branch, so Σ is not threaded from arm to
  arm.
- **At the arm's end** the payload locals leave scope under §5.6
  (`NoResidualLinear` over the entries the arm pops), and what is left is that
  arm's contribution to `Σ' = join(Σ1, …, Σn)` (`Ctx.joinAll`).

Σ at the four points that matter:

```
  before the match         [v0: E0 = Owned]
  Σ0, after the scrutinee  [v0: E0 = MovedOut]          ← (Use-Move) §5.1
  inside arm K0            [v1: S1 = Owned, v0: E0 = MovedOut]
  inside arm K1            [v0: E0 = MovedOut]           ← no payload to bind
```

The payload local enters `Owned` and **unmarked**: §2 gives a pattern binding
no `μ`, and the compiler's parser rejects `mut` there. Arm `K0` pops its one
local, arm `K1` pops none, and both leave `[v0: E0 = MovedOut]`, so the join
is that too.

`join(Σ1, …, Σn)` is unordered in the calculus. `Ctx.joinAll` is a **left
fold** of the binary join over the arms in declaration order, starting from
the first arm's state. The binary join is proved commutative
(`OwnSt.join_comm`) and associative (`OwnSt.join_assoc`) over well-formed
states, so `Ctx.joinAll_perm` says the fold's order does not matter. Its
well-formedness hypothesis is one every derivation from a well-formed context
meets (`Typed.wf`): a diverging arm, which could once conclude at any context,
contributes no state at all.

Change one thing and each premise answers in turn:

- make the payload `linear` and leave it, and the §5.6 check at the arm's end
  is the leak (`enum_arm_leaks_payload`, E0406);
- consume the enum in one arm of an `if` only, and the join has `MovedOut`
  against `Owned` at a `Linear` type (`enum_match_one_arm`, E0443); at an
  `Affine` payload the same join is `MovedOut` and accepted
  (`enum_match_one_arm_affine`);
- match it twice, and the second scrutinee is a use of a moved-out place
  (`enum_matched_twice_moving`, E0205);
- make the scrutinee a field of a struct, and the move is `3.8:22`'s partial
  one, whose sibling still drops at scope exit (`enum_match_projection`,
  `enum_holder_partial_then_drop`).

#### The run

```
  [3]  (D-Struct) §6.5     mint #0, reserving ℓ0
  [4]  (D-Enum-Intro) §6.6 mint #1, reserving ℓ1
  [5]  (D-Let) §6.7        let v0 = E0.K0⟨S1 { 1 }#0⟩#1 at ℓ2
                           store  [ℓ0 = †, ℓ1 = †, ℓ2 = E0.K0⟨S1 { 1 }#0⟩#1]
  [6]  (D-Use-Move) §6.3   v0
                           store  [ℓ0 = †, ℓ1 = †, ℓ2 = ⊘]          ← the scrutinee moved out
  [7]  (D-Match) §6.6      bind E0.K0's payload to [ℓ3]
                           store  [ℓ0 = †, ℓ1 = †, ℓ2 = ⊘, ℓ3 = S1 { 1 }#0]
                           events >> consume E0.K0⟨⊘⟩#1
  [9]  (Dbg)               @dbg prints 10
  [12] (D-EndScope) §6.6   endscope([ℓ3])
                           events >> drop ℓ3 = S1 { 1 }#0; run drop fn S1(S1 { 1 }#0)
  [15] (Dbg)               @dbg prints 20
  [19] (D-EndScope) §6.7   endscope([ℓ2])                          ← ℓ2 is ⊘: nothing drops
```

The payload keeps its identity, `#0`, as it moves from the enum into the
arm's cell, and the enum's own identity, `#1`, is consumed by the match and
never dropped: the `consume` event on row [7] records the shell's end, every
payload slot `⊘` (RUE-2427), and the arm's cell is the payload's one owner.

Row [7] is (D-Match): the tag `K0` selects the covering arm, and the payload
is bound to **fresh cells**, appended to the innermost scope record *and*
owed to an `endscope` marker around the arm's body, exactly as (D-Let) binds
one. That is why row [12] falls where it does: the drops run when the arm's
body becomes a value (`6.3:17`), not at a later frame pop. It is also why an
unwinding `return` inside an arm still finds them in σ
(`enum_return_past_payload`).

**The two drops that do not happen.** Row [19] drops **nothing**, and that
is what keeps the payload from being dropped twice:

- §6.11's enum case recurses into the **active** variant's payload only
  (`6.3:20`). An inactive variant's payload has no storage, and a
  discriminant-only variant drops nothing (`enum_drop_unmatched` holds one
  of each).
- A payload a `match` already moved out left the enum place `⊘`, and the
  walk skips every `⊘`. So `S1 { 1 }` is destroyed exactly once, by the owner
  the arm gave it.

§3 gives an enum no `drop fn`, so the payload's destructor is the entire
observation channel at an enum drop.

### Example 9: `array_dyn_write_rhs_first`, the right-hand side before the index

The smallest program in which the order of an assignment's operands is
observable. `5.2:14` is normative: "the right-hand side `expression` is
evaluated first … any index subexpressions appearing in the target … are
evaluated after the right-hand side, in source order", and §6.2's
`assign p = E` context says the same.

#### The program

```lean
letIn true (aiPair 1 2 3 4)
  (seq (indexWrite (.var 0) [call 1 [lit 1]] [[0]] (call 2 [lit 9]))
    (seq (dbg (lit 20)) (lit 7)))
```

`f1` is `id`, which prints its argument and returns it; `f2` is `mk`, which
prints its argument and builds an `S1` of it. `indexWrite p idx πs e` is the
place `p`, then one dynamic step per index in `idx`, each followed by the
constant path at the same position of `πs`: here one dynamic step, then
`.x0`. With `S8 { x0: S1, x1: i64 }`, the body prints as

```rue
let mut v0: [S8; 2] = [S8 { x0: S1 { x0: 1 }, x1: 2 }, S8 { x0: S1 { x0: 3 }, x1: 4 }];
{ v0[{ let t3i0: i64 = f1(1); t3i0 }].x0 = f2(9); };
```

Each index is printed as a typed block in place, so the printed statement
keeps the surface's own order.

#### What the checker demands

`Typed.indexWrite` types the right-hand side **first**, at the leaf type
`S1`, and threads Σ from it into the index list (`TypedArgs` at integer
types). Then it reads the array `v0` on the post-operand state: `fully-owned`
there, and `assignArrayOk` above it (nothing to check, since `v0` is the
root). The last premise is (Assign) §5.2's
`Σ1(p) = MovedOut ∨ ¬carries_linear(T)` at the leaf. A place under a
run-time index is never `MovedOut`, so this is `class(S1) ≠ Linear`.

#### The run

| What happens, in order | Events |
| --- | --- |
| the right-hand side `mk(9)` runs and builds `S1 { 9 }#7` | `@dbg 9` |
| the index `id(1)` runs | `@dbg 1` |
| `dynPlace` resolves `v0[1].x0` to the constant path `[1, 0]`, bounds-checking `1 < 2` | |
| §6.8's overwrite-drop of the old leaf `S1 { 3 }#2` | `drop`, `dtor S1 { 3 }#2` |
| the store writes `S1 { 9 }#7` at `ℓ5`'s path `[1, 0]` (`v0` is at `ℓ5`: the five literals reserved `ℓ0`–`ℓ4`) | |
| `@dbg(20)`, then the scope exit drops `v0`, elements ascending | `20`; `1`, `9` |

So stdout is `9 1 3 20 1 9 7`, and the compiler prints the same.

Had the index been out of range, the bounds check would trap, and the
`S1 { 9 }` already built would never be dropped: a trap runs no drops
(§6.12), and the compiler does the same (`array_dyn_write_trap_negative`).
Had the index `return`ed instead, the value would be lost the same way: the
pending-value edge of section 2.

### Example 10: `float_to_int_trap_inf`, the one float trap and where the float assumption sits

Floats are in the core (§2, §5.8, §6.4), and they are the one construct whose
IEEE side the mechanization *assumes* rather than proves. This case shows the
boundary because it reaches the only trap a float program can reach.

#### The program

```lean
fintrin (.floatToInt .w32 .signed) (binop .div (flE .w64 1 0) (flE .w64 0 0))
```

(`Examples.floatToIntTrapInf` writes the operand as `posInf .w64`, which is
this division.) Printed:

```rue
fn f0() -> i32 {
    { let t1c: f64 = (1.0 / 0.0); let t1k: i32 = @float_to_int(t1c); t1k }
}
```

The two binders are the printer supplying types that nothing downstream
names: `@float_to_int` takes its result type from the *use* site (`3.12:17`)
and fixes nothing about its operand (`Print.lean`, "Integer typing"), and a
float literal would otherwise default to `f64` (`3.12:8`).

#### What the checker demands

| Node | Rule | Concludes |
| --- | --- | --- |
| `1.0`, `0.0` | (Lit) §5.8 at `float(64)` | `⇒ f64`. `3.12:9` *rounds*, so an inexact decimal like `0.1` denotes the nearest `f64` rather than being rejected. The one premise is `3.12:10`, which refuses a literal whose value rounds to an **infinity** at the width (E0206): `RueCore.FloatLit.RoundsFinite`, an exact comparison against `max_{𝔽_w}` plus half an ulp. An underflow to zero is legal |
| `1.0 / 0.0` | (Float-Arith) §5.8 | `⇒ f64`. One `w` for both operands, because `3.12:13` gives no implicit widening. `BinOp.floatAdmits` is §5.8's "rejected by the absence of a rule" for `%` and the bitwise operators, written as a side condition because one constructor stands for (Float-Arith), (Float-Ord) and (Total-Cmp) |
| `@float_to_int(…)` | (Float-To-Int) §5.8 | `⇒ i32`. Whether the value *survives* is dynamic, not a typing question (`3.12:18`) |

#### The run

| Row | Rule | Effect |
| --- | --- | --- |
| 2–3 | (Lit) §6.3 | each literal becomes `M.ofLit`'s datum: `3.12:9`'s rounding, which is the **model's**, not this module's |
| 4 | **(D-Float-Arith) §6.4** | `1.0 / 0.0 → +inf`. Not a trap: none of (D-Arith-Trap), (D-Div-Zero) or (D-Div-Overflow) is stated over a float redex, and `3.12:22` fixes the answer: a finite non-zero over a zero is the infinity of the xor sign |
| 5 | **(D-Float-To-Int-Trap) §6.4** | `+inf` is neither truncatable nor in range, so the conversion traps. The category is `↯overflow`, the one §6.12 already lists (`8.1:7`), which is why the compiler reports `integer overflow` here and not a float-specific message |

The exported expectation is
`{"kind": "panic", "panic": "overflow", "stdout": []}`, and the compiled
program exits 101 with `error: integer overflow`.

#### What the proof needs

Row 5 is a **theorem**. `floatToInt_partition` (`RueCore/Float.lean`) says
the premises of (D-Float-To-Int) and (D-Float-To-Int-Trap) partition `𝔽_w`,
which is what §7 asks for and what keeps progress intact. It is provable
because §2 models a float as a *datum*, so truncation toward zero is exact
integer arithmetic.

Row 4 is an **assumption**: `FloatModel.div_by_zero`, `3.12:22` as §6.4
quotes it. `Examples.floatDivZeroToInt_traps` is the two rows together,
stated over an *arbitrary* `FloatModel` and proved from its laws. So the
witness is a claim about IEEE 754 rather than about this package's instance,
and it computes no float at all. That is also why it costs no axiom: Lean's
own `Float` is defined over an `opaque` constant, and a theorem that so much
as mentions one reports `Classical.choice`.

The laws are **structure fields**, not `axiom` declarations, so a theorem
that rests on one says so in its own statement (the `M : FloatModel`
argument of section 4's theorems), and `TRUST.md` lists them in a section of
their own. `Float.exactOps`, the instance the corpus and the examples run, is
constructive integer arithmetic. That it *satisfies* the laws is the residual
assumption, and it is checked by running the float corpus against the
compiler rather than proved.

### Example 11: `loop_move_every_path_breaks` and `loop_linear_one_exit`, a loop's head state and its exits

A loop is where one typing of the body has to speak for every turn. Two
corpus cases show the two things the loop rules compute to make that true:
the state at the loop head, which is RUE-1615's shape, and the state after
the loop, which is RUE-1614's.

#### The program

```rue
linear struct S3 { x0: i64 }
drop fn S3(self) { @dbg(self.x0); }     // the observation channel

// loop_move_every_path_breaks: accepted
fn f0() -> i64 {
    {
        let v0: S3 = S3 { x0: 1 };
        { loop { { @drop(v0); break } }; 5 }
    }
}

// loop_linear_one_exit: rejected (E0443)
fn f0() -> i64 {
    {
        let v0: S3 = S3 { x0: 1 };
        { loop { if false { { @drop(v0); break } } else { break } }; 0 }
    }
}
```

In the core they are `Examples.loopMoveEveryPathBreaks` and
`Examples.loopLinearOneExit`: `loop e` and the nullary `brk`. Both loops
consume the linear `v0` inside the body. The first always does it just before
leaving. The second does it on one way out and not on the other.

#### What the checker demands

(Loop-Break) §5.7, `Typed.loopBreak` in Lean, types a loop in three steps, and
the `break`-less (Loop-Div) forms share the first two:

1. **Find the loop-head state** `Σ_h`: what is true at the top of *every*
   turn. It is the entry state joined (§5.5) with the state at every back
   edge, the end of a turn that goes round again. `Σ_h` depends on the body,
   and the body is typed at `Σ_h`, so the rule takes `Σ_h` as a premise
   (`LoopHead`), and `check` finds it by iterating from the entry state until
   it stops changing (`headIter`).
2. **Type the body once, at `Σ_h`.** Its outgoing result says where the turn
   ends: a normal state is the back edge, and each `⟨break, Σ_x⟩` delivery is
   an exit (§5.3's `Ω`).
3. **Read the exits.** At each exit the bindings the body opened are
   discharged, since their scopes end there (`Ctx.loopLocals`, checked by
   `NoResidualLinear`). The rest is joined over every exit
   (`Ctx.outsideLoop`, `3.8:80`), and that join is the state after the loop.

For `loop_move_every_path_breaks`, in the notation `explain/` renders:

```
  entry             Σ   = [v0: S3 = Owned]
  body at Σ         { @drop(v0); break }
                      ⇒ never ⊣ ⊥; Δ = {⟨break, [v0: S3 = MovedOut]⟩}
  back edges        none: the body's result is ⊥, so no turn goes round
  loop head         Σ_h = Σ = [v0: S3 = Owned]
  exits             exit 0: [v0: S3 = MovedOut]
  after the loop    [v0: S3 = MovedOut]
```

The `@drop` is checked at `Σ_h`, where `v0` is `Owned`, so it is legal, and
the `let`'s scope exit then finds `v0` `MovedOut` and owes nothing. The
compiler used to reject this program as "moved in a previous iteration"
(RUE-1615). A previous iteration is exactly what the head state describes,
and here no turn reaches the back edge, so there is no previous iteration
whose move could matter.

Give the body a way round and the same move is refused.
`loop_moved_prev_iteration` is `loop { @drop(v0) }` on an affine `S1`, a
`break`-less loop, so (Loop-Div-Backedge) rather than (Loop-Break) — but the
head state is found the same way:

```
  entry             Σ   = [v0: S1 = Owned]
  iteration 1       body at Σ reaches the back edge with [v0: S1 = MovedOut]
                    join(Σ, back edge) = [v0: S1 = MovedOut]      changed
  iteration 2       body at [v0: S1 = MovedOut]: @drop(v0) refused
  loop head         none: E0205 "moved in a previous iteration" (3.8:79)
```

For `loop_linear_one_exit`, the body has two exits and still no back edge:

```
  entry             Σ   = [v0: S3 = Owned]
  body at Σ         if false { @drop(v0); break } else { break }
                      ⇒ never ⊣ ⊥; Δ = {⟨break, [v0: S3 = MovedOut]⟩, ⟨break, [v0: S3 = Owned]⟩}
  loop head         Σ_h = Σ
  exits             exit 0: [v0: S3 = MovedOut]
                    exit 1: [v0: S3 = Owned]
  after the loop    join undefined ✗
```

The join is undefined because `S3` is linear and the exits disagree on `v0`
(`3.8:50`). The explainer names it: "the reachable exits disagree on a
linear-carrying binding — v0: S3 is MovedOut in exit 0 and Owned in exit 1",
and the compiler reports E0443. This is RUE-1614's rule, that a linear value
must be consumed on every way out, applied to `break` edges instead of
`return` edges. With an affine `S1` in place of `S3` the join is defined and
gives `MovedOut` ("maybe moved"): `loop_two_exits` is that case, and it is
accepted.

`loop_reassign_then_move` is the seed where the head is not the entry state.
Each turn assigns `d` and then drops it; in the printed program `d` is `v0` and
the counter `n` is `v1`:

```
  entry             Σ   = [v1: i64 mut = Owned, v0: S1 mut = Owned]
  iteration 1       back edge [v1 = Owned, v0 = MovedOut]
                    join = [v1: i64 mut = Owned, v0: S1 mut = MovedOut]     changed
  iteration 2       body at that state: (Assign) reinitializes v0 before the @drop
                    back edge [v1 = Owned, v0 = MovedOut]; join unchanged
  loop head         Σ_h = [v1: i64 mut = Owned, v0: S1 mut = MovedOut]
```

`Examples.lean` kernel-checks both steps of that iteration: a bound of one
refuses, and a bound of two reaches this head.

#### The run

`loop_move_every_path_breaks`, rows as `explain/loop_move_every_path_breaks.txt`
numbers them:

| Row | Rule | Store before | Effect | Store after | Events |
| --- | --- | --- | --- | --- | --- |
| 3 | (D-Struct) §6.5 | `[]` | the literal mints identity `#0`, reserving `ℓ0` | `[ℓ0 = †]` | |
| 4 | (D-Let) §6.7 | `[ℓ0 = †]` | mint `ℓ1` for `v0` | `[ℓ0 = †, ℓ1 = S3 { 1 }#0]` | |
| 5 | `@drop` §6.11 | `[…, ℓ1 = S3 { 1 }#0]` | the glue runs, and the cell is marked `⊘` | `[…, ℓ1 = ⊘]` | `drop ℓ1 = S3 { 1 }#0`; `run drop fn S3(S3 { 1 }#0)` |
| 6 | (D-Break) §6.10 | `[…, ℓ1 = ⊘]` | the `break` fires (`EvalRes.broke`) | `[…, ℓ1 = ⊘]` | |
| 7 | (D-Seq) §6.7 | `[…, ℓ1 = S3 { 1 }#0]` | the sequence `{ @drop(v0); break }` passes the `break` on | `[…, ℓ1 = ⊘]` | |
| **8** | **(D-Break) §6.10 (unwind to the loop)** | `[…, ℓ1 = ⊘]` | the loop catches it and drops the cells the body opened, newest first. The body opened none, so `unwind-drops([])` does nothing, and the loop's value is `()` | `[…, ℓ1 = ⊘]` | |
| 9 | literal §6.3 | `[…, ℓ1 = ⊘]` | the value `5` | `[…, ℓ1 = ⊘]` | |
| 10 | (D-Seq) §6.7 | `[…, ℓ1 = S3 { 1 }#0]` | the sequence `{ loop { … }; 5 }` ends with `5` | `[…, ℓ1 = ⊘]` | |
| 11 | (D-EndScope) §6.7 | `[…, ℓ1 = ⊘]` | `v0`'s cell is already `⊘`, so its scope exit drops nothing | `[ℓ0 = †, ℓ1 = †]` | |

A row for a compound form (rows 7 and 10) records the store at the form's
start, before its parts ran, which is why its "before" still shows `S3 { 1 }`.
The output is `1`, then `5`. A body that had opened a binding would see it
dropped at row 8 instead of at its own scope's end (`loop_break_past_local`).

`loop_linear_one_exit` takes exit 1: `false` sends it to the `else` arm, whose
`break` leaves `v0` alone. Then the `let`'s scope exit (row 11) meets a live
linear value, and the machine refuses with `linearLeak`. The bridge never
sees that run, because the compiler rejects the program first.

#### What the proof needs

Three things, in `Statics.lean` and `Soundness.lean`:

- **The back edge.** A turn that completes re-enters the loop at its head, and
  the same body derivation must type it there. `LoopHead.reenter` says it
  does: the join absorbs a second copy of a back-edge state
  (`Ctx.join_absorb`), so joining the head with the new back edge gives the
  head again. `soundness`'s fuel induction then takes every later turn.
  `LoopHead.enter` and `LoopHead.backEdge` carry the store's agreement onto the
  head, from the entry and from the back edge.
- **The exits.** `loop_exit_ok` carries the store's agreement across a
  `break`: the unwind drops exactly the cells `Ctx.loopLocals` names, and the
  exit's state joins into the loop's outgoing one. The machine keeps the
  state of the path it took, and the join is the "maybe" over all paths, which
  is `3.8:60`'s asymmetry again.
- **Nontermination.** A loop may never finish. Each turn spends fuel, so a
  run that has not finished is `outOfFuel`, never a wrong answer
  (`infiniteLoop_outOfFuel` proves an infinite loop exhausts every bound), and
  the export leaves such a case out (`Corpus.lean`).

#### Before and after: what the loop-head rewrite fixed

The calculus's changelog in §5.7 (`01-core-calculus.md`, "Rewritten into
judgment form") lists three verdicts the rewrite (#3181, RUE-2321) changed.
Here they are in plain terms.

**Before**, the two loop rules differed. The `break`-exited rule,
(Loop-Break), typed the body from the entry state and then *checked* each back
edge: the state at the end of a turn that goes round again had to **equal**
the state at entry (`3.8:79` read literally, "invariant across every reachable
back edge"). The `break`-less rule, (Loop-Div-Backedge), had no such check at
all.

1. **A loop that moves an outer value and never exits was accepted.**
   `loop { @drop(v0); }` with no `break` fell under (Loop-Div-Backedge), which
   typed the body once, from the entry state, where `v0` is `Owned`, and never
   asked about the second turn. The second turn drops a value the first one
   already dropped. That was the old calculus's unsound verdict. The compiler
   always rejected it (E0205, "moved in a previous iteration"), and so does the
   new rule: `loop_moved_prev_iteration` above is this program.
2. **Reassign-then-move was rejected, though it is safe.** In

   ```rue
   loop { if n > 1 { break; } d = S1 { x0: n + 10 }; @drop(d); n = n + 1; }
   ```

   every turn gives `d` a new value before dropping it. But each turn *ends*
   with `d` moved and began with `d` owned, so the equality failed. The
   opposite order, move-then-reassign (`@drop(d); d = …;`), passed the old
   check, because that turn ends with `d` owned again. The compiler accepts
   both on purpose (`reassign_before_move_ok`); the old calculus was too strict
   about the first.
3. **An exit on a later turn.** Because every back edge had to equal the
   entry, a later turn always started where the first did, and reading the
   exits from the entry state was right *under the old rule*. The gap was the
   compiler's: it relaxed the equality to admit reassign-then-move, but kept
   reading exits from its first pass, so a `break` after an earlier turn had
   moved something was read as if nothing had moved. That was unsound, and
   RUE-2354 fixed the compiler.

**After**, both rules type the body once at the **loop-head state**: the entry
joined with every back-edge state, which is what is true at the top of *every*
turn. In case 1 the head has `v0` `MovedOut`, so the `@drop` is refused. In
case 2 the head has `d` `MovedOut`, and the body's assignment reinitializes it
before the drop, so it is accepted. In case 3 the exits are read at the head,
so a late `break` sees what earlier turns did. The head is on both sides of its
own definition, so it is an equation rather than a check. The Lean package
computes its least solution by iteration (`headIter`) and then checks it
(`LoopHead`). The spec's prose paragraph `3.8:79` still words the rule as
back-edge invariance; bringing that wording in line is RUE-2355.

## 6. Running things yourself

- **Build and check everything.** `scripts/rue lean` builds the package
  through Buck with the pinned toolchain, re-checks the compiled modules with
  `leanchecker`, and prints the trust report. `lake build` in this directory
  does the build alone, with `elan` fetching the same pinned toolchain.
- **Run a program.** Open `RueCore/Examples.lean`. Each
  `#eval run demoOps p demoFuel` line runs a program; `lake build`'s log
  prints the result next to the line number, and an editor with the Lean
  extension shows it inline. Change a program, rebuild, and watch the outcome
  change. `#eval checkProgram p` runs the checker the same way.
- **Read a kernel-checked fact.**
  `example : run demoOps returnPastLinear demoFuel = .stuck .linearLeak := by rfl`
  is not a test that ran once; it is a statement the kernel verified when the
  file compiled. Every refusal and trap the fragment can reach has such a
  witness (`Examples.lean`, `Corpus.lean`), and so does the fuel boundary
  (`run demoOps countdown 16` versus `17`).
- **Read the reports.** `DIGEST.md` is every theorem's statement and every
  definition those statements are written in terms of; `TRUST.md` is every
  theorem's axioms. Both are committed, and `lake exe ruecore-digest`
  (`--trust` for the second) regenerates them, so a reviewer's check is to
  regenerate and diff. Section 7 walks the whole path.
- **Read `#print axioms`.** The trust boundary of a Lean proof is the list of
  axioms it depends on, which `TRUST.md` tabulates. The Buck build also writes
  the raw listing (`axioms.txt` beside `trust.md`) for each theorem the target
  trusts (`BUCK`, `root//:lean-ruecore`), a line like

  ```
  'RueCore.soundness' depends on axioms: [propext, Quot.sound]
  ```

  Two things there would be holes: `sorryAx`, which means a proof was left
  unfinished, and `Lean.ofReduceBool` (what `native_decide` introduces), which
  means a result the kernel did not verify itself. Lean's three standard
  axioms, `propext`, `Quot.sound` and `Classical.choice`, are kernel-checked
  assumptions of the logic, not holes; this project's policy is to use only
  the first two (constructive proofs, no classical choice). Any axiom the
  package declared itself would be an assumption to review. The Buck build
  fails on anything outside its allowed set (`toolchains/lean/defs.bzl`), and
  so does `ruecore-digest --trust`, which applies the same policy to *every*
  theorem rather than to the ones the target names.
- **Find the rule.** `INDEX.md` lists every labeled rule of the calculus's §5
  and §6, and every alternative of its §2 grammar, with the declaration that
  mechanizes it or *not yet mechanized*. Start there for "where is (If)?", "is
  (Call) covered yet?", or "does the fragment have arrays?".

## 7. Validating this in thirty minutes

This is the path for a reader who knows type systems or proof assistants and
wants to decide whether to believe the mechanization without trusting whoever
wrote it. It has six steps, each with what a defect would look like. None
requires reading a proof.

**1. Build it yourself (five minutes warm; the first run downloads the
toolchain).**

```bash
scripts/rue lean
```

Buck fetches the SHA-pinned Lean toolchain, about 17,500 files and 2.7 GB
unpacked (`toolchains/lean/defs.bzl`), so the first run is a download and the
five minutes are the ones after it. It then builds the package, re-checks the
compiled modules with the toolchain's own `leanchecker` (an independent
re-verification of the `.olean`s, not a replay of the build), runs the
reports, and prints the trust report. *A defect looks like:* the build
failing, which means what is committed does not compile; no result below is
worth anything until it does. `lake build` in `docs/formal/lean` is the same
check without Buck, using `elan` and the same pin
(`scripts/validate-lean-toolchain-pin.py` holds the two pins equal).

**2. Read the trust report (two minutes).**

`TRUST.md`, which `scripts/rue lean` just printed from the build's own
output, lists every theorem in the `RueCore` namespace with the axioms
`Lean.collectAxioms` says its proof depends on, the number of proofs resting
on `sorryAx`, and the axioms the package declares itself. Its header gives
the counts; expect no axiom anywhere outside `propext` and `Quot.sound`, no
`sorryAx`, and no declared axiom. *A defect looks like:* a `sorryAx` (an
unfinished proof), a `Lean.ofReduceBool` (a `native_decide` the kernel did
not check), a `Classical.choice` (allowed by Lean, outside this project's
constructive policy), or a package-declared axiom that assumes the thing
being proved. A grep for `sorry` would miss the first of those if a macro hid
it; the axiom list would not. Regenerate the report with
`lake exe ruecore-digest --trust` and diff it against the committed copy.
There is no CI gate on that diff yet (RUE-2241 tracks it; nothing in CI runs
the Lean build until ADR-0097's gate is met), so the reviewer is the gate.

**3. Read the digest (ten minutes).**

`DIGEST.md` is likewise generated (`lake exe ruecore-digest`) and likewise
worth regenerating and diffing. Regenerating it also runs the two checks the
file claims for itself: that every `RueCore` constant it prints has an entry
of its own, and that every declaration the generated `INDEX.md` names
survived its filter. A miss is a message on stderr and a non-zero exit, not a
quietly wrong file.

The digest opens with the fragment boundary: how many of the calculus's §5/§6
rules and §2 syntactic forms have a core image at all, quoted from
`INDEX.md`. Then it gives every theorem's statement as Lean elaborated it,
then every definition those statements are written in terms of, in
dependency order. A definition's body is included where it is short enough to
read, so `Ty.mult` (which is `class(T)`, and so what makes a `linear` struct
linear) and `Ctx.join` (§5.5, a premise of `Typed.ite`) can be read rather
than taken on their signatures.

Read `soundness` first and make sure you can state it in one sentence; then
read the corollaries, which should say nothing `soundness` does not. *A
defect looks like:*

- a theorem that quantifies over less than you expected: a hypothesis that
  makes it vacuous, or a `Γ` fixed to `[]` where the claim should be general;
- a corollary that is not an instance of the main theorem;
- a definition whose doc-comment describes something other than what its
  signature says;
- a definition whose body makes the claims about it trivial: a `Ty.mult` that
  answered `.copy` everywhere, or a `Ctx.join` that answered `none`, would
  leave every linearity corollary true and empty.

**4. Read the invariant, and hold it against §7 (five minutes).**

The one place a soundness proof can quietly cheat is its invariant. An
invariant too strong to be provable is caught by the kernel; one too weak to
mean anything is not. `FrameMatches` (in `DIGEST.md`, or `Soundness.lean`) is
this proof's invariant, and its `store` field, through `CellMatches` and
`ContentsMatches`, is what §7's no-use-after-move bullet names in words:
"preservation maintains the invariant that Σ faithfully tracks the store's
initialization". Check `ContentsMatches`'s clauses against that phrase, as
section 3 spells them out: an `owned` node holds a hole-free, well-typed
value; a `movedOut` node holds well-typed contents whose `residualLinear` is
`false`, that is, with no live linear sub-value. *A defect looks like:* that
second clause losing its `residualLinear` condition. Then a live linear value
could sit behind a `MovedOut` entry, the scope-exit leak check could pass
over it, and `no_linear_leak` would be false, yet the proof would still go
through, because the invariant would no longer rule the case out. The
asymmetry is deliberate, and §5.5's join is why (section 3); the missing
restriction would not be.

Then read the other field, `record`: the frame's scope record, reversed,
*is* its environment. *A defect looks like:* that clause weakened to an
inclusion, or dropped. Then a cell could sit in the record twice, or stay in
the record after its `endscope` retired it, and a `return`'s unwind would
drop-retire it a second time: the `useAfterDrop` that `no_use_after_drop`
rules out. Row 9 of example 2 is the walk that clause protects.

Fuel is the other place to look. The theorems say "for every fuel", and
`outOfFuel` satisfies them for free, so check that `fuel_mono` and
`no_masking` are in `DIGEST.md` and say what section 2 says they say. *A
defect looks like:* either one missing, or stated with a hypothesis that
makes it vacuous (`no_masking` with `n = m`, say).

Then ask the fuel question from §6's side, which is checkpoint C's: does
"for every fuel, never `.stuck`" imply that no reduction sequence reaches a
stuck configuration? `step_never_stuck_of_run` says yes, on every program;
`never_stuck_iff` is the equivalence on checked programs; and
`step_progress` is the conclusion for checked programs. Can `outOfFuel` hide
a violation? `run_stuck_of_step_stuck` says a stuck §6 run is a refusal at
every fuel past its length, and `eval_diverges_iff` says that on a checked
program, exhaustion at every fuel is divergence. `03-metatheory.md`'s
"ADR-0097's conditions" section maps each gate condition to its theorem.
*A defect looks like:* any of these stated with `P` fixed or with an extra
hypothesis beyond `ProgramTyped`.

**5. Run the three-way bridge (three minutes).**

The theorems are about `eval` and `check`, not about the compiler. The bridge
ties the two together: every corpus case is printed as a Rue program, and the
compiler, the reference oracle and the native binary are run on it and
compared with what the mechanization says (`README.md`, "The bridge corpus").

```bash
scripts/rue lean-bridge
```

It prints one line per case: the case's name, the verdict (`accept(i64)`,
`reject`), and `agree` or `DISAGREE` with the number of disagreeing pairs,
each carrying the diagnostic code where a program was refused. Then, for each
case that disagrees, it prints the Rue program, the four views side by side,
and the pairs that differ. Last comes a tally:

```text
  cases: <n> (<n-k> agree, <k> disagree)
  checker <-> compiler: …
  lean <-> oracle: …
  lean <-> native: …
  oracle <-> native: …
```

**Expect one disagreement, and a non-zero exit.** It is seeded
deliberately and stays until its issue is decided:

- `array_elem_self_assign`: `a[0] = a[0]`. The model refuses the write into
  the holed array under `3.8:72`; the compiler accepts it on purpose since
  RUE-228. Which is right is a decision: RUE-2346.

Other seeded cases were red until the compiler defect they found was fixed,
and stay as regression signals; `README.md`, "The bridge corpus", lists them.
*A defect looks like:* any **other** case disagreeing. A disagreement is a
defect in one of the four views (the mechanization, the compiler, the oracle,
or the printed program), and the case's `explain/<case>.txt` rendering (the
tables of section 5, from `lake exe ruecore-explain <case>`) is meant to say
which.

**6. Spot-check statements against the calculus (five minutes).**

`INDEX.md` maps every labeled rule to the declaration that mechanizes it.
Pick two of these three and read the calculus and the Lean side by side.

- **(Assign) §5.2, against `Typed.assign`.** The calculus's premises: the
  target is `mut`; the right-hand side is typed first; and `3.8:77`'s
  overwrite premise (`Σ1(p) = MovedOut ∨ ¬carries_linear(T)`) is checked on
  the state *after* the right-hand side. The constructor should have an
  argument for each, with the `Γ₁[p.root]?` lookup (the post-RHS state)
  feeding the disjunction. *A defect looks like:* the disjunction reading
  `Γ[p.root]?`, the pre-RHS state, which would accept a program that
  overwrites a live linear value the right-hand side had not yet consumed.
  (The constructor also carries `assignArrayOk`, §5.2's array side condition,
  and `Owned-Base` on the incoming state as well as the post-RHS one; its
  doc-comment and `assignArrayOk`'s record both as deviations from §5.2 as
  written.)
- **(D-Return) §6.9, against `eval`'s `ret` arm.** The rule discards the
  evaluation context `E'`, every pending `endscope` marker in it included, and
  runs `run-all-scope-drops(H, φ)` instead: over every live binding of every
  enclosing scope (`3.9:18`), in reverse declaration order (`3.9:4`). The arm
  should evaluate the operand, then walk the frame's scope record, then
  return `.returned`, which every enclosing form passes on until a `call`
  absorbs it. *A defect looks like:* the arm running the *innermost* scope
  only (then an early return two scopes deep would leak the outer binding,
  against `3.9:18`); the record walked oldest-first (against `3.9:4`, which
  step 5's stdout comparison would catch on `return_past_affine`); or
  `.returned` absorbed somewhere other than a call boundary (then a `return`
  would stop at the nearest `let`). What the rule does *not* cover is the
  value of an argument already evaluated when a sibling argument returns:
  section 2's pending-value edge, with its witnesses in `Examples.lean`.
- **(D-Let)/(D-EndScope) §6.7, against `eval`'s `letIn` arm.** The machine
  mints a fresh cell for the binder, runs the body, then at scope exit
  inspects that cell. A live linear value is `linearLeak`, and the machine
  stops there. A live affine value is dropped, its event appended to the
  trace, and the cell retired (`dead`). A live copy value or a `⊘` cell drops
  nothing, and the cell is retired just the same. A `†` cell cannot arise
  here, since §6.7 mints the binder's own cell and this arm is where it
  retires it; the machine refuses one (`useAfterDrop`), as it refuses an
  unbound index. *A defect looks like:* the retire omitted (then a use after
  scope exit would read a stale value instead of refusing), or the affine
  drop event emitted in the wrong order relative to the body's own trace,
  which step 5's stdout comparison would catch.

**What thirty minutes does not buy.** The adequacy lemma tying this
executable dynamics to §6's reduction relation is proved both ways
(`eval_sound` and `eval_complete`, section 2), and §7's progress and
preservation are stated over `Step` itself (`step_progress`,
`step_preservation`). But the preservation there is for a semantic
configuration typing, not a syntactic one (section 2). The fuel is this
interpreter's device and has no counterpart in §6, so `fuel_mono` and
`no_masking` are about `eval`, not about the paper machine; their §6-side
counterparts are `run_stuck_of_step_stuck` and `eval_diverges_iff`. And the rules and forms `INDEX.md` marks *not yet
mechanized* are outside every theorem above. The fragment boundary in step 3
is not a formality; it is most of what the reports are for.

## 8. Writing a doc-comment that the index can read

Every top-level declaration in a rule-bearing module (`Syntax`, `Statics`,
`Dynamics`, `Soundness`, `Checker`, `Print`, `Explain`, `CorpusMain`, and any
new module) has a doc-comment citing what it mechanizes: a rule label exactly
as the calculus writes it (`(Use-Move)`, `(D-Let)`, `(@Drop)`), a section
(`§5.5`), or a prose paragraph (`3.8:73`). `instance`s, `example`s, and
constructors without a doc-comment of their own are exempt.

- A declaration that mechanizes nothing on its own (an inversion lemma, a
  printing helper) says `(helper)` instead.
- A module whose declarations are programs rather than rules says
  `xref: examples` in its module docstring.

`scripts/validate-lean-xref-index.py` enforces this and regenerates
`INDEX.md` with `--write`; `README.md` has the full convention. The same
script holds the §2 forms table, which maps each alternative of the
calculus's grammar to the `Expr`/`Ty` constructors that mechanize it. A change
that gives a form its first core image updates that row in the same change.
