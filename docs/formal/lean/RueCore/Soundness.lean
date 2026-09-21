import RueCore.Dynamics

/-!
# RueCore.Soundness — the §7 memory-safety theorem, fragment-sized

The central invariant is `Matches D Γ ρ H` — "Σ faithfully tracks the store's
initialization", the load-bearing clause of §7's no-use-after-move bullet. `D`
is the program's struct declarations, which is what a type's class (§3) and a
value's drop (§6.11) are read against; every predicate here carries it.
`CellMatches` is deliberately asymmetric, mirroring §5.5: a *statically*
`MovedOut` entry may still hold a live (non-linear!) value dynamically —
that is exactly the state a conservative branch join produces, and the machine
drops such residues path-specifically (`3.8:73`); a statically `Owned` entry
always holds a well-typed value; and a live **linear** value is never behind a
`MovedOut` entry, which is what makes the leak/overwrite refusals unreachable.

## The frame invariant (§6.1, §6.9)

A frame carries an environment `ρ` and a scope record `σ`, and the machine
keeps two books on every live binding: `ρ` says where it is, `σ` says it is
owed a drop. `FrameMatches` states both halves at once — `Matches Γ ρ H`, and
`σ` reversed **is** `ρ`.

The second conjunct is **definitional in this fragment**, and it is worth
being plain about that. Every frame `eval` builds — the callee's frame at a
call, the extended frame inside a `let` body — builds σ and ρ from the same
list, so σ carries no information ρ does not and the equation cannot fail
here. It is stated as an invariant because it is what the *teardown* proofs
consume, and because it is the clause that stops being free the moment
`Frame.scope` becomes the stack §6.1 actually specifies: §6.6's `match` arms
and §6.10's loops push and pop scopes independently of the binder chain, and
then σ and ρ are two different books that a slice has to keep in step. That
is the shape the RUE-1277 redundancy was raised for, and this fragment does
not have it.

What the clause does buy, already: `run-all-scope-drops` walks σ, and because
σ is ρ, `Matches` — every cell live or moved-out, no two bindings sharing one
— applies to the walk. That is what turns §7's no-use-after-drop bullet from a
structural observation into a consequence of the invariant, and what proves no
unwind retires a cell twice or touches a `†` cell.

## Locality (`Untouched`)

A call runs the callee in a frame of its own, and the caller's invariant has
to survive it. `Untouched ρ H H'` is the statement that carries it: the store
only grows, and every cell below `|H|` that `ρ` does not name has the contents
it had. Since a callee's parameter cells are minted above the caller's whole
store, the caller's cells are outside the callee's `ρ`, and the caller's
`Matches` transports across the call unchanged.

## Structs: value typing, and the drop that never refuses

`HasTy D` types a struct value against its declaration's field list, which is
(Struct-Intro) §5.8 read on values. Statements about §6.11's walk rest on it
and are what the rest of the file uses. `dropValue_events` and
`dropValues_events` give the walk in **closed form** — for a well-typed value
it emits exactly `dropEvents`, §6.11's order written as a function — and
`dropValue_struct_events` is that read at a struct: the destructor's event
(`3.9:28`) followed by the concatenation of the fields' events in declaration
order (`3.9:13`), which is the shape RUE-2237's "dropped exactly once"
quantifies over. `dropValue_order` and `dropValues_order` are the one-level
induction steps, `dropValue_ok` the corollary that the walk never refuses, and
`StructDecl.Wf.field_not_linear` (`Statics.lean`) the reason the leak monitor
reads a value's own class and never descends into it.

## The theorem

`soundness` is type safety in definitional-interpreter form: a well-typed
expression evaluates to a well-typed value with the invariant restored
(preservation), to a value handed back by an unwinding `return`, to a *defined*
panic, or to `outOfFuel` — never to a `Violation` (progress). `fuel_mono` and
`no_masking` are what keep the fuel caveat honest. The corollaries at the
bottom restate the theorem per §7 bullet, over a whole program.
-/

namespace RueCore

/-! ## Value typing -/

