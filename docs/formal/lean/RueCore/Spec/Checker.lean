module

public import RueCore.Checker.Defs

@[expose] public section

/-!
# RueCore.Spec.Checker — the checker decides the typing hypothesis (Spec layer)

§7's theorems are about well-typed programs, and the Spec layer's program
theorems take `ProgramTyped P`. These two statements say that the executable
checker `check`/`checkProgram` (`Checker/Defs.lean`) is sound for §5's
judgment, so running it is enough to know the theorems apply. Neither says
the checker is complete.
-/

namespace RueCore.Spec

/-- **The checker is sound** (§5 as an algorithm). Every `check` acceptance is
a derivation of `Typed`, at every type the result fits. -/
def check_sound_stmt : Prop :=
  ∀ {P : Program} {R : Ty} (e : Expr) {Γ : Ctx} {c : CTy} {Ω : Out},
    check P R Γ e = some (c, Ω) → ∀ T, c.fits T = true → Typed P R Γ e T Ω

/-- **An accepted program is well-typed** (§3, (Fn) §5.8): `checkProgram`
decides the hypothesis `ProgramTyped` of the program statements. -/
def checkProgram_sound_stmt : Prop :=
  ∀ {P : Program} (_ : checkProgram P = true), ProgramTyped P

end RueCore.Spec
