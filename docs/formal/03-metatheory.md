# The Rue Core Metatheory

The proofs of `01-core-calculus.md` §7, as they are discharged in the
mechanization (`lean/`, package `RueCore`; ADR-0097). One section per §7
bullet. Each names the Lean theorem that establishes it, the fragment of the
core it covers, and the assumptions it takes. A bullet whose section says
*not yet mechanized* is a promise, not a result; the issue named there tracks
it. This document is filled in by the "Formal core mechanization" project and
is complete when no section says so (RUE-207).

**How to read a theorem here.** The mechanization proves safety over a
definitional interpreter, `eval`, rather than over §6's small-step relation
(ADR-0097, decision 3): a well-typed program evaluates to a well-typed value,
a defined panic, or exhausted fuel, and never to a named stuck state. Each
theorem below is stated in that form. Its agreement with §6's reduction is
the adequacy lemma owed in the last section, which ADR-0097 requires before any CI promotion (RUE-2289). To check any claim yourself:
`scripts/rue lean` builds the package, re-checks it, and prints the axioms
every listed theorem depends on; the reading guide in `lean/README.md` is the
entry point for a reader with no Lean.

**Fragment today.** Scalars (`int` as `int(64, signed)`, `bool`, `unit`) and
an abstract resource type standing in for a monomorphic struct of each
multiplicity class; use (copy/move), `@drop`, `let` with scope-exit drop,
assignment with reinitialization, sequencing with the discard check, `if`
with the §5.5 branch join, `+`/`/`/`<` with the §6.4 traps, and — with
RUE-2233 — top-level function definitions, by-value calls with frames and
scope records ((Fn)/(Call) §5.8, (D-Call)/(D-Return-Value) §6.9), and `return`
with its σ unwind ((Return-Value) §5.7, (D-Return) §6.9). Whole bindings only.
No structs with fields, paths, enums, arrays, `inout`/`borrow` parameters,
accessor calls, loops, loans, or buffers.

**Fuel.** Because a callee's body is not a subexpression of its call,
recursion makes the interpreter's recursion unbounded, so `eval` takes a fuel
bound and reports `outOfFuel` when it runs out. Every theorem below is
quantified over the bound, and two lemmas say that quantification is not
vacuous: `RueCore.fuel_mono` (a result other than `outOfFuel` is the result at
every larger bound) and `RueCore.no_masking` (no bound turns a violation into
exhaustion for a program some fuel completes). `outOfFuel` is the
interpreter's admission that it stopped, not a state of §6's machine, and it
is outside the adequacy correspondence for the same reason.

**Axioms.** Every theorem below depends on `propext` and `Quot.sound` only;
the build fails otherwise (`toolchains/lean/defs.bzl`).

---

## Type safety (progress + preservation)

- **Theorems:** `RueCore.soundness`, and `RueCore.run_safe` over a whole
  program (`lean/RueCore/Soundness.lean`).
- **Statement, in words:** if `Typed P R Γ e T Γ'` holds and the frame agrees
  with `Γ`, then at every fuel bound `eval` yields a well-typed value with the
  outgoing context's agreement restored, a value handed back by an unwinding
  `return`, a defined panic, or `outOfFuel`; it is never a `Violation`.
  `run_safe` reads it off for a whole program: a well-formed program either
  exhausts its fuel, traps in a defined way, or produces a value of the entry
  point's declared return type. `RueCore.ProgramTyped.run_safe` is the same
  statement from the packaged hypothesis, with the entry function
  existentially quantified (`∃ fd, P[0]? = some fd ∧ …`) because
  `ProgramTyped` only says one exists; the value's type is still that
  function's declared return type. Progress and preservation in one
  statement, because the interpreter is total.
- **Invariant:** `RueCore.FrameMatches`, in two halves. `Matches` — "Σ
  faithfully tracks the store's initialization" (§7), whose `CellMatches`
  clause is deliberately asymmetric: a statically `MovedOut` cell may still
  hold a live *non-linear* value (the §5.5 conservative join, `3.8:73`), never
  a live linear one (`3.8:50`). And the σ invariant: the frame's scope record,
  read newest-first, *is* its environment, which is what lets `Matches` apply
  to `run-all-scope-drops`'s walk. In this fragment that equation is
  **definitional** — every frame the interpreter builds builds σ and ρ from
  one list — so it is not yet evidence about the RUE-1277 redundancy, which
  was raised for scopes pushed and popped independently of the binder chain
  (§6.6's `match` arms, §6.10's loops). It becomes a real obligation when
  `Frame.scope` is §6.1's stack. `RueCore.Untouched` carries frame locality
  across a call, so a caller's agreement survives a callee's run.
- **Hypothesis:** `RueCore.ProgramTyped` — (Fn) §5.8 for every function plus
  an entry point taking no parameters — which `RueCore.checkProgram_sound`
  decides.
- **Covers:** the fragment above. **Owed:** every remaining Phase C slice
  re-establishes this theorem for its forms (RUE-2230 through RUE-2237,
  RUE-2282).

## No use-after-move

- **Theorem:** `RueCore.no_use_after_move`.
- **In words:** no evaluation of a well-typed program touches a `⊘` cell.
- **Covers:** whole bindings. **Owed:** paths make the invariant recursive
  (RUE-2231).

## No double-free

- **Not yet mechanized.** The drop trace makes double frees visible; the
  theorem over minted value identities is RUE-2237.

## No use-after-drop / no leak of drops

