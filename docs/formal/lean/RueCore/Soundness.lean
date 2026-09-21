import RueCore.Dynamics

/-!
# RueCore.Soundness — the §7 memory-safety theorem, fragment-sized

The central invariant is `Matches D Γ ρ H` — "Σ faithfully tracks the store's
initialization", the load-bearing clause of §7's no-use-after-move bullet. `D`
is the program's struct declarations, which is what a type's class (§3) and a
value's drop (§6.11) are read against; every predicate here carries it.

With Σ keyed by path (`OwnSt`, `Statics.lean`) and a cell holding a tree with
`⊘` at any node (`Contents`, `Dynamics.lean`), the per-cell clause is
**recursive**: `ContentsMatches` relates the two trees at the path's declared
type. It is deliberately asymmetric at its `movedOut` clause, mirroring §5.5:
a *statically* `MovedOut` path may still hold live (non-linear!) contents —
that is exactly the state a conservative branch join produces, and the machine
drops such residues path-specifically (`3.8:60`); a statically `Owned` path
always holds a value; and a live **linear** sub-value is never behind a
`MovedOut` node, which is what makes the leak/overwrite refusals unreachable.

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

## Values, contents, and the drop that never refuses

`HasTy D` types a *value* — the thing an expression evaluates to, which never
has a hole in it — against §2's types, a struct against its declaration's
field list ((Struct-Intro) §5.8 read on values). `ContentsTy D` is the same for
what a cell holds, with §6.1's `⊘` admitted at every node and well typed at
every type, because a moved-out position claims nothing about what used to be
there. `Contents.holeFree` is what says a contents *is* a value, and
`ContentsTy.toVal` is the bridge: that is the half (D-Use-Copy)/(D-Use-Move)
§6.3 need, since they hand the context a value and `fully-owned(Σ, p)` §5.1 is
what says the contents they read has no hole in it.

Statements about §6.11's walk rest on `ContentsTy`. `dropContents_events` and
`dropContentsList_events` give the walk in **closed form** — for well-typed
contents it emits exactly `dropEvents`, §6.11's order written as a function —
and `dropContents_struct_events` is that read at a struct: the destructor's
event (`3.9:28`) followed by the concatenation of the fields' events in
declaration order (`3.9:13`), with a field that has been **moved out**
contributing none (`3.8:60`), which is the shape RUE-2237's "dropped exactly
once" quantifies over. `dropContents_order` and `dropContentsList_order` are
the one-level induction steps, `dropContents_ok` the corollary that the walk
never refuses, and `ContentsMatches.residualLinear_false` the reason the leak
monitor's reading of the residue is the one §5.6 computes. The **overwrite**
monitor reads the residue too, but (Assign) §5.2's premise is type-keyed
(`overwriteOk`), so that case of `soundness` discharges it from either
disjunct: `MovedOut` carries nothing, and `ContentsTy.residualLinear_false`
settles a non-linear type.

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
  /-- §6.1's `f_T` at `T = float(w)`: the datum lies in `𝔽_w`, which is the
  float counterpart of `n_T`'s `min_T ≤ n ≤ max_T` side condition. Keeping it
  is what gives §7's "totality of the float operations" lemma something to
  preserve: the model's closure laws (`FloatModel`, `Float.lean`) are exactly
  what re-establishes it after a rounded operation. -/
  | float {w f} : f.Wf w → HasTy D (.float w f) (.float w)
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

/-- Inversion of value typing at a float type (helper). -/
theorem HasTy.float_inv {D v w} (h : HasTy D v (.float w)) :
    ∃ f, v = .float w f ∧ f.Wf w := by
  cases h; exact ⟨_, rfl, ‹_›⟩

/-- Inversion of value typing at `bool` (helper). -/
theorem HasTy.bool_inv {D v} (h : HasTy D v .bool) : ∃ b, v = .bool b := by
  cases h; exact ⟨_, rfl⟩

/-- Inversion of value typing at a struct type (helper). -/
theorem HasTy.struct_inv {D v s} (h : HasTy D v (.struct s)) :
    ∃ sd vs, v = .struct s vs ∧ D[s]? = some sd ∧ HasTys D vs sd.fields := by
  cases h; exact ⟨_, _, rfl, ‹_›, ‹_›⟩

/-! ## Typing the contents of a cell

`HasTy` types the machine's *values*, which never have a hole in them. A cell
holds `Contents` — the same trees with §6.1's `⊘` admitted at any node, which
is what a partial move leaves (§4.2, `3.8:22`) — so the invariant needs the
analogous relation. `ContentsTy` is it: a `⊘` is well typed at **every** type,
because a moved-out position makes no claim about what used to be there, and
everything else types as its value would. `holeFree` says the tree has no `⊘`
in it, and a hole-free well-typed contents is exactly (the image of) a
well-typed value.
-/

mutual
/-- Whether a contents tree has no `⊘` anywhere in it — the tree of a value
(helper). -/
def Contents.holeFree : Contents → Bool
  | .hole => false
  | .int _ _ _ | .float _ _ | .bool _ | .unit => true
  | .struct _ cs => Contents.holeFreeList cs

/-- The same over a field list (helper). -/
def Contents.holeFreeList : List Contents → Bool
  | [] => true
  | c :: cs => Contents.holeFree c && Contents.holeFreeList cs
end

mutual
/-- Cell contents typed against §2's types, with §6.1's `⊘` admitted at any
node. A moved-out position claims nothing, so it is well typed at every type;
every other node types as the corresponding value form does (§5.8's
(Struct-Intro), read on stored contents). -/
inductive ContentsTy (D : StructEnv) : Contents → Ty → Prop where
  | hole {T} : ContentsTy D .hole T
  | int {w s n} : InBounds w s n → ContentsTy D (.int w s n) (.int w s)
  /-- §6.1's `f_T` stored in a cell, with the same `𝔽_w` side condition
  `HasTy.float` carries. -/
  | float {w f} : f.Wf w → ContentsTy D (.float w f) (.float w)
  | bool {b} : ContentsTy D (.bool b) .bool
  | unit : ContentsTy D .unit .unit
  | struct {s sd cs} :
      D[s]? = some sd → ContentsTys D cs sd.fields → ContentsTy D (.struct s cs) (.struct s)

/-- The same, pointwise against a declaration's field list (§5.8's
(Struct-Intro), read on stored contents). -/
inductive ContentsTys (D : StructEnv) : List Contents → List Ty → Prop where
  | nil : ContentsTys D [] []
  | cons {c cs T Ts} : ContentsTy D c T → ContentsTys D cs Ts → ContentsTys D (c :: cs) (T :: Ts)
end

/-- A field list has as many members as expected types (helper). -/
theorem ContentsTys.length_eq : ∀ {D cs Ts}, ContentsTys D cs Ts → cs.length = Ts.length
  | _, _, _, .nil => rfl
  | _, _, _, .cons _ h => by simp [ContentsTys.length_eq h]

/-- Inversion of contents typing at a struct type, for a contents that is not
a hole (helper). -/
theorem ContentsTy.struct_inv {D s cs T} (h : ContentsTy D (.struct s cs) T) :
    ∃ sd, T = .struct s ∧ D[s]? = some sd ∧ ContentsTys D cs sd.fields := by
  cases h; exact ⟨_, rfl, ‹_›, ‹_›⟩

/-- A field of a well-typed contents is well typed at its declared type
(helper). -/
theorem ContentsTys.index : ∀ {D : StructEnv} {cs : List Contents} {Ts : List Ty}
    (f : Nat) {Tf : Ty}, ContentsTys D cs Ts → Ts[f]? = some Tf →
    ∃ cf, cs[f]? = some cf ∧ ContentsTy D cf Tf
  | _, _, _, _, _, .nil, h => by simp at h
  | _, _, _, 0, _, .cons hc _, h => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at h
      exact ⟨_, rfl, h ▸ hc⟩
  | _, _, _, (f + 1), _, .cons _ hcs, h => by
      simp only [List.getElem?_cons_succ] at h ⊢
      exact ContentsTys.index f hcs h

/-- Writing a well-typed contents into a field slot keeps the list well typed
(helper). -/
theorem ContentsTys.set : ∀ {D : StructEnv} {cs : List Contents} {Ts : List Ty}
    (f : Nat) {Tf : Ty} {cf' : Contents}, ContentsTys D cs Ts → Ts[f]? = some Tf →
    ContentsTy D cf' Tf → ContentsTys D (cs.set f cf') Ts
  | _, _, _, _, _, _, .nil, h, _ => by simp at h
  | _, _, _, 0, _, _, .cons _ hcs, h, hnew => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at h
      exact .cons (h ▸ hnew) hcs
  | _, _, _, (f + 1), _, _, .cons hc hcs, h, hnew => by
      simp only [List.getElem?_cons_succ] at h
      exact .cons hc (ContentsTys.set f hcs h hnew)

mutual
/-- The image of a well-typed value is well-typed contents (helper). -/
theorem HasTy.contentsTy {D v T} (h : HasTy D v T) : ContentsTy D (Contents.ofVal v) T := by
  cases h with
  | int hb => exact .int hb
  | float hw => exact .float hw
  | bool => exact .bool
  | unit => exact .unit
  | struct hd hvs => exact .struct hd (HasTys.contentsTys hvs)

/-- The same over a field list (helper). -/
theorem HasTys.contentsTys {D vs Ts} (h : HasTys D vs Ts) :
    ContentsTys D (Contents.ofVals vs) Ts := by
  cases h with
  | nil => exact .nil
  | cons hv hvs => exact .cons (HasTy.contentsTy hv) (HasTys.contentsTys hvs)
end

mutual
/-- The image of a value has no hole in it (helper). -/
theorem Contents.holeFree_ofVal (v : Val) : (Contents.ofVal v).holeFree = true := by
  cases v with
  | int => rfl
  | float => rfl
  | bool => rfl
  | unit => rfl
  | struct s vs => exact Contents.holeFreeList_ofVals vs

/-- The same over a field list (helper). -/
theorem Contents.holeFreeList_ofVals : ∀ vs : List Val,
    Contents.holeFreeList (Contents.ofVals vs) = true
  | [] => rfl
  | v :: vs => by
      show (Contents.holeFree (Contents.ofVal v) && _) = true
      rw [Contents.holeFree_ofVal v, Contents.holeFreeList_ofVals vs]
      rfl
end

mutual
/-- **A hole-free well-typed contents is a well-typed value.** This is the
half of the correspondence the machine needs at a use: (D-Use-Copy)/(D-Use-Move)
§6.3 hand the context a *value*, and `fully-owned(Σ, p)` (§5.1) is what says
the contents they read has no hole in it (helper). -/
theorem ContentsTy.toVal {D c T} (h : ContentsTy D c T) (hf : c.holeFree = true) :
    ∃ v, c.toVal = some v ∧ HasTy D v T := by
  cases h with
  | hole => exact absurd hf (by simp [Contents.holeFree])
  | int hb => exact ⟨_, rfl, .int hb⟩
  | float hw => exact ⟨_, rfl, .float hw⟩
  | bool => exact ⟨_, rfl, .bool⟩
  | unit => exact ⟨_, rfl, .unit⟩
  | @struct s sd cs hd hcs =>
      obtain ⟨vs, hvs, hty⟩ := ContentsTys.toVals hcs (by
        simpa only [Contents.holeFree] using hf)
      exact ⟨_, by simp only [Contents.toVal, hvs, Option.map_some], .struct hd hty⟩

