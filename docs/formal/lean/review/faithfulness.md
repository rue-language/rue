# Faithfulness to the cited calculus paragraph

Integrity angle: any finding blocks.

A proof the kernel accepts can prove the wrong statement. The kernel checks
proofs; the lint checks that each proof proves its Spec statement; nothing
mechanical checks that a statement, or a definition it unfolds to, says what
the paragraph of the calculus it cites says. That is this angle. Try to show
each new or changed statement and definition wrong, weaker than it reads, or
unfaithful to its paragraph; approve only once you have tried and failed.

## Scope

Everything in the diff that states or defines what the mechanization claims:

- the Spec statements (`docs/formal/lean/RueCore/Spec.lean`,
  `docs/formal/lean/RueCore/Spec/*.lean`) and their doc-comments, which give
  the English reading, the paragraph realized and where the statement is
  narrower;
- the L0 and L1 definitions (`../README.md`, "Layers"), which the statements
  unfold to: each rule, constructor or equation against the calculus rule
  (`(Use-Move)`, `(D-Let)`), section (`§6.11`) or specification paragraph
  (`3.9:28`) its doc-comment cites;
- the `SPINE.md` table's row for a changed statement
  (`RueCore/Literature.lean`), where it says how ours differs from the
  field's form.

A statement or definition the change does not touch is out of scope, unless
the change alters a definition it unfolds to.

## What to run

From the repository root:

```bash
git diff <base>...HEAD --stat -- docs/formal/lean/RueCore docs/formal/01-core-calculus.md
git diff <base>...HEAD -- docs/formal/lean/RueCore/Spec.lean docs/formal/lean/RueCore/Spec
python3 scripts/validate-lean-xref-index.py --lean-dir docs/formal/lean --calculus docs/formal/01-core-calculus.md
```

The last checks that every cited rule label exists and every declaration
cites something; it does not check that a citation is right. From
`docs/formal/lean`, after `lake build`:

```bash
lake exe ruecore-digest --spine > "$SCRATCH/SPINE.md"   # each statement, elaborated, with its reading
lake exe ruecore-digest > "$SCRATCH/DIGEST.md"          # every definition a statement names, in order
```

For each changed statement: read the elaborated form in `$SCRATCH/SPINE.md`,
the definitions it names in `$SCRATCH/DIGEST.md`, and the paragraph it cites
in `01-core-calculus.md`, with the calculus open, not the doc-comment. Write
your own reading first, then compare it with the doc-comment's. Where you
suspect a gap, write a probe: a program or configuration, run through
`eval`, `run`, `step` or `checkProgram` with `#eval` or proved with `decide`,
that the statement admits and the paragraph forbids, or the reverse.

## Rules

- **R1. The statement says what its paragraph says.** Quantifiers, the
  direction of implications, which programs and which runs are covered, and
  the edge cases the paragraph names (a panic, a diverging run, an unwind, an
  empty activation record). A statement narrower than its paragraph is
  faithful only when its doc-comment says where it is narrower.
- **R2. The reading is no stronger than the statement.** The doc-comment's
  English reading, and the `SPINE.md` row, claim nothing the Lean does not
  state. A reading that names a guarantee, where a counter-example program
  shows the statement does not give it, is a finding.
- **R3. The content is not in the hypotheses.** A hypothesis that restates
  the conclusion, a new structure or predicate that turns a consequence into
  an assumption, or a missing prerequisite assumed rather than proved.
- **R4. A definition is the image of the rule it cites.** A rule label in a
  doc-comment is a claim that the declaration is that rule
  (`../README.md`, "Doc-comment convention"). Premises dropped, added or
  weakened against the rule, a side condition moved elsewhere, or a label
  on a declaration that abstracts the rule away without saying so.
- **R5. No placeholder.** No `True`, trivially satisfiable predicate or
  constant function standing in for a notion the paragraph defines, however
  plausibly named.
- **R6. Changed definitions carry their statements with them.** A change to
  an L0 or L1 definition a spine statement unfolds to (its `TRUST.md`
  "Trusted base") changes what every such statement says. Read each affected
  statement again under R1 and R2.

## What counts as a finding

A statement or definition, and a program, configuration or quoted pair of
texts showing that it disagrees with the paragraph it cites, or that its
reading overclaims. The repro is the probe and its result, or the two quoted
texts side by side.

Not a finding here: a statement that holds vacuously (`non-vacuity`), a change
to a spine statement's fingerprint as such (`spine-change`), a prose document
other than the reading and the row (`documentation`).

## Severity

Every finding blocks. If a mismatch turns on what the calculus should say
rather than what it says, report it as a finding and say so: it goes to a
maintainer, and the lane waits.

## From this package's history

- `no_linear_leak`'s reading claimed "no scope exit or unwind meets a live
  linear value". `f1(Tok{}, return 7)`, with `Tok` linear, is checked and
  runs to `ok 7` with the token in no trace: a value abandoned by a sibling's
  `return` is in no drop scope. R2 (`../../REDTEAM-LOG.md`, RUE-2470 run 1,
  S1).
- `freed_once` and `dtor_once` read as properties of the type system; a
  refused or fuel-exhausted run has an empty trace, so on unchecked programs
  the bound rests on `eval`'s refusals. R2 (same entry, S3).
- `WfProgram` admits any entry-point return type where §2 fixes
  `i32 | unit`. Disclosed in the statements' readings rather than
  restricted. R1 with its disclosure (same entry, S4).
- `step_safeAt` states `SafeAt` of `Config.init`: its `∀ C` adds nothing,
  and no configuration typing is preserved. Its name read as the syntactic
  lemma. R2 (first full-claim pass, R4).
- `drop_order`'s `Blocks` was defined through `dropEvents`, so §6.11's order
  was fixed by the machine, not by a statement; three mutants reordering it
  falsified nothing stated. R3: the content was in a definition the
  statement took from the machine (`../MUTATION.md`; RUE-2487 added
  `drop_glue_order`, written from §6.11's equations).
- A comment said `@panic`'s message "is emitted"; it is not modelled. R4
  (RUE-2470 run 1, S5).
