# Mutation analysis of the definitions: would anything notice a wrong rule?

The theorems show that the definitions have the properties we state. They do
not show that the definitions say what the calculus says. A typing rule could
admit too much in a way no theorem mentions, or refuse too much in a way that
makes a theorem easy, and the kernel would check every proof all the same.
This page asks how much of the semantics and the checker the rest of the
package pins (RUE-2465). We change one rule at a time and record what, if
anything, notices.

This is **mutation analysis** ([FIELD.md](../FIELD.md) §7: DeMillo, Lipton &
Sayward, "Hints on Test Data Selection", 1978; Jia & Harman, "An Analysis and
Survey of the Development of Mutation Testing", IEEE TSE 2011, §II.B), applied
to a specification rather than to a program. We use FIELD §7's terms. A
**mutant** is the definitions with one small, deliberate change. A mutant is
**killed** when some test's result on it differs from its result on the real
definitions, and has **survived** otherwise. An **equivalent mutant** always
produces the same result as the original, so no test can kill it. The
**mutation score** is the number of killed mutants divided by the number of
non-equivalent ones. The sibling page
[BRIDGE-SENSITIVITY.md](BRIDGE-SENSITIVITY.md) applies the same analysis to the
compiler; this page applies it to the Lean definitions.

Measured on trunk `c2fe428ff` (2026-09-25), with the six seeds this page adds
(RUE-2465), and again without them for the mutants the seeds could affect
("Mutation score").
The non-vacuity witnesses of RUE-2469 landed after this measurement.
They add proofs and witnesses, never remove them, so a rerun with them can
only kill more; the table and score below predate them.
RUE-2486 (part 1 of 2) seeds seven of proposed issue 1's thirteen
witness-only refusals — `use-move-rootidx`, `index-read-copy`,
`index-drop-copy-checker`, `const-index-off-by-one`, `index-write-linear`,
`residual-declared` and `residual-untracked` — and reruns exactly those seven
(`mutate.py --only`) to confirm the new seed kills each one at the corpus
level; their table rows and the "seeds and the bridge alone" score row are
current, the rest of the table and score still predate RUE-2469 as above.

## What is mutated

The semantics and the checker: `Syntax` (layer L0) and four of L1's seven
modules (README, "Layers").

* `Syntax.lean`: §3's classes (`Ty.mult`, `Attr.lift`, `Mult.join`), the
  place helpers (`Ty.atPath`, `linearResidue`, `Expr.breaks`).
* `Statics.lean`: §5's judgment `Typed` and the ownership states it threads
  (`OwnSt.join`, `residualLinear`, `fnCtx`, `armCtx`, `Ctx.joinOpt`).
* `Checker/Defs.lean`: `check`, the decision procedure the corpus verdicts
  come from.
* `Dynamics.lean`: the definitional interpreter `eval`, the drop glue and the
  run-time monitors.
* `Step.lean`: §6's small-step relation `Step` and its function `step`.

**Not mutated:** L0's `Float` (the float data and the `FloatModel`
interface) and L1's statement vocabulary: `Soundness/Defs` (`HasTy`,
`ContentsTy`, `FrameMatches`, `EvalOk`), `Trace/Defs` (the trace projections,
`Exact`, `Blocks`, `Lifo`) and `Adequacy/Defs` (`Config.SafeAt`). A wrong
definition there weakens a stated property itself, rather than the semantics
the property is about, so what would notice it is a different question: the
Spec layer's Comparator check, the witnesses, the non-vacuity work
(RUE-2469). Mutating those four modules is RUE-2490. So this page measures
whether a wrong rule is noticed, with the statements taken as written.

