import RueCore.Syntax

/-!
# RueCore.Statics — ownership-threading typing (§5)

The judgment `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` of the calculus, with `Γ` and `Σ` fused into
one flow-sensitive context: a list of entries carrying the fixed skeleton
(type, mutability mark) and the flowing ownership state. The judgment's output
context has the same skeleton with updated states (`skel_preserved`).

The judgment is parameterized by the program `P` — the top-level function
environment §5.8's (Call) looks a callee's signature up in — and by `R`, the
enclosing function's declared return type, which is what (Return-Value) §5.7
checks a `return` operand against. Both are fixed for a whole derivation, as
the calculus fixes them for a function body.

Loans (Λ) are omitted: the fragment has no borrows, and Λ is ambiently empty
in the current core (§5 preamble).

## Divergence, without a `never` type

§5.7 types `return e` at `never` and lets (Sub-Never) coerce it to any type,
with a divergent outgoing state `⊥` that §5.5's join excludes. The fragment
folds both into one rule: `Typed.ret` concludes at **any** type `T` and with
**any** outgoing context of the same skeleton, which is exactly what a `never`
value and a `⊥` state license a context to assume. `Ty` therefore has no
`never` constructor and `HasTy` (`Soundness.lean`) needs no case for it — sound
because `never` has no values (`3.4:1`), so nothing is ever typed at it
dynamically. (Return-Bottom) needs no rule of its own for the same reason: a
`return` whose operand itself diverges is typed by this rule with the operand
at `R`. `INDEX.md` records (Sub-Never) as mechanized only at this one form,
the only never-typed form the fragment has.

An *algorithm* cannot leave a type and a state free, so `check`
(`Checker.lean`) picks one of each — the enclosing return type and the state
in force after the operand — and its module docstring says what completeness
that costs.
-/

namespace RueCore

/-- `Σ`'s per-path state (§5): `Owned` or `MovedOut`. (`Uninit` is absence,
which the fragment never observes: bindings are initialized at `let`.) -/
inductive OwnState where
  | owned
  | movedOut
deriving DecidableEq, Repr

/-- One context entry: the binding's declared type and `μ` mark (fixed at the
binder: `Γ`'s part) plus its current ownership state (flow-sensitive: `Σ`'s
part), one row of §5's fused `Γ ; Σ`. -/
structure Entry where
  ty : Ty
  mu : Bool
  st : OwnState
deriving DecidableEq, Repr

/-- Re-mark an entry's ownership state (helper). -/
def Entry.setSt (en : Entry) (s : OwnState) : Entry := { en with st := s }

/-- The fused `Γ ; Σ` context of the judgment `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` (§5),
innermost binding first (de Bruijn). -/
abbrev Ctx := List Entry

/-- The fixed part of an entry, preserved by every rule (helper). -/
def Entry.skel (en : Entry) : Ty × Bool := (en.ty, en.mu)

/-- The skeleton of a whole context (helper). -/
def Ctx.skel (Γ : Ctx) : List (Ty × Bool) := Γ.map Entry.skel

/-- §5.6's residual-linear condition, read over a whole frame: no binding is
still `Owned` at a linear-carrying type. This is the premise (Fn) §5.8 imposes
on a function body's exit edges for its by-value parameters (`3.8:62`) and
that §5.6's `⊥_exit` carries at an early `return`: at such an edge every open
scope of the frame ends at once, so the check is frame-wide rather than
per-binding. In the fragment `residual-linear(Σ, x, T)` collapses to
`Σ(x) = Owned ∧ class(T) = Linear` (whole bindings, no paths). -/
def NoOwnedLinear (Γ : Ctx) : Prop :=
  ∀ en ∈ Γ, ¬(en.st = .owned ∧ en.ty.mult = .linear)

instance (Γ : Ctx) : Decidable (NoOwnedLinear Γ) := by
  unfold NoOwnedLinear; infer_instance

/-- (Fn) §5.8's entry context `Γ0;Σ0`: every by-value parameter enters the
body `Owned` and subject to the ordinary use/drop rules. The list is reversed
because `Ctx` is innermost-binder-first while a signature lists parameters
left to right, so the first parameter is the outermost binder — which is what
gives it de Bruijn index `m-1` and the printed name `v0`. -/
def fnCtx (fd : FnDef) : Ctx :=
  (fd.params.map fun p => { ty := p.ty, mu := p.mu, st := .owned }).reverse

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

mutual
/-- `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` (§5), over the fused context, under the program `P`
and the enclosing function's return type `R`.

