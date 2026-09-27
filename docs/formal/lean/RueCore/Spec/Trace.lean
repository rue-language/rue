module

public import RueCore.Trace.Defs

@[expose] public section

/-!
# RueCore.Spec.Trace — what the drop trace says (Spec layer)

§7's "No double-free" and "No use-after-drop / no leak of drops" bullets,
read off the trace every run records: each drop, destructor, consumption and
`@dbg`, in order, with the identity of the value each one is of. The
multiplicity statements are over `run`'s and `eval`'s results, so over runs
that terminate (a result at some fuel), except `step_no_double_free`, which bounds
the trace of every configuration §6's relation `Step` reaches, terminating or
not; the order statements are over `Step` too.
-/

namespace RueCore.Spec

/-- **No double free** (§7 "No double-free"). A checked program's run is never
refused, and its trace frees no identity twice and runs no destructor twice
on one. Narrower than the bullet: an `outOfFuel` result has an empty trace,
so a run that never terminates is not covered here; `step_no_double_free`
covers it, over every configuration a run reaches (RUE-2477). -/
def no_double_free_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P) (fuel : Nat),
    (∀ w, run M.toFloatSig P fuel ≠ .refused w) ∧
      (∀ a, (freedIds P.decls (run M.toFloatSig P fuel).trace).count a ≤ 1) ∧
      (∀ a, (dtorIds (run M.toFloatSig P fuel).trace).count a ≤ 1)

/-- **No double free, on every prefix of a run** (§7 "No double-free", read as a
safety property; RUE-2477). For a checked program, every configuration §6's
relation reaches from `Config.init` — the run so far, whether or not it ever
terminates — has a trace that frees no identity twice and runs no destructor
twice on one, in `no_double_free`'s terms (`freedIds`, `dtorIds`). A safety
property is one a finite prefix of a run can violate (Alpern & Schneider,
`FIELD.md`), so this is the bullet's form over every run, a diverging one
included; `no_double_free` over a terminating run follows from it
(`no_double_free_of_step`). -/
def step_no_double_free_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P) {C : Config}
    (_ : Steps M.toFloatSig P Config.init C),
    (∀ a, (freedIds P.decls C.trace).count a ≤ 1) ∧ (∀ a, (dtorIds C.trace).count a ≤ 1)

/-- **Nothing freed twice, on every program** (§6.11): a run that answers a
value, an unwind or a panic frees each identity at most once, with no typing
hypothesis. A refused or fuel-exhausted run has an empty trace
(`EvalRes.trace`), so on an unchecked program the bound rests on `eval`'s
refusals: a second `@drop` of one place is refused `useAfterMove`, and an
owned value under a `Copy` one is refused `ownedUnderCopy`. `Step` has neither
refusal, and its bound is `step_no_double_free`, which needs `ProgramTyped`. -/
def freed_once_stmt : Prop :=
  ∀ (M : FloatSig) (P : Program) (fuel : Nat),
    ∀ a, (freedIds P.decls (run M P fuel).trace).count a ≤ 1

/-- **No destructor twice on one value** (§6.11, `3.9:28`), given only that a
destructor-bearing struct is not `Copy` (`3.9:31`). As for `freed_once`, a
refused or fuel-exhausted run has an empty trace, so the bound is over the
runs `eval` terminates. -/
def dtor_once_stmt : Prop :=
  ∀ (M : FloatSig) {P : Program} (_ : DtorNotCopy P.decls) (fuel : Nat),
    ∀ a, (dtorIds (run M P fuel).trace).count a ≤ 1

/-- **Every owned value ends exactly once** (§7 "No use-after-drop / no leak of
drops"). A typed expression of a checked program, the expression and the
program both `pendingSafe` (`e.pendingSafe`, `P.pendingSafe`), run from an
agreeing activation record and store, is never refused; every identity the store
holds ends up in an old cell, in the result, or ended in the trace as often
as held (`Exact`); every cell it allocated is retired (`Tidy`). Narrower
than the bullet: `pendingSafe` (RUE-2316), nothing about a panic, and per
evaluation, not per run; the whole-run form is `whole_program_exactly_once`
(RUE-2478). -/
def drop_exactly_once_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P)
    (_ : P.pendingSafe = true) {fuel : Nat} {R : Ty} {Γ : Ctx} {e : Expr} {T : Ty} {Ω : Out}
    {φ : Activation} {H : Store} (_ : Typed P R Γ e T Ω) (_ : ActivationTyping P.decls Γ φ H)
    (_ : StoreCC P.decls H) (_ : e.pendingSafe = true),
    (∀ w, eval M.toFloatSig fuel P H φ e ≠ .refused w) ∧
      Exact P.decls H [] (eval M.toFloatSig fuel P H φ e) ∧
      Tidy φ H (eval M.toFloatSig fuel P H φ e)

