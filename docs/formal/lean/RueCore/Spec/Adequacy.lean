module

public import RueCore.Adequacy.Defs

@[expose] public section

/-!
# RueCore.Spec.Adequacy — `eval` and `Step` agree (Spec layer)

§7 says the mechanization states type safety over its interpreter and that
"the two readings meet in the adequacy lemma `03-metatheory.md` owes"
(ADR-0097 decision 3). These statements are that lemma: on the programs
`check` accepts, `run`'s values and panics are exactly the ends of §6's runs
from `Config.init`, `run` is never stuck exactly when no reachable
configuration is, and exhausting the fuel at every bound is divergence.
In the literature's terms, the equivalence of a big-step and a small-step
semantics (`FIELD.md`).
-/

namespace RueCore.Spec

/-- **`eval` is sound for `Step`** (§7's adequacy sentence; ADR-0097). For a
checked program, `run` is never stuck, and its values and panics are reached
by `→*` from `Config.init` with the same store and trace. -/
def eval_sound_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P) (fuel : Nat),
    (∀ w, run M.toFloatOps P fuel ≠ .stuck w) ∧
    (∀ H v tr, run M.toFloatOps P fuel = .ok H v tr →
      Steps M.toFloatOps P Config.init (.run H Frame.empty [] (.ret v) tr)) ∧
    (∀ k tr, run M.toFloatOps P fuel = .panic k tr →
      Steps M.toFloatOps P Config.init (.panic k tr))

/-- **`run` is simulated by `Step`, on every program** (§6.12): the same, with
no typing hypothesis. -/
def run_sim_stmt : Prop :=
  ∀ (M : FloatOps) (P : Program) (fuel : Nat),
    (∀ H v tr, run M P fuel = .ok H v tr →
      Steps M P Config.init (.run H Frame.empty [] (.ret v) tr)) ∧
    (∀ k tr, run M P fuel = .panic k tr → Steps M P Config.init (.panic k tr))

/-- **`eval` is complete for `Step`, modulo fuel** (§7's adequacy sentence).
For a checked program, a value or panic `→*` reaches is `run`'s answer at
every large enough fuel. -/
def eval_complete_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    (∀ H φ v tr, Steps M.toFloatOps P Config.init (.run H φ [] (.ret v) tr) →
      ∃ n, ∀ fuel, n < fuel → run M.toFloatOps P fuel = .ok H v tr) ∧
    (∀ κ tr, Steps M.toFloatOps P Config.init (.panic κ tr) →
      ∃ n, ∀ fuel, n < fuel → run M.toFloatOps P fuel = .panic κ tr)

/-- **Completeness on every program** (§6.12): the same, up to a refusal of
`run`'s (RUE-2314). With no typing hypothesis the escape is wide: a `run` that
is `.stuck` past some fuel satisfies it, whatever `→*` reaches. -/
def run_complete_stmt : Prop :=
  ∀ (M : FloatOps) (P : Program),
    (∀ H φ v tr, Steps M P Config.init (.run H φ [] (.ret v) tr) →
      ∃ n, ∀ fuel, n < fuel → run M P fuel = .ok H v tr ∨ ∃ w, run M P fuel = .stuck w) ∧
    (∀ κ tr, Steps M P Config.init (.panic κ tr) →
      ∃ n, ∀ fuel, n < fuel → run M P fuel = .panic κ tr ∨ ∃ w, run M P fuel = .stuck w)

/-- **Never stuck, both ways** (§7 "Type safety"). For a checked program, `run`
is never stuck iff no reachable configuration is. Under `ProgramTyped` both
sides hold outright, so the equivalence adds nothing; cite
`step_never_stuck_of_run` (R5 of `REDTEAM-LOG.md`). -/
def never_stuck_iff_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    (∀ fuel w, run M.toFloatOps P fuel ≠ .stuck w) ↔
      ∀ C, Steps M.toFloatOps P Config.init C → C.Terminal ∨ ∃ C', Step M.toFloatOps P C C'

/-- **`eval` never stuck, so `Step` never stuck, on every program** (§7 "Type
safety": "it either reduces, halts with a value, or halts with one of the
defined panics"). -/
def step_never_stuck_of_run_stmt : Prop :=
  ∀ (M : FloatOps) (P : Program) (_ : ∀ fuel w, run M P fuel ≠ .stuck w),
    ∀ C, Steps M P Config.init C → C.Terminal ∨ ∃ C', Step M P C C'

/-- **A stuck `Step` run is a refusal of `run`** (§6), at every large enough
fuel, perhaps with another `Violation`. -/
def run_stuck_of_step_stuck_stmt : Prop :=
  ∀ (M : FloatOps) (P : Program) {C : Config} {w : Violation}
    (_ : Steps M P Config.init C) (_ : C.Stuck M P w),
    ∃ n, ∀ fuel, n < fuel → ∃ w', run M P fuel = .stuck w'

/-- **Divergence is exhaustion at every fuel** (§7 "Type safety"; §6.12): for a
checked program, `run` is `outOfFuel` at every fuel iff `Step` has runs of
every length from `Config.init`. -/
def eval_diverges_iff_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P),
    (∀ fuel, run M.toFloatOps P fuel = .outOfFuel) ↔
      ∀ n, ∃ D, StepsN M.toFloatOps P n Config.init D

end RueCore.Spec
