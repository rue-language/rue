import RueCore.Syntax

/-!
# RueCore.Statics — ownership-threading typing (§5)

The judgment `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` of the calculus, with `Γ` and `Σ` fused into
one flow-sensitive context: a list of entries carrying the fixed skeleton
(type, mutability mark) and the flowing ownership state. The judgment's output
context has the same skeleton with updated states (`skel_preserved`).

The judgment is parameterized by the program `P` — the struct environment
(Struct-Intro) §5.8 looks a declaration up in and the top-level function
environment (Call) §5.8 looks a callee up in — and by `R`, the enclosing
function's declared return type, which is what (Return-Value) §5.7 checks a
`return` operand against. Both are fixed for a whole derivation, as the
calculus fixes them for a function body.

Loans (Λ) are omitted: the fragment has no borrows, and Λ is ambiently empty
in the current core (§5 preamble).

## Structs and their class

§3 assigns a struct the join of its field classes lifted by the declared
attribute, and a declaration records that class (`Syntax.lean`). `WfStructs`
is §3's equation, made a premise of a well-formed program: the recorded class
*is* the lifted join, a `@copy` declaration's join is already `Copy` and it
declares no destructor (`3.8:18`, `3.9:31`), a destructor-bearing declaration
carries no linear field (`3.9:44`, E0462 — `3.9:34` forbids moving one out, so
the obligation could only be met by the glue, which is the implicit discard
§5.6 forbids; §3 states this as a well-formedness condition on the
declaration, beside the `@copy` one), and a field may name only an
earlier declaration, so the equation is a definition rather than a fixpoint
condition — `struct_class_unique` is that statement, proved.

`3.9:44` is stated "through any depth of struct nesting", and `dtorWf` looks
one level down — at `sd.baseOf D`, the join of the *immediate* field classes.
The two agree because `WfStructs` holds at every declaration, the fields'
included: a field whose type is linear only by infection has `baseOf = Linear`
at *its* declaration, and `Attr.lift` then forces its recorded class to
`Linear` for every attribute `copyWf` permits. So a linear value at any depth
has already reached the immediate field's class by the time `dtorWf` reads the
join, and one level is the whole depth.

`carries_linear(T) ⟺ class(T) = Linear` is §5.3's own reading, so it is a
definition here (`Ty.carriesLinear`); what §5.3 asks to be checked is the
*lifting*, and `struct_carriesLinear_iff` is it: a struct's class reaches
`Linear` exactly when the declaration says `linear` or some field carries a
linear value.

## Divergence, without a `never` type

§5.7 types `return e` at `never` and lets (Sub-Never) coerce it to any type,
with a divergent outgoing state `⊥` that §5.5's join excludes. The fragment
folds both into one rule at each never-typed form: `Typed.ret` and
`Typed.panic` conclude at **any** type `T` and with **any** outgoing context
of the same skeleton, which is exactly what a `never` value and a `⊥` state
license a context to assume. `Ty` therefore needs no `never` constructor and
`HasTy` (`Soundness.lean`) no case for it — sound because `never` has no
values (`3.4:1`), so nothing is ever typed at it dynamically. Adding the
constructor would buy nothing here and cost something: every rule that demands
two equal types (§5.5's arms, (Assign)'s target) would have to admit a
subsumption it can never observe. (Return-Bottom) needs no rule of its own for
the same reason: a `return` whose operand itself diverges is typed by this
rule with the operand at `R`. `INDEX.md` records (Sub-Never) as mechanized at
these two forms, the only never-typed forms the fragment has.

The two differ in one premise, and the difference is §5.7's provenance.
`return` carries `⊥_exit`, which is the §5.6 scope-exit obligation taken
frame-wide, so `Typed.ret` demands `NoOwnedLinear`. `@panic` carries
`⊥_panic`, which §5.7 exempts from that check — "§5.6 performs no scope-exit
check or drop on that edge" — so `Typed.panic` demands nothing of the
context, and §6.12's dynamics run no drop to match.

