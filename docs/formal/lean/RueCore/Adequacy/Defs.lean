import RueCore.Step
import RueCore.Soundness.Defs

/-!
# RueCore.Adequacy.Defs — what the adequacy theorems are stated over (layer L1)

The three definitions `Adequacy.lean`'s headline statements use beyond §6's
`Step`: the entry point's empty frame (`Frame.empty`), counted runs
(`StepsN`), and §7's semantic typing of a configuration (`Config.SafeAt`).
They are moved here verbatim from `Adequacy.lean` (RUE-2456); the simulation
relations its proofs are built from (`Sim`, `Long`) stay there, since no
headline statement mentions them.
-/

namespace RueCore

/-- The empty frame the entry point is called from (helper). -/
abbrev Frame.empty : Frame := { env := [], scope := [] }

/-- `→ⁿ`: a run of exactly `n` steps of §6's reduction (helper). Completeness
counts steps, because fuel is a bound on them. -/
inductive StepsN (M : FloatOps) (P : Program) : Nat → Config → Config → Prop where
  | refl (C : Config) : StepsN M P 0 C C
  | step {n : Nat} {C₁ C₂ C₃ : Config} :
      Step M P C₁ C₂ → StepsN M P n C₂ C₃ → StepsN M P (n + 1) C₁ C₃

/-- **A configuration typed at `T`, semantically** (§7, first bullet): every
configuration `→*` reaches from `C` reduces or has halted ((Result-Ok),
(Result-Panic) §6.12), and every value `C` halts with — `✓v`, a value at an
empty stack — has type `T` (§5's value typing, `HasTy`). The typing is
defined by reduction, not by a syntactic judgment over the configuration
(this section's docstring says why). -/
def Config.SafeAt (M : FloatOps) (P : Program) (T : Ty) (C : Config) : Prop :=
  (∀ D, Steps M P C D → D.Terminal ∨ ∃ D', Step M P D D') ∧
  (∀ H φ v tr, Steps M P C (.run H φ [] (.ret v) tr) → HasTy P.decls v T)

end RueCore
