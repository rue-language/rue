# Reading RueCore without Lean

A guide for a Rue contributor who can read the core calculus
(`../01-core-calculus.md`) and has never opened a Lean file. It explains what
each Lean artifact *is* in the calculus's own terms, walks one program from Rue
source through the checker and the interpreter to the theorem that covers it,
and says how to run things yourself. The companion `INDEX.md` (generated) maps
every calculus rule to the declaration that mechanizes it, and `README.md` has
the build commands and the file map.

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
inductive Typed : Ctx → Expr → Ty → Ctx → Prop
```

Read `Typed Γ e T Γ'` as the judgment above. Three things are different in
shape and identical in content:

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
  program `e` is well-typed exactly when a value of type `Typed [] e T Γ'`
  exists for some `T` and `Γ'`, which is what "there is a derivation" means.

For example, `(Use-Move)` in the calculus (§5.1, whole bindings only) says: a
use of an owned, non-`Copy` place has the place's type and marks the place
`MovedOut`. In Lean:

```lean
| useMove {Γ i en} :
    Γ[i]? = some en → en.st = .owned → en.ty.mult ≠ .copy →
    Typed Γ (.use i) en.ty (Γ.set i (en.setSt .movedOut))
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
def eval (H : Store) (ρ : Env) : Expr → EvalRes
```

`H` is §6.1's store (a list of cells: `full v`, the moved-out marker `moved`
for `⊘`, or `dead` for a retired allocation `†`), and `ρ` is §6.1's
environment (position `i` ↦ its location in `H`). Instead of stepping once,
`eval` runs the program to the end and reports one of three outcomes:

| `EvalRes` | Meaning in §6 |
| --- | --- |
| `.ok H' v tr` | the machine halted normally with value `v`, final store `H'`, and drop trace `tr` |
| `.panic k` | the machine halted in a defined trap `↯κ` (§6.12): `overflow` or `divZero` |
| `.stuck w` | the machine refused: `w` names either a configuration §6 leaves undefined or a linear action the machine monitors (see below) |

The drop trace `tr` is the list of every drop the machine performed, in
order: `drop ℓ v` for a binding's drop (at scope exit, at `@drop`, or when
overwritten) and `dropTemp v` for a discarded temporary. This is the
fragment's image of the oracle interpreter's observable outcome, and it is
what the bridge compares against a native binary's stdout (`README.md`, "The
bridge corpus").

A `Violation` is a refusal: `useAfterMove` (reading a `⊘` cell),
`useAfterDrop` (touching a `†` cell), `linearLeak` (scope exit on a live
linear value), `linearOverwrite` (`3.8:77`), `linearDiscard` (`3.8:64`),
plus `unbound` and `typeConfusion` for ill-scoped or ill-typed input. §7's
memory-safety bullets each say that one of these never happens. The three
linear refusals are monitors the machine adds (§6's rules would drop the
value and rely on §5 to have forbidden it); the other four are §6's own
stuck states. That is why `eval` is a model of §6 on the programs `check`
accepts, and only there (`Dynamics.lean`).

Why a function rather than the relation: a function can be *run*, so every
semantic question about a fragment program is answerable by execution, and a
total function always returns one of the three outcomes, so progress becomes
the single statement "never `.stuck`", which is what the theorem in section 4
proves. What the function owes the relation is an adequacy lemma (they agree
on every program), owed by RUE-2289 and required before the mechanization
gates anything (`../03-metatheory.md`, "How to read a theorem here").

## 3. What `Matches` says, and why it is asymmetric

Every safety proof carries an invariant relating the static story to the
dynamic one. Here it is `Matches Γ ρ H` (`Soundness.lean`): for each binding,
the cell its location holds agrees with its context entry, locations are
inside the store, and no two bindings share a location. Per cell, `CellMatches`
says:

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

## 4. The theorem, and what `check_sound` buys

```lean
theorem soundness :
  Typed Γ e T Γ' → Matches Γ ρ H →
    (∃ k,        eval H ρ e = .panic k) ∨
    (∃ H' v tr,  eval H ρ e = .ok H' v tr ∧ HasTy v T ∧ Matches Γ' ρ H')
```

In words: a well-typed expression, run in any store that agrees with its
incoming context, either traps in a defined way or produces a well-typed
value with the outgoing context's agreement restored. It never returns
`.stuck`. That is progress and preservation in one statement (§7, first
bullet), and the named corollaries (`no_use_after_move`, `no_linear_leak`,
…) each restate "never `.stuck` with this particular violation" for one §7
bullet.

