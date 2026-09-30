# Review angles for a Lean lane

A pull request that changes this package is reviewed from several angles, each
by its own reviewer with a fresh context. A reviewer's prompt is
[`_common.md`](_common.md), the shared protocol, followed by one angle file.
Each angle asks one question and names the commands that answer it, so two
reviewers of the same angle should reach the same findings, and a reviewer
of one angle leaves the others alone.

The scheme follows [TauCetiReview](https://github.com/TauCetiProject/TauCetiReview)'s
rubrics (`rubrics/README.md`, `rubrics/_common.md` and one file per angle, read
at commit `dcc918abe55f0300e74aabda317d0cc00ecaca10`), adapted to what this
package is: a formalization of a language calculus
([`01-core-calculus.md`](../../01-core-calculus.md)), with no Mathlib, whose
claim is the statement layer ([`../README.md`](../README.md), "The statement
layer") and which is tested against the compiler by differential testing. The
per-lane review here is not the red-team program
([`../../REDTEAM.md`](../../REDTEAM.md)): that attacks the claim as a whole,
on a cadence; this reviews one change before it merges.

## Integrity and advisory angles

An **integrity** angle guards what the mechanization claims: that the
statements say what the calculus says, that they are not vacuous, that the
claim changed only under review, that the proofs rest on nothing outside the
trust bar, and that the words the documents use are defined. Any finding of an
integrity angle blocks the merge until it is fixed, or until the reviewer,
shown the fix or the counter-evidence, withdraws it.

An **advisory** angle guards the package's quality: reuse, placement, names,
documentation, proof style. Its findings do not block. The implementer fixes
each, files it as its own issue, or declines it with a reason in the pull
request's description.

| Angle | Question | Kind |
| --- | --- | --- |
| [`faithfulness`](faithfulness.md) | Does each new or changed statement and definition say what the calculus paragraph it cites says? | integrity |
| [`non-vacuity`](non-vacuity.md) | Can every hypothesis hold, of a program on which the conclusion has content, and is each one needed? | integrity |
| [`spine-change`](spine-change.md) | Did any spine statement change, and if so, with a clean Comparator run and the change stated? | integrity |
| [`trusted-base`](trusted-base.md) | Is the trusted-base lint clean, does the kernel re-check pass, and is every change to `TRUST.md` intended? | integrity |
| [`glossary`](glossary.md) | Is every term the change's prose uses in the glossary, with the right class? | integrity |
| [`reuse`](reuse.md) | Does it reuse Lean core and the package's own lemmas rather than re-prove them? | advisory |
| [`scope`](scope.md) | Is the change its issue's scope, and one topic? | advisory |
| [`attribution`](attribution.md) | Does it credit the external formal and informal work it follows? | advisory |
| [`api-design`](api-design.md) | Does each new definition come with the lemmas that characterize it, and carry no free data? | advisory |
| [`generality`](generality.md) | Are the lemmas at their natural level, with no unused assumption? | advisory |
| [`placement`](placement.md) | Is each declaration in its layer's canonical module? | advisory |
| [`naming`](naming.md) | Do names describe conclusions, in the terms the package already uses? | advisory |
| [`documentation`](documentation.md) | Are the doc-comments and docs accurate, cited, and no stronger than the statements? | advisory |
| [`proof-quality`](proof-quality.md) | Are the proofs robust, readable and free of undocumented definitional tricks? | advisory |

## How TauCeti's angles map here

| TauCeti angle | Here | Reason | Our file |
| --- | --- | --- | --- |
| `correctness` (may block) | adapt, split in two | Its two halves are separate risks here with separate evidence: fidelity to a cited calculus paragraph is judged by reading, vacuity by the witnesses and sharpness counter-examples the package already keeps. Each gets its own reviewer. | [`faithfulness`](faithfulness.md), [`non-vacuity`](non-vacuity.md) |
| `reuse` (blocks outright duplication) | adapt, advisory | No Mathlib: the library to search is Lean core and the package itself. A duplicate lemma costs maintenance, not truth, so it does not block. | [`reuse`](reuse.md) |
| `scope` (may block) | adapt, advisory | The roadmap is the Linear issue and the project's milestone ladder, and the loop coordinator already holds each lane to one issue. A statement change the issue did not ask for is caught by `spine-change`, which blocks. | [`scope`](scope.md) |
| `attribution` (blocks clear missing credit) | adapt, advisory | The package rarely follows external formal code; citing the calculus is enforced by the cross-reference index; the literature a statement corresponds to is in the `SPINE.md` table. Uncredited vendored code goes to the coordinator as a human decision. | [`attribution`](attribution.md) |
| `api-design` | adapt, advisory | The module system exposes every body by design (`../README.md`, "Layers"), so TauCeti's "keep bodies hidden" is inverted; the characteristic-API and free-data rules stand. | [`api-design`](api-design.md) |
| `generality` | adapt, advisory | Unused hypotheses of a spine statement are an integrity matter (`non-vacuity`); for other lemmas the rule stands, measured against the package's conventions rather than Mathlib's. | [`generality`](generality.md) |
| `placement` | adapt, advisory | The canonical home is the layer table (L0, L1, Spec, L2, L3), whose imports `ruecore-layers` checks; the Mathlib directory rules do not apply. | [`placement`](placement.md) |
| `naming` | adapt, advisory | Conclusion-describing names stand. The Mathlib naming guide TauCeti vendors (`rubrics/references/naming-conventions.md`) is not adopted: this is not a Mathlib project (`../TOOLING.md`, "What we decline"). The field's words come from [`../../FIELD.md`](../../FIELD.md). | [`naming`](naming.md) |
| `documentation` | adapt, advisory | The package's doc-comment convention and its citation gate replace Mathlib's docstring rules. An overclaiming reading of a spine statement is `faithfulness`'s, and blocks. | [`documentation`](documentation.md) |
| `proof-quality` | adapt, advisory | No Mathlib renames to be brittle against; the brittleness here is heartbeat budgets and a lemma replacement that widens the axioms, the second of which `trusted-base` blocks. | [`proof-quality`](proof-quality.md) |
| (none) | ours | A change to a spine statement is a change to the claim, which the fingerprints and Comparator make visible. | [`spine-change`](spine-change.md) |
| (none) | ours | TauCeti's CI runs the axiom audit before any reviewer; no CI lane builds this package yet (RUE-2241), so the reviewer runs the lint and the kernel re-check. | [`trusted-base`](trusted-base.md) |
| (none) | ours | The formal documents define every term they use ([`../../GLOSSARY.md`](../../GLOSSARY.md)). | [`glossary`](glossary.md) |

TauCeti's reviewers run only after CI is green and never re-check the build or
the axioms. Here the Lean build is not in CI yet, so the integrity angles run
the package's own gates themselves; the implementer's reported output is
evidence the implementer wrote, not a result the reviewer may rely on.

## How a lane uses them

1. The lane's implementer finishes, and the lane's own checks pass.
2. For each integrity angle, one reviewer: a fresh context, never the lane's
   implementer, given `_common.md`, that angle's file, and the lane's inputs
   (below). Reviewers of different angles do not see each other's reports in
   the first round. How many run at once is the coordinator's concurrency
   limit; when they must run one at a time, the mechanical integrity angles
   (`spine-change`, `trusted-base`, `glossary`) go first, as the cheapest,
   then `faithfulness` and `non-vacuity`.
3. The advisory angles may share one reviewer, given `_common.md` and every
   advisory angle file, reporting under each angle's heading.
4. Only integrity findings block. The implementer answers each finding: a
   fix, or evidence that it does not hold. The same angle's reviewer, in a
   second round, withdraws or confirms it ("Contested findings" in
   `_common.md`). A finding two rounds cannot settle, or one that turns on
   what the calculus should say, goes to a maintainer.
5. The pull request's description lists each angle's verdict, the blocking
   findings and their resolution, and the advisory findings deferred or
   declined, with a reason each.

A lane's inputs, given to every reviewer: the checkout path, the base commit
the change is measured against, the issue's text, the pull request's
description (or the implementer's plan), and a scratch directory outside the
checkout for regenerated files and probes.

## What a finding contains

Every finding, whatever the angle, has:

- **the rule** it breaks: the angle file and the rule's number (`faithfulness
  R2`);
- **the location**: `path:line`, from the repository root (line 0 for a
  change-wide finding);
- **a repro**: the command run and the lines of its output that show the
  problem, or the probe file's text and what Lean said, or the quoted lines
  of the two texts that disagree. A suspicion the reviewer could not
  reproduce is not a finding; the reviewer may list it as a note;
- **the fix**: a concrete change.

The full format is in [`_common.md`](_common.md), "Output".

[`CALIBRATION.md`](CALIBRATION.md) records what the angles found when #3243 was reviewed again with them.
