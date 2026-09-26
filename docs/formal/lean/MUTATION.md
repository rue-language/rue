# Mutation analysis of the definitions: would anything notice a wrong rule?

The theorems show that the definitions have the properties we state. They do
not show that the definitions say what the calculus says. A typing rule could
admit too much in a way no theorem mentions, or refuse too much in a way that
makes a theorem easy, and the kernel would check every proof all the same.
This page asks how much of the definition layer the rest of the package
actually pins (RUE-2465). We change one rule at a time and record what, if
anything, notices.

This is **mutation analysis** (DeMillo, Lipton & Sayward, "Hints on Test Data
Selection", 1978; surveyed by Jia & Harman, "An Analysis and Survey of the
Development of Mutation Testing", IEEE TSE 2011), applied to a specification
rather than to a program. A **mutant** is the definitions with one small,
deliberate change. A mutant is **killed** when something that passes on the
real definitions fails on it, and **survives** otherwise. An **equivalent
mutant** is one no correct test could kill, because it means the same as the
original. The **mutation score** is killed / (total − equivalent).
[FIELD.md](../FIELD.md) does not list these terms yet (RUE-2464 proposed
adding them). The sibling page [BRIDGE-SENSITIVITY.md](BRIDGE-SENSITIVITY.md)
applies the same analysis to the compiler; this page applies it to the Lean
definitions, and uses the same vocabulary.

Measured on trunk `41c365ee8` (2026-09-25), before the six seeds this page
adds. The six seeds are checked against their mutants afterwards
("Survivors and their resolution").

## What is mutated

The definition layer: L0 and L1 of the README's "Layers", the modules every
headline statement is written in.

* `Syntax.lean`: §3's classes (`Ty.mult`, `Attr.lift`, `Mult.join`), the
  place helpers (`Ty.atPath`, `linearResidue`, `Expr.breaks`).
* `Statics.lean`: §5's judgment `Typed` and the ownership states it threads
  (`OwnSt.join`, `residualLinear`, `fnCtx`, `armCtx`, `Ctx.joinOpt`).
* `Checker/Defs.lean`: `check`, the decision procedure the corpus verdicts
  come from.
* `Dynamics.lean`: the definitional interpreter `eval`, the drop glue and the
  run-time monitors.
* `Step.lean`: §6's small-step relation `Step` and its function `step`.

There are **80 mutants**, spread over §3, §§5.1–5.8 and §§6.2–6.12 (the table
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
`loop-head-unverified`, `entry-params`, …), the typing rule alone
(`loop-div-breaks`, `loop-break-div-brk`), `eval` alone
(`binop-eval-order`, `index-write-order`, `seq-affine-as-linear`) or `Step`
alone (`step-usecopy-nondet`). A dynamics mutant changes `eval`, `Step` and
`step` alike unless its row says otherwise. Each mutant is written as
exact-text edits in [`bin/mutate.py`](bin/mutate.py), which checks that every
edit still matches the sources exactly as often as it says (`--check`).

## Method

`bin/mutate.py` copies the package's sources into a scratch directory (never
the package itself), applies one mutant, and records the **first** of these
that notices it, in this order, the brief's (a)–(e):

1. **proof**: `lake build` fails in a proof, an L1 lemma or an L2 theorem;
2. **witness**: the build fails only in an example, a witness or a `#guard`
   (`Examples`, `Witnesses`, `Corpus`, `Print`, `Explain`, or `Step.lean`'s
   `demo_*` theorems);
3. **corpus**: the build succeeds, and `lake exe ruecore-corpus` (every seed's
   verdict and expected outcome) differs from the unmutated baseline;