The theorem quantifies over derivations. To apply it to a *program* you need
to know a derivation exists, and `Checker.lean` is how you find out:
`check Γ e` is the §5 rules run as an algorithm, returning the type and
outgoing context or rejecting. `check_sound` proves that every acceptance is
backed by a real derivation. So the pipeline for any program is: run `check`;
if it accepts, `soundness` applies and the §7 guarantees hold for it; if it
rejects, the program is outside the theorem, and `eval` usually shows which
refusal it would have reached (the corpus prints that when there is one; a
rejection can also be a plain type error, such as an out-of-range literal,
which `eval` runs without complaint). Completeness, that every derivable
program is accepted, is expected but not yet proved (`Checker.lean`).

## 5. A worked example: `reinit`

The corpus case `reinit` (`Corpus.lean`) moves a linear value out of a
binding, assigns a new one back in, and consumes that. It exercises
`(Use-Move)`, `(Assign)` with the `3.8:77` premise checked on the post-RHS
state, reinitialization (`3.8:55`), and the scope-exit leak check (§5.6).

### The program

Core syntax, as `Examples.reinit` writes it:

```lean
letIn true (mkres .linear (intLit 1))
  (seq (consume (use 0))
    (seq (assign 0 (mkres .linear (intLit 2)))
      (consume (use 0))))
```

Printed as Rue source by the bridge (the prelude declaring `RLinear` and
`consume_linear` is omitted here; `README.md` shows it):

```rue
fn main() -> i32 {
    let result: i64 = {
        let mut v0: RLinear = RLinear { value: 1 };
        {
            consume_linear(v0);
            {
                { v0 = RLinear { value: 2 }; };
                consume_linear(v0)
            }
        }
    };
    @dbg(result);
    0
}
```

### The checker's derivation

`check [] reinit` accepts with type `int` and outgoing context `[]`. The
derivation it certifies, read from the outside in, with the fused context
written as `[type, μ, state]` per binding (only one binding, `v0`):

| Step | Rule | Context in | Context out |
| --- | --- | --- | --- |
| `mkres .linear (intLit 1)` | `Typed.mkres`: the abstract resource introduction, the shape of §5.8's aggregate intro without fields | `[]` | `[]` |
| enter the `let` body | `Typed.letIn`, `(Let)`: the binder enters `Owned` | `[]` | `[RLinear, mut, Owned]` |
| `use 0` | `(Use-Move)`: linear, so the use moves | `[RLinear, mut, Owned]` | `[RLinear, mut, MovedOut]` |
| `consume (…)` | `Typed.consume` | `[…, MovedOut]` | `[…, MovedOut]` |
| `seq` discard | `(Seq)`: an `int` carries no linear value (`3.8:64`) | | |
| `mkres .linear (intLit 2)` | RHS of the assignment, typed first | `[…, MovedOut]` | `[…, MovedOut]` |
| `assign 0 …` | `(Assign)`: `v0` is `mut`; on the post-RHS state `v0` is `MovedOut`, so the `3.8:77` premise holds; `v0` becomes `Owned` (`3.8:55`) | `[…, MovedOut]` | `[RLinear, mut, Owned]` |
| inner `seq` discard | `(Seq)`: the assignment's `unit` carries no linear value | | |
| `use 0` | `(Use-Move)` again | `[…, Owned]` | `[…, MovedOut]` |
| `consume (…)` | `Typed.consume`, type `int` | | |
| leave the `let` body | §5.6's scope-exit check, folded into `Typed.letIn`: the residual state is `MovedOut`, so no linear value leaks | `[RLinear, mut, MovedOut]` | `[]` |

The premise that matters is in the `(Assign)` row. Had the first
`consume_linear(v0)` been omitted, the post-RHS state of `v0` would be
`Owned`, the `3.8:77` premise `Σ1(p) = MovedOut ∨ ¬carries_linear(T)` would
fail, and `check` would reject; that is the corpus case `linear_overwrite`,
which the compiler rejects with E0493 and the machine refuses with
`linearOverwrite`.

### The interpreter's run

`eval [] [] reinit` returns `.ok [dead] (.int 2) []`. Step by step, with the
store `H` as a list of cells indexed by location and `ρ` mapping position 0
to its location:

