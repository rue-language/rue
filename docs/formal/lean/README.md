# RueCore — Lean 4 mechanization spike

A machine-checked mechanization of a fragment of the Rue core calculus
(`../01-core-calculus.md`), proving the fragment's slice of the §7
memory-safety theorems in Lean 4. This is the spike for making mechanized
proofs part of the formal core; the findings and project outline live in
`../../notes/lean-mechanization-spike.md`.

**Status: complete, zero `sorry`, axioms `propext`/`Quot.sound` only**
(no `Classical.choice`, no `native_decide`). Adopted as the fourth view of the
language by ADR-0097 (`docs/designs/0097-mechanized-formal-core.md`), which
fixes the theorem shape, the authority rule, and the non-blocking posture the
project "Formal core mechanization" grows this seed under.

## Building

Through Buck, with the SHA-pinned toolchain the repository fetches itself
(`toolchains//lean`, no `elan` needed):

```bash
scripts/rue lean          # builds, re-checks with leanchecker, prints the axioms report
./buck2 build root//:lean-ruecore --show-simple-output
```

Or directly, for editor work, with `elan` bootstrapping the same pinned
toolchain from `./lean-toolchain`:

```bash
lake build
```

The two pins are held equal by `scripts/validate-lean-toolchain-pin.py`. The
Buck target is build-only and carries no test tier: nothing in CI runs it
until ADR-0097's gate is met (RUE-2241).

## The bridge corpus (ADR-0097, RUE-2227)

`lake exe ruecore-corpus` (or the `corpus.json` output of `scripts/rue lean`)
prints every corpus case as JSON: the fragment program printed as a complete
Rue module, the verified checker's verdict, and the interpreter's outcome.
`crates/rue-oracle-diff` consumes it (RUE-2228) and runs the compiler, the
oracle, and the native binary on each source. Any pairwise disagreement is a
defect in one of the four views (RUE-305).

One case, abbreviated:

```json
{
  "name": "affine_scope_drop",
  "description": "An affine resource silently dropped at scope exit; ...",
  "rules": ["(Let) §5.6", "endscope §6.7"],
  "source": "// Case: affine_scope_drop\n// ...\nfn main() -> i32 { ... }\n",
  "verdict": {"accept": {"type": "i64"}},
  "expected": {"kind": "ok", "stdout": ["7", "1"], "exit": 0}
}
```

`verdict` is `{"accept": {"type": T}}` or `{"reject": {}}`. `expected` is
`ok` with the stdout lines the native binary must print (one line per drop
event in trace order, then the program's value) and exit 0; `panic` with
`overflow` or `divZero` (the trap ends the process before the value prints,
so only the trap kind is compared; the compiler's runtime reports these as
`error: integer overflow` / `error: division by zero` with exit status 101);
or, for a rejected program, `stuck` with the refusal the machine would
reach, which the bridge cannot observe because the compiler rejects the
program first (the compiler's diagnostics for the seed cases: E0406 linear
leak, E0205 use after move, E0443 join, E0493 linear overwrite, E0478
linear discard).

How a drop event becomes a printed line is decided per multiplicity class
by the spec's destructor rules; `RueCore/Print.lean`'s module docstring is
the reference. In short: a copy value has no destructor and no events; an
affine resource's destructor prints its payload while a `live` flag holds,
and `consume` disarms the husk first; a linear resource cannot carry a
destructor (3.9:34 would forbid the projection `consume` needs), so its
only observable event, an explicit `@drop`, is printed as
`@dbg(consume_linear(x))`. Every printed program opens with a comment naming
its case, its rules, and its expected outcome in words, so `corpus.json`
doubles as a readable example set.

## How to read this, with no Lean

- **A judgment is an inductive type.** The calculus writes
  `Γ; Σ ⊢ e ⇒ T ⊣ Σ'` (§5); `Statics.lean` writes `Typed Γ e T Γ'`. Each
  constructor of `Typed` is one inference rule, its arguments are the rule's
  premises, and its doc-comment names the §5 rule and the prose paragraph it
  encodes. A program is well-typed when a value of `Typed [] e T Γ'` exists.
- **The dynamics is a function.** `Dynamics.lean` defines `eval`, which runs
  a program and returns `.ok store value trace`, `.panic kind` (a defined
  trap, §6.12), or `.stuck violation`. A `Violation` is a named refusal
  (`useAfterMove`, `useAfterDrop`, `linearLeak`, ...): the machine states
  §6 leaves stuck, made explicit. The trace lists every drop in order.
- **The theorem says stuck is unreachable.** `soundness` (`Soundness.lean`)
  states: if `Typed [] e T Γ'` holds, then `eval` never returns `.stuck`.
  The corollaries name one §7 bullet each. `Matches` is the invariant the
  proof carries: "Σ faithfully tracks the store's initialization", with one
  deliberate asymmetry explained in its doc-comment.
- **Run something.** Open `RueCore/Examples.lean`; each `#eval` line runs a
  program, and the editor (or `lake build`'s log) shows its result. Change a
  program and watch the result change. Each `example : check ... = none := by
  rfl` is a kernel-checked rejection.
- **Check what is trusted.** `#print axioms RueCore.soundness` must list only
  `propext` and `Quot.sound`. `scripts/rue lean` prints that report and fails
  if anything else appears.


## What is mechanized

| File | Contents | Calculus |
| --- | --- | --- |
| `RueCore/Syntax.lean` | multiplicity lattice, types, `class(T)`, expressions | §2, §3 |
| `RueCore/Statics.lean` | fused flow-sensitive `Γ;Σ` context, the ownership-threading judgment `Typed`, the §5.5 branch join, skeleton preservation | §4.2, §5.1–§5.3, §5.5–§5.6 |
| `RueCore/Dynamics.lean` | store/env machine as a total definitional interpreter with drop traces; violations as named refusals; overflow/div-zero traps | §6.1–§6.12 |
| `RueCore/Soundness.lean` | value typing, the store–Σ agreement invariant `Matches`, **the safety theorem** and per-§7-bullet corollaries | §7 |
| `RueCore/Checker.lean` | decidable checker `check` + `check_sound` (every acceptance is a derivation) | §5 as an algorithm |
| `RueCore/Examples.lean` | `#eval` demos; kernel-checked acceptance/rejection of example programs | — |
| `RueCore/Print.lean` | core syntax → Rue source, and the observation channel (one printed line per drop event, per multiplicity class) | §2 elaboration inventory, 3.9 |
| `RueCore/Corpus.lean` | the bridge corpus: each case's checker verdict and interpreter outcome, exported as JSON (`lake exe ruecore-corpus`) | §5, §6, §7 witnesses |

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
`Violation` (`useAfterMove`, `useAfterDrop`, `linearLeak`,
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
