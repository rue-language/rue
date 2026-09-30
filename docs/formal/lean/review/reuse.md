# Reuse and duplication

Advisory angle: findings do not block.

A new declaration should not re-prove what Lean core or the package already
has, directly or through a thin wrapper. There is no Mathlib here: the library
to search is Lean core (`Init`, `Std`) and `RueCore` itself.

## Scope

Every new declaration and every proof more than a few lines long in the diff.

## What to run

From `docs/formal/lean`:

```bash
git diff <base>...HEAD -- RueCore
grep -rn '<distinctive identifier or statement shape>' RueCore
python3 bin/simplify-metrics.py --no-time > "$SCRATCH/metrics.md"   # "Duplicated lemma shapes": exact and skeleton duplicates
```

Search Lean core with a probe in `$SCRATCH` (`#check`, `exact?`, `apply?`) run
by `lake env lean`. Compare the duplicated-shape groups at the head with the
same section of `../SIMPLIFY.md` (the baseline) and name any new group.

## Rules

- **R1. No declaration an existing one replaces.** A new lemma with the same
  statement as an existing one, up to renaming, argument order, symmetry or
  unfolding.
- **R2. No re-derived plumbing.** A proof step, or a definition assembled
  from pieces, that a located core or package lemma provides.
- **R3. No near-clone.** A new block with the shape of an existing one (the
  same induction over `Steps`, the same `Sim`/`MSim` pair) that should be one
  shared lemma.
- **R4. No compatibility artifact.** An alias, wrapper or forwarding module
  kept for an old name.
- **R5. But: a replacement keeps the axioms.** A core lemma that would
  replace a package proof must not widen the axioms (`trusted-base` R3); a
  suggestion to reuse one says to check.

Every finding names the located replacement and shows how to use it. Not
every hit is a defect: the witness glue modules restate one statement per
witness by design.

## From this package's history

- `reachable_ordered`, `reachable_nested` and `steps_live` (`Tombstone.lean`) each
  prove "a property every `Step` preserves holds along `Steps`" by the same
  six-line induction; one shared lemma beside `Steps` replaces the three. R3
  (`../TOOLING.md`, "Golf, on a scratch copy").
- `MSim.absorb`/`Sim.absorb`, `MSteps.trans`/`Steps.trans` and six more pairs
  share a skeleton across `TraceWhole` and `Equivalence`. R3
  (`../SIMPLIFY.md`, "Duplicated lemma shapes").
- `no_double_free` (`Trace.lean`, 23 lines) and `no_double_free_of_step`
  (`TracePrefix.lean`, 20 lines) have the same statement up to
  alpha-equivalence, each with its own proof, at the baseline. R1 (`../SIMPLIFY.md`,
  "Duplicated lemma shapes").
- Core's `List.pairwise_lt_range'` replaces `range'_increasing`'s proof, and
  adds `Classical.choice`. R5.