/-- The same over a field list (helper). -/
theorem ContentsTys.toVals {D cs Ts} (h : ContentsTys D cs Ts)
    (hf : Contents.holeFreeList cs = true) :
    ∃ vs, Contents.toVals cs = some vs ∧ HasTys D vs Ts := by
  cases h with
  | nil => exact ⟨[], rfl, .nil⟩
  | @cons c cs T Ts hc hcs =>
      simp only [Contents.holeFreeList, Bool.and_eq_true] at hf
      obtain ⟨v, hv, htv⟩ := ContentsTy.toVal hc hf.1
      obtain ⟨vs, hvs, htvs⟩ := ContentsTys.toVals hcs hf.2
      exact ⟨v :: vs, by simp only [Contents.toVals, hv, hvs], .cons htv htvs⟩
end

/-- A hole-free well-typed contents has its type's class, which is what the
`Copy` test of (D-Use-Copy) and of `dropCell` reads (helper). -/
theorem ContentsTy.mult_eq {D c T} (h : ContentsTy D c T) (hf : c.holeFree = true) :
    c.mult D = T.mult D := by
  cases h with
  | hole => exact absurd hf (by simp [Contents.holeFree])
  | int => rfl
  | float => rfl
  | bool => rfl
  | unit => rfl
  | struct hd _ => simp [Contents.mult, Ty.mult, StructEnv.classOf, hd]

/-! ## §6.11's walk over contents: it never refuses, and it drops in order -/

mutual
/-- **§6.11's order, in closed form.** For well-typed contents the walk's
result is exactly `dropEvents`, §6.11's order written out as a function
(`Dynamics.lean`) — the destructor first (`3.9:28`), then the fields in
declaration order (`3.9:13`), every `⊘` skipped (`3.8:60`). -/
theorem dropContents_events {D : StructEnv} {c : Contents} {T : Ty} (h : ContentsTy D c T) :
    dropContents D c = .ok (dropEvents D c) := by
  cases h with
  | hole => rfl
  | int => rfl
  | float => rfl
  | bool => rfl
  | unit => rfl
  | @struct s sd cs hd hcs =>
      simp only [dropContents, dropEvents, hd, dropContentsList_events hcs]

/-- The same over a field list: `drop*` emits exactly the fields' events, in
declaration order (`3.9:13`). -/
theorem dropContentsList_events {D : StructEnv} {cs : List Contents} {Ts : List Ty}
    (h : ContentsTys D cs Ts) : dropContentsList D cs = .ok (dropEventsList D cs) := by
  cases h with
  | nil => rfl
  | cons hc hcs =>
      simp only [dropContentsList, dropEventsList, dropContents_events hc,
        dropContentsList_events hcs]
end

/-- **A well-typed cell's drop always runs.** `dropContents` (§6.11) refuses
only where a struct names a declaration the program does not have, and contents
typing rules that out. -/
theorem dropContents_ok {D : StructEnv} {c : Contents} {T : Ty} (h : ContentsTy D c T) :
    ∃ evs, dropContents D c = .ok evs :=
  ⟨dropEvents D c, dropContents_events h⟩

/-- **The drop-order theorem, in the shape RUE-2237 needs.** Dropping a
well-typed struct's stored contents emits its user destructor's event — when
its declaration has one (`3.9:28`) — followed by the **concatenation of its
fields' drop events, in declaration order** (`3.9:13`), each field's events
given by the same closed form, recursively. Nothing else, and nothing in
another order; the whole list is determined by the contents and the
declarations.

Be exact about the `⊘`-skip: this theorem states the **map**, `cs.map
(dropEvents D)`, and a moved-out field contributes nothing because
`dropEvents .hole = []` *by definition* (`Dynamics.lean`). So `3.8:60`'s skip
is carried by the closed form's own leaf case, not concluded here; what the
theorem adds is that the walk emits exactly that map, in that order. -/
theorem dropContents_struct_events {D : StructEnv} {s : Nat} {sd : StructDecl}
    {cs : List Contents} (hd : D[s]? = some sd) (h : ContentsTy D (.struct s cs) (.struct s)) :
    dropContents D (.struct s cs)
      = .ok ((if sd.dtor then [Event.dtor s (.struct s cs)] else [])
              ++ (cs.map (dropEvents D)).flatten) := by
  rw [dropContents_events h]
  simp only [dropEvents, hd, dropEventsList_eq_flatten]

/-- **§6.11's order, one level.** A struct's drop emits its user destructor's
event first — when its declaration has one — and then exactly the events its
fields' drops emit, in declaration order. This is the induction step;
`dropContents_struct_events` is the closed form. -/
theorem dropContents_order {D : StructEnv} {s : Nat} {sd : StructDecl} {cs : List Contents}
    {evs : List Event} (hd : D[s]? = some sd) (h : dropContents D (.struct s cs) = .ok evs) :
    ∃ fevs, dropContentsList D cs = .ok fevs ∧
      evs = (if sd.dtor then [Event.dtor s (.struct s cs)] else []) ++ fevs := by
  simp only [dropContents, hd] at h
  cases hf : dropContentsList D cs with
  | error w => rw [hf] at h; cases h
  | ok fevs => rw [hf] at h; cases h; exact ⟨fevs, rfl, rfl⟩

/-- **`drop*` is the fields in order** (§6.11): the events of a field list's
drop are the head's followed by the tail's. This is the induction step;
`dropContentsList_events` is the closed form. -/
theorem dropContentsList_order {D : StructEnv} {c : Contents} {cs : List Contents}
    {evs : List Event} (h : dropContentsList D (c :: cs) = .ok evs) :
    ∃ e₁ e₂, dropContents D c = .ok e₁ ∧ dropContentsList D cs = .ok e₂ ∧ evs = e₁ ++ e₂ := by
  simp only [dropContentsList] at h
  cases h₁ : dropContents D c with
  | error w => rw [h₁] at h; cases h
  | ok e₁ =>
      rw [h₁] at h
      cases h₂ : dropContentsList D cs with
      | error w => rw [h₂] at h; cases h
      | ok e₂ => rw [h₂] at h; cases h; exact ⟨e₁, e₂, rfl, rfl, rfl⟩

/-! ## The leak monitor reads what §5.6 computes -/

mutual
/-- **A value of a non-linear type carries no linear residue.** §3's join makes
a struct whose class is not `Linear` one with no linear field at any depth
(`StructDecl.Wf.field_not_linear`), so the machine's monitor — which walks the
stored contents looking for a live declared-`linear` struct — finds none. A
`⊘` contributes nothing whatever its type, so the statement needs no
hole-freeness. -/
theorem ContentsTy.residualLinear_false {D : StructEnv} {c : Contents} {T : Ty}
    (hwf : WfStructs D) (h : ContentsTy D c T) (hnl : T.mult D ≠ .linear) :
    c.residualLinear D = false := by
  cases h with
  | hole => rfl
  | int => rfl
  | float => rfl
  | bool => rfl
  | unit => rfl
  | @struct s sd cs hd hcs =>
      have hcls : sd.cls ≠ .linear := by
        simpa only [Ty.mult, StructEnv.classOf, hd] using hnl
      have hw := hwf s sd hd
      have hattr : ¬ (sd.attr = .linear) := by
        intro ha
        exact hcls (by rw [hw.classIsJoin, ha]; rfl)
      simp only [Contents.residualLinear, hd, hattr, Bool.false_or, decide_false]
      exact ContentsTys.residualLinearList_false hwf hcs (hw.field_not_linear hcls)

/-- The same over a field list (helper). -/
theorem ContentsTys.residualLinearList_false {D : StructEnv} {cs : List Contents} {Ts : List Ty}
    (hwf : WfStructs D) (h : ContentsTys D cs Ts) (hnl : ∀ T ∈ Ts, T.mult D ≠ .linear) :
    Contents.residualLinearList D cs = false := by
  cases h with
  | nil => rfl
  | @cons c cs T Ts hc hcs =>
      simp only [Contents.residualLinearList,
        ContentsTy.residualLinear_false hwf hc (hnl T List.mem_cons_self),
        ContentsTys.residualLinearList_false hwf hcs
          (fun T' hm => hnl T' (List.mem_cons_of_mem _ hm)), Bool.or_self]
end

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
theorem binOpInt_res {D : StructEnv} (op : BinOp) (w : IntWidth) (s : Sign) (n₁ n₂ : Int)
    (hop : op.intAdmits = true) :
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
    simp only [binOpInt, BinOp.resultTy, BinOp.isCompare, BinOp.intAdmits, if_true, if_false,
      Bool.false_eq_true] at hop ⊢
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
  -- `@total_cmp` has no integer arm: `3.12:31` gives it float operands, and
  -- the `intAdmits` premise of (Arith)/(Ord) excludes it, so `hop` is false
  -- there and `simp` has already closed that goal.

/-- The same, over the two machine values §5.8's operator rules give one
`int(w,s)`: the shape mismatch `evalBinOp` refuses is not reachable from
them. -/
theorem evalBinOp_res {D : StructEnv} (M : FloatOps) (op : BinOp) (w : IntWidth) (s : Sign)
    (n₁ n₂ : Int) (hop : op.intAdmits = true) :
    (∃ v, evalBinOp M op (.int w s n₁) (.int w s n₂) = .val v ∧
        HasTy D v (op.resultTy (.int w s))) ∨
      (∃ k, evalBinOp M op (.int w s n₁) (.int w s n₂) = .trap k) := by
  simp only [evalBinOp]
  exact binOpInt_res op w s n₁ n₂ hop

/-- **Every §6.4 float operator lands on a value of its rule's type, and never
traps.** The value cases are (D-Float-Arith), (D-Float-Ord) and (D-Total-Cmp);
there is no trap case at all, which is `3.12:21` and §6.4's note that no
arithmetic trap rule is stated over a float redex. `M.arith_wf` is §7's
closure law — the one thing about `⊕_w` that cannot be proved of an arbitrary
`FloatOps` — and it is what re-establishes `HasTy` at the result. -/
theorem binOpFloat_res {D : StructEnv} (M : FloatModel) (op : BinOp) (w : FloatWidth)
    (a b : FloatDatum) (ha : a.Wf w) (hb : b.Wf w) (hop : op.floatAdmits = true) :
    ∃ v, binOpFloat M.toFloatOps op w a b = .val v ∧ HasTy D v (op.resultTy (.float w)) := by
  cases op <;>
    simp only [binOpFloat, BinOp.resultTy, BinOp.isCompare, BinOp.floatAdmits, if_true, if_false,
      Bool.false_eq_true] at hop ⊢
  case add => exact ⟨_, rfl, .float (M.arith_wf w .add a b ha hb)⟩
  case sub => exact ⟨_, rfl, .float (M.arith_wf w .sub a b ha hb)⟩
  case mul => exact ⟨_, rfl, .float (M.arith_wf w .mul a b ha hb)⟩
  case div => exact ⟨_, rfl, .float (M.arith_wf w .div a b ha hb)⟩
  case lt => exact ⟨_, rfl, .bool⟩
  case le => exact ⟨_, rfl, .bool⟩
  case gt => exact ⟨_, rfl, .bool⟩
  case ge => exact ⟨_, rfl, .bool⟩
  -- `@total_cmp` concludes at `int(32, signed)`, and `totalCmp_trichotomy`
  -- is why `-1`/`0`/`1` is a value of it.
  case totalCmp =>
      refine ⟨_, rfl, .int ?_⟩
      rcases totalCmp_trichotomy a b with h | h | h <;>
        simp only [InBounds, intMin, intMax, IntWidth.bits, h] <;> refine ⟨?_, ?_⟩ <;> decide

/-- The same, over the two machine values (Float-Arith)/(Float-Ord)/
(Total-Cmp) §5.8 give one `float(w)`: the shape mismatch `evalBinOp` refuses
is not reachable from them. -/
theorem evalBinOpFloat_res {D : StructEnv} (M : FloatModel) (op : BinOp) (w : FloatWidth)
    (a b : FloatDatum) (ha : a.Wf w) (hb : b.Wf w) (hop : op.floatAdmits = true) :
    ∃ v, evalBinOp M.toFloatOps op (.float w a) (.float w b) = .val v ∧
      HasTy D v (op.resultTy (.float w)) := by
  simp only [evalBinOp]
  exact binOpFloat_res M op w a b ha hb hop

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