An *algorithm* cannot leave a type and a state free, so `check`
(`Checker.lean`) picks one of each — the enclosing return type and the state
in force at the form — and its module docstring says what completeness
that costs.
-/

namespace RueCore

/-! ## §3's lattice, and the class of a declared struct -/

/-- The join is an upper bound of its left argument (§3) (helper). -/
theorem Mult.rank_le_join_left (a b : Mult) : a.rank ≤ (a.join b).rank := by
  unfold Mult.join
  split
  · omega
  · exact Nat.le_refl _

/-- The join is an upper bound of its right argument (§3) (helper). -/
theorem Mult.rank_le_join_right (a b : Mult) : b.rank ≤ (a.join b).rank := by
  unfold Mult.join
  split
  · exact Nat.le_refl _
  · omega

/-- `Linear` is the top of §3's lattice, so nothing outranks it (helper). -/
theorem Mult.eq_linear_of_rank {m : Mult} (h : 2 ≤ m.rank) : m = .linear := by
  cases m <;> simp_all [Mult.rank]

/-- §3's field join, over the field types of one declaration: `⊔ { class(Ti) }`
read left to right. `Attr.lift` then lifts it by the declared attribute. -/
def StructDecl.baseOf (D : StructEnv) (sd : StructDecl) : Mult :=
  sd.fields.foldl (fun m T => m.join (Ty.mult D T)) .copy

/-- The accumulator of §3's join is a lower bound of the result (helper). -/
theorem rank_le_joinFold (D : StructEnv) : ∀ (Ts : List Ty) (acc : Mult),
    acc.rank ≤ (Ts.foldl (fun m T => m.join (Ty.mult D T)) acc).rank
  | [], _ => Nat.le_refl _
  | _ :: Ts, acc =>
      Nat.le_trans (Mult.rank_le_join_left acc _) (rank_le_joinFold D Ts _)

/-- Every field's class is below §3's join of them (helper). -/
theorem rank_le_joinFold_of_mem (D : StructEnv) : ∀ (Ts : List Ty) (acc : Mult) (T : Ty),
    T ∈ Ts → (T.mult D).rank ≤ (Ts.foldl (fun m T => m.join (Ty.mult D T)) acc).rank
  | T' :: Ts, acc, T, h => by
      cases h with
      | head =>
          exact Nat.le_trans (Mult.rank_le_join_right acc _) (rank_le_joinFold D Ts _)
      | tail _ h => exact rank_le_joinFold_of_mem D Ts _ T h

/-- §3's join reaches `Linear` only through a field that does (helper). -/
theorem joinFold_linear_inv (D : StructEnv) : ∀ (Ts : List Ty) (acc : Mult),
    Ts.foldl (fun m T => m.join (Ty.mult D T)) acc = .linear →
      acc = .linear ∨ ∃ T ∈ Ts, T.mult D = .linear
  | [], _, h => Or.inl h
  | T :: Ts, acc, h => by
      simp only [List.foldl_cons] at h
      rcases joinFold_linear_inv D Ts _ h with h' | ⟨T', hmem, hT'⟩
      · have h'' : Mult.join acc (Ty.mult D T) = .linear := h'
        unfold Mult.join at h''
        split at h''
        · exact Or.inr ⟨T, List.mem_cons_self, h''⟩
        · exact Or.inl h''
      · exact Or.inr ⟨T', List.mem_cons_of_mem _ hmem, hT'⟩

