module

public import RueCore.Adequacy.Defs

@[expose] public section

/-!
# RueCore.Spec.Step — §6's reduction relation, and §7 over it (Spec layer)

§7's type-safety bullet is a statement about §6's machine. `Step M P C C'`
(`Step.lean`) is that machine, one constructor per §6 rule. The first six
statements here are the relation's own properties the readings of the others
rest on; the last three are §7's type-safety bullet stated over `Step`,
from §6.12's initial configuration `Config.init`.
-/

namespace RueCore.Spec

/-- **Determinacy** (§6): at most one step, `C → C₁` and `C → C₂` give
`C₁ = C₂` (PFPL's Lemma 5.3, with equality for `=α`: bindings are de Bruijn
indices). -/
def Step.det_stmt : Prop :=
  ∀ {M : FloatOps} {P : Program} {C C₁ C₂ : Config}
    (_ : Step M P C C₁) (_ : Step M P C C₂), C₁ = C₂

/-- **Terminal is final** (§6.12): `✓` and `↯κ` take no step (finality, PFPL's
Lemma 5.2, with a trap final as a checked error is). -/
def Step.terminal_stmt : Prop :=
  ∀ {M : FloatOps} {P : Program} {C C' : Config} (_ : C.Terminal), ¬ Step M P C C'

/-- **Steps, terminal, or stuck** (§6), a stuck one named by a `Violation`:
some `C → C'`, or `C` is `✓` or `↯κ`, or `step` refuses `C`. -/
def Config.trichotomy_stmt : Prop :=
  ∀ (M : FloatOps) (P : Program) (C : Config),
    (∃ C', Step M P C C') ∨ C.Terminal ∨ ∃ w, C.Stuck M P w

/-- **`step` computes `Step`** (§6): `C → C'` exactly when the step function
answers `C'`. -/
def step_iff_stmt : Prop :=
  ∀ {M : FloatOps} {P : Program} {C C' : Config}, Step M P C C' ↔ step M P C = .next C'

/-- **Stuck in `Step`'s terms** (§6): not terminal and no step exactly when
`step` says stuck. -/
def Config.stuck_iff_stmt : Prop :=
  ∀ {M : FloatOps} {P : Program} {C : Config},
    (¬ C.Terminal ∧ ∀ C', ¬ Step M P C C') ↔ ∃ w, C.Stuck M P w

/-- **Only §6's stuck states** (§6.3, §6.5; RUE-2314): a stuck configuration
is a use after move or drop, an unbound name or a type confusion, never a
monitor's. -/
def step_stuck_isStuckState_stmt : Prop :=
  ∀ {M : FloatOps} {P : Program} {C : Config} {w : Violation}
    (_ : C.Stuck M P w), w.isStuckState = true

/-- **Progress over `Step`** (§7 "Type safety": "does not get stuck"). For a
checked program, every `C` with `Config.init →* C` is terminal or has a step
`C → C'`. This is not the one-step progress lemma over a typed configuration
(no configuration typing is defined, RUE-2423) but its consequence along every
run, Timany et al.'s `safe` of the initial configuration (`FIELD.md`, section 2). -/
def step_progress_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    ∀ C, Steps M.toFloatOps P Config.init C → C.Terminal ∨ ∃ C', Step M.toFloatOps P C C'

/-- **The invariant `SafeAt` along every run** (§7 "Type safety"; *not* its
sentence "types are preserved under reduction"). For a checked program, every
`C` with `Config.init →* C` is `SafeAt` the entry type: nothing reachable from
it is stuck, and every value it halts with has that type.
`SafeAt` is closed under `→*` by definition, so this is `SafeAt` at
`Config.init` (R4 of `REDTEAM-LOG.md`), a semantic invariant; no
configuration typing `⊢ C : T` is defined or preserved (RUE-2423). In the
field's terms it is not preservation (subject reduction, PFPL's Thm 6.2) but
the conclusion of Timany et al.'s Cor. 2.3, `safe`, with typed halting values
(`FIELD.md`, section 2); the name is §7's, and RUE-2423 decides whether it stays. -/
def step_preservation_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    ∃ fd, P.fns[0]? = some fd ∧
      ∀ C, Steps M.toFloatOps P Config.init C → C.SafeAt M.toFloatOps P fd.ret

/-- **Type safety over `Step`, per horizon** (§7 "Type safety"; §6.12). For a
checked program and every `n`, `Config.init →ⁿ D` for some `D`, or
`Config.init →* ✓` with a value of the entry type, or `Config.init →* ↯κ`:
Wright & Felleisen's form (diverge, or a typed value), per horizon and with a
trap as a third outcome (`FIELD.md`, section 2), rather than progress ∧ preservation. -/
def step_type_safety_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    ∃ fd, P.fns[0]? = some fd ∧ ∀ n,
      (∃ D, StepsN M.toFloatOps P n Config.init D) ∨
      (∃ H v tr, Steps M.toFloatOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        HasTy P.decls v fd.ret) ∨
      (∃ κ tr, Steps M.toFloatOps P Config.init (.panic κ tr))

/-- **No use-after-drop over `Step`, on every program** (§7 "No use-after-drop /
no leak of drops"; §6.1's retired cell; RUE-2496). No configuration reachable
from `Config.init` is stuck on a retired (`†`) cell, whether or not the
program is checked. The hypothesis that the configuration is reached is
needed: a configuration whose frame names a retired cell is stuck so
(`Sharp.retired_cell`). -/
def step_no_use_after_drop_stmt : Prop :=
  ∀ (M : FloatOps) (P : Program) {C : Config} (_ : Steps M P Config.init C),
    ¬ C.Stuck M P .useAfterDrop

end RueCore.Spec
