# The Rue Core Metatheory

The proofs of `01-core-calculus.md` §7, as they are discharged in the
mechanization (`lean/`, package `RueCore`; ADR-0097). One section per §7
bullet. Each names the Lean theorem that establishes it, the fragment of the
core it covers, and the assumptions it takes. A bullet whose section says
*not yet mechanized* is a promise, not a result; the issue named there tracks
it. This document is filled in by the "Formal core mechanization" project and
is complete when no section says so (RUE-207).

**How to read a theorem here.** The mechanization proves safety over a
definitional interpreter, `eval`, rather than over §6's small-step relation
(ADR-0097, decision 3): a well-typed program evaluates to a well-typed value,
a defined panic, or exhausted fuel, and never to a named stuck state. Each
theorem below is stated in that form. Its agreement with §6's reduction is
the adequacy lemma owed in the last section, which ADR-0097 requires before any CI promotion (RUE-2289). To check any claim yourself:
`scripts/rue lean` builds the package, re-checks it, and prints the axioms
every listed theorem depends on; the reading guide in `lean/README.md` is the
entry point for a reader with no Lean.

**Fragment today.** Scalars (`int` as `int(64, signed)`, `bool`, `unit`) and
an abstract resource type standing in for a monomorphic struct of each
multiplicity class; use (copy/move), `@drop`, `let` with scope-exit drop,
assignment with reinitialization, sequencing with the discard check, `if`
with the §5.5 branch join, and `+`/`/`/`<` with the §6.4 traps. Whole
bindings only. No structs with fields, paths, enums, arrays, calls, loops,
loans, or buffers.

**Axioms.** Every theorem below depends on `propext` and `Quot.sound` only;
the build fails otherwise (`toolchains/lean/defs.bzl`).

---

## Type safety (progress + preservation)

- **Theorem:** `RueCore.soundness`
  (`lean/RueCore/Soundness.lean`).
- **Statement, in words:** if `Typed [] e T Γ'` holds, then `eval [] [] e` is
  either a defined panic or a well-typed value whose final store satisfies
  `Matches Γ'`; it is never a `Violation`. Progress and preservation in one
  statement, because the interpreter is total on this fragment.
- **Invariant:** `Matches` — "Σ faithfully tracks the store's
  initialization" (§7). Its `CellMatches` clause is deliberately asymmetric:
  a statically `MovedOut` cell may still hold a live *non-linear* value (the
  §5.5 conservative join, `3.8:73`), never a live linear one (`3.8:50`).
- **Covers:** the fragment above. **Owed:** every Phase C slice re-establishes
  this theorem for its forms (RUE-2230 through RUE-2237, RUE-2282); fuel
  enters with calls (RUE-2233), with monotonicity and no-masking lemmas.

## No use-after-move

- **Theorem:** `RueCore.no_use_after_move`.
- **In words:** no evaluation of a well-typed program touches a `⊘` cell.
- **Covers:** whole bindings. **Owed:** paths make the invariant recursive
  (RUE-2231).

## No double-free

- **Not yet mechanized.** The drop trace makes double frees visible; the
  theorem over minted value identities is RUE-2237.

## No use-after-drop / no leak of drops

- **Theorem:** `RueCore.no_use_after_drop` — the "never read afterward"
  half: no evaluation touches a retired (`†`) cell. At fragment scope this
  is structural rather than a consequence of typing: the interpreter
  resumes a `let`'s caller with the original environment, so no closed
  expression, well-typed or not, can name a retired cell. The guard itself
  is witnessed from an open machine state (`Examples.lean`); the bullet
  becomes falsifiable once scope records and unwind paths can retain a
  retired location (RUE-2233).
- **Owed:** the "exactly once, at the end of its scope" half needs the σ
  scope records and the unwind paths (RUE-2233), then the trace theorem
  `drop_exactly_once` (RUE-2237).

## No use-after-free

- **Not yet mechanized.** This is the §6.13 buffer bullet; it needs the
  allocation store and the §6.13.5 obligations as explicit interfaces
  (RUE-2240), after loans (RUE-2238).

## Linear values are consumed exactly once

- **Theorems:** `RueCore.no_linear_leak` (§5.6 scope exit),
  `RueCore.no_linear_overwrite` (§5.2, `3.8:77`),
  `RueCore.no_linear_discard` (§5.3, `3.8:64`).
- **In words:** a well-typed program never reaches the refusal the machine
  raises when a linear value would be leaked, overwritten while live, or
  discarded.
- **Covers:** whole bindings and the binary join. **Owed:** declared-linear
  destructure and residue ordering (RUE-2236), enums (RUE-2232).

## Exclusivity / no aliased mutation

- **Not yet mechanized.** Λ is ambiently empty in this fragment; the
  theorem and §7's four owed lemmas (loan/drop non-interference, loan-extent
  nesting, root separation, view-intact) are RUE-2238.

## Lemmas §7 owes, and the metatheory's own

| Lemma | Status |
|---|---|
| Totality of the float operations | not yet stated; an assumption about IEEE 754, named as such (RUE-2282) |
| Handle-uniqueness preservation (O1) | not yet mechanized (RUE-2240) |
| Adequacy of `eval` to §6's reduction, and progress/preservation derived over the mechanized relation | not yet stated; a Phase C deliverable required at checkpoint C and the CI gate (RUE-2289). Its domain is the programs `check` accepts: there `eval`'s `ok`/`panic` outcomes must agree with §6's values and panics, and neither side gets stuck. `.stuck` is outside the correspondence, because three of `eval`'s refusals are monitors §6 does not have, and `eval` refuses on an operand's shape before evaluating the next operand where §6.2's `v ⊕ E` context reduces that operand first (`Dynamics.lean`, "the correspondence with §6") |
| Fuel monotonicity and no masking | not yet stated; arrives with fuel (RUE-2233) |

## Traceability

`lean/INDEX.md` is the generated index (`scripts/validate-lean-xref-index.py`,
held fresh by the premerge tier): every declaration with the calculus rules,
sections, and prose paragraphs its doc-comment cites, and every labeled rule
and section of §5 and §6 with the declaration that mechanizes it or *not yet
mechanized*. The theorem side of the index, one row per §7 bullet naming its
theorem, is this document; RUE-207 completes it.