/-- One declaration's well-formedness (§3, `3.8:18`, `3.9:31`, `3.9:44`): its
recorded class is §3's field join lifted by its attribute, a `@copy`
declaration's join is already `Copy` and it declares no destructor, a
destructor-bearing declaration carries no linear field, and a field names only
an **earlier** declaration — so the class equation is solvable in one pass and
has one solution (`struct_class_unique`), and no struct contains itself. -/
structure StructDecl.Wf (D : StructEnv) (s : Nat) (sd : StructDecl) : Prop where
  /-- No recursive struct: a field may name only an earlier declaration. -/
  fieldsEarlier : ∀ s', Ty.struct s' ∈ sd.fields → s' < s
  /-- §3's assignment: `class(S) = attr(S) lifted over ⊔ { class(Ti) }`. -/
  classIsJoin : sd.cls = sd.attr.lift (sd.baseOf D)
  /-- `3.8:18` and `3.9:31`: `@copy` is well-formed only when every field is
  `Copy` and the struct declares no destructor. -/
  copyWf : sd.attr = .copy → sd.baseOf D = .copy ∧ sd.dtor = false
  /-- `3.9:44` (E0462): a struct that declares a destructor must not carry a
  linear value in a field. `3.9:34` forbids moving a field out of such a
  value, so the field's obligation could only ever be met by the drop glue
  §6.11 runs after the destructor — which is exactly the implicit discard
  §5.6 forbids. A *declared*-`linear` struct may still have a destructor: the
  condition is on the field join, not on the class. -/
  dtorWf : sd.dtor = true → sd.baseOf D ≠ .linear

/-- A well-formed struct environment: §3's class assignment holds of every
declaration (`StructDecl.Wf`). This is the premise that makes `Ty.mult`'s
lookup §3's join, and it is what `checkStructs` (`Checker.lean`) decides. -/
def WfStructs (D : StructEnv) : Prop := ∀ s sd, D[s]? = some sd → StructDecl.Wf D s sd

/-- **A droppable struct carries no linear field.** If a declaration's class
is not `Linear`, no field's class is — which is why the machine's leak monitor
(§6.7's `endscope`, §6.9's frame teardown) needs to look only at the value's
own class and never inside it. This is §3's infectiousness, used. -/
theorem StructDecl.Wf.field_not_linear {D : StructEnv} {s : Nat} {sd : StructDecl}
    (h : sd.Wf D s) (hcls : sd.cls ≠ .linear) : ∀ T ∈ sd.fields, T.mult D ≠ .linear := by
  intro T hmem hlin
  have hbase : sd.baseOf D = .linear := by
    refine Mult.eq_linear_of_rank ?_
    have := rank_le_joinFold_of_mem D sd.fields .copy T hmem
    rw [hlin] at this
    exact this
  refine hcls ?_
  rw [h.classIsJoin, hbase]
  cases hattr : sd.attr with
  | none => rfl
  | linear => rfl
  | copy =>
      have := (h.copyWf hattr).1
      rw [hbase] at this
      cases this

/-- **`carries_linear` lifts through the fields** (§5.3). A struct's class
reaches `Linear` exactly when its declaration says `linear` (`3.8:57`) or some
field carries a linear value (`3.8:58` — infectiousness is the join). Together
with `Ty.carriesLinear`'s definition this is §5.3's sentence, mechanized. -/
theorem struct_carriesLinear_iff {D : StructEnv} {s : Nat} {sd : StructDecl}
    (hd : D[s]? = some sd) (h : sd.Wf D s) :
    (Ty.struct s).mult D = .linear ↔
      (sd.attr = .linear ∨ ∃ T ∈ sd.fields, T.mult D = .linear) := by
  have hlookup : (Ty.struct s).mult D = sd.cls := by
    simp [Ty.mult, StructEnv.classOf, hd]
  constructor
  · intro hlin
    rw [hlookup, h.classIsJoin] at hlin
    cases hattr : sd.attr with
    | linear => exact Or.inl rfl
    | copy => rw [hattr] at hlin; cases hlin
    | none =>
        rw [hattr] at hlin
        simp only [Attr.lift] at hlin
        split at hlin
        · rename_i hbase
          rcases joinFold_linear_inv D sd.fields .copy hbase with h' | ⟨T, hmem, hT⟩
          · cases h'
          · exact Or.inr ⟨T, hmem, hT⟩
        · cases hlin
  · intro hsrc
    rw [hlookup, h.classIsJoin]
    rcases hsrc with hattr | ⟨T, hmem, hT⟩
    · rw [hattr]; rfl
    · have hbase : sd.baseOf D = .linear := by
        refine Mult.eq_linear_of_rank ?_
        have := rank_le_joinFold_of_mem D sd.fields .copy T hmem
        rw [hT] at this
        exact this
      rw [hbase]
      cases hattr : sd.attr with
      | none => rfl
      | linear => rfl
      | copy =>
          have := (h.copyWf hattr).1
          rw [hbase] at this
          cases this

