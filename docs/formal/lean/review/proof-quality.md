# Proof quality

Advisory angle: findings do not block.

You judge how proofs are written, not whether they are correct (the kernel's)
or what they mean (`faithfulness`'s). The package's simplification rules and
metrics are in [`../SIMPLIFY.md`](../SIMPLIFY.md); the golf and review
procedures that suit it are in [`../TOOLING.md`](../TOOLING.md).

## Scope

Every new or changed proof in the diff.

## What to run

From `docs/formal/lean`:

```bash
python3 bin/simplify-metrics.py --no-time > "$SCRATCH/metrics.md"   # lines per module, overrides, helper lemmas per spine theorem
lake env lean -DmaxHeartbeats=<n> RueCore/<Module>.lean            # the heartbeat floor of a module the change touches
git diff <base>...HEAD -- RueCore
```

Compare the touched modules' rows with `../SIMPLIFY.md`'s baseline.

## Rules

- **R1. Robust over long rewriting chains.** Prefer `simp`, `omega`,
  `decide` and `cases` to a long chain of named rewrites that breaks when a
  definition changes. A single `simp only` or `rw` step is fine.
- **R2. No undocumented definitional tricks.** A `change`, `show`, or
  reliance on accidental definitional unfolding across a definition carries a
  comment saying why a lemma will not do.
- **R3. Budgets are scoped and small.** A `set_option maxHeartbeats` is on
  one declaration (`… in`), and the change says why; a module's heartbeat
  floor that jumps is a sign the proof should be split.
- **R4. Long proofs are factored.** A proof over about 50 lines either has
  reusable intermediate lemmas pulled out, or comments giving its structure.
- **R5. No redundant steps.** Identical case branches not merged, a `have`
  used once and inlined better, a `revert` the proof does not need.

## From this package's history

- `eval_glue_blocks` was 248 lines, the longest proof in `TraceOrder.lean`;
  the rest of the module's longest are 43 to 63 (`../TOOLING.md`, "The
  module before"). R4.
- Eleven golf edits to `TraceOrder.lean` merged identical `loopIter`/`brk`
  and `callReturn`/`ret` cases and inlined single-use `have`s, for 25 fewer
  lines and no change in build time (`../TOOLING.md`, "Golf, on a scratch
  copy"). R5.
- A proof that times out after a rebase gets a scoped budget on that one
  lemma, not a file-wide option. R3.
