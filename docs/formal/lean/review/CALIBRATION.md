# Calibration: #3243 re-reviewed by angle

Before these angles were adopted, one already-merged Lean pull request was
reviewed again with them, to see what they find that its original review
did not. The pull request is #3243 (RUE-2428), which added `drop_order`: the
within-value drop order and last-in first-out teardown over §6's relation. It
was reviewed at its merged tip, `43a4d39b2` (range `e546a7335..43a4d39b2`).
Each angle's reviewer started from a fresh context, wrote its findings, and
only then read the original review's summary in the pull request's
description.

Several tools the angle files name did not exist at that commit: the
statement layer, Comparator, the trusted-base lint, the layer audit,
`glossary-check`, `SPINE.md` and `simplify-metrics.py`. Reviewers judged by
each angle's question with what did exist, and did not count a missing tool
as a finding. For the same reason the three mechanical integrity angles
(`spine-change`, `trusted-base`, `glossary`) were not run: their checks are
the missing tools.

## What each angle found

| Angle | Findings | New, against the original review | Still true on trunk, 2026-09-30 |
| --- | --- | --- | --- |
| `faithfulness` | 1 blocking | 1 | no: fixed by RUE-2487 |
| `non-vacuity` | 4 blocking | 4 | 1, filed as RUE-2527 |
| advisory (8 angles, one reviewer) | 14 | 14 | 3, added to RUE-2522; the documentation overclaim is RUE-2527 |

The original review ran two rounds. It found one blocking gap, cross-step
order holding only within one step, fixed before merge by `Config.Nested`,
plus wording nits. Both angle reviewers that looked at `Config.Nested`
checked the fix and found it holds.

**`faithfulness`.** The within-value half of `drop_order` took §6.11's order
from `Dynamics.dropEvents`, the machine's own closed form of its walk. A
mutant machine that ran a struct's destructor after its fields still proved
`drop_order` with its statement unchanged. What caught the mutant were `rfl`
examples and a brittle proof elsewhere, not a statement. RUE-2487 later
closed this by stating the order in §6.11's own terms (`drop_glue_order`).

**`non-vacuity`.**

1. The statement quantified over a `FloatModel` the package did not yet
   inhabit, which made it vacuous in Lean. `Nonvacuous.exact_model` later
   closed this (RUE-2469).
2. The last-in first-out conjunct puts no bound on the drops of a step that
   keeps the registration stack, while GUIDE and README said it does. This
   is still true of `StackDiscipline` on trunk: RUE-2527.
3. `ProgramTyped` had no sharpness counter-example. RUE-2485 and RUE-2495
   later added the sharpness pairs.
4. No witness applied the statement's own premises: the named witnesses ran
   the interpreter, not §6's relation. The statement layer's witnesses
   (RUE-2469) later covered this.

**Advisory.** The reviewer found 14 findings:

- `Config.Ordered` is implied by `Config.Nested`, about 110 redundant lines.
- A copied `Steps` induction, which is RUE-2519.
- A fifth copy of the fuel induction over the interpreter, which is RUE-2518.
- Duplicated lemma forms.
- An unused hypothesis.
- Names that don't describe their conclusions.
- General helpers in the proof module that first needed them.
- A 249-line proof with no structure.
- The GUIDE overclaim above.

None of the 14 was in the original review.

## What this says about the angles

The angles found what a single reviewer with a risk list did not.

- **Blocking gaps.** Five gaps that block, each of which the project later
  found and fixed by other means: red-team passes, mutation testing,
  sharpness pairs. One is still open.
- **Maintenance debt.** The simplification work (RUE-2471) independently
  rediscovered much of the same debt, days later.

The cost was three reviewers, each taking roughly five to eight minutes.
Most of the non-vacuity findings would have needed tools the package built
afterwards; the question the angle asks found them without those tools.

The reports are the loop's working files. This page is their summary.
