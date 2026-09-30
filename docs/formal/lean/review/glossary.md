# Terminology is in the glossary

Integrity angle: any finding blocks.

The formal documents define every term and symbol they use, in
[`../../GLOSSARY.md`](../../GLOSSARY.md), with its meaning, its source and
whether it is the field's word, a Rue concept the specification names, or our
own. A reader who meets an undefined word, or a standard word in a private
sense, reads the claim wrong. `scripts/glossary-check.py` enforces what it can
see; this reviewer runs it and covers what it cannot.

## Scope

Every prose change under `docs/formal/`, and every doc-comment change in the
L0 and L1 modules (the check reads their Lean names). The documents the check
covers are its `MARKDOWN_DOCS` list (`scripts/glossary-check.py`); the glossary's
opening paragraph names them.

## What to run

From the repository root:

```bash
python3 scripts/glossary-check.py              # never with --write: that edits GLOSSARY.md
git diff <base>...HEAD --stat -- docs/formal
git diff <base>...HEAD -- docs/formal/GLOSSARY.md
```

The check fails on a bold or italic term, a non-ASCII symbol or a
definition-layer Lean name with no row, and on a stale "First use" cell. It
does not see a term written in plain text, a document outside its list, or
whether a row's class and source are right.

## Rules

- **R1. The check passes.** A stale "First use" cell is a finding too (the
  author runs `scripts/glossary-check.py --write` and commits the result);
  the merge queue's glossary gate fails on it.
- **R2. A new term has a row.** A word or phrase the change uses as a
  technical term, emphasized or not, has a row, or is rephrased in words that
  have one. A term used before it is defined in a document counts too.
- **R3. The class is right.** A row classed *standard* links the
  [`../../FIELD.md`](../../FIELD.md) section whose source uses the word in that
  sense; a *Rue-specific, grounded* row cites the specification paragraph; a
  word of ours, or a standard word in our own sense, is *ours, pending audit*.
  Check a *standard* claim against FIELD.md's quoted source.
- **R4. One name per thing.** The change does not introduce a second name for
  a notion the glossary already names, or reuse a name for a second notion
  without a second row.
- **R5. A new formal document is covered.** A new document under
  `docs/formal/` that states or explains the claim is added to the check's
  `MARKDOWN_DOCS` list, or the description says why it is not.

## What counts as a finding

The check's failure; a term, symbol or name with no row; a row with the wrong
class or an unsupported source; a second name for one notion; an uncovered
new document. The repro is the check's output, or the line using the term
beside the glossary's lack of it (`grep -n` of the term in
`docs/formal/GLOSSARY.md`, empty).

## Severity

Every finding blocks. Terms that TauCeti or another project uses for its own
process, quoted as theirs, are not ours to define.

## From this package's history

- Stale "First use" cells bounced two pull requests from the merge queue after
  a rebase, on 2026-09-26: the rebase moved a document's sections and nobody
  reran the check with `--write`. R1.
- The terminology audit (RUE-2466) renamed the "adequacy lemma" to the
  semantic equivalence of `eval` and `Step`: the field's "adequacy" names
  several theorems ([`../../FIELD.md`](../../FIELD.md)), so the word was less
  specific than the claim. The Spec doc-comment keeps the field's name where
  one differs only in wording. R3, R4.
- The red-team program's own vocabulary (red agent, packet, adjudicator,
  non-vacuity witness) was flagged in [`../../REDTEAM.md`](../../REDTEAM.md)
  as ours, not FIELD.md's, and routed to the glossary. R3.
