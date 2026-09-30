# Attribution

Advisory angle: findings do not block. Code vendored or closely adapted from
an identifiable external source with no credit is reported to the coordinator
as well, for a maintainer's decision.

Does the change credit what it follows? Three kinds of source matter here: the
calculus paragraph a declaration mechanizes, the literature a statement
corresponds to, and external formal work (another formalization, a library,
a tool's script) the code follows.

## Scope

Every new declaration, doc-comment and document section in the diff.

## What to run

From the repository root:

```bash
python3 scripts/validate-lean-xref-index.py --lean-dir docs/formal/lean --calculus docs/formal/01-core-calculus.md   # every declaration cites a rule, section or paragraph
git diff <base>...HEAD -- docs/formal
```

The index gate enforces that a citation is present. Whether it is the right
one is `faithfulness` R4's.

## Rules

- **R1. External formal work is credited.** Code adapted from another
  project names it, where it is used (a doc-comment or the module's
  docstring), and in the description.
- **R2. Literature is cited through FIELD.md.** A statement said to
  correspond to a standard theorem cites the [`../../FIELD.md`](../../FIELD.md)
  section that quotes it; the `SPINE.md` table requires this for spine
  statements (`../README.md`, "Against the literature").
- **R3. The calculus is cited in its own terms.** A declaration cites the
  calculus rule, section or specification paragraph it mechanizes, in the
  spellings the index reads (`../README.md`, "Doc-comment convention").
- **R4. No invented requirement.** Routine work that follows nothing in
  particular needs no credit.

## From this package's history

- The lint's axiom pass follows TauCeti's `scripts/Axioms.lean` and says so
  (`../README.md`, "The trusted-base lint"). R1.
- The per-lane review angles follow TauCetiReview's rubrics and name them,
  with the commit read ([`README.md`](README.md)). R1.
