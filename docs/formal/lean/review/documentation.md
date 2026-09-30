# Documentation

Advisory angle: findings do not block.

You judge the doc-comments and documents the change adds or edits. The
cross-reference gate checks that a citation is present; you judge that the
text is accurate and useful. The English reading of a spine statement, and its
`SPINE.md` row, are `faithfulness`'s (an overclaim there blocks); every other
document is yours.

## Scope

Doc-comments and module docstrings in the diff; every changed document under
`docs/formal/`; the generated reports the change should have regenerated.

## What to run

From the repository root:

```bash
python3 scripts/validate-lean-xref-index.py --lean-dir docs/formal/lean --calculus docs/formal/01-core-calculus.md
python3 scripts/validate-doc-links.py
git diff <base>...HEAD -- docs/formal
```

From `docs/formal/lean`, regenerate into `$SCRATCH` and compare:

```bash
lake exe ruecore-digest > "$SCRATCH/DIGEST.md" && diff DIGEST.md "$SCRATCH/DIGEST.md"
lake exe ruecore-digest --map > "$SCRATCH/MAP.md" && diff MAP.md "$SCRATCH/MAP.md"
```

The cross-reference gate above also fails on a stale `INDEX.md`.

## Rules

- **R1. Every declaration follows the doc-comment convention.** It cites what
  it mechanizes, or says `(helper)` (`../README.md`, "Doc-comment
  convention").
- **R2. No document says more than the statements.** A claim that something
  is proved, verified, guaranteed or covered matches a statement, over the
  fragment and under the hypotheses that statement has. "Specified" is not
  "proved"; the fragment is not the whole core.
- **R3. Counts and lists are current.** A number the change moves (theorems,
  witnesses, hypotheses, modules, seed counts) is updated wherever a
  document states it.
- **R4. Generated files are regenerated, not edited.** `INDEX.md`,
  `DIGEST.md`, `TRUST.md`, `SPINE.md`, `MAP.md` equal their generators'
  output.
- **R5. The docstring is about the object.** It says what a declaration is
  and what it is for, not how the proof goes or which issue asked for it.
  Issue numbers in prose that explains a design decision are the package's
  convention and stay.
- **R6. Stale text.** A comment the change makes false, or one describing
  a migration that is complete.

## From this package's history

- `docs/formal/README.md` said the calculus "brings `ArrayBuf`/`StrBuf`
  buffers inside the proved perimeter"; nothing about buffers is
  mechanized. R2 (`../../REDTEAM-LOG.md`, first full-claim pass, R8).
- The same README stated for the whole core what is proved for a fragment,
  and never mentioned `pendingSafe` or that "safe" admits panics and
  divergence. R2 (same pass, R9).
- It called `rue-oracle` "the formal dynamic semantics" after ADR-0097 made
  the Lean `Step` and `eval` the proved one, and `03-metatheory.md` "a
  skeleton" after it was filled in. R6 (same pass, R10).