/-- **`neg` on a float is total** ((D-Float-Neg), `3.12:24`): a sign flip,
which `negate_wf` shows keeps the datum in `𝔽_w`. Unlike the integer `neg` it
has no trap case at all. -/
theorem evalUnOp_float_res {D : StructEnv} (w : FloatWidth) (f : FloatDatum) (hw : f.Wf w) :
    ∃ v, evalUnOp .neg (.float w f) = .val v ∧ HasTy D v (.float w) :=
  ⟨_, rfl, .float (negate_wf hw)⟩

/-- **`@int_to_float` lands on a value of its result type and never traps**
((D-Int-To-Float), `3.12:16`). -/
theorem evalFintrin_int_res {D : StructEnv} (M : FloatModel) (w : FloatWidth) (w' : IntWidth)
    (s' : Sign) (n : Int) :
    ∃ v, evalFintrin M.toFloatOps (.intToFloat w) (.int w' s' n) = .val v ∧
      HasTy D v (.float w) :=
  ⟨_, rfl, .float (M.ofInt_wf w n)⟩

/-- **Every one-operand float intrinsic on a `float(w)` operand lands on a
value of its rule's type or on `↯overflow`** — that category and no other,
and only `@float_to_int` reaches it (`3.12:18`, `8.1:7`). `@float_cast` and
the five `3.12:34` intrinsics never trap (`3.12:19`, `3.12:37`), which is why
the trap disjunct is `= .trap .overflow` rather than an existential. The
`Wf` half of each value case is §7's closure obligation: proved here for the
exact operations (`widen_wf`, `roundOp_wf`) and a law of the model for the
rounded ones (`narrow_wf`, `sqrt_wf`). -/
theorem evalFintrin_float_res {D : StructEnv} (M : FloatModel) (k : FloatIntrin)
    (w : FloatWidth) (f : FloatDatum) (hw : f.Wf w) (hk : k.floatSrc w = true) :
    (∃ v, evalFintrin M.toFloatOps k (.float w f) = .val v ∧ HasTy D v (k.resTy w)) ∨
      evalFintrin M.toFloatOps k (.float w f) = .trap .overflow := by
  cases k with
  | intToFloat _ => simp [FloatIntrin.floatSrc] at hk
  | floatToInt w' s' =>
      simp only [evalFintrin, FloatIntrin.resTy]
      split
      · next t ht => exact Or.inl ⟨_, rfl, .int (toIntIn_mem ht)⟩
      · exact Or.inr rfl
  | floatCast w' =>
      refine Or.inl ⟨_, rfl, .float ?_⟩
      cases w <;> cases w' <;>
        simp only [FloatOps.cast] <;>
        first
          | exact hw
          | exact M.narrow_wf f hw
          | exact widen_wf hw
  | roundOp k₀ =>
      refine Or.inl ⟨_, rfl, .float ?_⟩
      cases k₀ with
      | sqrt => exact M.sqrt_wf w f hw
      | round op => exact roundOp_wf hw

/-! ## The store–Σ agreement invariant, path by path

`Matches` is §7's "Σ faithfully tracks the store's initialization". With Σ
keyed by path (`OwnSt`, `Statics.lean`) and a cell holding a tree with `⊘` at
any node (`Contents`, `Dynamics.lean`), the per-cell clause becomes a
**recursive** relation between the two trees and the binding's declared type.

Three clauses, and the middle one carries the deliberate asymmetry §5.5's join
produces:

* an **`owned`** node holds a hole-free well-typed contents — a value;
* a **`movedOut`** node holds well-typed contents with **no live linear
  sub-value** in it. It need not hold `⊘`: a conservative join marks a node
  moved on a path that still holds something (`3.8:60`), and the machine then
  drops that residue path-specifically. What it may never hold is a live
  linear value, which is what makes the leak and overwrite refusals
  unreachable (`3.8:50`);
* a **`fields`** node — a partially moved aggregate — holds a struct of the
  declaration Σ's type names, matching field by field, with a slot no partial
  move touched read as `owned`.
-/

mutual
/-- Per-node agreement between Σ's state for a path and the contents stored
there, at the path's declared type (§7's "Σ faithfully tracks the store's
initialization", section docstring): `owned` holds a value, `movedOut` holds
contents with no live linear sub-value — the §5.5 join's asymmetry, whose
residue the machine drops path-specifically (`3.8:60`) — and `fields` holds the
struct its type names, matched field by field. -/
inductive ContentsMatches (D : StructEnv) : Contents → OwnSt → Ty → Prop where
  /-- An `Owned` path holds a value: well-typed contents with no `⊘` in it. -/
  | owned {c T} : ContentsTy D c T → c.holeFree = true → ContentsMatches D c .owned T
  /-- A `MovedOut` path may still hold live contents — the §5.5 join's
  asymmetry (`3.8:60`) — but never a live linear sub-value (`3.8:50`). -/
  | moved {c T} :
      ContentsTy D c T → c.residualLinear D = false → ContentsMatches D c .movedOut T
  /-- A partially moved path holds the struct its type names, field by
  field. -/
  | fields {s sd cs ts} :
      D[s]? = some sd → ContentsMatchesList D cs ts sd.fields →
      ContentsMatches D (.struct s cs) (.fields ts) (.struct s)

/-- The same over a declaration's fields, slot by slot; a slot Σ has no record
for is `owned` (`OwnSt.fieldAt`) (helper). -/
inductive ContentsMatchesList (D : StructEnv) : List Contents → List OwnSt → List Ty → Prop where
  /-- No fields left to match. -/
  | nil {ts} : ContentsMatchesList D [] ts []
  /-- The first field matches its own slot's state; the rest match the tail of
  the record. -/
  | cons {c cs ts T Ts} :
      ContentsMatches D c (OwnSt.fieldAt ts 0) T → ContentsMatchesList D cs ts.tail Ts →
      ContentsMatchesList D (c :: cs) ts (T :: Ts)
end

mutual
/-- Agreement implies contents typing, which is what makes every drop the
machine runs on a matched cell terminate in an `ok` (helper). -/
theorem ContentsMatches.contentsTy {D c t T} (h : ContentsMatches D c t T) :
    ContentsTy D c T := by
  cases h with
  | owned hc _ => exact hc
  | moved hc _ => exact hc
  | fields hd hl => exact .struct hd (ContentsMatchesList.contentsTys hl)

/-- The same over a field list (helper). -/
theorem ContentsMatchesList.contentsTys {D cs ts Ts} (h : ContentsMatchesList D cs ts Ts) :
    ContentsTys D cs Ts := by
  cases h with
  | nil => exact .nil
  | cons hc hl => exact .cons hc.contentsTy (ContentsMatchesList.contentsTys hl)
end

/-- `Σ`'s record for slot `f+1` is its tail's record for slot `f` (helper). -/
theorem OwnSt.fieldAt_succ (ts : List OwnSt) (f : Nat) :
    OwnSt.fieldAt ts (f + 1) = OwnSt.fieldAt ts.tail f := by
  cases ts <;> simp [OwnSt.fieldAt]

/-- Writing a slot's state at the head replaces it and keeps the tail
(helper). -/
theorem OwnSt.setField_zero : ∀ (ts : List OwnSt) (u : OwnSt),
    OwnSt.setField ts 0 u = u :: ts.tail
  | [], _ => rfl
  | _ :: _, _ => rfl

/-- A write below the head keeps the head and writes into the tail
(helper). -/
theorem OwnSt.setField_succ : ∀ (ts : List OwnSt) (f : Nat) (u : OwnSt),
    OwnSt.setField ts (f + 1) u = OwnSt.fieldAt ts 0 :: OwnSt.setField ts.tail f u
  | [], _, _ => rfl
  | _ :: _, _, _ => rfl

/-- A field of a matched aggregate matches its own slot's state (helper). -/
theorem ContentsMatchesList.index : ∀ {D : StructEnv} {cs : List Contents} {ts : List OwnSt}
    {Ts : List Ty} (f : Nat) {Tf : Ty}, ContentsMatchesList D cs ts Ts → Ts[f]? = some Tf →
    ∃ cf, cs[f]? = some cf ∧ ContentsMatches D cf (OwnSt.fieldAt ts f) Tf
  | _, _, _, _, _, _, .nil, hT => by simp at hT
  | _, _, _, _, 0, _, .cons hc _, hT => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at hT
      exact ⟨_, rfl, hT ▸ hc⟩
  | _, _, ts, _, (f + 1), _, .cons _ hl, hT => by
      simp only [List.getElem?_cons_succ] at hT ⊢
      rw [OwnSt.fieldAt_succ]
      exact ContentsMatchesList.index f hl hT

/-- **Writing one field's contents and its Σ slot together keeps the
aggregate matched.** This is the list half of the partial move: (Use-Move)
§5.1 marks exactly one slot and writes `⊘` into exactly the corresponding
position (helper). -/
theorem ContentsMatchesList.set : ∀ {D : StructEnv} {cs : List Contents} {ts : List OwnSt}
    {Ts : List Ty} (f : Nat) {Tf : Ty} {cf' : Contents} {u' : OwnSt},
    ContentsMatchesList D cs ts Ts → Ts[f]? = some Tf → ContentsMatches D cf' u' Tf →
    ContentsMatchesList D (cs.set f cf') (OwnSt.setField ts f u') Ts
  | _, _, _, _, _, _, _, _, .nil, hT, _ => by simp at hT
  | _, _, ts, _, 0, _, cf', u', .cons _ hl, hT, hnew => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at hT
      subst hT
      rw [OwnSt.setField_zero, List.set_cons_zero]
      refine .cons ?_ ?_
      · simpa only [OwnSt.fieldAt, List.getElem?_cons_zero, Option.getD_some] using hnew
      · simpa only [List.tail_cons] using hl
  | _, _, ts, _, (f + 1), _, cf', u', .cons hc hl, hT, hnew => by
      simp only [List.getElem?_cons_succ] at hT
      rw [OwnSt.setField_succ, List.set_cons_succ]
      refine .cons ?_ ?_
      · simpa only [OwnSt.fieldAt, List.getElem?_cons_zero, Option.getD_some] using hc
      · simpa only [List.tail_cons] using ContentsMatchesList.set f hl hT hnew

/-- A hole-free well-typed struct is a matched aggregate whose every slot is
`owned` — which is what lets the §5.5 join read an `Owned` arm field by field
against a partially moved one (helper). -/
theorem ContentsMatchesList.of_owned : ∀ {D : StructEnv} {cs : List Contents} {Ts : List Ty},
    ContentsTys D cs Ts → Contents.holeFreeList cs = true → ContentsMatchesList D cs [] Ts
  | _, _, _, .nil, _ => .nil
  | D, _, _, @ContentsTys.cons _ c cs T Ts hc hcs, hf => by
      have hf' : Contents.holeFree c = true ∧ Contents.holeFreeList cs = true := by
        simpa only [Contents.holeFreeList, Bool.and_eq_true] using hf
      refine .cons ?_ ?_
      · simpa only [OwnSt.fieldAt, List.getElem?_nil, Option.getD_none] using
          ContentsMatches.owned hc hf'.1
      · simpa only [List.tail_nil] using ContentsMatchesList.of_owned hcs hf'.2

/-- Inversion of an `Owned` match at a struct type: the cell holds that
struct, and every slot of it is `owned` (helper). -/
theorem ContentsMatches.owned_struct {D : StructEnv} {c : Contents} {s : Nat}
    {sd : StructDecl} (hd : D[s]? = some sd) (h : ContentsMatches D c .owned (.struct s)) :
    ∃ cs, c = .struct s cs ∧ ContentsMatchesList D cs [] sd.fields := by
  cases h with
  | owned hty hf =>
      cases hty with
      | hole => simp [Contents.holeFree] at hf
      | @struct s' sd' cs hd' hcs =>
          have : sd' = sd := by rw [hd'] at hd; cases hd; rfl
          subst this
          exact ⟨cs, rfl, ContentsMatchesList.of_owned hcs
            (by simpa only [Contents.holeFree] using hf)⟩

/-- Inversion of a field step (helper). -/
theorem Ty.fieldAt_inv {D : StructEnv} {T Tf : Ty} {f : Nat} (h : T.fieldAt D f = some Tf) :
    ∃ s sd, T = .struct s ∧ D[s]? = some sd ∧ sd.fields[f]? = some Tf := by
  cases T with
  | int w sg => simp [Ty.fieldAt] at h
  | float w => simp [Ty.fieldAt] at h
  | bool => simp [Ty.fieldAt] at h
  | unit => simp [Ty.fieldAt] at h
  | struct s =>
      simp only [Ty.fieldAt] at h
      split at h
      · exact ⟨s, _, rfl, ‹_›, h⟩
      · cases h

mutual
/-- A fully-owned node holds a hole-free contents: `fully-owned(Σ, p)` (§5.1)
is exactly what says the aggregate a use hands on has no hole in it
(`3.8:26`) (helper). -/
theorem ContentsMatches.holeFree {D c t T} (h : ContentsMatches D c t T)
    (hf : t.fullyOwned = true) : c.holeFree = true := by
  cases h with
  | owned _ hh => exact hh
  | moved _ _ => simp [OwnSt.fullyOwned] at hf
  | fields hd hl =>
      show Contents.holeFreeList _ = true
      exact ContentsMatchesList.holeFreeList hl (by simpa only [OwnSt.fullyOwned] using hf)

/-- The same over a field list (helper). -/
theorem ContentsMatchesList.holeFreeList {D cs ts Ts} (h : ContentsMatchesList D cs ts Ts)
    (hf : OwnSt.fullyOwnedList ts = true) : Contents.holeFreeList cs = true := by
  cases h with
  | nil => rfl
  | @cons c cs ts T Ts hc hl =>
      cases ts with
      | nil =>
          show (Contents.holeFree c && Contents.holeFreeList cs) = true
          rw [ContentsMatches.holeFree hc rfl,
            ContentsMatchesList.holeFreeList hl (by rfl)]
          rfl
      | cons t ts' =>
          have hf' : OwnSt.fullyOwned t = true ∧ OwnSt.fullyOwnedList ts' = true := by
            simpa only [OwnSt.fullyOwnedList, Bool.and_eq_true] using hf
          show (Contents.holeFree c && Contents.holeFreeList cs) = true
          rw [ContentsMatches.holeFree hc
                (by simpa only [OwnSt.fieldAt, List.getElem?_cons_zero,
                      Option.getD_some] using hf'.1),
            ContentsMatchesList.holeFreeList hl (by simpa only [List.tail_cons] using hf'.2)]
          rfl
end

/-- The contents of a matched, fully-owned node is the value the machine hands
on, well typed at the node's type (helper). -/
theorem ContentsMatches.toVal {D c t T} (h : ContentsMatches D c t T)
    (hf : t.fullyOwned = true) : ∃ v, c.toVal = some v ∧ HasTy D v T :=
  h.contentsTy.toVal (h.holeFree hf)

/-- A matched node whose state is `Owned` is not itself a hole — which is what
lets `@drop` at a partially moved place run at all (helper). -/
theorem ContentsMatches.ne_hole {D c t T} (h : ContentsMatches D c t T)
    (ho : t.isOwned = true) : c ≠ .hole := by
  cases h with
  | owned _ hh => intro hc; rw [hc] at hh; simp [Contents.holeFree] at hh
  | moved _ _ => simp [OwnSt.isOwned] at ho
  | fields _ _ => simp

/-- A matched node whose state is `Owned` has its type's class, which is what
`dropCell`'s `Copy` test reads (helper). -/
theorem ContentsMatches.mult_eq {D c t T} (h : ContentsMatches D c t T)
    (ho : t.isOwned = true) : c.mult D = T.mult D := by
  cases h with
  | owned hc hh => exact hc.mult_eq hh
  | moved _ _ => simp [OwnSt.isOwned] at ho
  | fields hd _ => simp [Contents.mult, Ty.mult, StructEnv.classOf, hd]

/-- The contents of a value written into a cell matches the `owned` state
(§6.7's (D-Let), §6.8's store) (helper). -/
theorem ContentsMatches.ofVal {D v T} (h : HasTy D v T) :
    ContentsMatches D (Contents.ofVal v) .owned T :=
  .owned h.contentsTy (Contents.holeFree_ofVal v)

/-- A contents that is not `⊘` fails the `⊘` test (helper). -/
theorem Contents.isHole_eq_false : ∀ {c : Contents}, c ≠ .hole → c.isHole = false
  | .hole, h => absurd rfl h
  | .int _ _ _, _ => rfl
  | .float _ _, _ => rfl
  | .bool _, _ => rfl
  | .unit, _ => rfl
  | .struct _ _, _ => rfl

/-- A fully-owned node is an `Owned` one (helper). -/
theorem OwnSt.isOwned_of_fullyOwned : ∀ {t : OwnSt}, t.fullyOwned = true → t.isOwned = true
  | .owned, _ => rfl
  | .fields _, _ => rfl
  | .movedOut, h => by simp [OwnSt.fullyOwned] at h

/-- **Navigating a path agrees on the two sides of the invariant.** Where `Σ`
has a state for the path — which is where no proper prefix of it is `MovedOut`
(§5.1's `Owned-Base`, `3.8:53`) — the store's `H(ℓ)@π` (§6.3) reaches a
sub-position, and the two match at the path's declared type. This is what makes
every place rule's premises enough for its dynamic rule to fire. -/
theorem ContentsMatches.readAt {D : StructEnv} : ∀ (π : List Nat) {c : Contents}
    {t u : OwnSt} {T T' : Ty}, ContentsMatches D c t T → t.get π = some u →
    T.atPath D π = some T' → ∃ sub, c.readAt π = .ok sub ∧ ContentsMatches D sub u T'
  | [], c, t, u, T, T', hm, hg, hty => by
      simp only [OwnSt.get, Option.some.injEq] at hg
      simp only [Ty.atPath, Option.some.injEq] at hty
      subst hg; subst hty
      exact ⟨c, by simp [Contents.readAt], hm⟩
  | f :: π, c, t, u, T, T', hm, hg, hty => by
      cases hfa : T.fieldAt D f with
      | none => simp [Ty.atPath, hfa] at hty
      | some Tf =>
        simp only [Ty.atPath, hfa] at hty
        obtain ⟨s, sd, rfl, hd, hf⟩ := Ty.fieldAt_inv hfa
        cases hm with
        | owned hcty hhf =>
            obtain ⟨cs, rfl, hl⟩ := ContentsMatches.owned_struct hd (.owned hcty hhf)
            obtain ⟨cf, hcf, hmf⟩ := ContentsMatchesList.index f hl hf
            simp only [OwnSt.get] at hg
            simp only [Contents.readAt, hcf]
            exact ContentsMatches.readAt π
              (by simpa only [OwnSt.fieldAt, List.getElem?_nil, Option.getD_none] using hmf)
              hg hty
        | moved _ _ => simp [OwnSt.get] at hg
        | @fields s' sd' cs ts hd' hl =>
            have heq : sd' = sd := by rw [hd'] at hd; cases hd; rfl
            subst heq
            obtain ⟨cf, hcf, hmf⟩ := ContentsMatchesList.index f hl hf
            simp only [OwnSt.get] at hg
            simp only [Contents.readAt, hcf]
            exact ContentsMatches.readAt π hmf hg hty

/-- **Writing a sub-position and its Σ state together keeps the cell
matched.** (Use-Move) §6.3's `H[ℓ@π ↦ ⊘]`, `@drop`'s write-back (§6.11) and
(D-Assign)'s store (§6.8) are all this lemma, with a different pair written in
at the path. -/
theorem ContentsMatches.writeAt {D : StructEnv} : ∀ (π : List Nat) {c sub' : Contents}
    {t u u' : OwnSt} {T T' : Ty}, ContentsMatches D c t T → t.get π = some u →
    T.atPath D π = some T' → ContentsMatches D sub' u' T' →
    ∃ c', c.writeAt π sub' = some c' ∧ ContentsMatches D c' (t.setAt π u') T
  | [], c, sub', t, u, u', T, T', hm, hg, hty, hnew => by
      simp only [Ty.atPath, Option.some.injEq] at hty
      subst hty
      exact ⟨sub', by simp [Contents.writeAt], by simpa only [OwnSt.setAt] using hnew⟩
  | f :: π, c, sub', t, u, u', T, T', hm, hg, hty, hnew => by
      cases hfa : T.fieldAt D f with
      | none => simp [Ty.atPath, hfa] at hty
      | some Tf =>
        simp only [Ty.atPath, hfa] at hty
        obtain ⟨s, sd, rfl, hd, hf⟩ := Ty.fieldAt_inv hfa
        cases hm with
        | owned hcty hhf =>
            obtain ⟨cs, rfl, hl⟩ := ContentsMatches.owned_struct hd (.owned hcty hhf)
            obtain ⟨cf, hcf, hmf⟩ := ContentsMatchesList.index f hl hf
            simp only [OwnSt.get] at hg
            obtain ⟨cf', hw, hmf'⟩ := ContentsMatches.writeAt π
              (by simpa only [OwnSt.fieldAt, List.getElem?_nil, Option.getD_none] using hmf)
              hg hty hnew
            refine ⟨.struct s (cs.set f cf'), by simp only [Contents.writeAt, hcf, hw,
              Option.map_some], ?_⟩
            simp only [OwnSt.setAt]
            exact .fields hd (ContentsMatchesList.set f hl hf
              (by simpa only [OwnSt.fieldAt, List.getElem?_nil, Option.getD_none] using hmf'))
        | moved _ _ => simp [OwnSt.get] at hg
        | @fields s' sd' cs ts hd' hl =>
            have heq : sd' = sd := by rw [hd'] at hd; cases hd; rfl
            subst heq
            obtain ⟨cf, hcf, hmf⟩ := ContentsMatchesList.index f hl hf
            simp only [OwnSt.get] at hg
            obtain ⟨cf', hw, hmf'⟩ := ContentsMatches.writeAt π hmf hg hty hnew
            refine ⟨.struct s (cs.set f cf'), by simp only [Contents.writeAt, hcf, hw,
              Option.map_some], ?_⟩
            simp only [OwnSt.setAt]
            exact .fields hd' (ContentsMatchesList.set f hl hf hmf')

/-- A `⊘` matches a `MovedOut` state at every type: nothing is stored, so
nothing is claimed (helper). -/
theorem ContentsMatches.hole {D T} : ContentsMatches D (.hole : Contents) .movedOut T :=
  .moved .hole rfl

/-! ### §5.6's obligation, read on Σ and read on the store, agree -/

/-- §5.6's field disjunction, read at one slot (helper). -/
theorem residualLinearFields_false {D : StructEnv} : ∀ {ts : List OwnSt} {Ts : List Ty}
    (f : Nat) {Tf : Ty}, residualLinearFields D ts Ts = false → Ts[f]? = some Tf →
    residualLinear D (OwnSt.fieldAt ts f) Tf = false
  | [], Ts, f, Tf, h, hT => by
      have hmem : Tf ∈ Ts := List.mem_of_getElem? hT
      simp only [residualLinearFields, List.any_eq_false] at h
      simpa only [OwnSt.fieldAt, List.getElem?_nil, Option.getD_none, residualLinear,
        Bool.not_eq_true] using h Tf hmem
  | _ :: _, [], _, _, _, hT => by simp at hT
  | t :: ts, T :: Ts, 0, Tf, h, hT => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at hT
      subst hT
      simp only [residualLinearFields, Bool.or_eq_false_iff] at h
      simpa only [OwnSt.fieldAt, List.getElem?_cons_zero, Option.getD_some] using h.1
  | t :: ts, T :: Ts, (g + 1), Tf, h, hT => by
      simp only [List.getElem?_cons_succ] at hT
      simp only [residualLinearFields, Bool.or_eq_false_iff] at h
      simpa only [OwnSt.fieldAt_succ, List.tail_cons] using
        residualLinearFields_false (ts := ts) g h.2 hT

mutual
/-- **The machine's leak monitor sees exactly what §5.6 computes.** If Σ says
the residue at a path carries no linear value, the contents stored there holds
no live declared-`linear` sub-value — so `endscope` (§6.7), the frame teardown
(§6.9) and the overwrite (§6.8) all let it through. This is the clause that
makes the RUE-1591 model sound: after a partial move the obligation is the
residue's, on both sides of the invariant. -/
theorem ContentsMatches.residualLinear_false {D c t T} (hwf : WfStructs D)
    (h : ContentsMatches D c t T) (hr : residualLinear D t T = false) :
    c.residualLinear D = false := by
  cases h with
  | owned hc _ =>
      refine hc.residualLinear_false hwf ?_
      simpa only [residualLinear, decide_eq_false_iff_not] using hr
  | moved _ hnl => exact hnl
  | @fields s sd cs ts hd hl =>
      simp only [residualLinear, hd, Bool.or_eq_false_iff, decide_eq_false_iff_not] at hr
      simp only [Contents.residualLinear, hd, hr.1, Bool.false_or, decide_false]
      exact ContentsMatchesList.residualLinearList_false hwf hl hr.2

/-- The same over a field list (helper). -/
theorem ContentsMatchesList.residualLinearList_false {D cs ts Ts} (hwf : WfStructs D)
    (h : ContentsMatchesList D cs ts Ts) (hr : residualLinearFields D ts Ts = false) :
    Contents.residualLinearList D cs = false := by
  cases h with
  | nil => rfl
  | @cons c cs ts T Ts hc hl =>
      cases ts with
      | nil =>
          have hsplit : decide (Ty.mult D T = Mult.linear) = false ∧
              residualLinearFields D [] Ts = false := by
            simpa only [residualLinearFields, List.any_cons, Bool.or_eq_false_iff] using hr
          show (Contents.residualLinear D c || Contents.residualLinearList D cs) = false
          rw [ContentsMatches.residualLinear_false hwf hc
                (by simpa only [OwnSt.fieldAt, List.getElem?_nil, Option.getD_none,
                      residualLinear] using hsplit.1),
            ContentsMatchesList.residualLinearList_false hwf hl
                (by simpa only [List.tail_nil] using hsplit.2)]
          rfl
      | cons t ts' =>
          have hsplit : residualLinear D t T = false ∧
              residualLinearFields D ts' Ts = false := by
            simpa only [residualLinearFields, Bool.or_eq_false_iff] using hr
          show (Contents.residualLinear D c || Contents.residualLinearList D cs) = false
          rw [ContentsMatches.residualLinear_false hwf hc
                (by simpa only [OwnSt.fieldAt, List.getElem?_cons_zero,
                      Option.getD_some] using hsplit.1),
            ContentsMatchesList.residualLinearList_false hwf hl
                (by simpa only [List.tail_cons] using hsplit.2)]
          rfl
end

/-- Per-cell agreement between the static entry and the dynamic cell: §7's
"Σ faithfully tracks the store's initialization", with the §5.5 join's
asymmetry built into `ContentsMatches`. A retired (`†`) cell matches no entry
at all, which is what keeps the unwind off one. -/
def CellMatches (D : StructEnv) (cell : Cell) (en : Entry) : Prop :=
  ∃ c, cell = .full c ∧ ContentsMatches D c en.st en.ty

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

/-- **A cell whose Σ record carries no residual linear content is one
`drop-retire` can retire.** Both halves come off the invariant: the contents
are well typed (so §6.11's walk never refuses) and hold no live
declared-`linear` sub-value (so the leak monitor lets them through)
(helper). -/
theorem CellMatches.dropOk {D cell en} (hwf : WfStructs D) (hcm : CellMatches D cell en)
    (h : residualLinear D en.st en.ty = false) :
    ∃ c, cell = .full c ∧ ContentsTy D c en.ty ∧ c.residualLinear D = false := by
  obtain ⟨c, rfl, hm⟩ := hcm
  exact ⟨c, rfl, hm.contentsTy, hm.residualLinear_false hwf h⟩

/-- The drop of a well-typed cell's contents always runs (§6.11) (helper). -/
theorem dropCell_ok {D : StructEnv} {ℓ : Nat} {c : Contents} {T : Ty} (h : ContentsTy D c T) :
    ∃ evs, dropCell D ℓ c = .ok evs := by
  unfold dropCell
  by_cases hcp : c.mult D = .copy
  · exact ⟨[], by simp [hcp]⟩
  · obtain ⟨evs, hevs⟩ := dropContents_ok h
    exact ⟨.drop ℓ c :: evs, by simp [hcp, hevs]⟩

/-- `drop-retire` (§6.1) succeeds on such a cell, retiring it: the contents'
own drop (§6.11) runs — `dropContents_ok` is why it never refuses — and the
leak monitor lets it through because no live linear sub-value is left in it
(helper). -/
theorem dropRetire_ok {D : StructEnv} {H : Store} {ℓ : Nat} {cell : Cell} {c : Contents}
    {T : Ty} (hc : H[ℓ]? = some cell) (hcell : cell = .full c) (hty : ContentsTy D c T)
    (hnl : c.residualLinear D = false) :
    ∃ evs, dropRetire D H ℓ = .ok (H.set ℓ .dead, evs) := by
  unfold dropRetire
  rw [hc, hcell]
  obtain ⟨evs, hdc⟩ := dropCell_ok (ℓ := ℓ) hty
  exact ⟨evs, by simp only [hnl, Bool.false_eq_true, if_neg, hdc, not_false_eq_true]⟩

/-- **Scope teardown never refuses on a frame the statics cleared.**
`run-scope-drops` (§6.1) over a frame whose bindings carry no residual linear
content retires every one of them: none is already retired (`Matches` says
every bound cell is live and that no two bindings share one — §7's
no-use-after-drop at an unwinding edge), and none holds a live linear
sub-value (the §5.6 obligation, read on the residue). -/
theorem Matches.unwind {D : StructEnv} (hwf : WfStructs D) :
    ∀ (Γ : Ctx) (ρ : Env) (H : Store), Matches D Γ ρ H → NoResidualLinear D Γ →
    ∃ H' evs, unwindLocs D H ρ = .ok (H', evs) ∧ H'.length = H.length ∧
      ∀ ℓ, ℓ ∉ ρ → H'[ℓ]? = H[ℓ]?
  | [], _, H, hm, _ => by
      cases hm
      exact ⟨H, [], rfl, rfl, fun _ _ => rfl⟩
  | en :: Γ₀, _, H, hm, hnl => by
      cases hm with
      | @cons _ _ ℓ ρ₀ _ cell hc hcm hnin hrest =>
        have hhead : residualLinear D en.st en.ty = false := hnl en (by simp)
        have hnl₀ : NoResidualLinear D Γ₀ := fun e he => hnl e (List.mem_cons_of_mem _ he)
        obtain ⟨c, hcell, hty, hres⟩ := hcm.dropOk hwf hhead
        obtain ⟨evs₀, hdr⟩ := dropRetire_ok hc hcell hty hres
        obtain ⟨H', evs', hrec, hlen, hout⟩ :=
          Matches.unwind hwf Γ₀ ρ₀ (H.set ℓ .dead) (hrest.set_outside hnin) hnl₀
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
theorem runAllScopeDrops_ok {D Γ φ H} (hwf : WfStructs D) (hfm : FrameMatches D Γ φ H)
    (hnl : NoResidualLinear D Γ) :
    ∃ H' evs, runAllScopeDrops D H φ = .ok (H', evs) ∧ H'.length = H.length ∧
      ∀ ℓ, ℓ ∉ φ.env → H'[ℓ]? = H[ℓ]? := by
  unfold runAllScopeDrops
  rw [hfm.record]
  exact Matches.unwind hwf _ _ _ hfm.store hnl

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

mutual
/-- **Joining a wholly-`Owned` arm with `t` yields `t`, and the `Owned` arm's
contents still matches it.** The `Owned` side never adds a move, so the only
question the join asks is whether each path `t` has `MovedOut` may be lost —
which `ownedJoinOk` has answered, and which the invariant's asymmetric
`movedOut` clause then admits (`3.8:50`, `3.8:60`). -/
theorem ownedJoinOk_matches {D : StructEnv} (hwf : WfStructs D) :
    ∀ (t : OwnSt) {T : Ty} {c : Contents}, ownedJoinOk D t T = true →
      ContentsMatches D c .owned T → ContentsMatches D c t T
  | .owned, _, _, _, h => h
  | .movedOut, T, c, hok, h => by
      simp only [ownedJoinOk, decide_eq_true_eq] at hok
      exact .moved h.contentsTy (h.contentsTy.residualLinear_false hwf hok)
  | .fields ts, T, c, hok, h => by
      cases T with
      | int w sg => simp [ownedJoinOk] at hok
      | float w => simp [ownedJoinOk] at hok
      | bool => simp [ownedJoinOk] at hok
      | unit => simp [ownedJoinOk] at hok
      | struct s =>
        simp only [ownedJoinOk] at hok
        split at hok
        · rename_i sd hd
          obtain ⟨cs, rfl, hl⟩ := ContentsMatches.owned_struct hd h
          exact .fields hd (ownedJoinOkList_matches hwf ts hok hl)
        · simp at hok

/-- The same over a declaration's fields (helper). -/
theorem ownedJoinOkList_matches {D : StructEnv} (hwf : WfStructs D) :
    ∀ (ts : List OwnSt) {Ts : List Ty} {cs : List Contents}, ownedJoinOkList D ts Ts = true →
      ContentsMatchesList D cs [] Ts → ContentsMatchesList D cs ts Ts
  | [], _, _, _, h => h
  | _ :: _, [], _, _, h => by cases h; exact .nil
  | t :: ts, T :: Ts, cs, hok, h => by
      cases h with
      | @cons c cs' _ _ _ hhd htl =>
        simp only [ownedJoinOkList, Bool.and_eq_true] at hok
        refine .cons ?_ ?_
        · simpa only [OwnSt.fieldAt, List.getElem?_cons_zero, Option.getD_some] using
            ownedJoinOk_matches hwf t hok.1
              (by simpa only [OwnSt.fieldAt, List.getElem?_nil, Option.getD_none] using hhd)
        · simpa only [List.tail_cons] using ownedJoinOkList_matches hwf ts hok.2
            (by simpa only [List.tail_nil] using htl)
end

mutual
/-- **The §5.5 join weakens each arm's agreement.** A cell matching either
arm's state at a path matches the joined state: where the two arms agree the
join is that state, and where they disagree the join is `MovedOut`, which the
invariant's asymmetric clause admits because the disagreement premise has
already ruled out live linear content there (`3.8:50`). The machine then drops
whatever residue the taken path left, path-specifically (`3.8:60`). -/
theorem OwnSt.join_matches {D : StructEnv} (hwf : WfStructs D) :
    ∀ (a b : OwnSt) {e : OwnSt} {T : Ty} {c : Contents}, OwnSt.join D a b T = some e →
      (ContentsMatches D c a T ∨ ContentsMatches D c b T) → ContentsMatches D c e T
  | .owned, b, e, T, c, hj, hc => by
      simp only [OwnSt.join] at hj
      split at hj
      · cases hj
        rcases hc with h | h
        · exact ownedJoinOk_matches hwf b ‹_› h
        · exact h
      · cases hj
  | .movedOut, .owned, e, T, c, hj, hc => by
      simp only [OwnSt.join] at hj
      split at hj
      · cases hj
        rcases hc with h | h
        · exact h
        · exact ownedJoinOk_matches hwf .movedOut ‹_› h
      · cases hj
  | .fields as, .owned, e, T, c, hj, hc => by
      simp only [OwnSt.join] at hj
      split at hj
      · cases hj
        rcases hc with h | h
        · exact h
        · exact ownedJoinOk_matches hwf (.fields as) ‹_› h
      · cases hj
  | .movedOut, .movedOut, e, T, c, hj, hc => by
      simp only [OwnSt.join, residualLinear, Bool.false_eq_true, if_neg,
        not_false_eq_true, Option.some.injEq] at hj
      cases hj
      rcases hc with h | h <;> exact h
  | .movedOut, .fields bs, e, T, c, hj, hc => by
      simp only [OwnSt.join] at hj
      split at hj
      · cases hj
      · cases hj
        rcases hc with h | h
        · exact h
        · exact .moved h.contentsTy
            (h.residualLinear_false hwf ((Bool.not_eq_true _).mp ‹_›))
  | .fields as, .movedOut, e, T, c, hj, hc => by
      simp only [OwnSt.join] at hj
      split at hj
      · cases hj
      · cases hj
        rcases hc with h | h
        · exact .moved h.contentsTy
            (h.residualLinear_false hwf ((Bool.not_eq_true _).mp ‹_›))
        · exact h
  | .fields as, .fields bs, e, T, c, hj, hc => by
      cases T with
      | int w sg => simp [OwnSt.join] at hj
      | float w => simp [OwnSt.join] at hj
      | bool => simp [OwnSt.join] at hj
      | unit => simp [OwnSt.join] at hj
      | struct s =>
        simp only [OwnSt.join] at hj
        split at hj
        · rename_i sd hd
          cases hjl : OwnSt.joinList D as bs sd.fields with
          | none => rw [hjl] at hj; cases hj
          | some es =>
              rw [hjl] at hj
              simp only [Option.map_some, Option.some.injEq] at hj
              subst hj
              rcases hc with h | h
              · cases h with
                | @fields _ sd' cs _ hd' hl =>
                  have heq : sd' = sd := by rw [hd'] at hd; cases hd; rfl
                  subst heq
                  exact .fields hd' (OwnSt.joinList_matches hwf as bs sd'.fields hjl (Or.inl hl))
              · cases h with
                | @fields _ sd' cs _ hd' hl =>
                  have heq : sd' = sd := by rw [hd'] at hd; cases hd; rfl
                  subst heq
                  exact .fields hd' (OwnSt.joinList_matches hwf as bs sd'.fields hjl (Or.inr hl))
        · cases hj

/-- The same over a declaration's field slots (helper). -/
theorem OwnSt.joinList_matches {D : StructEnv} (hwf : WfStructs D) :
    ∀ (as bs : List OwnSt) (Ts : List Ty) {es : List OwnSt} {cs : List Contents},
      OwnSt.joinList D as bs Ts = some es →
      (ContentsMatchesList D cs as Ts ∨ ContentsMatchesList D cs bs Ts) →
      ContentsMatchesList D cs es Ts
  | as, bs, [], es, cs, hj, hc => by
      simp only [OwnSt.joinList, Option.some.injEq] at hj
      cases hj
      rcases hc with h | h <;> cases h <;> exact .nil
  | [], bs, T :: Ts, es, cs, hj, hc => by
      simp only [OwnSt.joinList] at hj
      split at hj
      · cases hj
        rcases hc with h | h
        · exact ownedJoinOkList_matches hwf bs ‹_› h
        · exact h
      · cases hj
  | a :: as, [], T :: Ts, es, cs, hj, hc => by
      simp only [OwnSt.joinList] at hj
      split at hj
      · cases hj
        rcases hc with h | h
        · exact h
        · exact ownedJoinOkList_matches hwf (a :: as) ‹_› h
      · cases hj
  | a :: as, b :: bs, T :: Ts, es, cs, hj, hc => by
      simp only [OwnSt.joinList] at hj
      split at hj
      · rename_i e rest he hrest
        cases hj
        rcases hc with h | h <;> cases h with
          | @cons c cs' _ _ _ hhd htl =>
            refine .cons ?_ ?_
            · simp only [OwnSt.fieldAt, List.getElem?_cons_zero, Option.getD_some]
              refine OwnSt.join_matches hwf a b he ?_
              first
                | exact Or.inl (by
                    simpa only [OwnSt.fieldAt, List.getElem?_cons_zero,
                      Option.getD_some] using hhd)
                | exact Or.inr (by
                    simpa only [OwnSt.fieldAt, List.getElem?_cons_zero,
                      Option.getD_some] using hhd)
            · simp only [List.tail_cons]
              refine OwnSt.joinList_matches hwf as bs Ts hrest ?_
              first
                | exact Or.inl (by simpa only [List.tail_cons] using htl)
                | exact Or.inr (by simpa only [List.tail_cons] using htl)
      · cases hj
end

/-- The §5.5 join weakens the left arm's per-cell agreement: a cell matching
the left entry matches the joined entry (the conservative join, whose residue
the machine drops path-specifically: `3.8:60` for a struct's fields, `3.8:73`
the array-element form). -/
theorem Entry.join_matches_left {D : StructEnv} (hwf : WfStructs D) {a b e' : Entry}
    (hj : a.join D b = some e') {cell : Cell} (hc : CellMatches D cell a) :
    CellMatches D cell e' := by
  unfold Entry.join at hj
  cases hju : OwnSt.join D a.st b.st a.ty with
  | none => rw [hju] at hj; cases hj
  | some u =>
      rw [hju] at hj
      simp only [Option.map_some, Option.some.injEq] at hj
      obtain ⟨c, rfl, hm⟩ := hc
      exact ⟨c, rfl, hj ▸ OwnSt.join_matches hwf a.st b.st hju (Or.inl hm)⟩

/-- The §5.5 join weakens the right arm's per-cell agreement, given the two
arms share a skeleton (the conservative join, whose residue the machine drops
path-specifically: `3.8:60` for a struct's fields, `3.8:73` the array-element
form). -/
theorem Entry.join_matches_right {D : StructEnv} (hwf : WfStructs D) {a b e' : Entry}
    (hskel : a.skel = b.skel) (hj : a.join D b = some e') {cell : Cell}
    (hc : CellMatches D cell b) : CellMatches D cell e' := by
  have hty : a.ty = b.ty := congrArg Prod.fst hskel
  unfold Entry.join at hj
  cases hju : OwnSt.join D a.st b.st a.ty with
  | none => rw [hju] at hj; cases hj
  | some u =>
      rw [hju] at hj
      simp only [Option.map_some, Option.some.injEq] at hj
      obtain ⟨c, rfl, hm⟩ := hc
      exact ⟨c, rfl, hj ▸ OwnSt.join_matches hwf a.st b.st hju (Or.inr (hty ▸ hm))⟩

/-- The invariant survives the §5.5 join from the left arm. -/
theorem Matches.join_left {D : StructEnv} (hwf : WfStructs D) :
    ∀ {Γ₁ Γ₂ Γ' : Ctx} {ρ H},
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
            exact .cons hc (Entry.join_matches_left hwf hje hcm) hnin (ih hjrest)
          · cases hj

/-- The invariant survives the §5.5 join from the right arm. -/
theorem Matches.join_right {D : StructEnv} (hwf : WfStructs D) :
    ∀ {Γ₁ Γ₂ Γ' : Ctx} {ρ H},
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
            exact .cons hc (Entry.join_matches_right hwf hskel.1 hje hcm) hnin
              (ih hskel.2 hjrest)
          · cases hj

/-! ## Minting a callee's parameters (§6.9's (D-Call)) -/

/-- Minting appends one cell per by-value argument and nothing else
(helper). -/
theorem mintParams_store : ∀ (H : Store) (vs : List Val),
    (mintParams H vs).1 = H ++ vs.map (fun v => Cell.full (Contents.ofVal v))
  | H, [] => by simp [mintParams]
  | H, v :: vs => by
      simp [mintParams, mintParams_store (H ++ [Cell.full (Contents.ofVal v)]) vs]

/-- Every parameter cell is minted above the caller's whole store, which is
what makes a call local to the caller's frame (helper). -/
theorem mintParams_fresh : ∀ (H : Store) (vs : List Val) (ℓ : Nat),
    ℓ ∈ (mintParams H vs).2 → H.length ≤ ℓ
  | _, [], _, h => by simp [mintParams] at h
  | H, v :: vs, ℓ, h => by
      simp only [mintParams] at h
      rcases List.mem_cons.mp h with rfl | h
      · exact Nat.le_refl _
      · have := mintParams_fresh (H ++ [Cell.full (Contents.ofVal v)]) vs ℓ h
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
          have ih := matches_mintParams (D := D) ps vs' (H ++ [Cell.full (Contents.ofVal v)]) hvs
          simp only [List.map_cons, List.reverse_cons, mintParams]
          refine Matches.snoc ih ?_ ⟨Contents.ofVal v, rfl, ContentsMatches.ofVal hv⟩ ?_
          · rw [mintParams_store]
            rw [List.getElem?_append_left (by simp)]
            simp
          · intro hmem
            have := mintParams_fresh (H ++ [Cell.full (Contents.ofVal v)]) vs' _
              (List.mem_reverse.mp hmem)
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
theorem args_sound (M : FloatModel) {P : Program} {fuel : Nat}
    (ih : ∀ {R : Ty} {Γ Γ' : Ctx} {e : Expr} {T : Ty}, Typed P R Γ e T Γ' →
      ∀ {φ : Frame} {H : Store}, FrameMatches P.structs Γ φ H →
        EvalOk P.structs T R Γ' φ H (eval M.toFloatOps fuel P H φ e)) :
    ∀ (es : List Expr) {R : Ty} {Γ Γ' : Ctx} {Ts : List Ty} {φ : Frame} {H : Store},
      TypedArgs P R Γ es Ts Γ' → FrameMatches P.structs Γ φ H →
        ArgsOk P.structs R Ts Γ' φ H (evalArgs (fun H' e => eval M.toFloatOps fuel P H' φ e) H es) := by
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
        cases hr : eval M.toFloatOps fuel P H φ e with
        | ok H₁ v tr =>
            rw [hr] at k₁
            obtain ⟨hty, hfm₁, hu₁⟩ := k₁
            have k₂ := ihes h₂ hfm₁
            dsimp only
            cases hr₂ : evalArgs (fun H' e => eval M.toFloatOps fuel P H' φ e) H₁ es with
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
theorem soundness (M : FloatModel) {P : Program} (hwf : WfProgram P) :
    ∀ (fuel : Nat) {R : Ty} {Γ Γ' : Ctx} {e : Expr} {T : Ty}, Typed P R Γ e T Γ' →
      ∀ {φ : Frame} {H : Store}, FrameMatches P.structs Γ φ H →
        EvalOk P.structs T R Γ' φ H (eval M.toFloatOps fuel P H φ e) := by
  intro fuel
  induction fuel with
  | zero =>
      intro R Γ Γ' e T ht φ H hfm
      simp only [eval]
      trivial
  | succ fuel ih =>
      have hargs := args_sound M ih
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
      | @useCopy Γ pl en u T hget hg hfo hty hcopy _ =>
          -- (D-Use-Copy) §6.3: navigate the path and hand the value on; the
          -- cell is left untouched.
          obtain ⟨ℓ, cell, hρ, hc, hcm⟩ := hfm.store.lookup hget
          obtain ⟨cc, rfl, hmm⟩ := hcm
          obtain ⟨sub, hread, hsub⟩ := ContentsMatches.readAt pl.path hmm hg hty
          obtain ⟨v, hv, htyv⟩ := hsub.toVal hfo
          have hvm : v.mult P.structs = .copy := by rw [htyv.mult_eq]; exact hcopy
          have hev : eval M.toFloatOps (fuel + 1) P H φ (.use pl) = .ok H v [] := by
            simp [eval, hρ, hc, hread, hv, hvm]
          rw [hev]
          exact ⟨htyv, hfm, Untouched.refl⟩
      | @useMove Γ pl en u T hget hg hfo hty hncopy _ _ =>
          -- (D-Use-Move) §6.3: read the sub-position, then write `⊘` at
          -- exactly it — the partial move of §4.2.
          obtain ⟨ℓ, cell, hρ, hc, hcm⟩ := hfm.store.lookup hget
          obtain ⟨cc, rfl, hmm⟩ := hcm
          obtain ⟨sub, hread, hsub⟩ := ContentsMatches.readAt pl.path hmm hg hty
          obtain ⟨v, hv, htyv⟩ := hsub.toVal hfo
          have hvm : v.mult P.structs ≠ .copy := by rw [htyv.mult_eq]; exact hncopy
          obtain ⟨cc', hw, hmm'⟩ :=
            ContentsMatches.writeAt pl.path hmm hg hty (ContentsMatches.hole (T := T))
          have hev : eval M.toFloatOps (fuel + 1) P H φ (.use pl) = .ok (H.set ℓ (.full cc')) v [] := by
            simp [eval, hρ, hc, hread, hv, hvm, hw]
          rw [hev]
          exact ⟨htyv, ⟨hfm.store.set hρ ⟨cc', rfl, hmm'⟩, hfm.record⟩,
            Untouched.trans_set Untouched.refl (Or.inr (List.mem_of_getElem? hρ))⟩
      | @binop Γ Γ₁ Γ₂ op e₁ e₂ w sg h₁ h₂ hop =>
          simp only [eval]
          refine EvalOk.bind (ih h₁ hfm) ?_
          intro H₁ v₁ tr₁ _ hty₁ hfm₁
          obtain ⟨n₁, rfl, _⟩ := hty₁.int_inv
          refine EvalOk.bind (ih h₂ hfm₁) ?_
          intro H₂ v₂ tr₂ _ hty₂ hfm₂
          obtain ⟨n₂, rfl, _⟩ := hty₂.int_inv
          rcases evalBinOp_res (D := P.structs) M.toFloatOps op w sg n₁ n₂ hop with
            ⟨v, hv, hty⟩ | ⟨k, hk⟩
          · rw [hv]; exact ⟨hty, hfm₂, Untouched.refl⟩
          · rw [hk]; trivial
      | @floatBinop Γ Γ₁ Γ₂ op e₁ e₂ w h₁ h₂ hop =>
          -- (Float-Arith)/(Float-Ord)/(Total-Cmp) §5.8 with §6.4's dynamics:
          -- the operands reduce left to right (§6.2) and the operator is
          -- total, so unlike the integer arm there is no trap branch at all
          -- (`3.12:21`).
          simp only [eval]
          refine EvalOk.bind (ih h₁ hfm) ?_
          intro H₁ v₁ tr₁ _ hty₁ hfm₁
          obtain ⟨f₁, rfl, hw₁⟩ := hty₁.float_inv
          refine EvalOk.bind (ih h₂ hfm₁) ?_
          intro H₂ v₂ tr₂ _ hty₂ hfm₂
          obtain ⟨f₂, rfl, hw₂⟩ := hty₂.float_inv
          obtain ⟨v, hv, hty⟩ := evalBinOpFloat_res (D := P.structs) M op w f₁ f₂ hw₁ hw₂ hop
          rw [hv]
          exact ⟨hty, hfm₂, Untouched.refl⟩
      | @neg Γ Γ' e w h =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          obtain ⟨n, rfl, _⟩ := hty.int_inv
          rcases evalUnOp_int_res (D := P.structs) .neg w .signed n (by simp) with
            ⟨v', hv, hty'⟩ | hk
          · rw [hv]; exact ⟨hty', hfm', Untouched.refl⟩
          · rw [hk]; trivial
      | @floatNeg Γ Γ' e w h =>
          -- (Float-Neg) §5.8 with (D-Float-Neg) §6.4: total, so there is no
          -- trap branch here at all (`3.12:24`).
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          obtain ⟨f, rfl, hwf'⟩ := hty.float_inv
          obtain ⟨v', hv, hty'⟩ := evalUnOp_float_res (D := P.structs) w f hwf'
          rw [hv]
          exact ⟨hty', hfm', Untouched.refl⟩
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
      | @floatLit Γ w l _ =>
          -- (Lit) at `float(w)`: `3.12:9` rounds the literal's decimal into
          -- `𝔽_w`, and `ofLit_wf` is the closure law that says so.
          simp only [eval]
          exact ⟨.float (M.ofLit_wf w l.sig l.negExp l.e), hfm, Untouched.refl⟩
      | @intToFloat Γ Γ' w w' s' e h =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          obtain ⟨n, rfl, _⟩ := hty.int_inv
          obtain ⟨v', hv, hty'⟩ := evalFintrin_int_res (D := P.structs) M w w' s' n
          rw [hv]
          exact ⟨hty', hfm', Untouched.refl⟩
      | @floatIntrin Γ Γ' k w e h hk =>
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H' v tr _ hty hfm'
          obtain ⟨f, rfl, hwf'⟩ := hty.float_inv
          rcases evalFintrin_float_res (D := P.structs) M k w f hwf' hk with
            ⟨v', hv, hty'⟩ | hk'
          · rw [hv]; exact ⟨hty', hfm', Untouched.refl⟩
          · rw [hk']; trivial
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
          cases hra : evalArgs (fun H' e => eval M.toFloatOps fuel P H' φ e) H args with
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
      | @dropCopy Γ pl en u T hget hg hfo hty hcopy _ =>
          obtain ⟨ℓ, cell, hρ, hc, hcm⟩ := hfm.store.lookup hget
          obtain ⟨cc, rfl, hmm⟩ := hcm
          obtain ⟨sub, hread, hsub⟩ := ContentsMatches.readAt pl.path hmm hg hty
          have hnh : sub.isHole = false :=
            Contents.isHole_eq_false (hsub.ne_hole (OwnSt.isOwned_of_fullyOwned hfo))
          have hvm : sub.mult P.structs = .copy := by
            rw [hsub.mult_eq (OwnSt.isOwned_of_fullyOwned hfo)]; exact hcopy
          have hev : eval M.toFloatOps (fuel + 1) P H φ (.drop pl) = .ok H .unit [] := by
            simp [eval, hρ, hc, hread, hnh, hvm, dropCell]
          rw [hev]
          exact ⟨.unit, hfm, Untouched.refl⟩
      | @dropRes Γ pl en u T hget hg ho hty hncopy _ _ _ =>
          -- §6.11's explicit `@drop`: the walk skips every `⊘` already under
          -- the place, and the place itself becomes `⊘`.
          obtain ⟨ℓ, cell, hρ, hc, hcm⟩ := hfm.store.lookup hget
          obtain ⟨cc, rfl, hmm⟩ := hcm
          obtain ⟨sub, hread, hsub⟩ := ContentsMatches.readAt pl.path hmm hg hty
          have hnh : sub.isHole = false := Contents.isHole_eq_false (hsub.ne_hole ho)
          have hvm : sub.mult P.structs ≠ .copy := by rw [hsub.mult_eq ho]; exact hncopy
          obtain ⟨evs, hdc⟩ := dropCell_ok (D := P.structs) (ℓ := ℓ) hsub.contentsTy
          obtain ⟨cc', hw, hmm'⟩ :=
            ContentsMatches.writeAt pl.path hmm hg hty (ContentsMatches.hole (T := T))
          have hev : eval M.toFloatOps (fuel + 1) P H φ (.drop pl)
              = .ok (H.set ℓ (.full cc')) .unit evs := by
            simp [eval, hρ, hc, hread, hnh, hvm, hdc, hw]
          rw [hev]
          exact ⟨.unit, ⟨hfm.store.set hρ ⟨cc', rfl, hmm'⟩, hfm.record⟩,
            Untouched.trans_set Untouched.refl (Or.inr (List.mem_of_getElem? hρ))⟩
      | @letIn Γ Γ₁ Γ₂ m e₁ e₂ T₁ T₂ en' h₁ h₂ hres =>
          simp only [eval]
          refine EvalOk.bind (ih h₁ hfm) ?_
          intro H₁ v₁ tr₁ _ hty₁ hfm₁
          have hfresh : H₁.length ∉ φ.env := hfm₁.store.fresh_not_mem
          have hfm' : FrameMatches P.structs ({ ty := T₁, mu := m, st := .owned } :: Γ₁)
              { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] }
              (H₁ ++ [.full (Contents.ofVal v₁)]) := by
            constructor
            · refine .cons ?_ ⟨Contents.ofVal v₁, rfl, ContentsMatches.ofVal hty₁⟩ hfresh
                (hfm₁.store.append _)
              simp
            · simp [hfm₁.record]
          have kb := ih h₂ hfm'
          have hty_en' : en'.ty = T₁ := by
            have hskel := h₂.skel_preserved
            simp only [Ctx.skel, List.map_cons, List.cons.injEq] at hskel
            exact congrArg Prod.fst hskel.1
          cases hrb : eval M.toFloatOps fuel P (H₁ ++ [.full (Contents.ofVal v₁)])
              { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] } e₂ with
          | ok H₂ v₂ tr₂ =>
              rw [hrb] at kb
              obtain ⟨hty₂, hfm₂, hu₂⟩ := kb
              obtain ⟨c, hc, hcm, hnin, hrest⟩ := hfm₂.store.cons_inv
              have hdrop : residualLinear P.structs en'.st en'.ty = false := hres
              obtain ⟨c', hcell, hty', hres'⟩ := hcm.dropOk hwf.structs hdrop
              obtain ⟨evs, hdr⟩ := dropRetire_ok hc hcell hty' hres'
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
      | @assign Γ Γ₁ pl e en₀ en₁ u₀ u₁ T hget₀ hmut hg₀ hty₀ h hget₁ hg₁ hover =>
          -- (D-Assign) §6.8 at a sub-position: drop what is live there (a `⊘`
          -- drops nothing — reinitialization, `3.8:55`), then store.
          simp only [eval]
          refine EvalOk.bind (ih h hfm) ?_
          intro H₁ v tr _ hty hfm₁
          obtain ⟨ℓ, cell, hρ, hc, hcm⟩ := hfm₁.store.lookup hget₁
          obtain ⟨cc, rfl, hmm⟩ := hcm
          have hskel := h.skel_preserved
          have htyeq : en₁.ty = en₀.ty := (skel_lookup hskel hget₀ hget₁).1
          obtain ⟨old, hread, hold⟩ :=
            ContentsMatches.readAt pl.path hmm hg₁ (htyeq ▸ hty₀)
          -- §5.2's premise is the type-keyed disjunction; either disjunct gives
          -- the residue the overwrite-drop is about to walk no linear content,
          -- which is why `linearOverwrite` is unreachable from a typed program.
          have hnl : old.residualLinear P.structs = false := by
            rcases hover with rfl | hnlin
            · exact hold.residualLinear_false hwf.structs rfl
            · exact ContentsTy.residualLinear_false hwf.structs hold.contentsTy hnlin
          obtain ⟨evs, hdc⟩ := dropCell_ok (D := P.structs) (ℓ := ℓ) hold.contentsTy
          obtain ⟨cc', hw, hmm'⟩ := ContentsMatches.writeAt pl.path hmm hg₁
            (htyeq ▸ hty₀) (ContentsMatches.ofVal hty)
          have hmem : ℓ ∈ φ.env := List.mem_of_getElem? hρ
          simp only [hρ, hc, hread, hnl, Bool.false_eq_true, if_neg, hdc, hw,
            not_false_eq_true]
          exact ⟨.unit, ⟨hfm₁.store.set hρ ⟨cc', rfl, hmm'⟩, hfm₁.record⟩,
            Untouched.trans_set Untouched.refl (Or.inr hmem)⟩
      | @seq Γ Γ₁ Γ₂ e₁ e₂ T₁ T₂ h₁ hnl h₂ =>
          simp only [eval]
          refine EvalOk.bind (ih h₁ hfm) ?_
          intro H₁ v₁ tr₁ _ hty₁ hfm₁
          have hvnl : v₁.mult P.structs ≠ .linear := by rw [hty₁.mult_eq]; exact hnl
          cases hml : v₁.mult P.structs with
          | linear => exact absurd hml hvnl
          | affine =>
              obtain ⟨evs, hevs⟩ := dropContents_ok (D := P.structs) hty₁.contentsTy
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
                (fun H' hf => ⟨Matches.join_left hwf.structs hjoin hf.store, hf.record⟩)
                (ih h₁ hfm₀)
          | false =>
              dsimp only
              exact EvalOk.weaken
                (fun H' hf =>
                  ⟨Matches.join_right hwf.structs hskel12 hjoin hf.store, hf.record⟩)
                (ih h₂ hfm₀)
      | @call Γ Γ' f args fd hget hta =>
          simp only [eval]
          have ka := hargs args hta hfm
          cases hra : evalArgs (fun H' e => eval M.toFloatOps fuel P H' φ e) H args with
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
              cases hrb : eval M.toFloatOps fuel P (mintParams H₁ vs).1
                  { env := (mintParams H₁ vs).2.reverse, scope := (mintParams H₁ vs).2 }
                  fd.body with
              | ok H₃ v tr₃ =>
                  rw [hrb] at kb
                  obtain ⟨htyv, hfm₃, hu₃⟩ := kb
                  obtain ⟨H₄, evs, hrun, hlen4, hout4⟩ :=
                    runAllScopeDrops_ok hwf.structs hfm₃ hnlf
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
          obtain ⟨H₂, evs, hrun, hlen, hout⟩ := runAllScopeDrops_ok hwf.structs hfm₁ hnl
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
theorem eval_succ (M : FloatOps) {P : Program} : ∀ (fuel : Nat) (H : Store) (φ : Frame) (e : Expr),
    eval M fuel P H φ e ≠ .outOfFuel → eval M (fuel + 1) P H φ e = eval M fuel P H φ e := by
  intro fuel
  induction fuel with
  | zero => intro H φ e h; simp only [eval] at h; exact absurd rfl h
  | succ n ih =>
      intro H φ e h
      cases e with
      | intLit w sg m => rfl
      | floatLit w l => rfl
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
      | fintrin k e₁ =>
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
          have hargs : evalArgs (fun H' e' => eval M n P H' φ e') H args ≠ .abort .outOfFuel := by
            intro hc
            simp only [eval, hc] at h
            exact h rfl
          have heq := evalArgs_mono (fun H' e' hne => ih H' φ e' hne) H args hargs
          simp only [eval, heq]
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
              cases hdv : dropContents P.structs (Contents.ofVal v) with
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
          | float w f => rfl
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
          have hargs : evalArgs (fun H' e' => eval M n P H' φ e') H args ≠ .abort .outOfFuel := by
            intro hc
            simp only [eval, hc] at h
            exact h rfl
          have heq := evalArgs_mono (fun H' e' hne => ih H' φ e' hne) H args hargs
          simp only [eval, heq] at h ⊢
          cases hra : evalArgs (fun H' e' => eval M n P H' φ e') H args with
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
                    have h' : (eval M n P (mintParams H₁ vs).1
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
theorem fuel_mono (M : FloatOps) {P : Program} {H : Store} {φ : Frame} {e : Expr} :
    ∀ {n m : Nat}, n ≤ m → eval M n P H φ e ≠ .outOfFuel →
      eval M m P H φ e = eval M n P H φ e := by
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
        rw [eval_succ M m H φ e (by rw [hrec]; exact hne), hrec]

/-- **No masking.** A fuel bound that reached a violation cannot be traded for
one that hides it: at every bound that answers at all, the answer is that same
violation. So no choice of fuel turns a violation into exhaustion for a
program some fuel completes, and the `outOfFuel` escape hatch in the §7
theorems (§6's machine has no such state) cannot be what makes them true. -/
theorem no_masking (M : FloatOps) {P : Program} {H : Store} {φ : Frame} {e : Expr} {n m : Nat}
    {w : Violation} (hn : eval M n P H φ e = .stuck w) (hm : eval M m P H φ e ≠ .outOfFuel) :
    eval M m P H φ e = .stuck w := by
  rcases Nat.le_total n m with hle | hle
  · rw [fuel_mono M hle (by rw [hn]; simp)]; exact hn
  · rw [← fuel_mono M hle hm]; exact hn

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
theorem run_ne_returned (M : FloatOps) {P : Program} {fuel : Nat} :
    ∀ H v tr, run M P fuel ≠ .returned H v tr := by
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
theorem run_safe (M : FloatModel) {P : Program} {fd : FnDef} (hwf : WfProgram P)
    (h0 : P.fns[0]? = some fd) (hp : fd.params = []) (fuel : Nat) :
    run M.toFloatOps P fuel = .outOfFuel ∨ (∃ k tr, run M.toFloatOps P fuel = .panic k tr) ∨
      (∃ H v tr, run M.toFloatOps P fuel = .ok H v tr ∧ HasTy P.structs v fd.ret) := by
  have hok : EvalOk P.structs fd.ret fd.ret [] { env := [], scope := [] } [] (run M.toFloatOps P fuel) :=
    soundness M hwf fuel (entry_typed h0 hp fd.ret) frameMatches_empty
  cases hr : run M.toFloatOps P fuel with
  | ok H v tr =>
      rw [hr] at hok
      exact Or.inr (Or.inr ⟨H, v, tr, rfl, hok.1⟩)
  | returned H v tr => exact absurd hr (run_ne_returned M.toFloatOps H v tr)
  | panic k tr => exact Or.inr (Or.inl ⟨k, tr, rfl⟩)
  | stuck w => rw [hr] at hok; exact hok.elim
  | outOfFuel => exact Or.inl rfl

/-- The same, from the packaged well-formedness of a whole program: §7 over
`ProgramTyped`, which is what `checkProgram` decides. The entry function is
existentially quantified because `ProgramTyped` only says one exists; the
value's type is still the one that function declares, so this form claims
exactly what `run_safe` proves. -/
theorem ProgramTyped.run_safe (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    ∃ fd, P.fns[0]? = some fd ∧
      (run M.toFloatOps P fuel = .outOfFuel ∨ (∃ k tr, run M.toFloatOps P fuel = .panic k tr) ∨
        (∃ H v tr, run M.toFloatOps P fuel = .ok H v tr ∧ HasTy P.structs v fd.ret)) := by
  obtain ⟨fd, h0, hp⟩ := h.entry
  refine ⟨fd, h0, ?_⟩
  rcases RueCore.run_safe M h.wf h0 hp fuel with h₁ | ⟨k, trk, h₂⟩ | ⟨H, v, tr, h₃, hty⟩
  · exact Or.inl h₁
  · exact Or.inr (Or.inl ⟨k, trk, h₂⟩)
  · exact Or.inr (Or.inr ⟨H, v, tr, h₃, hty⟩)

/-! ## §7 corollaries, named -/

/-- A well-formed program never reaches any of the machine's **named**
violations, at any fuel.

Read as "§7's bullets, conjoined", this would overstate the linear bullet by
**two** edges, on both of which a linear value is consumed zero times without
this theorem noticing. They are different in kind: the first is a gap in the
calculus, the second is the calculus doing what it says.

* **A pending argument (open).** A by-value argument value that a *later*
  argument of the same call destroys by `return` is in no cell and no scope
  record, so its drop is neither run nor monitored and none of the five
  violations fires. That edge is the calculus as written — §6.9's unwinding
  rule walks only σ, and §5.7's strict-context bottom rule (`Strict-Bottom`
  there, which the fragment does not mechanize) imposes no discard check on
  siblings already evaluated — it is what the Rue compiler does, and closing
  it is an open spec decision (RUE-2316, the pending-argument decision).
  `Dynamics.lean`'s "Pending arguments" section states it in full and
  `Examples.lean`'s `linearLostAtCallArg` is the kernel-checked witness.
* **A `@panic` (by design).** §6.12 abandons the configuration, and §5.7
  exempts the `⊥_panic` edge from §5.6's obligation, so a trap runs no scope
  drop at all: a live linear binding at a `@panic` is destroyed with no
  violation and an empty trace. That is not a gap — it is what (Panic) says,
  and the Rue compiler agrees (`Examples.lean`'s `panicPastLinear`, whose
  derivation, rejection by `check` and run are all pinned). A `@panic`
  *sibling* of a pending argument reaches the identical state by the second
  route as well as the first.

Every *other* edge — a `let`'s scope exit, a frame's normal pop, and a
`return`'s unwind — is covered. -/
theorem no_violation (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) (w : Violation) :
    run M.toFloatOps P fuel ≠ .stuck w := by
  obtain ⟨_, _, h₁ | ⟨k, trk, h₂⟩ | ⟨H, v, tr, h₃, _⟩⟩ := h.run_safe M fuel
  · rw [h₁]; simp
  · rw [h₂]; simp
  · rw [h₃]; simp

/-- §7 "No use-after-move": the machine never reads a `⊘` cell. -/
theorem no_use_after_move (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run M.toFloatOps P fuel ≠ .stuck .useAfterMove := no_violation M h fuel _

/-- §7 "No use-after-drop": the machine never touches a retired (`†`) cell.
With frames, this is a consequence of the invariant rather than a structural
fact about closed expressions: `run-all-scope-drops` (§6.9) walks the frame's
scope record at every `return` and at every frame pop, and it is
`FrameMatches` — the record is the environment, whose cells `Matches` says are
live or moved out and pairwise distinct — that keeps those walks off a `†`
cell and stops any cell being retired twice. -/
theorem no_use_after_drop (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run M.toFloatOps P fuel ≠ .stuck .useAfterDrop := no_violation M h fuel _

/-- §7 "Linear values are consumed exactly once", leak half: neither a scope
exit (§6.7) nor a frame unwind (§6.9) ever sees a live linear value. -/
theorem no_linear_leak (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run M.toFloatOps P fuel ≠ .stuck .linearLeak := no_violation M h fuel _

/-- §7 linear bullet, overwrite half (`3.8:77`, the RUE-387 premise). The
monitor reads the residue the overwrite-drop is about to walk, and (Assign)
§5.2's premise is keyed on the destination's *type*, which is the stronger of
the two — so the residue is empty of linear content whenever the checker
accepted. -/
theorem no_linear_overwrite (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run M.toFloatOps P fuel ≠ .stuck .linearOverwrite := no_violation M h fuel _

/-- §7 linear bullet, discard half (`3.8:64`). -/
theorem no_linear_discard (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    run M.toFloatOps P fuel ≠ .stuck .linearDiscard := no_violation M h fuel _

end RueCore
