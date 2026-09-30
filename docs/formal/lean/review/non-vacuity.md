# No vacuous hypotheses

Integrity angle: any finding blocks.

A kernel-checked statement can still be empty: a checker that accepts nothing
is trivially sound, and a statement over every model of a set of laws holds
vacuously when no model exists. The package answers this with non-vacuity
witnesses and sharpness counter-examples (`../README.md`, "Non-vacuity
witnesses" and "Sharpness counter-examples"). This angle checks that the
change keeps them honest: every hypothesis can hold together with the others
of a program on which the conclusion has content, and each hypothesis is
needed.

## Scope

- Every spine statement the change adds or changes (`RueCore.Spec.spine` in
  `docs/formal/lean/RueCore/Spec.lean`), and every spine statement whose
  hypotheses unfold to a definition the change touches.
- The witnesses (`RueCore.Spec.witnesses`, statements in
  `RueCore/Spec/Nonvacuous.lean`, proofs in `RueCore/Nonvacuous.lean`, glue in
  `RueCore/Nonvacuous/Glue.lean`) and the sharpness counter-examples
  (`RueCore.Spec.sharpness` and `RueCore.Spec.sharpnessReasons`, statements in
  `RueCore/Spec/Sharp.lean`, proofs in `RueCore/Sharp.lean`, glue in
  `RueCore/Sharp/Glue.lean`) the change adds, changes or removes.
- Any new `Prop`-valued definition, structure or hypothesis, in any layer.
- A change to the checker (`RueCore/Checker/Defs.lean`) or the statics: a
  checker that accepts less makes every statement about checked programs say
  less.

## What to run

From `docs/formal/lean`, after `lake build`:

```bash
lake build ruecore-corpus ruecore-explain ruecore-digest ruecore-layers ruecore-lint
lake exe ruecore-lint            # fails on a spine theorem no witness names, a glue theorem missing, a hypothesis with neither counter-example nor reason
lake exe ruecore-digest --spine > "$SCRATCH/SPINE.md"   # each statement's "Non-vacuous" line and numbered hypotheses
git diff <base>...HEAD -- RueCore/Spec.lean RueCore/Spec/Nonvacuous.lean RueCore/Spec/Sharp.lean
```

When the checker or the statics change:

```bash
lake exe ruecore-corpus --profile > "$SCRATCH/profile-head.txt"
git show <base>:docs/formal/lean/README.md > "$SCRATCH/README-base.md"   # "The checker's acceptance profile": the counts at <base>; they can be stale, so a difference the change cannot explain is a note, not a finding
```

Compare the accepted counts at the head with those at `<base>`. A seed case
the checker accepted at `<base>` and rejects now narrows every statement about
checked programs; the description must name it and say why.

The lint checks coverage: that each listed pair has its glue and each
hypothesis its counter-example or reason. It does not check that a witness is
interesting or that a reason is true. Read each new or changed witness and
counter-example statement in `$SCRATCH/SPINE.md`.

## Rules

- **R1. Every hypothesis can hold, together.** Each new or changed spine
  statement has a witness in `RueCore.Spec.witnesses`, applied by its glue
  theorem, whose program meets every hypothesis at once. A statement over
  `M : FloatLaws` rests on `Nonvacuous.exact_model`; a new law added to
  `FloatLaws` must keep that proof.
- **R2. The witness is not trivial.** The witness's program is one on which
  the conclusion has content, and the non-triviality is written in the
  witness statement (a value returned, identities freed, a destructor run,
  a configuration reached). A witness of a run that is refused, runs out of
  fuel or holds an empty trace, for a conclusion about traces or values, is
  a finding. A hypothesis-free statement's witness must apply it to a
  non-trivial program, and `SPINE.md` must say it has no hypotheses.
- **R3. Every hypothesis is needed.** Each hypothesis of a new or changed
  spine statement has a sharpness counter-example, or a reason in
  `sharpnessReasons` that is true (a redundant hypothesis's reason names the
  proved theorem without it). A counter-example whose dropped premise is the
  only tie between a bound value and the program shows only that the
  conclusion is not a tautology; `../README.md` names these cases, and a new
  one must be named too.
- **R4. No unexercised predicate.** A new `Prop`-valued definition,
  structure or hypothesis has, in the change or on trunk, a consuming
  theorem and a witness where it holds non-degenerately. Until then its
  faithfulness cannot be tested.
- **R5. A conclusion a trivial model also meets is not content.** If a
  machine with a check removed, or a checker that accepts nothing, would
  satisfy the new statement as well, say so: the content is elsewhere (a
  witness, a refusal example) and the reading must not claim it.
- **R6. The checker accepts no less, unannounced.** A change that makes
  `checkProgram` reject a seed case it accepted at `<base>` names each such
  case in the description with its reason. An unannounced drop in the
  accepted count is a finding.

## What counts as a finding

A hypothesis shown unsatisfiable, or satisfiable only by programs on which the
conclusion is empty; a witness that is trivial; a hypothesis with no
counter-example and no true reason; a new predicate with no consumer or no
witness; a conclusion shown to hold of a degenerate model. The repro is a
probe (a program and the `#eval` or `decide` result), the lint's output, or
the witness statement quoted beside the conclusion it fails to exercise.

Not a finding here: a statement that is non-vacuous but says something other
than its paragraph (`faithfulness`).

## Severity

Every finding blocks.

## From this package's history

- All 14 statements over `M : FloatLaws` would be vacuous with no model; no
  instance was exhibited. `Nonvacuous.exact_model` now proves every law of the
  executable instance. R1 (`../../REDTEAM-LOG.md`, first full-claim pass,
  L4).
- The three linear theorems are `run … ≠ .refused .linearX`, which an `eval`
  with no monitors also meets. Removing any of the four run-time refusals left
  every stated property true; only refusal witnesses and seeds killed those
  mutants. R5 (first pass R3; RUE-2465 pass D2; `../MUTATION.md`).
- `pendingSafe`, a hypothesis of two spine statements, might have held of no
  program. Dropped on evidence: it is syntactic, and a `#guard` checks every
  accepted seed satisfies it. R1, withdrawn by a repro (first pass,
  "Dropped in adjudication").
- `WfProgram` admits a linear entry-point result; restricting it would empty
  `Nonvacuous.whole_result`, the witness that `whole_program_exactly_once`'s
  "carried out" case occurs. R2 weighed against faithfulness (RUE-2470 run 1,
  S4).
- `drop_order`'s `StackDiscipline` constrains nothing on a step that does not
  pop: its first disjunct holds on every such step. R5, for a conjunct of the
  conclusion (first pass, R7).