mutual
/-- Value typing: §6.1's values against §2's types, with the `n_T` bounds and,
for a struct value, its declaration's field list — §6.1's `{ v1, …, vk }_S`
well typed at `S` exactly when each field value is well typed at its declared
type (§5.8's (Struct-Intro), read on values). §7's preservation half is stated
over this relation. -/
inductive HasTy (D : StructEnv) : Val → Ty → Prop where
  | int {w s n} : InBounds w s n → HasTy D (.int w s n) (.int w s)
  | bool {b} : HasTy D (.bool b) .bool
  | unit : HasTy D .unit .unit
  | struct {s sd vs} :
      D[s]? = some sd → HasTys D vs sd.fields → HasTy D (.struct s vs) (.struct s)

/-- Value typing for a value list, pointwise against the expected types: a
call's arguments against the callee's parameter types (§5.8's (Call),
`4.10:4`) and a struct value's fields against its declared field list. -/
inductive HasTys (D : StructEnv) : List Val → List Ty → Prop where
  | nil : HasTys D [] []
  | cons {v vs T Ts} : HasTy D v T → HasTys D vs Ts → HasTys D (v :: vs) (T :: Ts)
end

/-- A value list has as many values as expected types (`4.10:3`, `3.6:5`)
(helper). -/
theorem HasTys.length_eq : ∀ {D vs Ts}, HasTys D vs Ts → vs.length = Ts.length
  | _, _, _, .nil => rfl
  | _, _, _, .cons _ h => by simp [HasTys.length_eq h]

/-- A well-typed value has its type's class (helper). -/
theorem HasTy.mult_eq {D v T} (h : HasTy D v T) : v.mult D = T.mult D := by
  cases h <;> rfl

/-- Inversion of value typing at an integer type (helper). -/
theorem HasTy.int_inv {D v w s} (h : HasTy D v (.int w s)) :
    ∃ n, v = .int w s n ∧ InBounds w s n := by
  cases h; exact ⟨_, rfl, ‹_›⟩

/-- Inversion of value typing at `bool` (helper). -/
theorem HasTy.bool_inv {D v} (h : HasTy D v .bool) : ∃ b, v = .bool b := by
  cases h; exact ⟨_, rfl⟩

/-- Inversion of value typing at a struct type (helper). -/
theorem HasTy.struct_inv {D v s} (h : HasTy D v (.struct s)) :
    ∃ sd vs, v = .struct s vs ∧ D[s]? = some sd ∧ HasTys D vs sd.fields := by
  cases h; exact ⟨_, _, rfl, ‹_›, ‹_›⟩

/-- The fields of a value at a `Consumable` declaration start with an `int`,
which is the payload `Expr.consume` reads (`Syntax.lean`) (helper). -/
theorem HasTys.head_int {D vs Ts} (h : HasTys D vs Ts) (hne : Ts ≠ [])
    (hall : ∀ T ∈ Ts, T.isInt = true) :
    ∃ w s n vs', vs = .int w s n :: vs' ∧ InBounds w s n ∧ Ts.head? = some (.int w s) := by
  cases h with
  | nil => exact absurd rfl hne
  | @cons v vs' T Ts' hv _ =>
      have hT : T.isInt = true := hall T List.mem_cons_self
      cases T with
      | int w s =>
          obtain ⟨n, rfl, hb⟩ := hv.int_inv
          exact ⟨w, s, n, vs', rfl, hb, rfl⟩
      | bool => cases hT
      | unit => cases hT
      | struct s' => cases hT

/-! ## §6.4's operators produce a value of the rule's type, or a defined trap

`eval` hands each operator to `evalBinOp`/`evalUnOp`/`evalIntCast`
(`Dynamics.lean`), so the operator half of preservation and progress is these
three lemmas: on operands of the type §5.8's rule gives them, the operator
never refuses, and where it produces a value that value has the rule's result
type. Every trap it can produce instead is one of §6.12's, which the safety
theorem permits.
-/

/-- `range_check` (§6.4) delivers a value of `int(w,s)` or `↯overflow` — that
category and no other, which is what (D-Arith-Trap) says (helper). -/
theorem intResult_res {D : StructEnv} (w : IntWidth) (s : Sign) (n : Int) :
    (∃ v, intResult w s n = .val v ∧ HasTy D v (.int w s)) ∨
      intResult w s n = .trap .overflow := by
  unfold intResult
  by_cases hb : InBounds w s n
  · exact Or.inl ⟨_, by rw [if_pos hb], .int hb⟩
  · exact Or.inr (by rw [if_neg hb])

/-- **Every §6.4 integer operator lands on a value of its rule's type or on a
defined trap.** The value cases are (D-Arith), (D-Div), the remainder arm,
(D-Bit), (D-Shl)/(D-Shr) and `cmp`; the trap cases are (D-Arith-Trap),
(D-Div-Zero), (D-Div-Overflow) and the remainder's two. Nothing else is
reachable, which is the operator half of progress. -/
theorem binOpInt_res {D : StructEnv} (op : BinOp) (w : IntWidth) (s : Sign) (n₁ n₂ : Int) :
    (∃ v, binOpInt op w s n₁ n₂ = .val v ∧ HasTy D v (op.resultTy (.int w s))) ∨
      (∃ k, binOpInt op w s n₁ n₂ = .trap k) := by
  have bits : ∀ b : Nat, (∃ v, OpRes.val (Val.int w s (valOf w s b)) = .val v ∧
      HasTy D v (.int w s)) ∨ (∃ k, OpRes.val (Val.int w s (valOf w s b)) = .trap k) :=
    fun b => Or.inl ⟨_, rfl, .int (valOf_inBounds w s b)⟩
  -- The arithmetic arms trap in one category; this statement ranges over all
  -- of §6.4's, because `/` and `%` add their own.
  have hint : ∀ n : Int, (∃ v, intResult w s n = .val v ∧ HasTy D v (.int w s)) ∨
      (∃ k, intResult w s n = .trap k) :=
    fun n => (intResult_res w s n).imp id (fun h => ⟨.overflow, h⟩)
  cases op <;>
    simp only [binOpInt, BinOp.resultTy, BinOp.isCompare, if_true, if_false, Bool.false_eq_true]
  case add => exact hint _
  case sub => exact hint _
  case mul => exact hint _
  case div =>
      by_cases hz : n₂ = 0
      · exact Or.inr ⟨.divZero, by rw [if_pos hz]⟩
      · rw [if_neg hz]; exact hint _
  case rem =>
      by_cases hz : n₂ = 0
      · exact Or.inr ⟨.remZero, by rw [if_pos hz]⟩
      · rw [if_neg hz]
        by_cases hm : s = .signed ∧ n₁ = intMin w s ∧ n₂ = -1
        · exact Or.inr ⟨.overflow, by rw [if_pos hm]⟩
        · rw [if_neg hm]; exact hint _
  case bitAnd => exact bits _
  case bitOr => exact bits _
  case bitXor => exact bits _
  case shl => exact bits _
  case shr => cases s <;> exact bits _
  case lt => exact Or.inl ⟨_, rfl, .bool⟩
  case le => exact Or.inl ⟨_, rfl, .bool⟩
  case gt => exact Or.inl ⟨_, rfl, .bool⟩
  case ge => exact Or.inl ⟨_, rfl, .bool⟩

/-- The same, over the two machine values §5.8's operator rules give one
`int(w,s)`: the shape mismatch `evalBinOp` refuses is not reachable from
them. -/
theorem evalBinOp_res {D : StructEnv} (op : BinOp) (w : IntWidth) (s : Sign) (n₁ n₂ : Int) :
    (∃ v, evalBinOp op (.int w s n₁) (.int w s n₂) = .val v ∧
        HasTy D v (op.resultTy (.int w s))) ∨
      (∃ k, evalBinOp op (.int w s n₁) (.int w s n₂) = .trap k) := by
  simp only [evalBinOp]
  exact binOpInt_res op w s n₁ n₂

/-- **`neg` and `bitnot` land on a value of the operand's type or on
`↯overflow`** — that category and no other (§6.4; §5.8 restricts `neg` to a
signed operand, and the lemma here covers both signednesses because the range
check is what decides). -/
theorem evalUnOp_int_res {D : StructEnv} (op : UnOp) (w : IntWidth) (s : Sign) (n : Int)
    (hop : op ≠ .not) :
    (∃ v, evalUnOp op (.int w s n) = .val v ∧ HasTy D v (.int w s)) ∨
      evalUnOp op (.int w s n) = .trap .overflow := by
  cases op with
  | neg => exact intResult_res w s _
  | not => exact absurd rfl hop
  | bitnot => exact Or.inl ⟨_, rfl, .int (valOf_inBounds w s _)⟩

/-- **`not` on a `bool` is total** (§6.4's `Not`). -/
theorem evalUnOp_bool_res {D : StructEnv} (b : Bool) :
    ∃ v, evalUnOp .not (.bool b) = .val v ∧ HasTy D v .bool :=
  ⟨_, rfl, .bool⟩

/-- **`@intCast` lands on a value of its target type or on `↯cast-overflow`**
— that category and no other, which is what `4.13:28` and §6.4's
(D-Int-Cast-Trap) say. -/
theorem evalIntCast_res {D : StructEnv} (w : IntWidth) (s : Sign) (w' : IntWidth) (s' : Sign)
    (n : Int) :
    (∃ v, evalIntCast w s (.int w' s' n) = .val v ∧ HasTy D v (.int w s)) ∨
      evalIntCast w s (.int w' s' n) = .trap .castOverflow := by
  simp only [evalIntCast]
  split
  · exact Or.inl ⟨_, rfl, .int ‹_›⟩
  · exact Or.inr rfl

/-! ## Dropping a value never refuses, and drops in §6.11's order -/

/-- **§6.11's order, in closed form.** For a well-typed value the walk's
result is not just "the destructor then the fields" one level at a time: it is
exactly `dropEvents`, the order written out as a function (`Dynamics.lean`).
Together with `dropValues_events` this is the statement the per-level lemmas
below are the induction steps of. -/
theorem dropValue_events {D : StructEnv} {v : Val} {T : Ty} (h : HasTy D v T) :
    dropValue D v = .ok (dropEvents D v) := by
  induction h using HasTy.rec
    (motive_2 := fun vs _ _ => dropValues D vs = .ok (dropEventsList D vs)) with
  | int _ => rfl
  | bool => rfl
  | unit => rfl
  | @struct s sd vs hd _ ih => simp only [dropValue, dropEvents, hd, ih]
  | nil => rfl
  | cons _ _ ih ihs => simp only [dropValues, dropEventsList, ih, ihs]

/-- The same over a field list: `drop*` emits exactly the fields' events, in
declaration order (`3.9:13`). -/
theorem dropValues_events {D : StructEnv} {vs : List Val} {Ts : List Ty}
    (h : HasTys D vs Ts) : dropValues D vs = .ok (dropEventsList D vs) := by
  induction h using HasTys.rec
    (motive_1 := fun v _ _ => dropValue D v = .ok (dropEvents D v)) with
  | int _ => rfl
  | bool => rfl
  | unit => rfl
  | @struct s sd vs hd _ ih => simp only [dropValue, dropEvents, hd, ih]
  | nil => rfl
  | cons _ _ ih ihs => simp only [dropValues, dropEventsList, ih, ihs]

/-- **A well-typed value's drop always runs.** `dropValue` (§6.11) refuses
only where a struct value names a declaration the program does not have, and
value typing rules that out. -/
theorem dropValue_ok {D : StructEnv} {v : Val} {T : Ty} (h : HasTy D v T) :
    ∃ evs, dropValue D v = .ok evs :=
  ⟨dropEvents D v, dropValue_events h⟩

/-- **The drop-order theorem, in the shape RUE-2237 needs.** Dropping a
well-typed struct value emits its user destructor's event — when its
declaration has one (`3.9:28`) — followed by the **concatenation of its
fields' drop events, in declaration order** (`3.9:13`), each field's events
given by the same closed form, recursively. Nothing else, and nothing in
another order; the whole list is determined by the value and the
declarations. -/
theorem dropValue_struct_events {D : StructEnv} {s : Nat} {sd : StructDecl} {vs : List Val}
    (hd : D[s]? = some sd) (h : HasTy D (.struct s vs) (.struct s)) :
    dropValue D (.struct s vs)
      = .ok ((if sd.dtor then [Event.dtor s (.struct s vs)] else [])
              ++ (vs.map (dropEvents D)).flatten) := by
  rw [dropValue_events h]
  simp only [dropEvents, hd, dropEventsList_eq_flatten]

/-- **§6.11's order, one level.** A struct value's drop emits its user
destructor's event first — when its declaration has one — and then exactly the
events its fields' drops emit, in declaration order. This is the induction
step; `dropValue_struct_events` is the closed form. -/
theorem dropValue_order {D : StructEnv} {s : Nat} {sd : StructDecl} {vs : List Val}
    {evs : List Event} (hd : D[s]? = some sd) (h : dropValue D (.struct s vs) = .ok evs) :
    ∃ fevs, dropValues D vs = .ok fevs ∧
      evs = (if sd.dtor then [Event.dtor s (.struct s vs)] else []) ++ fevs := by
  simp only [dropValue, hd] at h
  cases hf : dropValues D vs with
  | error w => rw [hf] at h; cases h
  | ok fevs => rw [hf] at h; cases h; exact ⟨fevs, rfl, rfl⟩

/-- **`drop*` is the fields in order** (§6.11): the events of a field list's
drop are the head's followed by the tail's. This is the induction step;
`dropValues_events` is the closed form. -/
theorem dropValues_order {D : StructEnv} {v : Val} {vs : List Val} {evs : List Event}
    (h : dropValues D (v :: vs) = .ok evs) :
    ∃ e₁ e₂, dropValue D v = .ok e₁ ∧ dropValues D vs = .ok e₂ ∧ evs = e₁ ++ e₂ := by
  simp only [dropValues] at h
  cases h₁ : dropValue D v with
  | error w => rw [h₁] at h; cases h
  | ok e₁ =>
      rw [h₁] at h
      cases h₂ : dropValues D vs with
      | error w => rw [h₂] at h; cases h
      | ok e₂ => rw [h₂] at h; cases h; exact ⟨e₁, e₂, rfl, rfl, rfl⟩

/-! ## The store–Σ agreement invariant -/

/-- Per-cell agreement between the static entry and the dynamic cell: §7's
"Σ faithfully tracks the store's initialization", with the §5.5 join's
asymmetry built in. A `MovedOut` entry may still hold a live *non-linear*
value, which the machine drops path-specifically at scope exit (§5.6, §6.7;
`3.8:73` is the array-element form of the same rule, cited by §5.5); it
never holds a live linear one (`3.8:50`). -/
def CellMatches (D : StructEnv) (c : Cell) (en : Entry) : Prop :=
  match en.st with
  | .owned => ∃ v, c = .full v ∧ HasTy D v en.ty
  | .movedOut => c = .moved ∨ ∃ v, c = .full v ∧ HasTy D v en.ty ∧ v.mult D ≠ .linear

/-- `Matches Γ ρ H`: each binding's location holds a cell agreeing with its
static entry; locations are live (in `H`) and pairwise distinct. This is the
§7 preservation invariant, over §6.1's environment `ρ` and store `H`. -/
inductive Matches (D : StructEnv) : Ctx → Env → Store → Prop where
  | nil {H} : Matches D [] [] H
  | cons {en : Entry} {Γ : Ctx} {ℓ : Nat} {ρ : Env} {H : Store} {c : Cell} :
      H[ℓ]? = some c → CellMatches D c en → ℓ ∉ ρ → Matches D Γ ρ H →
      Matches D (en :: Γ) (ℓ :: ρ) H

/-- Every bound location is inside the store (helper). -/
theorem Matches.mem_lt {D Γ ρ H} (hm : Matches D Γ ρ H) : ∀ ℓ ∈ ρ, ℓ < H.length := by
  induction hm with
  | nil => intro ℓ h; cases h
  | cons hc _ _ _ ih =>
      intro ℓ' hmem
      cases hmem with
      | head => exact List.getElem?_eq_some_iff.mp hc |>.1
      | tail _ h => exact ih _ h

/-- The next location to allocate is bound to nothing (helper). -/
theorem Matches.fresh_not_mem {D Γ ρ H} (hm : Matches D Γ ρ H) : H.length ∉ ρ := by
  intro hmem
  exact absurd (hm.mem_lt _ hmem) (by omega)

/-- Look a binding up through the invariant (helper). -/
theorem Matches.lookup {D Γ ρ H} (hm : Matches D Γ ρ H) {i : Nat} {en}
    (hget : Γ[i]? = some en) :
    ∃ ℓ c, ρ[i]? = some ℓ ∧ H[ℓ]? = some c ∧ CellMatches D c en := by
  induction hm generalizing i with
  | nil => simp at hget
  | cons hc hcm _ _ ih =>
      cases i with
      | zero => simp_all
      | succ j =>
          simp only [List.getElem?_cons_succ] at hget ⊢
          exact ih hget

/-- Inversion at a non-empty context (helper). -/
theorem Matches.cons_inv {D en Γ ℓ ρ H} (h : Matches D (en :: Γ) (ℓ :: ρ) H) :
    ∃ c, H[ℓ]? = some c ∧ CellMatches D c en ∧ ℓ ∉ ρ ∧ Matches D Γ ρ H := by
  cases h; exact ⟨_, ‹_›, ‹_›, ‹_›, ‹_›⟩

/-- Store growth by allocation preserves the invariant (helper). -/
theorem Matches.append {D Γ ρ H} (hm : Matches D Γ ρ H) (ext : Store) :
    Matches D Γ ρ (H ++ ext) := by
  induction hm with
  | nil => exact .nil
  | cons hc hcm hnin _ ih =>
      refine .cons ?_ hcm hnin ih
      rw [List.getElem?_append_left (List.getElem?_eq_some_iff.mp hc |>.1)]
      exact hc

/-- Writing a cell nobody in `ρ` points at preserves the invariant (helper). -/
theorem Matches.set_outside : ∀ {D Γ ρ H} {ℓ : Nat} {c : Cell},
    Matches D Γ ρ H → ℓ ∉ ρ → Matches D Γ ρ (H.set ℓ c)
  | _, _, _, _, _, _, .nil, _ => .nil
  | _, _, _, _, ℓ, c, .cons (ℓ := ℓ') hc hcm hnin' hrest, hnin => by
      have hne : ℓ ≠ ℓ' := fun h => hnin (by simp [h])
      refine .cons ?_ hcm hnin'
        (Matches.set_outside hrest (fun h => hnin (List.mem_cons_of_mem _ h)))
      rw [List.getElem?_set_ne hne]
      exact hc

/-- Updating binding `i`'s cell together with its entry preserves the
invariant, given the new cell matches the new entry (helper). -/
theorem Matches.set : ∀ {D Γ ρ H} {i ℓ : Nat} {en' : Entry} {c' : Cell},
    Matches D Γ ρ H → ρ[i]? = some ℓ → CellMatches D c' en' →
    Matches D (Γ.set i en') ρ (H.set ℓ c')
  | _, _, _, _, _, _, _, _, .nil, hρ, _ => by simp at hρ
  | _, _, _, _, 0, ℓ, en', c', .cons (ℓ := ℓ') hc hcm hnin hrest, hρ, hc' => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at hρ
      subst hρ
      have hlt : ℓ' < _ := List.getElem?_eq_some_iff.mp hc |>.1
      refine .cons ?_ hc' hnin (hrest.set_outside hnin)
      rw [List.getElem?_set_self hlt]
  | _, _, _, _, (i + 1), ℓ, en', c', .cons (ℓ := ℓ') hc hcm hnin hrest, hρ, hc' => by
      simp only [List.getElem?_cons_succ] at hρ
      have hmem : ℓ ∈ _ := List.mem_of_getElem? hρ
      have hne : ℓ' ≠ ℓ := fun h => hnin (h ▸ hmem)
      simp only [List.set_cons_succ]
      refine .cons ?_ hcm hnin (Matches.set hrest hρ hc')
      rw [List.getElem?_set_ne (fun h => hne h.symm)]
      exact hc

/-- Extending the invariant at the *outermost* end: the shape a parameter list
is built in, since a signature names its parameters left to right while `Ctx`
and `Env` list the innermost binder first (§5.8's (Fn), §6.9's (D-Call))
(helper). -/
theorem Matches.snoc : ∀ {D Γ ρ H} {ℓ : Nat} {en : Entry} {c : Cell},
    Matches D Γ ρ H → H[ℓ]? = some c → CellMatches D c en → ℓ ∉ ρ →
    Matches D (Γ ++ [en]) (ρ ++ [ℓ]) H
  | _, _, _, _, ℓ, en, c, .nil, hc, hcm, _ => by
      simp only [List.nil_append]
      refine .cons hc hcm ?_ .nil
      simp
  | _, _, _, _, ℓ, en, c, .cons (ℓ := ℓ') (ρ := ρ') hc' hcm' hnin' hrest, hc, hcm, hnin => by
      have hne : ℓ ≠ ℓ' := by
        rintro rfl
        exact hnin List.mem_cons_self
      have hnin'' : ℓ' ∉ ρ' ++ [ℓ] := by
        intro hmem
        rcases List.mem_append.mp hmem with h | h
        · exact hnin' h
        · exact hne (List.mem_singleton.mp h).symm
      simp only [List.cons_append]
      exact .cons hc' hcm' hnin''
        (Matches.snoc hrest hc hcm (fun h => hnin (List.mem_cons_of_mem _ h)))

/-! ## Locality: what a frame's evaluation may touch -/

/-- `Untouched ρ H H'`: the store only grew, and every cell that was already
allocated and that `ρ` does not name has the contents it had. This is the
frame-locality property a call needs — a callee's cells are minted above the
caller's whole store (§6.9's (D-Call)), so the caller's bindings are outside
the callee's `ρ` and survive the call untouched. -/
def Untouched (ρ : Env) (H H' : Store) : Prop :=
  H.length ≤ H'.length ∧ ∀ ℓ, ℓ < H.length → ℓ ∉ ρ → H'[ℓ]? = H[ℓ]?

/-- Locality is reflexive (helper). -/
theorem Untouched.refl {ρ : Env} {H : Store} : Untouched ρ H H :=
  ⟨Nat.le_refl _, fun _ _ _ => rfl⟩

/-- Locality composes along a sequence of steps in one frame (helper). -/
theorem Untouched.trans {ρ : Env} {H₁ H₂ H₃ : Store}
    (h₁ : Untouched ρ H₁ H₂) (h₂ : Untouched ρ H₂ H₃) : Untouched ρ H₁ H₃ :=
  ⟨Nat.le_trans h₁.1 h₂.1,
   fun ℓ hlt hnin => (h₂.2 ℓ (Nat.lt_of_lt_of_le hlt h₁.1) hnin).trans (h₁.2 ℓ hlt hnin)⟩

/-- Allocation is local: appending cells touches nothing already there
(helper). -/
theorem Untouched.append {ρ : Env} {H : Store} (ext : Store) : Untouched ρ H (H ++ ext) :=
  ⟨by simp, fun ℓ hlt _ => List.getElem?_append_left hlt⟩

/-- Writing a cell the frame names is local (helper). -/
theorem Untouched.set {ρ : Env} {H : Store} {ℓ : Nat} {c : Cell} (h : ℓ ∈ ρ) :
    Untouched ρ H (H.set ℓ c) := by
  refine ⟨by simp, fun ℓ' _ hnin => ?_⟩
  have hne : ℓ ≠ ℓ' := by rintro rfl; exact hnin h
  exact List.getElem?_set_ne hne

/-- Locality extends across a write to a cell the frame names, or to one
minted above the store locality is measured from (helper). -/
theorem Untouched.trans_set {ρ : Env} {H₀ H : Store} {ℓ : Nat} {c : Cell}
    (hu : Untouched ρ H₀ H) (h : H₀.length ≤ ℓ ∨ ℓ ∈ ρ) :
    Untouched ρ H₀ (H.set ℓ c) := by
  refine ⟨by simpa using hu.1, fun ℓ' hlt hnin => ?_⟩
  have hne : ℓ ≠ ℓ' := by
    rcases h with h | h
    · omega
    · rintro rfl; exact hnin h
  rw [List.getElem?_set_ne hne]
  exact hu.2 ℓ' hlt hnin

/-- A step that only touches cells the *inner* frame names, all of them minted
above the outer store, is local for the outer frame too: the shape both a
`let` body and a callee's frame take (helper). -/
theorem Untouched.of_fresh {ρ ρ' : Env} {H Hm H' : Store}
    (hpre : H.length ≤ Hm.length)
    (hfresh : ∀ ℓ ∈ ρ', H.length ≤ ℓ)
    (hkeep : ∀ ℓ, ℓ < H.length → Hm[ℓ]? = H[ℓ]?)
    (h : Untouched ρ' Hm H') : Untouched ρ H H' := by
  refine ⟨Nat.le_trans hpre h.1, fun ℓ hlt _ => ?_⟩
  have hnin : ℓ ∉ ρ' := fun hmem => absurd (hfresh ℓ hmem) (by omega)
  rw [h.2 ℓ (Nat.lt_of_lt_of_le hlt hpre) hnin]
  exact hkeep ℓ hlt

/-- A `let` body runs in a frame with one more binding, minted above the whole
store; what it does is local to the enclosing frame too (helper). -/
theorem Untouched.under_binder {ρ : Env} {H₁ H₂ : Store} {c : Cell}
    (h : Untouched (H₁.length :: ρ) (H₁ ++ [c]) H₂) : Untouched ρ H₁ H₂ := by
  have hlen := h.1
  simp only [List.length_append, List.length_cons, List.length_nil] at hlen
  refine ⟨by omega, fun ℓ hlt hnin => ?_⟩
  have hne : ℓ ∉ H₁.length :: ρ := by
    simp only [List.mem_cons, not_or]
    exact ⟨by omega, hnin⟩
  rw [h.2 ℓ (by simp; omega) hne]
  exact List.getElem?_append_left hlt

/-- The invariant of a frame transports across a step local to a *disjoint*
frame: the caller's bindings survive a callee's run (helper). -/
theorem Matches.transport {D Γ ρ₀ ρ H H'} (hm : Matches D Γ ρ₀ H)
    (hdisj : ∀ ℓ ∈ ρ₀, ℓ ∉ ρ) (hu : Untouched ρ H H') : Matches D Γ ρ₀ H' := by
  induction hm with
  | nil => exact .nil
  | cons hc hcm hnin hrest ih =>
      refine .cons ?_ hcm hnin (ih (fun ℓ hmem => hdisj ℓ (List.mem_cons_of_mem _ hmem)) hu)
      rw [hu.2 _ (List.getElem?_eq_some_iff.mp hc |>.1) (hdisj _ (by simp))]
      exact hc

/-! ## Frames: the environment and the scope record together -/

/-- The per-frame invariant (§6.1): the fused context agrees with the store
through the frame's environment, and the frame's scope record, read
newest-first, **is** that environment. The second clause is the RUE-1277
redundancy discharged — every live binding of the frame is registered for a
drop exactly once, which is what makes `run-all-scope-drops` (§6.9) safe at an
early `return`. -/
structure FrameMatches (D : StructEnv) (Γ : Ctx) (φ : Frame) (H : Store) : Prop where
  /-- `Matches` through the frame's environment `ρ`. -/
  store : Matches D Γ φ.env H
  /-- The scope record, newest-first, is the environment (`3.8:62`: every
  by-value binding is registered, and only those). In this fragment both are
  built from one list at every frame, so the equation holds definitionally;
  it becomes a real obligation when `Frame.scope` is §6.1's stack (§6.6,
  §6.10). -/
  record : φ.scope.reverse = φ.env

/-! ## Scope teardown never refuses -/

/-- A cell matching an entry that is not an owned linear one is a cell
`drop-retire` can retire: either moved out, or holding a non-linear value
(helper). -/
theorem CellMatches.dropOk {D c en} (hcm : CellMatches D c en)
    (h : ¬(en.st = .owned ∧ en.ty.mult D = .linear)) :
    c = .moved ∨ ∃ v T, c = .full v ∧ HasTy D v T ∧ v.mult D ≠ .linear := by
  unfold CellMatches at hcm
  cases hst : en.st with
  | owned =>
      rw [hst] at hcm
      obtain ⟨v, rfl, hv⟩ := hcm
      exact Or.inr ⟨v, en.ty, rfl, hv, by rw [hv.mult_eq]; exact fun hl => h ⟨hst, hl⟩⟩
  | movedOut =>
      rw [hst] at hcm
      rcases hcm with rfl | ⟨v, rfl, hv, hnl⟩
      · exact Or.inl rfl
      · exact Or.inr ⟨v, en.ty, rfl, hv, hnl⟩

/-- The drop of a well-typed value in a cell always runs (§6.11) (helper). -/
theorem dropCell_ok {D : StructEnv} {ℓ : Nat} {v : Val} {T : Ty} (h : HasTy D v T) :
    ∃ evs, dropCell D ℓ v = .ok evs := by
  unfold dropCell
  by_cases hcp : v.mult D = .copy
  · exact ⟨[], by simp [hcp]⟩
  · obtain ⟨evs, hevs⟩ := dropValue_ok h
    exact ⟨.drop ℓ v :: evs, by simp [hcp, hevs]⟩

/-- `drop-retire` (§6.1) succeeds on such a cell, retiring it: the value's own
drop (§6.11) runs — `dropValue_ok` is why it never refuses — and the leak
monitor lets it through because the value's class is not `Linear` (helper). -/
theorem dropRetire_ok {D : StructEnv} {H : Store} {ℓ : Nat} {c : Cell} (hc : H[ℓ]? = some c)
    (h : c = .moved ∨ ∃ v T, c = .full v ∧ HasTy D v T ∧ v.mult D ≠ .linear) :
    ∃ evs, dropRetire D H ℓ = .ok (H.set ℓ .dead, evs) := by
  unfold dropRetire
  rw [hc]
  rcases h with rfl | ⟨v, T, rfl, hv, hnl⟩
  · exact ⟨[], rfl⟩
  · obtain ⟨evs, hdc⟩ := dropCell_ok (ℓ := ℓ) hv
    exact ⟨evs, by simp only [if_neg hnl, hdc]⟩

/-- **Scope teardown never refuses on a frame the statics cleared.**
`run-scope-drops` (§6.1) over a frame whose bindings carry no residual linear
value retires every one of them: none is already retired (`Matches` says every
bound cell is live or moved out and that no two bindings share one — §7's
no-use-after-drop at an unwinding edge), and none is a live linear value (the
§5.6 obligation). -/
theorem Matches.unwind {D : StructEnv} : ∀ (Γ : Ctx) (ρ : Env) (H : Store), Matches D Γ ρ H →
    NoOwnedLinear D Γ →
    ∃ H' evs, unwindLocs D H ρ = .ok (H', evs) ∧ H'.length = H.length ∧
      ∀ ℓ, ℓ ∉ ρ → H'[ℓ]? = H[ℓ]?
  | [], _, H, hm, _ => by
      cases hm
      exact ⟨H, [], rfl, rfl, fun _ _ => rfl⟩
  | en :: Γ₀, _, H, hm, hnl => by
      cases hm with
      | @cons _ _ ℓ ρ₀ _ c hc hcm hnin hrest =>
        have hhead : ¬(en.st = .owned ∧ en.ty.mult D = .linear) := hnl en (by simp)
        have hnl₀ : NoOwnedLinear D Γ₀ := fun e he => hnl e (List.mem_cons_of_mem _ he)
        obtain ⟨evs₀, hdr⟩ := dropRetire_ok hc (hcm.dropOk hhead)
        obtain ⟨H', evs', hrec, hlen, hout⟩ :=
          Matches.unwind Γ₀ ρ₀ (H.set ℓ .dead) (hrest.set_outside hnin) hnl₀
        refine ⟨H', evs₀ ++ evs', ?_, ?_, ?_⟩
        · simp only [unwindLocs, hdr, hrec]
        · rw [hlen]; simp
        · intro ℓ' hnin'
          have hne : ℓ' ≠ ℓ := fun h => hnin' (by simp [h])
          rw [hout ℓ' (fun h => hnin' (List.mem_cons_of_mem _ h))]
          exact List.getElem?_set_ne (fun h => hne h.symm)

/-- **A frame's whole teardown never refuses** (§6.9's `run-all-scope-drops`,
run at (D-Return-Value) and at (D-Return)). The record is the environment
reversed, so this is `Matches.unwind` read newest-first. -/
theorem runAllScopeDrops_ok {D Γ φ H} (hfm : FrameMatches D Γ φ H) (hnl : NoOwnedLinear D Γ) :
    ∃ H' evs, runAllScopeDrops D H φ = .ok (H', evs) ∧ H'.length = H.length ∧
      ∀ ℓ, ℓ ∉ φ.env → H'[ℓ]? = H[ℓ]? := by
  unfold runAllScopeDrops
  rw [hfm.record]
  exact Matches.unwind _ _ _ hfm.store hnl

/-! ## Skeleton transport and join weakening -/

/-- Two contexts with one skeleton agree on every entry's type and mark
(helper). -/
theorem skel_lookup {Γ Γ' : Ctx} (h : Ctx.skel Γ' = Ctx.skel Γ) {i : Nat} {en en'}
    (h1 : Γ[i]? = some en) (h2 : Γ'[i]? = some en') :
    en'.ty = en.ty ∧ en'.mu = en.mu := by
  have hm : (Ctx.skel Γ')[i]? = (Ctx.skel Γ)[i]? := by rw [h]
  simp only [Ctx.skel, List.getElem?_map, h1, h2, Option.map_some,
    Option.some_inj] at hm
  exact ⟨congrArg Prod.fst hm, congrArg Prod.snd hm⟩

/-- The §5.5 join weakens the left arm's per-cell agreement: a cell matching
the left entry matches the joined entry (the conservative join; its
array-element form is `3.8:73`). -/
theorem Entry.join_matches_left {D : StructEnv} {a b e' : Entry} (hj : a.join D b = some e')
    {c : Cell} (hc : CellMatches D c a) : CellMatches D c e' := by
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

/-- The §5.5 join weakens the right arm's per-cell agreement, given the two
arms share a skeleton (the conservative join; its array-element form is
`3.8:73`). -/
theorem Entry.join_matches_right {D : StructEnv} {a b e' : Entry} (hskel : a.skel = b.skel)
    (hj : a.join D b = some e') {c : Cell} (hc : CellMatches D c b) : CellMatches D c e' := by
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

/-- The invariant survives the §5.5 join from the left arm. -/
theorem Matches.join_left {D : StructEnv} : ∀ {Γ₁ Γ₂ Γ' : Ctx} {ρ H},
    Ctx.join D Γ₁ Γ₂ = some Γ' → Matches D Γ₁ ρ H → Matches D Γ' ρ H := by
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

/-- The invariant survives the §5.5 join from the right arm. -/
theorem Matches.join_right {D : StructEnv} : ∀ {Γ₁ Γ₂ Γ' : Ctx} {ρ H},
    Ctx.skel Γ₁ = Ctx.skel Γ₂ →
    Ctx.join D Γ₁ Γ₂ = some Γ' → Matches D Γ₂ ρ H → Matches D Γ' ρ H := by
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

/-! ## Minting a callee's parameters (§6.9's (D-Call)) -/

/-- Minting appends one cell per by-value argument and nothing else
(helper). -/
theorem mintParams_store : ∀ (H : Store) (vs : List Val),
    (mintParams H vs).1 = H ++ vs.map Cell.full
  | H, [] => by simp [mintParams]
  | H, v :: vs => by
      simp [mintParams, mintParams_store (H ++ [Cell.full v]) vs]

/-- Every parameter cell is minted above the caller's whole store, which is
what makes a call local to the caller's frame (helper). -/
theorem mintParams_fresh : ∀ (H : Store) (vs : List Val) (ℓ : Nat),
    ℓ ∈ (mintParams H vs).2 → H.length ≤ ℓ
  | _, [], _, h => by simp [mintParams] at h
  | H, v :: vs, ℓ, h => by
      simp only [mintParams] at h
      rcases List.mem_cons.mp h with rfl | h
      · exact Nat.le_refl _
      · have := mintParams_fresh (H ++ [Cell.full v]) vs ℓ h
        simp at this
        omega

/-- (D-Call) §6.9 establishes the callee's entry invariant: the parameter
cells hold the argument values, and (Fn) §5.8's entry context `Γ0;Σ0`
(`fnCtx`) describes exactly them. The two `reverse`s are the same one: a
signature lists parameters left to right while `Ctx` and `Env` list the
innermost binder first. -/
theorem matches_mintParams {D : StructEnv} : ∀ (ps : List Param) (vs : List Val) (H : Store),
    HasTys D vs (ps.map Param.ty) →
    Matches D ((ps.map fun p => ({ ty := p.ty, mu := p.mu, st := .owned } : Entry)).reverse)
      (mintParams H vs).2.reverse (mintParams H vs).1
  | [], vs, H, h => by cases h; exact .nil
  | p :: ps, vs, H, h => by
      cases h with
      | @cons v vs' _ _ hv hvs =>
          have ih := matches_mintParams (D := D) ps vs' (H ++ [Cell.full v]) hvs
          simp only [List.map_cons, List.reverse_cons, mintParams]
          refine Matches.snoc ih ?_ ⟨v, rfl, hv⟩ ?_
          · rw [mintParams_store]
            rw [List.getElem?_append_left (by simp)]
            simp
          · intro hmem
            have := mintParams_fresh (H ++ [Cell.full v]) vs' _ (List.mem_reverse.mp hmem)
            simp at this
            omega

/-! ## What soundness promises about one evaluation -/

/-- The promise for an evaluation that does **not** produce a value here: an
unwinding `return` carries a value of the enclosing function's declared return
type `R` and leaves the frame's neighbours alone; a trap and exhausted fuel
promise nothing; a refusal is impossible, which is the whole theorem
(helper). -/
def AbortOk (D : StructEnv) (R : Ty) (φ : Frame) (H : Store) : EvalRes → Prop
  | .ok _ _ _ => False
  | .returned H' v _ => HasTy D v R ∧ Untouched φ.env H H'
  | .panic _ _ => True
  | .stuck _ => False
  | .outOfFuel => True

/-- The promise `soundness` makes about `eval`'s result: a value of the
expression's type with the outgoing context's invariant restored
(preservation), or one of `AbortOk`'s outcomes — never `.stuck` (progress).
Stating it as a predicate on the result, rather than as a disjunction of
existentials, is what lets the operand combinators (`andThen`) be discharged
once and reused at every form (helper). -/
def EvalOk (D : StructEnv) (T R : Ty) (Γ' : Ctx) (φ : Frame) (H : Store) : EvalRes → Prop
  | .ok H' v _ => HasTy D v T ∧ FrameMatches D Γ' φ H' ∧ Untouched φ.env H H'
  | .returned H' v _ => HasTy D v R ∧ Untouched φ.env H H'
  | .panic _ _ => True
  | .stuck _ => False
  | .outOfFuel => True

/-- The promise for an argument list (§5.8's (Call), left to right with Σ
threaded) (helper). -/
def ArgsOk (D : StructEnv) (R : Ty) (Ts : List Ty) (Γ' : Ctx) (φ : Frame) (H : Store) :
    ArgsRes → Prop
  | .ok H' vs _ => HasTys D vs Ts ∧ FrameMatches D Γ' φ H' ∧ Untouched φ.env H H'
  | .abort r => AbortOk D R φ H r

/-- A promise made from a later store is a promise from an earlier one, given
the step between them was local to the frame (helper). -/
theorem EvalOk.mono_store {D T R Γ' φ H H₁ r} (hu : Untouched φ.env H H₁)
    (h : EvalOk D T R Γ' φ H₁ r) : EvalOk D T R Γ' φ H r := by
  cases r with
  | ok H' v tr => exact ⟨h.1, h.2.1, hu.trans h.2.2⟩
  | returned H' v tr => exact ⟨h.1, hu.trans h.2⟩
  | panic k tr => trivial
  | stuck w => exact h.elim
  | outOfFuel => trivial

/-- The same, for a result that is not a value (helper). -/
theorem AbortOk.mono_store {D R φ H H₁ r} (hu : Untouched φ.env H H₁)
    (h : AbortOk D R φ H₁ r) : AbortOk D R φ H r := by
  cases r with
  | ok H' v tr => exact h.elim
  | returned H' v tr => exact ⟨h.1, hu.trans h.2⟩
  | panic k tr => trivial
  | stuck w => exact h.elim
  | outOfFuel => trivial

/-- Prefixing a trace changes no promise: the trace is an observation, not a
state (helper). -/
theorem EvalOk.withTrace {D T R Γ' φ H r} (h : EvalOk D T R Γ' φ H r) (tr : List Event) :
    EvalOk D T R Γ' φ H (r.withTrace tr) := by
  cases r <;> simp_all [EvalRes.withTrace, EvalOk]

/-- The same, for a result that is not a value (helper). -/
theorem AbortOk.withTrace {D R φ H r} (h : AbortOk D R φ H r) (tr : List Event) :
    AbortOk D R φ H (r.withTrace tr) := by
  cases r <;> simp_all [EvalRes.withTrace, AbortOk]

/-- A result that is not a value satisfies the full promise, whatever type and
outgoing context the form claims — the promise is only about values there
(helper). -/
theorem EvalOk.of_abort {D T R Γ' φ H r} (h : AbortOk D R φ H r) : EvalOk D T R Γ' φ H r := by
  cases r <;> simp_all [AbortOk, EvalOk]

/-- An evaluation that produced no value only ever produced an `AbortOk`
outcome (helper). -/
theorem EvalOk.toAbort {D T R Γ' φ H r} (h : EvalOk D T R Γ' φ H r)
    (hne : ∀ H' v tr, r ≠ .ok H' v tr) : AbortOk D R φ H r := by
  cases r with
  | ok H' v tr => exact absurd rfl (hne H' v tr)
  | returned H' v tr => exact h
  | panic k tr => trivial
  | stuck w => exact h.elim
  | outOfFuel => trivial

/-- **§6.2's search, once and for all.** An operand that promised its own
outcome, sequenced into a context that promises the form's outcome from the
operand's value, promises the form's outcome. Every operand of every form is
discharged by this lemma (helper). -/
theorem EvalOk.bind {D : StructEnv} {T T₀ R : Ty} {Γ' Γ₀ : Ctx} {φ : Frame} {H : Store}
    {r : EvalRes} {k : Store → Val → EvalRes}
    (hr : EvalOk D T₀ R Γ₀ φ H r)
    (hk : ∀ H₁ v tr, r = .ok H₁ v tr → HasTy D v T₀ → FrameMatches D Γ₀ φ H₁ →
            EvalOk D T R Γ' φ H₁ (k H₁ v)) :
    EvalOk D T R Γ' φ H (r.andThen k) := by
  cases r with
  | ok H₁ v tr =>
      obtain ⟨hty, hfm, hu⟩ := hr
      exact (((hk H₁ v tr rfl hty hfm).mono_store hu).withTrace tr)
  | returned H₁ v tr => exact hr
  | panic k tr => trivial
  | stuck w => exact hr.elim
  | outOfFuel => trivial

/-! ## The main theorem -/

/-- Weakening the outgoing context of a promise, which is what §5.5's join
asks of an arm (helper). -/
theorem EvalOk.weaken {D T R Γ₁ Γ' φ H r}
    (hw : ∀ H', FrameMatches D Γ₁ φ H' → FrameMatches D Γ' φ H')
    (h : EvalOk D T R Γ₁ φ H r) : EvalOk D T R Γ' φ H r := by
  cases r with
  | ok H' v tr => exact ⟨h.1, hw H' h.2.1, h.2.2⟩
  | returned H' v tr => exact h
  | panic k tr => trivial
  | stuck w => exact h.elim
  | outOfFuel => trivial

/-- **The argument list of a call is safe** (§5.8's (Call), §6.9's (D-Call)):
evaluated left to right with Σ threaded, it produces one well-typed value per
parameter with the invariant carried to the last argument's outgoing context,
or hands on the first argument's non-value outcome — an unwinding `return`
among them, which aborts the call before any parameter cell is minted. The
hypothesis is `soundness` at the fuel the call has already spent one unit of,
which is why this is a lemma rather than a case of the induction. -/
theorem args_sound {P : Program} {fuel : Nat}
    (ih : ∀ {R : Ty} {Γ Γ' : Ctx} {e : Expr} {T : Ty}, Typed P R Γ e T Γ' →
      ∀ {φ : Frame} {H : Store}, FrameMatches P.structs Γ φ H →
        EvalOk P.structs T R Γ' φ H (eval fuel P H φ e)) :
    ∀ (es : List Expr) {R : Ty} {Γ Γ' : Ctx} {Ts : List Ty} {φ : Frame} {H : Store},
      TypedArgs P R Γ es Ts Γ' → FrameMatches P.structs Γ φ H →
        ArgsOk P.structs R Ts Γ' φ H (evalArgs (fun H' e => eval fuel P H' φ e) H es) := by
  intro es
  induction es with
  | nil =>
      intro R Γ Γ' Ts φ H hta hfm
      cases hta
      exact ⟨.nil, hfm, Untouched.refl⟩
  | cons e es ihes =>
      intro R Γ Γ' Ts φ H hta hfm
      cases hta with
      | @cons _ Γ₁ _ _ _ T Ts' h₁ h₂ =>
        have k₁ := ih h₁ hfm
        simp only [evalArgs]
        cases hr : eval fuel P H φ e with
        | ok H₁ v tr =>
            rw [hr] at k₁
            obtain ⟨hty, hfm₁, hu₁⟩ := k₁
            have k₂ := ihes h₂ hfm₁
            dsimp only
            cases hr₂ : evalArgs (fun H' e => eval fuel P H' φ e) H₁ es with
            | ok H₂ vs tr₂ =>
                rw [hr₂] at k₂
                dsimp only
                obtain ⟨hvs, hfm₂, hu₂⟩ := k₂
                exact ⟨.cons hty hvs, hfm₂, hu₁.trans hu₂⟩
            | abort r =>
                rw [hr₂] at k₂
                dsimp only
                exact (k₂.mono_store hu₁).withTrace tr
        | returned H₁ v tr =>
            rw [hr] at k₁
            try dsimp only
            exact k₁
        | panic pk tr => try dsimp only; trivial
        | stuck w => rw [hr] at k₁; exact k₁.elim
        | outOfFuel => try dsimp only; trivial

/-- **Type safety for the fragment** (§7, first bullet, in
definitional-interpreter form).

A well-typed expression, run at any fuel in any frame and store agreeing with
its incoming context, yields a well-typed value with the outgoing context's
agreement restored, a value handed back by an unwinding `return` (§6.9), a
*defined* panic (§6.12), or `outOfFuel` — never `.stuck`, so never a
`Violation`: no use-after-move, no use-after-drop, no linear leak, no linear
overwrite, no linear discard (§7's decomposed bullets). The theorem is
quantified over every fuel, and `fuel_mono`/`no_masking` below are what say
that quantification is not vacuous.

The induction is on the fuel, not on the derivation: a callee's body is not a
subexpression of the call, so recursion is what the fuel is there to bound,
and every subexpression — a call's arguments and the callee's body alike —
runs at one unit less. -/
theorem soundness {P : Program} (hwf : WfProgram P) :
    ∀ (fuel : Nat) {R : Ty} {Γ Γ' : Ctx} {e : Expr} {T : Ty}, Typed P R Γ e T Γ' →
      ∀ {φ : Frame} {H : Store}, FrameMatches P.structs Γ φ H →
        EvalOk P.structs T R Γ' φ H (eval fuel P H φ e) := by
  intro fuel
  induction fuel with
  | zero =>
      intro R Γ Γ' e T ht φ H hfm
      simp only [eval]
      trivial
  | succ fuel ih =>
      have hargs := args_sound ih
      intro R Γ Γ' e T ht φ H hfm
      cases ht with
      | @intLit Γ w sg n hb =>
          simp only [eval]
          exact ⟨.int hb, hfm, Untouched.refl⟩
      | @boolLit Γ b =>
          simp only [eval]
          exact ⟨.bool, hfm, Untouched.refl⟩
      | @unitLit Γ =>
          simp only [eval]
          exact ⟨.unit, hfm, Untouched.refl⟩
      | @useCopy Γ i en hget hst hcopy =>
          obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hfm.store.lookup hget
          unfold CellMatches at hcm
          rw [hst] at hcm
          obtain ⟨v, rfl, hv⟩ := hcm
          have hvm : v.mult P.structs = .copy := by rw [hv.mult_eq]; exact hcopy
          have hev : eval (fuel + 1) P H φ (.use i) = .ok H v [] := by
            simp [eval, hρ, hc, hvm]
          rw [hev]
          exact ⟨hv, hfm, Untouched.refl⟩
      | @useMove Γ i en hget hst hncopy =>
          obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hfm.store.lookup hget
          unfold CellMatches at hcm
          rw [hst] at hcm
          obtain ⟨v, rfl, hv⟩ := hcm
          have hvm : v.mult P.structs ≠ .copy := by rw [hv.mult_eq]; exact hncopy
          have hev : eval (fuel + 1) P H φ (.use i) = .ok (H.set ℓ .moved) v [] := by
            simp [eval, hρ, hc, hvm]
          rw [hev]
          exact ⟨hv, ⟨hfm.store.set hρ (Or.inl rfl), hfm.record⟩,
            Untouched.trans_set Untouched.refl (Or.inr (List.mem_of_getElem? hρ))⟩
      | @binop Γ Γ₁ Γ₂ op e₁ e₂ w sg h₁ h₂ =>
          simp only [eval]
          refine EvalOk.bind (ih h₁ hfm) ?_
          intro H₁ v₁ tr₁ _ hty₁ hfm₁
          obtain ⟨n₁, rfl, _⟩ := hty₁.int_inv
          refine EvalOk.bind (ih h₂ hfm₁) ?_
          intro H₂ v₂ tr₂ _ hty₂ hfm₂
          obtain ⟨n₂, rfl, _⟩ := hty₂.int_inv
          rcases evalBinOp_res (D := P.structs) op w sg n₁ n₂ with ⟨v, hv, hty⟩ | ⟨k, hk⟩
          · rw [hv]; exact ⟨hty, hfm₂, Untouched.refl⟩
          · rw [hk]; trivial
      | @neg Γ Γ' e w h =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          obtain ⟨n, rfl, _⟩ := hty.int_inv
          rcases evalUnOp_int_res (D := P.structs) .neg w .signed n (by simp) with
            ⟨v', hv, hty'⟩ | hk
          · rw [hv]; exact ⟨hty', hfm', Untouched.refl⟩
          · rw [hk]; trivial
      | @notOp Γ Γ' e h =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          obtain ⟨b, rfl⟩ := hty.bool_inv
          obtain ⟨v', hv, hty'⟩ := evalUnOp_bool_res (D := P.structs) b
          rw [hv]
          exact ⟨hty', hfm', Untouched.refl⟩
      | @bitnot Γ Γ' e w sg h =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          obtain ⟨n, rfl, _⟩ := hty.int_inv
          rcases evalUnOp_int_res (D := P.structs) .bitnot w sg n (by simp) with
            ⟨v', hv, hty'⟩ | hk
          · rw [hv]; exact ⟨hty', hfm', Untouched.refl⟩
          · rw [hk]; trivial
      | @intCast Γ Γ' w sg w' s' e h =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          obtain ⟨n, rfl, _⟩ := hty.int_inv
          rcases evalIntCast_res (D := P.structs) w sg w' s' n with ⟨v', hv, hty'⟩ | hk
          · rw [hv]; exact ⟨hty', hfm', Untouched.refl⟩
          · rw [hk]; trivial
      | @panic Γ Γ'' T msg hskel =>
          -- (D-Panic) §6.12 abandons the configuration, which is a defined
          -- trap and so one of `EvalOk`'s permitted outcomes; the rule's
          -- arbitrary type and outgoing context are never read.
          simp only [eval]
          trivial
      | @dbg Γ Γ' e T h hobs =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          exact ⟨.unit, hfm', Untouched.refl⟩
      | @mkStruct Γ Γ' s args sd hget hta =>
          -- (D-Struct) §6.5 over §6.2's left-to-right search: the same
          -- argument-list lemma (Call) §5.8 uses, then the literal.
          simp only [eval]
          have ka := hargs args hta hfm
          cases hra : evalArgs (fun H' e => eval fuel P H' φ e) H args with
          | abort r =>
              rw [hra] at ka
              dsimp only
              exact EvalOk.of_abort ka
          | ok H₁ vs tr =>
              rw [hra] at ka
              obtain ⟨hvs, hfm₁, hu₁⟩ := ka
              have hlen : sd.fields.length = vs.length := hvs.length_eq.symm
              simp only [hget]
              rw [if_pos hlen]
              exact ⟨.struct hget hvs, hfm₁, hu₁⟩
      | @consume Γ Γ' s sd e h hget hcons =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          obtain ⟨sd', vs, rfl, hget', hvs⟩ := hty.struct_inv
          have hsd : sd' = sd := by rw [hget'] at hget; cases hget; rfl
          subst hsd
          obtain ⟨w, sg, n, vs', rfl, hbn, hhead⟩ := hvs.head_int hcons.1 hcons.2.1
          dsimp only
          rw [StructDecl.payloadTy_of_head hhead]
          exact ⟨.int hbn, hfm', Untouched.refl⟩
      | @dropCopy Γ i en hget hst hcopy =>
          obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hfm.store.lookup hget
          unfold CellMatches at hcm
          rw [hst] at hcm
          obtain ⟨v, rfl, hv⟩ := hcm
          have hvm : v.mult P.structs = .copy := by rw [hv.mult_eq]; exact hcopy
          have hev : eval (fuel + 1) P H φ (.drop i) = .ok H .unit [] := by
            simp [eval, hρ, hc, hvm, dropCell]
          rw [hev]
          exact ⟨.unit, hfm, Untouched.refl⟩
      | @dropRes Γ i en hget hst hncopy =>
          obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hfm.store.lookup hget
          unfold CellMatches at hcm
          rw [hst] at hcm
          obtain ⟨v, rfl, hv⟩ := hcm
          have hvm : v.mult P.structs ≠ .copy := by rw [hv.mult_eq]; exact hncopy
          obtain ⟨evs, hevs⟩ := dropValue_ok hv
          have hev : eval (fuel + 1) P H φ (.drop i)
              = .ok (H.set ℓ .moved) .unit (.drop ℓ v :: evs) := by
            simp [eval, hρ, hc, hvm, dropCell, hevs]
          rw [hev]
          exact ⟨.unit, ⟨hfm.store.set hρ (Or.inl rfl), hfm.record⟩,
            Untouched.trans_set Untouched.refl (Or.inr (List.mem_of_getElem? hρ))⟩
      | @letIn Γ Γ₁ Γ₂ m e₁ e₂ T₁ T₂ en' h₁ h₂ hres =>
          simp only [eval]
          refine EvalOk.bind (ih h₁ hfm) ?_
          intro H₁ v₁ tr₁ _ hty₁ hfm₁
          have hfresh : H₁.length ∉ φ.env := hfm₁.store.fresh_not_mem
          have hfm' : FrameMatches P.structs ({ ty := T₁, mu := m, st := .owned } :: Γ₁)
              { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] }
              (H₁ ++ [.full v₁]) := by
            constructor
            · refine .cons ?_ ⟨v₁, rfl, hty₁⟩ hfresh (hfm₁.store.append _)
              simp
            · simp [hfm₁.record]
          have kb := ih h₂ hfm'
          have hty_en' : en'.ty = T₁ := by
            have hskel := h₂.skel_preserved
            simp only [Ctx.skel, List.map_cons, List.cons.injEq] at hskel
            exact congrArg Prod.fst hskel.1
          cases hrb : eval fuel P (H₁ ++ [.full v₁])
              { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] } e₂ with
          | ok H₂ v₂ tr₂ =>
              rw [hrb] at kb
              obtain ⟨hty₂, hfm₂, hu₂⟩ := kb
              obtain ⟨c, hc, hcm, hnin, hrest⟩ := hfm₂.store.cons_inv
              have hdrop : ¬(en'.st = .owned ∧ en'.ty.mult P.structs = .linear) := by
                rw [hty_en']; exact hres
              obtain ⟨evs, hdr⟩ := dropRetire_ok hc (hcm.dropOk hdrop)
              simp only [EvalRes.andThen, hdr, EvalRes.withTrace]
              exact ⟨hty₂, ⟨hrest.set_outside hnin, hfm.record⟩,
                Untouched.trans_set hu₂.under_binder (Or.inl (Nat.le_refl _))⟩
          | returned H₂ v₂ tr₂ =>
              rw [hrb] at kb
              simp only [EvalRes.andThen]
              exact ⟨kb.1, kb.2.under_binder⟩
          | panic pk tr => simp only [EvalRes.andThen]; trivial
          | stuck w => rw [hrb] at kb; exact kb.elim
          | outOfFuel => simp only [EvalRes.andThen]; trivial
      | @assign Γ Γ₁ i e en₀ en₁ hget₀ hmut h hget₁ hpre =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H₁ v tr _ hty hfm₁
          obtain ⟨ℓ, c, hρ, hc, hcm⟩ := hfm₁.store.lookup hget₁
          have hskel := h.skel_preserved
          have htyeq : en₁.ty = en₀.ty := (skel_lookup hskel hget₀ hget₁).1
          have hty' : HasTy P.structs v en₁.ty := htyeq ▸ hty
          have hnewcm : CellMatches P.structs (.full v) (en₁.setSt .owned) := ⟨v, rfl, hty'⟩
          have hmem : ℓ ∈ φ.env := List.mem_of_getElem? hρ
          have hres : ∀ (evs : List Event),
              EvalOk P.structs .unit R (Γ₁.set i (en₁.setSt .owned)) φ H₁
                (EvalRes.ok (H₁.set ℓ (.full v)) .unit evs) :=
            fun evs => ⟨.unit, ⟨hfm₁.store.set hρ hnewcm, hfm₁.record⟩,
              Untouched.trans_set Untouched.refl (Or.inr hmem)⟩
          have hnotlin : ¬(en₁.st = .owned ∧ en₁.ty.mult P.structs = .linear) := by
            rintro ⟨hown, hlin⟩
            rcases hpre with h' | h'
            · rw [hown] at h'; cases h'
            · exact h' (htyeq ▸ hlin)
          rcases hcm.dropOk hnotlin with rfl | ⟨vOld, T', rfl, hvOld, hnl⟩
          · simp only [hρ, hc]
            exact hres []
          · obtain ⟨evs, hdc⟩ := dropCell_ok (ℓ := ℓ) hvOld
            simp only [hρ, hc, if_neg hnl, hdc]
            exact hres evs
      | @seq Γ Γ₁ Γ₂ e₁ e₂ T₁ T₂ h₁ hnl h₂ =>
          simp only [eval]
          refine EvalOk.bind (ih h₁ hfm) ?_
          intro H₁ v₁ tr₁ _ hty₁ hfm₁
          have hvnl : v₁.mult P.structs ≠ .linear := by rw [hty₁.mult_eq]; exact hnl
          cases hml : v₁.mult P.structs with
          | linear => exact absurd hml hvnl
          | affine =>
              obtain ⟨evs, hevs⟩ := dropValue_ok hty₁
              simp only [hevs]
              exact (ih h₂ hfm₁).withTrace _
          | copy => exact ih h₂ hfm₁
      | @ite Γ Γ₀ Γ₁ Γ₂ Γ' c e₁ e₂ T hc h₁ h₂ hjoin =>
          simp only [eval]
          refine EvalOk.bind (ih hc hfm) ?_
          intro H₀ v₀ tr₀ _ hty₀ hfm₀
          obtain ⟨b, rfl⟩ := hty₀.bool_inv
          have hskel12 : Ctx.skel Γ₁ = Ctx.skel Γ₂ :=
            h₁.skel_preserved.trans h₂.skel_preserved.symm
          cases b with
          | true =>
              dsimp only
              exact EvalOk.weaken
                (fun H' hf => ⟨Matches.join_left hjoin hf.store, hf.record⟩) (ih h₁ hfm₀)
          | false =>
              dsimp only
              exact EvalOk.weaken
                (fun H' hf => ⟨Matches.join_right hskel12 hjoin hf.store, hf.record⟩)
                (ih h₂ hfm₀)
      | @call Γ Γ' f args fd hget hta =>
          simp only [eval]
          have ka := hargs args hta hfm
          cases hra : evalArgs (fun H' e => eval fuel P H' φ e) H args with
          | abort r =>
              rw [hra] at ka
              dsimp only
              exact EvalOk.of_abort ka
          | ok H₁ vs tr =>
              rw [hra] at ka
              obtain ⟨hvs, hfm₁, hu₁⟩ := ka
              have hlen : fd.params.length = vs.length := by
                have hl := hvs.length_eq
                simp only [List.length_map] at hl
                omega
              simp only [hget]
              rw [if_pos hlen]
              obtain ⟨Γf, hbody, hnlf⟩ := hwf.fns fd (List.mem_of_getElem? hget)
              have hfmg : FrameMatches P.structs (fnCtx fd)
                  { env := (mintParams H₁ vs).2.reverse, scope := (mintParams H₁ vs).2 }
                  (mintParams H₁ vs).1 :=
                ⟨matches_mintParams fd.params vs H₁ hvs, rfl⟩
              have hfreshg : ∀ ℓ ∈ (mintParams H₁ vs).2.reverse, H₁.length ≤ ℓ :=
                fun ℓ hm => mintParams_fresh H₁ vs ℓ (List.mem_reverse.mp hm)
              have hkeep : ∀ ℓ, ℓ < H₁.length → (mintParams H₁ vs).1[ℓ]? = H₁[ℓ]? := by
                intro ℓ hlt
                rw [mintParams_store]
                exact List.getElem?_append_left hlt
              have hpre : H₁.length ≤ (mintParams H₁ vs).1.length := by
                rw [mintParams_store]; simp
              have hdisj : ∀ ℓ ∈ φ.env, ℓ ∉ (mintParams H₁ vs).2.reverse := by
                intro ℓ hm hg
                exact absurd (hfreshg ℓ hg) (by have := hfm₁.store.mem_lt ℓ hm; omega)
              have hmint : Matches P.structs Γ' φ.env (mintParams H₁ vs).1 := by
                rw [mintParams_store]; exact hfm₁.store.append _
              have kb := ih hbody hfmg
              cases hrb : eval fuel P (mintParams H₁ vs).1
                  { env := (mintParams H₁ vs).2.reverse, scope := (mintParams H₁ vs).2 }
                  fd.body with
              | ok H₃ v tr₃ =>
                  rw [hrb] at kb
                  obtain ⟨htyv, hfm₃, hu₃⟩ := kb
                  obtain ⟨H₄, evs, hrun, hlen4, hout4⟩ := runAllScopeDrops_ok hfm₃ hnlf
                  simp only [EvalRes.absorb, hrun, EvalRes.withTrace]
                  have hu34 : Untouched (mintParams H₁ vs).2.reverse H₃ H₄ :=
                    ⟨by omega, fun ℓ _ hnin => hout4 ℓ hnin⟩
                  have hu4 : Untouched (mintParams H₁ vs).2.reverse (mintParams H₁ vs).1 H₄ :=
                    hu₃.trans hu34
                  exact ⟨htyv, ⟨hmint.transport hdisj hu4, hfm.record⟩,
                    hu₁.trans (Untouched.of_fresh hpre hfreshg hkeep hu4)⟩
              | returned H₃ v tr₃ =>
                  rw [hrb] at kb
                  obtain ⟨htyv, hu₃⟩ := kb
                  simp only [EvalRes.absorb, EvalRes.withTrace]
                  exact ⟨htyv, ⟨hmint.transport hdisj hu₃, hfm.record⟩,
                    hu₁.trans (Untouched.of_fresh hpre hfreshg hkeep hu₃)⟩
              | panic pk tr => simp only [EvalRes.absorb, EvalRes.withTrace]; trivial
              | stuck w => rw [hrb] at kb; exact kb.elim
              | outOfFuel => simp only [EvalRes.absorb, EvalRes.withTrace]; trivial
      | @ret Γ Γ₁ Γ'' e T hty hnl hskel =>
          simp only [eval]
          refine EvalOk.bind (ih hty hfm) ?_
          intro H₁ v tr _ htyv hfm₁
          obtain ⟨H₂, evs, hrun, hlen, hout⟩ := runAllScopeDrops_ok hfm₁ hnl
          simp only [hrun]
          exact ⟨htyv, ⟨by omega, fun ℓ _ hnin => hout ℓ hnin⟩⟩

/-! ## Fuel: the ∀-fuel theorem is not vacuous

`soundness` quantifies over every fuel bound, and `outOfFuel` satisfies it for
free, so on its own it would also be satisfied by an interpreter that gave up
immediately. These two lemmas rule that out. `fuel_mono` says a bound that
produced an answer produces the *same* answer at every larger bound — so
raising the bound never changes a result, and the answer at any sufficient
bound is *the* answer. `no_masking` says the converse for refusals: a bound
that reached a violation cannot be traded for one that hides it, so no choice
of fuel turns a violation into exhaustion for a program some fuel completes.
-/

/-- Prefixing a trace neither creates nor destroys exhaustion (helper). -/
theorem EvalRes.withTrace_outOfFuel_iff {r : EvalRes} {tr : List Event} :
    r.withTrace tr = .outOfFuel ↔ r = .outOfFuel := by
  cases r <;> simp [EvalRes.withTrace]

/-- §6.2's search is monotone in the fuel: if the operand's result is stable
and the context's result is stable on that value, the whole form's result is
(helper). -/
theorem EvalRes.andThen_mono {r r' : EvalRes} {k k' : Store → Val → EvalRes}
    (hr : r ≠ .outOfFuel → r' = r)
    (hk : ∀ H v tr, r = .ok H v tr → k H v ≠ .outOfFuel → k' H v = k H v)
    (h : r.andThen k ≠ .outOfFuel) : r'.andThen k' = r.andThen k := by
  cases r with
  | ok H v tr =>
      rw [hr (by simp)]
      simp only [EvalRes.andThen] at h ⊢
      have hkne : k H v ≠ .outOfFuel := by
        intro hc
        exact h (by rw [hc]; simp [EvalRes.withTrace])
      rw [hk H v tr rfl hkne]
  | returned H v tr => rw [hr (by simp)]; rfl
  | panic pk tr => rw [hr (by simp)]; rfl
  | stuck w => rw [hr (by simp)]; rfl
  | outOfFuel => simp only [EvalRes.andThen] at h; exact absurd rfl h

/-- §6.9's call boundary is monotone in the fuel, for the same reason
(helper). -/
theorem EvalRes.absorb_mono {r r' : EvalRes} {k k' : Store → Val → EvalRes}
    (hr : r ≠ .outOfFuel → r' = r)
    (hk : ∀ H v tr, r = .ok H v tr → k H v ≠ .outOfFuel → k' H v = k H v)
    (h : r.absorb k ≠ .outOfFuel) : r'.absorb k' = r.absorb k := by
  cases r with
  | ok H v tr =>
      rw [hr (by simp)]
      simp only [EvalRes.absorb] at h ⊢
      have hkne : k H v ≠ .outOfFuel := by
        intro hc
        exact h (by rw [hc]; simp [EvalRes.withTrace])
      rw [hk H v tr rfl hkne]
  | returned H v tr => rw [hr (by simp)]; rfl
  | panic pk tr => rw [hr (by simp)]; rfl
  | stuck w => rw [hr (by simp)]; rfl
  | outOfFuel => simp only [EvalRes.absorb] at h; exact absurd rfl h

/-- An argument list's evaluation is monotone in the fuel, argument by
argument (helper). -/
theorem evalArgs_mono {ev ev' : Store → Expr → EvalRes}
    (hev : ∀ H e, ev H e ≠ .outOfFuel → ev' H e = ev H e) :
    ∀ (H : Store) (es : List Expr), evalArgs ev H es ≠ .abort .outOfFuel →
      evalArgs ev' H es = evalArgs ev H es
  | _, [], _ => rfl
  | H, e :: es, hne => by
      simp only [evalArgs] at hne ⊢
      cases hr : ev H e with
      | ok H₁ v tr =>
          rw [hr] at hne
          rw [hev H e (by rw [hr]; simp), hr]
          simp only [] at hne ⊢
          have hrest : evalArgs ev H₁ es ≠ .abort .outOfFuel := by
            intro hc
            rw [hc] at hne
            exact hne (by simp [EvalRes.withTrace])
          rw [evalArgs_mono hev H₁ es hrest]
      | returned H₁ v tr => rw [hev H e (by rw [hr]; simp), hr]
      | panic pk tr' => rw [hev H e (by rw [hr]; simp), hr]
      | stuck w => rw [hev H e (by rw [hr]; simp), hr]
      | outOfFuel => rw [hr] at hne; exact absurd rfl hne

/-- One step of fuel monotonicity: a bound that answered answers the same at
the next bound up (helper). -/
theorem eval_succ {P : Program} : ∀ (fuel : Nat) (H : Store) (φ : Frame) (e : Expr),
    eval fuel P H φ e ≠ .outOfFuel → eval (fuel + 1) P H φ e = eval fuel P H φ e := by
  intro fuel
  induction fuel with
  | zero => intro H φ e h; simp only [eval] at h; exact absurd rfl h
  | succ n ih =>
      intro H φ e h
      cases e with
      | intLit w sg m => rfl
      | boolLit b => rfl
      | unitLit => rfl
      | use i => rfl
      | drop i => rfl
      | panic msg => rfl
      | binop op e₁ e₂ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ e₁ hne) ?_ h
          intro H₁ v tr _ hkne
          refine EvalRes.andThen_mono (fun hne => ih H₁ φ e₂ hne) ?_ hkne
          intro H₂ v₂ tr₂ _ _
          rfl
      | unop op e₁ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ e₁ hne) ?_ h
          intro H₁ v tr _ _
          rfl
      | intCast w sg e₁ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ e₁ hne) ?_ h
          intro H₁ v tr _ _
          rfl
      | dbg e₁ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ e₁ hne) ?_ h
          intro H₁ v tr _ _
          rfl
      | mkStruct s' args =>
          have hargs : evalArgs (fun H' e' => eval n P H' φ e') H args ≠ .abort .outOfFuel := by
            intro hc
            simp only [eval, hc] at h
            exact h rfl
          have heq := evalArgs_mono (fun H' e' hne => ih H' φ e' hne) H args hargs
          simp only [eval, heq]
      | consume e₁ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ e₁ hne) ?_ h
          intro H₁ v tr _ _
          rfl
      | letIn m e₁ e₂ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ e₁ hne) ?_ h
          intro H₁ v tr _ hkne
          refine EvalRes.andThen_mono (fun hne => ih _ _ e₂ hne) ?_ hkne
          intro H₂ v₂ tr₂ _ _
          rfl
      | assign i e₁ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ e₁ hne) ?_ h
          intro H₁ v tr _ _
          rfl
      | seq e₁ e₂ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ e₁ hne) ?_ h
          intro H₁ v tr _ hkne
          cases hml : v.mult P.structs with
          | linear => rfl
          | affine =>
              simp only [hml] at hkne ⊢
              cases hdv : dropValue P.structs v with
              | error w => rfl
              | ok evs =>
                  simp only [hdv] at hkne ⊢
                  rw [ih H₁ φ e₂ (fun hc => hkne (by rw [hc]; simp [EvalRes.withTrace]))]
          | copy =>
              simp only [hml] at hkne
              rw [ih H₁ φ e₂ hkne]
      | ite c e₁ e₂ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ c hne) ?_ h
          intro H₀ v tr _ hkne
          cases v with
          | bool b =>
              dsimp only at hkne ⊢
              by_cases hb : b = true
              · simp only [if_pos hb] at hkne ⊢
                rw [ih H₀ φ e₁ hkne]
              · simp only [if_neg hb] at hkne ⊢
                rw [ih H₀ φ e₂ hkne]
          | int n => rfl
          | unit => rfl
          | struct s' vs => rfl
      | ret e₁ =>
          simp only [eval] at h ⊢
          refine EvalRes.andThen_mono (fun hne => ih H φ e₁ hne) ?_ h
          intro H₁ v tr _ _
          rfl
      | call f args =>
          have hargs : evalArgs (fun H' e' => eval n P H' φ e') H args ≠ .abort .outOfFuel := by
            intro hc
            simp only [eval, hc] at h
            exact h rfl
          have heq := evalArgs_mono (fun H' e' hne => ih H' φ e' hne) H args hargs
          simp only [eval, heq] at h ⊢
          cases hra : evalArgs (fun H' e' => eval n P H' φ e') H args with
          | abort r => rfl
          | ok H₁ vs tr =>
              rw [hra] at h
              simp only [] at h ⊢
              cases hf : P.fns[f]? with
              | none => rfl
              | some fd =>
                  rw [hf] at h
                  simp only [] at h ⊢
                  by_cases hlen : fd.params.length = vs.length
                  · simp only [if_pos hlen] at h ⊢
                    have h' : (eval n P (mintParams H₁ vs).1
                        { env := (mintParams H₁ vs).2.reverse, scope := (mintParams H₁ vs).2 }
                        fd.body).absorb (fun H₃ v =>
                          match runAllScopeDrops P.structs H₃
                              { env := (mintParams H₁ vs).2.reverse,
                                scope := (mintParams H₁ vs).2 } with
                          | .error w => .stuck w
                          | .ok (H₄, evs) => .ok H₄ v evs) ≠ .outOfFuel := by
                      intro hc
                      exact h (EvalRes.withTrace_outOfFuel_iff.mpr hc)
                    refine congrArg (EvalRes.withTrace tr) ?_
                    exact EvalRes.absorb_mono (fun hne => ih _ _ fd.body hne)
                      (fun H₃ v tr₃ _ _ => rfl) h'
                  · simp only [if_neg hlen]

/-- **Fuel monotonicity.** A bound that produced a result other than
`outOfFuel` produces that same result at every larger bound: raising the fuel
never changes an answer, so "the answer at some fuel" is well defined and the
∀-fuel form of `soundness` is a statement about it. The fuel is this
interpreter's own device, not a §6 notion, so what this lemma is about is the
claim `eval` makes on behalf of §6's machine. -/
theorem fuel_mono {P : Program} {H : Store} {φ : Frame} {e : Expr} :
    ∀ {n m : Nat}, n ≤ m → eval n P H φ e ≠ .outOfFuel →
      eval m P H φ e = eval n P H φ e := by
  intro n m hle hne
  induction m with
  | zero =>
      have hn : n = 0 := by omega
      subst hn; rfl
  | succ m ihm =>
      rcases Nat.lt_or_ge m.succ n.succ with hlt | hge
      · have hn : n = m + 1 := by omega
        subst hn; rfl
      · have hnm : n ≤ m := by omega
        have hrec := ihm hnm
        rw [eval_succ m H φ e (by rw [hrec]; exact hne), hrec]

/-- **No masking.** A fuel bound that reached a violation cannot be traded for
one that hides it: at every bound that answers at all, the answer is that same
violation. So no choice of fuel turns a violation into exhaustion for a
program some fuel completes, and the `outOfFuel` escape hatch in the §7
theorems (§6's machine has no such state) cannot be what makes them true. -/
theorem no_masking {P : Program} {H : Store} {φ : Frame} {e : Expr} {n m : Nat}
    {w : Violation} (hn : eval n P H φ e = .stuck w) (hm : eval m P H φ e ≠ .outOfFuel) :
    eval m P H φ e = .stuck w := by
  rcases Nat.le_total n m with hle | hle
  · rw [fuel_mono hle (by rw [hn]; simp)]; exact hn
  · rw [← fuel_mono hle hm]; exact hn

/-! ## §7, over a whole program -/

/-- The entry call `main()` is well typed under any enclosing return type: it
reads only the callee's signature (§5.8's (Call)) and passes no arguments
(helper). -/
theorem entry_typed {P : Program} {fd : FnDef} (h0 : P.fns[0]? = some fd)
    (hp : fd.params = []) (R : Ty) : Typed P R [] (.call 0 []) fd.ret [] := by
  refine .call h0 ?_
  simp only [hp, List.map_nil]
  exact .nil

/-- The machine's initial state satisfies the frame invariant: no bindings, no
store, an empty scope record (helper). -/
theorem frameMatches_empty {D : StructEnv} : FrameMatches D [] { env := [], scope := [] } [] :=
  ⟨.nil, rfl⟩

/-- Prefixing a trace cannot make a result an unwinding `return` that was not
one (helper). -/
theorem EvalRes.withTrace_ne_returned {r : EvalRes} {tr : List Event}
    (h : ∀ H v tr', r ≠ .returned H v tr') :
    ∀ H v tr', r.withTrace tr ≠ .returned H v tr' := by
  intro H v tr'
  cases r with
  | returned H₁ v₁ tr₁ => exact absurd rfl (h H₁ v₁ tr₁)
  | _ => simp [EvalRes.withTrace]

/-- §6.9's call boundary absorbs an unwinding `return`, so a call never hands
one on (helper). -/
theorem EvalRes.absorb_ne_returned {r : EvalRes} {k : Store → Val → EvalRes}
    (hk : ∀ H₀ v₀ H' v' tr', k H₀ v₀ ≠ .returned H' v' tr') :
    ∀ H v tr, r.absorb k ≠ .returned H v tr := by
  intro H v tr
  cases r with
  | ok H₁ v₁ tr₁ =>
      simp only [EvalRes.absorb]
      exact EvalRes.withTrace_ne_returned (fun H' v' tr' => hk H₁ v₁ H' v' tr') H v tr
  | returned H₁ v₁ tr₁ => simp [EvalRes.absorb]
  | panic pk tr => simp [EvalRes.absorb]
  | stuck w => simp [EvalRes.absorb]
  | outOfFuel => simp [EvalRes.absorb]

/-- (D-Return-Main) §6.9 needs no rule of its own here: the entry point is an
ordinary call, and the call boundary absorbs an unwinding `return` exactly as
it does anywhere, so a program's outcome is never a `returned` result. -/
theorem run_ne_returned {P : Program} {fuel : Nat} :
    ∀ H v tr, run P fuel ≠ .returned H v tr := by
  intro H v tr
  unfold run
  cases fuel with
  | zero => simp [eval]
  | succ n =>
      simp only [eval, evalArgs]
      refine EvalRes.withTrace_ne_returned ?_ H v tr
      intro H' v' tr'
      cases hf : P.fns[0]? with
      | none => simp
      | some fd =>
          simp only []
          split
          · exact EvalRes.absorb_ne_returned (by
              intro H₀ v₀ H₁ v₁ tr₁
              split <;> simp) H' v' tr'
          · simp

/-- **Program safety** (§7, over `Dynamics.run`). A well-formed program, run
at any fuel, either exhausts its fuel, traps in a defined way (§6.12), or
produces a value of the entry point's declared return type. It never reaches a
`Violation`. -/
theorem run_safe {P : Program} {fd : FnDef} (hwf : WfProgram P)
    (h0 : P.fns[0]? = some fd) (hp : fd.params = []) (fuel : Nat) :
    run P fuel = .outOfFuel ∨ (∃ k tr, run P fuel = .panic k tr) ∨
      (∃ H v tr, run P fuel = .ok H v tr ∧ HasTy P.structs v fd.ret) := by
  have hok : EvalOk P.structs fd.ret fd.ret [] { env := [], scope := [] } [] (run P fuel) :=
    soundness hwf fuel (entry_typed h0 hp fd.ret) frameMatches_empty
  cases hr : run P fuel with
  | ok H v tr =>
      rw [hr] at hok
      exact Or.inr (Or.inr ⟨H, v, tr, rfl, hok.1⟩)
  | returned H v tr => exact absurd hr (run_ne_returned H v tr)
  | panic k tr => exact Or.inr (Or.inl ⟨k, tr, rfl⟩)
  | stuck w => rw [hr] at hok; exact hok.elim
  | outOfFuel => exact Or.inl rfl

/-- The same, from the packaged well-formedness of a whole program: §7 over
`ProgramTyped`, which is what `checkProgram` decides. The entry function is
existentially quantified because `ProgramTyped` only says one exists; the
value's type is still the one that function declares, so this form claims
exactly what `run_safe` proves. -/
theorem ProgramTyped.run_safe {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    ∃ fd, P.fns[0]? = some fd ∧
      (run P fuel = .outOfFuel ∨ (∃ k tr, run P fuel = .panic k tr) ∨
        (∃ H v tr, run P fuel = .ok H v tr ∧ HasTy P.structs v fd.ret)) := by
  obtain ⟨fd, h0, hp⟩ := h.entry
  refine ⟨fd, h0, ?_⟩
  rcases RueCore.run_safe h.wf h0 hp fuel with h₁ | ⟨k, trk, h₂⟩ | ⟨H, v, tr, h₃, hty⟩
  · exact Or.inl h₁
  · exact Or.inr (Or.inl ⟨k, trk, h₂⟩)
  · exact Or.inr (Or.inr ⟨H, v, tr, h₃, hty⟩)

/-! ## §7 corollaries, named -/

/-- A well-formed program never reaches any of the machine's **named**
violations, at any fuel.

Read as "§7's bullets, conjoined", this would overstate the linear bullet by
one edge. A by-value argument value that a *later* argument of the same call
destroys by `return` is in no cell and no scope record, so its drop is neither
run nor monitored and none of the five violations fires — a linear value can
be consumed zero times without this theorem noticing. That edge is the
calculus as written — §6.9's unwinding rule walks only σ, and §5.7's
strict-context bottom rule (`Strict-Bottom` there, which the fragment does not
mechanize) imposes no discard check on siblings already evaluated — it is what
the Rue compiler does, and closing it is an open spec decision (RUE-2316, the
pending-argument decision). `Dynamics.lean`'s "Pending arguments" section
states it in full and `Examples.lean`'s `linearLostAtCallArg` is the
kernel-checked witness; every *other* edge — a `let`'s scope exit, a frame's
normal pop, and a `return`'s unwind — is covered. -/
theorem no_violation {P : Program} (h : ProgramTyped P) (fuel : Nat) (w : Violation) :
    run P fuel ≠ .stuck w := by
  obtain ⟨_, _, h₁ | ⟨k, trk, h₂⟩ | ⟨H, v, tr, h₃, _⟩⟩ := h.run_safe fuel
  · rw [h₁]; simp
  · rw [h₂]; simp
  · rw [h₃]; simp

/-- §7 "No use-after-move": the machine never reads a `⊘` cell. -/
theorem no_use_after_move {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run P fuel ≠ .stuck .useAfterMove := no_violation h fuel _

/-- §7 "No use-after-drop": the machine never touches a retired (`†`) cell.
With frames, this is a consequence of the invariant rather than a structural
fact about closed expressions: `run-all-scope-drops` (§6.9) walks the frame's
scope record at every `return` and at every frame pop, and it is
`FrameMatches` — the record is the environment, whose cells `Matches` says are
live or moved out and pairwise distinct — that keeps those walks off a `†`
cell and stops any cell being retired twice. -/
theorem no_use_after_drop {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run P fuel ≠ .stuck .useAfterDrop := no_violation h fuel _

/-- §7 "Linear values are consumed exactly once", leak half: neither a scope
exit (§6.7) nor a frame unwind (§6.9) ever sees a live linear value. -/
theorem no_linear_leak {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run P fuel ≠ .stuck .linearLeak := no_violation h fuel _

/-- §7 linear bullet, overwrite half (`3.8:77`, the RUE-387 premise). -/
theorem no_linear_overwrite {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run P fuel ≠ .stuck .linearOverwrite := no_violation h fuel _

/-- §7 linear bullet, discard half (`3.8:64`). -/
theorem no_linear_discard {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run P fuel ≠ .stuck .linearDiscard := no_violation h fuel _

end RueCore