There are 80 mutants, spread over §3, §§5.1–5.8 and §§6.2–6.12 (the table
below gives each one's section). They use the issue's operators and a few
classic ones:

| Operator | Mutants | What it does |
|---|---:|---|
| premise | 27 | drop a premise from a typing rule or a checker test (in a `Typed` rule the premise becomes `True`, see below) |
| join | 4 | change §5.5's join: the less-moved state wins, a linear disagreement joins, the residual check goes, a diverging arm swallows the branch |
| move-copy | 2 | make a move a copy, in the statics and in the dynamics |
| drop-skip, drop-order | 9 | skip one drop (a destructor, an overwrite drop, a drop mark, a match consume) or reorder drops (destructor after fields, fields last to first, a frame's bindings first to last, an arm's payload first to last) |
| copy-check | 4 | weaken a `Copy` check |
| affine-linear | 7 | make an affine thing linear or the reverse: the class of a linear-carrying struct, the class join, `[T; 0]`, a partially moved declared-linear struct, untracked residue, an affine discard |
| bounds, off-by-one | 7 | remove or weaken a bounds trap; off by one in an array rule, a comparison, a `break`'s unwind, a repeat count |
| order | 3 | change evaluation order (the right-hand side of `a[i] = e` before or after the index, a binary operator's operands) or the parameters' binding order |
| trap, operand | 7 | change an arithmetic trap (wrap instead, the wrong trap kind, `MIN % -1`, `-MIN`, a float-to-int that saturates) or swap an operator's operands |
| monitor | 4 | remove one of the machine's run-time refusals (`linearLeak`, `linearOverwrite`, `linearDiscard`, `ownedUnderCopy`) |
| completeness | 4 | make the checker refuse more: an arm type, the loop-head bound, the acyclicity rounds |
| equivalent-candidate | 2 | a change believed harmless, as a control |

Most mutants change the rule *and* the checker together, the way a real
mistake in the calculus or in its transcription would. Some change one side
only, on purpose: the checker alone (`index-drop-copy-checker`,
`loop-head-unverified`, `entry-params`, and others), the typing rule alone
(`loop-div-breaks`, `loop-break-div-brk`), `eval` alone
(`binop-eval-order`, `index-write-order`, `seq-affine-as-linear`) or `Step`
alone (`step-usecopy-nondet`). A dynamics mutant changes `eval`, `Step` and
`step` alike unless its row says otherwise. Each mutant is written as
exact-text edits in [`bin/mutate.py`](bin/mutate.py).

## Method

`bin/mutate.py` copies the package's sources into a scratch directory (never
the package itself), applies one mutant, and records the first of these that
fails on it, in this order:

1. **proof**: `lake build` fails in a theorem of the proof modules (layer L2:
   `Statics/Lemmas`, `Dynamics/Lemmas`, `Step/Lemmas`, `Soundness`,
   `Checker`, `Trace`, `Adequacy`, `TraceExact`, `TraceOrder`, `Spine`);
2. **witness**: the build fails only in layer L3 (`Examples`, `Witnesses`,
   `Corpus`, `Print`, `Explain`) or in an `example`;
3. **corpus**: the build succeeds, and `lake exe ruecore-corpus` (every
   seed's verdict and expected outcome) differs from the unmutated baseline;
4. **bridge**: the seeds are unchanged, but `--gen 200 --seed 7` (the
   per-lane check's generated cases) changes, and the compiler disagrees with
   the mutant on a changed case;
5. **survived**: nothing fails.

The module lists come from `RueCore/Layers.lean`'s table, the one list the
layering audit checks, so they follow the package as it changes.
`mutate.py --check` fails when a module is missing from the table, when a
mutant's edit no longer matches the sources or touches a module outside L0
and L1, and when the proofs-off or witnesses-off copy (below) leaves a
theorem, an example or a `#guard` on.

A witness failure that is only in `Explain.lean` is recorded apart, as the
**Explain mirror**. `Explain.lean`'s explainer and trace are second copies of
`check` and `eval`, proved equal to them (`explain_result`,
`traceEval_res`). A mutant that changes one copy fails that proof, whether or
not the change means anything, so it is not a test of the definition.

A seed whose expectation the mutant leaves unchanged cannot make the bridge
disagree anew: the bridge already compares that expectation with the
compiler, and it agrees on unmutated trunk (all but the allowed red,
`array_elem_self_assign`). So step 4 runs the compiler (`scripts/rue exec`,
the comparison of the loop's `bin/verify.py`) only on the generated cases the
mutant changed, and step 3 covers the seeds.

**Two further passes.** The first failure can hide the others, for two
reasons.

* **A proof can fail for a reason that is not about meaning.** A proof that
  takes a rule apart by position (`Typed.skel_preserved` does, for all 51 of
  `Typed`'s rules) fails when a premise is deleted, whether or not any
  statement became false. So a `Typed` premise is not deleted but replaced by
  `True`, which is the same rule with the same arity. Even so, `check_sound`
  builds each rule from the checker's tests and fails when a test is gone,
  and a lemma that unfolds a definition fails when the definition changes.
* **Everything imports the proofs.** `Corpus.lean` imports `Examples.lean`,
  which imports `Checker.lean`, so a proof failure stops the corpus from
  being run at all, and a witness failure does the same.

So every mutant a proof fails on is run again with every theorem of L0, L1,
the Spec layer and L2 given `sorry` in place of its proof, its statement
kept: the "without the proofs" column. And every mutant a witness (or the
Explain mirror) fails on, in the first pass or this one, is run a third time
with the examples, the L3 theorems and the `#guard`s given `sorry` as well:
the "corpus and bridge alone" column. Each pass's own baseline reproduces
the first baseline's corpus exactly. A proof failure in the proofs-off copy
stops the run as a script error, since it means a proof module was missed.

A changed case that the mutant's checker accepts and its machine refuses is a
concrete counterexample to `check_sound` together with soundness. The script
lists these (`unsound` in `results.json`), and the readings below cite them.

**The completeness mutants cannot falsify a statement.** `meet-never`,
`first-arm-ty`, `head-iter-bound` and `decl-cycle-rounds` make the checker
refuse more. No statement is about the checker's completeness, so what
notices them is a witness, a seed or the bridge, never a true proof failure.

## Reading a proof failure

A proof that fails on a mutant is evidence only if the statement it proves
is false for the mutant. So for every mutant we read it against the stated
properties: the Spec layer's statements (`SPINE.md`) and the theorems that
link them, such as `check_sound`, `checkProgram_sound`, `step_iff` and
`Step.det`. The reading is one of four, given with its reason in the table's
"Stated properties" and "Why" columns (and in `mutate.py`'s `RULINGS`):

| Reading | Meaning |
|---|---|
| **a stated property is false** | the mutant falsifies a stated property; where the corpus has a checked program the mutant's machine refuses, that program is the counterexample |
| **only a helper is false** | the only false statement is a lemma that restates a definition (for example `evalIntCast_res` names the trap kind); every stated property holds |
| **every statement holds** | no statement is false for the mutant; a proof that failed on it failed as a script (taking a rule apart by position, a `simp` or `rw` that no longer applies, a `split` on a condition that is now constant) |
| **equivalent** | the mutant is an equivalent mutant (FIELD §7): it decides the same as the original on every state a rule reaches |

The table names where the build stopped. That is not necessarily the
statement that is false.

**What counts as killed.** A mutant is killed when a stated property is
false for it, or when a witness, a seed or a generated case fails on it. A
proof that fails as a script, a helper lemma and the Explain mirror are
recorded but do not count on their own. Each is a difference between two
texts, not between a definition and what it should mean.

**Time.** A mutant took 18 s to 8 min, median 31 s, all passes
included: 54 minutes for all 80 on this machine (an Apple-silicon
laptop, one build at a time), after a warm start of the scratch packages'
`.lake`.

**Reproduce.** From a clean checkout, with the compiler buildable:

```bash
python3 docs/formal/lean/bin/mutate.py --check
python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --compiler-root "$PWD"
python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --table   # the table below
python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --score   # the score, after the seeds
```

The results go to `<work>/results.json`. A rerun resumes where the previous
one stopped. `--only a,b` runs a subset, and `--redo` reruns it. The "before"
column of the score comes from a second work directory run with `--src` set
to a copy of the package without the six seeds, over the mutants whose kills
involve them, and passed to `--score` as `--before`.

## Results

### Mutation score

80 mutants, 4 of them equivalent ("Equivalent mutants", below), so the
denominator is 76. A mutant is killed as "Reading a proof failure" defines
it: a stated property is false for it, or a witness, a seed or a generated
case fails on it. "Before" is the package without the six seeds this page
adds (and their six `Examples.lean` witnesses); "after" is with them.

| Measure | Before the six seeds | After |
|---|---:|---:|
| **Killed** | 73/76 (96%) | 76/76 (100%) |
| A stated property is false | 48/76 (63%) | 48/76 (63%) |
| The tests with the proofs off: witnesses, seeds, generated cases | 66/76 (87%) | 71/76 (93%) |
| The seeds and the bridge alone | 50/76 (66%) | 63/76 (83%) |

The "seeds and the bridge alone" row's "after" figure also counts RUE-2486's
seven seeds (`use_move_rootidx`, `index_read_copy`,
`index_drop_copy_checker`, `const_index_off_by_one`, `index_write_linear`,
`residual_declared`, `residual_untracked`), confirmed by a rerun of exactly
those seven mutants (`mutate.py --only`). The other three rows are unaffected
by them: each of the seven was already a proof kill ("Killed" and "a stated
property is false" do not change), and each already failed its
`Examples.lean` witness in the proofs-off pass ("the tests with the proofs
off" does not change either).

These rows are recorded but do not count as kills:

| Also recorded | Before | After |
|---|---:|---:|
| A stated property or a helper lemma is false | 53/76 (70%) | 53/76 (70%) |
| The build or the corpus fails at all: a proof script, a helper lemma or the Explain mirror included | 76/76 (100%) | 76/76 (100%) |

Before the seeds, three mutants were not killed. Each failed only on
something that does not count:

* `breaks-nested`: a proof script only (`eval_quiet`); every statement holds;
* `arm-payload-mutable`: a helper lemma only (`Ctx.skel_armCtx`, which
  restates `armCtx`);
* `assign-immutable`: the Explain mirror only (`explain_result`).

The six seeds kill all three. The "fails at all" row is 100% both times. That
is what a reader would quote as "every mutant was caught", and it is not a
mutation score: all 4 equivalent mutants failed a proof script as well, and 3
of them the Explain mirror.

Counted apart:

* **The Explain mirror** is the first test failure of 6 mutants:
  - the equivalent `use-copy-moved`, `match-exhaustive` and
    `loop-head-unverified`;
  - `repeat-count`, `seq-affine-as-linear` and `seq-droptemp-skip`.

  It was the only failure of `assign-immutable` before the seeds.
* **A helper lemma only** is the reading of 5 mutants:
  - `arm-payload-mutable` and `zero-array-linear`;
  - `cast-kind`, `discard-monitor-off` and `copy-monitor-off`.

  All five are killed after the seeds by a witness or a seed.
* **A proof script only**: 16 mutants fail a proof while every statement
  holds. All 16 are killed after the seeds by a witness, a seed or the bridge.

### What the proofs kill, and what needs the corpus or the bridge

* **The proofs are strongest on the statics.** A mutant that lets the checker
  (or the rules) accept a program the machine then refuses falsifies
  `soundness`, `check_sound` or `checkProgram_sound`. That covers:
  - a dropped fully-owned, `@drop`, overwrite, discard, leak or join premise;
  - a Copy check;
  - `[e; n]` of a non-Copy element;
  - an out-of-range literal;
  - the §3 class mistakes.

  For 18 of them the corpus holds a concrete counterexample: a seed the
  mutant's checker accepts and its machine refuses (`unsound` in
  `results.json`). For `join-residual`, that seed is this page's
  `join_moved_vs_partial_linear`.
* **Some mutants only a proof can see.** With the proofs off, 5 mutants
  survive the tests:
  - `loop-div-breaks` and `loop-break-div-brk` change the rules only.
    `Typed` is not executable, and the checker is unchanged.
  - `step-usecopy-nondet` changes `Step` only, and only `Step.det` sees it,
    because the corpus runs `eval`.
  - `entry-params` is caught only by `checkProgram_sound`. The printed corpus
    frames `main` itself, so a seed cannot express the shape.
  - `seq-droptemp-skip` removes a drop mark, which no output line shows.
    `rest_exactly_once`'s `Exact` sees it, and so does the Explain mirror,
    which does not count.
* **The dynamics' functional content is the tests'.** For these mutants
  every statement holds, because each result is still safe:
  - the §6.4 arithmetic mutants: wraparound instead of a trap, the wrong trap
    kind, `MIN % -1`, a saturating float-to-int, swapped operands;
  - `bounds-negative`, where a negative index reads element 0.

  Witnesses and seeds kill them: `overflow`, `div_zero`,
  `i8_rem_min_by_neg_one`, `float_to_int_trap_*`,
  `array_dyn_write_trap_negative`, and others. This is by design: the Spec
  states safety, not functional correctness, and the corpus compares the
  functional content with the compiler.
* **§6.11's drop order is fixed by a definition, not by a statement.** Three
  drop-glue mutants change `dropContents` and `dropEvents` together:
  - `dtor-skip`, where no destructor runs;
  - `dtor-after-fields`;
  - `fields-reverse`.

  For each, every statement holds. `drop_order`'s `Blocks` is written in
  terms of `dropEvents`, so it follows whatever `dropEvents` says, and
  `no_double_free` counts at most one destructor, which zero satisfies. The
  witnesses and 89, 2 and 24 seeds kill them through their destructor lines.
  The frame-exit and match-exit order mutants (`scope-fifo`,
  `payload-order`) do falsify `drop_order`'s `Lifo`.
* **The monitors are pinned only by examples.** Removing the `linearLeak`,
  `linearOverwrite`, `linearDiscard` or `ownedUnderCopy` refusal leaves every
  stated property true; two of them fail a helper lemma. The linear theorems
  say the machine never refuses a checked program, and a machine that never
  refuses meets that. The refusal witnesses in `Examples.lean`,
  `Corpus.lean` and `Trace.lean` kill all four, and the seeds whose expected
  outcome is that refusal kill three. This is the red-team log's R3
  (RUE-2485), measured.
* **Trace-only mutants are invisible to the bridge by construction.**
  `seq-droptemp-skip`, `residue-mark-skip` and `match-consume-skip` remove a
  drop mark or a `consume` event, and none of these is an output line.
  `Exact` (`drop_exactly_once`, `rest_exactly_once`) is false for all three.
* **The bridge adds 4 kills that the seeds do not make:**
  `zero-array-linear`, `repeat-count`, `binop-eval-order` and
  `decl-cycle-rounds`. With the proofs off, the bridge is the first test to
  kill `decl-cycle-rounds`: 17 generated cases nest declarations deeper than
  the peel's shortened round count, and no seed does.
* **20 mutants got past the seeds and the bridge together**, after the six
  RUE-2465 seeds and before RUE-2486. They fell into three groups, and
  RUE-2486 (part 1 of 2) seeds seven of the third group, leaving **13**:
  - 4 no corpus case can show: `loop-div-breaks`, `loop-break-div-brk`,
    `step-usecopy-nondet` and `entry-params`. Each falsifies a stated
    property.
  - 3 are trace-only: `seq-droptemp-skip`, `residue-mark-skip` and
    `match-consume-skip`. Each falsifies `Exact`.
  - 6 are refusals that a witness proves but still no seed exports, so the
    compiler's matching refusal is never compared. Five are in
    `Examples.lean`: `lit-bounds`, `dbg-observable`, `repeat-copy`,
    `copy-struct-dtor` and `dtor-linear-field`. The sixth, `copy-monitor-off`,
    is `Trace.lean`'s `ownedUnderCopy` witness. This remaining group is
    proposed issue 1's second PR, below.

    RUE-2486's seven new seeds (`use_move_rootidx`, `index_read_copy`,
    `index_drop_copy_checker`, `const_index_off_by_one`, `index_write_linear`,
    `residual_declared`, `residual_untracked`) each reproduce their mutant's
    `Examples.lean` witness as a corpus case, so `use-move-rootidx`,
    `index-read-copy`, `index-drop-copy-checker`, `const-index-off-by-one`,
    `index-write-linear`, `residual-declared` and `residual-untracked` are now
    killed at the corpus level (the "Corpus and bridge alone" column of "The
    mutants", below), the same reading a proof already gave each of them.

### The mutants

* "Killed first by": the first failure in the order of "Method", with the
  theorem or example that failed first, or the seeds whose outcome changed.
* "Without the proofs": the second pass, run only after a proof failure
  ("—" otherwise).
* "Corpus and bridge alone": the third pass, run after a witness or Explain
  mirror failure. "(same)" means the earlier pass already ended at the
  corpus, the bridge or "survived".
* "Stated properties" and "Why": the mutant's reading ("Reading a proof
  failure") and its reason.
* "s": the mutant's wall time in seconds, all passes included.

Measured with the six seeds. A line number is given only for a module the
pass leaves as written. `mutate.py --table` prints this table from
`results.json`.

| # | Mutant | § | Rule | Operator | Killed first by | Without the proofs | Corpus and bridge alone | Stated properties | Why | s |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `use-move-partial` | §5.1 | (Use-Move) | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1179) | corpus: `array_zero_length_moved_twice`, `enum_matched_twice_moving` +1 | a stated property is false | moves a partially moved aggregate whole; the machine meets the hole (seed `partial_then_whole`): `soundness`, `check_sound` | 37 |
| 2 | `use-copy-moved` | §5.1 | (Use-Copy) | premise | proof: `soundness` (`Soundness.lean`) | Explain mirror: `explain_result` (`Explain.lean`) | survived | equivalent | a `Copy` place is never `MovedOut` in a reachable state: a `Copy` `@drop` moves nothing and a `Copy` value is never a hole | 61 |
| 3 | `use-move-dtor` | §5.1 | (Use-Move) 3.9:34 | premise | proof: `check_sound` (`Checker.lean`) | witness: `Examples.lean` example (l. 2742) | corpus: `partial_under_dtor` | every statement holds | E0456 is a static discipline with no dynamic counterpart: the machine runs the program (`partial_under_dtor`), so no stated property is false | 34 |
| 4 | `use-move-rootidx` | §5.1 | (Use-Move) 3.8:68 | premise | proof: `check_sound` (`Checker.lean`) | witness: `Examples.lean` example (l. 1262) | corpus: `use_move_rootidx` | every statement holds | a static discipline (`3.8:68`): the machine moves the element out and drops the rest path by path, without a refusal | 34 |
| 5 | `use-affine-as-copy` | §5.1 | (Use-Copy)/(Use-Move) | move-copy | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1170) | corpus: `array_dyn_write_after_field_move`, `array_elem_reinit` +6 | a stated property is false | an affine use leaves the place `Owned`, so a second use is accepted and the machine meets a hole (`use_after_move`): `soundness` | 31 |
| 6 | `use-declared-residue` | §5.1 | (Use-Declared-Linear-Destructure) | premise | proof: `splitResidue_ok` (`Soundness.lean`) | witness: `Examples.lean` example (l. 2885) | corpus: `destructure_linear_residue` | a stated property is false | accepts a destructure that strands a linear sibling; the machine refuses with `linearLeak` (`destructure_linear_residue`): `soundness` | 34 |
| 7 | `index-read-copy` | §5.1 | (Use-Untrackable-Dynamic-Copy) | copy-check | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1275) | corpus: `index_read_copy` | a stated property is false | accepts a dynamic-index read of a non-Copy element, which the machine refuses (`typeConfusion`): `soundness` | 30 |
| 8 | `index-drop-copy-checker` | §5.1 | (Use-Untrackable-Dynamic-Copy), @drop | copy-check | proof: `check_sound` (`Checker.lean`) | witness: `Examples.lean` example (l. 1305) | corpus: `index_drop_copy_checker` | a stated property is false | the checker accepts `@drop(a[i])` of a non-Copy element, which no `Typed` rule derives: `check_sound` | 34 |
| 9 | `const-index-off-by-one` | §5.1 | Ty.atPath (7.1:9) | off-by-one | proof: `OwnSt.setAt_wf` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 1276) | corpus: `const_index_off_by_one` | a stated property is false | a constant index equal to the length types, and the machine's read fails (`typeConfusion`): `soundness` | 29 |
| 10 | `assign-overwrite` | §5.2 | (Assign) 3.8:77 | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3138) | corpus: `linear_overwrite`, `overwrite_field_past_partial_linear` +1 | a stated property is false | accepts overwriting a live linear place; the machine refuses with `linearOverwrite` (`linear_overwrite`): `soundness` | 34 |
| 11 | `assign-array-ok` | §5.2 | (Assign) 3.8:72 | premise | proof: `check_sound` (`Checker.lean`) | witness: `Examples.lean` example (l. 1170) | corpus: `array_elem_reinit`, `array_elem_self_assign` | every statement holds | `soundness` does not use the premise (`assignArrayOk`'s doc-comment): the write it refuses runs without a refusal | 34 |
| 12 | `assign-immutable` | §5.2 | (Assign) mut | premise | witness: `Examples.lean` example (l. 4083) | — | corpus: `assign_immutable`, `match_payload_assign` | every statement holds | mutability is not a safety property: the machine performs the write | 32 |
| 13 | `index-write-linear` | §5.2 | (Assign) at a dynamic index | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1278) | corpus: `index_write_linear` | a stated property is false | accepts writing a linear element through a dynamic index; the machine refuses with `linearOverwrite`: `soundness` | 34 |
| 14 | `drop-residual-below` | §5.3 | (@Drop) E0406 | premise | proof: `check_sound` (`Checker.lean`) | witness: `Examples.lean` example (l. 2747) | corpus: `linear_field_stranded` | every statement holds | E0406's residual side condition has no dynamic counterpart (`linear_field_stranded` runs): no stated property is false | 34 |
| 15 | `drop-moved` | §5.3 | (@Drop) | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3127) | corpus: `loop_moved_prev_iteration`, `loop_nested_move_outer` +1 | a stated property is false | accepts `@drop` of a moved-out place; the machine meets the hole (`use_after_move`): `soundness` | 31 |
| 16 | `seq-discard` | §5.3 | (Seq) 3.8:64 | premise | proof: `soundness` (`Soundness.lean`) | witness: `Corpus.lean` example (l. 976) | corpus: `linear_temporary_discarded` | a stated property is false | accepts discarding a linear value; the machine refuses with `linearDiscard` (`linear_temporary_discarded`): `soundness` | 32 |
| 17 | `join-owned-wins` | §5.5 | join | join | proof: `ownedJoinOkList_of_residualLinearFields_false` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 3183) | corpus: `loop_moved_prev_iteration`, `loop_nested_move_outer` | a stated property is false | the join keeps `Owned` where one arm moved, so a later use is accepted and meets the hole (`loop_moved_prev_iteration`): `soundness` | 26 |
| 18 | `join-linear-disagree` | §5.5 | join 3.8:50 (E0443) | join | proof: `ownedJoinOk_residualLinear` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 1228) | corpus: `array_linear_elem_one_path`, `destructure_one_arm` +6 | a stated property is false | a linear path `Owned` on one arm and `MovedOut` on the other joins; the run leaks (`linear_half_consumed`): `soundness` | 27 |
| 19 | `join-residual` | §5.5 | join (residual reading) | join | proof: `OwnSt.join_movedOut_left` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 4072) | corpus: `join_moved_vs_partial_linear` | a stated property is false | accepts a leak through the join; `join_moved_vs_partial_linear` is accepted and refused with `linearLeak`: `soundness` | 26 |
| 20 | `join-diverge-arm` | §5.5/§5.7 | join over Ω (Sub-Never) | join | proof: `Ctx.joinOpt_skel` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 3814) | corpus: `loop_nested_move_outer` | a stated property is false | one diverging arm makes the branch diverge, so what follows is not typed but runs (`loop_nested_move_outer` refused): `soundness` | 26 |
| 21 | `meet-never` | §5.5/§5.7 | (If) arm type, (Sub-Never) | completeness | proof: `CTy.meet_fits` (`Checker.lean`) | witness: `Examples.lean` example (l. 3710) | corpus: `if_panic_arm_linear`, `if_return_arm_affine` +8 | every statement holds | refuses more: no statement is about the checker's completeness | 36 |
| 22 | `first-arm-ty` | §5.5 | (Match) arm type | completeness | witness: `Examples.lean` example (l. 2786) | — | corpus: `enum_return_past_payload`, `match_never_first_arm` +1 | every statement holds | refuses more: no statement is about the checker's completeness | 18 |
| 23 | `match-exhaustive` | §5.5 | (Match) exhaustiveness | equivalent-candidate | proof: `soundness` (`Soundness.lean`) | Explain mirror: `explain_result` (`Explain.lean`) | survived | equivalent | `TypedArms` and `checkArms` walk arms and variants in step and fail on a length mismatch, so the premise is implied | 64 |
| 24 | `arm-leak` | §5.5/§5.6 | (Match) arm scope exit | premise | proof: `TypedArms.at_index` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 2826) | corpus: `enum_arm_leaks_payload` | a stated property is false | accepts an arm that ends with a live linear payload binding; the machine refuses with `linearLeak` (`enum_arm_leaks_payload`): `soundness` | 27 |
| 25 | `arm-payload-mutable` | §5.5 | (Match) payload binders | premise | proof: `Ctx.skel_armCtx` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 4083) | corpus: `match_payload_assign` | only a helper is false | only `Ctx.skel_armCtx`, which restates `armCtx`; mutability is not a safety property | 26 |
| 26 | `let-leak` | §5.6 | (Let) scope exit | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1266) | corpus: `linear_leaked`, `struct_linear_field_leaked` | a stated property is false | accepts a `let` that ends with a live linear binding; `linearLeak` (`linear_leaked`): `soundness` | 31 |
| 27 | `residual-declared` | §5.6 | residual-linear (3.8:74) | affine-linear | proof: `residualLinear_mult_linear` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 2954) | corpus: `residual_declared` | a stated property is false | a partially moved declared-linear struct owes nothing, so its leak is accepted; the machine's monitor still refuses: `soundness` | 26 |
| 28 | `residual-untracked` | §5.6 | residual-linear, untracked residue | affine-linear | proof: `ownedJoinOkList_residualLinearFields` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 1266) | corpus: `residual_untracked` | a stated property is false | untouched linear slots owe nothing, so their leak is accepted; the machine refuses: `soundness` | 26 |
| 29 | `return-leak` | §5.7 | (Return-Value) | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3131) | corpus: `return_past_linear` | a stated property is false | accepts a `return` past a live linear binding; `linearLeak` (`return_past_linear`): `soundness` | 32 |
| 30 | `break-leak` | §5.7 | (Loop-Break) loop locals | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3874) | corpus: `loop_break_past_linear` | a stated property is false | accepts a `break` past a live linear loop-local; `linearLeak` (`loop_break_past_linear`): `soundness` | 31 |
| 31 | `loop-div-breaks` | §5.7 | (Loop-Div) | premise | proof: `soundness` (`Soundness.lean`) | survived | (same) | a stated property is false | the rules type a loop that breaks as diverging, so what follows is not typed but runs: `soundness` | 57 |
| 32 | `loop-break-div-brk` | §5.7 | (Loop-Break), no reachable exit | premise | proof: `soundness` (`Soundness.lean`) | survived | (same) | a stated property is false | the rules type a loop with a reachable `break` as diverging: `soundness` | 54 |
| 33 | `loop-head-unverified` | §5.7 | loop head (LoopHead) | premise | proof: `check_sound` (`Checker.lean`) | Explain mirror: `explain_result` (`Explain.lean`) | survived | equivalent | `headIter` returns a candidate only when one more step leaves it unchanged, and `check` is deterministic, so the re-check always passes | 65 |
| 34 | `head-iter-bound` | §5.7 | loop head iteration | completeness | witness: `Examples.lean` example (l. 3798) | — | corpus: `loop_reassign_then_move` | every statement holds | refuses more: no statement is about the checker's completeness | 18 |
| 35 | `breaks-nested` | §5.7 | Expr.breaks | premise | proof: `eval_quiet` (`TraceExact.lean`) | witness: `Examples.lean` example (l. 4097) | corpus: `loop_inner_break_outer_return` | every statement holds | refuses more: the outer loop is typed `unit` rather than `never` | 45 |
| 36 | `fn-exit-leak` | §5.8 | (Fn) exit edge | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3132) | corpus: `linear_param_leaked` | a stated property is false | accepts a function body that ends with a live linear parameter; `linearLeak` (`linear_param_leaked`): `soundness` | 34 |
| 37 | `fn-params-order` | §5.8 | (Fn) entry context | order | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 2976) | corpus: `params_two_types` | a stated property is false | a body is typed against its parameters in the wrong order, so it runs on values of other types: `soundness` | 38 |
| 38 | `entry-params` | §6.12 | top-level main() | premise | proof: `checkProgram_sound` (`Checker.lean`) | survived | (same) | a stated property is false | `checkProgram` accepts an entry point with parameters, which `ProgramTyped` excludes: `checkProgram_sound` | 61 |
| 39 | `lit-bounds` | §5.8 | (Lit) | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3592) | survived | a stated property is false | an out-of-range literal types, and `HasTy.int` requires `InBounds`: `soundness` | 33 |
| 40 | `dbg-observable` | §5.8 | (Dbg) | premise | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3124) | survived | a stated property is false | `@dbg` of an aggregate types; the machine refuses (`typeConfusion`): `soundness` | 32 |
| 41 | `repeat-copy` | §5.8 | array repeat (7.1:36) | copy-check | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1274) | survived | a stated property is false | `[e; n]` of a non-Copy element types; the machine refuses (`typeConfusion`): `soundness` | 31 |
| 42 | `class-not-infectious` | §3 | class of a struct (Attr.lift) | affine-linear | proof: `StructDecl.Wf.field_not_linear` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 1057) | corpus: `affine_explicit_drop`, `affine_overwrite` +94 | a stated property is false | a linear-carrying struct is `Affine`, so dropping it is accepted and the machine's monitor refuses the live linear field: `soundness` | 31 |
| 43 | `mult-join-meet` | §3 | class join | affine-linear | proof: `Mult.rank_le_join_left` (`Statics/Lemmas.lean`) | witness: `Examples.lean` example (l. 1057) | corpus: `affine_explicit_drop`, `affine_overwrite` +94 | a stated property is false | the class join takes the lesser class, so a linear-carrying struct is not `Linear`; as `class-not-infectious`: `soundness` | 31 |
| 44 | `zero-array-linear` | §3 | class of [T; 0] (3.8:74) | affine-linear | proof: `Ty.array_mult_linear` (`Statics/Lemmas.lean`) | witness: `Print.lean` example (l. 729) | bridge: `gen_7_185` | only a helper is false | only `Ty.array_mult_linear`, which restates `Ty.mult`; `[T; 0]` being `Linear` refuses more | 31 |
| 45 | `copy-struct-dtor` | §3 | @copy struct (3.9:31) | premise | proof: `checkStructDecl_sound` (`Checker.lean`) | witness: `Examples.lean` example (l. 3164) | survived | a stated property is false | accepts a `@copy` struct with a destructor, which `WfDecls` excludes: `checkProgram_sound` | 39 |
| 46 | `dtor-linear-field` | §3 | destructor with a linear field (3.9:44) | premise | proof: `checkStructDecl_sound` (`Checker.lean`) | witness: `Examples.lean` example (l. 3171) | survived | a stated property is false | accepts a destructor-bearing struct with a linear field, which `WfDecls` excludes: `checkProgram_sound` | 19 |
| 47 | `decl-cycle-rounds` | §3 | acyclicity 3.0:5 (E0483) | completeness | proof: `checkNoCycle_sound` (`Checker.lean`) | bridge: `gen_7_127` +16 | (same) | every statement holds | refuses more: no statement is about the checker's completeness | 63 |
| 48 | `entry-join-bty` | §5.5 | Entry.join | equivalent-candidate | proof: `Entry.join_assoc` (`Statics/Lemmas.lean`) | survived | (same) | equivalent | every join is of two entries with one skeleton, so the two declared types are equal | 51 |
| 49 | `dyn-move-as-copy` | §6.3 | (D-Use-Move) | move-copy | proof: `stepEval_complete` (`Step/Lemmas.lean`) | witness: `Examples.lean` example (l. 1128) | corpus: `array_dyn_read_after_sibling_move`, `array_dyn_write_after_field_move` +24 | a stated property is false | an affine use copies, so both copies drop and a destructor runs twice: `no_double_free` | 30 |
| 50 | `step-usecopy-nondet` | §6.3 | (D-Use-Copy), Step only | copy-check | proof: `Step.step_eq` (`Step/Lemmas.lean`) | survived | (same) | a stated property is false | two `Step` rules apply to one non-Copy use: `Step.det` | 57 |
| 51 | `bounds-off-by-one` | §6.5 | (D-Index-Trap) | off-by-one | proof: `inBoundsIdx_eq_true` (`Dynamics/Lemmas.lean`) | witness: `Examples.lean` example (l. 1175) | corpus: `array_bounds_trap_at_len`, `array_zero_length_dyn_trap` +1 | a stated property is false | an index equal to the length passes the check and the read fails (`typeConfusion`): `soundness` | 20 |
| 52 | `bounds-negative` | §6.5 | (D-Index-Trap) | bounds | proof: `Contents.resolveDyn_ok` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1106) | corpus: `array_dyn_write_trap`, `array_dyn_write_trap_negative` | every statement holds | a negative index reads element 0: a defined, well-typed result, so every stated property holds | 23 |
| 53 | `bounds-stuck` | §6.5 | (D-Index-Trap) | bounds | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1101) | corpus: `array_bounds_trap`, `array_bounds_trap_at_len` +6 | a stated property is false | an out-of-range index is a stuck state: `soundness` | 23 |
| 54 | `repeat-count` | §6.5 | array repeat | off-by-one | proof: `soundness` (`Soundness.lean`) | Explain mirror: `traceEval_res` (`Explain.lean`) | bridge: `gen_7_156` +2 | a stated property is false | `[v; n]` builds `n + 1` elements, not a value of `[T; n]`: `soundness` | 58 |
| 55 | `overflow-wrap` | §6.4 | (D-Arith-Trap) | trap | proof: `intResult_res` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3313) | corpus: `i64_min_times_neg1`, `i8_div_min_by_neg_one` +4 | every statement holds | wraparound yields an in-range value: safe, and the spine states safety, not the arithmetic | 24 |
| 56 | `divzero-kind` | §6.4 | (D-Div-Trap) | trap | proof: `binOpInt_res` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3354) | corpus: `dbg_before_trap`, `div_zero` +1 | every statement holds | a trap of the wrong kind is still a defined trap | 25 |
| 57 | `rem-min-overflow` | §6.4 | (D-Div-Trap), MIN % -1 | trap | proof: `binOpInt_res` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3321) | corpus: `i8_rem_min_by_neg_one` | every statement holds | `MIN % -1` yields `0`, an in-range value | 27 |
| 58 | `operand-swap` | §6.4 | (D-Arith) | operand | proof: `evalBinOp_res` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3317) | corpus: the export aborts (stack overflow) | every statement holds | swapped operands still yield an in-range value or a trap | 481 |
| 59 | `gt-off-by-one` | §6.4 | (D-Ord) | off-by-one | witness: `Witnesses.lean` example (l. 247) | — | corpus: `loop_break_past_local`, `loop_reassign_then_move` | every statement holds | `>` as `>=` still yields a `bool` | 55 |
| 60 | `neg-no-overflow` | §6.4 | (D-Neg) | trap | proof: `evalUnOp_int_res` (`Soundness.lean`) | witness: `Examples.lean` example (l. 4104) | corpus: `i8_neg_min` | a stated property is false | `-MIN` yields the out-of-range `128` at `i8`, and `HasTy.int` requires `InBounds`: `soundness` | 22 |
| 61 | `cast-kind` | §6.4 | (D-Int-Cast-Trap) | trap | proof: `evalIntCast_res` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3331) | corpus: `int_cast_out_of_range` | only a helper is false | only `evalIntCast_res`, which names the trap kind; a trap of the wrong kind is still a defined trap | 23 |
| 62 | `float-to-int-saturate` | §6.4 | (D-Float-To-Int) | trap | proof: `evalFintrin_float_res` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3540) | corpus: `float_to_int_trap_inf`, `float_to_int_trap_nan` +1 | every statement holds | an out-of-range float-to-int yields `0`, an in-range value | 22 |
| 63 | `binop-eval-order` | §6.2 | evaluation order, eval only | order | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1710) | bridge: `gen_7_118` +1 | a stated property is false | `eval` runs the right operand first and `Step` the left, so they disagree on a program with effects in both: `eval_sound`, `soundness` | 24 |
| 64 | `index-write-order` | §6.2 | evaluation order, eval only | order | proof: `soundness` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1680) | corpus: `array_dyn_write_rhs_first` | a stated property is false | `eval` runs the index first and `Step` the right-hand side: `eval_sound`, `soundness` | 22 |
| 65 | `dtor-skip` | §6.11 | drop glue: destructor | drop-skip | proof: `dropContents_events` (`Soundness.lean`) | witness: `Examples.lean` example (l. 1068) | corpus: `affine_explicit_drop`, `affine_overwrite` +87 | every statement holds | `drop_order`'s `Blocks` is written in terms of `dropEvents`, which changes with it, and zero destructors satisfy `no_double_free` | 23 |
| 66 | `dtor-after-fields` | §6.11 | drop glue order (3.9:15) | drop-order | proof: `dropContents_struct_events` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3442) | corpus: `struct_nested_dtor_drop`, `two_params_dropped_at_pop` | every statement holds | `Blocks` follows `dropEvents`, which changes with `dropContents` | 22 |
| 67 | `fields-reverse` | §6.11 | drop glue order (3.9:15) | drop-order | proof: `dropEventsList_eq_flatten` (`Dynamics/Lemmas.lean`) | witness: `Examples.lean` example (l. 1068) | corpus: `array_drop_order`, `array_dyn_read_after_sibling_move` +22 | every statement holds | `Blocks` follows `dropEvents`, which changes with `dropContents` | 20 |
| 68 | `scope-fifo` | §6.9 | frame exit drop order | drop-order | proof: `runAllScopeDrops_ok` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3435) | corpus: `enum_return_past_payload`, `return_past_affine` +2 | a stated property is false | a frame's bindings are dropped first-declared first: `drop_order`'s `Lifo` | 22 |
| 69 | `payload-order` | §6.6 | match arm exit order | drop-order | proof: `soundness` (`Soundness.lean`) | witness: `Witnesses.lean` example (l. 187) | corpus: `enum_two_payload_bindings` | a stated property is false | an arm's payload bindings are dropped first to last: `drop_order`'s `Lifo` | 54 |
| 70 | `overwrite-no-drop` | §6.8 | (D-Assign) overwrite drop | drop-skip | proof: `sim_assign` (`Adequacy.lean`) | witness: `Examples.lean` example (l. 1078) | corpus: `affine_overwrite`, `array_elem_overwrite` +7 | a stated property is false | an assignment's old value is never dropped or freed: `Exact` (`drop_exactly_once`) | 32 |
| 71 | `break-skip-local` | §6.10 | (D-Break) unwind | off-by-one | proof: `loop_step` (`Soundness.lean`) | witness: `Examples.lean` example (l. 3875) | corpus: `loop_break_past_linear`, `loop_break_past_local` +1 | a stated property is false | `break` skips a loop-local's drop, which is never freed: `Exact` | 23 |
| 72 | `seq-affine-as-linear` | §6.7 | (D-Seq) affine discard | affine-linear | proof: `soundness` (`Soundness.lean`) | Explain mirror: `traceEval_res` (`Explain.lean`) | corpus: `affine_temporary_discarded` | a stated property is false | `eval` refuses to discard an affine temporary in a checked program: `soundness` | 54 |
| 73 | `seq-droptemp-skip` | §6.7 | (D-Seq) temporary drop mark | drop-skip | proof: `rest_step` (`TraceExact.lean`) | Explain mirror: `traceEval_res` (`Explain.lean`) | survived | a stated property is false | a discarded temporary is never marked freed: `rest_exactly_once`'s `Exact` | 94 |
| 74 | `residue-mark-skip` | §6.3 | destructure residue drop mark | drop-skip | proof: `residueMark_measure` (`Trace.lean`) | witness: `Examples.lean` example (l. 2921) | survived | a stated property is false | a destructure's residue is never marked freed: `Exact` | 32 |
| 75 | `match-consume-skip` | §6.6 | (D-Match) consume | drop-skip | proof: `matchConsume_measure` (`Trace.lean`) | witness: `Examples.lean` example (l. 2821) | survived | a stated property is false | a matched enum's shell identity is never freed: `Exact` | 32 |
| 76 | `leak-monitor-off` | §6.11 | linearLeak monitor | monitor | witness: `Examples.lean` example (l. 1347) | — | corpus: `destructure_linear_residue`, `enum_arm_leaks_payload` +9 | every statement holds | the linear theorems say the machine never refuses a checked program, which a machine with no refusal meets | 24 |
| 77 | `overwrite-monitor-off` | §6.8 | linearOverwrite monitor | monitor | witness: `Corpus.lean` example (l. 972) | — | corpus: `linear_overwrite` | every statement holds | as `leak-monitor-off` | 24 |
| 78 | `discard-monitor-off` | §6.7 | linearDiscard monitor | monitor | proof: `eval_succ` (`Soundness.lean`) | witness: `Corpus.lean` example (l. 974) | corpus: `linear_temporary_discarded` | only a helper is false | only `eval_succ`, which restates `eval`; the linear theorems hold as for `leak-monitor-off` | 24 |
| 79 | `copy-monitor-off` | §6.5 | ownedUnderCopy monitor | monitor | proof: `Cons.intro` (`Trace.lean`) | witness: `Trace.lean` example | survived | only a helper is false | only `Cons.intro`, a ledger step for `introVal`; a checked program never builds an owned value under a `Copy` one | 91 |
| 80 | `dyn-residual-declared` | §6.11 | Contents.residualLinear (3.8:74) | affine-linear | witness: `Examples.lean` example (l. 1342) | — | corpus: `destructure_linear_residue`, `enum_arm_leaks_payload` +10 | every statement holds | the machine's leak monitor is weaker; as `leak-monitor-off` | 24 |

