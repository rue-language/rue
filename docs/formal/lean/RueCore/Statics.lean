import RueCore.Syntax

/-!
# RueCore.Statics — ownership-threading typing (§5)

The judgment `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` of the calculus, with `Γ` and `Σ` fused into
one flow-sensitive context: a list of entries carrying the fixed skeleton
(type, mutability mark) and the flowing ownership state. The judgment's output
context has the same skeleton with updated states (`skel_preserved`).

Loans (Λ) are omitted: the fragment has no borrows, and Λ is ambiently empty
in the current core (§5 preamble).
-/

namespace RueCore

/-- `Σ`'s per-path state (§5): `Owned` or `MovedOut`. (`Uninit` is absence,
which the fragment never observes: bindings are initialized at `let`.) -/
inductive OwnState where
  | owned
  | movedOut
deriving DecidableEq, Repr

/-- One context entry: the binding's declared type and `μ` mark (fixed at the
binder) plus its current ownership state (flow-sensitive). -/
structure Entry where
  ty : Ty
  mu : Bool
  st : OwnState
deriving DecidableEq, Repr

def Entry.setSt (en : Entry) (s : OwnState) : Entry := { en with st := s }

/-- The fused `Γ ; Σ` context, innermost binding first (de Bruijn). -/
abbrev Ctx := List Entry

/-- The fixed part of an entry, preserved by every rule. -/
def Entry.skel (en : Entry) : Ty × Bool := (en.ty, en.mu)

def Ctx.skel (Γ : Ctx) : List (Ty × Bool) := Γ.map Entry.skel

/-- The §5.5 branch join, per entry. Agreeing states join to themselves. A
disagreement on a linear-carrying entry is ill-formed (`3.8:50`); on any other
entry it joins conservatively to `MovedOut`. -/
def Entry.join (a b : Entry) : Option Entry :=
  if a.st = b.st then some a
  else if a.ty.mult = .linear then none
  else some (a.setSt .movedOut)

/-- The §5.5 branch join, pointwise. Defined only on equal-length contexts
(the two arms extend one incoming context, so lengths always agree). -/
def Ctx.join : Ctx → Ctx → Option Ctx
  | [], [] => some []
  | a :: as, b :: bs =>
      match a.join b, Ctx.join as bs with
      | some e, some rest => some (e :: rest)
      | _, _ => none
  | _, _ => none

/-- `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` (§5), over the fused context.