- **Theorem:** `RueCore.no_use_after_drop` — the "never read afterward"
  half: no evaluation touches a retired (`†`) cell. With frames this is a
  consequence of the invariant rather than a structural fact about closed
  expressions: `run-all-scope-drops` (§6.9) walks the frame's scope record at
  every `return` and at every frame pop, and it is `FrameMatches` — the record
  *is* the environment, whose cells `Matches` says are live or moved out and
  pairwise distinct — that keeps those walks off a `†` cell and stops any cell
  being retired twice. The guard is also witnessed directly from an open
  machine state, including one whose scope record names a retired cell
  (`Examples.lean`).
- **Owed:** the "exactly once, at the end of its scope" half is the trace
  theorem `drop_exactly_once` (RUE-2237); the σ records and unwind paths it
  quantifies over are in place.

## No use-after-free

- **Not yet mechanized.** This is the §6.13 buffer bullet; it needs the
  allocation store and the §6.13.5 obligations as explicit interfaces
  (RUE-2240), after loans (RUE-2238).

## Linear values are consumed exactly once

- **Theorems:** `RueCore.no_linear_leak` (§5.6 scope exit, and §6.9's frame
  teardown at an early `return` or a frame pop),
  `RueCore.no_linear_overwrite` (§5.2, `3.8:77`),
  `RueCore.no_linear_discard` (§5.3, `3.8:64`).
- **In words:** a well-formed program never reaches the refusal the machine
  raises when a linear value would be leaked, overwritten while live, or
  discarded. The leak half covers three edges: a `let`'s scope exit, a frame's
  normal pop (a by-value parameter the callee never consumed — (Fn) §5.8's
  second clause, `3.8:62`), and a `return`'s `⊥_exit` unwind. The frame-pop
  edge is reached only through `Examples.lean`'s kernel-checked
  `run linearParamLeaked … = .stuck .linearLeak`: the bridge cannot exercise
  it, because the compiler rejects that program (E0406) before anything runs.
- **Carve-out (RUE-2316), the one edge no refusal covers.** A by-value
  argument's value is in no cell and no scope record between the `use` that
  produced it and §6.9's `mintParams`. If a *later* argument of the same call
  unwinds by `return`, (D-Return) discards the evaluation context with the
  pending arguments in it and unwinds only σ, so that value's drop is neither
  run nor monitored: a **linear value can be consumed zero times** with none
  of the three refusals firing, and an affine one loses its drop event
  silently. This is the calculus as written — (D-Return) §6.9 unwinds σ and
  nothing else, and (Strict-Bottom) §5.7, the only bottom rule for an argument
  position, imposes no §5.3 discard check on siblings already evaluated — so
  the statics cannot reject it without ⊥ provenance they do not carry, and the
  Rue compiler behaves the same way (the `RAffine` destructor does not run).
  The mechanization models the calculus rather than patching it and states the
  gap instead: `Dynamics.lean`'s "Pending arguments" section, the
  `no_violation` docstring, and the kernel-checked witnesses
  `RueCore.Examples.linearLostAtCallArg` and `affineLostAtCallArg`, both of
  which `checkProgram` accepts and both of which end with an empty drop trace.
  Closing it needs a rule, in §5.7 or §6.9, and is tracked as RUE-2316.
- **Covers:** whole bindings, the binary join, and by-value parameters, on
  every edge but the one above. **Owed:** RUE-2316; declared-linear
  destructure and residue ordering (RUE-2236); enums (RUE-2232).

## Exclusivity / no aliased mutation

- **Not yet mechanized.** Λ is ambiently empty in this fragment; the
  theorem and §7's four owed lemmas (loan/drop non-interference, loan-extent
  nesting, root separation, view-intact) are RUE-2238.

## Lemmas §7 owes, and the metatheory's own

| Lemma | Status |
|---|---|
| Totality of the float operations | not yet stated; an assumption about IEEE 754, named as such (RUE-2282) |
| Handle-uniqueness preservation (O1) | not yet mechanized (RUE-2240) |
| Adequacy of `eval` to §6's reduction, and progress/preservation derived over the mechanized relation | not yet stated; a Phase C deliverable required at checkpoint C and the CI gate (RUE-2289). Its domain is the programs `check` accepts: there `eval`'s `ok`/`panic` outcomes must agree with §6's values and panics, and neither side gets stuck. `.stuck` is outside the correspondence, because three of `eval`'s refusals are monitors §6 does not have, and `eval` refuses on an operand's shape before evaluating the next operand where §6.2's `v ⊕ E` context reduces that operand first (`Dynamics.lean`, "the correspondence with §6") |
| Fuel monotonicity and no masking | `RueCore.fuel_mono` and `RueCore.no_masking` (`lean/RueCore/Soundness.lean`). A bound that produced a result other than `outOfFuel` produces that same result at every larger bound; a bound that reached a violation reaches that same violation at every bound that answers at all. Together they say the ∀-fuel form of the theorems above is a statement about one real outcome, and that no choice of fuel hides a violation behind exhaustion |

## Traceability

`lean/INDEX.md` is the generated index (`scripts/validate-lean-xref-index.py`,
held fresh by the premerge tier): every declaration with the calculus rules,
sections, and prose paragraphs its doc-comment cites, and every labeled rule
and section of §5 and §6 with the declaration that mechanizes it or *not yet
mechanized*. The theorem side of the index, one row per §7 bullet naming its
theorem, is this document; RUE-207 completes it.