### Equivalent mutants

Four mutants are equivalent (FIELD §7). Each changes the text of a
definition but not what the definition decides on any state a rule can
reach, so no test can kill it. All four failed a proof script anyway, and
three of them failed the Explain mirror. This is why neither counts as a
kill on its own.

* **`use-copy-moved`** drops fully-owned from the `Copy` use. A `Copy` place
  is never `MovedOut` in any state the checker computes:
  - `@drop` of a Copy place moves nothing;
  - a Copy struct's fields are Copy;
  - the checker's loop head is the least one.

  At run time a Copy value is never a hole, and the checker's verdicts are
  unchanged on every seed and generated case.
* **`match-exhaustive`** drops `arms.length = ed.variants.length`. `TypedArms`
  and `checkArms` walk the arms and the variants in step and fail on a length
  mismatch, so the premise is implied.
* **`loop-head-unverified`**: the checker no longer re-checks that its
  iterated loop head solves the head equation. `headIter` returns a candidate
  only when one more step leaves it unchanged, and `check` is deterministic,
  so the re-check always passes.
* **`entry-join-bty`** joins at the second entry's declared type. Every join
  is between two entries with the same skeleton (`Ctx.join` is only ever
  applied to two arms of one incoming context), so the two types are equal.
  This answers the red-team log's "`Entry.join` ignores the second entry's
  type" (dropped there and "left to RUE-2465's mutants"): it is harmless.