| Step | Store before | Effect | Store after | Trace |
| --- | --- | --- | --- | --- |
| `mkres .linear (intLit 1)` | `[]` | a value, no store effect | `[]` | |
| `(D-Let)`: mint a cell for `v0` | `[]` | allocate location 0, `ρ = [0]` | `[full (res linear 1)]` | |
| `use 0`, `(D-Use-Move)` | `[full (res linear 1)]` | the value moves out; the cell becomes `⊘` | `[moved]` | |
| `consume` | | payload `1`, an `int` | `[moved]` | |
| `(D-Seq)` discard | | an `int` is `Copy`: no drop | `[moved]` | |
| `mkres .linear (intLit 2)` | | a value | `[moved]` | |
| `assign 0`, `(D-Assign)` | `[moved]` | the cell is `⊘`, so nothing is dropped; reinitialize | `[full (res linear 2)]` | |
| inner `(D-Seq)` discard | | `unit` is `Copy`: no drop | `[full (res linear 2)]` | |
| `use 0`, `(D-Use-Move)` | `[full (res linear 2)]` | move out again | `[moved]` | |
| `consume` | | payload `2` | `[moved]` | |
| `(D-EndScope)`: retire `v0` | `[moved]` | the cell is `⊘`, so nothing to drop; retire it | `[dead]` | |

No drop event is ever emitted (both values were consumed, so every cell was
`⊘` at every drop point), so the trace is empty and the printed program's only
output line is the value, `2`. The bridge expectation in `corpus.json` is
exactly that: `{"kind": "ok", "stdout": ["2"], "exit": 0}`.

Both tables above are generated for every corpus case: this one is
`explain/reinit.txt`, printed by `lake exe ruecore-explain reinit`.

### The theorem that covers it

`check` accepted, so by `check_sound` a derivation `Typed [] reinit .int []`
exists, and `soundness` applies with the empty store (the invariant
`Matches [] [] []` holds trivially). It promises: either a defined panic, or
`.ok` with a value of type `int` and the invariant restored for the outgoing
context `[]`. The run above is the second case; `HasTy (.int 2) .int` holds
because `2` is in bounds. The corollary this program illustrates is
`no_linear_overwrite`: the assignment in the middle is the very shape
`3.8:77` guards, and the theorem says the guard is never needed at run time
for a program `check` accepts, because the checker has already demanded the
`MovedOut` state that makes the overwrite-drop a no-op.

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
- **Run a program.** Open `RueCore/Examples.lean`. Each `#eval eval [] [] p`
  line runs a program; `lake build`'s log prints the result next to the line
  number, and an editor with the Lean extension shows it inline. Change a
  program, rebuild, and watch the outcome change. `#eval check [] p` runs the
  checker the same way.
- **Read a kernel-checked fact.** `example : eval [] [] linearLeaked = .stuck
  .linearLeak := by rfl` is not a test that ran once; it is a statement the
  kernel verified when the file compiled. Every refusal and trap the fragment
  can reach has such a witness (`Examples.lean`, `Corpus.lean`).
- **Read `#print axioms`.** The trust boundary of a Lean proof is the list of
  axioms it depends on. `scripts/rue lean` prints, for each of the seven
  theorems the Buck target trusts (`BUCK`, `root//:lean-ruecore`), a line
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
  (`toolchains/lean/defs.bzl`).
- **Find the rule.** `INDEX.md` lists every labeled rule of the calculus's
  §5 and §6 and the declaration that mechanizes it, or *not yet mechanized*.
  Start there when the question is "where is `(If)`?" or "is `(Call)` covered
  yet?".

## 7. Writing a doc-comment that the index can read

Every top-level declaration in a rule-bearing module (`Syntax`, `Statics`,
`Dynamics`, `Soundness`, `Checker`, `Print`, `CorpusMain`, and any new module;
`instance`s, `example`s, and constructors without a doc-comment of their own
are exempt) has a doc-comment citing what it mechanizes: a rule label exactly as the calculus writes it
(`(Use-Move)`, `(D-Let)`, `(@Drop)`), a section (`§5.5`), or a prose
paragraph (`3.8:73`). A declaration that mechanizes nothing on its own (an
inversion lemma, a printing helper) says `(helper)` instead. A module whose
declarations are programs rather than rules says `xref: examples` in its
module docstring. `scripts/validate-lean-xref-index.py` enforces this and
regenerates `INDEX.md` with `--write`; `README.md` has the full convention.
