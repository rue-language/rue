# RueCore — Lean 4 mechanization spike

A machine-checked mechanization of a fragment of the Rue core calculus
(`../01-core-calculus.md`), proving the fragment's slice of the §7
memory-safety theorems in Lean 4. This is the spike for making mechanized
proofs part of the formal core; the findings and project outline live in
`../../notes/lean-mechanization-spike.md`.

**Status: complete, zero `sorry`, axioms `propext`/`Quot.sound` only**
(no `Classical.choice`, no `native_decide`).

## Building

```bash
# elan (the Lean toolchain manager) bootstraps the pinned toolchain from
# ./lean-toolchain (Lean 4.33.1) automatically:
lake build
```

## What is mechanized

| File | Contents | Calculus |
| --- | --- | --- |
| `RueCore/Syntax.lean` | multiplicity lattice, types, `class(T)`, expressions | §2, §3 |
| `RueCore/Statics.lean` | fused flow-sensitive `Γ;Σ` context, the ownership-threading judgment `Typed`, the §5.5 branch join, skeleton preservation | §4.2, §5.1–§5.6 |
| `RueCore/Dynamics.lean` | store/env machine as a total definitional interpreter with drop traces; violations as named refusals; overflow/div-zero traps | §6.1–§6.12 |
| `RueCore/Soundness.lean` | value typing, the store–Σ agreement invariant `Matches`, **the safety theorem** and per-§7-bullet corollaries | §7 |
| `RueCore/Checker.lean` | decidable checker `check` + `check_sound` (every acceptance is a derivation) | §5 as an algorithm |
| `RueCore/Examples.lean` | `#eval` demos; kernel-checked acceptance/rejection of example programs | — |

The fragment: scalars + an abstract resource type `res κ` carrying its
multiplicity class; use (copy/move), `@drop`, `let` scope exit with the
residual-linear leak check, assignment with reinitialization and the
`3.8:77` linear-overwrite premise, sequence discard, `if` with the
conservative branch join, and `+`/`/`/`<` with the §6.4 traps. Whole
bindings only — no projections/partial moves, no borrows, no calls, no
loops (see the outline doc for the milestone ladder that adds them).

## The main theorem

```
theorem soundness :
  Typed Γ e T Γ' → Matches Γ ρ H →
    (∃ k,        eval H ρ e = .panic k) ∨
    (∃ H' v tr,  eval H ρ e = .ok H' v tr ∧ HasTy v T ∧ Matches Γ' ρ H')
```

Type safety in definitional-interpreter form: a well-typed program either
panics (a *defined* trap) or produces a well-typed value — never a
`Violation` (`useAfterMove`, `useAfterFree`, `linearLeak`,
`linearOverwrite`, `linearDiscard`, …). The interpreter is total, so this
is progress and preservation in one statement; `Matches` — "Σ faithfully
tracks the store's initialization" — is the §7 preservation invariant, and
its `CellMatches` clause encodes the deliberate asymmetry of the §5.5 join
(a statically `MovedOut` entry may dynamically still hold a live
*non-linear* value, which the machine then drops path-specifically,
`3.8:73`; a live linear value is never statically lost).

The dynamics deliberately mirror `crates/rue-oracle`: an interpreter
producing a result plus a drop trace. `eval` runs under `#eval`, so every
semantic question ("what does this program drop, in what order?") is
answerable by execution — and the Lean model can seed a differential
harness against the Rust oracle (the Cedar pattern; see the outline doc).
