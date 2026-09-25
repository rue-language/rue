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

/-- **Determinism** (§6): at most one step. -/
def Step.det_stmt : Prop :=
  ∀ {M : FloatOps} {P : Program} {C C₁ C₂ : Config}
    (_ : Step M P C C₁) (_ : Step M P C C₂), C₁ = C₂

/-- **Terminal is final** (§6.12): `✓` and `↯κ` take no step. -/
def Step.terminal_stmt : Prop :=
  ∀ {M : FloatOps} {P : Program} {C C' : Config} (_ : C.Terminal), ¬ Step M P C C'

/-- **Steps, terminal, or stuck** (§6), a stuck one named by a `Violation`. -/
def Config.trichotomy_stmt : Prop :=
  ∀ (M : FloatOps) (P : Program) (C : Config),
    (∃ C', Step M P C C') ∨ C.Terminal ∨ ∃ w, C.Stuck M P w

/-- **`step` computes `Step`** (§6). -/
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
checked program, every configuration reachable from `Config.init` is
terminal or steps. -/
def step_progress_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    ∀ C, Steps M.toFloatOps P Config.init C → C.Terminal ∨ ∃ C', Step M.toFloatOps P C C'

/-- **The invariant `SafeAt` along every run** (§7 "Type safety"; *not* its
sentence "types are preserved under reduction"). For a checked program, every
configuration reachable from `Config.init` is `SafeAt` the entry type: nothing
reachable from it is stuck, and every value it halts with has that type.
`SafeAt` is closed under `Steps` by definition, so this is `SafeAt` at
`Config.init` (R4 of `REDTEAM-LOG.md`), a semantic invariant; no
configuration typing `⊢ C : T` is defined or preserved (RUE-2423). -/
def step_preservation_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    ∃ fd, P.fns[0]? = some fd ∧
      ∀ C, Steps M.toFloatOps P Config.init C → C.SafeAt M.toFloatOps P fd.ret

/-- **Type safety over `Step`, per horizon** (§7 "Type safety"; §6.12). For a
checked program and every `n`, the machine has run `n` steps, or halted with a
well-typed value, or halted with a defined panic. -/
def step_type_safety_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    ∃ fd, P.fns[0]? = some fd ∧ ∀ n,
      (∃ D, StepsN M.toFloatOps P n Config.init D) ∨
      (∃ H v tr, Steps M.toFloatOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        HasTy P.decls v fd.ret) ∨
      (∃ κ tr, Steps M.toFloatOps P Config.init (.panic κ tr))

end RueCore.Spec
