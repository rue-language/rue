module

public import RueCore.Trace.Defs

@[expose] public section

/-!
# RueCore.Spec.Trace — what the drop trace says (Spec layer)

§7's "No double-free" and "No use-after-drop / no leak of drops" bullets,
read off the trace every run records: each drop, destructor, consumption and
`@dbg`, in order, with the identity of the value each one is of. The
multiplicity statements are over `run`'s and `eval`'s results, so over runs
that finish (a result at some fuel); the order statement is over §6's
relation `Step`.
-/

namespace RueCore.Spec

/-- **No double free** (§7 "No double-free"). A checked program's run is never
refused, and its trace frees no identity twice and runs no destructor twice
on one. Narrower than the bullet: an `outOfFuel` result has an empty trace,
so a run that never finishes is not covered (RUE-2477). -/
def no_double_free_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P) (fuel : Nat),
    (∀ w, run M.toFloatOps P fuel ≠ .stuck w) ∧
      (∀ a, (freedIds P.decls (run M.toFloatOps P fuel).trace).count a ≤ 1) ∧
      (∀ a, (dtorIds (run M.toFloatOps P fuel).trace).count a ≤ 1)

/-- **Nothing freed twice, on every program** (§6.11): a finished run frees
each identity at most once, with no typing hypothesis. -/
def freed_once_stmt : Prop :=
  ∀ (M : FloatOps) (P : Program) (fuel : Nat),
    ∀ a, (freedIds P.decls (run M P fuel).trace).count a ≤ 1

/-- **No destructor twice on one value** (§6.11, `3.9:28`), given only that a
destructor-bearing struct is not `Copy` (`3.9:31`). -/
def dtor_once_stmt : Prop :=
  ∀ (M : FloatOps) {P : Program} (_ : DtorNotCopy P.decls) (fuel : Nat),
    ∀ a, (dtorIds (run M P fuel).trace).count a ≤ 1

/-- **Every owned value ends exactly once** (§7 "No use-after-drop / no leak of
drops"). A typed expression of a checked program, the expression and the
program both `pendingSafe` (`e.pendingSafe`, `P.pendingSafe`), run from an
agreeing frame and store, is never refused; every identity the store
holds ends up in an old cell, in the result, or ended in the trace as often
as held (`Exact`); every cell it allocated is retired (`Tidy`). Narrower
than the bullet: `pendingSafe` (RUE-2316), nothing about a panic, and per
evaluation, not per run (RUE-2478). -/
def drop_exactly_once_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P)
    (_ : P.pendingSafe = true) {fuel : Nat} {R : Ty} {Γ : Ctx} {e : Expr} {T : Ty} {Ω : Out}
    {φ : Frame} {H : Store} (_ : Typed P R Γ e T Ω) (_ : FrameMatches P.decls Γ φ H)
    (_ : StoreCC P.decls H) (_ : e.pendingSafe = true),
    (∀ w, eval M.toFloatOps fuel P H φ e ≠ .stuck w) ∧
      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) ∧
      Tidy φ H (eval M.toFloatOps fuel P H φ e)

/-- **Values minted during an evaluation end exactly once too** (the same §7
bullet; §6.7, §6.9, §6.10): under the same hypotheses, once a form's leading
operands produced `vs` in `H₁` (`Lead`), the rest of the form ends them and
`H₁`'s identities as `Exact` counts, and retires what it allocated
(`Settled`). This is the form the proof of `drop_exactly_once` inducts on
(`Lead`, `fuel + 1`, `withTrace`), listed as a linking statement: it is what
says the values a form mints mid-evaluation are covered too. -/
def rest_exactly_once_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P)
    (_ : P.pendingSafe = true) {fuel : Nat} {R : Ty} {Γ : Ctx} {e : Expr} {T : Ty} {Ω : Out}
    {φ : Frame} {H : Store} (_ : Typed P R Γ e T Ω) (_ : FrameMatches P.decls Γ φ H)
    (_ : StoreCC P.decls H) (_ : e.pendingSafe = true)
    {H₁ : Store} {vs : List Val} {tr : List Event} (_ : Lead M.toFloatOps P fuel H φ H₁ vs tr e)
    {r : EvalRes} (_ : eval M.toFloatOps (fuel + 1) P H φ e = r.withTrace tr),
    (∀ w, r ≠ .stuck w) ∧
      Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧ Settled φ H₁ r

/-- **Drop order** (§7 "No use-after-drop / no leak of drops", "at the end of
its scope"; §6.7, §6.9–§6.11), over `Step`. A finished run's trace — value
or panic — is in §6.11's block grammar (`Blocks`). Each step from a
reachable configuration drops one cell or distinct cells newest first, and
the registration stack is in location order. `Lifo` holds of every step
that keeps its stack, whatever it drops, so it constrains only a step that
pops: the cells it drops are among those it cut, newest first (R7 of
`REDTEAM-LOG.md`). -/
def drop_order_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    (∀ H φ v tr, Steps M.toFloatOps P Config.init (.run H φ [] (.ret v) tr) → Blocks P.decls tr) ∧
    (∀ κ tr, Steps M.toFloatOps P Config.init (.panic κ tr) → Blocks P.decls tr) ∧
    ∀ C C', Steps M.toFloatOps P Config.init C → Step M.toFloatOps P C C' →
      ∃ evs, C'.trace = C.trace ++ evs ∧ NewestFirst (dropLocs evs) ∧
        Lifo C.stack C'.stack (dropLocs evs) ∧ (C.stack.Pairwise (· < ·))

end RueCore.Spec
