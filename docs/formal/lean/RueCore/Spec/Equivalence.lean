module

public import RueCore.Equivalence.Defs

@[expose] public section

/-!
# RueCore.Spec.Equivalence — semantic equivalence of `eval` and `Step` (Spec layer)

§7 says the mechanization states type safety over its interpreter and that
"the two readings meet in the semantic-equivalence lemma `03-metatheory.md` owes"
(ADR-0097 decision 3). These statements are that lemma: on the programs
`check` accepts, `run`'s values and panics are exactly the ends of §6's runs
from `Config.init`, `run` is never refused exactly when no reachable
configuration is, and exhausting the fuel at every bound is divergence.

The field's name for it is **semantic equivalence** (Amin & Rompf, Thm 2), the
equivalence of a definitional interpreter and a small-step semantics, in the
line of the equivalence of big-step and small-step semantics (Leroy & Grall,
Thms 9 and 11; `FIELD.md`, section 3). "Adequacy" was the calculus's word, in the sense
of one operational semantics being adequate with respect to another; in the
field it more often means computational adequacy or the adequacy of a
semantic model, neither of which this is. RUE-2466 renamed the module and the
prose to the field's term; the proof module `RueCore.Equivalence` took the same name.
`eval_big_to_small` and
`eval_small_to_big` are the equivalence's two directions, named as Isabelle's
HOL-IMP names them (`big_to_small`, `small_to_big`).
-/

namespace RueCore.Spec

/-- **The big-to-small direction, `eval` to `Step`**: the interpreter-to-small-step direction of
the semantic equivalence (core:7:3's semantic-equivalence sentence; ADR-0097). For a
checked program, `run` is never refused, and its values and panics are reached
by `→*` from `Config.init` with the same store and trace. -/
def eval_big_to_small_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P) (fuel : Nat),
    (∀ w, run M.toFloatSig P fuel ≠ .refused w) ∧
    (∀ H v tr, run M.toFloatSig P fuel = .ok H v tr →
      Steps M.toFloatSig P Config.init (.run H Activation.empty [] (.ret v) tr)) ∧
    (∀ k tr, run M.toFloatSig P fuel = .panic k tr →
      Steps M.toFloatSig P Config.init (.panic k tr))

/-- **`run` is simulated by `Step`, on every program** (§6.12): the same, with
no typing hypothesis (core:7:3's semantic-equivalence sentence). -/
def run_sim_stmt : Prop :=
  ∀ (M : FloatSig) (P : Program) (fuel : Nat),
    (∀ H v tr, run M P fuel = .ok H v tr →
      Steps M P Config.init (.run H Activation.empty [] (.ret v) tr)) ∧
    (∀ k tr, run M P fuel = .panic k tr → Steps M P Config.init (.panic k tr))

/-- **The small-to-big direction, `Step` to `eval`, modulo fuel**: the small-step-to-interpreter
direction of the semantic equivalence (core:7:3's semantic-equivalence sentence).
For a checked program, a value or panic `→*` reaches is `run`'s answer at
every large enough fuel. -/
def eval_small_to_big_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P),
    (∀ H φ v tr, Steps M.toFloatSig P Config.init (.run H φ [] (.ret v) tr) →
      ∃ n, ∀ fuel, n < fuel → run M.toFloatSig P fuel = .ok H v tr) ∧
    (∀ κ tr, Steps M.toFloatSig P Config.init (.panic κ tr) →
      ∃ n, ∀ fuel, n < fuel → run M.toFloatSig P fuel = .panic κ tr)

/-- **Completeness on every program** (§6.12): the same, up to a refusal of
`run`'s (RUE-2314; core:7:3's semantic-equivalence sentence). With no typing
hypothesis the escape is wide: a `run` that is `.refused` past some fuel
satisfies it, whatever `→*` reaches. -/
def run_small_to_big_stmt : Prop :=
  ∀ (M : FloatSig) (P : Program),
    (∀ H φ v tr, Steps M P Config.init (.run H φ [] (.ret v) tr) →
      ∃ n, ∀ fuel, n < fuel → run M P fuel = .ok H v tr ∨ ∃ w, run M P fuel = .refused w) ∧
    (∀ κ tr, Steps M P Config.init (.panic κ tr) →
      ∃ n, ∀ fuel, n < fuel → run M P fuel = .panic κ tr ∨ ∃ w, run M P fuel = .refused w)

/-- **Never refused iff never stuck** (core:7:3 "Type safety"). For a checked program,
`run` is never refused iff no reachable configuration is stuck. Under `ProgramTyped` both
sides hold outright, so the equivalence adds nothing; cite
`step_never_stuck_of_run` (R5 of `REDTEAM-LOG.md`). -/
def never_refused_iff_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P),
    (∀ fuel w, run M.toFloatSig P fuel ≠ .refused w) ↔
      ∀ C, Steps M.toFloatSig P Config.init C → C.Terminal ∨ ∃ C', Step M.toFloatSig P C C'

/-- **`eval` never refused, so `Step` never stuck, on every program** (core:7:3 "Type
safety": "it either reduces, halts with a value, or halts with one of the
defined panics"). -/
def step_never_stuck_of_run_stmt : Prop :=
  ∀ (M : FloatSig) (P : Program) (_ : ∀ fuel w, run M P fuel ≠ .refused w),
    ∀ C, Steps M P Config.init C → C.Terminal ∨ ∃ C', Step M P C C'

/-- **A stuck `Step` run is a refusal of `run`** (§6), at every large enough
fuel, perhaps with another `Refusal`: the stuck case of core:7:3's
semantic-equivalence sentence. -/
def run_refused_of_step_stuck_stmt : Prop :=
  ∀ (M : FloatSig) (P : Program) {C : Config} {w : Refusal}
    (_ : Steps M P Config.init C) (_ : C.Stuck M P w),
    ∃ n, ∀ fuel, n < fuel → ∃ w', run M P fuel = .refused w'

/-- **Divergence is exhaustion at every fuel** (core:7:3 "Type safety"; §6.12): for a
checked program, `run` is `outOfFuel` at every fuel iff `Step` has runs of
every length from `Config.init`. -/
def eval_diverges_iff_stmt : Prop :=
  ∀ (M : FloatLaws) {P : Program} (_ : ProgramTyped P),
    (∀ fuel, run M.toFloatSig P fuel = .outOfFuel) ↔
      ∀ n, ∃ D, StepsN M.toFloatSig P n Config.init D

end RueCore.Spec
