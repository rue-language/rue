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

Measured on trunk `b36896b5f` (2026-09-26): all 96 mutants in one run of
`bin/mutate.py`, with the spec pass RUE-2499 added ("Method"). The table and
the score below are its `--table` and `--score` output; nothing in them is
updated by hand. The readings ("Reading a proof failure") are `mutate.py`'s
`RULINGS`. Earlier measurements, each a partial rerun whose rows were updated
by hand: the first 80 mutants at `c2fe428ff` (RUE-2465, with and without the
six seeds this page adds), the monitor mutants after the sharpness statements
(RUE-2485), the thirteen refusal seeds (RUE-2486), the drop-glue mutants after
`drop_glue_order` (RUE-2487), the 15 statement-vocabulary mutants at
`1b58cdc26` (RUE-2490), and RUE-2500's four sharpness counter-examples and
sixteenth vocabulary mutant, read by hand from kernel-checked refutations.
This run reproduces every one of those readings or corrects it
(["Readings the spec pass corrected"](#readings-the-spec-pass-corrected)).

## What is mutated

The semantics and the checker — `Syntax` (layer L0) and four of L1's seven
modules — **and, since RUE-2490, the statement vocabulary itself**: L0's
`Float` and L1's other three modules (README, "Layers").

* `Syntax.lean`: §3's classes (`Ty.mult`, `Attr.lift`, `Mult.join`), the
  place helpers (`Ty.atPath`, `linearResidue`, `Expr.breaks`).
* `Statics.lean`: §5's judgment `Typed` and the ownership states it threads
  (`OwnSt.join`, `residualLinear`, `fnCtx`, `armCtx`, `Ctx.joinOpt`).
* `Checker/Defs.lean`: `check`, the decision procedure the corpus verdicts
  come from.
* `Dynamics.lean`: the definitional interpreter `eval`, the drop glue and the
  run-time monitors.
* `Step.lean`: §6's small-step relation `Step` and its function `step`.
* `Soundness/Defs.lean` (RUE-2490): what `soundness` is stated over —
  `HasTy`, `ContentsTy`, `ContentsMatches`, `FrameMatches`, `EvalOk`.
* `Trace/Defs.lean` (RUE-2490): what the trace theorems are stated over —
  `Exact`, `Blocks`, `Lifo`, `NewestFirst`, `Config.Ordered`.
* `Adequacy/Defs.lean` (RUE-2490): what the adequacy theorems are stated
  over — `Config.SafeAt`, `StepsN`.
* `Float.lean` (RUE-2490): `FloatDatum.Wf`, the float counterpart of
  `InBounds`.

For the first five modules, mutating the rule and asking what a proof
notices is the point: the statements (`SPINE.md`) are fixed and taken as
written, so a wrong rule is a wrong semantics, caught (or not) by
`soundness`, `check_sound` and the rest. For the last four, mutating the
definitions the statements are written in is a different exercise
(RUE-2465's original scope card this page as "Not mutated," reasoning that
"what would notice it is a different question": the Spec layer's non-vacuity
witnesses and sharpness counter-examples, RUE-2469). RUE-2490 runs that
exercise: 15 mutants over these four modules (and RUE-2500 one more), added after RUE-2469/2485/2495
gave the page a non-vacuity witness and a sharpness counter-example for
several of the statements that rest on them, which — per this page's own
method — a weakened rule keeps proofs building past, but a weakened
statement definition should break directly, since the witness or
counter-example is stated in terms of the very thing that was weakened.
["The mutants"](#the-mutants) below has all 96; the statement vocabulary's
candidates, and how the spec pass reads them, are in
["RUE-2490: the statement vocabulary and Float"](#rue-2490-the-statement-vocabulary-and-float).

There are 96 mutants, 80 over §3, §§5.1–5.8 and §§6.2–6.12 (the semantics and
the checker) and 16 over the statement vocabulary and Float (RUE-2490's 15 and
RUE-2500's control, cited
by §7 throughout since that is where the headline statements live). They use
the issue's operators and a few classic ones:

| Operator | Mutants | What it does |
|---|---:|---|
| premise | 30 | drop a premise from a typing rule, a checker test or a statement definition (in a `Typed` rule or a definition's constructor, the premise becomes `True`, see below) |
| join | 4 | change §5.5's join: the less-moved state wins, a linear disagreement joins, the residual check goes, a diverging arm swallows the branch |
| move-copy | 2 | make a move a copy, in the statics and in the dynamics |
| drop-skip, drop-order | 9 | skip one drop (a destructor, an overwrite drop, a drop mark, a match consume) or reorder drops (destructor after fields, fields last to first, a frame's bindings first to last, an arm's payload first to last) |
| copy-check | 4 | weaken a `Copy` check |
| affine-linear | 7 | make an affine thing linear or the reverse: the class of a linear-carrying struct, the class join, `[T; 0]`, a partially moved declared-linear struct, untracked residue, an affine discard |
| bounds, off-by-one | 9 | remove or weaken a bounds trap; off by one in an array rule, a comparison, a `break`'s unwind, a repeat count; drop `FloatDatum.Wf`'s subnormal floor or its canonical-significand requirement (RUE-2490) |
| order | 3 | change evaluation order (the right-hand side of `a[i] = e` before or after the index, a binary operator's operands) or the parameters' binding order |
| trap, operand | 7 | change an arithmetic trap (wrap instead, the wrong trap kind, `MIN % -1`, `-MIN`, a float-to-int that saturates) or swap an operator's operands |
| monitor | 4 | remove one of the machine's run-time refusals (`linearLeak`, `linearOverwrite`, `linearDiscard`, `ownedUnderCopy`) |
| completeness | 4 | make the checker refuse more: an arm type, the loop-head bound, the acyclicity rounds |
| equivalent-candidate | 2 | a change believed harmless, as a control |
| vacuous (RUE-2490) | 6 | replace a whole clause or definition by `True`: `Lifo`, `NewestFirst`, `Config.Ordered`, `EvalOk`'s `.stuck` clause, and each conjunct of `Config.SafeAt` |
| wildcard (RUE-2490) | 1 | add an unconstrained constructor to an inductive relation: `Blocks` accepts any trace |
| count (RUE-2490) | 1 | weaken an exact ledger's `=` to `≤`: `Exact` |
| strengthen (RUE-2490, RUE-2500) | 3 | add a premise or drop a disjunct — the reverse of `premise` — where the strengthening could make the statement vacuous: `StepsN.step`, `Config.SafeAt`'s progress conjunct, and (RUE-2500) `ContentsMatches.owned` demanding `False`, which sits in a hypothesis |

Most mutants change the rule *and* the checker together, the way a real
mistake in the calculus or in its transcription would. Some change one side
only, on purpose: the checker alone (`index-drop-copy-checker`,
`loop-head-unverified`, `entry-params`, and others), the typing rule alone
(`loop-div-breaks`, `loop-break-div-brk`), `eval` alone
(`binop-eval-order`, `index-write-order`, `seq-affine-as-linear`) or `Step`
alone (`step-usecopy-nondet`). A dynamics mutant changes `eval`, `Step` and
`step` alike unless its row says otherwise. RUE-2490's mutants change one
definition each, since the statement vocabulary has no separate "checker
side." Each mutant is written as exact-text edits in
[`bin/mutate.py`](bin/mutate.py).

## Method

`bin/mutate.py` copies the package's sources into a scratch directory (never
the package itself), applies one mutant, and records the first of these that
fails on it, in this order:

1. **spec**: a *candidate statement* is unproved under the mutant — a
   statement of the Spec layer (the spine, the non-vacuity witnesses, the
   sharpness counter-examples, each as `RueCore.Spine` proves it) or of
   either `Glue` module, in which a mutated definition occurs where the
   mutant can make it false, and whose proof rests on a proof that fails
   ("The spec pass", below);
2. **proof**: `lake build` fails in a theorem of the proof modules (layer L2:
   `Statics/Lemmas`, `Dynamics/Lemmas`, `Step/Lemmas`, `Soundness`,
   `Checker`, `Trace`, `Adequacy`, `TraceExact`, `TraceOrder`, `Spine`, ...),
   and no candidate statement rests on the failure;
3. **witness**: the build fails only in layer L3 (`Examples`, `Witnesses`,
   `Corpus`, `Print`, `Explain`) or in an `example`;
4. **corpus**: the build succeeds, and `lake exe ruecore-corpus` (every
   seed's verdict and expected outcome) differs from the unmutated baseline;
5. **bridge**: the seeds are unchanged, but `--gen 200 --seed 7` (the
   per-lane check's generated cases) changes, and the compiler disagrees with
   the mutant on a changed case;
6. **survived**: nothing fails.

The module lists come from `RueCore/Layers.lean`'s table, the one list the
layering audit checks, so they follow the package as it changes.
`mutate.py --check` fails when a module is missing from the table, when a
mutant's edit no longer matches the sources or touches a module outside L0
and L1, and when the proofs-off or witnesses-off copy (below) leaves a
theorem, an example or a `#guard` on. It also reads the built package's
environment, and fails when a source theorem is not the environment's
theorem at that line, when an edit falls in no definition, when a mutant has
no reading, or when a reading's `stays` (below) names a statement that is not
one of the mutant's candidates.

A witness failure that is only in `Explain.lean` is recorded apart, as the
**Explain mirror**. `Explain.lean`'s explainer and trace are second copies of
`check` and `eval`, proved equal to them (`explain_result`,
`traceEval_res`). A mutant that changes one copy fails that proof, whether or
not the change means anything, so it is not a test of the definition.

A seed whose expectation the mutant leaves unchanged cannot make the bridge
disagree anew: the bridge already compares that expectation with the
compiler, and it agrees on unmutated trunk (all but the allowed red,
`array_elem_self_assign`). So step 5 runs the compiler (`scripts/rue exec`,
the comparison of the loop's `bin/verify.py`) only on the generated cases the
mutant changed, and step 4 covers the seeds.

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

**The spec pass (RUE-2499).** Neither of those passes says whether a
mutant makes a *statement* false. The statements are proved in layer L2
(`Sharp.lean`, `Nonvacuous.lean`, `Spine.lean`, both `Glue.lean`), so the
proofs-off pass turns them off, and in the first pass they sit downstream of
the first module that fails. Until RUE-2499 every statement reading was
therefore by hand, and two were wrong (`blocks-any-trace`,
`contentsmatches-moved-residue`; RUE-2490's review). So every mutant a proof
fails on is run a fourth time, in a copy built to the statements alone:

* **Candidates, by polarity.** `bin/mutate_polarity.lean` reads the built
  package's environment. A mutant's *mutation targets* are the definitions its edits
  fall in. For every theorem it finds where each target occurs in the
  statement, unfolding the package's `Prop`-valued definitions and
  inductives (a constructor's premise occurs at its inductive's polarity):
  in a conclusion (`concl`, `+`), in a hypothesis or under `¬` (`hyp`, `¬`,
  `-`), or both ways, in an `↔` or inside a term such as an argument of `=`
  (`↔`, `term`, `±`). A weakened definition makes the statement stronger at
  `-` and can only weaken it at `+`; a strengthened one the reverse. The
  sixteen statement-vocabulary mutants each have a direction (`DIRECTION` in
  `mutate.py`); a semantics or checker mutant changes its definitions both
  ways, so every occurrence counts. The statements a mutant can make false
  this way are its **candidate statements** (`mutate.py --candidates` lists them, with
  each occurrence's position and the definitions unfolded to reach it).
* **An iterated build with marker axioms.** In the copy, every theorem of
  L0-L2 whose statement does not involve a target is unchanged by the mutant,
  and every theorem whose occurrences are all at a polarity the mutant cannot
  falsify is implied by the original theorem; both get a marker axiom for a
  proof, so only what the mutant can change is rebuilt. `RueCore.Sharp.Glue`
  and `RueCore.Nonvacuous.Glue`, which import every proof module, are built;
  each proof that fails is given a failure marker of its own, and the build is
  repeated until it succeeds. The markers exist only in the scratch copy.
* **The axiom trace.** `#print axioms` of every statement then names the
  failed proofs it rests on. A candidate resting on one is **unproved**: it
  may be false for the mutant, or its proof may have broken as a script.

Keeping only the Spec proof modules on would not be enough: `soundness` is
`Spine.lean`'s alias of the L2 theorem, and `Sharp.bare_dtor` and
`.unreached` rest on `TraceOrder`'s `Blocks.not_dtor`. Before the mutants
run, the copy is built once with no edit and every mutant's targets, so that
nearly every proof is rebuilt; it must succeed with no failure, or the run
stops, since a failure there would be the copy's and not a mutant's.

A changed case that the mutant's checker accepts and its machine refuses is a
concrete counterexample to `check_sound` together with soundness. The script
lists these (`unsound` in `results.json`), and the readings below cite them.

**The completeness mutants.** `meet-never`, `first-arm-ty`,
`head-iter-bound` and `decl-cycle-rounds` make the checker refuse more. No
statement is about the checker's completeness, but the non-vacuity
witnesses each state that `checkProgram` accepts a program: refusing a
witness's program falsifies the witness. The first readings missed this
("no statement is about the checker's completeness"); the spec pass found
that `meet-never` refuses `Nonvacuous.loop`'s program ("Readings the spec
pass corrected", below). The other three still pass every witness.

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

The spec pass narrows the reading to its unproved candidates: a statement
it proves under the mutant is true for it, and a statement that is not a
candidate cannot be made false by the change. A "helper" or "holds" reading
must still say why each unproved candidate is true, either for the statement
or for each failed proof it rests on (`RULINGS`' third element, `stays`);
`--check --work`, `--table` and `--score` refuse a reading that leaves one
out. A "statement" reading names what is false, and the unproved candidates
are where to look.

**What counts as killed.** A mutant is killed when a stated property is
false for it, or when a witness, a seed or a generated case fails on it. A
proof that fails as a script, a helper lemma and the Explain mirror are
recorded but do not count on their own. Each is a difference between two
texts, not between a definition and what it should mean.

**Time.** A mutant took 15 s to 401 s, median 52 s, all passes included,
the spec pass among them (it builds in 5-30 s, since the marker axioms leave
only what the mutant can change to rebuild): 95 minutes for all 96, the four
baselines included, on this machine (an Apple-silicon laptop, one build at a
time), after a warm start of the scratch packages' `.lake`.

**Reproduce.** From a clean checkout, with the compiler buildable:

```bash
(cd docs/formal/lean && lake build)   # --check and --candidates read the environment
python3 docs/formal/lean/bin/mutate.py --check
python3 docs/formal/lean/bin/mutate.py --candidates --only blocks-any-trace
python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --compiler-root "$PWD"
python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --check   # every unproved candidate read
python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --table   # the tables below
python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --score   # the score
```

The results go to `<work>/results.json`. A rerun resumes where the previous
one stopped. `--only a,b` runs a subset, and `--redo` reruns it. The "before"
column of the score comes from a second work directory run with `--src` set
to a copy of the package without the six seeds, over the mutants whose kills
involve them, and passed to `--score` as `--before`.

## Results

### Mutation score

A mutant is killed as "Reading a proof failure" defines it: a stated property
is false for it, or a witness, a seed or a generated case fails on it. The
four equivalent mutants ("Equivalent mutants", below) are left out of the
denominator. The score is `mutate.py --score`, over the whole run and over
its two halves: the 80 mutants of the semantics and the checker, and the 16
of the statement vocabulary (RUE-2490's 15 and RUE-2500's control).

96 mutants, 4 equivalent (use-copy-moved, match-exhaustive, loop-head-unverified, entry-join-bty): the denominator is 92 (76 the semantics and the checker, 16 the statement vocabulary).

| Measure | All | The semantics and the checker | The statement vocabulary |
|---|---:|---:|---:|
| **Killed: a stated property is false, or a witness, seed or generated case fails** | 92/92 (100%) | 76/76 (100%) | 16/16 (100%) |
| A stated property is false (proof reading) | 73/92 (79%) | 59/76 (78%) | 14/16 (88%) |
| A candidate statement is unproved under the mutant (the spec pass) | 87/92 (95%) | 72/76 (95%) | 15/16 (94%) |
| A stated property or a helper lemma is false | 78/92 (85%) | 62/76 (82%) | 16/16 (100%) |
| The tests with the proofs off: witnesses, seeds, generated cases | 73/92 (79%) | 71/76 (93%) | 2/16 (12%) |
| The seeds and the bridge alone | 69/92 (75%) | 69/76 (91%) | 0/16 (0%) |
| The build or the corpus fails at all (a proof script, a helper or the Explain mirror included) | 92/92 (100%) | 76/76 (100%) | 16/16 (100%) |

- Not killed: 0
- A statement is false, and the spec pass left it unproved: 73: `use-move-partial`, `use-affine-as-copy`, `use-declared-residue`, `index-read-copy`, `index-drop-copy-checker`, `const-index-off-by-one`, `assign-overwrite`, `index-write-linear`, `drop-moved`, `seq-discard`, `join-owned-wins`, `join-linear-disagree`, `join-residual`, `join-diverge-arm`, `meet-never`, `arm-leak`, `let-leak`, `residual-declared`, `residual-untracked`, `return-leak`, `break-leak`, `loop-div-breaks`, `loop-break-div-brk`, `fn-exit-leak`, `fn-params-order`, `entry-params`, `lit-bounds`, `dbg-observable`, `repeat-copy`, `class-not-infectious`, `mult-join-meet`, `copy-struct-dtor`, `dtor-linear-field`, `dyn-move-as-copy`, `step-usecopy-nondet`, `bounds-off-by-one`, `bounds-negative`, `bounds-stuck`, `repeat-count`, `operand-swap`, `neg-no-overflow`, `binop-eval-order`, `index-write-order`, `dtor-skip`, `dtor-after-fields`, `fields-reverse`, `scope-fifo`, `payload-order`, `overwrite-no-drop`, `break-skip-local`, `seq-affine-as-linear`, `seq-droptemp-skip`, `residue-mark-skip`, `match-consume-skip`, `leak-monitor-off`, `overwrite-monitor-off`, `discard-monitor-off`, `copy-monitor-off`, `dyn-residual-declared`, `hasty-int-any-value`, `hasty-float-any-value`, `evalok-stuck-ok`, `contentsmatches-moved-residue`, `exact-at-most`, `blocks-any-trace`, `lifo-vacuous`, `safeat-progress-vacuous`, `safeat-typing-vacuous`, `safeat-terminal-only`, `stepsn-one-step-only`, `float-wf-no-emin`, `float-wf-noncanonical`, `contentsmatches-owned-false`
- A statement is false, and the spec pass left no candidate unproved: 0
- The spec pass left a candidate unproved, and the reading says it holds (`stays`): 18: `use-copy-moved`, `use-move-dtor`, `use-move-rootidx`, `assign-array-ok`, `drop-residual-below`, `match-exhaustive`, `arm-payload-mutable`, `loop-head-unverified`, `breaks-nested`, `zero-array-linear`, `decl-cycle-rounds`, `entry-join-bty`, `overflow-wrap`, `divzero-kind`, `rem-min-overflow`, `cast-kind`, `float-to-int-saturate`, `newestfirst-vacuous`
- Killed by a proof script only (every statement holds): 0
- Killed by a helper lemma only: 0
- Missed by the tests with the proofs off: 19: `loop-div-breaks`, `loop-break-div-brk`, `entry-params`, `step-usecopy-nondet`, `seq-droptemp-skip`, `hasty-int-any-value`, `hasty-float-any-value`, `evalok-stuck-ok`, `contentsmatches-moved-residue`, `exact-at-most`, `blocks-any-trace`, `lifo-vacuous`, `safeat-progress-vacuous`, `safeat-typing-vacuous`, `safeat-terminal-only`, `stepsn-one-step-only`, `float-wf-no-emin`, `float-wf-noncanonical`, `contentsmatches-owned-false`
- Missed by the seeds and the bridge: 23: `loop-div-breaks`, `loop-break-div-brk`, `entry-params`, `step-usecopy-nondet`, `seq-droptemp-skip`, `residue-mark-skip`, `match-consume-skip`, `hasty-int-any-value`, `hasty-float-any-value`, `evalok-stuck-ok`, `contentsmatches-moved-residue`, `exact-at-most`, `blocks-any-trace`, `lifo-vacuous`, `newestfirst-vacuous`, `ordered-vacuous`, `safeat-progress-vacuous`, `safeat-typing-vacuous`, `safeat-terminal-only`, `stepsn-one-step-only`, `float-wf-no-emin`, `float-wf-noncanonical`, `contentsmatches-owned-false`
- Failed in the Explain mirror: 6: `use-copy-moved`, `match-exhaustive`, `loop-head-unverified`, `repeat-count`, `seq-affine-as-linear`, `seq-droptemp-skip`

What the rows mean:

* **Killed** is the mutation score. Every non-equivalent mutant is killed.
* **A candidate statement is unproved (the spec pass)** is RUE-2499's
  measure: a statement the mutant can make false by polarity rests on a
  proof that fails under it. It is not a kill by itself, since a proof can
  fail as a script. It holds for 87 of the 92, and the readings say which of
  those 87 are false: all but 14, and for each of those 14 the reading says
  why every unproved candidate still holds (the list above names 18 with the
  four equivalent mutants). The other five are the four mutants no proof
  fails on (`assign-immutable`, `first-arm-ty`, `head-iter-bound`,
  `gt-off-by-one`: every statement is proved under them, and a witness kills
  each) and `ordered-vacuous`, which has no candidate: `Config.Ordered`
  occurs in no statement.
* **A stated property is false** is the readings'. Every one of those 73
  rests on the spec pass: none reads a statement false that the pass proved.
* **The tests with the proofs off** and **the seeds and the bridge alone**
  are the other two passes. They see 71 and 69 of the 76 semantics mutants,
  and 2 and 0 of the 16 vocabulary mutants: a wrong statement definition
  changes no program's behaviour, so only a statement can notice it.
* **The build or the corpus fails at all** is 100%, as it was before the
  seeds, and is not a mutation score: every equivalent mutant fails a proof
  script too, and three of them the Explain mirror.

The historical comparison this page was written for, RUE-2465's six seeds,
was measured at `c2fe428ff` on the first 80 mutants: without the seeds 73 of
76 were killed (`breaks-nested` by a proof script only, `arm-payload-mutable`
by a helper only, `assign-immutable` by the Explain mirror only), with them
all 76; the seeds and the bridge alone went from 50 to 69 of 76. That run's
work directory is not rerun here (`--before` recomputes the column from one).

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
* **The dynamics' functional content is the tests'.** For the §6.4
  arithmetic mutants (wraparound instead of a trap, the wrong trap kind,
  `MIN % -1`, a saturating float-to-int) every statement holds, because each
  result is still safe. Swapped operands (`operand-swap`) keep the safety
  spine true too, but falsify `Nonvacuous.loop`, whose loop counts to three;
  and a negative index (`bounds-negative`) is not safe after all: at an empty
  array it reads a missing element (both found by the spec pass, "Readings the
  spec pass corrected").

  Witnesses and seeds kill them all: `overflow`, `div_zero`,
  `i8_rem_min_by_neg_one`, `float_to_int_trap_*`,
  `array_dyn_write_trap_negative`, and others. This is by design: the Spec
  states safety, not functional correctness, and the corpus compares the
  functional content with the compiler.
* **§6.11's drop order is fixed by a statement since RUE-2487.** Three
  drop-glue mutants change `dropContents` and `dropEvents` together:
  - `dtor-skip`, where no destructor runs;
  - `dtor-after-fields`;
  - `fields-reverse`.

  Until RUE-2487 every statement held for each of them: `drop_order`'s
  `Blocks` is written in terms of `dropEvents`, so it follows whatever
  `dropEvents` says, and `no_double_free` counts at most one destructor,
  which zero satisfies. `drop_glue_order` states the order in §6.11's own
  terms: a finished run's trace is in the grammar `GlueBlocks`, whose drop
  blocks are the rules `DropGlue`, written from §6.11's equations over the
  declarations and not through `dropEvents`. Each mutant's machine emits a
  trace that grammar rejects, so each now falsifies a statement. The
  rejected traces are `Witnesses.lean`'s `glue_dtorSkipped_rejected`,
  `glue_dtorAfterFields_rejected` and `glue_fieldsSwapped_rejected`. The
  falsity was also checked in the kernel on the proofs-off copy of each
  mutant: a checked program the mutant's `stepN` runs to such a trace
  refutes `Spec.drop_glue_order_stmt`, resting only on `checkProgram_sound`
  and `stepN_steps`, whose proofs that copy turns off and which the
  mutants do not touch. The build still stops first at a proof script
  upstream (`dropContents_events`, `dropContents_struct_events`,
  `dropEventsList_eq_flatten`), before `drop_glue_order`'s own proof
  (`dropContents_glue`). The witnesses and 90, 2 and 24 seeds kill them
  through their destructor lines. The frame-exit and match-exit order
  mutants (`scope-fifo`, `payload-order`) falsify `drop_order`'s `Lifo`.
* **The monitors are pinned by the sharpness statements.** The linear
  theorems say the machine never refuses a checked program, and a machine
  that never refuses meets that: removing the `linearLeak`,
  `linearOverwrite`, `linearDiscard` or `ownedUnderCopy` refusal, or
  weakening the leak monitor (`dyn-residual-declared`), leaves every spine
  statement true. This was the red-team log's R3, measured. Since RUE-2485
  each of the five falsifies a Spec statement, a sharpness counter-example
  that states the refusal of an unchecked program (`Sharp.leak`,
  `Sharp.overwrite`, `Sharp.discard`, `Sharp.discard_loop`, `Sharp.copy`;
  lean/README "Sharpness counter-examples"). For three the build stops in
  `Sharp.lean`; for `discard-monitor-off` and `copy-monitor-off` it stops
  first at a helper upstream (`eval_succ`, `Cons.intro`), and the Sharp
  statements' falsity was checked by hand. The spec pass now shows the same
  automatically: each of the five leaves its Sharp statement unproved (for
  `leak-monitor-off`, `overwrite-monitor-off` and `dyn-residual-declared`
  nothing else among the spine statements). With the mutated `eval`, the
  discard program panics, the discarding loop exhausts its fuel, and
  `dupProgram` returns with one destructor run twice, none of them the
  refusal the statements state. The refusal witnesses in `Examples.lean`,
  `Corpus.lean` and `Trace.lean` still kill all five with the proofs off,
  and the seeds whose expected outcome is that refusal kill four.
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
  RUE-2486 seeds all of the third group, leaving **7**:
  - 4 no corpus case can show: `loop-div-breaks`, `loop-break-div-brk`,
    `step-usecopy-nondet` and `entry-params`. Each falsifies a stated
    property.
  - 3 are trace-only: `seq-droptemp-skip`, `residue-mark-skip` and
    `match-consume-skip`. Each falsifies `Exact`.
  - 13 were refusals that a witness proved but no seed exported, so the
    compiler's matching refusal was never compared. Twelve were in
    `Examples.lean`: `use-move-rootidx`, `index-read-copy`,
    `index-drop-copy-checker`, `const-index-off-by-one`, `index-write-linear`,
    `residual-declared`, `residual-untracked`, `lit-bounds`,
    `dbg-observable`, `repeat-copy`, `copy-struct-dtor` and
    `dtor-linear-field`. The thirteenth, `copy-monitor-off`, was
    `Trace.lean`'s `ownedUnderCopy` witness.

    RUE-2486's thirteen new seeds — part 1's `use_move_rootidx`,
    `index_read_copy`, `index_drop_copy_checker`, `const_index_off_by_one`,
    `index_write_linear`, `residual_declared`, `residual_untracked`, and
    part 2's `lit_out_of_range`, `dbg_aggregate`, `repeat_copy_affine`,
    `copy_struct_dtor`, `dtor_linear_field`, `copy_monitor_off` — each
    reproduce their mutant's own witness as a corpus case (`Examples.lean`
    for twelve, `Trace.lean`'s `dupProgram` for `copy-monitor-off`), so all
    thirteen mutants above are now killed at the corpus level (the "Corpus
    and bridge alone" column of "The mutants", below), the same reading a
    proof already gave each of them. Proposed issue 1 (below) is done.

    The remaining 7 that get past the seeds and the bridge together are the
    4 no corpus case can show plus the 3 trace-only ones.

### The mutants

* "Killed first by": the first failure in the order of "Method". For a
  `spec` kill, the unproved candidate statements, spine statements first,
  each with the positions of the mutated definition in it, then how many more
  and how many `Glue` theorems; otherwise the theorem or example that failed
  first, or the seeds whose outcome changed.
* "Spec pass": the unproved candidates over all candidates, and the failed
  proofs they rest on. With no spec pass (no proof failed), every statement
  is proved, so none is unproved.
* "Without the proofs": the second pass, run only after a proof failure
  ("—" otherwise).
* "Corpus and bridge alone": the third pass, run after a witness or Explain
  mirror failure. "(same)" means the earlier pass already ended at the
  corpus, the bridge or "survived".
* "Stated properties" and "Why": the mutant's reading ("Reading a proof
  failure") and its reason.
* "s": the mutant's wall time in seconds, all passes included.

A line number is given only for a module the pass leaves as written.
`mutate.py --table` prints this table from `results.json`.

| # | Mutant | § | Rule | Operator | Killed first by | Spec pass: unproved/candidates | Without the proofs | Corpus and bridge alone | Stated properties | Why | s |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `use-move-partial` | §5.1 | (Use-Move) | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 1179) | corpus: `array_zero_length_moved_twice`, `enum_matched_twice_moving` +1 | a stated property is false | moves a partially moved aggregate whole; the machine meets the hole (seed `partial_then_whole`): `soundness`, `check_sound` | 51 |
| 2 | `use-copy-moved` | §5.1 | (Use-Copy) | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | Explain mirror: `explain_result` (`Explain.lean`) | survived | equivalent | a `Copy` place is never `MovedOut` in a reachable state: a `Copy` `@drop` moves nothing and a `Copy` value is never a hole | 79 |
| 3 | `use-move-dtor` | §5.1 | (Use-Move) 3.9:34 | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `Nonvacuous.array` (concl, term) +34 more +41 Glue | 78/110; via `check_sound` | witness: `Examples.lean` example (l. 2764) | corpus: `partial_under_dtor` | every statement holds | E0456 is a static discipline with no dynamic counterpart: the machine runs the program (`partial_under_dtor`), so no stated property is false | 52 |
| 4 | `use-move-rootidx` | §5.1 | (Use-Move) 3.8:68 | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `Nonvacuous.array` (concl, term) +34 more +41 Glue | 78/110; via `check_sound` | witness: `Examples.lean` example (l. 1262) | corpus: `use_move_rootidx` | every statement holds | a static discipline (`3.8:68`): the machine moves the element out and drops the rest path by path, without a refusal | 52 |
| 5 | `use-affine-as-copy` | §5.1 | (Use-Copy)/(Use-Move) | move-copy | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 1170) | corpus: `array_dyn_write_after_field_move`, `array_elem_reinit` +7 | a stated property is false | an affine use leaves the place `Owned`, so a second use is accepted and the machine meets a hole (`use_after_move`): `soundness` | 50 |
| 6 | `use-declared-residue` | §5.1 | (Use-Declared-Linear-Destructure) | premise | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `splitResidue_ok` | witness: `Examples.lean` example (l. 2907) | corpus: `destructure_linear_residue` | a stated property is false | accepts a destructure that strands a linear sibling; the machine refuses with `linearLeak` (`destructure_linear_residue`): `soundness` | 54 |
| 7 | `index-read-copy` | §5.1 | (Use-Untrackable-Dynamic-Copy) | copy-check | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 1275) | corpus: `index_read_copy` | a stated property is false | accepts a dynamic-index read of a non-Copy element, which the machine refuses (`typeConfusion`): `soundness` | 54 |
| 8 | `index-drop-copy-checker` | §5.1 | (Use-Untrackable-Dynamic-Copy), @drop | copy-check | spec: `checkProgram_sound` (hyp/term), `check_sound` (hyp/term), `Nonvacuous.array` (term) +19 more +1 Glue | 23/31; via `check_sound` | witness: `Examples.lean` example (l. 1305) | corpus: `index_drop_copy_checker` | a stated property is false | the checker accepts `@drop(a[i])` of a non-Copy element, which no `Typed` rule derives: `check_sound` | 46 |
| 9 | `const-index-off-by-one` | §5.1 | Ty.atPath (7.1:9) | off-by-one | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `Contents.resolveDyn_ok`, `OwnSt.setAt_wf`, `Ty.fieldAt_inv` | witness: `Examples.lean` example (l. 1276) | corpus: `const_index_off_by_one` | a stated property is false | a constant index equal to the length types, and the machine's read fails (`typeConfusion`): `soundness` | 56 |
| 10 | `assign-overwrite` | §5.2 | (Assign) 3.8:77 | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness`, `Sharp.overwrite` | witness: `Examples.lean` example (l. 3160) | corpus: `linear_overwrite`, `overwrite_field_past_partial_linear` +1 | a stated property is false | accepts overwriting a live linear place; the machine refuses with `linearOverwrite` (`linear_overwrite`): `soundness` | 55 |
| 11 | `assign-array-ok` | §5.2 | (Assign) 3.8:72 | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `Nonvacuous.array` (concl, term) +34 more +41 Glue | 78/110; via `check_sound` | witness: `Examples.lean` example (l. 1170) | corpus: `array_elem_reinit`, `array_elem_self_assign` | every statement holds | `soundness` does not use the premise (`assignArrayOk`'s doc-comment): the write it refuses runs without a refusal | 52 |
| 12 | `assign-immutable` | §5.2 | (Assign) mut | premise | witness: `Examples.lean` example (l. 4141) | 0/110 | — | corpus: `assign_immutable`, `match_payload_assign` | every statement holds | mutability is not a safety property: the machine performs the write | 40 |
| 13 | `index-write-linear` | §5.2 | (Assign) at a dynamic index | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 1278) | corpus: `index_write_linear` | a stated property is false | accepts writing a linear element through a dynamic index; the machine refuses with `linearOverwrite`: `soundness` | 50 |
| 14 | `drop-residual-below` | §5.3 | (@Drop) E0406 | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `Nonvacuous.array` (concl, term) +34 more +41 Glue | 78/110; via `check_sound` | witness: `Examples.lean` example (l. 2769) | corpus: `linear_field_stranded` | every statement holds | E0406's residual side condition has no dynamic counterpart (`linear_field_stranded` runs): no stated property is false | 52 |
| 15 | `drop-moved` | §5.3 | (@Drop) | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 3149) | corpus: `loop_moved_prev_iteration`, `loop_nested_move_outer` +1 | a stated property is false | accepts `@drop` of a moved-out place; the machine meets the hole (`use_after_move`): `soundness` | 51 |
| 16 | `seq-discard` | §5.3 | (Seq) 3.8:64 | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness`, `Sharp.discard` +1 | witness: `Corpus.lean` example (l. 1033) | corpus: `linear_temporary_discarded` | a stated property is false | accepts discarding a linear value; the machine refuses with `linearDiscard` (`linear_temporary_discarded`): `soundness` | 53 |
| 17 | `join-owned-wins` | §5.5 | join | join | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `OwnSt.join_matches`, `OwnSt.join_movedOut_left`, `OwnSt.join_movedOut_right` +2 | witness: `Examples.lean` example (l. 3240) | corpus: `loop_moved_prev_iteration`, `loop_nested_move_outer` | a stated property is false | the join keeps `Owned` where one arm moved, so a later use is accepted and meets the hole (`loop_moved_prev_iteration`): `soundness` | 55 |
| 18 | `join-linear-disagree` | §5.5 | join 3.8:50 (E0443) | join | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `OwnSt.join_movedOut_owned_eq`, `ownedJoinOk_matches` | witness: `Examples.lean` example (l. 1228) | corpus: `array_linear_elem_one_path`, `destructure_one_arm` +6 | a stated property is false | a linear path `Owned` on one arm and `MovedOut` on the other joins; the run leaks (`linear_half_consumed`): `soundness` | 54 |
| 19 | `join-residual` | §5.5 | join (residual reading) | join | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `OwnSt.join_matches`, `OwnSt.join_movedOut_left`, `OwnSt.join_movedOut_right` | witness: `Examples.lean` example (l. 4130) | corpus: `join_moved_vs_partial_linear` | a stated property is false | accepts a leak through the join; `join_moved_vs_partial_linear` is accepted and refused with `linearLeak`: `soundness` | 53 |
| 20 | `join-diverge-arm` | §5.5/§5.7 | join over Ω (Sub-Never) | join | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `Ctx.joinOpt_skel`, `Ctx.joinOpt_wf`, `Matches.joinOpt_right` | witness: `Examples.lean` example (l. 3872) | corpus: `loop_nested_move_outer` | a stated property is false | one diverging arm makes the branch diverge, so what follows is not typed but runs (`loop_nested_move_outer` refused): `soundness` | 51 |
| 21 | `meet-never` | §5.5/§5.7 | (If) arm type, (Sub-Never) | completeness | spec: `checkProgram_sound` (hyp/term), `check_sound` (hyp/term), `Nonvacuous.array` (term) +19 more +1 Glue | 23/31; via `CTy.meet_fits`, `Nonvacuous.loop` | witness: `Examples.lean` example (l. 3768) | corpus: `if_panic_arm_linear`, `if_return_arm_affine` +8 | a stated property is false | `Nonvacuous.loop` is false (RUE-2499's spec pass; kernel-checked refutation, scratch/rue-2499/refute/meet-never-T.lean): its loop body's `if i >= 3 { break }` has a diverging arm, which no longer meets `()`, so `checkProgram` refuses the witness. Refusing more falsifies a non-vacuity witness, which states an acceptance; the first reading, 'no statement is about the checker's completeness', missed the witnesses | 50 |
| 22 | `first-arm-ty` | §5.5 | (Match) arm type | completeness | witness: `Examples.lean` example (l. 2808) | 0/31 | — | corpus: `enum_return_past_payload`, `match_never_first_arm` +1 | every statement holds | refuses more: no statement is about the checker's completeness | 26 |
| 23 | `match-exhaustive` | §5.5 | (Match) exhaustiveness | equivalent-candidate | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | Explain mirror: `explain_result` (`Explain.lean`) | survived | equivalent | `TypedArms` and `checkArms` walk arms and variants in step and fail on a length mismatch, so the premise is implied | 80 |
| 24 | `arm-leak` | §5.5/§5.6 | (Match) arm scope exit | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `TypedArms.at_index`, `checkArms_sound` | witness: `Examples.lean` example (l. 2848) | corpus: `enum_arm_leaks_payload` | a stated property is false | accepts an arm that ends with a live linear payload binding; the machine refuses with `linearLeak` (`enum_arm_leaks_payload`): `soundness` | 52 |
| 25 | `arm-payload-mutable` | §5.5 | (Match) payload binders | premise | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `soundness` | witness: `Examples.lean` example (l. 4141) | corpus: `match_payload_assign` | only a helper is false | only `Ctx.skel_armCtx`, which restates `armCtx`; mutability is not a safety property | 50 |
| 26 | `let-leak` | §5.6 | (Let) scope exit | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness`, `Sharp.leak` | witness: `Examples.lean` example (l. 1266) | corpus: `linear_leaked`, `residual_declared` +2 | a stated property is false | accepts a `let` that ends with a live linear binding; `linearLeak` (`linear_leaked`): `soundness` | 52 |
| 27 | `residual-declared` | §5.6 | residual-linear (3.8:74) | affine-linear | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `ContentsMatches.residualLinear_false` | witness: `Examples.lean` example (l. 2976) | corpus: `residual_declared` | a stated property is false | a partially moved declared-linear struct owes nothing, so its leak is accepted; the machine's monitor still refuses: `soundness` | 55 |
| 28 | `residual-untracked` | §5.6 | residual-linear, untracked residue | affine-linear | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `ContentsMatchesList.residualLinearList_false` | witness: `Examples.lean` example (l. 1266) | corpus: `residual_untracked` | a stated property is false | untouched linear slots owe nothing, so their leak is accepted; the machine refuses: `soundness` | 59 |
| 29 | `return-leak` | §5.7 | (Return-Value) | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 3153) | corpus: `return_past_linear` | a stated property is false | accepts a `return` past a live linear binding; `linearLeak` (`return_past_linear`): `soundness` | 56 |
| 30 | `break-leak` | §5.7 | (Loop-Break) loop locals | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 3932) | corpus: `loop_break_past_linear` | a stated property is false | accepts a `break` past a live linear loop-local; `linearLeak` (`loop_break_past_linear`): `soundness` | 52 |
| 31 | `loop-div-breaks` | §5.7 | (Loop-Div) | premise | spec: `drop_exactly_once` (hyp), `drop_glue_order` (hyp), `drop_order` (hyp) +50 more +32 Glue | 85/109; via `soundness` | survived | (same) | a stated property is false | the rules type a loop that breaks as diverging, so what follows is not typed but runs: `soundness` | 75 |
| 32 | `loop-break-div-brk` | §5.7 | (Loop-Break), no reachable exit | premise | spec: `checkProgram_sound` (concl), `check_sound` (concl), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/109; via `check_sound`, `soundness` | survived | (same) | a stated property is false | the rules type a loop with a reachable `break` as diverging: `soundness` | 76 |
| 33 | `loop-head-unverified` | §5.7 | loop head (LoopHead) | premise | spec: `checkProgram_sound` (hyp/term), `check_sound` (hyp/term), `Nonvacuous.array` (term) +19 more +1 Glue | 23/31; via `check_sound` | Explain mirror: `explain_result` (`Explain.lean`) | survived | equivalent | `headIter` returns a candidate only when one more step leaves it unchanged, and `check` is deterministic, so the re-check always passes | 79 |
| 34 | `head-iter-bound` | §5.7 | loop head iteration | completeness | witness: `Examples.lean` example (l. 3856) | 0/31 | — | corpus: `loop_reassign_then_move` | every statement holds | refuses more: no statement is about the checker's completeness | 27 |
| 35 | `breaks-nested` | §5.7 | Expr.breaks | premise | spec: `drop_exactly_once` (hyp/term), `rest_exactly_once` (hyp/term) | 2/110; via `eval_quiet` | witness: `Examples.lean` example (l. 4155) | corpus: `loop_inner_break_outer_return` | every statement holds | refuses more: the outer loop is typed `unit` rather than `never` | 67 |
| 36 | `fn-exit-leak` | §5.8 | (Fn) exit edge | premise | spec: `checkProgram_sound` (concl, hyp/term), `drop_exactly_once` (hyp), `drop_glue_order` (hyp) +62 more +36 Glue | 101/104; via `checkFn_sound`, `soundness` | witness: `Examples.lean` example (l. 3154) | corpus: `linear_param_leaked` | a stated property is false | accepts a function body that ends with a live linear parameter; `linearLeak` (`linear_param_leaked`): `soundness` | 49 |
| 37 | `fn-params-order` | §5.8 | (Fn) entry context | order | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +27 Glue | 80/104; via `soundness` | witness: `Examples.lean` example (l. 2998) | corpus: `params_two_types` | a stated property is false | a body is typed against its parameters in the wrong order, so it runs on values of other types: `soundness` | 44 |
| 38 | `entry-params` | §6.12 | top-level main() | premise | spec: `checkProgram_sound` (hyp/term), `Nonvacuous.array` (term), `Nonvacuous.diverges` (term) +8 more | 11/20; via `checkProgram_sound` | survived | (same) | a stated property is false | `checkProgram` accepts an entry point with parameters, which `ProgramTyped` excludes: `checkProgram_sound` | 66 |
| 39 | `lit-bounds` | §5.8 | (Lit) | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 3228) | corpus: `lit_out_of_range` | a stated property is false | an out-of-range literal types, and `HasTy.int` requires `InBounds`: `soundness` | 50 |
| 40 | `dbg-observable` | §5.8 | (Dbg) | premise | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 3146) | corpus: `dbg_aggregate` | a stated property is false | `@dbg` of an aggregate types; the machine refuses (`typeConfusion`): `soundness` | 50 |
| 41 | `repeat-copy` | §5.8 | array repeat (7.1:36) | copy-check | spec: `checkProgram_sound` (concl, hyp/term), `check_sound` (concl, hyp/term), `drop_exactly_once` (hyp) +63 more +41 Glue | 107/110; via `check_sound`, `soundness` | witness: `Examples.lean` example (l. 1274) | corpus: `repeat_copy_affine` | a stated property is false | `[e; n]` of a non-Copy element types; the machine refuses (`typeConfusion`): `soundness` | 50 |
| 42 | `class-not-infectious` | §3 | class of a struct (Attr.lift) | affine-linear | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +27 Glue | 80/104; via `StructDecl.Wf.field_copy`, `StructDecl.Wf.field_not_linear`, `WfDecls.dtorNotCopy` | witness: `Examples.lean` example (l. 1057) | corpus: `affine_explicit_drop`, `affine_overwrite` +94 | a stated property is false | a linear-carrying struct is `Affine`, so dropping it is accepted and the machine's monitor refuses the live linear field: `soundness` | 52 |
| 43 | `mult-join-meet` | §3 | class join | affine-linear | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +61 more +37 Glue | 101/104; via `Nonvacuous.array`, `Nonvacuous.diverges_drop`, `Nonvacuous.dtor` +32 | witness: `Examples.lean` example (l. 1057) | corpus: `affine_explicit_drop`, `affine_overwrite` +94 | a stated property is false | the class join takes the lesser class, so a linear-carrying struct is not `Linear`; as `class-not-infectious`: `soundness` | 55 |
| 44 | `zero-array-linear` | §3 | class of [T; 0] (3.8:74) | affine-linear | spec: `drop_exactly_once` (hyp/term, term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +67 Glue | 120/169; via `Ty.array_copy_elem` | witness: `Print.lean` example (l. 729) | bridge: `gen_7_185` | only a helper is false | only `Ty.array_mult_linear`, which restates `Ty.mult`; `[T; 0]` being `Linear` refuses more | 69 |
| 45 | `copy-struct-dtor` | §3 | @copy struct (3.9:31) | premise | spec: `checkProgram_sound` (hyp/term), `Nonvacuous.array` (term), `Nonvacuous.diverges` (term) +10 more | 13/20; via `checkStructDecl_sound`, `Sharp.bare_dtor`, `Sharp.double_drop` | witness: `Examples.lean` example (l. 3186) | corpus: `copy_struct_dtor` | a stated property is false | accepts a `@copy` struct with a destructor, which `WfDecls` excludes: `checkProgram_sound` | 50 |
| 46 | `dtor-linear-field` | §3 | destructor with a linear field (3.9:44) | premise | spec: `checkProgram_sound` (hyp/term), `Nonvacuous.array` (term), `Nonvacuous.diverges` (term) +8 more | 11/20; via `checkStructDecl_sound` | witness: `Examples.lean` example (l. 3193) | corpus: `dtor_linear_field` | a stated property is false | accepts a destructor-bearing struct with a linear field, which `WfDecls` excludes: `checkProgram_sound` | 27 |
| 47 | `decl-cycle-rounds` | §3 | acyclicity 3.0:5 (E0483) | completeness | spec: `checkProgram_sound` (hyp/term), `Nonvacuous.array` (term), `Nonvacuous.diverges` (term) +8 more | 11/20; via `checkNoCycle_sound` | bridge: `gen_7_127` +16 | (same) | every statement holds | refuses more: the checker's own completeness is in no statement, and every witness's declarations still pass | 69 |
| 48 | `entry-join-bty` | §5.5 | Entry.join | equivalent-candidate | spec: `drop_exactly_once` (hyp/term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +50 more +32 Glue | 85/110; via `Entry.join_absorb`, `Entry.join_matches_left`, `Entry.join_matches_right` +2 | survived | (same) | equivalent | every join is of two entries with one skeleton, so the two declared types are equal | 77 |
| 49 | `dyn-move-as-copy` | §6.3 | (D-Use-Move) | move-copy | spec: `Config.stuck_iff` (↔/term, ↔/¬), `Config.trichotomy` (concl, term), `drop_exactly_once` (term) +65 more +72 Glue | 140/161; via `stepEval_complete`, `sim_use`, `soundness` +4 | witness: `Examples.lean` example (l. 1128) | corpus: `array_dyn_read_after_sibling_move`, `array_dyn_write_after_field_move` +25 | a stated property is false | an affine use copies, so both copies drop and a destructor runs twice: `no_double_free` | 53 |
| 50 | `step-usecopy-nondet` | §6.3 | (D-Use-Copy), Step only | copy-check | spec: `Config.stuck_iff` (↔/¬), `Config.trichotomy` (concl), `Step.det` (hyp) +45 more +42 Glue | 90/92; via `Step.step_eq`, `sim_use` | survived | (same) | a stated property is false | two `Step` rules apply to one non-Copy use: `Step.det` | 71 |
| 51 | `bounds-off-by-one` | §6.5 | (D-Index-Trap) | off-by-one | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +49 more +63 Glue | 115/161; via `inBoundsIdx_eq_true` | witness: `Examples.lean` example (l. 1175) | corpus: `array_bounds_trap_at_len`, `array_zero_length_dyn_trap` +1 | a stated property is false | an index equal to the length passes the check and the read fails (`typeConfusion`): `soundness` | 37 |
| 52 | `bounds-negative` | §6.5 | (D-Index-Trap) | bounds | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +49 more +63 Glue | 115/161; via `Contents.resolveDyn_ok` | witness: `Examples.lean` example (l. 1106) | corpus: `array_dyn_write_trap`, `array_dyn_write_trap_negative` | a stated property is false | `no_violation`, `soundness` and the rest of the safety spine are false (RUE-2499's spec pass; kernel-checked refutation, scratch/rue-2499/refute/bounds-negative-T.lean): `let a: [i64; 0] = []; a[-1]` is checked, `-1 < 0` passes the test, and element `0` of an empty array is a `typeConfusion`. The first reading, 'a negative index reads element 0', missed the empty array | 40 |
| 53 | `bounds-stuck` | §6.5 | (D-Index-Trap) | bounds | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +52 more +64 Glue | 119/161; via `soundness`, `Retire.dynPlace_ne_uad` | witness: `Examples.lean` example (l. 1101) | corpus: `array_bounds_trap`, `array_bounds_trap_at_len` +6 | a stated property is false | an out-of-range index is a stuck state: `soundness` | 39 |
| 54 | `repeat-count` | §6.5 | array repeat | off-by-one | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp), `drop_order` (hyp) +51 more +63 Glue | 117/161; via `soundness`, `eval_conserves`, `rest_step` | Explain mirror: `traceEval_res` (`Explain.lean`) | bridge: `gen_7_156` +2 | a stated property is false | `[v; n]` builds `n + 1` elements, not a value of `[T; n]`: `soundness` | 81 |
| 55 | `overflow-wrap` | §6.4 | (D-Arith-Trap) | trap | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +51 more +63 Glue | 117/161; via `intResult_res`, `intResult_scalar` | witness: `Examples.lean` example (l. 3370) | corpus: `i64_min_times_neg1`, `i8_div_min_by_neg_one` +4 | every statement holds | wraparound yields an in-range value: safe, and the spine states safety, not the arithmetic | 42 |
| 56 | `divzero-kind` | §6.4 | (D-Div-Trap) | trap | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +49 more +63 Glue | 115/161; via `binOpInt_res` | witness: `Examples.lean` example (l. 3411) | corpus: `dbg_before_trap`, `div_zero` +1 | every statement holds | a trap of the wrong kind is still a defined trap | 40 |
| 57 | `rem-min-overflow` | §6.4 | (D-Div-Trap), MIN % -1 | trap | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +49 more +63 Glue | 115/161; via `binOpInt_res` | witness: `Examples.lean` example (l. 3378) | corpus: `i8_rem_min_by_neg_one` | every statement holds | `MIN % -1` yields `0`, an in-range value | 40 |
| 58 | `operand-swap` | §6.4 | (D-Arith) | operand | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +49 more +63 Glue | 115/161; via `evalBinOp_res`, `Nonvacuous.loop` | witness: `Examples.lean` example (l. 3374) | corpus: the export aborts (stack overflow) | a stated property is false | `Nonvacuous.loop` is false (RUE-2499's spec pass; kernel-checked refutation, scratch/rue-2499/refute/operand-swap-T.lean): `i >= 3` runs as `3 >= i`, so the loop breaks on its first turn and no destructor runs, short of the three it states. Swapped operands still yield a typed value or a trap (`evalBinOp_res` holds), so the safety spine stays true; the witness pins the arithmetic | 401 |
| 59 | `gt-off-by-one` | §6.4 | (D-Ord) | off-by-one | witness: `Witnesses.lean` example (l. 295) | 0/161 | — | corpus: `loop_break_past_local`, `loop_reassign_then_move` | every statement holds | `>` as `>=` still yields a `bool` | 54 |
| 60 | `neg-no-overflow` | §6.4 | (D-Neg) | trap | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +49 more +63 Glue | 115/161; via `evalUnOp_int_res` | witness: `Examples.lean` example (l. 4162) | corpus: `i8_neg_min` | a stated property is false | `-MIN` yields the out-of-range `128` at `i8`, and `HasTy.int` requires `InBounds`: `soundness` | 40 |
| 61 | `cast-kind` | §6.4 | (D-Int-Cast-Trap) | trap | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +49 more +63 Glue | 115/161; via `evalIntCast_res` | witness: `Examples.lean` example (l. 3388) | corpus: `int_cast_out_of_range` | only a helper is false | only `evalIntCast_res`, which names the trap kind; a trap of the wrong kind is still a defined trap | 40 |
| 62 | `float-to-int-saturate` | §6.4 | (D-Float-To-Int) | trap | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +49 more +63 Glue | 115/161; via `evalFintrin_float_res` | witness: `Examples.lean` example (l. 3597) | corpus: `float_to_int_trap_inf`, `float_to_int_trap_nan` +1 | every statement holds | an out-of-range float-to-int yields `0`, an in-range value | 40 |
| 63 | `binop-eval-order` | §6.2 | evaluation order, eval only | order | spec: `drop_exactly_once` (term), `dtor_once` (term), `eval_complete` (term) +45 more +45 Glue | 93/117; via `sim_binop`, `soundness`, `eval_succ` +7 | witness: `Examples.lean` example (l. 1732) | bridge: `gen_7_118` +1 | a stated property is false | `eval` runs the right operand first and `Step` the left, so they disagree on a program with effects in both: `eval_sound`, `soundness` | 46 |
| 64 | `index-write-order` | §6.2 | evaluation order, eval only | order | spec: `drop_exactly_once` (term), `dtor_once` (term), `eval_complete` (term) +45 more +45 Glue | 93/117; via `sim_indexWrite`, `soundness`, `eval_succ` +7 | witness: `Examples.lean` example (l. 1702) | corpus: `array_dyn_write_rhs_first` | a stated property is false | `eval` runs the index first and `Step` the right-hand side: `eval_sound`, `soundness` | 43 |
| 65 | `dtor-skip` | §6.11 | drop glue: destructor | drop-skip | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term, term) +53 more +63 Glue | 119/161; via `Nonvacuous.array`, `Nonvacuous.diverges_drop`, `Nonvacuous.dtor` +13 | witness: `Examples.lean` example (l. 1068) | corpus: `affine_explicit_drop`, `affine_overwrite` +88 | a stated property is false | `drop_glue_order` is false: a destructor-bearing struct is dropped with no `dtor` event, which `GlueBlocks` rejects (`glue_dtorSkipped_rejected`; RUE-2487) | 49 |
| 66 | `dtor-after-fields` | §6.11 | drop glue order (3.9:15) | drop-order | spec: `drop_glue_order` (hyp/term), `drop_order` (hyp/term, term), `dtor_once` (term) +7 more +9 Glue | 19/161; via `DropGlue.eq_dropEvents`, `dropContents_glue`, `dropContents_dtor` | witness: `Examples.lean` example (l. 3499) | corpus: `struct_nested_dtor_drop`, `two_params_dropped_at_pop` | a stated property is false | `drop_glue_order` is false: a field's destructor runs before its owner's, which `GlueBlocks` rejects (`glue_dtorAfterFields_rejected`; RUE-2487) | 45 |
| 67 | `fields-reverse` | §6.11 | drop glue order (3.9:15) | drop-order | spec: `drop_glue_order` (hyp/term), `drop_order` (hyp/term, term), `Sharp.bare_dtor` (term, ¬/term) +2 more +6 Glue | 11/161; via `DropGlueSeq.eq_dropEventsList`, `dropContentsList_glue` | witness: `Examples.lean` example (l. 1068) | corpus: `array_drop_order`, `array_dyn_read_after_sibling_move` +22 | a stated property is false | `drop_glue_order` is false: fields drop last to first, which `GlueBlocks` rejects (`glue_fieldsSwapped_rejected`; RUE-2487) | 51 |
| 68 | `scope-fifo` | §6.9 | frame exit drop order | drop-order | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp), `drop_order` (hyp) +52 more +64 Glue | 119/161; via `runAllScopeDrops_ok`, `step_drop_order`, `step_lifo` +3 | witness: `Examples.lean` example (l. 3492) | corpus: `enum_return_past_payload`, `return_past_affine` +2 | a stated property is false | a frame's bindings are dropped first-declared first: `drop_order`'s `Lifo` | 45 |
| 69 | `payload-order` | §6.6 | match arm exit order | drop-order | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp), `drop_order` (hyp) +52 more +64 Glue | 119/161; via `soundness`, `step_drop_order`, `step_lifo` +2 | witness: `Witnesses.lean` example (l. 235) | corpus: `enum_two_payload_bindings` | a stated property is false | an arm's payload bindings are dropped first to last: `drop_order`'s `Lifo` | 74 |
| 70 | `overwrite-no-drop` | §6.8 | (D-Assign) overwrite drop | drop-skip | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp), `drop_order` (hyp) +38 more +35 Glue | 76/161; via `sim_assign`, `eval_conserves`, `step_drop_order` +1 | witness: `Examples.lean` example (l. 1078) | corpus: `affine_overwrite`, `array_elem_overwrite` +7 | a stated property is false | an assignment's old value is never dropped or freed: `Exact` (`drop_exactly_once`) | 54 |
| 71 | `break-skip-local` | §6.10 | (D-Break) unwind | off-by-one | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp), `drop_order` (hyp) +56 more +64 Glue | 123/161; via `loop_step`, `sim_brk`, `sim_loop` +4 | witness: `Examples.lean` example (l. 3933) | corpus: `loop_break_past_linear`, `loop_break_past_local` +1 | a stated property is false | `break` skips a loop-local's drop, which is never freed: `Exact` | 47 |
| 72 | `seq-affine-as-linear` | §6.7 | (D-Seq) affine discard | affine-linear | spec: `drop_exactly_once` (term), `dtor_once` (term), `eval_complete` (term) +45 more +45 Glue | 93/117; via `sim_seq`, `soundness`, `eval_succ` +6 | Explain mirror: `traceEval_res` (`Explain.lean`) | corpus: `affine_temporary_discarded` | a stated property is false | `eval` refuses to discard an affine temporary in a checked program: `soundness` | 77 |
| 73 | `seq-droptemp-skip` | §6.7 | (D-Seq) temporary drop mark | drop-skip | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp), `drop_order` (hyp) +7 more +7 Glue | 17/161; via `eval_glue_blocks`, `step_drop_order`, `Sharp.loopTurn_step` +2 | Explain mirror: `traceEval_res` (`Explain.lean`) | survived | a stated property is false | a discarded temporary is never marked freed: `rest_exactly_once`'s `Exact` | 115 |
| 74 | `residue-mark-skip` | §6.3 | destructure residue drop mark | drop-skip | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +10 more +7 Glue | 20/161; via `dropResidue_blocks`, `plainResidue_locs`, `residueMark_measure` +1 | witness: `Examples.lean` example (l. 2943) | survived | a stated property is false | a destructure's residue is never marked freed: `Exact` | 55 |
| 75 | `match-consume-skip` | §6.6 | (D-Match) consume | drop-skip | spec: `drop_exactly_once` (term), `drop_glue_order` (hyp/term), `drop_order` (hyp/term) +11 more +7 Glue | 21/161; via `Nonvacuous.enum_match`, `matchConsume_blocks`, `matchConsume_measure` +2 | witness: `Examples.lean` example (l. 2843) | survived | a stated property is false | a matched enum's shell identity is never freed: `Exact` | 53 |
| 76 | `leak-monitor-off` | §6.11 | linearLeak monitor | monitor | spec: `Sharp.leak` (term, ¬/term) +2 Glue | 3/117; via `Sharp.leak` | witness: `Examples.lean` example (l. 1369) | corpus: `destructure_linear_residue`, `enum_arm_leaks_payload` +11 | a stated property is false | `Sharp.leak` is false: an unchecked leak is no longer refused (RUE-2485); the linear theorems still hold, since a machine with no refusal meets them | 52 |
| 77 | `overwrite-monitor-off` | §6.8 | linearOverwrite monitor | monitor | spec: `Sharp.overwrite` (term) +1 Glue | 2/117; via `Sharp.overwrite` | witness: `Corpus.lean` example (l. 1029) | corpus: `linear_overwrite` | a stated property is false | `Sharp.overwrite` is false: an unchecked overwrite of a live linear value is no longer refused (RUE-2485) | 50 |
| 78 | `discard-monitor-off` | §6.7 | linearDiscard monitor | monitor | spec: `drop_exactly_once` (term), `dtor_once` (term), `eval_complete` (term) +31 more +28 Glue | 62/117; via `sim_seq`, `Sharp.discard`, `Sharp.discard_loop` +7 | witness: `Corpus.lean` example (l. 1031) | corpus: `linear_temporary_discarded` | a stated property is false | `Sharp.discard` and `Sharp.discard_loop` are false: an unchecked discard is no longer refused (RUE-2485); the build stops first at `eval_succ`, which restates `eval` | 47 |
| 79 | `copy-monitor-off` | §6.5 | ownedUnderCopy monitor | monitor | spec: `drop_exactly_once` (term), `dtor_once` (term), `freed_once` (term) +4 more +3 Glue | 10/117; via `Sharp.copy`, `Cons.intro`, `Exact.intro` | witness: `Trace.lean` example | corpus: `copy_monitor_off` | a stated property is false | `Sharp.copy` is false: an owned value under a `Copy` one is no longer refused (RUE-2485); the build stops first at `Cons.intro`, a ledger step for `introVal` | 50 |
| 80 | `dyn-residual-declared` | §6.11 | Contents.residualLinear (3.8:74) | affine-linear | spec: `Sharp.leak` (term, ¬/term), `Sharp.overwrite` (term) +3 Glue | 5/118; via `Sharp.leak`, `Sharp.overwrite` | witness: `Examples.lean` example (l. 1364) | corpus: `destructure_linear_residue`, `enum_arm_leaks_payload` +13 | a stated property is false | `Sharp.leak` and `Sharp.overwrite` are false: a declared-linear struct with no linear field owes nothing, so its leak and its overwrite are no longer refused (RUE-2485) | 51 |
| 81 | `hasty-int-any-value` | §6.1 | HasTy.int | premise | spec: `Sharp.out_of_range_halt` (¬) +1 Glue | 2/22; via `Sharp.out_of_range_halt` | survived | (same) | a stated property is false | `Sharp.out_of_range_halt` is false (RUE-2500): its `¬ HasTy` of `2^63` at `i64`, and its `¬ SafeAt` of the configuration halted with it, rest on `HasTy.int`'s bounds, which the mutant drops (kernel-checked refutation, scratch/rue-2500/hasty-int-any-value-T.lean). Before RUE-2500 no Spec statement was false: `HasTy` occurred only in conclusions and in `¬` claims about stuck or valueless runs; `HasTy.contentsTy` is also a false helper | 57 |
| 82 | `hasty-float-any-value` | §6.1 | HasTy.float | premise | spec: `Sharp.float_halt` (¬) +1 Glue | 2/22; via `Sharp.float_halt` | survived | (same) | a stated property is false | `Sharp.float_halt` is false (RUE-2500): the configurations halted with `30 · 2^-2` and `1 · 2^-1075` are now `SafeAt` `f64`, since `HasTy.float` no longer asks `Wf` (scratch/rue-2500/hasty-float-any-value-T.lean); `HasTy.contentsTy` is also a false helper | 53 |
| 83 | `evalok-stuck-ok` | §7 | EvalOk (progress) | vacuous | spec: `Sharp.frame` (¬), `Sharp.stuck` (¬), `Sharp.typed` (¬) +3 Glue | 6/6; via `Sharp.frame`, `Sharp.stuck`, `Sharp.typed` | survived | (same) | a stated property is false | `Sharp.stuck`, `.typed` and `.frame` are false: each asserts `¬ EvalOk … (.stuck _)`, now `¬ True`. `Spec.soundness_stmt` is only weakened by this mutant, not false, so that is not the kill | 52 |
| 84 | `contentsmatches-moved-residue` | §7 | ContentsMatches.moved | premise | spec: `drop_exactly_once` (hyp), `rest_exactly_once` (hyp), `soundness` (hyp) +3 more +3 Glue | 9/9; via `ContentsMatches.residualLinear_false`, `OwnSt.join_matches` | survived | (same) | a stated property is false | `Spec.soundness_stmt` — the headline — and `drop_exactly_once_stmt` are both false: the dropped residual-linear check sits in `FrameMatches`, a *hypothesis* of `soundness`, so weakening it strengthens the claim; a live linear overwrite `check` now accepts still runs to `.stuck .linearOverwrite` (kernel-checked counterexample, contentsmatches-moved-residue-T.lean) | 56 |
| 85 | `exact-at-most` | §7 | Exact (ok/returned) | count | spec: `Sharp.no_lead` (¬), `Sharp.pending_expr` (¬), `Sharp.pending_program` (¬) +5 Glue | 8/18; via `Sharp.not_exact_ok`, `Sharp.not_exact_returned` | survived | (same) | a stated property is false | `Sharp.pending_program`, `.pending_expr` and `.no_lead` are false: each `¬ Exact` rested on a strict `<` that the weakened `≤` now satisfies; `Sharp.store_cc` stays true, since its `¬ Exact` rests on `StoreCC` instead | 93 |
| 86 | `blocks-any-trace` | §6.11 | Blocks | wildcard | spec: `Sharp.bare_dtor` (¬), `Sharp.unreached` (¬), `Sharp.unreached_panic` (¬) +3 Glue | 6/9; via `Blocks.not_dtor` | survived | (same) | a stated property is false | `Sharp.bare_dtor`, `.unreached` and `.unreached_panic` are false via `Blocks.not_dtor`; `Sharp.leak`, `.overwrite`, `.discard`, `.discard_loop` and `.copy` never mention `Blocks` and stay true | 71 |
| 87 | `lifo-vacuous` | §6.11 | Lifo | vacuous | spec: `Sharp.uncut_drop` (¬) +1 Glue | 2/9; via `Sharp.uncut_drop` | survived | (same) | a stated property is false | `Sharp.uncut_drop` is false (RUE-2500): its `¬ Lifo [0, 1] [0] [0]` (a pop that cut cell 1 and dropped cell 0) is now `¬ True`, and so is its refutation of `drop_order`'s last half, where `NewestFirst` and the stack's order hold (scratch/rue-2500/lifo-vacuous-T.lean). `Sharp.unordered` and `.not_a_step` stay true through other conjuncts | 73 |
| 88 | `newestfirst-vacuous` | §6.11 | NewestFirst | vacuous | spec: `Sharp.uncut_drop` (¬) +1 Glue | 2/9; via `Sharp.uncut_drop` | witness: `unorderedRecord_rejected` (`Witnesses.lean`) | survived | only a helper is false | no Spec statement is false: `Sharp.unordered` and `.not_a_step` stay true through other conjuncts, and `Sharp.uncut_drop` through its `¬ Lifo` (`stays`); killed instead by `Witnesses.lean`'s `unorderedRecord_rejected`, a layer-4 witness, not a stated property | 86 |
| 89 | `ordered-vacuous` | §6.11 | Config.Ordered | vacuous | proof: `Config.Ordered.keep` (`TraceOrder.lean`) | 0/0 | witness: `unorderedRecord_rejected` (`Witnesses.lean`) | survived | only a helper is false | `Config.Ordered` occurs in no Spec, Sharp, Nonvacuous or Glue statement; killed by the same witness, `unorderedRecord_rejected`, not a stated property | 81 |
| 90 | `safeat-progress-vacuous` | §7 | Config.SafeAt (progress) | vacuous | spec: `Sharp.stuck_step` (¬), `Sharp.unreachable_stuck` (¬) +2 Glue | 4/10; via `Sharp.stuck_step`, `Sharp.unreachable_stuck` | survived | (same) | a stated property is false | `Sharp.unreachable_stuck` and `.stuck_step` are false: dropping the progress conjunct lets `¬ SafeAt` of a stuck configuration hold vacuously on typing alone, and the stuck program's reachable-value claim is refuted by `Step.det` | 76 |
| 91 | `safeat-typing-vacuous` | §7 | Config.SafeAt (typing) | vacuous | spec: `Sharp.float_halt` (¬), `Sharp.ill_typed_halt` (¬), `Sharp.out_of_range_halt` (¬) +3 Glue | 6/10; via `Sharp.float_halt`, `Sharp.ill_typed_halt`, `Sharp.out_of_range_halt` | survived | (same) | a stated property is false | `Sharp.ill_typed_halt` is false (RUE-2500): the configuration halted with `true` is terminal, so with the typing conjunct gone it is `SafeAt` `i64` (scratch/rue-2500/safeat-typing-vacuous-T.lean); `Sharp.out_of_range_halt` and `.float_halt` are false too. `unreachable_stuck` and `.stuck_step` rest on the progress conjunct and stay true | 17 |
| 92 | `safeat-terminal-only` | §7 | Config.SafeAt (progress) | strengthen | spec: `step_preservation` (concl) | 1/1; via `init_safeAt` | survived | (same) | a stated property is false | `Spec.step_preservation_stmt` is false, refuted at `Config.init` of `loop {()}`, which is not terminal | 15 |
| 93 | `stepsn-one-step-only` | §7 | StepsN.step | strengthen | spec: `eval_diverges_iff` (↔), `step_type_safety` (concl), `Sharp.discard_loop` (concl, ¬/↔) +1 Glue | 4/4; via `Sharp.loop_forever`, `Long.pre1`, `Steps.toN` +3 | survived | (same) | a stated property is false | `Spec.eval_diverges_iff_stmt` and `Sharp.discard_loop` are false: `loop {()}` exhausts every fuel but has no `StepsN 2`; `step_type_safety_stmt` is false by the same argument | 22 |
| 94 | `float-wf-no-emin` | §7 | FloatDatum.Wf | bounds | spec: `drop_exactly_once` (hyp, term), `drop_glue_order` (hyp/term, term), `drop_order` (hyp/term, term) +27 more +60 Glue | 90/92; via `Float.sqrt_wf`, `Sharp.float_halt`, `roundOp_wf` +1 | survived | (same) | a stated property is false | `Sharp.float_halt` is false (RUE-2500): its `¬ (num false 1 (-1075)).Wf .w64`, half the least subnormal, is refuted (scratch/rue-2500/float-wf-no-emin-T.lean). The float laws still hold on the mutant's larger `Wf` as far as RUE-2490 sampled, and the four Float lemmas that fail conclude a weaker `Wf` and stay true, so without that statement nothing would be false | 65 |
| 95 | `float-wf-noncanonical` | §7 | FloatDatum.Wf | bounds | spec: `drop_exactly_once` (hyp, term), `drop_glue_order` (hyp/term, term), `drop_order` (hyp/term, term) +27 more +60 Glue | 90/92; via `Float.sqrt_wf`, `Sharp.float_halt`, `roundOp_wf` +1 | survived | (same) | a stated property is false | `Sharp.float_halt` is false (RUE-2500): its `¬ (num false 30 (-2)).Wf .w64`, the non-canonical spelling of the `7.5` `Nonvacuous.float` returns, is refuted (scratch/rue-2500/float-wf-noncanonical-T.lean); the float laws and the four Float lemmas stay true as for `float-wf-no-emin` | 62 |
| 96 | `contentsmatches-owned-false` | §7 | ContentsMatches.owned | strengthen | spec: `soundness` (concl), `Nonvacuous.open_frame` (concl), `Sharp.pending_expr` (concl) +3 more +10 Glue | 16/24; via `Nonvacuous.open_frame`, `Sharp.pending_expr`, `Sharp.pending_program` +1 | survived | (same) | a stated property is false | `Nonvacuous.open_frame` is false: its `FrameMatches` of the owned binding `s : S0` against the cell `S0 { 5 }` needs `ContentsMatches .owned`, whose premise is now `False` (scratch/rue-2500/contentsmatches-owned-false-T.lean). No other witness states `FrameMatches` at a frame with an owned binding: `empty_frame` and `Sharp.stuck` state it of the empty frame, and `Nonvacuous.dtor` does not state it at all, so `soundness`, `drop_exactly_once` and `rest_exactly_once` would be vacuous at every open frame and only `open_frame` shows it | 65 |

#### RUE-2490: the statement vocabulary and Float

Rows 81–96 change what the statements are written in: `HasTy`,
`ContentsMatches`, `EvalOk`, `Exact`, `Blocks`, `Lifo`, `NewestFirst`,
`Config.Ordered`, `Config.SafeAt`, `StepsN` and `FloatDatum.Wf`. Each has a
direction, and a weakening can make a statement false only where the
definition occurs in a hypothesis or under `¬`, a strengthening only where it
occurs in a conclusion. The table below lists, for each, every candidate
statement with the positions of the definition in it (`hyp`, `¬`, `↔`,
`term`, `concl`; a position is written outermost first, so `¬/hyp` is a
hypothesis under a `¬`), unproved ones in bold, and the count of `Glue`
theorems among the candidates. It is the second table `mutate.py --table`
prints; `mutate.py --candidates --only ID` prints one mutant's list in full,
with the definitions unfolded to reach each occurrence.

| # | Mutant | Direction | Candidate statements (position of the definition; **unproved** under the mutant) |
|---|---|---|---|
| 81 | `hasty-int-any-value` | weaken | `Sharp.entry_param` (¬), `Sharp.float_halt` (¬), `Sharp.frame` (¬), `Sharp.ill_typed_halt` (¬), `Sharp.no_entry` (¬), **`Sharp.out_of_range_halt`** (¬), `Sharp.stuck` (¬), `Sharp.stuck_step` (¬), `Sharp.typed` (¬), `Sharp.unreachable_stuck` (¬); 12 Glue (1 unproved) |
| 82 | `hasty-float-any-value` | weaken | `Sharp.entry_param` (¬), **`Sharp.float_halt`** (¬), `Sharp.frame` (¬), `Sharp.ill_typed_halt` (¬), `Sharp.no_entry` (¬), `Sharp.out_of_range_halt` (¬), `Sharp.stuck` (¬), `Sharp.stuck_step` (¬), `Sharp.typed` (¬), `Sharp.unreachable_stuck` (¬); 12 Glue (1 unproved) |
| 83 | `evalok-stuck-ok` | weaken | **`Sharp.frame`** (¬), **`Sharp.stuck`** (¬), **`Sharp.typed`** (¬); 3 Glue (3 unproved) |
| 84 | `contentsmatches-moved-residue` | weaken | **`Sharp.frame`** (¬), **`Sharp.stuck`** (¬), **`Sharp.typed`** (¬), **`drop_exactly_once`** (hyp), **`rest_exactly_once`** (hyp), **`soundness`** (hyp); 3 Glue (3 unproved) |
| 85 | `exact-at-most` | weaken | **`Sharp.no_lead`** (¬), **`Sharp.pending_expr`** (¬), **`Sharp.pending_program`** (¬), `Sharp.store_cc` (¬); 14 Glue (5 unproved) |
| 86 | `blocks-any-trace` | weaken | **`Sharp.bare_dtor`** (¬), **`Sharp.unreached`** (¬), **`Sharp.unreached_panic`** (¬); 6 Glue (3 unproved) |
| 87 | `lifo-vacuous` | weaken | `Sharp.not_a_step` (¬), **`Sharp.uncut_drop`** (¬), `Sharp.unordered` (¬); 6 Glue (1 unproved) |
| 88 | `newestfirst-vacuous` | weaken | `Sharp.not_a_step` (¬), **`Sharp.uncut_drop`** (¬), `Sharp.unordered` (¬); 6 Glue (1 unproved) |
| 89 | `ordered-vacuous` | weaken | none |
| 90 | `safeat-progress-vacuous` | weaken | `Sharp.float_halt` (¬), `Sharp.ill_typed_halt` (¬), `Sharp.out_of_range_halt` (¬), **`Sharp.stuck_step`** (¬), **`Sharp.unreachable_stuck`** (¬); 5 Glue (2 unproved) |
| 91 | `safeat-typing-vacuous` | weaken | **`Sharp.float_halt`** (¬), **`Sharp.ill_typed_halt`** (¬), **`Sharp.out_of_range_halt`** (¬), `Sharp.stuck_step` (¬), `Sharp.unreachable_stuck` (¬); 5 Glue (3 unproved) |
| 92 | `safeat-terminal-only` | strengthen | **`step_preservation`** (concl) |
| 93 | `stepsn-one-step-only` | strengthen | **`Sharp.discard_loop`** (concl, ¬/↔), **`eval_diverges_iff`** (↔), **`step_type_safety`** (concl); 1 Glue (1 unproved) |
| 94 | `float-wf-no-emin` | weaken | **`Nonvacuous.exact_model`** (term), `Sharp.entry_param` (¬), **`Sharp.float_halt`** (¬), **`Sharp.frame`** (¬), **`Sharp.ill_typed_halt`** (¬), `Sharp.no_entry` (¬), **`Sharp.out_of_range_halt`** (¬), **`Sharp.stuck`** (¬), **`Sharp.stuck_step`** (¬), **`Sharp.typed`** (¬), **`Sharp.unreachable_stuck`** (¬), **`drop_exactly_once`** (hyp, term), **`drop_glue_order`** (hyp/term, term), **`drop_order`** (hyp/term, term), **`eval_complete`** (hyp/term, term), **`eval_diverges_iff`** (term, ↔/term), **`eval_sound`** (hyp/term, term), **`never_stuck_iff`** (term, ↔/hyp/term, ↔/term), **`no_double_free`** (term), **`no_linear_discard`** (term), **`no_linear_leak`** (term), **`no_linear_overwrite`** (term), **`no_use_after_drop`** (term), **`no_use_after_move`** (term), **`no_violation`** (term), **`rest_exactly_once`** (hyp, hyp/term, term), **`run_safe`** (term), **`soundness`** (hyp, term), **`step_no_double_free`** (hyp/term, term), **`step_preservation`** (hyp/term, term), **`step_progress`** (hyp/term, term), **`step_type_safety`** (term); 60 Glue (60 unproved) |
| 95 | `float-wf-noncanonical` | weaken | **`Nonvacuous.exact_model`** (term), `Sharp.entry_param` (¬), **`Sharp.float_halt`** (¬), **`Sharp.frame`** (¬), **`Sharp.ill_typed_halt`** (¬), `Sharp.no_entry` (¬), **`Sharp.out_of_range_halt`** (¬), **`Sharp.stuck`** (¬), **`Sharp.stuck_step`** (¬), **`Sharp.typed`** (¬), **`Sharp.unreachable_stuck`** (¬), **`drop_exactly_once`** (hyp, term), **`drop_glue_order`** (hyp/term, term), **`drop_order`** (hyp/term, term), **`eval_complete`** (hyp/term, term), **`eval_diverges_iff`** (term, ↔/term), **`eval_sound`** (hyp/term, term), **`never_stuck_iff`** (term, ↔/hyp/term, ↔/term), **`no_double_free`** (term), **`no_linear_discard`** (term), **`no_linear_leak`** (term), **`no_linear_overwrite`** (term), **`no_use_after_drop`** (term), **`no_use_after_move`** (term), **`no_violation`** (term), **`rest_exactly_once`** (hyp, hyp/term, term), **`run_safe`** (term), **`soundness`** (hyp, term), **`step_no_double_free`** (hyp/term, term), **`step_preservation`** (hyp/term, term), **`step_progress`** (hyp/term, term), **`step_type_safety`** (term); 60 Glue (60 unproved) |
| 96 | `contentsmatches-owned-false` | strengthen | `Nonvacuous.empty_frame` (concl), **`Nonvacuous.open_frame`** (concl), `Sharp.no_eval` (concl), `Sharp.no_lead` (concl), **`Sharp.pending_expr`** (concl), **`Sharp.pending_program`** (concl), `Sharp.store_cc` (concl), **`Sharp.stuck`** (concl), **`Sharp.typed`** (concl), **`soundness`** (concl); 14 Glue (10 unproved) |

Read against the hand refutations of RUE-2490's review
(`scratch/rue-2490-review/`) and RUE-2500 (`scratch/rue-2500/`), the spec
pass agrees with every one: each statement a refutation shows false is
unproved, and each statement a review showed still true is proved (for
example `Sharp.store_cc` under `exact-at-most`, `Sharp.unordered` and
`.not_a_step` under `lifo-vacuous`, `unreachable_stuck` under
`safeat-typing-vacuous`). It leaves some more candidates unproved than the
refutations cover, all under readings that are already "a stated property
is false": `rest_exactly_once` and `Sharp.stuck`, `.typed` and `.frame`
under `contentsmatches-moved-residue` (they rest on `soundness`'s broken
proof), `Sharp.stuck` and `.typed` under `contentsmatches-owned-false`,
and, under the two `Wf` mutants, most of the package, since `widen_wf`,
`roundOp_wf` and `Float.sqrt_wf` fail (each has `Wf` on both sides, so it is
rebuilt, and each breaks as a script) and the float laws rest on them.

Three rows need no hand reading now: `blocks-any-trace` and
`contentsmatches-moved-residue` (RUE-2499's acceptance: the pass names
`Sharp.bare_dtor`, `.unreached` and `.unreached_panic`, and `soundness`,
`drop_exactly_once` and the three Sharp `¬ EvalOk` statements), and
`ordered-vacuous`, whose empty candidate list shows that no statement can
be false for it. `newestfirst-vacuous` leaves `Sharp.uncut_drop` unproved:
its proof breaks at the conjunct `NewestFirst [0]`, now `True`, and the
statement still holds through its `¬ Lifo`, which the reading's `stays`
records.

### Readings the spec pass corrected

The spec pass left an unproved candidate under 87 mutants. For 70 of them the
earlier reading was already "a stated property is false" and names a
statement the pass leaves unproved. For 14 it was "helper" or "holds", and
the reading now says why each unproved candidate holds (`stays`); for 11 of
those the only failed proof is a lemma whose statement is still true for the
mutant (`check_sound`, whose rule and checker drop the same premise, for
four; `binOpInt_res`, `intResult_res`, `evalFintrin_float_res` and the like,
whose result is still typed, for the arithmetic mutants). Three readings
were wrong, each a "holds" the pass flagged, and each is now "a stated
property is false", with a refutation checked in the kernel against the
mutated copy (`scratch/rue-2499/refute/` in the loop's state directory; each
fails on the unmutated package):

* **`meet-never`**: `Nonvacuous.loop` is false. The witness's loop body
  `if i >= 3 { break }` has a diverging arm, whose type no longer meets `()`,
  so `checkProgram` refuses the program the witness says it accepts. The
  earlier reading, "no statement is about the checker's completeness",
  overlooked that a witness states an acceptance.
* **`operand-swap`**: `Nonvacuous.loop` is false. `i >= 3` runs as
  `3 >= i`, so the loop breaks on its first turn and no destructor runs,
  where the witness states three. The safety spine does hold ("swapped
  operands still yield an in-range value or a trap"); the witness pins the
  arithmetic.
* **`bounds-negative`**: `no_violation`, and with it the safety spine, is
  false. `let a: [i64; 0] = []; a[-1]` is checked, `-1 < 0` passes the
  mutated bounds test, and element `0` of an empty array is a
  `typeConfusion`. The earlier reading, "a negative index reads element 0: a
  defined, well-typed result", overlooked the empty array.

The pass also flags a statement no hand reading considered: under
`contentsmatches-owned-false` it leaves `soundness` unproved beside the three
witnesses RUE-2500 named. `ContentsMatches` occurs in `soundness`'s
conclusion too (through `EvalOk`'s final `FrameMatches`), where a
strengthening can make it false, for instance by an assignment that makes a
moved-out binding `Owned`. The reading stands on the witnesses; whether
`soundness` is also false is not read here.

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

### RUE-2490's survivors

RUE-2499's run confirms what follows: under each of the six, the spec pass
leaves exactly RUE-2500's statement unproved among the Spine statements
(`Sharp.out_of_range_halt`, `.float_halt`, `.uncut_drop`, `.ill_typed_halt`;
`safeat-typing-vacuous` also `.float_halt` and `.out_of_range_halt`, the
float mutants most of the package, above). RUE-2500 resolved these: each of the six below now falsifies a Sharp
statement. The "Proposed witness" column is what RUE-2490 asked for; what
was built is in the list after the score block above. It differs from the
proposal in two places. The `HasTy` witnesses are sharpness
counter-examples of `step_preservation` over a halted, unreached
configuration, not witnesses about a checked program's contents: a value
`HasTy` must reject cannot be written by a checked program, and no run
produces one. And the float witness needs no proof about the `FloatModel`
laws, since `¬ Wf` of the datum is what the mutant falsifies. The table is
kept as RUE-2490's record.

Six of the 15 statement-vocabulary mutants are genuine survivors: no Spec,
Sharp, Nonvacuous or Glue statement is false for them (checked by hand,
`scratch/rue-2490-review/`). Each is killed only by a helper lemma that
restates the mutated definition (or, for the two float mutants, not even
that — the helper's own conclusion is merely weaker, still true), and would
survive with the proofs off. The other nine are killed, six of them (rows
83, 84, 85, 86, 90, 92, 93) by a false Spec or Sharp statement, and two more
(88, 89) by an existing layer-4 witness (`unorderedRecord_rejected`); see the
score block above for the full breakdown.

| Mutant | Why it survives | Proposed witness |
|---|---|---|
| `hasty-int-any-value` (`HasTy.int` drops `InBounds`) | `HasTy` occurs only in conclusions; every negated `HasTy`-adjacent Sharp claim (`.stuck`, `.stuck_step`, `.no_entry`, `.entry_param`) is really about a stuck or valueless run, not about `HasTy` itself | a Sharp- or Nonvacuous-style witness that a concrete out-of-range int's contents are never `HasTy`/`ContentsTy`-well-typed at its width |
| `hasty-float-any-value` (`HasTy.float` drops `f.Wf w`) | the same argument, over `f.Wf w` | the same shape, for a concrete `FloatDatum` that is not `Wf` |
| `lifo-vacuous` (`Lifo → True`) | `Lifo.newer` is an L2 helper, not a stated property; the two Sharp `¬ Lifo` claims (`.unordered`, `.not_a_step`) stay true through other facts, not through `Lifo` | a Sharp statement with `¬ Lifo` of a pop that drops a cell it did not cut — the counterpart of `unorderedRecord_rejected`, in the Spec layer rather than a witness |
| `safeat-typing-vacuous` (`Config.SafeAt`'s typing conjunct `→ True`) | both Sharp `¬ SafeAt` claims (`.stuck_step`, `.unreachable_stuck`) rest on the progress conjunct, which this mutant leaves alone | a `¬ SafeAt` of a configuration that halts with an ill-typed value, naming the typing conjunct the way `unreachable_stuck` names progress |
| `float-wf-no-emin` (`FloatDatum.Wf` drops the `eMin` floor) | sampled, not proved: `exactOps` on data that satisfy the mutant's weaker `Wf` but not the real one still satisfies `arith_wf`/`sqrt_wf`/`narrow_wf`/`div_by_zero` on the cases checked, so `Nonvacuous.exact_model` stands | a `¬ Wf` witness for a below-`eMin` datum, and a proof (not a sample) that no `FloatModel` law is vacuous on it |
| `float-wf-noncanonical` (drops the odd-significand requirement) | the same sampling argument | a `¬ Wf` witness for an even-significand datum |

Two kills are worth flagging for the coordinator, not because they are
misread now but because of what made them hard to see:

* `blocks-any-trace` (`Blocks` gains a wildcard case) is killed by
  `Sharp.bare_dtor`, `.unreached` and `.unreached_panic` — all three via
  `Blocks.not_dtor` (`TraceOrder.lean:1279`), which each of them uses to
  name a trace with no drop marker around a bare destructor. `Sharp.leak`,
  `.overwrite`, `.discard`, `.discard_loop` and `.copy` do not mention
  `Blocks` and stay true regardless. The automated pipeline never sees any
  of this — `Sharp.lean` is layer 3, sorried by the proofs-off pass, and in
  the first pass it sits downstream of `TraceOrder.lean`'s own script
  failure (`Blocks.append`, `Blocks.drop_inv`, both ordinary
  structural-induction helpers with no case for the new constructor). A
  narrower weakening of one existing `Blocks` constructor, rather than an
  added case, would not trip that induction, would reach `Sharp.lean`
  untouched by the tool, and would still be killed there — by `bare_dtor`,
  `unreached` or `unreached_panic` (read by hand then; the spec pass now
  builds `Sharp.lean` under the mutant and names them). `REDTEAM-LOG.md` should
  note that these three statements are `Blocks`'s real protection, not the
  induction helpers that happen to break first.
* `safeat-progress-vacuous` (`Config.SafeAt`'s progress conjunct `→ True`)
  is killed by `Sharp.unreachable_stuck` and `.stuck_step`, invisible to the
  automated passes before RUE-2499 for the same layer-3 reason, and named by
  the spec pass now.

`safeat-terminal-only` and `stepsn-one-step-only` (the two strengthening
controls) are caught immediately by a false Spec statement
(`step_preservation`; `eval_diverges_iff` and `Sharp.discard_loop`), exactly
as a strengthening is expected to work — but they exercise only the trivial
direction. `SafeAt` and `StepsN` occur positively in a conclusion, so any
non-equivalent strengthening there must break `adequacy`'s own proof; they
say nothing about whether the witness and sharpness layers pin the
vocabulary against a hypothesis-side strengthening, which is the direction
that can make a theorem vacuous rather than merely false. A control for that
direction — for example `ContentsMatches.owned` demanding `False`, or
`StoreCC` demanding a false conjunct, either of which would make
`soundness`/`drop_exactly_once` vacuous — is not in this block, and would
need a non-vacuity witness that states `FrameMatches`/`StoreCC` positively
(`Nonvacuous.open_frame`, `.dtor`) to catch.

### Proposed issues (for the coordinator)

1. **[Formal/Bridge] Seed the refusals only a witness proves — done
   (RUE-2486).** 13 mutants were killed by an `Examples.lean` or
   `Trace.lean` refusal witness and by no seed or generated case, each a
   refusal the compiler should make too, that the bridge never compared. Two
   PRs seeded all 13:
   * Part 1 (seven): moving an element out below a projection
     (`use-move-rootidx`); a dynamic-index read, `@drop` or write of a
     non-Copy or linear element (`index-read-copy`,
     `index-drop-copy-checker`, `index-write-linear`); a constant index
     equal to the length (`const-index-off-by-one`); a partially reassigned
     declared-linear struct that leaks (`residual-declared`); and a linear
     field after a moved slot that leaks (`residual-untracked`).
   * Part 2 (the remaining six): an out-of-range literal (`lit-bounds`);
     `@dbg` of an aggregate (`dbg-observable`); `[e; n]` of a non-Copy
     element (`repeat-copy`); a `@copy` struct with a destructor, and a
     destructor-bearing struct with a linear field (`copy-struct-dtor`,
     `dtor-linear-field`); and the `ownedUnderCopy` refusal
     (`copy-monitor-off`). The last two seeds (`copy_struct_dtor`,
     `dtor_linear_field`) are declarations `main` never instantiates —
     `checkDecls` checks every declaration regardless of use, so there is no
     dynamics to compare — and `copy_monitor_off` reuses `Trace.lean`'s
     `dupProgram`, an ill-typed program the checker already refuses for an
     unrelated reason (a struct field's declared type doesn't match its
     literal), since the state the monitor guards is otherwise unreachable
     by any well-typed program.

   Evidence: the refusal mutants listed under "What the proofs kill" now all
   show `corpus:` in the table's "Corpus and bridge alone" column, each
   confirmed by a rerun of exactly its own part's mutants
   (`mutate.py --only`), and each seed's printed source was run through
   `scripts/rue exec` and confirmed to give the compiler error its
   description claims (E0800, E0702, E0905, E0457, E0462 and E0206
   respectively).
2. **[Formal/Assurance] State §6.11's drop order independently of
   `dropEvents`.** `drop_order`'s `Blocks` is defined through `dropEvents`,
   so changing `dropContents` and `dropEvents` together falsified no stated
   property. That covered:
   * no destructor at all (`dtor-skip`);
   * the destructor after the fields (`dtor-after-fields`);
   * the fields last to first (`fields-reverse`).

   Done in RUE-2487: `drop_glue_order` states the destructor-first,
   declaration-order and ascending-index order over §6.11's own rules
   (`DropGlue`, `GlueBlocks`), and each of the three mutants now falsifies
   it ("What the proofs kill", above). Whether the order should be a §7
   bullet is for Steve.
3. **[Formal/Assurance] The linear theorems are satisfied by a machine with
   no monitors.** Removing any of the four run-time refusals leaves every
   spine statement true. Evidence: `leak-monitor-off`,
   `overwrite-monitor-off`, `discard-monitor-off`, `copy-monitor-off` and
   `dyn-residual-declared`. This is R3 of the red-team log, measured. Done
   in RUE-2485: the monitor-fires statements are in the Spec layer, and each
   of the five mutants now falsifies one.
4. **(Loop tooling, state-dir `bin/`) Keep the mutants applicable.**
   `bin/mutate.py --check` takes a few seconds. It fails when an edit no
   longer matches the sources, and when a module is missing from
   `Layers.lean` or left on by the proofs-off copy. Adding it to `chain.sh`
   would keep the mutants current as the definitions change, and REDTEAM.md's
   cadence could rerun the analysis per milestone. The coordinator decides.
5. **[Formal/Assurance] The statement-vocabulary mutants, and what they still
   need (RUE-2490; done by RUE-2499 and RUE-2500).** 15 mutants over `Soundness/Defs`, `Trace/Defs`,
   `Adequacy/Defs` and `Float` (["RUE-2490: the statement vocabulary and
   Float"](#rue-2490-the-statement-vocabulary-and-float)) found the analysis
   cannot see the Spec layer for these mutants at all: `Sharp.lean`,
   `Nonvacuous.lean`, `Spine.lean` and both `Glue.lean` files are layer 3, so
   the proofs-off pass sorries them, and in the first pass they sit
   downstream of the layer-2 module that fails first. Every "Stated
   properties" reading for rows 81–95 is therefore by hand
   (`scratch/rue-2490-review/`), not by the tool. Read that way, the
   sharpness/non-vacuity layer already pins more of the vocabulary than the
   first pass of readings credited it for: `Exact` (`Sharp.pending_*`,
   `.no_lead`, `.store_cc`), `Config.SafeAt` (`Sharp.stuck_step`,
   `.unreachable_stuck`) and `EvalOk` (`Sharp.stuck`, `.typed`, `.frame`)
   are each pinned by a Sharp statement whose `¬` sits directly on the
   mutated definition, and `ContentsMatches` is pinned by `soundness` itself
   through `Spine.lean`. `blocks-any-trace` is killed the same way, by
   `Sharp.bare_dtor`/`.unreached`/`.unreached_panic` (not, as first read, by
   `Sharp.leak`/`.overwrite`/`.discard`/`.discard_loop`/`.copy`, which never
   mention `Blocks`).
   * **The method fix is RUE-2499 (done)**: a polarity-aware pass that lists, per
     mutant, which Spec/Sharp/Nonvacuous/Glue statements the mutated
     definition occurs in negatively (a hypothesis, under `¬`, or on either
     side of `↔`) — only those can be falsified by a weakening — and an
     iterated build with axiom tracing that `#print axioms` every
     `Spine`/`Sharp`/`Nonvacuous`/`Glue` theorem under the mutant, so a
     layer-3 kill is never hidden behind a sorried dependency again.
   * **The six real survivors are RUE-2500 (done)**: `hasty-int-any-value`,
     `hasty-float-any-value`, `lifo-vacuous`, `safeat-typing-vacuous`,
     `float-wf-no-emin` and `float-wf-noncanonical` — each because the
     mutated definition occurs only in a conclusion, so a weakening can only
     weaken. ["RUE-2490's survivors"](#rue-2490s-survivors), above, proposes
     a witness for each. It also proposes a hypothesis-side strengthening
     control (`ContentsMatches.owned` demanding `False`, or `StoreCC`
     demanding a false conjunct) that the two strengthening mutants here do
     not exercise, since `SafeAt` and `StepsN` occur only positively.
     RUE-2500 added `Sharp.out_of_range_halt`, `.float_halt`, `.uncut_drop`
     and `.ill_typed_halt`, one of which each survivor falsifies, and the
     control `contentsmatches-owned-false` (row 96), which falsifies
     `Nonvacuous.open_frame`.
   Both are done. RUE-2499's spec pass ("Method") is the full fix, not
   S1(b)'s cheaper variant (keep the layer-3 Spec modules on, sorry the
   rest), which would still miss row 84 (`soundness` is aliased in
   `Spine.lean` to a sorried L2 theorem) and row 86 (`bare_dtor`/`unreached`
   use `TraceOrder`'s `Blocks.not_dtor`); the rerun found three readings to
   correct ("Readings the spec pass corrected").

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
  7`, the per-lane check's. A larger stream might kill more of the 7 mutants
  the seeds and the bridge still miss (the 4 no corpus case can show, and
  the 3 trace-only ones); the 13 refusal shapes RUE-2486 seeded were never
  ones the generator drew, which is why a seed rather than a larger stream
  was the fix.
* **`operand-swap`'s corpus kill is a crash.** With the operands swapped, a
  seed's counted loop never exits, and `ruecore-corpus` overflows the native
  stack at the export fuel instead of reporting the case as not completed.
  The mutant is killed either way, but the export would crash the same way on
  any non-terminating seed.
* **The equivalence of the four equivalent mutants is argued, not proved.**
* **An unproved statement is not a false one.** The spec pass finds the
  statements a mutant can make false *and* whose proofs it breaks, from the
  environment and the kernel; whether each is false is still a reading.
  Where a reading says a statement is false, the evidence is a corpus
  counterexample, a refutation checked in the kernel against the mutated copy
  (RUE-2490's review, RUE-2500, and the three below), or an argument. Where
  it says one holds, `stays` gives the reason, not a proof.
* **The polarity walk is conservative, not exact.** It reads `∀`, `→`, `¬`,
  `∧`, `∨`, `∃`, `↔`, `match` and the package's own `Prop` definitions and
  inductives; anything else (an argument of `=`, a recursive definition, an
  application of a library predicate such as `List.Pairwise`) is a term, where
  a definition counts both ways. So a statement it calls safe really is
  (only a conclusion under a weakening, say), but it can call a statement a
  candidate that is not one, and so rebuild a theorem it could have trusted.
  The direction of each vocabulary mutant (`DIRECTION`) is declared by hand;
  a wrong one would let the pass trust a theorem the mutant can falsify.