Rule names cite the calculus: `useCopy`/`useMove` are (Use-Copy)/(Use-Move)
(§5.1); `dropCopy`/`dropRes` are (@Drop-Copy)/(@Drop) (§5.3); `letIn` folds in
§5.6's residual-linear scope-exit check; `assign` is (Assign) with the
`3.8:77` linear-overwrite premise on the *post-RHS* state; `seq` is (Seq) with
the `3.8:64` discard check; `ite` is (If) with the §5.5 join. -/
inductive Typed : Ctx → Expr → Ty → Ctx → Prop where
  | intLit {Γ n} :
      InBounds n →
      Typed Γ (.intLit n) .int Γ
  | boolLit {Γ b} :
      Typed Γ (.boolLit b) .bool Γ
  | unitLit {Γ} :
      Typed Γ .unitLit .unit Γ
  /-- (Use-Copy): a use of a `Copy` place copies; Σ unchanged. -/
  | useCopy {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult = .copy →
      Typed Γ (.use i) en.ty Γ
  /-- (Use-Move): a use of an `Affine`/`Linear` place moves it out. -/
  | useMove {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult ≠ .copy →
      Typed Γ (.use i) en.ty (Γ.set i (en.setSt .movedOut))
  | add {Γ Γ₁ Γ₂ e₁ e₂} :
      Typed Γ e₁ .int Γ₁ → Typed Γ₁ e₂ .int Γ₂ →
      Typed Γ (.add e₁ e₂) .int Γ₂
  | div {Γ Γ₁ Γ₂ e₁ e₂} :
      Typed Γ e₁ .int Γ₁ → Typed Γ₁ e₂ .int Γ₂ →
      Typed Γ (.div e₁ e₂) .int Γ₂
  | lt {Γ Γ₁ Γ₂ e₁ e₂} :
      Typed Γ e₁ .int Γ₁ → Typed Γ₁ e₂ .int Γ₂ →
      Typed Γ (.lt e₁ e₂) .bool Γ₂
  | mkres {Γ Γ' κ e} :
      Typed Γ e .int Γ' →
      Typed Γ (.mkres κ e) (.res κ) Γ'
  /-- Consuming elimination: takes the resource by value (a §4.2 use of its
  operand's places happens inside `e`'s own typing). -/
  | consume {Γ Γ' κ e} :
      Typed Γ e (.res κ) Γ' →
      Typed Γ (.consume e) .int Γ'
  /-- (@Drop-Copy): no drop glue, no ownership effect. -/
  | dropCopy {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult = .copy →
      Typed Γ (.drop i) .unit Γ
  /-- (@Drop): consumes the operand and discharges its (affine or linear)
  obligation; the only non-move discharge of a linear obligation. -/
  | dropRes {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult ≠ .copy →
      Typed Γ (.drop i) .unit (Γ.set i (en.setSt .movedOut))
  /-- (Let) + §5.6 scope exit: the binder enters `Owned`; at the body's end
  its residual state must not be an unconsumed linear value (the leak check).
  An `Owned` affine residue is dropped by the machine (§6.7); `MovedOut` needs
  nothing. -/
  | letIn {Γ Γ₁ Γ₂ m e₁ e₂ T₁ T₂ en'} :
      Typed Γ e₁ T₁ Γ₁ →
      Typed ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ T₂ (en' :: Γ₂) →
      ¬(en'.st = .owned ∧ T₁.mult = .linear) →
      Typed Γ (.letIn m e₁ e₂) T₂ Γ₂
  /-- (Assign): RHS first; overwrite of a live linear value is ill-formed
  (`3.8:77`, checked on the post-RHS state — the RUE-387 premise); the target
  is `Owned` afterward (reinitialization, `3.8:55`). -/
  | assign {Γ Γ₁ i e en₀ en₁} :
      Γ[i]? = some en₀ → en₀.mu = true →
      Typed Γ e en₀.ty Γ₁ →
      Γ₁[i]? = some en₁ →
      (en₁.st = .movedOut ∨ en₀.ty.mult ≠ .linear) →
      Typed Γ (.assign i e) .unit (Γ₁.set i (en₁.setSt .owned))
  /-- (Seq): the discarded value must not carry a linear value (`3.8:64`). -/
  | seq {Γ Γ₁ Γ₂ e₁ e₂ T₁ T₂} :
      Typed Γ e₁ T₁ Γ₁ → T₁.mult ≠ .linear →
      Typed Γ₁ e₂ T₂ Γ₂ →
      Typed Γ (.seq e₁ e₂) T₂ Γ₂
  /-- (If): both arms from the post-scrutinee state; outgoing state is the
  §5.5 join. -/
  | ite {Γ Γ₀ Γ₁ Γ₂ Γ' c e₁ e₂ T} :
      Typed Γ c .bool Γ₀ →
      Typed Γ₀ e₁ T Γ₁ → Typed Γ₀ e₂ T Γ₂ →
      Ctx.join Γ₁ Γ₂ = some Γ' →
      Typed Γ (.ite c e₁ e₂) T Γ'

/-! ## Skeleton preservation -/

theorem Entry.join_skel {a b e : Entry} (h : a.join b = some e) :
    e.skel = a.skel := by
  unfold Entry.join at h
  split at h
  · cases h; rfl
  · split at h
    · cases h
    · cases h; rfl

/-- Setting an index to the element already there is the identity. -/
theorem List.set_self_of_getElem? {α} : ∀ {l : List α} {i : Nat} {a : α},
    l[i]? = some a → l.set i a = l
  | [], i, a, h => by simp at h
  | x :: xs, 0, a, h => by simp_all
  | x :: xs, i + 1, a, h => by
      simp only [List.getElem?_cons_succ] at h
      simp [List.set_self_of_getElem? h]

/-- Re-marking an entry's ownership state does not change the skeleton. -/
theorem skel_set_setSt {Γ : Ctx} {i : Nat} {en : Entry} (h : Γ[i]? = some en)
    (s : OwnState) : Ctx.skel (Γ.set i (en.setSt s)) = Ctx.skel Γ := by
  unfold Ctx.skel
  rw [List.map_set]
  exact List.set_self_of_getElem? (by simp [h]; rfl)

theorem Ctx.join_skel : ∀ {Γ₁ Γ₂ Γ' : Ctx}, Ctx.join Γ₁ Γ₂ = some Γ' →
    Γ'.skel = Γ₁.skel
  | [], [], _, h => by cases h; rfl
  | a :: as, b :: bs, Γ', h => by
      unfold Ctx.join at h
      split at h
      · next e rest he hrest =>
          cases h
          simp [Ctx.skel, List.map_cons] at *
          exact ⟨Entry.join_skel he, Ctx.join_skel hrest⟩
      · cases h

/-- Every rule preserves the context skeleton: only ownership states flow. -/
theorem Typed.skel_preserved {Γ Γ' : Ctx} {e T} (h : Typed Γ e T Γ') :
    Γ'.skel = Γ.skel := by
  induction h with
  | intLit _ => rfl
  | boolLit => rfl
  | unitLit => rfl
  | useCopy _ _ _ => rfl
  | useMove hget _ _ => exact skel_set_setSt hget _
  | add _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | div _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | lt _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | mkres _ ih => exact ih
  | consume _ ih => exact ih
  | dropCopy _ _ _ => rfl
  | dropRes hget _ _ => exact skel_set_setSt hget _
  | letIn _ _ _ ih₁ ih₂ =>
      have := ih₂
      simp [Ctx.skel, List.map_cons] at this
      exact this.2.trans ih₁
  | assign _ _ _ hget₁ _ ih => exact (skel_set_setSt hget₁ _).trans ih
  | seq _ _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | ite _ _ _ hjoin ihc ih₁ _ => exact (Ctx.join_skel hjoin).trans (ih₁.trans ihc)

end RueCore