4. **bridge**: the seeds are unchanged, but `--gen 200 --seed 7` (the
   per-lane check's generated cases) changes, and the compiler disagrees with
   the mutant on a changed case;
5. **survived**: nothing does.

A seed whose expectation the mutant leaves unchanged cannot make the bridge
disagree anew: the bridge already compares that expectation with the
compiler, and it agrees on unmutated trunk (all but the allowed red,
`array_elem_self_assign`). So step 4 runs the compiler (`scripts/rue exec`,
the comparison of the loop's `bin/verify.py`) only on the generated cases the
mutant changed, and the seeds are covered by step 3.

**Two further passes.** The first killer can hide the others. Two effects
make that matter here.

* **A proof can break for a reason that is not semantic.** A proof that
  destructures a rule by position (`Typed.skel_preserved` does, for all 51
  of `Typed`'s rules) breaks when a premise is deleted, whether or not any statement
  became false. So a `Typed` premise is not deleted but replaced by `True`,
  which is the same rule with the same arity. Even so, `check_sound` builds
  each rule from the checker's tests and breaks when a test is gone, and a
  helper lemma that unfolds a definition breaks when the definition changes.
* **Everything imports the proofs.** `Corpus.lean` imports `Examples.lean`,
  which imports `Checker.lean`, so a proof kill stops the corpus from being
  run at all, and a witness kill does the same for the corpus.

So every mutant a proof kills is run again with every theorem of L0–L2 given
`sorry` in place of its proof (its statement kept, `Step.lean`'s `demo_*`
witnesses kept): the **without the proofs** column. And every mutant a
witness kills, in the first pass or this one, is run a third time with the
examples, the witness theorems and the `#guard`s given `sorry` as well: the
**corpus and bridge alone** column. Each pass's own baseline reproduces the
first baseline's corpus exactly. A changed case that the mutant's checker
accepts and its machine refuses is a concrete counterexample to `check_sound`
plus soundness; the script lists these, and the discussion below cites them.

**Classifying the proof kills.** For every mutant a proof killed we read the
failing theorem and decided whether the mutant makes a *stated* property false:

* **S**: a headline or linking statement is false for the mutant. Examples
  are `soundness`, `check_sound`, `checkProgram_sound`, `Step.det`,
  `no_double_free`, `drop_order`'s `Lifo`, and `drop_exactly_once` or
  `rest_exactly_once`'s `Exact`. Where the corpus shows a checked program the
  mutant's machine refuses, that is the evidence. Otherwise it is a
  counterexample argued from the mutant.
* **H**: only a helper lemma that restates the definition is false (for
  example `evalIntCast_res` names the trap kind); every headline statement
  still holds.
* **B**: every statement still holds; only a proof script broke (positional
  destructuring, a `simp` or `rw` that no longer fires, a `split` on a
  condition that is now constant).
* **E**: the mutant is equivalent.

The first failing theorem is in the table. It is where the build stopped, not
necessarily the statement that is false. A B kill is a kill by the brief's
rule, but it is not the metatheory noticing anything.

**Time.** A mutant took 18 s to 8 min, median 33 s, all three passes
included: about 60 minutes for all 80 on this machine (an Apple-silicon
laptop, one build at a time), after a warm start of the scratch packages'
`.lake`. A mutant a proof kills early is fast; one that reaches `TraceExact`
takes about two minutes per pass.

**Reproduce.** From a clean checkout, and with the compiler buildable:

```bash
python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --compiler-root "$PWD"
python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --table   # the table below, raw
```

The results go to `<work>/results.json`. A rerun resumes where the previous
one stopped. `--only a,b` runs a subset, and `--redo` reruns it.

## Results

### Mutation score

80 mutants. 4 are equivalent (below), so the denominator is **76**.

| Measure | Killed | Of | Score |
|---|---:|---:|---:|
| **Everything, first killer (the brief's (a)–(e))** | 76 | 76 | **100%** |
| A proof, by the letter (the build breaks in a proof) | 68 | 76 | 89% |
| A proof, semantically: a stated property is false (S; one, `match-consume-skip`, found behind a witness) | 48 | 76 | **63%** |
| A proof, semantically or through a helper that restates a definition (S or H) | 53 | 76 | 70% |
| Without the proofs: witnesses, corpus and bridge | 68 | 76 | 89% |
| The corpus and the bridge alone, before this page's seeds | 50 | 76 | **66%** |
| The corpus and the bridge alone, after them | 56 | 76 | 74% |
| Semantically (S or H), or by any test: the honest union | 75 | 76 | 99%; 100% after the seeds |

The first line is 100%, but it overstates the metatheory. Of the 68 proof
kills, 16 are B kills, where every statement still holds and only a script
broke. The 4 equivalent mutants were "killed" too: all 4 by a proof script, and 3
of those also by `Explain.lean`'s second copy of the checker. So a proof kill
is not evidence by itself. The S column is the metatheory's share.

The one mutant that nothing but a structural proof break noticed is
`breaks-nested`, now pinned by a seed.

### What the proofs kill, and what needs the corpus or the bridge

* **The proofs are strongest on the statics.** A mutant that lets the checker
  (or the rules) accept a program the machine then refuses is an S kill,
  through `soundness`, `check_sound`, `checkProgram_sound` or a lemma they
  rest on. That covers a dropped fully-owned, `@drop`, overwrite, discard,
  leak or join premise, a Copy check, `[e; n]` of a non-Copy element, an
  out-of-range literal, and the §3 class mistakes. For 17 of them the corpus
  holds a concrete counterexample: a seed the mutant's checker accepts and
  its machine refuses (the `unsound` list in `results.json`).
* **Some rules only a proof can see.** Each of these 8 mutants is killed by a
  proof, and by nothing with the proofs off:
  * the rules-only mutants `loop-div-breaks` and `loop-break-div-brk`
    (`Typed` is not executable, and the checker is unchanged);
  * `step-usecopy-nondet`, which only `Step.det` sees, because the corpus
    runs `eval`;
  * `entry-params`, which `checkProgram_sound` sees; the printed corpus
    frames `main` itself, so it cannot express the shape.
  * The other four had no example and no seed before this page:
    `join-residual`, `neg-no-overflow`, `arm-payload-mutable`, and
    `breaks-nested`, whose proof kill is B.
* **The dynamics' functional content is the corpus's and the bridge's.** The
  §6.4 arithmetic mutants are B kills: wraparound instead of a trap, the
  wrong trap kind, `MIN % -1`, a saturating float-to-int, swapped operands.
  So is `bounds-negative`, where a negative index reads element 0. Every
  headline statement still holds for each of them, because they are all
  safe. They are killed by witnesses and by seeds (`overflow`, `div_zero`,
  `i8_rem_min_by_neg_one`, `float_to_int_trap_*`,
  `array_dyn_write_trap_negative`, …). This is by design: the spine
  states safety, not functional correctness, and the corpus compares the
  functional content with the compiler.
* **§6.11's drop order is fixed by a definition, not by a theorem.** The three
  drop-glue mutants change `dropContents` and `dropEvents` together:
  `dtor-skip` (no destructor runs), `dtor-after-fields` and `fields-reverse`.
  Each passes every headline statement. `drop_order`'s `Blocks` is written in
  terms of `dropEvents`, so it follows whatever `dropEvents` says, and
  `no_double_free` counts at most one destructor, which zero satisfies. Only
  helper proofs broke (B). The witnesses and 89, 2 and 24 seeds kill them
  through their destructor lines. By contrast, the frame-exit and
  match-exit order mutants (`scope-fifo`, `payload-order`) do falsify
  `drop_order`'s `Lifo`.
* **Monitors are pinned only by examples.** Removing the `linearLeak`,
  `linearOverwrite`, `linearDiscard` or `ownedUnderCopy` refusal leaves every
  headline statement true (two of them break a helper that restates the
  definition: H). The linear theorems say the machine never refuses a checked
  program, and a machine that never refuses satisfies that. The refusal
  witnesses in `Examples.lean`, `Corpus.lean` and `Trace.lean` kill all four,
  and the seeds whose expected outcome is that refusal kill three. This is the
  red-team log's R3 (RUE-2469), now measured.
* **Trace-only mutants are invisible to the bridge by construction.**
  `seq-droptemp-skip`, `residue-mark-skip` and `match-consume-skip` remove a
  drop *mark* or a `consume` event. None of these is an output line, so no
  compiler comparison can see them. `rest_exactly_once` and the `Exact`
  ledger kill all three semantically. `match-consume-skip` is first stopped
  by `Step.lean`'s `demo_returnInMatch_runs`, which builds before the proofs.
  A run with that demo given `sorry` then broke in `Trace.lean`'s
  `matchConsume_measure`, and `matchConsume_exact`'s equation is false for
  it: the enum shell's identity is never freed.
* **The bridge adds 4 kills** that the seeds do not make. With the witnesses
  off, it catches `zero-array-linear`, `repeat-count`, `binop-eval-order` and
  `decl-cycle-rounds`. The first three were already caught by a witness. The
  bridge is the *first* killer of `decl-cycle-rounds` once the proofs are
  off: 17 generated cases nest declarations deeper than the peel's shortened
  round count, and no seed does.
* **26 mutants got past the corpus and the bridge together**, before this
  page's seeds. They split four ways:
  * 6 are now seeded (below);
  * 4 no corpus case can show: the rules-only `loop-div-breaks` and
    `loop-break-div-brk`, `step-usecopy-nondet` (the corpus runs `eval`) and
    `entry-params` (the printer frames `main`); the proofs kill all four (S);
  * 3 are trace-only (`seq-droptemp-skip`, `residue-mark-skip`,
    `match-consume-skip`), invisible to the bridge by construction and
    killed by the proofs, as above;
  * 13 are refusals that a witness proves but no seed exports, so the
    compiler's matching refusal is never compared: `use-move-rootidx`,
    `index-read-copy`, `index-drop-copy-checker`, `const-index-off-by-one`,
    `index-write-linear`, `residual-declared`, `residual-untracked`,
    `lit-bounds`, `dbg-observable`, `repeat-copy`, `copy-struct-dtor`,
    `dtor-linear-field` (all in `Examples.lean`) and `copy-monitor-off`
    (`Trace.lean`'s `ownedUnderCopy` witness). A proposed issue (below).

### The mutants

**Killed first by** is the first killer in the (a)–(e) order, with the
first failing theorem or example, or the seeds whose outcome changed.
**Proof kill** is our reading of a proof kill: S, H, B or E, as defined in
"Method". **Without the proofs** is the second pass, run only after a proof
kill ("—" otherwise). **Corpus and bridge alone** is the third pass, run
after a witness kill; "(same)" means the earlier pass already ended at the
corpus, the bridge, or `survived`. **s** is the mutant's wall time in seconds,
all passes included. The mutants are defined in `bin/mutate.py` under the same
names. The measurement is before this page's six seeds.

| # | Mutant | § | Rule | Operator | Killed first by | Proof kill | Without the proofs | Corpus and bridge alone | s |
|---|---|---|---|---|---|---|---|---|---|
| 1 | `use-move-partial` | §5.1 | (Use-Move) | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 1179) | corpus: `array_zero_length_moved_twice`, `enum_matched_twice_moving` +1 | 33 |
| 2 | `use-copy-moved` | §5.1 | (Use-Copy) | premise | proof: `soundness` (`Soundness.lean`) | E | witness: `explain_result` (`Explain.lean`) | survived | 64 |
| 3 | `use-move-dtor` | §5.1 | (Use-Move) 3.9:34 | premise | proof: `check_sound` (`Checker.lean`) | B | witness: `Examples.lean` example (l. 2742) | corpus: `partial_under_dtor` | 36 |
| 4 | `use-move-rootidx` | §5.1 | (Use-Move) 3.8:68 | premise | proof: `check_sound` (`Checker.lean`) | B | witness: `Examples.lean` example (l. 1262) | survived | 36 |
| 5 | `use-affine-as-copy` | §5.1 | (Use-Copy)/(Use-Move) | move-copy | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 1170) | corpus: `array_dyn_write_after_field_move`, `array_elem_reinit` +6 | 34 |
| 6 | `use-declared-residue` | §5.1 | (Use-Declared-Linear-Destructure) | premise | proof: `splitResidue_ok` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 2885) | corpus: `destructure_linear_residue` | 37 |
| 7 | `index-read-copy` | §5.1 | (Use-Untrackable-Dynamic-Copy) | copy-check | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 1275) | survived | 33 |
| 8 | `index-drop-copy-checker` | §5.1 | (Use-Untrackable-Dynamic-Copy), @drop | copy-check | proof: `check_sound` (`Checker.lean`) | S | witness: `Examples.lean` example (l. 1305) | survived | 38 |
| 9 | `const-index-off-by-one` | §5.1 | Ty.atPath (7.1:9) | off-by-one | proof: `OwnSt.setAt_wf` (`Statics.lean`) | S | witness: `Examples.lean` example (l. 1276) | survived | 31 |
| 10 | `assign-overwrite` | §5.2 | (Assign) 3.8:77 | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 3138) | corpus: `linear_overwrite`, `overwrite_field_past_partial_linear` +1 | 32 |
| 11 | `assign-array-ok` | §5.2 | (Assign) 3.8:72 | premise | proof: `check_sound` (`Checker.lean`) | B | witness: `Examples.lean` example (l. 1170) | corpus: `array_elem_reinit`, `array_elem_self_assign` | 36 |
| 12 | `assign-immutable` | §5.2 | (Assign) mut | premise | witness: `explain_result` (`Explain.lean`) |  | — | survived | 63 |
| 13 | `index-write-linear` | §5.2 | (Assign) at a dynamic index | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 1278) | survived | 33 |
| 14 | `drop-residual-below` | §5.3 | (@Drop) E0406 | premise | proof: `check_sound` (`Checker.lean`) | B | witness: `Examples.lean` example (l. 2747) | corpus: `linear_field_stranded` | 36 |
| 15 | `drop-moved` | §5.3 | (@Drop) | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 3127) | corpus: `loop_moved_prev_iteration`, `loop_nested_move_outer` +1 | 33 |
| 16 | `seq-discard` | §5.3 | (Seq) 3.8:64 | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Corpus.lean` example (l. 952) | corpus: `linear_temporary_discarded` | 33 |
| 17 | `join-owned-wins` | §5.5 | join | join | proof: `ownedJoinOkList_of_residualLinearFields_false` (`Statics.lean`) | S | witness: `Examples.lean` example (l. 3183) | corpus: `loop_moved_prev_iteration`, `loop_nested_move_outer` | 28 |
| 18 | `join-linear-disagree` | §5.5 | join 3.8:50 (E0443) | join | proof: `ownedJoinOk_residualLinear` (`Statics.lean`) | S | witness: `Examples.lean` example (l. 1228) | corpus: `array_linear_elem_one_path`, `destructure_one_arm` +6 | 34 |
| 19 | `join-residual` | §5.5 | join (residual reading) | join | proof: `OwnSt.join_movedOut_left` (`Statics.lean`) | S | survived | (same) | 52 |
| 20 | `join-diverge-arm` | §5.5/§5.7 | join over Ω (Sub-Never) | join | proof: `Ctx.joinOpt_skel` (`Statics.lean`) | S | witness: `Examples.lean` example (l. 3814) | corpus: `loop_nested_move_outer` | 27 |
| 21 | `meet-never` | §5.5/§5.7 | (If) arm type, (Sub-Never) | completeness | proof: `CTy.meet_fits` (`Checker.lean`) | B | witness: `Examples.lean` example (l. 3710) | corpus: `if_panic_arm_linear`, `if_return_arm_affine` +7 | 31 |
| 22 | `first-arm-ty` | §5.5 | (Match) arm type | completeness | witness: `Examples.lean` example (l. 2786) |  | — | corpus: `enum_return_past_payload`, `match_never_first_arm` +1 | 18 |
| 23 | `match-exhaustive` | §5.5 | (Match) exhaustiveness | equivalent-candidate | proof: `soundness` (`Soundness.lean`) | E | witness: `explain_result` (`Explain.lean`) | survived | 64 |
| 24 | `arm-leak` | §5.5/§5.6 | (Match) arm scope exit | premise | proof: `TypedArms.at_index` (`Statics.lean`) | S | witness: `Examples.lean` example (l. 2826) | corpus: `enum_arm_leaks_payload` | 26 |
| 25 | `arm-payload-mutable` | §5.5 | (Match) payload binders | premise | proof: `Ctx.skel_armCtx` (`Statics.lean`) | H | survived | (same) | 52 |
| 26 | `let-leak` | §5.6 | (Let) scope exit | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 1266) | corpus: `linear_leaked`, `struct_linear_field_leaked` | 33 |
| 27 | `residual-declared` | §5.6 | residual-linear (3.8:74) | affine-linear | proof: `residualLinear_mult_linear` (`Statics.lean`) | S | witness: `Examples.lean` example (l. 2954) | survived | 27 |
| 28 | `residual-untracked` | §5.6 | residual-linear, untracked residue | affine-linear | proof: `ownedJoinOkList_residualLinearFields` (`Statics.lean`) | S | witness: `Examples.lean` example (l. 1266) | survived | 27 |
| 29 | `return-leak` | §5.7 | (Return-Value) | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 3131) | corpus: `return_past_linear` | 32 |
| 30 | `break-leak` | §5.7 | (Loop-Break) loop locals | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 3874) | corpus: `loop_break_past_linear` | 32 |
| 31 | `loop-div-breaks` | §5.7 | (Loop-Div) | premise | proof: `soundness` (`Soundness.lean`) | S | survived | (same) | 55 |
| 32 | `loop-break-div-brk` | §5.7 | (Loop-Break), no reachable exit | premise | proof: `soundness` (`Soundness.lean`) | S | survived | (same) | 55 |
| 33 | `loop-head-unverified` | §5.7 | loop head (LoopHead) | premise | proof: `check_sound` (`Checker.lean`) | E | witness: `explain_result` (`Explain.lean`) | survived | 64 |
| 34 | `head-iter-bound` | §5.7 | loop head iteration | completeness | witness: `Examples.lean` example (l. 3798) |  | — | corpus: `loop_reassign_then_move` | 18 |
| 35 | `breaks-nested` | §5.7 | Expr.breaks | premise | proof: `eval_quiet` (`TraceExact.lean`) | B | survived | (same) | 117 |
| 36 | `fn-exit-leak` | §5.8 | (Fn) exit edge | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 3132) | corpus: `linear_param_leaked` | 33 |
| 37 | `fn-params-order` | §5.8 | (Fn) entry context | order | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 2976) | survived | 35 |
| 38 | `entry-params` | §6.12 | top-level main() | premise | proof: `checkProgram_sound` (`Checker.lean`) | S | survived | (same) | 62 |
| 39 | `lit-bounds` | §5.8 | (Lit) | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 3592) | survived | 32 |
| 40 | `dbg-observable` | §5.8 | (Dbg) | premise | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 3124) | survived | 33 |
| 41 | `repeat-copy` | §5.8 | array repeat (7.1:36) | copy-check | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 1274) | survived | 33 |
| 42 | `class-not-infectious` | §3 | class of a struct (Attr.lift) | affine-linear | proof: `StructDecl.Wf.field_not_linear` (`Statics.lean`) | S | witness: `Examples.lean` example (l. 1057) | corpus: `affine_explicit_drop`, `affine_overwrite` +94 | 30 |
| 43 | `mult-join-meet` | §3 | class join | affine-linear | proof: `Mult.rank_le_join_left` (`Statics.lean`) | S | witness: `Examples.lean` example (l. 1057) | corpus: `affine_explicit_drop`, `affine_overwrite` +94 | 30 |
| 44 | `zero-array-linear` | §3 | class of [T; 0] (3.8:74) | affine-linear | proof: `Ty.array_mult_linear` (`Statics.lean`) | H | witness: `Print.lean` example (l. 729) | bridge: compiler disagrees on `gen_7_185` | 37 |
| 45 | `copy-struct-dtor` | §3 | @copy struct (3.9:31) | premise | proof: `checkStructDecl_sound` (`Checker.lean`) | S | witness: `Examples.lean` example (l. 3164) | survived | 35 |
| 46 | `dtor-linear-field` | §3 | destructor with a linear field (3.9:44) | premise | proof: `checkStructDecl_sound` (`Checker.lean`) | S | witness: `Examples.lean` example (l. 3171) | survived | 18 |
| 47 | `decl-cycle-rounds` | §3 | acyclicity 3.0:5 (E0483) | completeness | proof: `checkNoCycle_sound` (`Checker.lean`) | B | bridge: compiler disagrees on 17 generated (`gen_7_127`…) | (same) | 167 |
| 48 | `entry-join-bty` | §5.5 | Entry.join | equivalent-candidate | proof: `Entry.join_assoc` (`Statics.lean`) | E | survived | (same) | 57 |
| 49 | `dyn-move-as-copy` | §6.3 | (D-Use-Move) | move-copy | proof: `stepEval_complete` (`Step.lean`) | S | witness: `demo_dropMoved_runs` (`Step.lean`) | corpus: `array_dyn_read_after_sibling_move`, `array_dyn_write_after_field_move` +24 | 32 |
| 50 | `step-usecopy-nondet` | §6.3 | (D-Use-Copy), Step only | copy-check | proof: `Step.step_eq` (`Step.lean`) | S | survived | (same) | 100 |
| 51 | `bounds-off-by-one` | §6.5 | (D-Index-Trap) | off-by-one | proof: `inBoundsIdx_eq_true` (`Dynamics.lean`) | S | witness: `Examples.lean` example (l. 1175) | corpus: `array_bounds_trap_at_len`, `array_zero_length_dyn_trap` +1 | 19 |
| 52 | `bounds-negative` | §6.5 | (D-Index-Trap) | bounds | proof: `Contents.resolveDyn_ok` (`Soundness.lean`) | B | witness: `Examples.lean` example (l. 1106) | corpus: `array_dyn_write_trap`, `array_dyn_write_trap_negative` | 22 |
| 53 | `bounds-stuck` | §6.5 | (D-Index-Trap) | bounds | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 1101) | corpus: `array_bounds_trap`, `array_bounds_trap_at_len` +6 | 23 |
| 54 | `repeat-count` | §6.5 | array repeat | off-by-one | proof: `soundness` (`Soundness.lean`) | S | witness: `traceEval_res` (`Explain.lean`) | bridge: compiler disagrees on 3 generated (`gen_7_156`…) | 56 |
| 55 | `overflow-wrap` | §6.4 | (D-Arith-Trap) | trap | proof: `intResult_res` (`Soundness.lean`) | B | witness: `Examples.lean` example (l. 3313) | corpus: `i64_min_times_neg1`, `i8_div_min_by_neg_one` +3 | 22 |
| 56 | `divzero-kind` | §6.4 | (D-Div-Trap) | trap | proof: `binOpInt_res` (`Soundness.lean`) | B | witness: `Examples.lean` example (l. 3354) | corpus: `dbg_before_trap`, `div_zero` +1 | 22 |
| 57 | `rem-min-overflow` | §6.4 | (D-Div-Trap), MIN % -1 | trap | proof: `binOpInt_res` (`Soundness.lean`) | B | witness: `Examples.lean` example (l. 3321) | corpus: `i8_rem_min_by_neg_one` | 22 |
| 58 | `operand-swap` | §6.4 | (D-Arith) | operand | proof: `evalBinOp_res` (`Soundness.lean`) | B | witness: `demo_loopTurns_runs` (`Step.lean`) | corpus: the export aborts (stack overflow) | 499 |
| 59 | `gt-off-by-one` | §6.4 | (D-Ord) | off-by-one | witness: `Witnesses.lean` example (l. 247) |  | — | corpus: `loop_break_past_local`, `loop_reassign_then_move` | 57 |
| 60 | `neg-no-overflow` | §6.4 | (D-Neg) | trap | proof: `evalUnOp_int_res` (`Soundness.lean`) | S | survived | (same) | 49 |
| 61 | `cast-kind` | §6.4 | (D-Int-Cast-Trap) | trap | proof: `evalIntCast_res` (`Soundness.lean`) | H | witness: `Examples.lean` example (l. 3331) | corpus: `int_cast_out_of_range` | 23 |
| 62 | `float-to-int-saturate` | §6.4 | (D-Float-To-Int) | trap | proof: `evalFintrin_float_res` (`Soundness.lean`) | B | witness: `Examples.lean` example (l. 3540) | corpus: `float_to_int_trap_inf`, `float_to_int_trap_nan` +1 | 23 |
| 63 | `binop-eval-order` | §6.2 | evaluation order, eval only | order | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 1710) | bridge: compiler disagrees on 2 generated (`gen_7_118`…) | 25 |
| 64 | `index-write-order` | §6.2 | evaluation order, eval only | order | proof: `soundness` (`Soundness.lean`) | S | witness: `Examples.lean` example (l. 1680) | corpus: `array_dyn_write_rhs_first` | 22 |
| 65 | `dtor-skip` | §6.11 | drop glue: destructor | drop-skip | proof: `dropContents_events` (`Soundness.lean`) | B | witness: `demo_dropMoved_runs` (`Step.lean`) | corpus: `affine_explicit_drop`, `affine_overwrite` +87 | 23 |
| 66 | `dtor-after-fields` | §6.11 | drop glue order (3.9:15) | drop-order | proof: `dropContents_struct_events` (`Soundness.lean`) | B | witness: `Examples.lean` example (l. 3442) | corpus: `struct_nested_dtor_drop`, `two_params_dropped_at_pop` | 22 |
| 67 | `fields-reverse` | §6.11 | drop glue order (3.9:15) | drop-order | proof: `dropEventsList_eq_flatten` (`Dynamics.lean`) | B | witness: `Examples.lean` example (l. 1068) | corpus: `array_drop_order`, `array_dyn_read_after_sibling_move` +22 | 18 |
| 68 | `scope-fifo` | §6.9 | frame exit drop order | drop-order | proof: `runAllScopeDrops_ok` (`Soundness.lean`) | S | witness: `demo_returnInMatch_runs` (`Step.lean`) | corpus: `enum_return_past_payload`, `return_past_affine` +2 | 22 |
| 69 | `payload-order` | §6.6 | match arm exit order | drop-order | proof: `soundness` (`Soundness.lean`) | S | witness: `Witnesses.lean` example (l. 187) | corpus: `enum_two_payload_bindings` | 56 |
| 70 | `overwrite-no-drop` | §6.8 | (D-Assign) overwrite drop | drop-skip | proof: `sim_assign` (`Adequacy.lean`) | S | witness: `Examples.lean` example (l. 1078) | corpus: `affine_overwrite`, `array_elem_overwrite` +7 | 36 |
| 71 | `break-skip-local` | §6.10 | (D-Break) unwind | off-by-one | proof: `loop_step` (`Soundness.lean`) | S | witness: `demo_breakDrops_runs` (`Step.lean`) | corpus: `loop_break_past_linear`, `loop_break_past_local` +1 | 27 |
| 72 | `seq-affine-as-linear` | §6.7 | (D-Seq) affine discard | affine-linear | proof: `soundness` (`Soundness.lean`) | S | witness: `traceEval_res` (`Explain.lean`) | corpus: `affine_temporary_discarded` | 53 |
| 73 | `seq-droptemp-skip` | §6.7 | (D-Seq) temporary drop mark | drop-skip | proof: `rest_step` (`TraceExact.lean`) | S | witness: `traceEval_res` (`Explain.lean`) | survived | 95 |
| 74 | `residue-mark-skip` | §6.3 | destructure residue drop mark | drop-skip | proof: `residueMark_measure` (`Trace.lean`) | S | witness: `Examples.lean` example (l. 2921) | survived | 33 |
| 75 | `match-consume-skip` | §6.6 | (D-Match) consume | drop-skip | witness: `demo_returnInMatch_runs` (`Step.lean`) | (S) | — | survived | 24 |
| 76 | `leak-monitor-off` | §6.11 | linearLeak monitor | monitor | witness: `Examples.lean` example (l. 1347) |  | — | corpus: `destructure_linear_residue`, `enum_arm_leaks_payload` +8 | 29 |
| 77 | `overwrite-monitor-off` | §6.8 | linearOverwrite monitor | monitor | witness: `Corpus.lean` example (l. 948) |  | — | corpus: `linear_overwrite` | 26 |
| 78 | `discard-monitor-off` | §6.7 | linearDiscard monitor | monitor | proof: `eval_succ` (`Soundness.lean`) | H | witness: `Corpus.lean` example (l. 950) | corpus: `linear_temporary_discarded` | 23 |
| 79 | `copy-monitor-off` | §6.5 | ownedUnderCopy monitor | monitor | proof: `Cons.intro` (`Trace.lean`) | H | witness: `Trace.lean` example (l. 2159) | survived | 96 |
| 80 | `dyn-residual-declared` | §6.11 | Contents.residualLinear (3.8:74) | affine-linear | witness: `Examples.lean` example (l. 1342) |  | — | corpus: `destructure_linear_residue`, `enum_arm_leaks_payload` +9 | 24 |

### Equivalent mutants

Four mutants are equivalent. Each changes the text of a definition but not
what the definition decides on any state a rule can reach, so no correct test
could tell it from the original. All four were killed anyway, by a proof
script, which is why a proof kill alone is not taken as evidence.

* **`use-copy-moved`** drops fully-owned from the `Copy` use. A `Copy` place
  is never `MovedOut` in any state the checker computes. `@drop` of a Copy
  place moves nothing, a Copy struct's fields are Copy, and the checker's
  loop head is the least one. At run time a Copy value is never a hole. The
  checker's verdicts are unchanged on every seed and generated case. The
  `soundness` proof uses the premise in its frame invariant (it is a B kill),
  and `Explain.lean`'s copy of the checker disagrees.
* **`match-exhaustive`** drops `arms.length = ed.variants.length`. `TypedArms`
  and `checkArms` walk the arms and the variants in step and fail on a
  length mismatch, so the premise is implied. `soundness` used it directly,
  and `Explain.lean` keeps the check.
* **`loop-head-unverified`**: the checker no longer re-checks that its
  iterated loop head solves the head equation. `headIter` returns a candidate
  only when one more step leaves it unchanged, and `check` is deterministic,
  so the check always passes. `check_sound`'s proof reads the conjunct, and
  `Explain.lean` keeps it.
* **`entry-join-bty`** joins at the second entry's declared type. Every join
  is between two entries with the same skeleton (`Ctx.join` is only ever
  applied to two arms of one incoming context), so the two types are equal.
  This answers the red-team log's "`Entry.join` ignores the second entry's
  type" (dropped there, "left to RUE-2465's mutants"): it is harmless.
  `Entry.join_assoc`'s proof unfolds `a.ty`.

### Survivors and their resolution

**Survivors of the brief's (a)–(e): none.** Every non-equivalent mutant was
killed. But a kill by a proof script alone (B), with nothing else noticing,
is not a real kill. And a mutant that only a proof killed (S or H) still
leaves the bridge blind to the same mistake in the compiler. Both kinds are
resolved here:

| Mutant | What noticed it | Resolution |
|---|---|---|
| `breaks-nested` (§5.7, `Expr.breaks` looks inside a nested loop) | a B kill only (`eval_quiet`'s script); every statement holds, since typing the outer loop `unit` rather than `never` only refuses more | **pinned**: seed `loop_inner_break_outer_return` (an outer loop that only an inner `break` exits, leaving by `return`) |
| `join-residual` (§5.5, `MovedOut` joins a partially moved node without the residual check) | S: `soundness` is false. The mutant accepts a leak, and the new seed shows the checked program refused (`linearLeak`). The first failure was the helper `OwnSt.join_movedOut_left` | **pinned**: seed `join_moved_vs_partial_linear`, and an `Examples.lean` witness |
| `arm-payload-mutable` (§5.5, payload bindings are `mut`) | H only (`Ctx.skel_armCtx` restates `armCtx`). Mutability is not a safety property, so no headline statement notices | **pinned**: seed `match_payload_assign` (the compiler refuses: "cannot assign to immutable") |
| `neg-no-overflow` (§6.4, `-MIN` wraps) | S: `soundness`'s value typing (`evalUnOp_int_res`). No seed negated `MIN` | **pinned**: seed `i8_neg_min` (traps with overflow) |
| `fn-params-order` (§5.8, parameters bound in the wrong order) | S (`soundness`) and an `Examples.lean` witness; no seed, because every multi-parameter seed takes one type | **pinned**: seed `params_two_types` |
| `assign-immutable` (§5.2, the `mut` premise dropped) | `Explain.lean`'s copy of the checker only; no statement, no example, no seed | **pinned**: seed `assign_immutable` |
| `loop-div-breaks`, `loop-break-div-brk` (§5.7, rules only) | S (`soundness`); the checker is unchanged, so no test can see a rules-only change | none needed: the proof is the right detector |
| `step-usecopy-nondet` (§6.3, `Step` only) | S (`Step.step_eq`, `Step.det`); the corpus runs `eval` | none needed |
| `entry-params` (§6.12, checker only) | S (`checkProgram_sound`) | none needed: the printer frames `main`, so a seed cannot express it |

After the six seeds (commit `eb532d8c5`), each of the six pinned mutants was
run again on the new sources. With the proofs off, each is killed by its new
`Examples.lean` witness. With the witnesses off too, each is killed by its new
seed; `assign-immutable` is killed by `match_payload_assign` as well. All six
seeds agree with the compiler (`bin/verify.py`: 180 seeds, the one
disagreement the allowed red).

### Proposed issues (for the coordinator)

1. **[Formal/Bridge] Seed the refusals only a witness proves.** 13 mutants
   are killed by an `Examples.lean` (or `Trace.lean`) refusal witness and by
   no seed or generated case. Each is a refusal the compiler should make too,
   and the bridge never compares it:
   * moving an element out below a projection;
   * a dynamic-index read, `@drop` or write of a non-Copy or linear element;
   * a constant index equal to the length;
   * a partially reassigned declared-linear struct that leaks;
   * a linear field after a moved slot that leaks;
   * an out-of-range literal;
   * `@dbg` of an aggregate;
   * `[e; n]` of a non-Copy element;
   * a `@copy` struct with a destructor, and a destructor-bearing struct with
     a linear field;
   * the `ownedUnderCopy` refusal.

   Evidence: the 13 mutants of the corpus-blind group above, with the corpus
   and the bridge alone `survived` in the table. At most 6–8 seeds per PR, so
   perhaps two PRs.
2. **[Formal/Assurance] State §6.11's drop order independently of
   `dropEvents`.** `drop_order`'s `Blocks` is defined through `dropEvents`,
   so changing `dropContents` and `dropEvents` together does not falsify any
   headline statement. That covers no destructor at all (`dtor-skip`), the
   destructor after the fields (`dtor-after-fields`), and the fields last to
   first (`fields-reverse`). Evidence: those three mutants are B kills; only
   helper scripts broke. The destructor-first, declaration-order and
   ascending-index claims live in a definition, not in a statement. A
   statement-layer lemma (RUE-2460's Spec) such as "`dropEvents` of a
   destructor-bearing struct starts with its `dtor` event, then its fields'
   blocks in declaration order" would put them in the claim. For the Spec
   layer's review, and for Steve if the order should be a §7 bullet.
3. **[Formal/Assurance] The linear theorems are satisfied by a machine with
   no monitors.** Removing any of the four run-time refusals leaves every
   headline statement true. Evidence: `leak-monitor-off`,
   `overwrite-monitor-off`, `discard-monitor-off`, `copy-monitor-off` and
   `dyn-residual-declared`. This is R3 of the red-team log, measured; a
   comment on RUE-2469 (bring the monitor-fires witnesses into the statement
   layer) rather than a new issue.
4. **(Loop tooling, state-dir `bin/`) Keep the mutants applicable.**
   `bin/mutate.py --check` takes a second and fails when an edit no longer
   matches the sources. Adding it to `chain.sh` would keep the mutants current
   as the definitions change, and REDTEAM.md's cadence could rerun the
   analysis per milestone. The coordinator decides.
5. **FIELD.md: the mutation-analysis terms** (mutant, killed, equivalent
   mutant, mutation score). This repeats RUE-2464's proposal 6. This page
   defines them inline in the meantime.

### Limits

* **The adjudication is by reading.** S, H, B and E are our judgement of each
  failing theorem against the mutant, with a concrete counterexample where
  the corpus has one (17 mutants). A B kill, in particular, is a claim that
  no statement became false. It rests on the argument in the table, not on a
  proof.
* **The first killer hides the rest.** The table's proof column names where
  the build stopped. A later theorem may also be false, and a helper's
  failure may mask a headline's.
* **Seeds and 200 generated cases.** The bridge column uses `--gen 200 --seed
  7`, the per-lane check's. A larger stream might kill more of the 26
  corpus-blind mutants; the 13 refusal shapes above are not ones the
  generator draws.
* **`operand-swap`'s corpus kill is a crash.** With the operands swapped, a
  seed's counted loop never exits, and `ruecore-corpus` overflowed the native
  stack at the export fuel instead of reporting the case as not completed.
  The mutant is killed either way, but the export would crash the same way on
  any non-terminating seed.
* **The equivalence of the two controls** (`match-exhaustive`,
  `entry-join-bty`) and of the two others is argued, not proved.
