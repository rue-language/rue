import RueCore.Dynamics

/-!
# RueCore.Soundness — the §7 memory-safety theorem, fragment-sized

The central invariant is `Matches Γ ρ H` — "Σ faithfully tracks the store's
initialization", the load-bearing clause of §7's no-use-after-move bullet.
`CellMatches` is deliberately asymmetric, mirroring §5.5: a *statically*
`MovedOut` entry may still hold a live (non-linear!) value dynamically —
that is exactly the state a conservative branch join produces, and the machine
drops such residues path-specifically (`3.8:73`); a statically `Owned` entry
always holds a well-typed value; and a live **linear** value is never behind a
`MovedOut` entry, which is what makes the leak/overwrite refusals unreachable.

The main theorem, `soundness`, is type safety in definitional-interpreter
form: a well-typed expression evaluates to a well-typed value with the
invariant restored (preservation), or to a *defined* panic — never to a
`Violation` (progress; the interpreter is total, so no fuel caveats). The
corollaries at the bottom restate it per §7 bullet.
-/

namespace RueCore

/-! ## Value typing -/

inductive HasTy : Val → Ty → Prop where
  | int {n} : InBounds n → HasTy (.int n) .int
  | bool {b} : HasTy (.bool b) .bool
  | unit : HasTy .unit .unit
  | res {κ n} : InBounds n → HasTy (.res κ n) (.res κ)

theorem HasTy.mult_eq {v T} (h : HasTy v T) : v.mult = T.mult := by
  cases h <;> rfl

theorem HasTy.int_inv {v} (h : HasTy v .int) : ∃ n, v = .int n ∧ InBounds n := by
  cases h; exact ⟨_, rfl, ‹_›⟩

theorem HasTy.bool_inv {v} (h : HasTy v .bool) : ∃ b, v = .bool b := by
  cases h; exact ⟨_, rfl⟩

theorem HasTy.res_inv {v κ} (h : HasTy v (.res κ)) : ∃ n, v = .res κ n ∧ InBounds n := by
  cases h; exact ⟨_, rfl, ‹_›⟩

/-! ## The store–Σ agreement invariant -/

/-- Per-cell agreement between the static entry and the dynamic cell. -/
def CellMatches (c : Cell) (en : Entry) : Prop :=
  match en.st with
  | .owned => ∃ v, c = .full v ∧ HasTy v en.ty
  | .movedOut => c = .moved ∨ ∃ v, c = .full v ∧ HasTy v en.ty ∧ v.mult ≠ .linear

/-- `Matches Γ ρ H`: each binding's location holds a cell agreeing with its
static entry; locations are live (in `H`) and pairwise distinct. -/
inductive Matches : Ctx → Env → Store → Prop where
  | nil {H} : Matches [] [] H
  | cons {en : Entry} {Γ : Ctx} {ℓ : Nat} {ρ : Env} {H : Store} {c : Cell} :
      H[ℓ]? = some c → CellMatches c en → ℓ ∉ ρ → Matches Γ ρ H →
      Matches (en :: Γ) (ℓ :: ρ) H

theorem Matches.mem_lt {Γ ρ H} (hm : Matches Γ ρ H) : ∀ ℓ ∈ ρ, ℓ < H.length := by
  induction hm with
  | nil => intro ℓ h; cases h
  | cons hc _ _ _ ih =>
      intro ℓ' hmem
      cases hmem with
      | head => exact List.getElem?_eq_some_iff.mp hc |>.1
      | tail _ h => exact ih _ h

theorem Matches.fresh_not_mem {Γ ρ H} (hm : Matches Γ ρ H) : H.length ∉ ρ := by
  intro hmem
  exact absurd (hm.mem_lt _ hmem) (by omega)

theorem Matches.lookup {Γ ρ H} (hm : Matches Γ ρ H) {i : Nat} {en}
    (hget : Γ[i]? = some en) :
    ∃ ℓ c, ρ[i]? = some ℓ ∧ H[ℓ]? = some c ∧ CellMatches c en := by
  induction hm generalizing i with
  | nil => simp at hget
  | cons hc hcm _ _ ih =>
      cases i with
      | zero => simp_all
      | succ j =>
          simp only [List.getElem?_cons_succ] at hget ⊢
          exact ih hget