/-- **Values minted during an evaluation end exactly once too** (the same §7
bullet; §6.7, §6.9, §6.10): under the same hypotheses, once a form's leading
operands produced `vs` in `H₁` (`Lead`), the rest of the form ends them and
`H₁`'s identities as `Exact` counts, and retires what it allocated
(`Settled`). This is the form the proof of `drop_exactly_once` inducts on
(`Lead`, `fuel + 1`, `withTrace`), listed as a linking statement: it is what
says the values a form mints mid-evaluation are covered too. -/
def rest_exactly_once_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P)
    (_ : P.pendingSafe = true) {fuel : Nat} {R : Ty} {Γ : Ctx} {e : Expr} {T : Ty} {Ω : Out}
    {φ : Activation} {H : Store} (_ : Typed P R Γ e T Ω) (_ : ActivationTyping P.decls Γ φ H)
    (_ : StoreCC P.decls H) (_ : e.pendingSafe = true)
    {H₁ : Store} {vs : List Val} {tr : List Event} (_ : Lead M.toFloatSig P fuel H φ H₁ vs tr e)
    {r : EvalRes} (_ : eval M.toFloatSig (fuel + 1) P H φ e = r.withTrace tr),
    (∀ w, r ≠ .refused w) ∧
      Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧ Settled φ H₁ r

/-- **Every owned value of a run that terminates with a value ends exactly once** (§7 "No
use-after-drop / no leak of drops", over a whole program; RUE-2478). For a
checked, `pendingSafe` program, take any configuration `C` §6's relation
reaches from `Config.init` and any owned identity `a` that `C` holds — in a
cell, in focus, or pending on the control stack (`Config.held`); these are the
owned values allocated along the run. If the run from `C` terminates with a
value (`✓v`, a value at an empty stack), then `a` is ended in the final trace
(a drop, a discarded temporary's drop, or a consumption: `freedIds`) or is
part of the final value (which counts as ended: §2 restricts `main` to `i32` or
`unit`, which own nothing, and the fragment does not), exactly once between
the two: no owned value the
run holds is lost, and none is ended twice. Narrower than the bullet:
`pendingSafe` (RUE-2316), nothing about a panic (§6.12's trap runs no drop, so
what it abandons is not ended), and nothing about a run that never terminates
(`step_no_double_free` bounds every prefix from above). -/
def whole_program_exactly_once_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P) (_ : P.pendingSafe = true)
    {C : Config} (_ : Steps M.toFloatSig P Config.init C) {a : Nat} (_ : a ∈ C.held P.decls)
    {H : Store} {φ : Activation} {v : Val} {tr : List Event}
    (_ : Steps M.toFloatSig P C (.run H φ [] (.ret v) tr)),
    (v.own P.decls).count a + (freedIds P.decls tr).count a = 1

/-- **Drop order** (§7 "No use-after-drop / no leak of drops", "at the end of
its scope"; §6.7, §6.9–§6.11), over `Step`. A terminating run's trace — value
or panic — is in §6.11's block grammar (`Blocks`). Each step from a
reachable configuration drops one cell or distinct cells newest first, and
the registration stack is in location order. `StackDiscipline` holds of every step
that keeps its stack, whatever it drops, so it constrains only a step that
pops: the cells it drops are among those it cut, newest first (R7 of
`REDTEAM-LOG.md`). -/
def drop_order_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P),
    (∀ H φ v tr, Steps M.toFloatSig P Config.init (.run H φ [] (.ret v) tr) → Blocks P.decls tr) ∧
    (∀ κ tr, Steps M.toFloatSig P Config.init (.panic κ tr) → Blocks P.decls tr) ∧
    ∀ C C', Steps M.toFloatSig P Config.init C → Step M.toFloatSig P C C' →
      ∃ evs, C'.trace = C.trace ++ evs ∧ StrictStackOrder (dropLocs evs) ∧
        StackDiscipline C.stack C'.stack (dropLocs evs) ∧ (C.stack.Pairwise (· < ·))

/-- **Drop glue order, in §6.11's own terms** (§3.9, §6.11; §7 "No
use-after-drop / no leak of drops", *how* a value is dropped; RUE-2487), over
`Step`. A terminating run's trace — value or panic — is in §6.11's block grammar
with each drop's events given by §6.11's rules (`DropGlueBlocks`, `DropGlue`):
after each drop marker, the value's destructor first, then its fields in
declaration order, an array's elements in ascending index order, and an enum's
active payload only. Unlike `drop_order`'s `Blocks`, the rules are not the
function `dropEvents` the machine's walk is proved equal to, so a change to the
machine's drop glue cannot carry this statement with it. -/
def drop_glue_order_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P),
    (∀ H φ v tr, Steps M.toFloatSig P Config.init (.run H φ [] (.ret v) tr) →
      DropGlueBlocks P.decls tr) ∧
    (∀ κ tr, Steps M.toFloatSig P Config.init (.panic κ tr) → DropGlueBlocks P.decls tr)

end RueCore.Spec
