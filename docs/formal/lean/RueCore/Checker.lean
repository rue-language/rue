import RueCore.Soundness

/-!
# RueCore.Checker — a decidable, verified checker for the §5 rules

The `Typed` judgment is syntax-directed, so it has a computable image:
`check Γ e` either produces `(T, Γ')` or rejects. `check_sound` proves every
acceptance is backed by a real derivation — so the §7 safety theorems apply to
anything `check` accepts. This is the seed of the "second, independent
implementation" purpose of the formal core (`docs/formal/README.md`): a
verified reference for what the compiler's semantic phase must accept.

(Completeness — `Typed` implies `check` succeeds — is deferred; the rules are
deterministic, so it is expected to hold. The spike only needs soundness.)
-/

namespace RueCore

def check (Γ : Ctx) : Expr → Option (Ty × Ctx)
  | .intLit n => if InBounds n then some (.int, Γ) else none
  | .boolLit _ => some (.bool, Γ)
  | .unitLit => some (.unit, Γ)
  | .use i =>
      match Γ[i]? with
      | none => none
      | some en =>
        match en.st with
        | .movedOut => none
        | .owned =>
          if en.ty.mult = .copy then some (en.ty, Γ)
          else some (en.ty, Γ.set i (en.setSt .movedOut))
  | .add e₁ e₂ =>
      match check Γ e₁ with
      | some (.int, Γ₁) =>
        (match check Γ₁ e₂ with
        | some (.int, Γ₂) => some (.int, Γ₂)
        | _ => none)
      | _ => none
  | .div e₁ e₂ =>
      match check Γ e₁ with
      | some (.int, Γ₁) =>
        (match check Γ₁ e₂ with
        | some (.int, Γ₂) => some (.int, Γ₂)
        | _ => none)
      | _ => none
  | .lt e₁ e₂ =>
      match check Γ e₁ with
      | some (.int, Γ₁) =>
        (match check Γ₁ e₂ with
        | some (.int, Γ₂) => some (.bool, Γ₂)
        | _ => none)
      | _ => none
  | .mkres κ e =>
      match check Γ e with
      | some (.int, Γ') => some (.res κ, Γ')
      | _ => none
  | .consume e =>
      match check Γ e with
      | some (.res _, Γ') => some (.int, Γ')
      | _ => none
  | .drop i =>
      match Γ[i]? with
      | none => none
      | some en =>
        match en.st with
        | .movedOut => none
        | .owned =>
          if en.ty.mult = .copy then some (.unit, Γ)
          else some (.unit, Γ.set i (en.setSt .movedOut))
  | .letIn m e₁ e₂ =>
      match check Γ e₁ with
      | none => none
      | some (T₁, Γ₁) =>
        match check ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ with
        | some (T₂, en' :: Γ₂) =>
            if en'.st = .owned ∧ T₁.mult = .linear then none
            else some (T₂, Γ₂)
        | _ => none
  | .assign i e =>
      match Γ[i]? with
      | none => none
      | some en₀ =>
        if en₀.mu = true then
          match check Γ e with
          | some (T, Γ₁) =>
            if T = en₀.ty then
              match Γ₁[i]? with
              | some en₁ =>
                  if en₁.st = .movedOut ∨ en₀.ty.mult ≠ .linear then
                    some (.unit, Γ₁.set i (en₁.setSt .owned))
                  else none
              | none => none
            else none
          | none => none
        else none
  | .seq e₁ e₂ =>
      match check Γ e₁ with
      | some (T₁, Γ₁) =>
          if T₁.mult = .linear then none
          else check Γ₁ e₂
      | none => none
  | .ite c e₁ e₂ =>
      match check Γ c with
      | some (.bool, Γ₀) =>
        (match check Γ₀ e₁, check Γ₀ e₂ with
        | some (T₁, Γ₁), some (T₂, Γ₂) =>
            if T₁ = T₂ then
              match Ctx.join Γ₁ Γ₂ with
              | some Γ' => some (T₁, Γ')
              | none => none
            else none
        | _, _ => none)
      | _ => none

/-- Every `check` acceptance is a real derivation. -/
theorem check_sound : ∀ {e : Expr} {Γ : Ctx} {T Γ'},
    check Γ e = some (T, Γ') → Typed Γ e T Γ' := by
  intro e
  induction e with
    (intro Γ T Γ' h
     simp only [check] at h)
  | intLit n =>
      split at h
      · cases h; exact .intLit ‹_›
      · cases h
  | boolLit b => cases h; exact .boolLit
  | unitLit => cases h; exact .unitLit
  | use i =>
      split at h
      · cases h
      · split at h
        · cases h
        · split at h
          · cases h; exact .useCopy ‹_› ‹_› ‹_›
          · cases h; exact .useMove ‹_› ‹_› ‹_›
  | add e₁ e₂ ih₁ ih₂ =>
      split at h
      · split at h
        · cases h; exact .add (ih₁ ‹_›) (ih₂ ‹_›)
        · cases h
      · cases h
  | div e₁ e₂ ih₁ ih₂ =>
      split at h
      · split at h
        · cases h; exact .div (ih₁ ‹_›) (ih₂ ‹_›)
        · cases h
      · cases h
  | lt e₁ e₂ ih₁ ih₂ =>
      split at h
      · split at h
        · cases h; exact .lt (ih₁ ‹_›) (ih₂ ‹_›)
        · cases h
      · cases h
  | mkres κ e ih =>
      split at h
      · cases h; exact .mkres (ih ‹_›)
      · cases h
  | consume e ih =>
      split at h
      · cases h; exact .consume (ih ‹_›)
      · cases h
  | drop i =>
      split at h
      · cases h
      · split at h
        · cases h
        · split at h
          · cases h; exact .dropCopy ‹_› ‹_› ‹_›
          · cases h; exact .dropRes ‹_› ‹_› ‹_›
  | letIn m e₁ e₂ ih₁ ih₂ =>
      split at h
      · cases h
      · split at h
        · split at h
          · cases h
          · cases h; exact .letIn (ih₁ ‹_›) (ih₂ ‹_›) ‹_›
        · cases h
  | assign i e ih =>
      split at h
      · cases h
      · rename_i en₀ hget₀
        split at h
        · rename_i hmu
          split at h
          · rename_i T' Γ₁ hchk
            split at h
            · rename_i hT
              split at h
              · rename_i en₁ hget₁
                split at h
                · rename_i hpre
                  cases h
                  subst hT
                  exact .assign hget₀ hmu (ih hchk) hget₁ hpre
                · cases h
              · cases h
            · cases h
          · cases h
        · cases h
  | seq e₁ e₂ ih₁ ih₂ =>
      split at h
      · split at h
        · cases h
        · exact .seq (ih₁ ‹_›) ‹_› (ih₂ h)
      · cases h
  | ite c e₁ e₂ ihc ih₁ ih₂ =>
      split at h
      · rename_i Γ₀ hcond
        split at h
        · rename_i T₁ Γ₁ T₂ Γ₂ h₁ h₂
          split at h
          · rename_i hT
            split at h
            · rename_i Γj hjoin
              cases h
              subst hT
              exact .ite (ihc hcond) (ih₁ h₁) (ih₂ h₂) hjoin
            · cases h
          · cases h
        · cases h
      · cases h

end RueCore