/-! ## The class a declaration records is determined, not free

A declaration carries `class(S)` so that `Ty.mult` is a lookup. That is only
honest if §3's equation has one solution, which is what acyclicity buys: read
left to right, declaration `s`'s class is fixed by the classes of the
declarations before it. -/

/-- Two environments that agree on every class below `n` give the same §3 join
to a field list that names only declarations below `n` (helper). -/
theorem joinFold_congr {D D' : StructEnv} {n : Nat}
    (hcls : ∀ s', s' < n → D.classOf s' = D'.classOf s') :
    ∀ (Ts : List Ty) (acc : Mult), (∀ s', Ty.struct s' ∈ Ts → s' < n) →
      Ts.foldl (fun m T => m.join (Ty.mult D T)) acc
        = Ts.foldl (fun m T => m.join (Ty.mult D' T)) acc
  | [], _, _ => rfl
  | T :: Ts, acc, hearly => by
      have hT : T.mult D = T.mult D' := by
        cases T with
        | struct s' => exact hcls s' (hearly s' List.mem_cons_self)
        | _ => rfl
      simp only [List.foldl_cons, hT]
      exact joinFold_congr hcls Ts _ (fun s' hm => hearly s' (List.mem_cons_of_mem _ hm))

/-- **§3's class assignment has one solution.** Two well-formed environments
of the same length whose declarations agree on their attributes and field
lists agree on every class. So recording `class(S)` in the declaration
(`Syntax.lean`) records a determined value rather than a free parameter: it is
§3's join, and `WfStructs` is the equation that says so. -/
theorem struct_class_unique {D D' : StructEnv} (hwf : WfStructs D) (hwf' : WfStructs D')
    (hlen : D.length = D'.length)
    (hshape : ∀ (s : Nat) (sd sd' : StructDecl), D[s]? = some sd → D'[s]? = some sd' →
      sd.attr = sd'.attr ∧ sd.fields = sd'.fields) :
    ∀ s, D.classOf s = D'.classOf s := by
  intro s
  induction s using Nat.strongRecOn with
  | _ s ih =>
    cases hd : D[s]? with
    | none =>
        have : D'[s]? = none := by
          rcases hd' : D'[s]? with _ | sd'
          · rfl
          · exact absurd (List.getElem?_eq_some_iff.mp hd' |>.1)
              (by have := List.getElem?_eq_none_iff.mp hd; omega)
        simp [StructEnv.classOf, hd, this]
    | some sd =>
        have hlt : s < D'.length := by
          have := List.getElem?_eq_some_iff.mp hd |>.1; omega
        obtain ⟨sd', hd'⟩ : ∃ sd', D'[s]? = some sd' := by
          rcases hd' : D'[s]? with _ | sd'
          · exact absurd (List.getElem?_eq_none_iff.mp hd') (by omega)
          · exact ⟨sd', rfl⟩
        obtain ⟨hattr, hfields⟩ := hshape s sd sd' hd hd'
        have hw := hwf s sd hd
        have hw' := hwf' s sd' hd'
        have hbase : sd.baseOf D = sd'.baseOf D' := by
          unfold StructDecl.baseOf
          rw [← hfields]
          exact joinFold_congr (fun s' hs' => ih s' hs') sd.fields .copy
            (fun s' hm => hw.fieldsEarlier s' hm)
        simp only [StructEnv.classOf, hd, hd']
        rw [hw.classIsJoin, hw'.classIsJoin, hattr, hbase]

/-! ## The fused `Γ ; Σ` context -/

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
`Σ(x) = Owned ∧ class(T) = Linear` (whole bindings, no paths: a partially
moved struct is RUE-2231's). -/
def NoOwnedLinear (D : StructEnv) (Γ : Ctx) : Prop :=
  ∀ en ∈ Γ, ¬(en.st = .owned ∧ en.ty.mult D = .linear)

instance (D : StructEnv) (Γ : Ctx) : Decidable (NoOwnedLinear D Γ) := by
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
def Entry.join (D : StructEnv) (a b : Entry) : Option Entry :=
  if a.st = b.st then some a
  else if a.ty.mult D = .linear then none
  else some (a.setSt .movedOut)

/-- The §5.5 branch join, pointwise. Defined only on equal-length contexts
(the two arms extend one incoming context, so lengths always agree). -/
def Ctx.join (D : StructEnv) : Ctx → Ctx → Option Ctx
  | [], [] => some []
  | a :: as, b :: bs =>
      match a.join D b, Ctx.join D as bs with
      | some e, some rest => some (e :: rest)
      | _, _ => none
  | _, _ => none

mutual
/-- `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` (§5), over the fused context, under the program `P`
and the enclosing function's return type `R`.

Rule names cite the calculus: `useCopy`/`useMove` are (Use-Copy)/(Use-Move)
(§5.1); `dropCopy`/`dropRes` are (@Drop-Copy)/(@Drop) (§5.3); `mkStruct` is
(Struct-Intro) (§5.8); `letIn` folds in §5.6's residual-linear scope-exit
check; `assign` is (Assign) with the `3.8:77` linear-overwrite premise on the
*post-RHS* state; `seq` is (Seq) with the `3.8:64` discard check; `ite` is (If)
with the §5.5 join; `call` is (Call) by value (§5.8); `ret` is (Return-Value)
with (Sub-Never) folded in (§5.7). -/
inductive Typed (P : Program) (R : Ty) : Ctx → Expr → Ty → Ctx → Prop where
  /-- (Lit) §5.8: an integer literal at the `int(w,s)` elaboration resolved
  for it (`4.1:2`), denoting a value of that type (§6.1's `n_T` bound). -/
  | intLit {Γ w s n} :
      InBounds w s n →
      Typed P R Γ (.intLit w s n) (.int w s) Γ
  /-- (Lit) §5.8: a boolean literal. -/
  | boolLit {Γ b} :
      Typed P R Γ (.boolLit b) .bool Γ
  /-- (Lit) §5.8: the unit literal. -/
  | unitLit {Γ} :
      Typed P R Γ .unitLit .unit Γ
  /-- (Use-Copy): a use of a `Copy` place copies; Σ unchanged. -/
  | useCopy {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult P.structs = .copy →
      Typed P R Γ (.use i) en.ty Γ
  /-- (Use-Move): a use of an `Affine`/`Linear` place moves it out. -/
  | useMove {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult P.structs ≠ .copy →
      Typed P R Γ (.use i) en.ty (Γ.set i (en.setSt .movedOut))
  /-- (Arith) and (Ord) §5.8, in one rule because they differ only in the
  type they conclude at (`BinOp.resultTy`): both operands share one
  `int(w,s)`, typed left to right with Σ threaded (`4.2:1`), and the result is
  that same type for the arithmetic, bitwise and shift operators and `bool`
  for the ordering compares (`4.3:1`). The shift operators take their amount
  at the shifted operand's own type, which is `4.3a:9` and is why they need no
  second operand type here. -/
  | binop {Γ Γ₁ Γ₂ op e₁ e₂ w s} :
      Typed P R Γ e₁ (.int w s) Γ₁ → Typed P R Γ₁ e₂ (.int w s) Γ₂ →
      Typed P R Γ (.binop op e₁ e₂) (op.resultTy (.int w s)) Γ₂
  /-- (Neg) §5.8: negation demands a **signed** operand (`4.2:6`; rejecting it
  on an unsigned type is `4.2:14`) and concludes at that type. -/
  | neg {Γ Γ' e w} :
      Typed P R Γ e (.int w .signed) Γ' →
      Typed P R Γ (.unop .neg e) (.int w .signed) Γ'
  /-- (Not) §5.8: logical negation demands `bool` (`4.4:2`). The bitwise
  operators do not accept `bool` at all (`4.3a:18`, `4.3a:19`), which is why
  `binop` above is stated only at `int(w,s)`. -/
  | notOp {Γ Γ' e} :
      Typed P R Γ e .bool Γ' →
      Typed P R Γ (.unop .not e) .bool Γ'
  /-- (BitNot) §5.8: the bitwise complement takes any integer type
  (`4.3a:3`, `4.3a:4`) and concludes at it. -/
  | bitnot {Γ Γ' e w s} :
      Typed P R Γ e (.int w s) Γ' →
      Typed P R Γ (.unop .bitnot e) (.int w s) Γ'
  /-- (Int-Cast) §5.8 (`4.13:24`–`4.13:27`): the operand is any integer type
  and the result is the one elaboration took from the use site, which the form
  carries. Whether the value survives the conversion is dynamic (`4.13:28`,
  §6.4's own trap rule), not a typing question. -/
  | intCast {Γ Γ' w s w' s' e} :
      Typed P R Γ e (.int w' s') Γ' →
      Typed P R Γ (.intCast w s e) (.int w s) Γ'
  /-- (Panic) §5.8 with (Sub-Never) folded in (§5.7), the same fold
  `Typed.ret` makes: `@panic` is `never`-typed, so the rule concludes at an
  arbitrary type and — since `⊥` contributes no state to a join — at an
  arbitrary outgoing context of the same skeleton. Unlike `ret` it imposes no
  residual-linear premise: §5.7 exempts the `⊥_panic` edge from §5.6's
  scope-exit check, and §6.12's own rule runs no drop. The message is a string
  literal the form carries rather than an operand, because the fragment has no
  string type, which is also why §5.8's operand-diverging companion has no
  instance. -/
  | panic {Γ Γ' T msg} :
      Ctx.skel Γ' = Ctx.skel Γ →
      Typed P R Γ (.panic msg) T Γ'
  /-- (Dbg) §5.8: the operand is a value-context use of a type `@dbg` renders
  — `int(w,s)` or `bool` in this fragment (`Ty.observable`; the compiler
  rejects an aggregate with E0702) — and the form itself is `unit`. -/
  | dbg {Γ Γ' e T} :
      Typed P R Γ e T Γ' → T.observable = true →
      Typed P R Γ (.dbg e) .unit Γ'
  /-- (Struct-Intro) §5.8: one initializer per declared field, typed in
  declaration order at its field's type with Σ threaded left to right
  (`3.6:5`, `3.6:6`, `3.6:15`), and the result owns every field — which is why
  `class(S)` is the field join of §3. -/
  | mkStruct {Γ Γ' s args sd} :
      P.structs[s]? = some sd →
      TypedArgs P R Γ args sd.fields Γ' →
      Typed P R Γ (.mkStruct s args) (.struct s) Γ'
  /-- The fragment's whole-value struct elimination: it takes the struct by
  value (a §4.2 use of its operand's places happens inside `e`'s own typing)
  and yields the first field's payload. It is not a calculus rule — the
  calculus reads a field through a projection, which is a place and therefore
  RUE-2231's — so its side condition is the fragment's
  (`StructDecl.Consumable`: every field `int`, no destructor). -/
  | consume {Γ Γ' s sd e} :
      Typed P R Γ e (.struct s) Γ' →
      P.structs[s]? = some sd → sd.Consumable →
      Typed P R Γ (.consume e) sd.payloadTy Γ'
  /-- (@Drop-Copy): no drop glue, no ownership effect. -/
  | dropCopy {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult P.structs = .copy →
      Typed P R Γ (.drop i) .unit Γ
  /-- (@Drop): consumes the operand and discharges its (affine or linear)
  obligation; the only non-move discharge of a linear obligation. The
  operand is a whole binding, so the rule's projection side conditions
  (`3.9:34`, `3.8:68`) and its partial-move clause have no instance here. -/
  | dropRes {Γ i en} :
      Γ[i]? = some en → en.st = .owned → en.ty.mult P.structs ≠ .copy →
      Typed P R Γ (.drop i) .unit (Γ.set i (en.setSt .movedOut))
  /-- (Let) + §5.6 scope exit: the binder enters `Owned`; at the body's end
  its residual state must not be an unconsumed linear value (the leak check).
  An `Owned` affine residue is dropped by the machine (§6.7); `MovedOut` needs
  nothing. -/
  | letIn {Γ Γ₁ Γ₂ m e₁ e₂ T₁ T₂ en'} :
      Typed P R Γ e₁ T₁ Γ₁ →
      Typed P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ T₂ (en' :: Γ₂) →
      ¬(en'.st = .owned ∧ T₁.mult P.structs = .linear) →
      Typed P R Γ (.letIn m e₁ e₂) T₂ Γ₂
  /-- (Assign): RHS first; overwrite of a live linear value is ill-formed
  (`3.8:77`, checked on the post-RHS state — the RUE-387 premise); the target
  is `Owned` afterward (reinitialization, `3.8:55`). -/
  | assign {Γ Γ₁ i e en₀ en₁} :
      Γ[i]? = some en₀ → en₀.mu = true →
      Typed P R Γ e en₀.ty Γ₁ →
      Γ₁[i]? = some en₁ →
      (en₁.st = .movedOut ∨ en₀.ty.mult P.structs ≠ .linear) →
      Typed P R Γ (.assign i e) .unit (Γ₁.set i (en₁.setSt .owned))
  /-- (Seq): the discarded value must not carry a linear value (`3.8:64`). -/
  | seq {Γ Γ₁ Γ₂ e₁ e₂ T₁ T₂} :
      Typed P R Γ e₁ T₁ Γ₁ → T₁.mult P.structs ≠ .linear →
      Typed P R Γ₁ e₂ T₂ Γ₂ →
      Typed P R Γ (.seq e₁ e₂) T₂ Γ₂
  /-- (If): both arms from the post-scrutinee state; outgoing state is the
  §5.5 join. -/
  | ite {Γ Γ₀ Γ₁ Γ₂ Γ' c e₁ e₂ T} :
      Typed P R Γ c .bool Γ₀ →
      Typed P R Γ₀ e₁ T Γ₁ → Typed P R Γ₀ e₂ T Γ₂ →
      Ctx.join P.structs Γ₁ Γ₂ = some Γ' →
      Typed P R Γ (.ite c e₁ e₂) T Γ'
  /-- (Call) §5.8, by value: the callee's signature is looked up in the
  program, the arguments are checked against the parameter list in order with
  Σ threaded left to right, and the call's type is the callee's return type
  (`4.10:5`, `4.10:3`, `4.10:4`). The rule's by-reference clauses, `Λ_call`
  and its consistency and entry-recheck premises (§5.4), and the `Tr ≠ never`
  side condition with its (Call-Bottom) companion are not modelled: the
  fragment has no borrows and no `never` type. -/
  | call {Γ Γ' f args fd} :
      P.fns[f]? = some fd →
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
      NoOwnedLinear P.structs Γ₁ →
      Ctx.skel Γ' = Ctx.skel Γ₁ →
      Typed P R Γ (.ret e) T Γ'

/-- An expression list typed left to right against a list of expected types
with Σ threaded (`Σ0 = Σ`, then `Γ;Σ_{i-1};Λ ⊢ e ⇒ Ti ⊣ Σi` for each `i`).
Both §5.8 forms that take one use it: (Call)'s by-value argument list and
(Struct-Intro)'s field initializers. (Call)'s by-reference argument forms, and
with them `Λ_call`, its consistency premise and the call-entry recheck, are
not in the fragment. -/
inductive TypedArgs (P : Program) (R : Ty) : Ctx → List Expr → List Ty → Ctx → Prop where
  /-- The empty list leaves Σ alone (`Σ0 = Σ`, §5.8). -/
  | nil {Γ} : TypedArgs P R Γ [] [] Γ
  /-- One member is a value-context use at its expected type (§4.2),
  threading Σ into the rest of the list (§5.8). -/
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
  ∃ Γf, Typed P fd.ret (fnCtx fd) fd.body fd.ret Γf ∧ NoOwnedLinear P.structs Γf

/-- A well-formed program: §3's class assignment holds of every struct
declaration and (Fn) §5.8 of every function. Recursion is ordinary — a body
may call any function of the program, itself included, since (Call) reads only
the callee's signature (§5.8, "the core is fully monomorphic") — while struct
declarations are *not* recursive (`StructDecl.Wf.fieldsEarlier`). -/
structure WfProgram (P : Program) : Prop where
  /-- §3's class assignment, for every declaration. -/
  structs : WfStructs P.structs
  /-- (Fn) §5.8, for every function. -/
  fns : ∀ fd ∈ P.fns, WfFn P fd

/-- A whole program, ready to run (§6.12's top-level result): every function
is well-formed by (Fn) §5.8, every struct declaration by §3, and the entry
point — function index `0`, the one `Dynamics.run` calls — takes no
parameters, so `main()` is a call (Call) §5.8 accepts with an empty argument
list. -/
structure ProgramTyped (P : Program) : Prop where
  /-- §3 and (Fn) §5.8 hold of the program. -/
  wf : WfProgram P
  /-- The entry point exists and takes no arguments (`4.10:3`). -/
  entry : ∃ fd, P.fns[0]? = some fd ∧ fd.params = []

/-! ## Skeleton preservation -/

/-- The §5.5 join preserves an entry's skeleton (helper). -/
theorem Entry.join_skel {D : StructEnv} {a b e : Entry} (h : a.join D b = some e) :
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
theorem Ctx.join_skel {D : StructEnv} : ∀ {Γ₁ Γ₂ Γ' : Ctx}, Ctx.join D Γ₁ Γ₂ = some Γ' →
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
theorem Ctx.join_self (D : StructEnv) : ∀ Γ : Ctx, Ctx.join D Γ Γ = some Γ
  | [] => rfl
  | a :: as => by
      unfold Ctx.join Entry.join
      simp [Ctx.join_self D as]

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
  | binop _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | neg _ ih => exact ih
  | notOp _ ih => exact ih
  | bitnot _ ih => exact ih
  | intCast _ ih => exact ih
  | panic hskel => exact hskel
  | dbg _ _ ih => exact ih
  | mkStruct _ _ ih => exact ih
  | consume _ _ _ ih => exact ih
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

/-- A typed expression list preserves the context skeleton too (helper). -/
theorem TypedArgs.skel_preserved {P R} {Γ Γ' : Ctx} {es Ts} (h : TypedArgs P R Γ es Ts Γ') :
    Γ'.skel = Γ.skel := by
  induction h using TypedArgs.rec
    (motive_1 := fun Γ _ _ Γ' _ => Ctx.skel Γ' = Ctx.skel Γ) with
  | intLit _ => rfl
  | boolLit => rfl
  | unitLit => rfl
  | useCopy _ _ _ => rfl
  | useMove hget _ _ => exact skel_set_setSt hget _
  | binop _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | neg _ ih => exact ih
  | notOp _ ih => exact ih
  | bitnot _ ih => exact ih
  | intCast _ ih => exact ih
  | panic hskel => exact hskel
  | dbg _ _ ih => exact ih
  | mkStruct _ _ ih => exact ih
  | consume _ _ _ ih => exact ih
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
