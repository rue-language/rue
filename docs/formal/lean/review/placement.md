# Placement

Advisory angle: findings do not block.

Where does each new declaration live? The layer table (`../README.md`,
"Layers") fixes what each layer holds, and `lake exe ruecore-layers` fails on
an upward import; this angle judges what the audit cannot, the right module
within a layer.

## Scope

Every new declaration and every new module in the diff, and any change to
`RueCore/Layers.lean`.

## What to run

From `docs/formal/lean`, after `lake build`:

```bash
lake build ruecore-layers && lake exe ruecore-layers | tail -1
git diff <base>...HEAD --stat -- RueCore
```

## Rules

- **R1. The layer fits.** A definition a headline statement mentions is in
  L1 (a `*/Defs` module when it belongs to a proof module's topic); a theorem
  about L0's or L1's definitions is in that layer's `*.Lemmas` module (L2); a
  theorem that mentions an example or corpus program is in L3
  (`Witnesses.lean`); a Spec module holds only statements and lists.
- **R2. The module fits.** Within a layer, a declaration sits beside the
  definition or result it elaborates, in the module whose topic it is. A
  general lemma about `Steps` belongs beside `Steps`, not in the one proof
  module that first needed it.
- **R3. A new module is in the table.** A new module is added to
  `RueCore/Layers.lean` in the right layer, and a proof module whose
  definitions a statement will mention has its `*/Defs` companion.
- **R4. A move updates every import.** A relocation moves every import and
  use, and leaves no forwarding module.

## From this package's history

- L1 once held 181 theorems; they moved verbatim to `Statics/Lemmas.lean`,
  `Dynamics/Lemmas.lean` and `Step/Lemmas.lean`, and `Step.lean`'s demo
  witnesses to `Witnesses.lean` (RUE-2460). R1.
- The shared `Steps` induction the tooling calibration found belongs beside
  `Steps`, not in `TraceOrder.lean` or `Tombstone.lean` (`../TOOLING.md`). R2.
