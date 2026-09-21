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
frame-wide, so `Typed.ret` demands `NoResidualLinear`. `@panic` carries
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

/-! ## The fused `Γ ; Σ` context, keyed by path -/

/-- `Σ`'s state for one binding, as a tree over the paths under it (§5
preamble: `Σ : Path ⇀ { Owned, MovedOut }`, where a path is *absent* once a
prefix of it is `MovedOut`).

* `owned` — this path and every path under it is `Owned`.
* `movedOut` — this path is `MovedOut`; every path strictly under it is absent,
  which is what (Use-Move) §5.1's "every path strictly under `p` removed" does.
* `fields ts` — this path is `Owned` and its fields carry the states `ts`,
  which is the state a **partial move** leaves (`3.8:22`). A field beyond
  `ts`'s length is `owned`, so a partial move records only the fields it
  touched.

`Uninit` is still absence: bindings are initialized at their `let`, so the
fragment never observes it at a root. -/
inductive OwnSt where
  | owned
  | movedOut
  | fields (ts : List OwnSt)
deriving Repr

/-- The recorded states of a node's fields; a node with no record of its own
has none, and every field of it is `owned` (helper). -/
def OwnSt.fieldStates : OwnSt → List OwnSt
  | .fields ts => ts
  | .owned | .movedOut => []

/-- The state recorded for a field slot, defaulting to `owned` for a slot no
partial move has touched (helper). -/
def OwnSt.fieldAt (ts : List OwnSt) (f : Nat) : OwnSt := (ts[f]?).getD .owned

/-- Write a field slot's state, padding with `owned` for the untouched slots
before it (helper). -/
def OwnSt.setField : List OwnSt → Nat → OwnSt → List OwnSt
  | [], 0, u => [u]
  | [], f + 1, u => .owned :: OwnSt.setField [] f u
  | _ :: ts, 0, u => u :: ts
  | t :: ts, f + 1, u => t :: OwnSt.setField ts f u

/-- `Σ(p)` for a path under this binding: `some` the state recorded there, and
`none` exactly when a **proper prefix** of the path is `MovedOut` — §5's
absence. So the lookup *is* (Owned-Base) §5.1 (`3.8:53`: the base of a
projection must currently own its storage, in any context), and every rule that
names a place gets that side condition for free by asking for a state at all.
The compiler reports the failure as E0205. -/
def OwnSt.get : OwnSt → List Nat → Option OwnSt
  | t, [] => some t
  | .owned, _ :: π => OwnSt.get .owned π
  | .movedOut, _ :: _ => none
  | .fields ts, f :: π => OwnSt.get (OwnSt.fieldAt ts f) π

/-- `Σ[ p ↦ u, and every path strictly under p removed ]` (§5.1, §5.2, §5.3):
write a state at a path, expanding the nodes above it into field records as it
goes. A path under a `MovedOut` prefix is not a path `Σ` has, and the rules
that write only ever do so at a path their own `get` premise found. -/
def OwnSt.setAt : OwnSt → List Nat → OwnSt → OwnSt
  | _, [], u => u
  | .movedOut, _ :: _, _ => .movedOut
  | .owned, f :: π, u => .fields (OwnSt.setField [] f (OwnSt.setAt .owned π u))
  | .fields ts, f :: π, u =>
      .fields (OwnSt.setField ts f (OwnSt.setAt (OwnSt.fieldAt ts f) π u))

/-- `Σ(p) = Owned` (§5 preamble): the node itself owns its storage, whether or
not a path under it has been moved out. This is (Use-Copy) §5.1's and (@Drop)
§5.3's strength (helper). -/
def OwnSt.isOwned : OwnSt → Bool
  | .movedOut => false
  | .owned | .fields _ => true