Rule names cite the calculus: `useCopy`/`useMove` are (Use-Copy)/(Use-Move)
(§5.1); `dropCopy`/`dropRes` are (@Drop-Copy)/(@Drop) (§5.3); `letIn` folds in
§5.6's residual-linear scope-exit check; `assign` is (Assign) with the
`3.8:77` linear-overwrite premise on the *post-RHS* state; `seq` is (Seq) with
the `3.8:64` discard check; `ite` is (If) with the §5.5 join; `call` is (Call)
by value (§5.8); `ret` is (Return-Value) with (Sub-Never) folded in (§5.7). -/
inductive Typed (P : Program) (R : Ty) : Ctx → Expr → Ty → Ctx → Prop where
  /-- (Lit) §5.8: an integer literal of `int(64, signed)`, in range (the
  fragment's fixing of `int(w,s)`; elaboration resolves the width, `4.1:2`). -/
  | intLit {Γ n} :
      InBounds n →
      Typed P R Γ (.intLit n) .int Γ
  /-- (Lit) §5.8: a boolean literal. -/
  | boolLit {Γ b} :
      Typed P R Γ (.boolLit b) .bool Γ
  /-- (Lit) §5.8: the unit literal. -/
  | unitLit {Γ} :
      Typed P R Γ .unitLit .unit Γ
  /-- (Use-Copy): a use of a `Copy` place copies; Σ unchanged. -/
  | useCopy {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult = .copy →
      Typed P R Γ (.use i) en.ty Γ
  /-- (Use-Move): a use of an `Affine`/`Linear` place moves it out. -/
  | useMove {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult ≠ .copy →
      Typed P R Γ (.use i) en.ty (Γ.set i (en.setSt .movedOut))
  /-- (Arith) §5.8 for `+`: both operands `int`, left to right, Σ threaded
  (`4.2:1`). -/
  | add {Γ Γ₁ Γ₂ e₁ e₂} :
      Typed P R Γ e₁ .int Γ₁ → Typed P R Γ₁ e₂ .int Γ₂ →
      Typed P R Γ (.add e₁ e₂) .int Γ₂
  /-- (Arith) §5.8 for `/`. -/
  | div {Γ Γ₁ Γ₂ e₁ e₂} :
      Typed P R Γ e₁ .int Γ₁ → Typed P R Γ₁ e₂ .int Γ₂ →
      Typed P R Γ (.div e₁ e₂) .int Γ₂
  /-- (Ord) §5.8 for `<`: an ordering compare of two `int` operands yields
  `bool`. -/
  | lt {Γ Γ₁ Γ₂ e₁ e₂} :
      Typed P R Γ e₁ .int Γ₁ → Typed P R Γ₁ e₂ .int Γ₂ →
      Typed P R Γ (.lt e₁ e₂) .bool Γ₂
  /-- Abstract resource introduction: the shape of §5.8's aggregate
  introduction with one integer payload and no fields, standing in for a
  struct literal until structs land (RUE-2230). Not the calculus's
  `Struct-Intro` rule itself. -/
  | mkres {Γ Γ' κ e} :
      Typed P R Γ e .int Γ' →
      Typed P R Γ (.mkres κ e) (.res κ) Γ'
  /-- Consuming elimination: takes the resource by value (a §4.2 use of its
  operand's places happens inside `e`'s own typing). -/
  | consume {Γ Γ' κ e} :
      Typed P R Γ e (.res κ) Γ' →
      Typed P R Γ (.consume e) .int Γ'
  /-- (@Drop-Copy): no drop glue, no ownership effect. -/
  | dropCopy {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult = .copy →
      Typed P R Γ (.drop i) .unit Γ
  /-- (@Drop): consumes the operand and discharges its (affine or linear)
  obligation; the only non-move discharge of a linear obligation. -/
  | dropRes {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult ≠ .copy →
      Typed P R Γ (.drop i) .unit (Γ.set i (en.setSt .movedOut))
  /-- (Let) + §5.6 scope exit: the binder enters `Owned`; at the body's end
  its residual state must not be an unconsumed linear value (the leak check).
  An `Owned` affine residue is dropped by the machine (§6.7); `MovedOut` needs
  nothing. -/
  | letIn {Γ Γ₁ Γ₂ m e₁ e₂ T₁ T₂ en'} :
      Typed P R Γ e₁ T₁ Γ₁ →
      Typed P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ T₂ (en' :: Γ₂) →
      ¬(en'.st = .owned ∧ T₁.mult = .linear) →
      Typed P R Γ (.letIn m e₁ e₂) T₂ Γ₂
  /-- (Assign): RHS first; overwrite of a live linear value is ill-formed
  (`3.8:77`, checked on the post-RHS state — the RUE-387 premise); the target
  is `Owned` afterward (reinitialization, `3.8:55`). -/
  | assign {Γ Γ₁ i e en₀ en₁} :
      Γ[i]? = some en₀ → en₀.mu = true →
      Typed P R Γ e en₀.ty Γ₁ →
      Γ₁[i]? = some en₁ →
      (en₁.st = .movedOut ∨ en₀.ty.mult ≠ .linear) →
      Typed P R Γ (.assign i e) .unit (Γ₁.set i (en₁.setSt .owned))
  /-- (Seq): the discarded value must not carry a linear value (`3.8:64`). -/
  | seq {Γ Γ₁ Γ₂ e₁ e₂ T₁ T₂} :
      Typed P R Γ e₁ T₁ Γ₁ → T₁.mult ≠ .linear →
      Typed P R Γ₁ e₂ T₂ Γ₂ →
      Typed P R Γ (.seq e₁ e₂) T₂ Γ₂
  /-- (If): both arms from the post-scrutinee state; outgoing state is the
  §5.5 join. -/
  | ite {Γ Γ₀ Γ₁ Γ₂ Γ' c e₁ e₂ T} :
      Typed P R Γ c .bool Γ₀ →
      Typed P R Γ₀ e₁ T Γ₁ → Typed P R Γ₀ e₂ T Γ₂ →
      Ctx.join Γ₁ Γ₂ = some Γ' →
      Typed P R Γ (.ite c e₁ e₂) T Γ'
  /-- (Call) §5.8, by value: the callee's signature is looked up in the
  program, the arguments are checked against the parameter list in order with
  Σ threaded left to right, and the call's type is the callee's return type
  (`4.10:5`, `4.10:3`, `4.10:4`). The rule's by-reference clauses, `Λ_call`
  and its consistency and entry-recheck premises (§5.4), and the `Tr ≠ never`
  side condition with its (Call-Bottom) companion are not modelled: the
  fragment has no borrows and no `never` type. -/
  | call {Γ Γ' f args fd} :
      P[f]? = some fd →
      TypedArgs P R Γ args (fd.params.map Param.ty) Γ' →
      Typed P R Γ (.call f args) fd.ret Γ'
  /-- (Return-Value) §5.7 with (Sub-Never) folded in: the operand is checked
  against the enclosing function's declared return type `R`; §5.6's `⊥_exit`
  obligation is the frame-wide residual-linear premise (no binding of the
  current frame is still `Owned` at a linear type — `3.8:62`, and (Fn) §5.8's
  second clause, which is why an early `return` past a live linear is
  rejected). The conclusion is at an arbitrary type, which is (Sub-Never)
  §5.7 applied to `never`, and at an arbitrary outgoing context of the same
  skeleton, which is `⊥`: §5.5's join reads no state from a diverging arm, so
  the context may be taken to be whatever the join needs. (Return-Bottom) is
  subsumed — a `return` whose operand itself diverges types by this rule with
  the operand at `R`. -/
  | ret {Γ Γ₁ Γ' e T} :
      Typed P R Γ e R Γ₁ →
      NoOwnedLinear Γ₁ →
      Ctx.skel Γ' = Ctx.skel Γ₁ →
      Typed P R Γ (.ret e) T Γ'

/-- The argument list of §5.8's (Call), typed left to right with Σ threaded
(`Σ0 = Σ`, then `Γ;Σ_{i-1};Λ ⊢ e ⇒ Ti ⊣ Σi` for each `i`), every argument by
value. The by-reference argument forms, and with them `Λ_call`, its
consistency premise and the call-entry recheck, are not in the fragment. -/
inductive TypedArgs (P : Program) (R : Ty) : Ctx → List Expr → List Ty → Ctx → Prop where
  /-- The empty argument list leaves Σ alone (`Σ0 = Σ`, §5.8). -/
  | nil {Γ} : TypedArgs P R Γ [] [] Γ
  /-- One by-value argument is a value-context use at its parameter's type
  (§4.2), threading Σ into the rest of the list (§5.8). -/
  | cons {Γ Γ₁ Γ₂ e es T Ts} :
      Typed P R Γ e T Γ₁ → TypedArgs P R Γ₁ es Ts Γ₂ →
      TypedArgs P R Γ (e :: es) (T :: Ts) Γ₂
end

/-- (Fn) §5.8: a function is well-formed when its body checks at its declared
return type from the entry context `Γ0;Σ0` (`fnCtx`), and the body's normal
exit edge discharges §5.6's obligation for every by-value parameter and every
still-open body-local binding (`3.8:62` — a by-value parameter carrying a
linear value must be consumed on every non-diverging path). The rule's early
exits are covered by `Typed.ret`, which carries the same premise at the edge
where the frame's scopes end (§5.7's `⊥_exit`). -/
def WfFn (P : Program) (fd : FnDef) : Prop :=
  ∃ Γf, Typed P fd.ret (fnCtx fd) fd.body fd.ret Γf ∧ NoOwnedLinear Γf

/-- A well-formed program: every function satisfies (Fn) §5.8. Recursion is
ordinary — a body may call any function of the program, itself included, since
(Call) reads only the callee's signature (§5.8, "the core is fully
monomorphic"). -/
def WfProgram (P : Program) : Prop := ∀ fd ∈ P, WfFn P fd

/-- A whole program, ready to run (§6.12's top-level result): every function
is well-formed by (Fn) §5.8, and the entry point — index `0`, the function
`Dynamics.run` calls — takes no parameters, so `main()` is a call (Call) §5.8
accepts with an empty argument list. -/
structure ProgramTyped (P : Program) : Prop where
  /-- (Fn) §5.8 holds of every function of the program. -/
  fns : WfProgram P
  /-- The entry point exists and takes no arguments (`4.10:3`). -/
  entry : ∃ fd, P[0]? = some fd ∧ fd.params = []

/-! ## Skeleton preservation -/

/-- The §5.5 join preserves an entry's skeleton (helper). -/
theorem Entry.join_skel {a b e : Entry} (h : a.join b = some e) :
    e.skel = a.skel := by
  unfold Entry.join at h
  split at h
  · cases h; rfl
  · split at h
    · cases h
    · cases h; rfl

/-- Setting an index to the element already there is the identity (helper). -/
theorem List.set_self_of_getElem? {α} : ∀ {l : List α} {i : Nat} {a : α},
    l[i]? = some a → l.set i a = l
  | [], i, a, h => by simp at h
  | x :: xs, 0, a, h => by simp_all
  | x :: xs, i + 1, a, h => by
      simp only [List.getElem?_cons_succ] at h
      simp [List.set_self_of_getElem? h]

/-- Re-marking an entry's ownership state does not change the skeleton
(helper). -/
theorem skel_set_setSt {Γ : Ctx} {i : Nat} {en : Entry} (h : Γ[i]? = some en)
    (s : OwnState) : Ctx.skel (Γ.set i (en.setSt s)) = Ctx.skel Γ := by
  unfold Ctx.skel
  rw [List.map_set]
  exact List.set_self_of_getElem? (by simp [h]; rfl)

/-- The §5.5 join preserves the context skeleton (helper). -/
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

/-- The §5.5 join of a context with itself is that context: two arms that
deliver the same Σ agree everywhere, which is how a branch whose arms both
diverge joins (§5.7: a diverging arm contributes no state, so the join reads
the same context twice) (helper). -/
theorem Ctx.join_self : ∀ Γ : Ctx, Ctx.join Γ Γ = some Γ
  | [] => rfl
  | a :: as => by
      unfold Ctx.join Entry.join
      simp [Ctx.join_self as]

/-- Every rule preserves the context skeleton: only ownership states flow.
This is the fused context's image of §5's convention that `Γ` is fixed while
`Σ` is threaded through the judgment. The `ret` rule's arbitrary outgoing
context (§5.7's `⊥`) is restricted to the same skeleton for exactly this
reason. -/
theorem Typed.skel_preserved {P R} {Γ Γ' : Ctx} {e T} (h : Typed P R Γ e T Γ') :
    Γ'.skel = Γ.skel := by
  induction h using Typed.rec
    (motive_2 := fun Γ _ _ Γ' _ => Ctx.skel Γ' = Ctx.skel Γ) with
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
  | call _ _ ih => exact ih
  | ret _ _ hskel ih => exact hskel.trans ih
  | nil => rfl
  | cons _ _ ih ihs => exact ihs.trans ih

/-- The argument list preserves the context skeleton too (helper). -/
theorem TypedArgs.skel_preserved {P R} {Γ Γ' : Ctx} {es Ts} (h : TypedArgs P R Γ es Ts Γ') :
    Γ'.skel = Γ.skel := by
  induction h using TypedArgs.rec
    (motive_1 := fun Γ _ _ Γ' _ => Ctx.skel Γ' = Ctx.skel Γ) with
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
  | call _ _ ih => exact ih
  | ret _ _ hskel ih => exact hskel.trans ih
  | nil => rfl
  | cons _ _ ih ihs => exact ihs.trans ih

end RueCore
