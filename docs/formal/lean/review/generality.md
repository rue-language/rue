# Generality

Advisory angle: findings do not block.

Are the lemmas at their natural level? This is quality, not truth: a spine
statement's hypotheses are `non-vacuity`'s (each must be needed), and an
assumption that changes what a statement claims is `faithfulness`'s.

## Scope

Every new or changed theorem in L0 to L2 that is not on the spine.

## What to run

From `docs/formal/lean`, a probe in `$SCRATCH` restating a lemma without the
assumption you suspect, proved by the same script:

```bash
lake env lean "$SCRATCH/Probe.lean"
```

## Rules

- **R1. No unused assumption.** A hypothesis the proof never uses, or one
  implied by the others. Show the probe that proves the lemma without it.
- **R2. The general result first.** A lemma proved for a special case, when
  the change proves or could readily prove the general one and derive the
  case.
- **R3. Not speculative either.** A parameter that never takes a second
  value, or an abstraction with one instance and no issue that needs more.

## From this package's history

None recorded yet: the package's lemmas have not been reviewed from this
angle. Unused hypotheses of spine statements are measured by the sharpness
counter-examples (`non-vacuity` R3).