mutual
/-- `fully-owned(Σ, p)` (§5 preamble): `Σ(p) = Owned` **and** no path strictly
under `p` is `MovedOut` — the no-use-after-move premise strengthened to the
whole subtree (`3.8:5/24/26/53`), which is what (Use-Move) §5.1 demands because
it hands the aggregate to a new owner (`3.8:26`, the compiler's E0205 "use of
partially moved value"). -/
def OwnSt.fullyOwned : OwnSt → Bool
  | .owned => true
  | .movedOut => false
  | .fields ts => OwnSt.fullyOwnedList ts

/-- `fully-owned` over a node's recorded field states; an untouched slot is
`owned` and contributes nothing (helper). -/
def OwnSt.fullyOwnedList : List OwnSt → Bool
  | [] => true
  | t :: ts => OwnSt.fullyOwned t && OwnSt.fullyOwnedList ts
end

mutual
/-- `residual-linear(Σ, p, T)` (§5.6), on the state recorded at `p` and its
declared type.

* a `MovedOut` path carries nothing (`Σ(p) = MovedOut ⇒ false`);
* a path that is wholly `Owned` carries a linear value exactly when
  `class(T) = Linear`, because §3's class *is* the join that reaches `Linear`
  through a declared-`linear` struct at some depth (`struct_carriesLinear_iff`)
  — so the type-level test is the fixed point of §5.6's own recursion on a
  subtree with no holes in it;
* a **declared**-`linear` struct still `Owned` carries the obligation itself,
  whatever its fields do (`3.8:74`; `3.8:75`'s empty `linear struct MustUse` is
  the motivating case);
* otherwise the obligation is the disjunction over the fields.

Keying the leak check on the residual *state* rather than on the binding's type
is the RUE-1591 model §5.6 states: after a partial move the obligation attaches
to whatever linear content is still present, so consuming exactly the linear
part of an infectious carrier and letting the rest drop is legal. -/
def residualLinear (D : StructEnv) : OwnSt → Ty → Bool
  | .movedOut, _ => false
  | .owned, T => decide (T.mult D = .linear)
  | .fields ts, .struct s =>
      (match D[s]? with
       | some sd => sd.attr = .linear || residualLinearFields D ts sd.fields
       | none => false)
  | .fields _, _ => false

/-- §5.6's field disjunction: a field slot no partial move touched is `owned`,
so its clause is the type-level test (helper). -/
def residualLinearFields (D : StructEnv) : List OwnSt → List Ty → Bool
  | [], Ts => Ts.any fun T => decide (T.mult D = .linear)
  | _ :: _, [] => false
  | t :: ts, T :: Ts => residualLinear D t T || residualLinearFields D ts Ts
end

/-- §5.6's obligation read over the paths **strictly under** `p`: (@Drop)
§5.3's last premise, "if `p` has a `MovedOut` descendant, no still-owned linear
sub-place remains below `p`". `@drop(p)` discharges `p`'s own obligation
(`3.9:39`), so the root's declared linearity is deliberately not read here;
what it may not do is silently destroy a linear sub-place that a partial move
has separated from it. Verified against the compiler: `@drop(v.x1)` then
`@drop(v)` on a carrier whose `x0` is a live linear field is E0406. -/
def residualLinearBelow (D : StructEnv) : OwnSt → Ty → Bool
  | .movedOut, _ => false
  | t, .struct s =>
      (match D[s]? with
       | some sd => residualLinearFields D t.fieldStates sd.fields
       | none => false)
  | _, _ => false

/-- One context entry: the binding's declared type and `μ` mark (fixed at the
binder: `Γ`'s part) plus the ownership state of every path under it
(flow-sensitive: `Σ`'s part), one row of §5's fused `Γ ; Σ`. -/
structure Entry where
  ty : Ty
  mu : Bool
  st : OwnSt
deriving Repr

/-- Re-mark an entry's ownership state (helper). -/
def Entry.setSt (en : Entry) (s : OwnSt) : Entry := { en with st := s }

/-- The fused `Γ ; Σ` context of the judgment `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` (§5),
innermost binding first (de Bruijn). -/
abbrev Ctx := List Entry

/-- The fixed part of an entry, preserved by every rule (helper). -/
def Entry.skel (en : Entry) : Ty × Bool := (en.ty, en.mu)

/-- The skeleton of a whole context (helper). -/
def Ctx.skel (Γ : Ctx) : List (Ty × Bool) := Γ.map Entry.skel

/-- §5.6's residual-linear condition, read over a whole frame: no binding has
residual linear content left. This is the premise (Fn) §5.8 imposes on a
function body's exit edges for its by-value parameters (`3.8:62`) and that
§5.6's `⊥_exit` carries at an early `return`: at such an edge every open scope
of the frame ends at once, so the check is frame-wide rather than
per-binding. -/
def NoResidualLinear (D : StructEnv) (Γ : Ctx) : Prop :=
  ∀ en ∈ Γ, residualLinear D en.st en.ty = false

instance (D : StructEnv) (Γ : Ctx) : Decidable (NoResidualLinear D Γ) := by
  unfold NoResidualLinear; infer_instance

/-- (Fn) §5.8's entry context `Γ0;Σ0`: every by-value parameter enters the
body `Owned` and subject to the ordinary use/drop rules. The list is reversed
because `Ctx` is innermost-binder-first while a signature lists parameters
left to right, so the first parameter is the outermost binder — which is what
gives it de Bruijn index `m-1` and the printed name `v0`. -/
def fnCtx (fd : FnDef) : Ctx :=
  (fd.params.map fun p => { ty := p.ty, mu := p.mu, st := .owned }).reverse

/-! ### The §5.5 join

Agreeing nodes join to themselves. Where the two branches disagree — one has
the path `MovedOut` and the other `Owned` — the join is ill-formed exactly when
the `Owned` side still has **residual linear content** there, and is `MovedOut`
otherwise: the conservative reading, which the machine then makes good on by
dropping the residue path-specifically (`3.8:73`). Two `Owned` nodes with
different partial moves under them join field by field.

§5.5 writes the disagreement test as `carries_linear(T)` — on the binding's
*type*. That over-rejects for the same reason §5.6 abandoned the type-level
test (RUE-526, RUE-1591), and the compiler already uses the residual reading:
`if c { @drop(v) } else { @drop(v.x0) }` on a carrier whose only linear content
is `x0` is accepted, although `v` is a linear-carrying place that is `MovedOut`
on one branch and `Owned` on the other. On whole bindings with no partial move
the two readings coincide, so nothing §5.5 accepted becomes rejected.

One arm being wholly `Owned` is the case worth naming, because it is what makes
the join computable by structural recursion: joining `Owned` with `t` at every
path under `p` yields `t` itself — the `Owned` side never *adds* a move — so
the only question is whether `t`'s moves are admissible, which is what
`ownedJoinOk` decides.
-/

mutual
/-- Whether joining a wholly-`Owned` arm with `t` is well-formed: every path
`t` has `MovedOut` must be one the `Owned` side may lose, which by §5.6 read on
an `Owned` subtree is `class(T) ≠ Linear` at that path (`3.8:50`). -/
def ownedJoinOk (D : StructEnv) : OwnSt → Ty → Bool
  | .owned, _ => true
  | .movedOut, T => decide (T.mult D ≠ .linear)
  | .fields ts, .struct s =>
      (match D[s]? with
       | some sd => ownedJoinOkList D ts sd.fields
       | none => false)
  | .fields _, _ => false

/-- The same over a declaration's fields; a slot no partial move touched is
`owned` and always admissible (helper). -/
def ownedJoinOkList (D : StructEnv) : List OwnSt → List Ty → Bool
  | [], _ => true
  | _ :: _, [] => true
  | t :: ts, T :: Ts => ownedJoinOk D t T && ownedJoinOkList D ts Ts
end

mutual
/-- The §5.5 branch join, at one path and its subtree (section docstring). -/
def OwnSt.join (D : StructEnv) : OwnSt → OwnSt → Ty → Option OwnSt
  | .owned, b, T => if ownedJoinOk D b T then some b else none
  | a, .owned, T => if ownedJoinOk D a T then some a else none
  | .movedOut, b, T => if residualLinear D b T then none else some .movedOut
  | a, .movedOut, T => if residualLinear D a T then none else some .movedOut
  | .fields as, .fields bs, T =>
      (match T with
       | .struct s =>
           (match D[s]? with
            | some sd => (OwnSt.joinList D as bs sd.fields).map OwnSt.fields
            | none => none)
       | _ => none)

/-- The §5.5 join over a declaration's fields, slot by slot; where one arm has
no record the other arm's is kept, subject to `ownedJoinOk` (helper). -/
def OwnSt.joinList (D : StructEnv) : List OwnSt → List OwnSt → List Ty → Option (List OwnSt)
  | _, _, [] => some []
  | [], bs, Ts => if ownedJoinOkList D bs Ts then some bs else none
  | as, [], Ts => if ownedJoinOkList D as Ts then some as else none
  | a :: as, b :: bs, T :: Ts =>
      (match OwnSt.join D a b T, OwnSt.joinList D as bs Ts with
       | some e, some rest => some (e :: rest)
       | _, _ => none)
end

/-- The §5.5 branch join, per entry: the two arms' states for the binding,
joined over its paths at its declared type. -/
def Entry.join (D : StructEnv) (a b : Entry) : Option Entry :=
  (OwnSt.join D a.st b.st a.ty).map a.setSt

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
(§5.1); `binop` is (Arith) and (Ord) at once, `neg`/`notOp`/`bitnot` are
(Neg)/(Not)/(BitNot), `intCast` is (Int-Cast) and `dbg` is (Dbg), all §5.8;
`dropCopy`/`dropRes` are (@Drop-Copy)/(@Drop) (§5.3); `mkStruct` is
(Struct-Intro) (§5.8); `letIn` folds in §5.6's residual-linear scope-exit
check; `assign` is (Assign) with the `3.8:77` linear-overwrite premise on the
*post-RHS* state; `seq` is (Seq) with the `3.8:64` discard check; `ite` is (If)
with the §5.5 join; `call` is (Call) by value (§5.8); `ret` is (Return-Value)
and `panic` is (Panic), each with (Sub-Never) folded in (§5.7, §5.8). -/
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
  /-- (Use-Copy) §5.1: a use of a `Copy` place copies; Σ unchanged. `get`
  returning a state at all is `Owned-Base` for every proper prefix (`3.8:53`),
  since a path under a `MovedOut` prefix is absent from Σ.

  §5.1 states the node's own premise as `Σ(p) = Owned` and argues the subtree
  condition away: "every sub-place of a `Copy` type is itself `Copy`, so no
  descendant can be `MovedOut`, and the two premises coincide there". The rule
  here makes the subtree condition a premise instead of carrying that argument
  as an invariant of Σ. It restricts nothing a program can reach — no rule
  ever marks a sub-place of a `Copy` type `MovedOut`, since (Use-Move) and
  (@Drop) both demand a non-`Copy` type at the path they mark, and §3's
  `3.8:18` makes every field of a `@copy` declaration `Copy` — so `check`
  accepts the same programs either way. `noLinearPrefix` is the fragment's
  restriction, not a §5.1 premise (`Syntax.lean`). -/
  | useCopy {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.fullyOwned = true →
      en.ty.atPath P.structs p.path = some T →
      T.mult P.structs = .copy →
      noLinearPrefix P.structs en.ty p.path = true →
      Typed P R Γ (.use p) T Γ
  /-- (Use-Move) §5.1: a use of an `Affine`/`Linear` place moves it out — at a
  projection, the **partial move** of `3.8:22`, which marks exactly `p` and
  removes every path under it while leaving `p`'s siblings alone.
  `fully-owned(Σ, p)` is the premise (`3.8:26`: handing an aggregate with a
  hole to a new owner is ill-formed), and `noDtorPrefix` is `3.9:34`'s
  restriction (E0456). §4.2's third restriction, `3.8:68`'s root-index rule,
  has no instance without arrays. -/
  | useMove {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.fullyOwned = true →
      en.ty.atPath P.structs p.path = some T →
      T.mult P.structs ≠ .copy →
      noDtorPrefix P.structs en.ty p.path = true →
      noLinearPrefix P.structs en.ty p.path = true →
      Typed P R Γ (.use p) T (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut)))
  /-- (Arith) and (Ord) §5.8, in one rule because they differ only in the
  type they conclude at (`BinOp.resultTy`): both operands share one
  `int(w,s)`, typed left to right with Σ threaded (`4.2:1`), and the result is
  that same type for the arithmetic, bitwise and shift operators and `bool`
  for the ordering compares (`4.3:1`). The shift operators take their amount
  at the shifted operand's own type, which is `4.3a:9` and is why they need no
  second operand type here. -/
  | binop {Γ Γ₁ Γ₂ op e₁ e₂ w s} :
      Typed P R Γ e₁ (.int w s) Γ₁ → Typed P R Γ₁ e₂ (.int w s) Γ₂ →
      op.intAdmits = true →
      Typed P R Γ (.binop op e₁ e₂) (op.resultTy (.int w s)) Γ₂
  /-- (Float-Arith), (Float-Ord) and (Total-Cmp) §5.8, in one rule for the
  same reason `binop` fuses (Arith) and (Ord): they differ only in the type
  they conclude at (`BinOp.resultTy` — `float(w)`, `bool`, `int(32,signed)`).
  Both operands share **one** `float(w)`: `3.12:13` gives no implicit
  widening, so an `f32`/`f64` mix has no derivation, and `3.12:14` relates no
  float operand to an integer one — the only bridges are the intrinsics.
  `BinOp.floatAdmits` is §5.8's "rejected by the absence of a rule" for `%`
  (`3.12:25`) and for the bitwise and shift operators, written as a side
  condition because one constructor stands for the three rule groups. -/
  | floatBinop {Γ Γ₁ Γ₂ op e₁ e₂ w} :
      Typed P R Γ e₁ (.float w) Γ₁ → Typed P R Γ₁ e₂ (.float w) Γ₂ →
      op.floatAdmits = true →
      Typed P R Γ (.binop op e₁ e₂) (op.resultTy (.float w)) Γ₂
  /-- (Neg) §5.8: negation demands a **signed** operand (`4.2:6`; rejecting it
  on an unsigned type is `4.2:14`) and concludes at that type. -/
  | neg {Γ Γ' e w} :
      Typed P R Γ e (.int w .signed) Γ' →
      Typed P R Γ (.unop .neg e) (.int w .signed) Γ'
  /-- (Float-Neg) §5.8: float negation applies at **every** float type, where
  (Neg) restricts the integer case to a signed one (`3.12:24`, `4.2:14`), and
  §6.4 makes it total rather than trapping on a minimum — a sign flip, on
  `-0.0` and on a NaN alike. -/
  | floatNeg {Γ Γ' e w} :
      Typed P R Γ e (.float w) Γ' →
      Typed P R Γ (.unop .neg e) (.float w) Γ'
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
  /-- (Lit) §5.8 for a float: the literal at the `float(w)` elaboration
  resolved for it (`3.12:7`), denoting a *finite* value of that type. The side
  condition is `3.12:10`, a **legality** rule — "a float literal whose value
  rounds to an infinity in its target type MUST be rejected at compile time
  (`E0206`)" — which §5.8's own prose cites, so it belongs here rather than
  being left to the surface; it is `intLit`'s range premise at the float
  widths. What it does *not* say is that the decimal is representable:
  `3.12:9` rounds, so `0.1` is fine and so is an underflow to zero. The
  threshold is `FloatWidth.overflowNum`, half an ulp above `max_{𝔽_w}`, and it
  is exact natural arithmetic rather than anything the model decides. -/
  | floatLit {Γ w l} :
      l.RoundsFinite w →
      Typed P R Γ (.floatLit w l) (.float w) Γ
  /-- (Int-To-Float) §5.8: the operand is an integer of any width and
  signedness (`3.12:16`, `4.13:139`) and the result is the `float(w)`
  elaboration took from the use site. It never traps (§6.4). -/
  | intToFloat {Γ Γ' w w' s' e} :
      Typed P R Γ e (.int w' s') Γ' →
      Typed P R Γ (.fintrin (.intToFloat w) e) (.float w) Γ'
  /-- (Float-To-Int), (Float-Cast) and (Float-Round) §5.8, in one rule: each
  takes one `float(w)` operand and concludes at the type the form carries
  (`FloatIntrin.resTy`). `FloatIntrin.floatSrc` carries (Float-Cast)'s
  `w' ≠ w` side condition (`3.12:19`) and keeps `@int_to_float`, whose operand
  is an integer, on its own rule above. Whether a `@float_to_int` *survives*
  is dynamic, not a typing question: `3.12:18` and §6.4's
  (D-Float-To-Int-Trap). -/
  | floatIntrin {Γ Γ' k w e} :
      Typed P R Γ e (.float w) Γ' → k.floatSrc w = true →
      Typed P R Γ (.fintrin k e) (k.resTy w) Γ'
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
  /-- (@Drop-Copy) §5.3: no drop glue, no ownership effect. §5.3 gives it
  neither of (@Drop)'s projection premises — a `Copy` place is moved by
  nothing — so only `noLinearPrefix`, the fragment's own restriction, is
  added. The subtree condition is read the way (Use-Copy) above reads it, for
  the same reason and at the same cost (none). -/
  | dropCopy {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.fullyOwned = true →
      en.ty.atPath P.structs p.path = some T →
      T.mult P.structs = .copy →
      noLinearPrefix P.structs en.ty p.path = true →
      Typed P R Γ (.drop p) .unit Γ
  /-- (@Drop) §5.3: consumes the place and discharges its (affine or linear)
  obligation; the only non-move discharge of a linear obligation. At a
  projection it *is* a partial move, so it carries (Use-Move)'s `3.9:34`
  premise. What it does **not** carry is `fully-owned`: §5.3 states
  `Σ(p) = Owned` and says why — `@drop` hands the value to no new owner, and
  §6.11's `⊘`-skip drops a partially moved value correctly. Its own last
  premise takes that strength's place: where a path under `p` has been moved
  out, no still-owned linear sub-place may remain below `p`
  (`residualLinearBelow`). That premise is a **statics-only** discipline: the
  machine runs `@drop`'s glue over whatever the place holds, linear content
  included — which is what makes `@drop` the one non-move discharge of a
  linear obligation (`3.9:39`) — so no monitor refuses the state it forbids
  and `soundness` does not consume it. It is here because the calculus has it
  and the compiler enforces it (E0406). -/
  | dropRes {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.isOwned = true →
      en.ty.atPath P.structs p.path = some T →
      T.mult P.structs ≠ .copy →
      noDtorPrefix P.structs en.ty p.path = true →
      noLinearPrefix P.structs en.ty p.path = true →
      (u.fullyOwned = true ∨ residualLinearBelow P.structs u T = false) →
      Typed P R Γ (.drop p) .unit (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut)))
  /-- (Let) + §5.6 scope exit: the binder enters `Owned`; at the body's end
  its residual state must not be an unconsumed linear value (the leak check).
  An `Owned` affine residue is dropped by the machine (§6.7); `MovedOut` needs
  nothing. -/
  | letIn {Γ Γ₁ Γ₂ m e₁ e₂ T₁ T₂ en'} :
      Typed P R Γ e₁ T₁ Γ₁ →
      Typed P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ T₂ (en' :: Γ₂) →
      residualLinear P.structs en'.st en'.ty = false →
      Typed P R Γ (.letIn m e₁ e₂) T₂ Γ₂
  /-- (Assign) §5.2, at a place: the root must be a `μ = mut` binding (§5
  preamble), the RHS runs first, the overwrite of live linear content is
  ill-formed (`3.8:77`, checked on the **post-RHS** state — the RUE-387
  premise, and the `Σ1` reading that makes `p = f(p)` legal), and the subtree
  at `p` becomes `Owned` afterward (reinitialization, `3.8:55`). The `get`
  premises are `Owned-Base` (`3.8:53`) at both states: a path under a moved
  prefix is not a path to assign to, which the compiler reports as E0205. The
  `3.8:77` premise is read on the residual state for U1's reason, and on a
  whole binding it is §5.2's own disjunction. -/
  | assign {Γ Γ₁ p e en₀ en₁ u₀ u₁ T} :
      Γ[p.root]? = some en₀ → en₀.mu = true →
      en₀.st.get p.path = some u₀ →
      en₀.ty.atPath P.structs p.path = some T →
      Typed P R Γ e T Γ₁ →
      Γ₁[p.root]? = some en₁ →
      en₁.st.get p.path = some u₁ →
      residualLinear P.structs u₁ T = false →
      Typed P R Γ (.assign p e) .unit (Γ₁.set p.root (en₁.setSt (en₁.st.setAt p.path .owned)))
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
      NoResidualLinear P.structs Γ₁ →
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
  ∃ Γf, Typed P fd.ret (fnCtx fd) fd.body fd.ret Γf ∧ NoResidualLinear P.structs Γf

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

/-- The §5.5 join preserves an entry's skeleton: it rewrites the entry's
ownership state and nothing else (helper). -/
theorem Entry.join_skel {D : StructEnv} {a b e : Entry} (h : a.join D b = some e) :
    e.skel = a.skel := by
  unfold Entry.join at h
  cases hj : OwnSt.join D a.st b.st a.ty with
  | none => rw [hj] at h; cases h
  | some u => rw [hj] at h; cases h; rfl

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
    (s : OwnSt) : Ctx.skel (Γ.set i (en.setSt s)) = Ctx.skel Γ := by
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
  | useCopy _ _ _ _ _ _ => rfl
  | useMove hget _ _ _ _ _ _ => exact skel_set_setSt hget _
  | binop _ _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | floatBinop _ _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | neg _ ih => exact ih
  | floatNeg _ ih => exact ih
  | notOp _ ih => exact ih
  | bitnot _ ih => exact ih
  | intCast _ ih => exact ih
  | floatLit _ => rfl
  | intToFloat _ ih => exact ih
  | floatIntrin _ _ ih => exact ih
  | panic hskel => exact hskel
  | dbg _ _ ih => exact ih
  | mkStruct _ _ ih => exact ih
  | dropCopy _ _ _ _ _ _ => rfl
  | dropRes hget _ _ _ _ _ _ _ => exact skel_set_setSt hget _
  | letIn _ _ _ ih₁ ih₂ =>
      have := ih₂
      simp [Ctx.skel, List.map_cons] at this
      exact this.2.trans ih₁
  | assign _ _ _ _ _ hget₁ _ _ ih => exact (skel_set_setSt hget₁ _).trans ih
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
  | useCopy _ _ _ _ _ _ => rfl
  | useMove hget _ _ _ _ _ _ => exact skel_set_setSt hget _
  | binop _ _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | floatBinop _ _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | neg _ ih => exact ih
  | floatNeg _ ih => exact ih
  | notOp _ ih => exact ih
  | bitnot _ ih => exact ih
  | intCast _ ih => exact ih
  | floatLit _ => rfl
  | intToFloat _ ih => exact ih
  | floatIntrin _ _ ih => exact ih
  | panic hskel => exact hskel
  | dbg _ _ ih => exact ih
  | mkStruct _ _ ih => exact ih
  | dropCopy _ _ _ _ _ _ => rfl
  | dropRes hget _ _ _ _ _ _ _ => exact skel_set_setSt hget _
  | letIn _ _ _ ih₁ ih₂ =>
      have := ih₂
      simp [Ctx.skel, List.map_cons] at this
      exact this.2.trans ih₁
  | assign _ _ _ _ _ hget₁ _ _ ih => exact (skel_set_setSt hget₁ _).trans ih
  | seq _ _ _ ih₁ ih₂ => exact ih₂.trans ih₁
  | ite _ _ _ hjoin ihc ih₁ _ => exact (Ctx.join_skel hjoin).trans (ih₁.trans ihc)
  | call _ _ ih => exact ih
  | ret _ _ hskel ih => exact hskel.trans ih
  | nil => rfl
  | cons _ _ ih ihs => exact ihs.trans ih

end RueCore