### Survivors and their resolution

Before the six seeds, three mutants survived by this page's measure, and six
more were killed only because a stated property is false for them, with no
test failing. Each is resolved here:

| Mutant | Before the seeds | Resolution |
|---|---|---|
| `breaks-nested` (§5.7, `Expr.breaks` looks inside a nested loop) | survived: a proof script only (`eval_quiet`); every statement holds, since typing the outer loop `unit` rather than `never` only refuses more | **seed** `loop_inner_break_outer_return` (an outer loop that only an inner `break` exits, leaving by `return`), and its witness |
| `arm-payload-mutable` (§5.5, payload bindings are `mut`) | survived: a helper lemma only (`Ctx.skel_armCtx` restates `armCtx`); mutability is not a safety property | **seed** `match_payload_assign` (the compiler refuses: "cannot assign to immutable variable", E0203) |
| `assign-immutable` (§5.2, the `mut` premise dropped) | survived: the Explain mirror only | **seed** `assign_immutable` (E0203) |
| `join-residual` (§5.5, `MovedOut` joins a partially moved node without the residual check) | a stated property is false (`soundness`), and no test failed | **seed** `join_moved_vs_partial_linear` (E0443), which is also `soundness`'s counterexample: checked by the mutant, refused with `linearLeak` |
| `neg-no-overflow` (§6.4, `-MIN` yields the out-of-range `128` at `i8`) | a stated property is false (`soundness`'s value typing), and no test failed | **seed** `i8_neg_min` (traps with overflow) |
| `fn-params-order` (§5.8, parameters bound in the wrong order) | a stated property is false, and a witness failed, but no seed: every multi-parameter seed takes one type | **seed** `params_two_types` |
| `loop-div-breaks`, `loop-break-div-brk` (§5.7, rules only) | a stated property is false (`soundness`); the checker is unchanged, so no test can see a change to the rules alone | none: the proof is the right detector |
| `step-usecopy-nondet` (§6.3, `Step` only) | a stated property is false (`Step.det`); the corpus runs `eval` | none |
| `entry-params` (§6.12, checker only) | a stated property is false (`checkProgram_sound`) | none: the printer frames `main`, so a seed cannot express it |

With the seeds, the six seeded mutants are killed as follows:

* **With the proofs off:**
  - `join-residual`, `arm-payload-mutable`, `breaks-nested` and
    `neg-no-overflow` are killed by their new `Examples.lean` witness;
  - `fn-params-order` is killed by the witness it already had;
  - `assign-immutable` is killed first by its new witness.
* **With the witnesses off too:** each is killed by its new seed, and
  `assign-immutable` by `match_payload_assign` as well.

All six seeds agree with the compiler (`bin/verify.py`: 180 seeds, the one
disagreement the allowed red).

### Proposed issues (for the coordinator)

1. **[Formal/Bridge] Seed the refusals only a witness proves.** 13 mutants
   were killed by an `Examples.lean` or `Trace.lean` refusal witness and by
   no seed or generated case. Each is a refusal the compiler should make too,
   and the bridge never compared it. RUE-2486 is this issue's two PRs:
   * **Part 1 (done, this page's seven `corpus:` rows above):** moving an
     element out below a projection (`use-move-rootidx`); a dynamic-index
     read, `@drop` or write of a non-Copy or linear element
     (`index-read-copy`, `index-drop-copy-checker`, `index-write-linear`); a
     constant index equal to the length (`const-index-off-by-one`); a
     partially reassigned declared-linear struct that leaks
     (`residual-declared`); and a linear field after a moved slot that leaks
     (`residual-untracked`).
   * **Part 2 (remaining, 6 mutants):** an out-of-range literal
     (`lit-bounds`); `@dbg` of an aggregate (`dbg-observable`); `[e; n]` of a
     non-Copy element (`repeat-copy`); a `@copy` struct with a destructor,
     and a destructor-bearing struct with a linear field (`copy-struct-dtor`,
     `dtor-linear-field`); and the `ownedUnderCopy` refusal
     (`copy-monitor-off`).

   Evidence: the refusal mutants listed under "What the proofs kill". Each
   still-open one shows "survived" in the table's "Corpus and bridge alone"
   column; the seven part 1 seeded show `corpus:` there instead, confirmed by
   a rerun of exactly those seven (`mutate.py --only`). At most 6–8 seeds go
   in one PR, so this is about two PRs.
2. **[Formal/Assurance] State §6.11's drop order independently of
   `dropEvents`.** `drop_order`'s `Blocks` is defined through `dropEvents`,
   so changing `dropContents` and `dropEvents` together falsifies no stated
   property. That covers:
   * no destructor at all (`dtor-skip`);
   * the destructor after the fields (`dtor-after-fields`);
   * the fields last to first (`fields-reverse`).

   The destructor-first, declaration-order and ascending-index claims live
   in a definition, not in a statement. A Spec statement would put them in
   the claim, for example: "`dropEvents` of a destructor-bearing struct
   starts with its `dtor` event, then its fields' blocks in declaration
   order". That is for the Spec layer's review, and for Steve if the order
   should be a §7 bullet.
3. **[Formal/Assurance] The linear theorems are satisfied by a machine with
   no monitors.** Removing any of the four run-time refusals leaves every
   stated property true. Evidence: `leak-monitor-off`,
   `overwrite-monitor-off`, `discard-monitor-off`, `copy-monitor-off` and
   `dyn-residual-declared`. This is R3 of the red-team log, measured. It
   belongs as a comment on RUE-2469 (bring the monitor-fires witnesses into
   the statement layer) rather than as a new issue.
4. **(Loop tooling, state-dir `bin/`) Keep the mutants applicable.**
   `bin/mutate.py --check` takes a few seconds. It fails when an edit no
   longer matches the sources, and when a module is missing from
   `Layers.lean` or left on by the proofs-off copy. Adding it to `chain.sh`
   would keep the mutants current as the definitions change, and REDTEAM.md's
   cadence could rerun the analysis per milestone. The coordinator decides.

Mutating the statement vocabulary (`Soundness/Defs`, `Trace/Defs`,
`Adequacy/Defs`) and `Float` is RUE-2490 ("What is mutated").

### Limits

* **The readings are by reading.** Each mutant's reading, in the table and
  in `mutate.py`'s `RULINGS`, is our judgement against the stated
  properties. The reason is given in one line. Where the corpus has a
  concrete counterexample (18 mutants) the reading rests on it; elsewhere it
  rests on the argument, not on a proof. "Every statement holds", in
  particular, is a claim that no statement became false.
* **The first failure hides the rest.** The table names where the build
  stopped. A later theorem may also be false, and a helper's failure may
  mask a stated property's.
* **Seeds and 200 generated cases.** The bridge column uses `--gen 200 --seed
  7`, the per-lane check's. A larger stream might kill more of the 20 mutants
  the seeds and the bridge miss; the 13 refusal shapes above are not ones the
  generator draws.
* **`operand-swap`'s corpus kill is a crash.** With the operands swapped, a
  seed's counted loop never exits, and `ruecore-corpus` overflows the native
  stack at the export fuel instead of reporting the case as not completed.
  The mutant is killed either way, but the export would crash the same way on
  any non-terminating seed.
* **The equivalence of the four equivalent mutants is argued, not proved.**
