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
| `.stuck w` | the machine reached a configuration §6 leaves undefined, named by the `Violation` `w` |

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
memory-safety bullets each say that one of these never happens.

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
- **Read the reports.** `DIGEST.md` is every theorem's statement and every
  definition those statements are written in terms of; `TRUST.md` is every
  theorem's axioms. Both are committed and both are regenerated by `lake exe
  ruecore-digest` (`--trust` for the second), so a reviewer's check is to
  regenerate and diff. Section 7 walks the whole path.
- **Read `#print axioms`.** The trust boundary of a Lean proof is the list of
  axioms it depends on, which is what `TRUST.md` tabulates. The Buck build
  also writes the raw listing (`axioms.txt` beside `trust.md`), for each of
  the nine theorems the target trusts (`BUCK`, `root//:lean-ruecore`), a line
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
  applies the same policy to *every* theorem rather than to the trusted nine.
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

**1. Build it yourself (five minutes, most of it waiting).**

```bash
scripts/rue lean
```

Buck fetches the SHA-pinned Lean toolchain, builds the package, re-checks the
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
`sorryAx`, and the axioms the package declares itself. Today: 31 theorems, no
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
regenerating and diffing. It opens with the fragment boundary — how many of the
calculus's §5/§6 rules and §2 syntactic forms have a core image at all, quoted
from `INDEX.md` — then gives every theorem's statement as Lean elaborated it,
then every definition those statements are written in terms of, in dependency
order. Read `soundness` first and satisfy yourself you can state it in one
sentence; then read the corollaries, which should say nothing `soundness` does
not. *A defect looks like:* a theorem that quantifies over less than you
expected (a hypothesis that makes it vacuous, a `Γ` fixed to `[]` where the
claim should be general), a corollary that is not an instance of the main
theorem, or a definition in the dependency list whose doc-comment describes
something other than what its signature says.

**4. Read `Matches`, and hold it against §7 (five minutes).**

The one place a soundness proof can quietly cheat is its invariant: an
invariant strong enough to be unprovable is caught by the kernel, but one too
weak to mean anything is not. `Matches` (in `DIGEST.md`, or `Soundness.lean`)
is this proof's invariant, and §7's no-use-after-move bullet names it in
words — "preservation maintains the invariant that Σ faithfully tracks the
store's initialization". Check the two directions of `CellMatches` against that
phrase, as section 3 above spells them out: an `Owned` entry holds a live
well-typed value; a `MovedOut` entry holds the moved-out marker *or* a live
well-typed **non-linear** value. *A defect looks like:* that second clause
dropping the word `non-linear`. Then a live linear value could sit behind a
`MovedOut` entry, the scope-exit leak check could pass over it, and
`no_linear_leak` would be false — and the proof would still go through, because
the invariant would no longer rule the case out. The asymmetry is deliberate
and §5.5's join is why (section 3); the missing restriction would not be.

**5. Run the three-way bridge (three minutes).**

The theorems are about `eval` and `check`, not about the compiler. What ties
the two together is the bridge: every corpus case is printed as a Rue program,
and the compiler, the reference oracle, and the native binary are run on it and
compared against what the mechanization says (`README.md`, "The bridge
corpus"). It lands with RUE-2228 (#3165) as

```bash
scripts/rue lean-bridge
```

and is expected to report 20 of the 21 seed cases agreeing, with
`cond_drop_affine` red: the compiler drops a conditionally-dropped affine
residue on a path the calculus says it should not, which is a real compiler
defect, tracked as RUE-2290 and left visible rather than suppressed. *A defect
looks like:* any *other* case disagreeing. A disagreement is a defect in one of
the four views — the mechanization, the compiler, the oracle, or the printed
program — and which one is a question the case's `explain/<case>.txt` rendering
(section 5's tables, `lake exe ruecore-explain <case>`) is meant to answer.

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
- `(D-Let)`/`(D-EndScope)`, §6.7, against `eval`'s `letIn` arm. The machine
  mints a fresh cell for the binder, runs the body, then at scope exit inspects
  that cell: a live linear value is `linearLeak`, a live affine value is dropped
  and its event appended to the trace, a `⊘` or `†` cell drops nothing, and the
  cell is retired (`dead`) either way. *A defect looks like:* the retire being
  omitted (then a use after scope exit would read a stale value instead of
  refusing), or the affine drop event being emitted in the wrong order relative
  to the body's own trace, which is exactly what the bridge's stdout comparison
  in step 5 would catch.

What thirty minutes does **not** buy: the adequacy lemma tying this executable
dynamics to §6's reduction relation is owed by RUE-2289 and not proved here
(section 2), and the rules and forms marked *not yet mechanized* in `INDEX.md`
are outside every theorem above. The fragment boundary in step 3 is not a
formality; it is most of what the reports are for.

## 8. Writing a doc-comment that the index can read

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
The same script holds the §2 forms table, which maps each alternative of the
calculus's grammar to the `Expr`/`Ty` constructors that mechanize it; a slice
that gives a form its first core image updates that row in the same change.