/-- Store growth by allocation preserves the invariant. -/
theorem Matches.append {Γ ρ H} (hm : Matches Γ ρ H) (ext : Store) :
    Matches Γ ρ (H ++ ext) := by
  induction hm with
  | nil => exact .nil
  | cons hc hcm hnin _ ih =>
      refine .cons ?_ hcm hnin ih
      rw [List.getElem?_append_left (List.getElem?_eq_some_iff.mp hc |>.1)]
      exact hc

/-- Writing a cell nobody in `ρ` points at preserves the invariant. -/
theorem Matches.set_outside : ∀ {Γ ρ H} {ℓ : Nat} {c : Cell},
    Matches Γ ρ H → ℓ ∉ ρ → Matches Γ ρ (H.set ℓ c)
  | _, _, _, _, _, .nil, _ => .nil
  | _, _, _, ℓ, c, .cons (ℓ := ℓ') hc hcm hnin' hrest, hnin => by
      have hne : ℓ ≠ ℓ' := fun h => hnin (by simp [h])
      refine .cons ?_ hcm hnin'
        (Matches.set_outside hrest (fun h => hnin (List.mem_cons_of_mem _ h)))
      rw [List.getElem?_set_ne hne]
      exact hc

/-- Updating binding `i`'s cell together with its entry preserves the
invariant, given the new cell matches the new entry. -/
theorem Matches.set : ∀ {Γ ρ H} {i ℓ : Nat} {en' : Entry} {c' : Cell},
    Matches Γ ρ H → ρ[i]? = some ℓ → CellMatches c' en' →
    Matches (Γ.set i en') ρ (H.set ℓ c')
  | _, _, _, _, _, _, _, .nil, hρ, _ => by simp at hρ
  | _, _, _, 0, ℓ, en', c', .cons (ℓ := ℓ') hc hcm hnin hrest, hρ, hc' => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at hρ
      subst hρ
      have hlt : ℓ' < _ := List.getElem?_eq_some_iff.mp hc |>.1
      refine .cons ?_ hc' hnin (hrest.set_outside hnin)
      rw [List.getElem?_set_self hlt]
  | _, _, _, (i + 1), ℓ, en', c', .cons (ℓ := ℓ') hc hcm hnin hrest, hρ, hc' => by
      simp only [List.getElem?_cons_succ] at hρ
      have hmem : ℓ ∈ _ := List.mem_of_getElem? hρ
      have hne : ℓ' ≠ ℓ := fun h => hnin (h ▸ hmem)
      simp only [List.set_cons_succ]
      refine .cons ?_ hcm hnin (Matches.set hrest hρ hc')
      rw [List.getElem?_set_ne (fun h => hne h.symm)]
      exact hc

/-! ## Skeleton transport and join weakening -/

theorem skel_lookup {Γ Γ' : Ctx} (h : Ctx.skel Γ' = Ctx.skel Γ) {i : Nat} {en en'}
    (h1 : Γ[i]? = some en) (h2 : Γ'[i]? = some en') :
    en'.ty = en.ty ∧ en'.mu = en.mu := by
  have hm : (Ctx.skel Γ')[i]? = (Ctx.skel Γ)[i]? := by rw [h]
  simp only [Ctx.skel, List.getElem?_map, h1, h2, Option.map_some,
    Option.some_inj] at hm
  exact ⟨congrArg Prod.fst hm, congrArg Prod.snd hm⟩

theorem Entry.join_matches_left {a b e' : Entry} (hj : a.join b = some e') {c : Cell}
    (hc : CellMatches c a) : CellMatches c e' := by
  unfold Entry.join at hj
  split at hj
  · cases hj; exact hc
  · split at hj
    · cases hj
    · cases hj
      rename_i hne hnl
      unfold CellMatches at hc ⊢
      cases hst : a.st with
      | owned =>
          rw [hst] at hc
          obtain ⟨v, rfl, hv⟩ := hc
          exact Or.inr ⟨v, rfl, hv, by rw [hv.mult_eq]; exact hnl⟩
      | movedOut =>
          rw [hst] at hc
          exact hc

theorem Entry.join_matches_right {a b e' : Entry} (hskel : a.skel = b.skel)
    (hj : a.join b = some e') {c : Cell} (hc : CellMatches c b) : CellMatches c e' := by
  have hty : a.ty = b.ty := congrArg Prod.fst hskel
  unfold Entry.join at hj
  split at hj
  · cases hj
    rename_i hst
    unfold CellMatches at hc ⊢
    rw [hst, hty]
    exact hc
  · split at hj
    · cases hj
    · cases hj
      rename_i hne hnl
      unfold CellMatches at hc ⊢
      cases hst : b.st with
      | owned =>
          rw [hst] at hc
          obtain ⟨v, rfl, hv⟩ := hc
          refine Or.inr ⟨v, rfl, hty ▸ hv, ?_⟩
          rw [hv.mult_eq, ← hty]
          exact hnl
      | movedOut =>
          rw [hst] at hc
          rcases hc with h | ⟨v, rfl, hv, hvm⟩
          · exact Or.inl h
          · exact Or.inr ⟨v, rfl, hty ▸ hv, hvm⟩

theorem Matches.join_left : ∀ {Γ₁ Γ₂ Γ' : Ctx} {ρ H},
    Ctx.join Γ₁ Γ₂ = some Γ' → Matches Γ₁ ρ H → Matches Γ' ρ H := by
  intro Γ₁ Γ₂ Γ' ρ H hj hm
  induction hm generalizing Γ₂ Γ' with
  | nil =>
      cases Γ₂ with
      | nil => cases hj; exact .nil
      | cons _ _ => cases hj
  | cons hc hcm hnin _ ih =>
      cases Γ₂ with
      | nil => cases hj
      | cons b bs =>
          unfold Ctx.join at hj
          split at hj
          · rename_i e rest hje hjrest
            cases hj
            exact .cons hc (Entry.join_matches_left hje hcm) hnin (ih hjrest)
          · cases hj

theorem Matches.join_right : ∀ {Γ₁ Γ₂ Γ' : Ctx} {ρ H},
    Ctx.skel Γ₁ = Ctx.skel Γ₂ →
    Ctx.join Γ₁ Γ₂ = some Γ' → Matches Γ₂ ρ H → Matches Γ' ρ H := by
  intro Γ₁ Γ₂ Γ' ρ H hskel hj hm
  induction hm generalizing Γ₁ Γ' with
  | nil =>
      cases Γ₁ with
      | nil => cases hj; exact .nil
      | cons _ _ => cases hj
  | cons hc hcm hnin _ ih =>
      cases Γ₁ with
      | nil => cases hj
      | cons a as =>
          simp only [Ctx.skel, List.map_cons, List.cons.injEq] at hskel
          unfold Ctx.join at hj
          split at hj
          · rename_i e rest hje hjrest
            cases hj
            exact .cons hc (Entry.join_matches_right hskel.1 hje hcm) hnin
              (ih hskel.2 hjrest)
          · cases hj

/-! ## The main theorem -/

/-- **Type safety for the fragment** (§7, first bullet, in
definitional-interpreter form; the interpreter's totality is progress).

A well-typed expression, run in any store/environment agreeing with its
incoming context, yields either a *defined* panic or a well-typed value with
the outgoing context's agreement restored. In particular it never returns
`.stuck` — no use-after-move, no use-after-free, no linear leak, no linear
overwrite, no linear discard (§7's decomposed bullets, as `Violation`s). -/
theorem soundness {Γ Γ' : Ctx} {e : Expr} {T : Ty} (ht : Typed Γ e T Γ') :
    ∀ {ρ H}, Matches Γ ρ H →
      (∃ k, eval H ρ e = .panic k) ∨
      (∃ H' v tr, eval H ρ e = .ok H' v tr ∧ HasTy v T ∧ Matches Γ' ρ H') := by
  induction ht with
  | @intLit Γ n hb =>
      intro ρ H hm
      exact Or.inr ⟨H, .int n, [], rfl, .int hb, hm⟩
  | @boolLit Γ b =>
      intro ρ H hm
      exact Or.inr ⟨H, .bool b, [], rfl, .bool, hm⟩
  | @unitLit Γ =>
      intro ρ H hm
      exact Or.inr ⟨H, .unit, [], rfl, .unit, hm⟩
  | @useCopy Γ i en hget hst hcopy =>
      intro ρ H hm
      obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hm.lookup hget
      unfold CellMatches at hcm
      rw [hst] at hcm
      obtain ⟨v, rfl, hv⟩ := hcm
      have hvm : v.mult = .copy := by rw [hv.mult_eq]; exact hcopy
      refine Or.inr ⟨H, v, [], ?_, hv, hm⟩
      simp [eval, hρ, hc, hvm]
  | @useMove Γ i en hget hst hncopy =>
      intro ρ H hm
      obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hm.lookup hget
      unfold CellMatches at hcm
      rw [hst] at hcm
      obtain ⟨v, rfl, hv⟩ := hcm
      have hvm : v.mult ≠ .copy := by rw [hv.mult_eq]; exact hncopy
      refine Or.inr ⟨H.set ℓ .moved, v, [], ?_, hv, ?_⟩
      · simp [eval, hρ, hc, hvm]
      · exact hm.set hρ (Or.inl rfl)
  | @add Γ Γ₁ Γ₂ e₁ e₂ h₁ h₂ ih₁ ih₂ =>
      intro ρ H hm
      rcases ih₁ hm with ⟨k, hk⟩ | ⟨H₁, v₁, tr₁, he₁, hty₁, hm₁⟩
      · exact Or.inl ⟨k, by simp [eval, hk]⟩
      obtain ⟨n₁, rfl, _⟩ := hty₁.int_inv
      rcases ih₂ hm₁ with ⟨k, hk⟩ | ⟨H₂, v₂, tr₂, he₂, hty₂, hm₂⟩
      · exact Or.inl ⟨k, by simp [eval, he₁, hk]⟩
      obtain ⟨n₂, rfl, _⟩ := hty₂.int_inv
      by_cases hbnd : InBounds (n₁ + n₂)
      · exact Or.inr ⟨H₂, .int (n₁ + n₂), tr₁ ++ tr₂,
          by simp [eval, he₁, he₂, hbnd], .int hbnd, hm₂⟩
      · exact Or.inl ⟨.overflow, by simp [eval, he₁, he₂, hbnd]⟩
  | @div Γ Γ₁ Γ₂ e₁ e₂ h₁ h₂ ih₁ ih₂ =>
      intro ρ H hm
      rcases ih₁ hm with ⟨k, hk⟩ | ⟨H₁, v₁, tr₁, he₁, hty₁, hm₁⟩
      · exact Or.inl ⟨k, by simp [eval, hk]⟩
      obtain ⟨n₁, rfl, _⟩ := hty₁.int_inv
      rcases ih₂ hm₁ with ⟨k, hk⟩ | ⟨H₂, v₂, tr₂, he₂, hty₂, hm₂⟩
      · exact Or.inl ⟨k, by simp [eval, he₁, hk]⟩
      obtain ⟨n₂, rfl, _⟩ := hty₂.int_inv
      by_cases hz : n₂ = 0
      · exact Or.inl ⟨.divZero, by simp [eval, he₁, he₂, hz]⟩
      by_cases hbnd : InBounds (n₁.tdiv n₂)
      · exact Or.inr ⟨H₂, .int (n₁.tdiv n₂), tr₁ ++ tr₂,
          by simp [eval, he₁, he₂, hz, hbnd], .int hbnd, hm₂⟩
      · exact Or.inl ⟨.overflow, by simp [eval, he₁, he₂, hz, hbnd]⟩
  | @lt Γ Γ₁ Γ₂ e₁ e₂ h₁ h₂ ih₁ ih₂ =>
      intro ρ H hm
      rcases ih₁ hm with ⟨k, hk⟩ | ⟨H₁, v₁, tr₁, he₁, hty₁, hm₁⟩
      · exact Or.inl ⟨k, by simp [eval, hk]⟩
      obtain ⟨n₁, rfl, _⟩ := hty₁.int_inv
      rcases ih₂ hm₁ with ⟨k, hk⟩ | ⟨H₂, v₂, tr₂, he₂, hty₂, hm₂⟩
      · exact Or.inl ⟨k, by simp [eval, he₁, hk]⟩
      obtain ⟨n₂, rfl, _⟩ := hty₂.int_inv
      exact Or.inr ⟨H₂, .bool (decide (n₁ < n₂)), tr₁ ++ tr₂,
        by simp [eval, he₁, he₂], .bool, hm₂⟩
  | @mkres Γ Γ' κ e h ih =>
      intro ρ H hm
      rcases ih hm with ⟨k, hk⟩ | ⟨H', v, tr, he, hty, hm'⟩
      · exact Or.inl ⟨k, by simp [eval, hk]⟩
      obtain ⟨n, rfl, hb⟩ := hty.int_inv
      exact Or.inr ⟨H', .res κ n, tr, by simp [eval, he], .res hb, hm'⟩
  | @consume Γ Γ' κ e h ih =>
      intro ρ H hm
      rcases ih hm with ⟨k, hk⟩ | ⟨H', v, tr, he, hty, hm'⟩
      · exact Or.inl ⟨k, by simp [eval, hk]⟩
      obtain ⟨n, rfl, hb⟩ := hty.res_inv
      exact Or.inr ⟨H', .int n, tr, by simp [eval, he], .int hb, hm'⟩
  | @dropCopy Γ i en hget hst hcopy =>
      intro ρ H hm
      obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hm.lookup hget
      unfold CellMatches at hcm
      rw [hst] at hcm
      obtain ⟨v, rfl, hv⟩ := hcm
      have hvm : v.mult = .copy := by rw [hv.mult_eq]; exact hcopy
      exact Or.inr ⟨H, .unit, [], by simp [eval, hρ, hc, hvm], .unit, hm⟩
  | @dropRes Γ i en hget hst hncopy =>
      intro ρ H hm
      obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hm.lookup hget
      unfold CellMatches at hcm
      rw [hst] at hcm
      obtain ⟨v, rfl, hv⟩ := hcm
      have hvm : v.mult ≠ .copy := by rw [hv.mult_eq]; exact hncopy
      refine Or.inr ⟨H.set ℓ .moved, .unit, [.drop ℓ v], ?_, .unit, ?_⟩
      · simp [eval, hρ, hc, hvm]
      · exact hm.set hρ (Or.inl rfl)
  | @letIn Γ Γ₁ Γ₂ m e₁ e₂ T₁ T₂ en' h₁ h₂ hres ih₁ ih₂ =>
      intro ρ H hm
      rcases ih₁ hm with ⟨k, hk⟩ | ⟨H₁, v₁, tr₁, he₁, hty₁, hm₁⟩
      · exact Or.inl ⟨k, by simp [eval, hk]⟩
      -- Mint the binding allocation.
      have hfresh : H₁.length ∉ ρ := hm₁.fresh_not_mem
      have hmbody : Matches ({ ty := T₁, mu := m, st := .owned } :: Γ₁)
          (H₁.length :: ρ) (H₁ ++ [.full v₁]) := by
        refine .cons ?_ ⟨v₁, rfl, hty₁⟩ hfresh (hm₁.append _)
        simp
      rcases ih₂ hmbody with ⟨k, hk⟩ | ⟨H₂, v₂, tr₂, he₂, hty₂, hm₂⟩
      · exact Or.inl ⟨k, by simp [eval, he₁, hk, EvalRes.withTrace]⟩
      -- Scope exit: invert the body's invariant at the binder.
      cases hm₂ with
      | cons hc hcm hnin hm₂' =>
        -- The binder's entry has the binder's declared type.
        have hskel := h₂.skel_preserved
        simp only [Ctx.skel, List.map_cons, List.cons.injEq] at hskel
        have hty_en' : en'.ty = T₁ := congrArg Prod.fst hskel.1
        unfold CellMatches at hcm
        cases hstn : en'.st with
        | owned =>
            rw [hstn] at hcm
            obtain ⟨v', rfl, hv'⟩ := hcm
            have hnl : T₁.mult ≠ .linear := fun h => hres ⟨hstn, h⟩
            have hvm : v'.mult = T₁.mult := by rw [hv'.mult_eq, hty_en']
            cases hml : v'.mult with
            | linear => exact absurd (hvm ▸ hml).symm (Ne.symm hnl)
            | affine =>
                refine Or.inr ⟨H₂.set H₁.length .dead, v₂,
                  tr₁ ++ tr₂ ++ [.drop H₁.length v'], ?_, hty₂, hm₂'.set_outside hnin⟩
                simp [eval, he₁, he₂, hc, hml]
            | copy =>
                refine Or.inr ⟨H₂.set H₁.length .dead, v₂, tr₁ ++ tr₂, ?_, hty₂,
                  hm₂'.set_outside hnin⟩
                simp [eval, he₁, he₂, hc, hml]
        | movedOut =>
            rw [hstn] at hcm
            rcases hcm with rfl | ⟨v', rfl, hv', hvnl⟩
            · refine Or.inr ⟨H₂.set H₁.length .dead, v₂, tr₁ ++ tr₂, ?_, hty₂,
                hm₂'.set_outside hnin⟩
              simp [eval, he₁, he₂, hc]
            · cases hml : v'.mult with
              | linear => exact absurd hml hvnl
              | affine =>
                  refine Or.inr ⟨H₂.set H₁.length .dead, v₂,
                    tr₁ ++ tr₂ ++ [.drop H₁.length v'], ?_, hty₂,
                    hm₂'.set_outside hnin⟩
                  simp [eval, he₁, he₂, hc, hml]
              | copy =>
                  refine Or.inr ⟨H₂.set H₁.length .dead, v₂, tr₁ ++ tr₂, ?_, hty₂,
                    hm₂'.set_outside hnin⟩
                  simp [eval, he₁, he₂, hc, hml]
  | @assign Γ Γ₁ i e en₀ en₁ hget₀ hmut h hget₁ hpre ih =>
      intro ρ H hm
      rcases ih hm with ⟨k, hk⟩ | ⟨H₁, v, tr, he, hty, hm₁⟩
      · exact Or.inl ⟨k, by simp [eval, hk]⟩
      obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hm₁.lookup hget₁
      have hskel := h.skel_preserved
      have htyeq : en₁.ty = en₀.ty := (skel_lookup hskel hget₀ hget₁).1
      have hty' : HasTy v en₁.ty := htyeq ▸ hty
      have hnewcm : CellMatches (.full v) (en₁.setSt .owned) := ⟨v, rfl, hty'⟩
      unfold CellMatches at hcm
      cases hstn : en₁.st with
      | owned =>
          rw [hstn] at hcm
          obtain ⟨vOld, rfl, hvOld⟩ := hcm
          have hnl : en₀.ty.mult ≠ .linear := by
            rcases hpre with h' | h'
            · rw [hstn] at h'; cases h'
            · exact h'
          have hvm : vOld.mult ≠ .linear := by
            rw [hvOld.mult_eq, htyeq]; exact hnl
          cases hml : vOld.mult with
          | linear => exact absurd hml hvm
          | affine =>
              refine Or.inr ⟨H₁.set ℓ (.full v), .unit, tr ++ [.drop ℓ vOld], ?_,
                .unit, hm₁.set hρ hnewcm⟩
              simp [eval, he, hρ, hc, hml]
          | copy =>
              refine Or.inr ⟨H₁.set ℓ (.full v), .unit, tr, ?_, .unit,
                hm₁.set hρ hnewcm⟩
              simp [eval, he, hρ, hc, hml]
      | movedOut =>
          rw [hstn] at hcm
          rcases hcm with rfl | ⟨vOld, rfl, hvOld, hvnl⟩
          · refine Or.inr ⟨H₁.set ℓ (.full v), .unit, tr, ?_, .unit,
              hm₁.set hρ hnewcm⟩
            simp [eval, he, hρ, hc]
          · cases hml : vOld.mult with
            | linear => exact absurd hml hvnl
            | affine =>
                refine Or.inr ⟨H₁.set ℓ (.full v), .unit, tr ++ [.drop ℓ vOld], ?_,
                  .unit, hm₁.set hρ hnewcm⟩
                simp [eval, he, hρ, hc, hml]
            | copy =>
                refine Or.inr ⟨H₁.set ℓ (.full v), .unit, tr, ?_, .unit,
                  hm₁.set hρ hnewcm⟩
                simp [eval, he, hρ, hc, hml]
  | @seq Γ Γ₁ Γ₂ e₁ e₂ T₁ T₂ h₁ hnl h₂ ih₁ ih₂ =>
      intro ρ H hm
      rcases ih₁ hm with ⟨k, hk⟩ | ⟨H₁, v₁, tr₁, he₁, hty₁, hm₁⟩
      · exact Or.inl ⟨k, by simp [eval, hk]⟩
      have hvnl : v₁.mult ≠ .linear := by rw [hty₁.mult_eq]; exact hnl
      rcases ih₂ hm₁ with ⟨k, hk⟩ | ⟨H₂, v₂, tr₂, he₂, hty₂, hm₂⟩
      · refine Or.inl ⟨k, ?_⟩
        cases hml : v₁.mult with
        | linear => exact absurd hml hvnl
        | affine => simp [eval, he₁, hml, hk, EvalRes.withTrace]
        | copy => simp [eval, he₁, hml, hk, EvalRes.withTrace]
      · cases hml : v₁.mult with
        | linear => exact absurd hml hvnl
        | affine =>
            exact Or.inr ⟨H₂, v₂, (tr₁ ++ [.dropTemp v₁]) ++ tr₂,
              by simp [eval, he₁, hml, he₂, EvalRes.withTrace], hty₂, hm₂⟩
        | copy =>
            exact Or.inr ⟨H₂, v₂, tr₁ ++ tr₂,
              by simp [eval, he₁, hml, he₂, EvalRes.withTrace], hty₂, hm₂⟩
  | @ite Γ Γ₀ Γ₁ Γ₂ Γ' c e₁ e₂ T hc h₁ h₂ hjoin ihc ih₁ ih₂ =>
      intro ρ H hm
      rcases ihc hm with ⟨k, hk⟩ | ⟨H₀, v₀, tr₀, he₀, hty₀, hm₀⟩
      · exact Or.inl ⟨k, by simp [eval, hk]⟩
      obtain ⟨b, rfl⟩ := hty₀.bool_inv
      have hskel12 : Ctx.skel Γ₁ = Ctx.skel Γ₂ :=
        h₁.skel_preserved.trans h₂.skel_preserved.symm
      cases b with
      | true =>
          rcases ih₁ hm₀ with ⟨k, hk⟩ | ⟨H', v, tr, he, hty, hm'⟩
          · exact Or.inl ⟨k, by simp [eval, he₀, hk, EvalRes.withTrace]⟩
          · exact Or.inr ⟨H', v, tr₀ ++ tr,
              by simp [eval, he₀, he, EvalRes.withTrace], hty,
              Matches.join_left hjoin hm'⟩
      | false =>
          rcases ih₂ hm₀ with ⟨k, hk⟩ | ⟨H', v, tr, he, hty, hm'⟩
          · exact Or.inl ⟨k, by simp [eval, he₀, hk, EvalRes.withTrace]⟩
          · exact Or.inr ⟨H', v, tr₀ ++ tr,
              by simp [eval, he₀, he, EvalRes.withTrace], hty,
              Matches.join_right hskel12 hjoin hm'⟩

/-! ## §7 corollaries, named -/

/-- A closed, well-typed program never reaches **any** memory violation. -/
theorem no_violation {e T Γ'} (ht : Typed [] e T Γ') (w : Violation) :
    eval [] [] e ≠ .stuck w := by
  rcases soundness ht (ρ := []) (H := []) Matches.nil with ⟨k, hk⟩ | ⟨H', v, tr, he, _, _⟩ <;>
    simp_all

/-- §7 "No use-after-move": the machine never reads a `⊘` cell. -/
theorem no_use_after_move {e T Γ'} (ht : Typed [] e T Γ') :
    eval [] [] e ≠ .stuck .useAfterMove := no_violation ht _

/-- §7 "No use-after-drop": the machine never touches a retired (`†`) cell. -/
theorem no_use_after_free {e T Γ'} (ht : Typed [] e T Γ') :
    eval [] [] e ≠ .stuck .useAfterFree := no_violation ht _

/-- §7 "Linear values are consumed exactly once", leak half: scope exit never
sees a live linear value. -/
theorem no_linear_leak {e T Γ'} (ht : Typed [] e T Γ') :
    eval [] [] e ≠ .stuck .linearLeak := no_violation ht _

/-- §7 linear bullet, overwrite half (`3.8:77`, the RUE-387 premise). -/
theorem no_linear_overwrite {e T Γ'} (ht : Typed [] e T Γ') :
    eval [] [] e ≠ .stuck .linearOverwrite := no_violation ht _

/-- §7 linear bullet, discard half (`3.8:64`). -/
theorem no_linear_discard {e T Γ'} (ht : Typed [] e T Γ') :
    eval [] [] e ≠ .stuck .linearDiscard := no_violation ht _

end RueCore
