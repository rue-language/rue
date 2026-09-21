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

**Fragment today.** Scalars — `int(w, s)` at every width `w ∈ {8, 16, 32, 64}`
and both signednesses, `float(w)` at both widths, `bool`, `unit` — and
monomorphic struct types declared by the program — named fields by position,
the `@copy`/`linear` attribute, whether the struct declares a destructor, and
`class(S)` as §3's join of the field classes lifted by that attribute
(`WfStructs` is the equation, and `checkStructs` decides it); struct literals
((Struct-Intro) §5.8, (D-Struct) §6.5) and §6.11's drop order — a value's user
destructor, then its fields in declaration order, recursively; use
(copy/move), `@drop`, `let` with scope-exit drop, assignment with
reinitialization, sequencing with the discard check, `if` with the §5.5 branch
join, §2's whole integer operator set — `+ - * / %`, `& | ^`, `<< >>`,
`< <= > >=` and the three unary forms, by (Arith)/(Ord)/(Neg)/(Not)/(BitNot)
§5.8 with §6.4's traps and its `val_{w,s}(β_w(·))` bit semantics — `@intCast`
((Int-Cast) §5.8, `(D-Int-Cast-Trap)` §6.4), `@panic` ((Panic) §5.8,
(D-Panic) §6.12) and `@dbg` ((Dbg) §5.8) — whose float rendering is
`3.12:40`–`3.12:42`'s shortest round-trip text; §2's float operator set —
`+ - * /`, `neg`, `< <= > >=` and `@total_cmp`, by
(Float-Arith)/(Float-Neg)/(Float-Ord)/(Total-Cmp) §5.8 with §6.4's trap-free
dynamics — and the one-operand float intrinsics `@int_to_float`,
`@float_to_int` (the one float form that traps, `3.12:18`), `@float_cast` and
the five of `3.12:34`; top-level function definitions,
by-value calls with frames and scope records ((Fn)/(Call) §5.8,
(D-Call)/(D-Return-Value) §6.9), and `return` with its σ unwind
((Return-Value) §5.7, (D-Return) §6.9). **Places** are §5's
`Path ::= x | Path.f`, so a use, a `@drop` and an assignment each name one:
a projection in value context is the partial move of `3.8:22` (§4.2), the
`fully-owned` and `3.9:34` premises of (Use-Move) §5.1 are checked, §5.6's
leak check is the recursive `residual-linear` read on the residue, and §5.5's
join is taken path by path. No declared-linear destructure — a path with a
declared-`linear` proper prefix is rejected as a stated restriction of the
fragment rather than given (Use-Declared-Linear-Destructure) §5.1 (RUE-2236) —
no arrays and so no `Path[c]` step, no element-wise `3.8:73` form and no
`3.8:68` root-index restriction, no equality compare (it borrows its
operands, `4.3:3f`, so `≈`'s float leaf has no instance here), no enums,
`inout`/`borrow` parameters, accessor calls, loops, loans, or buffers.

**The trap inventory, and what a trap carries.** Every §6.12 category the
fragment reaches is a `PanicKind`: `overflow` (`+ - *`, `neg`, `min_T / -1`,
`min_T % -1`), `divZero`, `remZero`, `castOverflow` (`@intCast`, `4.13:28`)
and `user` (`@panic`). The one float producer is `@float_to_int`, and it
reaches `overflow` rather than a category of its own (`3.12:18`, `8.1:7`):
float *arithmetic* never traps at all (`3.12:21`). `bounds` follows the
arrays. A trap result carries the **observable output that ran
before it** — the user destructors and the `@dbg` lines — because §6.12's
`Outcome` is exit status and stdout together and a trapping process prints
what it printed before exiting 101. `Examples.panicAfterDrop` and
`Examples.dbgBeforeTrap` are the kernel-checked witnesses, one per trap
source, and `crates/rue-oracle-diff` compares that output against the native
binary's. The same section's `panicPastAffine` and `panicPastLinear` are the negative
half: a `@panic` past a live binding carries an **empty** trace, because §5.7
exempts the `⊥_panic` edge from §5.6's obligation and §6.12 abandons the
configuration rather than unwinding it — so a destructor that had not
already run does not run. The linear one is the interesting case, because it
is the only shape where (Panic) and (Return-Value) differ: the linear value
is consumed zero times and no refusal fires, which is the second bullet of
the linear theorem's carve-out below.

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
  faithfully tracks the store's initialization" (§7), whose per-cell clause is
  the **recursive** `RueCore.ContentsMatches`: Σ's state for a binding is a
  tree over its paths (`owned`, `movedOut`, or `fields` for a partially moved
  value) and the cell holds the same shape with §6.1's `⊘` admitted at any
  node, and the two are related path by path. It is deliberately asymmetric at
  the `movedOut` clause: a statically `MovedOut` path may still hold live
  *non-linear* content (the §5.5 conservative join, `3.8:73`), never a live
  linear sub-value (`3.8:50`). Two lemmas turn that into what the proof uses:
  `RueCore.ContentsMatches.residualLinear_false` says the machine's leak
  monitor sees exactly what §5.6's `residual-linear` computes, and
  `RueCore.ContentsMatches.readAt`/`.writeAt` say that navigating a path
  agrees on the two sides wherever Σ has a state for it — which is wherever no
  proper prefix is `MovedOut`, (Owned-Base) §5.1. And the σ invariant: the frame's scope record,
  read newest-first, *is* its environment, which is what lets `Matches` apply
  to `run-all-scope-drops`'s walk. In this fragment that equation is
  **definitional** — every frame the interpreter builds builds σ and ρ from
  one list — so it is not yet evidence about the RUE-1277 redundancy, which
  was raised for scopes pushed and popped independently of the binder chain
  (§6.6's `match` arms, §6.10's loops). It becomes a real obligation when
  `Frame.scope` is §6.1's stack. `RueCore.Untouched` carries frame locality
  across a call, so a caller's agreement survives a callee's run.
- **Hypothesis:** `RueCore.ProgramTyped` — §3's class assignment for every
  struct declaration and (Fn) §5.8 for every function, plus an entry point
  taking no parameters — which `RueCore.checkProgram_sound` decides.
- **Covers:** the fragment above. **Owed:** every remaining Phase C slice
  re-establishes this theorem for its forms (RUE-2232 through RUE-2237).

## No use-after-move

- **Theorem:** `RueCore.no_use_after_move`.
- **In words:** no evaluation of a well-typed program touches a `⊘` — at the
  root of a cell, or at any node inside it.
- **Covers:** whole bindings **and paths**. The premise that carries the
  second is `fully-owned(Σ, p)` (§5.1, `3.8:26`): a read that reaches a hole
  anywhere inside the aggregate it names is `useAfterMove`, and the rule
  forbids handing such an aggregate to a new owner — which is the compiler's
  E0205 "use of partially moved value" (`RueCore.Examples.partialThenWhole`).
  Reading a *sibling* through a partially moved base stays legal
  (`3.8:53`, (Owned-Base) §5.1, mechanized as `RueCore.OwnSt.get`).
  **Owed:** array elements at constant indices (RUE-2235) and the
  declared-linear destructure's selected leaf (RUE-2236).

## No double-free

- **Not yet mechanized.** The drop trace makes double frees visible; the
  theorem over minted value identities is RUE-2237.
- **Partial progress:** the walk it will quantify over now has a proved
  shape, in closed form, **over cell contents rather than values** — which is
  where the `⊘`-skip that makes double frees impossible lives.
  `RueCore.dropContents_struct_events` (`lean/RueCore/Soundness.lean`) says
  that for well-typed contents at a struct type the events its drop emits are
  its user destructor's event — when the declaration has one (`3.9:28`) —
  followed by the concatenation of its fields' drop events in **declaration
  order** (`3.9:13`), each field's given by the same closed form recursively
  and a field that has been **moved out contributing none** (`3.8:73`);
  `RueCore.dropContents_events` is the equation it reads off, and
  `RueCore.dropEvents` (`lean/RueCore/Dynamics.lean`) is §6.11's order written
  as a function. `RueCore.dropContents_ok` says the walk never refuses on
  well-typed contents, and
  `RueCore.ContentsMatches.residualLinear_false` says contents the leak
  monitor lets through holds no live declared-`linear` sub-value — the
  residual reading §5.6 asks for, which is what makes a partially consumed
  carrier's residue droppable. What is still owed is the
  identity-level statement: that each minted value appears in the trace
  exactly once.

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
  quantifies over are in place, and so is the order *within* one value's drop
  (`dropContents_struct_events`, above).

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
- **Carve-out, the two edges no refusal covers.** On both a **linear value
  can be consumed zero times** with none of the three refusals firing, and a
  destructor-bearing one loses its observable drop silently. They are
  different in kind, and only the first is a gap.
  - **A pending argument (open, RUE-2316).** A by-value
    argument's value is in no cell and no scope record between the `use` that
    produced it and §6.9's `mintParams`. If a *later* argument of the same
    call unwinds by `return`, (D-Return) discards the evaluation context with
    the pending arguments in it and unwinds only σ, so that value's drop is
    neither run nor monitored. This is the calculus as written — (D-Return)
    §6.9 unwinds σ and nothing else, and (Strict-Bottom) §5.7, the only bottom
    rule for an argument position, imposes no §5.3 discard check on siblings
    already evaluated — so the statics cannot reject it without ⊥ provenance
    they do not carry, and the Rue compiler behaves the same way (the
    destructor does not run). The mechanization models the calculus rather
    than patching it and states the gap instead: `Dynamics.lean`'s "Pending
    arguments" section, the `no_violation` docstring, and the kernel-checked
    witnesses `RueCore.Examples.linearLostAtCallArg` and
    `affineLostAtCallArg`, both of which `checkProgram` accepts and both of
    which end with an empty drop trace. Closing it needs a rule, in §5.7 or
    §6.9, and is tracked as RUE-2316. A `@panic` *sibling* of a pending
    argument reaches the identical state, by this route as well as the next.
  - **A `@panic` (by design).** §6.12 abandons the configuration and §5.7
    exempts the `⊥_panic` edge from §5.6's obligation, so a trap runs no
    scope drop at all — where a `return` in the same position would have
    unwound the frame and run every one. A live linear binding at a `@panic`
    is therefore destroyed with no violation and an empty trace. That is
    (Panic) as specified rather than a gap, and the Rue compiler agrees:
    `RueCore.Examples.panicPastLinear` is the program, with its `Typed`
    derivation (`panicPastLinear_typed`), `checkProgram = false`, and
    `run … = .panic .user []` all kernel-checked; the compiled program prints
    nothing, says `panic: boom` and exits 101 (verified by hand).
    `panicPastAffine` is the same shape one class down, where the obligation
    was never there to lose.
- **Covers:** whole bindings, **paths and per-field obligations**, the binary
  join, by-value parameters, and struct values whose class is `Linear` through
  a field — §3's join, proved to be what a declaration records
  (`RueCore.struct_class_unique`) and to reach `Linear` exactly when the
  declaration says so or a field does (`RueCore.struct_carriesLinear_iff`,
  §5.3's `carries_linear` lifting) — on every edge but the two above. The
  per-field half is §5.6's `residual-linear` read on the residue
  (`RueCore.residualLinear`), so consuming exactly the linear part of an
  infectious carrier and letting the rest drop is accepted (the RUE-1591
  model), while stranding a linear sub-place under a partially moved place is
  rejected ((@Drop) §5.3's own side condition, E0406). **Owed:** RUE-2316;
  declared-linear destructure and residue ordering (RUE-2236); arrays
  (RUE-2235); enums (RUE-2232).

## Exclusivity / no aliased mutation

- **Not yet mechanized.** Λ is ambiently empty in this fragment; the
  theorem and §7's four owed lemmas (loan/drop non-interference, loan-extent
  nesting, root separation, view-intact) are RUE-2238.

## Lemmas §7 owes, and the metatheory's own

| Lemma | Status |
|---|---|
| Totality of the float operations | **assumed, named** — and less of it assumed than §7 expected. The obligation splits three ways. (1) *Totality as a function* is free: every §6.4 float operation is a total Lean function, so no float redex is stuck for want of a result. (2) *The `(D-Float-To-Int)`/`(D-Float-To-Int-Trap)` partition* is a **theorem**, `RueCore.floatToInt_partition`, because §2's datum model makes truncation exact integer arithmetic; `RueCore.evalFintrin_float_res` is it at the machine, and `RueCore.binOpFloat_res` is the matching statement for the arithmetic and the compares — the latter with no trap disjunct at all (`3.12:21`). Closure of the *exact* operations in `𝔽_w` is proved too: `RueCore.negate_wf` (`(D-Float-Neg)`), `RueCore.widen_wf` (the widening half of `(D-Float-Cast)`) and `RueCore.roundOp_wf` (`@floor`/`@ceil`/`@trunc`/`@round`). (3) What is **assumed** is closure of the *rounded* operations, which is IEEE's and not the mechanization's: the fields `arith_wf`, `sqrt_wf`, `ofLit_wf`, `ofInt_wf` and `narrow_wf` of `RueCore.FloatModel`, together with the behavioural laws §6.4 quotes from `3.12:22` and `3.12:19` (`arith_nan`, `narrow_nan`, `div_by_zero`, `zero_div_zero`) and `3.12:9`'s `ofLit_zero`/`ofLit_one`. Every one of those is a statement that is true of IEEE 754 *and* of the compiler, which is why the two NaN laws are the **weak** ones — a NaN operand yields *a* NaN, sign unspecified. `3.12:44`'s `σ_NaN` is the sign of a NaN an invalid operation *creates*; a propagated NaN keeps its operand's sign on every target Rue has, so a law that fixed the result's sign would be false of both. What `RueCore.Float.exactOps` propagates (the first NaN operand's sign) and the `σ_NaN` it picks are therefore **model choices**, checked against the compiler rather than assumed, and no theorem depends on either. They are structure fields, not `axiom` declarations, so every theorem that uses one carries it in its statement and `TRUST.md` lists them apart from Lean's axioms (`lean/RueCore/Float.lean`) |
| Totality of the **integer** operations | discharged where it is needed, by construction rather than as a lemma: `RueCore.valOf_inBounds` says every `w`-bit pattern read at a signedness denotes a value of that type, which is what makes §6.4's bitwise and shift rules total, and `RueCore.binOpInt_res`, `RueCore.evalUnOp_int_res` and `RueCore.evalIntCast_res` say every integer operator lands on a value of its rule's type or on a trap — the operator half of progress. The last two name the category: `neg` traps only as `overflow` and `@intCast` only as `castOverflow`. `binOpInt_res` ranges over §6.12's categories rather than naming one, because `/` and `%` add their own (`lean/RueCore/Soundness.lean`) |
| Handle-uniqueness preservation (O1) | not yet mechanized (RUE-2240) |
| Adequacy of `eval` to §6's reduction, and progress/preservation derived over the mechanized relation | not yet stated; a Phase C deliverable required at checkpoint C and the CI gate (RUE-2289). Its domain is the programs `check` accepts: there `eval`'s `ok`/`panic` outcomes must agree with §6's values and panics, and neither side gets stuck. `.stuck` is outside the correspondence, because three of `eval`'s refusals are monitors §6 does not have, and because `eval` names an operand-shape mismatch where §6 simply has no rule (`Dynamics.lean`, "the correspondence with §6"). A raw literal outside its type's `n_T` range is the other known difference, and `check` rejects it |
| Fuel monotonicity and no masking | `RueCore.fuel_mono` and `RueCore.no_masking` (`lean/RueCore/Soundness.lean`). A bound that produced a result other than `outOfFuel` produces that same result at every larger bound; a bound that reached a violation reaches that same violation at every bound that answers at all. Together they say the ∀-fuel form of the theorems above is a statement about one real outcome, and that no choice of fuel hides a violation behind exhaustion |

## Traceability

`lean/INDEX.md` is the generated index (`scripts/validate-lean-xref-index.py`,
held fresh by the premerge tier): every declaration with the calculus rules,
sections, and prose paragraphs its doc-comment cites, and every labeled rule
and section of §5 and §6 with the declaration that mechanizes it or *not yet
mechanized*. The theorem side of the index, one row per §7 bullet naming its
theorem, is this document; RUE-207 completes it.
