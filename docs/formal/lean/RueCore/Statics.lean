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

## Types and their class

§3 assigns a struct the join of its field classes lifted by the declared
attribute, and a declaration records that class (`Syntax.lean`). `WfStructs`
is §3's equation, made a premise of a well-formed program: the recorded class
*is* the lifted join, a `@copy` declaration's join is already `Copy` and it
declares no destructor (`3.8:18`, `3.9:31`), a destructor-bearing declaration
carries no linear field (`3.9:44`, E0462 — `3.9:34` forbids moving one out, so
the obligation could only be met by the glue, which is the implicit discard
§5.6 forbids; §3 states this as a well-formedness condition on the
declaration, beside the `@copy` one). The equation is a definition rather than
a fixpoint condition because `3.0:5` (E0483) forbids a declaration to contain
itself by value, directly or through a cycle — `WfNames` is that rule, joint
over both layers, and `class_unique` is the statement it buys, proved.

`3.9:44` is stated "through any depth of struct nesting", and `dtorWf` looks
one level down — at `sd.baseOf D`, the join of the *immediate* field classes.
The two agree because `WfStructs` holds at every declaration, the fields'
included: a field whose type is linear only by infection has `baseOf = Linear`
at *its* declaration, and `Attr.lift` then forces its recorded class to
`Linear` for every attribute `copyWf` permits. So a linear value at any depth
has already reached the immediate field's class by the time `dtorWf` reads the
join, and one level is the whole depth.

§3 gives an **enum** one equation and no lifting: `class(E)` is the join over
every payload component of every variant (`6.3:19`), so `EnumDecl.Wf` is that
equation and `WfEnums` its program-wide form. An enum declares no attribute and
no destructor, so there is no `@copy`/`linear` clause to check and nothing for
§6.11 to run before the payload (the compiler rejects `drop fn E(self)` with
E0417: a destructor names a struct type).

`carries_linear(T) ⟺ class(T) = Linear` is §5.3's own reading, so it is a
definition here (`Ty.carriesLinear`); what §5.3 asks to be checked is the
*lifting*, and `struct_carriesLinear_iff` is it: a struct's class reaches
`Linear` exactly when the declaration says `linear` or some field carries a
linear value. `enum_carriesLinear_iff` is the same sentence for an enum, over
every variant rather than the active one.

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
def StructDecl.baseOf (D : Decls) (sd : StructDecl) : Mult :=
  sd.fields.foldl (fun m T => m.join (Ty.mult D T)) .copy

/-- The accumulator of §3's join is a lower bound of the result (helper). -/
theorem rank_le_joinFold (D : Decls) : ∀ (Ts : List Ty) (acc : Mult),
    acc.rank ≤ (Ts.foldl (fun m T => m.join (Ty.mult D T)) acc).rank
  | [], _ => Nat.le_refl _
  | _ :: Ts, acc =>
      Nat.le_trans (Mult.rank_le_join_left acc _) (rank_le_joinFold D Ts _)

/-- Every field's class is below §3's join of them (helper). -/
theorem rank_le_joinFold_of_mem (D : Decls) : ∀ (Ts : List Ty) (acc : Mult) (T : Ty),
    T ∈ Ts → (T.mult D).rank ≤ (Ts.foldl (fun m T => m.join (Ty.mult D T)) acc).rank
  | T' :: Ts, acc, T, h => by
      cases h with
      | head =>
          exact Nat.le_trans (Mult.rank_le_join_right acc _) (rank_le_joinFold D Ts _)
      | tail _ h => exact rank_le_joinFold_of_mem D Ts _ T h

/-- §3's join reaches `Linear` only through a field that does (helper). -/
theorem joinFold_linear_inv (D : Decls) : ∀ (Ts : List Ty) (acc : Mult),
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
declaration's join is already `Copy` and it declares no destructor, and a
destructor-bearing declaration carries no linear field.

This is the *equation* only. What makes it solvable — that no declaration
contains itself by value, directly or through a cycle (`3.0:5`, E0483) — is
`WfNames`, stated jointly over both layers below, because a field may name an
enum and a payload may name a struct. -/
structure StructDecl.Wf (D : Decls) (sd : StructDecl) : Prop where
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
def WfStructs (D : Decls) : Prop :=
  ∀ (s : Nat) (sd : StructDecl), D.structs[s]? = some sd → StructDecl.Wf D sd

/-- **A droppable struct carries no linear field.** If a declaration's class
is not `Linear`, no field's class is — which is why the machine's leak monitor
(§6.7's `endscope`, §6.9's frame teardown) needs to look only at the value's
own class and never inside it. This is §3's infectiousness, used. -/
theorem StructDecl.Wf.field_not_linear {D : Decls} {sd : StructDecl}
    (h : sd.Wf D) (hcls : sd.cls ≠ .linear) : ∀ T ∈ sd.fields, T.mult D ≠ .linear := by
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
theorem struct_carriesLinear_iff {D : Decls} {s : Nat} {sd : StructDecl}
    (hd : D.structs[s]? = some sd) (h : sd.Wf D) :
    (Ty.struct s).mult D = .linear ↔
      (sd.attr = .linear ∨ ∃ T ∈ sd.fields, T.mult D = .linear) := by
  have hlookup : (Ty.struct s).mult D = sd.cls := by
    simp [Ty.mult, Decls.classOf, hd]
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

/-! ## §3's class assignment for an enum: the payload join (`6.3:19`)

An enum has no attribute to lift and no destructor to declare, so §3 gives it
one equation and nothing else: `class(E)` is the join over **every** payload
component of **every** variant, because the active variant is not a static
fact. A discriminant-only enum's join is empty and so `Copy` (`6.3:19`,
`3.8:2`), which is what makes `enum C { A, B }` a duplicable tag.

`EnumDecl.Wf` is that equation, made a premise of a well-formed program; what
makes it solvable is `3.0:5`'s acyclicity (`WfNames`), which is joint over the
two layers because a payload may name a struct and a field may name an enum.
`enum_carriesLinear_iff` is `6.3:19`'s
must-consume sentence as a biconditional — an enum is `Linear` exactly when some
variant's payload carries a linear value, whatever variant a particular value
holds — and `EnumDecl.Wf.payload_not_linear` is the direction the machine needs:
a non-`Linear` enum has no linear payload to leak.
-/

/-- §3's payload join for one enum declaration: `⊔ { class(Tij) }` over every
component of every variant, read left to right, variant by variant (`6.3:19`).
The empty join is `Copy`, which is the discriminant-only case. -/
def EnumDecl.payloadJoin (D : Decls) (ed : EnumDecl) : Mult :=
  ed.variants.foldl (fun m Ts => Ts.foldl (fun m' T => m'.join (Ty.mult D T)) m) .copy

/-- One enum declaration's well-formedness (§3, `6.3:19`): its recorded class
is the payload join. As for a struct this is the equation only, and `WfNames`
is what makes it solvable (`3.0:5` forbids an enum to contain itself by value
through any cycle of fields and payloads).

There is no attribute clause and no destructor clause, because §3 gives an enum
neither: `6.3:19` fixes its class as the join with no `@copy`/`linear` mark to
lift, and the compiler rejects `drop fn E(self)` where it is declared (E0417),
which is why `EnumDecl` records no `dtor` field for §6.11 to read. -/
structure EnumDecl.Wf (D : Decls) (ed : EnumDecl) : Prop where
  /-- §3's assignment: `class(E) = ⊔ { class(Tij) }` (`6.3:19`). -/
  classIsJoin : ed.cls = ed.payloadJoin D

/-- A well-formed enum environment: §3's class assignment holds of every enum
declaration (`EnumDecl.Wf`). Together with `WfStructs` this is the premise that
makes `Ty.mult`'s lookup §3's join at every type, and it is what `checkEnums`
(`Checker.lean`) decides. -/
def WfEnums (D : Decls) : Prop :=
  ∀ (e : Nat) (ed : EnumDecl), D.enums[e]? = some ed → EnumDecl.Wf D ed

/-! ### `3.0:5`: no declaration contains itself by value

`3.0:5` (E0483) is the rule that makes §3's two class equations a definition:
"A struct or enum **MUST NOT** contain itself by value, either directly or
through a cycle of struct fields, enum payloads, or array elements." It is one
rule over **both** layers, and it has to be: a field may name an enum
(`struct H0 { x0: E0, x1: S1 }`) and a payload may name a struct
(`enum E0 { K0(S1), K1 }`), so the two equations are mutually recursive and a
per-layer order does not exclude `struct S { x0: E } / enum E { K(S) }` — a
shape both equations solve at more than one assignment
(`Examples.lean`'s cycle witnesses; the compiler reports E0483).

`Decls.Names` is `3.0:5`'s "contains by value" relation, one step, and
`WfNames` is the rule itself: the relation is **well-founded**, so each
declaration's class is the unique solution of its equation (`class_unique`).
The calculus states the equations but not this side condition; §3 gains the
paragraph in RUE-2334, and `3.0:5` is the normative form it mechanizes.
`checkNoCycle` (`Checker.lean`) decides it by peeling. -/

/-- A declaration of either kind, named the way a type names it: the domain of
`3.0:5`'s "contains by value" relation. -/
inductive DeclId where
  /-- The struct declaration `Ty.struct s` names. -/
  | struct (s : Nat)
  /-- The enum declaration `Ty.enum e` names. -/
  | enum (e : Nat)
deriving DecidableEq, Repr

/-- The type that names this declaration (helper). -/
def DeclId.ty : DeclId → Ty
  | .struct s => .struct s
  | .enum e => .enum e

/-- The declarations a type names **by value** (`3.0:5`): a struct or an enum
type names its own declaration, an array names whatever its element type names
— `3.0:5` lists "array elements" beside struct fields and enum payloads, and
an array's storage *is* its elements' (`3.5:4`), so `struct S { x0: [S; 1] }`
is no less recursive than `struct S { x0: S }` and the compiler reports E0483
for both — and a scalar names none (helper). -/
def Ty.declIds : Ty → List DeclId
  | .struct s => [.struct s]
  | .enum e => [.enum e]
  | .array T _ => T.declIds
  | .int _ _ | .float _ | .bool | .unit => []

/-- Two environments that give the same class to every declaration a type
names by value give that type the same class: `class([T; n])` is §3's lift of
`class(T)`, so peeling the array wrappers loses nothing (helper). -/
theorem Ty.mult_congr_declIds {D D' : Decls} :
    ∀ T : Ty, (∀ d ∈ T.declIds, d.ty.mult D = d.ty.mult D') → T.mult D = T.mult D'
  | .struct s, h => h (.struct s) (by simp [Ty.declIds])
  | .enum e, h => h (.enum e) (by simp [Ty.declIds])
  | .array T _, h => by
      simp only [Ty.mult,
        Ty.mult_congr_declIds T (fun d hd => h d (by simpa only [Ty.declIds] using hd))]
  | .int _ _, _ | .float _, _ | .bool, _ | .unit, _ => rfl

/-- The types a declaration contains **by value** (`3.0:5`): a struct's fields
and an enum's payload components, over every variant. An index the environment
does not have contains nothing. -/
def Decls.byValue (D : Decls) : DeclId → List Ty
  | .struct s => match D.structs[s]? with
                 | some sd => sd.fields
                 | none => []
  | .enum e => match D.enums[e]? with
               | some ed => ed.variants.flatten
               | none => []

/-- `3.0:5`'s relation, one step: `d` contains `d'` by value. A slot reaches
its declaration **through any depth of array nesting** (`Ty.declIds`), because
`3.0:5` names array elements beside fields and payloads; without that, a
struct naming itself through an array element would satisfy `WfNames` and §3's
equation would have more than one solution at it. -/
def Decls.Names (D : Decls) (d d' : DeclId) : Prop := ∃ T ∈ D.byValue d, d' ∈ T.declIds

/-- **`3.0:5` (E0483), mechanized**: the by-value "contains" relation over the
declarations is well-founded, so no declaration reaches itself through a cycle
of struct fields and enum payloads. This is the one premise that makes §3's
struct and enum equations a *definition* — `class_unique` is the induction it
licenses — and it is joint over the two layers because `3.0:5` is. -/
def WfNames (D : Decls) : Prop := WellFounded (fun d' d => D.Names d d')

/-- A well-formed declaration environment: `3.0:5`'s acyclicity (`WfNames`),
§3's class assignment for every struct declaration (`WfStructs`) and for every
enum declaration (`WfEnums`). This is the premise every theorem that reads a
recorded class through `Ty.mult` carries, and it is what `checkDecls`
(`Checker.lean`) decides. -/
structure WfDecls (D : Decls) : Prop where
  /-- `3.0:5` (E0483): no declaration contains itself by value. -/
  names : WfNames D
  /-- §3's class assignment for the struct layer (`3.8:18`, `3.9:31`, `3.9:44`). -/
  structs : WfStructs D
  /-- §3's class assignment for the enum layer (`6.3:19`). -/
  enums : WfEnums D

/-- The accumulator of the payload join is a lower bound of the result
(helper). -/
theorem rank_le_payloadFold (D : Decls) : ∀ (Tss : List (List Ty)) (acc : Mult),
    acc.rank ≤ (Tss.foldl (fun m Ts => Ts.foldl (fun m' T => m'.join (Ty.mult D T)) m) acc).rank
  | [], _ => Nat.le_refl _
  | Ts :: Tss, acc =>
      Nat.le_trans (rank_le_joinFold D Ts acc) (rank_le_payloadFold D Tss _)

/-- Every payload component's class is below §3's join of them (`6.3:19`)
(helper). -/
theorem rank_le_payloadFold_of_mem (D : Decls) :
    ∀ (Tss : List (List Ty)) (acc : Mult) (Ts : List Ty) (T : Ty), Ts ∈ Tss → T ∈ Ts →
      (T.mult D).rank
        ≤ (Tss.foldl (fun m Ts' => Ts'.foldl (fun m' T' => m'.join (Ty.mult D T')) m) acc).rank
  | Ts' :: Tss, acc, Ts, T, hTs, hT => by
      cases hTs with
      | head =>
          exact Nat.le_trans (rank_le_joinFold_of_mem D Ts' acc T hT)
            (rank_le_payloadFold D Tss _)
      | tail _ h => exact rank_le_payloadFold_of_mem D Tss _ Ts T h hT

/-- §3's payload join reaches `Linear` only through a payload component that
does (helper). -/
theorem payloadFold_linear_inv (D : Decls) : ∀ (Tss : List (List Ty)) (acc : Mult),
    Tss.foldl (fun m Ts => Ts.foldl (fun m' T => m'.join (Ty.mult D T)) m) acc = .linear →
      acc = .linear ∨ ∃ Ts ∈ Tss, ∃ T ∈ Ts, T.mult D = .linear
  | [], _, h => Or.inl h
  | Ts :: Tss, acc, h => by
      simp only [List.foldl_cons] at h
      rcases payloadFold_linear_inv D Tss _ h with h' | ⟨Ts', hmem, hT'⟩
      · rcases joinFold_linear_inv D Ts acc h' with h'' | ⟨T, hmem, hT⟩
        · exact Or.inl h''
        · exact Or.inr ⟨Ts, List.mem_cons_self, T, hmem, hT⟩
      · exact Or.inr ⟨Ts', List.mem_cons_of_mem _ hmem, hT'⟩

/-- **A droppable enum carries no linear payload.** If a declaration's class is
not `Linear`, no payload component of any variant is — which is why the
machine's leak monitor need only read the payload it finds under the active tag
(§6.11) and never the declaration. This is `6.3:19`'s join, used. -/
theorem EnumDecl.Wf.payload_not_linear {D : Decls} {ed : EnumDecl}
    (h : ed.Wf D) (hcls : ed.cls ≠ .linear) :
    ∀ Ts ∈ ed.variants, ∀ T ∈ Ts, T.mult D ≠ .linear := by
  intro Ts hTs T hT hlin
  refine hcls ?_
  rw [h.classIsJoin]
  refine Mult.eq_linear_of_rank ?_
  have := rank_le_payloadFold_of_mem D ed.variants .copy Ts T hTs hT
  rw [hlin] at this
  exact this

/-- **`carries_linear` lifts through an enum's payloads** (§5.3, `6.3:19`). An
enum's class reaches `Linear` exactly when some variant carries a linear payload
component — over *every* variant, not the active one, because the active variant
is a dynamic fact and the class is the type's worst case. This is what makes
`E0.K1` of `enum E0 { K0(T0), K1 }` with `T0` declared `linear` a must-consume
value even though the value it holds carries nothing (probe e11, E0406). -/
theorem enum_carriesLinear_iff {D : Decls} {e : Nat} {ed : EnumDecl}
    (hd : D.enums[e]? = some ed) (h : ed.Wf D) :
    (Ty.enum e).mult D = .linear ↔ ∃ Ts ∈ ed.variants, ∃ T ∈ Ts, T.mult D = .linear := by
  have hlookup : (Ty.enum e).mult D = ed.cls := by
    simp [Ty.mult, Decls.enumClassOf, hd]
  constructor
  · intro hlin
    rw [hlookup, h.classIsJoin] at hlin
    rcases payloadFold_linear_inv D ed.variants .copy hlin with h' | hex
    · cases h'
    · exact hex
  · intro ⟨Ts, hTs, T, hT, hlin⟩
    rw [hlookup, h.classIsJoin]
    refine Mult.eq_linear_of_rank ?_
    have := rank_le_payloadFold_of_mem D ed.variants .copy Ts T hTs hT
    rw [hlin] at this
    exact this

/-! ## The class a declaration records is determined, not free

A declaration carries `class(S)`/`class(E)` so that `Ty.mult` is a lookup. That
is only honest if §3's equations have one solution, which is what `3.0:5`
buys: no declaration contains itself by value, so the by-value relation is
well-founded (`WfNames`) and each declaration's class is fixed by the classes
of the declarations it names.

The condition has to be **joint**, because the recursion is. A field may name
an enum and a payload may name a struct, so `struct S { x0: E }` /
`enum E { K(S) }` satisfies §3's struct equation *and* `6.3:19`'s enum equation
at more than one assignment, and only a cross-layer condition excludes it
(`Examples.lean` pins that shape as a refusal witness; the compiler reports
E0483). `class_unique` is therefore **one** theorem over both layers, assuming
nothing about the other layer's classes, and
`struct_class_unique`/`enum_class_unique` are its two projections.

The specification states the rule normatively and across both layers — `3.0:5`,
"A struct or enum MUST NOT contain itself by value, either directly or through
a cycle of struct fields, enum payloads, or array elements" (E0483) — and that
is the citation this fragment mechanizes. The *calculus* states §3's equations
without the side condition; the paragraph that adds it is §3 (RUE-2334). -/

/-- Two environments that give every type of a field list the same class give
that list the same §3 join (helper). -/
theorem joinFold_congr {D D' : Decls} :
    ∀ (Ts : List Ty) (acc : Mult), (∀ T ∈ Ts, T.mult D = T.mult D') →
      Ts.foldl (fun m T => m.join (Ty.mult D T)) acc
        = Ts.foldl (fun m T => m.join (Ty.mult D' T)) acc
  | [], _, _ => rfl
  | T :: Ts, acc, h => by
      simp only [List.foldl_cons, h T List.mem_cons_self]
      exact joinFold_congr Ts _ (fun T' hm => h T' (List.mem_cons_of_mem _ hm))

/-- The same for `6.3:19`'s payload join, over every component of every variant
(helper). -/
theorem payloadFold_congr {D D' : Decls} :
    ∀ (Tss : List (List Ty)) (acc : Mult),
      (∀ Ts ∈ Tss, ∀ T ∈ Ts, T.mult D = T.mult D') →
      Tss.foldl (fun m Ts => Ts.foldl (fun m' T => m'.join (Ty.mult D T)) m) acc
        = Tss.foldl (fun m Ts => Ts.foldl (fun m' T => m'.join (Ty.mult D' T)) m) acc
  | [], _, _ => rfl
  | Ts :: Tss, acc, h => by
      have hhead := joinFold_congr (D := D) (D' := D') Ts acc (h Ts List.mem_cons_self)
      simp only [List.foldl_cons, hhead]
      exact payloadFold_congr Tss _ (fun Ts' hm => h Ts' (List.mem_cons_of_mem _ hm))

/-- **§3's class assignment has exactly one solution** (`3.0:5`, `6.3:19`).
Two declaration environments of the same *shapes* — the same number of struct
and of enum declarations, the same attribute and field list at every struct
index, the same variant payloads at every enum index — that each satisfy
`WfDecls` assign the same class to **every** type: every struct, every enum,
and every scalar. So recording `class(S)`/`class(E)` in the declaration
(`Syntax.lean`) records a determined value rather than a free parameter, and a
`checkProgram = true` verdict is a verdict about the declarations the compiler
would compute the same classes for.

The theorem takes no hypothesis about the other layer's classes, which is what
`3.0:5`'s joint well-foundedness buys: the induction is over the by-value
"contains" relation rather than over a declaration index, so a field naming an
enum and a payload naming a struct are the same step. `dtor` does not appear,
because §3's equations do not read it. -/
theorem class_unique {D D' : Decls} (hwf : WfDecls D) (hwf' : WfDecls D')
    (hslen : D.structs.length = D'.structs.length)
    (helen : D.enums.length = D'.enums.length)
    (hsshape : ∀ (s : Nat) (sd sd' : StructDecl),
      D.structs[s]? = some sd → D'.structs[s]? = some sd' →
        sd.attr = sd'.attr ∧ sd.fields = sd'.fields)
    (heshape : ∀ (e : Nat) (ed ed' : EnumDecl),
      D.enums[e]? = some ed → D'.enums[e]? = some ed' → ed.variants = ed'.variants) :
    ∀ T : Ty, T.mult D = T.mult D' := by
  have key : ∀ d : DeclId, d.ty.mult D = d.ty.mult D' := by
    intro d
    refine WellFounded.induction (C := fun d => d.ty.mult D = d.ty.mult D') hwf.names d ?_
    clear d
    intro d ih
    have hmem : ∀ T ∈ D.byValue d, T.mult D = T.mult D' := fun T hT =>
      Ty.mult_congr_declIds T (fun d' hd' => ih d' ⟨T, hT, hd'⟩)
    cases d with
    | struct s =>
        show D.classOf s = D'.classOf s
        cases hd : D.structs[s]? with
        | none =>
            have : D'.structs[s]? = none := by
              rcases hd' : D'.structs[s]? with _ | sd'
              · rfl
              · exact absurd (List.getElem?_eq_some_iff.mp hd' |>.1)
                  (by have := List.getElem?_eq_none_iff.mp hd; omega)
            simp [Decls.classOf, hd, this]
        | some sd =>
            have hlt : s < D'.structs.length := by
              have := List.getElem?_eq_some_iff.mp hd |>.1; omega
            obtain ⟨sd', hd'⟩ : ∃ sd', D'.structs[s]? = some sd' := by
              rcases hd' : D'.structs[s]? with _ | sd'
              · exact absurd (List.getElem?_eq_none_iff.mp hd') (by omega)
              · exact ⟨sd', rfl⟩
            obtain ⟨hattr, hfields⟩ := hsshape s sd sd' hd hd'
            have hfmem : ∀ T ∈ sd.fields, T.mult D = T.mult D' := by
              intro T hT
              exact hmem T (by simp only [Decls.byValue, hd]; exact hT)
            have hbase : sd.baseOf D = sd'.baseOf D' := by
              unfold StructDecl.baseOf
              rw [← hfields]
              exact joinFold_congr sd.fields .copy hfmem
            simp only [Decls.classOf, hd, hd']
            rw [(hwf.structs s sd hd).classIsJoin, (hwf'.structs s sd' hd').classIsJoin,
              hattr, hbase]
    | enum e =>
        show D.enumClassOf e = D'.enumClassOf e
        cases hd : D.enums[e]? with
        | none =>
            have : D'.enums[e]? = none := by
              rcases hd' : D'.enums[e]? with _ | ed'
              · rfl
              · exact absurd (List.getElem?_eq_some_iff.mp hd' |>.1)
                  (by have := List.getElem?_eq_none_iff.mp hd; omega)
            simp [Decls.enumClassOf, hd, this]
        | some ed =>
            have hlt : e < D'.enums.length := by
              have := List.getElem?_eq_some_iff.mp hd |>.1; omega
            obtain ⟨ed', hd'⟩ : ∃ ed', D'.enums[e]? = some ed' := by
              rcases hd' : D'.enums[e]? with _ | ed'
              · exact absurd (List.getElem?_eq_none_iff.mp hd') (by omega)
              · exact ⟨ed', rfl⟩
            have hvar := heshape e ed ed' hd hd'
            have hpmem : ∀ Ts ∈ ed.variants, ∀ T ∈ Ts, T.mult D = T.mult D' := by
              intro Ts hTs T hT
              refine hmem T ?_
              simp only [Decls.byValue, hd]
              exact List.mem_flatten.mpr ⟨Ts, hTs, hT⟩
            have hjoin : ed.payloadJoin D = ed'.payloadJoin D' := by
              unfold EnumDecl.payloadJoin
              rw [← hvar]
              exact payloadFold_congr ed.variants .copy hpmem
            simp only [Decls.enumClassOf, hd, hd']
            rw [(hwf.enums e ed hd).classIsJoin, (hwf'.enums e ed' hd').classIsJoin, hjoin]
  exact fun T => Ty.mult_congr_declIds T (fun d _ => key d)

/-- **§3's class assignment for the struct layer has one solution**, the
projection of `class_unique` §3's own sentence asks for. It needs the enum
layer's shapes as well as the struct layer's, because a field may name an enum
— that is the mutual recursion `3.0:5` grounds, not a weakness of the
statement. -/
theorem struct_class_unique {D D' : Decls} (hwf : WfDecls D) (hwf' : WfDecls D')
    (hslen : D.structs.length = D'.structs.length)
    (helen : D.enums.length = D'.enums.length)
    (hsshape : ∀ (s : Nat) (sd sd' : StructDecl),
      D.structs[s]? = some sd → D'.structs[s]? = some sd' →
        sd.attr = sd'.attr ∧ sd.fields = sd'.fields)
    (heshape : ∀ (e : Nat) (ed ed' : EnumDecl),
      D.enums[e]? = some ed → D'.enums[e]? = some ed' → ed.variants = ed'.variants) :
    ∀ s, D.classOf s = D'.classOf s :=
  fun s => class_unique hwf hwf' hslen helen hsshape heshape (.struct s)

/-- **§3's class assignment for the enum layer has one solution** (`6.3:19`),
the other projection of `class_unique`. Simpler than the struct one in its own
layer — an enum records no attribute, so its class *is* the payload join — and
mutual in the same way: a payload may name a struct. -/
theorem enum_class_unique {D D' : Decls} (hwf : WfDecls D) (hwf' : WfDecls D')
    (hslen : D.structs.length = D'.structs.length)
    (helen : D.enums.length = D'.enums.length)
    (hsshape : ∀ (s : Nat) (sd sd' : StructDecl),
      D.structs[s]? = some sd → D'.structs[s]? = some sd' →
        sd.attr = sd'.attr ∧ sd.fields = sd'.fields)
    (heshape : ∀ (e : Nat) (ed ed' : EnumDecl),
      D.enums[e]? = some ed → D'.enums[e]? = some ed' → ed.variants = ed'.variants) :
    ∀ e, D.enumClassOf e = D'.enumClassOf e :=
  fun e => class_unique hwf hwf' hslen helen hsshape heshape (.enum e)

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

/-- `Σ`'s lookup along a concatenated path is the two lookups in turn. This is
what lets (Use-Declared-Linear-Destructure) §5.1 state its ownership premise at
`d` — reached by `π_d` — and still speak for the leaf at `π_d · π_s`
(helper). -/
theorem OwnSt.get_append : ∀ (t : OwnSt) (π ρ : List Nat),
    t.get (π ++ ρ) = (t.get π).bind fun u => u.get ρ
  | _, [], _ => rfl
  | .owned, _ :: π, ρ => OwnSt.get_append .owned π ρ
  | .movedOut, _ :: _, _ => rfl
  | .fields ts, f :: π, ρ => OwnSt.get_append (OwnSt.fieldAt ts f) π ρ

/-- Every field slot of a fully-owned node is fully owned (helper). -/
theorem OwnSt.fullyOwned_fieldAt : ∀ {ts : List OwnSt} (f : Nat),
    OwnSt.fullyOwnedList ts = true → (OwnSt.fieldAt ts f).fullyOwned = true
  | [], _, _ => rfl
  | _ :: _, 0, h => by
      simp only [OwnSt.fullyOwnedList, Bool.and_eq_true] at h
      simpa only [OwnSt.fieldAt, List.getElem?_cons_zero, Option.getD_some] using h.1
  | _ :: ts, f + 1, h => by
      simp only [OwnSt.fullyOwnedList, Bool.and_eq_true] at h
      simpa only [OwnSt.fieldAt, List.getElem?_cons_succ] using
        OwnSt.fullyOwned_fieldAt (ts := ts) f h.2

/-- **A fully-owned node owns every path under it.** `fully-owned(Σ, d)`
(§5 preamble) gives every place under `d` a state of its own, itself fully
owned — which is why (Use-Declared-Linear-Destructure) §5.1 asks it of `d`
alone and still knows the selected leaf is there, whole (helper). -/
theorem OwnSt.fullyOwned_get : ∀ {t : OwnSt} (π : List Nat), t.fullyOwned = true →
    ∃ u, t.get π = some u ∧ u.fullyOwned = true
  | t, [], h => ⟨t, rfl, h⟩
  | .owned, _ :: π, _ => OwnSt.fullyOwned_get (t := .owned) π rfl
  | .movedOut, _ :: _, h => by simp [OwnSt.fullyOwned] at h
  | .fields ts, _ :: π, h =>
      OwnSt.fullyOwned_get π (OwnSt.fullyOwned_fieldAt _ (by simpa [OwnSt.fullyOwned] using h))

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
part of an infectious carrier and letting the rest drop is legal.

**The array clause and §5.6's second disjunct.** An array's node carries no
obligation of its own — it declares no attribute, and `3.8:74` makes a
zero-length one vacuous — so the obligation is the disjunction over its `n`
elements, each at the element type (`3.8:71`, §5.3's "the element type for an
array of nonzero length"). §5.6 writes that clause with a second disjunct,
"(untracked residue carries linear)", for the elements the tracked list does
not reach. It is not absent here: `residualLinearFields`' `[], Ts` base case
answers those slots at the **type** level, `Ts.any (·.mult D = .linear)`,
which is the second disjunct's job done conservatively — it can only say
"carries" where the calculus's own disjunct would. What would make the
difference observable is a *dynamic-index* move, which leaves residue no path
names (`3.8:70`); this part has no such move, so the two readings agree on
every program it accepts. RUE-2327 is where the distinction starts to bite. -/
def residualLinear (D : Decls) : OwnSt → Ty → Bool
  | .movedOut, _ => false
  | .owned, T => decide (T.mult D = .linear)
  | .fields ts, .struct s =>
      (match D.structs[s]? with
       | some sd => sd.attr = .linear || residualLinearFields D ts sd.fields
       | none => false)
  -- The array clause, and §5.6's second disjunct: see the docstring above. A
  -- partially-written array node is reachable in this part (`a[0] = …`); a
  -- partially *moved* one is RUE-2327's (`Syntax.lean`).
  | .fields ts, .array T n => residualLinearFields D ts (List.replicate n T)
  | .fields _, _ => false

/-- §5.6's field disjunction: a field slot no partial move touched is `owned`,
so its clause is the type-level test (helper). -/
def residualLinearFields (D : Decls) : List OwnSt → List Ty → Bool
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
def residualLinearBelow (D : Decls) : OwnSt → Ty → Bool
  | .movedOut, _ => false
  | t, .struct s =>
      (match D.structs[s]? with
       | some sd => residualLinearFields D t.fieldStates sd.fields
       | none => false)
  -- The array form of the same reading (`3.8:73` is the element-wise `3.8:60`).
  | t, .array T n => residualLinearFields D t.fieldStates (List.replicate n T)
  | _, _ => false

/-- §5.2's (Assign) premise `Σ1(p) = MovedOut ∨ ¬carries_linear(T)` (`3.8:77`),
as a decidable test on the post-RHS state.

This one is keyed on the destination's **type**, not on its residue, and
deliberately so: `3.8:77` says the diagnostic "is determined by the
destination's *type* together with the statically tracked move paths, never by
a run-time drop flag", and the compiler agrees (E0493 fires on a root
reassignment of a linear-carrying struct even when a field `@drop` has already
taken the linear part out from under it). Reading it on the residue instead —
`residualLinear D u T = false`, the shape §5.6's leak check and §5.5's join
use — would accept that program, so this is the one place in the fragment
where the residual reading is *not* the right one. §5.6 and §5.5 abandoned the
type-level test because it over-rejects a **discharge**; (Assign) is not a
discharge, and `3.8:77`'s point is that an overwrite never performs one.

`residualLinear D u T = false` follows from either disjunct — `MovedOut`
carries nothing, and a non-linear type has no linear content to carry
(`ContentsTy.residualLinear_false`) — so this premise is strictly stronger
than the residual one and the dynamic `linearOverwrite` monitor, which reads
the residue because the residue is what the machine is about to drop, stays
reachable only through a program `check` rejects. -/
def overwriteOk (D : Decls) : OwnSt → Ty → Bool
  | .movedOut, _ => true
  | .owned, T => decide (T.mult D ≠ .linear)
  | .fields _, T => decide (T.mult D ≠ .linear)

/-- `overwriteOk` is §5.2's disjunction, spelled as the rule spells it: the
`Prop` form is what `Typed.assign` carries, the `Bool` form what `check`
decides. -/
theorem overwriteOk_iff {D : Decls} {u : OwnSt} {T : Ty} :
    overwriteOk D u T = true ↔ (u = .movedOut ∨ T.mult D ≠ .linear) := by
  cases u <;> simp [overwriteOk]

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
def NoResidualLinear (D : Decls) (Γ : Ctx) : Prop :=
  ∀ en ∈ Γ, residualLinear D en.st en.ty = false

instance (D : Decls) (Γ : Ctx) : Decidable (NoResidualLinear D Γ) := by
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
dropping the residue path-specifically (`3.8:60`). Two `Owned` nodes with
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
def ownedJoinOk (D : Decls) : OwnSt → Ty → Bool
  | .owned, _ => true
  | .movedOut, T => decide (T.mult D ≠ .linear)
  | .fields ts, .struct s =>
      (match D.structs[s]? with
       | some sd => ownedJoinOkList D ts sd.fields
       | none => false)
  -- The array node, read element by element (`3.8:73`).
  | .fields ts, .array T n => ownedJoinOkList D ts (List.replicate n T)
  | .fields _, _ => false

/-- The same over a declaration's fields; a slot no partial move touched is
`owned` and always admissible (helper). -/
def ownedJoinOkList (D : Decls) : List OwnSt → List Ty → Bool
  | [], _ => true
  | _ :: _, [] => true
  | t :: ts, T :: Ts => ownedJoinOk D t T && ownedJoinOkList D ts Ts
end

mutual
/-- The §5.5 branch join, at one path and its subtree (section docstring). -/
def OwnSt.join (D : Decls) : OwnSt → OwnSt → Ty → Option OwnSt
  | .owned, b, T => if ownedJoinOk D b T then some b else none
  | a, .owned, T => if ownedJoinOk D a T then some a else none
  | .movedOut, b, T => if residualLinear D b T then none else some .movedOut
  | a, .movedOut, T => if residualLinear D a T then none else some .movedOut
  | .fields as, .fields bs, T =>
      (match T with
       | .struct s =>
           (match D.structs[s]? with
            | some sd => (OwnSt.joinList D as bs sd.fields).map OwnSt.fields
            | none => none)
       -- The array node, joined element by element (`3.8:73`, the
       -- element-wise form of the field join above).
       | .array T' n => (OwnSt.joinList D as bs (List.replicate n T')).map OwnSt.fields
       | _ => none)

/-- The §5.5 join over a declaration's fields, slot by slot; where one arm has
no record the other arm's is kept, subject to `ownedJoinOk` (helper). -/
def OwnSt.joinList (D : Decls) : List OwnSt → List OwnSt → List Ty → Option (List OwnSt)
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
def Entry.join (D : Decls) (a b : Entry) : Option Entry :=
  (OwnSt.join D a.st b.st a.ty).map a.setSt

/-- The §5.5 branch join, pointwise. Defined only on equal-length contexts
(the two arms extend one incoming context, so lengths always agree). -/
def Ctx.join (D : Decls) : Ctx → Ctx → Option Ctx
  | [], [] => some []
  | a :: as, b :: bs =>
      match a.join D b, Ctx.join D as bs with
      | some e, some rest => some (e :: rest)
      | _, _ => none
  | _, _ => none

/-! ### The join is symmetric; bracketing is not proved

§5.5 writes `join(Σ1, …, Σn)` with no order and no bracketing, and the
mechanization computes it as a left fold (below), so the two readings agree
only to the extent that the binary join is commutative and associative.

**Commutativity is proved.** `OwnSt.join_comm` holds of arbitrary states, and
`Entry.join_comm`/`Ctx.join_comm` lift it to same-skeleton entries and contexts
— which is every pair the rule joins, since the arms of a `match` or an `if`
extend one incoming context and `Typed.skel_preserved` keeps their skeletons
equal.

**Associativity is not proved, and is false in the generality the statement
would need.** `.fields` at a scalar type is a state no rule can write;
`ownedJoinOk` refuses it while `residualLinear` sees nothing in it, so at `int`
the two associations of `MovedOut`, `Owned`, `fields [Owned]` are `MovedOut`
and ill-formed respectively (`Examples.lean` pins the pair). Over states that
*are* well formed at their type it holds: it was checked exhaustively over a
13-declaration fixture and every well-formed `OwnSt` to depth 3 — commutativity,
associativity, and all six orders of three states, with no counterexample.
Proving it needs a state-against-type well-formedness invariant the fragment
does not carry today; that, and the permutation corollary for `Ctx.joinAll`
that would follow from it, are owed on RUE-2337.
-/

mutual
/-- **The §5.5 join is commutative**, at one path and its subtree. Joining is
symmetric in the two arms: where one side is wholly `Owned` the result is the
other side subject to `ownedJoinOk`, where one side is `MovedOut` the result is
`MovedOut` subject to the other's residue, and two field records join slot by
slot — each of which reads the same from either side. -/
theorem OwnSt.join_comm (D : Decls) : ∀ (a b : OwnSt) (T : Ty),
    OwnSt.join D a b T = OwnSt.join D b a T
  | .owned, .owned, _ => rfl
  | .owned, .movedOut, _ => rfl
  | .owned, .fields _, _ => rfl
  | .movedOut, .owned, _ => rfl
  | .movedOut, .movedOut, _ => rfl
  | .movedOut, .fields _, _ => rfl
  | .fields _, .owned, _ => rfl
  | .fields _, .movedOut, _ => rfl
  | .fields as, .fields bs, T => by
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [OwnSt.join, hd]
          | some sd => simp [OwnSt.join, hd, OwnSt.joinList_comm D as bs sd.fields]
      | array T' n => simp [OwnSt.join, OwnSt.joinList_comm D as bs (List.replicate n T')]
      | _ => simp [OwnSt.join]

/-- The same over a declaration's fields, slot by slot (helper). -/
theorem OwnSt.joinList_comm (D : Decls) : ∀ (as bs : List OwnSt) (Ts : List Ty),
    OwnSt.joinList D as bs Ts = OwnSt.joinList D bs as Ts
  | as, bs, [] => by cases as <;> cases bs <;> rfl
  | [], [], _ :: _ => rfl
  | [], _ :: _, _ :: _ => rfl
  | _ :: _, [], _ :: _ => rfl
  | a :: as, b :: bs, T :: Ts => by
      simp only [OwnSt.joinList, OwnSt.join_comm D a b T, OwnSt.joinList_comm D as bs Ts]
end

/-- **The §5.5 join is commutative on one entry**, whose skeleton the two arms
share — the entry's declared type and `mut` mark come from the incoming
context, so only the state differs. -/
theorem Entry.join_comm {D : Decls} {a b : Entry} (hsk : a.skel = b.skel) :
    Entry.join D a b = Entry.join D b a := by
  simp only [Entry.skel, Prod.mk.injEq] at hsk
  obtain ⟨hty, hmu⟩ := hsk
  have hset : a.setSt = b.setSt := by
    funext u
    simp [Entry.setSt, hty, hmu]
  simp only [Entry.join, hty, hset, OwnSt.join_comm D a.st b.st b.ty]

/-- **The §5.5 join is commutative on a whole context**, pointwise, whenever
the two arms carry the same skeleton — which `skel_preserved` guarantees of any
two outgoing contexts of one incoming one (`Typed.skel_preserved`). So which
arm the algorithm reads
first is immaterial; what is not proved is the bracketing (section docstring). -/
theorem Ctx.join_comm {D : Decls} : ∀ (Γ₁ Γ₂ : Ctx), Γ₁.skel = Γ₂.skel →
    Ctx.join D Γ₁ Γ₂ = Ctx.join D Γ₂ Γ₁
  | [], [], _ => rfl
  | [], _ :: _, h => by simp [Ctx.skel] at h
  | _ :: _, [], h => by simp [Ctx.skel] at h
  | a :: as, b :: bs, h => by
      simp only [Ctx.skel, List.map_cons, List.cons.injEq] at h
      simp only [Ctx.join, Entry.join_comm h.1, Ctx.join_comm as bs h.2]

/-! ### The n-way §5.5 join, and a `match` arm's own binders

(Match) §5.5 writes `Σ' = join(Σ1, …, Σn)` over one outgoing state per arm.
`join` is a binary least upper bound taken path by path, so the n-way form is
its **left fold** over the arms in declaration order, starting from the first
arm's state; `Ctx.joinAll` is that fold and `Ctx.joinFold` its accumulator step.
A one-armed `match` joins nothing and is the arm's own outgoing state, which is
`Ctx.joinFold`'s base case; a zero-armed one has no state to start from, so
`Ctx.joinAll []` is `none` — and §5.5 says why nothing needs it: every core enum
has at least one variant, the zero-arm `match` on an uninhabited scrutinee being
a surface form elaboration never brings here (§2's reachability pruning,
`10.5:4`).

The fold is the *computation* §5.5's unordered `join(Σ1, …, Σn)` is read as,
and it is what `Matches.joinFold` (`Soundness.lean`) consumes. That reading is
exact in one half and checked in the other: the binary join is **commutative**,
proved (`OwnSt.join_comm`, `Ctx.join_comm`), so which of two arms is taken
first does not matter; **associativity**, which is what would make the
bracketing immaterial and `Ctx.joinAll` invariant under a permutation of the
arms, is checked exhaustively over a fixture rather than proved, and is false
of states no rule can produce. The section above states both and RUE-2337 owes
the proof.
-/

/-- One arm's entry context: (Match) §5.5's `Γ, x_{i1}:Ti1, …, x_{i,ai}:Ti_{ai} ;
Σ0[ x_{ij} ↦ Owned ]`. The payload locals enter `Owned`, unmarked (§2 gives a
pattern binding no `μ`, so nothing may assign to one), and the list is
**reversed** for the reason `fnCtx` reverses a parameter list: `Ctx` is
innermost-binder-first while a payload tuple is written left to right, so
component 1 is the outermost of the arm's binders and component `ai` has de
Bruijn index `0`. -/
def armCtx (Ts : List Ty) (Γ : Ctx) : Ctx :=
  (Ts.map fun T => ({ ty := T, mu := false, st := .owned } : Entry)).reverse ++ Γ

/-- The accumulator step of (Match) §5.5's `join(Σ1, …, Σn)`: fold the binary
§5.5 join over the remaining arms' outgoing states, left to right. -/
def Ctx.joinFold (D : Decls) : Ctx → List Ctx → Option Ctx
  | acc, [] => some acc
  | acc, Γ :: Γs =>
      match Ctx.join D acc Γ with
      | some acc' => Ctx.joinFold D acc' Γs
      | none => none

/-- (Match) §5.5's `Σ' = join(Σ1, …, Σn)`: the n-way join of the arms' outgoing
states, as the left fold of the binary join (section docstring). -/
def Ctx.joinAll (D : Decls) : List Ctx → Option Ctx
  | [] => none
  | Γ :: Γs => Ctx.joinFold D Γ Γs

mutual
/-- `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` (§5), over the fused context, under the program `P`
and the enclosing function's return type `R`.

Rule names cite the calculus: `useCopy`/`useMove` are (Use-Copy)/(Use-Move)
(§5.1) and `useDeclared` is (Use-Declared-Linear-Destructure) §5.1, the
declared-linear destructure §4.2 selects by the `Declared(d, π_s)` plan, with
`dropDeclared` its `@drop` half (§5.3's "read the same way"); `binop` is
(Arith) and (Ord) at once, `neg`/`notOp`/`bitnot` are
(Neg)/(Not)/(BitNot), `intCast` is (Int-Cast) and `dbg` is (Dbg), all §5.8;
`dropCopy`/`dropRes` are (@Drop-Copy)/(@Drop) (§5.3); `mkStruct` is
(Struct-Intro) (§5.8), `mkEnum` is (Enum-Intro) (§5.5) and `mkArray` is
(Array-Intro) (§5.8); `repeatArray` is the surface repeat form §2 elaborates
away (`7.1:36`–`7.1:39`); `indexRead`/`indexWrite` are the dynamic index,
typed by (Use-Untrackable-Dynamic-Copy) §5.1 and by (Assign) §5.2; `«match»`
is (Match)
(§5.5), whose arms fold in §5.6's check for their payload locals and whose
outgoing states join n-way; `letIn` folds in §5.6's residual-linear scope-exit
check; `assign` is (Assign) with the `3.8:77` linear-overwrite premise, keyed
on the destination's type (`overwriteOk`), on the *post-RHS* state; `seq` is (Seq) with the `3.8:64` discard check; `ite` is (If)
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
  accepts the same programs either way. `declaredPrefix … = none` is §5.1's
  `plan_Γ(p) = Ordinary(Copy, T)`: the ordinary rules "are read only with an
  `Ordinary` plan", which is what keeps this rule from overlapping
  `useDeclared` when the selected leaf is `Copy` (`Syntax.lean`). -/
  | useCopy {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.fullyOwned = true →
      en.ty.atPath P.decls p.path = some T →
      T.mult P.decls = .copy →
      declaredPrefix P.decls en.ty p.path = none →
      Typed P R Γ (.use p) T Γ
  /-- (Use-Move) §5.1: a use of an `Affine`/`Linear` place moves it out — at a
  projection, the **partial move** of `3.8:22`, which marks exactly `p` and
  removes every path under it while leaving `p`'s siblings alone.
  `fully-owned(Σ, p)` is the premise (`3.8:26`: handing an aggregate with a
  hole to a new owner is ill-formed), and `noDtorPrefix` is `3.9:34`'s
  restriction (E0456). §4.2's third restriction, `3.8:68`'s root-index rule, is
  **strengthened** here to `Place.noIdx`: this part moves no array element at
  all, which is a restriction of the fragment and not of the calculus
  (RUE-2327; `Syntax.lean`, "Arrays"). `declaredPrefix … = none` is §5.1's
  `Ordinary` plan premise, exactly as the `Copy` rule above carries it. -/
  | useMove {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.fullyOwned = true →
      en.ty.atPath P.decls p.path = some T →
      T.mult P.decls ≠ .copy →
      noDtorPrefix P.decls en.ty p.path = true →
      declaredPrefix P.decls en.ty p.path = none →
      p.noIdx = true →
      Typed P R Γ (.use p) T (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut)))
  /-- **(Use-Declared-Linear-Destructure) §5.1**, the declared-linear
  destructure of `3.8:33`: a use of a place whose path has a proper prefix of
  declared-`linear` struct type consumes that prefix — the **smallest**
  enclosing one, `d` — and produces the selected leaf, destroying `d`'s
  droppable residue on the way (§6.3's `destructure`).

  The premises are the rule's, in its order. `declaredPrefix` is §4.2's
  `plan_Γ(p) = Declared(d, π_s)`, and it carries the rule's second premise with
  it: `Γ ⊢ d : S` with `S` declared `linear` is
  `declaredPrefix_declaredLinear` (`Syntax.lean`) rather than a premise here.
  `fully-owned(Σ, d)` is asked of `d`, not of `p` — the rule hands a new owner
  the leaf and destroys the rest, so the whole subtree must be there
  (`3.8:26`). `linearResidue = false` is `¬ linear-residue(S, π_s)`, the
  premise that rejects the access "before any residue can be silently dropped"
  (`3.8:60`, E0474). `noDtorPrefix` is read over the **whole** path, which is
  the rule's "no proper prefix `q` of `p` has a user-defined destructor — every
  enclosing value, including `d`" (`3.9:34`, E0456). And `T` is the leaf's
  type, bound by the rule's `Γ ⊢ p : T`.

  `Place.noIdx` is the array part's own restriction, carried here for the
  reason (Use-Move) carries it: a destructure moves the leaf out, and this part
  moves nothing out of an array element (RUE-2327; `Syntax.lean`, "Arrays").
  So a plan whose **selected path** passes through an index step — `x.arr[0]`
  on a declared-`linear` `x`, probe d9b — is refused here although the calculus
  accepts it. A retained *array* in the residue needs nothing of the sort
  (probe d9).

  The Σ effect is §5.1's move effect, taken at `d`: `Σ[ d ↦ MovedOut, and
  every path strictly under d removed ]`. Nothing else in the context moves, so a
  declared-linear **ancestor** of `d` stays `Owned` and keeps its own
  obligation (§5.6's declared clause), and a sibling of `d` keeps its own
  state — which is what makes `h.l.a` consume `h.l` alone (probe d4). Because
  the rule is selected by the *plan* rather than by `class(T)`, it fires at a
  `Copy` leaf too: that is §4.2's "central override", and probe d1 is it. -/
  | useDeclared {Γ p en u πd πs Td T} :
      Γ[p.root]? = some en →
      declaredPrefix P.decls en.ty p.path = some (πd, πs) →
      en.st.get πd = some u → u.fullyOwned = true →
      en.ty.atPath P.decls πd = some Td →
      linearResidue P.decls Td πs = false →
      en.ty.atPath P.decls p.path = some T →
      noDtorPrefix P.decls en.ty p.path = true →
      p.noIdx = true →
      Typed P R Γ (.use p) T (Γ.set p.root (en.setSt (en.st.setAt πd .movedOut)))
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
      P.decls.structs[s]? = some sd →
      TypedArgs P R Γ args sd.fields Γ' →
      Typed P R Γ (.mkStruct s args) (.struct s) Γ'
  /-- (Enum-Intro) §5.5: one payload argument per declared component of the
  variant the tag names, typed left to right at its component's type with Σ
  threaded (§6.2's order, the same `TypedArgs` (Struct-Intro) uses), and the
  result owns the tag and the supplied payload — which is why `class(E)` is the
  payload join of §3 (`6.3:19`). The tag is the variant's declaration slot, so
  `variants[k]? = some Ts` is both §5.5's `E = enum { …, Kj(T̄j), … }` premise
  and `6.3:16`'s "the variant exists" (E0420 otherwise); the argument count is
  `6.3:16`'s arity premise, carried by `TypedArgs`' own shape. -/
  | mkEnum {Γ Γ' e k args ed Ts} :
      P.decls.enums[e]? = some ed →
      ed.variants[k]? = some Ts →
      TypedArgs P R Γ args Ts Γ' →
      Typed P R Γ (.mkEnum e k args) (.enum e) Γ'
  /-- (Match) §5.5, the elimination form for enums.

  The scrutinee is typed first, at the enum type, and its Σ effect is whatever
  typing it did: at a place that is (Use-Copy)/(Use-Move) §5.1 by `class(E)` —
  a non-`Copy` enum is *consumed* by the match, because a scrutinee is a value
  context and a use of a move-type place there moves it (`3.8:7`, `3.8:76`;
  `6.3:17` for the payload the arm binds out of it, and not `3.8:33`'s
  declared-`linear` destructure, which is a rule this fragment does not
  mechanize), and a second `match` on it is then the use of a moved-out place
  the compiler reports as E0205 (`3.8:5`).

  Exhaustiveness is the arm list's **shape**: `arms.length =
  ed.variants.length`, with arm `j` the arm for variant `j`, so §5.5's "exactly
  the variants K1..Kn" needs no coverage search and no ordering side condition
  (`4.7:9`, `4.7:10`'s enum clause; the wildcard, the repeated pattern and the
  first-match order are elaboration obligations §5.5 states). Progress rests on
  it: `exhaustive_arm_exists` (`Soundness.lean`) is that a well-typed tag has an arm.

  Each arm is typed from the **same** post-scrutinee state `Σ0` under its
  payload locals (`armCtx`), all arms at one type `T` — the premise a diverging
  arm satisfies through `Typed.ret`/`Typed.panic`, which conclude at any type
  and any same-skeleton context, exactly as an `ite` arm does (§5.7's
  (Sub-Never), and the `⊥` a join reads nothing from). At the arm's end the
  payload locals leave scope under §5.6: `TypedArms` carries the same
  residual-linear check `Typed.letIn` carries for its one binder, over the
  `ai` entries the arm pops. The outgoing states then join n-way
  (`Ctx.joinAll`). -/
  | «match» {Γ Γ₀ Γ' Γs scrut arms e ed T} :
      Typed P R Γ scrut (.enum e) Γ₀ →
      P.decls.enums[e]? = some ed →
      arms.length = ed.variants.length →
      TypedArms P R Γ₀ arms ed.variants T Γs →
      Ctx.joinAll P.decls Γs = some Γ' →
      Typed P R Γ (.«match» scrut arms) T Γ'
  /-- (Array-Intro) §5.8: all `n` elements share one element type `T`
  (`3.5:3`, `7.1:3`), are typed left to right with Σ threaded, and the array
  owns all of them — which is why `class([T; n])` is §3's lift of `class(T)`.
  `n` is the literal's own length (`7.1:4` — the declared size must match), and
  `n = 0` is admitted: `[]` is the zero-sized `[T; 0]` and uses nothing. The
  element-type list is `List.replicate n T`, so this rule is
  (Struct-Intro)'s `TypedArgs` at a constant field list. -/
  | mkArray {Γ Γ' T args} :
      TypedArgs P R Γ args (List.replicate args.length T) Γ' →
      Typed P R Γ (.mkArray T args) (.array T args.length) Γ'
  /-- The surface repeat form `[e; n]` (`7.1:36`–`7.1:39`), whose element type
  `7.1:38` restricts to `Copy` (E0905, probe `a2b`).

  §2's elaboration inventory gives this form **no core image**: it elaborates
  to `let t = e; [t, …, t]`, "one evaluation of the operand, then `n`
  value-context *copies* (§4.2)", precisely because the `Copy` restriction
  makes those copies free. The form is kept here as a rule of its own so the
  printer can emit the surface spelling the compiler's E0905 is about and so
  the bridge exercises it; the premise and the dynamics are exactly that
  elaboration's, and `Ty.mult P.decls T = .copy` is `7.1:38`. That the
  calculus and this rule agree is by construction and not by a theorem — it is
  named as a deviation in `../03-metatheory.md`. -/
  | repeatArray {Γ Γ' T e n} :
      Typed P R Γ e T Γ' → T.mult P.decls = .copy →
      Typed P R Γ (.repeatArray T e n) (.array T n) Γ'
  /-- (Use-Untrackable-Dynamic-Copy) §5.1, at the read `p[e]` whose index is
  not a compile-time constant: §4.2's `Untrackable(OrdinaryDynamic)` plan, and
  the *only* successful static rule for it. `class(T) = Copy` is the rule's own
  premise, and §4.2's "there is no successful static rule … when
  `class(T) ∈ {Affine,Linear}`" is that premise's absence rather than a
  rejection of its own (E0904, probe `a5`). `fully-owned(Σ, p)` is stronger
  than §5.1's `Σ(p) = Owned` and is `3.8:70`/`7.1:45`'s own rule: "it is a
  compile-time error … to index the array with a non-constant index" while an
  element is moved out, "because the compiler cannot know at compile time
  whether a runtime index denotes a moved-out element". `declaredPrefix … =
  none` keeps §4.2's `Untrackable(DeclaredLinearDynamic)` — ill-formed there —
  without an instance: a base under a declared-`linear` prefix draws the
  `Declared` plan, and this rule refuses it. The index is typed **first** and Σ threaded through it, and the
  base place is read on the resulting context; `eval` runs the two in the same
  order. `4.11:14` states the opposite for a full index *expression* — "the
  base expression is evaluated before the index expression" — and §6.2's
  `E[e]`/`v[E]` contexts are that order. It is unobservable here because the
  base is a `Place`, not an expression: reading a place runs nothing, allocates
  nothing and threads no Σ of its own, so the two orders agree on every
  program. The order becomes observable only when a base expression can have
  an effect, which is a form this fragment does not have. The read copies, so
  the outgoing state is the index's. Whether the index is *in range*
  is dynamic (`7.1:10`, §6.5's (D-Index-Trap)), not a typing question. -/
  | indexRead {Γ Γ₁ p e en u T n w s} :
      Typed P R Γ e (.int w s) Γ₁ →
      Γ₁[p.root]? = some en →
      en.st.get p.path = some u → u.fullyOwned = true →
      en.ty.atPath P.decls p.path = some (.array T n) →
      T.mult P.decls = .copy →
      declaredPrefix P.decls en.ty p.path = none →
      Typed P R Γ (.indexRead p e) T Γ₁
  /-- (Assign) §5.2 at a dynamic index, `p[e₁] = e₂` (`7.1:30`, `4.11:12`): an
  in-place mutation that modifies the array without moving it. The root must be
  a `μ = mut` binding (§5 preamble), and the index reduces before the
  right-hand side (§6.2: "`assign p = E` — right-hand side (`p`'s index
  subexpressions reduce first)") with Σ threaded in that order.

  The destination is **not** a use, so the read's `class(T) = Copy` premise
  does not transfer here: §4.2's plans classify a value-context use, and what
  (Assign) demands of a destination is its own last premise,
  `Σ1(p) = MovedOut ∨ ¬carries_linear(T)` — `overwriteOk`/`3.8:77`, the same
  premise `Typed.assign` carries, read at the **element** type on the post-RHS
  state. A runtime index can never establish `MovedOut` at the element, so the
  disjunction bites as its right half: an affine, even destructor-bearing,
  element type is admitted (and the machine's overwrite-drop below runs its
  glue), while a linear-carrying one is refused, which is the compiler's E0493.

  There is **no plan premise**: §4.2's plans classify value-context uses,
  and an assignment destination is not one, so a dynamic-index write into an
  array field of a declared-`linear` struct (`v0.x0[i] = 9`) is admitted here
  exactly as the compiler admits it (second-review probe c3, which prints
  `1 9 2 7`). The write lands on an element the declared-`linear` place still
  owns whole — `fully-owned` below is what guards that — and consumes nothing,
  so `Untrackable(DeclaredLinearDynamic)` has no instance at a write; its one
  instance is the dynamic *read*, which `indexRead` refuses. The arrays part
  carried `declaredPrefix … = none` here as a restriction of its own; it is
  dropped with the destructure mechanized.

  Two more of (Assign)'s clauses are discharged rather than restated.
  `3.8:72`/`7.1:46` — "while one or more elements of an array are moved out, it
  is a compile-time error to assign into the array" — is `fully-owned(Σ, p)` on
  the post-RHS state. And `3.8:55`'s reinitialization is (Assign)'s own
  `Σ1[p ↦ Owned]`, taken at the **whole array** rather than at the element:
  `7.1:46` says an element write "does not reinstate per-element ownership",
  and on the `fully-owned` premise there is nothing to reinstate, so writing
  `Owned` at `p` is the rule as §5.2 states it and changes no path's state.

  `en₀.st.get p.path = some u₀` constrains `u₀` nowhere, and deliberately: it
  is (Assign)'s own incoming `Σ(p)` lookup, whose content is that the
  destination path is *reachable* — `OwnSt.get` is `none` under a moved-out
  prefix — while every condition on the state itself is read after the operands
  have run, on `u₁`, because that is the state the write overwrites. -/
  | indexWrite {Γ Γ₁ Γ₂ p e₁ e₂ en₀ en₁ u₀ u₁ T n w s} :
      Γ[p.root]? = some en₀ → en₀.mu = true →
      en₀.st.get p.path = some u₀ →
      en₀.ty.atPath P.decls p.path = some (.array T n) →
      Typed P R Γ e₁ (.int w s) Γ₁ →
      Typed P R Γ₁ e₂ T Γ₂ →
      Γ₂[p.root]? = some en₁ →
      en₁.st.get p.path = some u₁ → u₁.fullyOwned = true →
      (u₁ = .movedOut ∨ T.mult P.decls ≠ .linear) →
      Typed P R Γ (.indexWrite p e₁ e₂) .unit
        (Γ₂.set p.root (en₁.setSt (en₁.st.setAt p.path .owned)))
  /-- (@Drop-Copy) §5.3: no drop glue, no ownership effect. §5.3 gives it
  neither of (@Drop)'s projection premises — a `Copy` place is moved by
  nothing — so only the `Ordinary` plan premise is added: §5.3 says the two
  `@drop` rules "are read the same way" as §5.1's two use rules, which is
  `declaredPrefix … = none`. The subtree condition is read the way the `Copy`
  use rule above reads it, for the same reason and at the same cost (none). -/
  | dropCopy {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.fullyOwned = true →
      en.ty.atPath P.decls p.path = some T →
      T.mult P.decls = .copy →
      declaredPrefix P.decls en.ty p.path = none →
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
  and the compiler enforces it (E0406). `Place.noIdx` is this part's own
  restriction, exactly as on (Use-Move) above: §5.3 records that `@drop(a[0])`
  at the root *is* accepted by the compiler and drops exactly that element, so
  refusing it is RUE-2327's debt and not the calculus's rule. -/
  | dropRes {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.isOwned = true →
      en.ty.atPath P.decls p.path = some T →
      T.mult P.decls ≠ .copy →
      noDtorPrefix P.decls en.ty p.path = true →
      declaredPrefix P.decls en.ty p.path = none →
      (u.fullyOwned = true ∨ residualLinearBelow P.decls u T = false) →
      p.noIdx = true →
      Typed P R Γ (.drop p) .unit (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut)))
  /-- **(@Drop) §5.3 at a declared-linear plan**, the `@drop` half of the
  destructure. §5.3 states it in prose rather than as a fourth rule: the two
  `@drop` rules "are read the same way" as §5.1's two use rules, so
  "`@drop(p)` leaves `p` `MovedOut`, so where elaboration records
  `Declared(d, π)` for `p` the intrinsic consumes `d` and destroys its
  droppable residue exactly as a use does, rather than marking the projected
  leaf alone."

  So the premises are `Typed.useDeclared`'s, verbatim, and there is **no
  premise on the leaf's class**: §5.3 is explicit that the whole of `d` is
  consumed "for a `Copy` field `f` as much as for a droppable one", and the
  compiler agrees —
  after `@drop(d.f)` at a `Copy` field, a later use of `d` is E0205 (probe
  d6/d6b). That is the one place where `@drop` at a `Copy` place is not a
  no-op, and it is why this rule is not folded into `dropCopy`. The **prose
  spec** does not say it yet: `3.9:37-39` describe `@drop` at the named place
  only, and `3.9:39`'s "applied to a `@copy` value, it is a no-op" is about
  that place, not about a `Copy` leaf reached through a declared-`linear`
  prefix. The rule follows the calculus §5.3 and `3.8:33`'s destructure, which
  the compiler matches; RUE-2338 is the spec paragraph that is owed.

  What the dynamics adds over a use is only the leaf: §6.3's `destructure`
  runs the residue's drops, and then §6.11 drops the selected leaf itself
  (probe d6c fixes the order — residue first, leaf second). `Place.noIdx` is
  carried for the reason (@Drop) above carries it, and refuses a selected path
  through an index step (RUE-2327). -/
  | dropDeclared {Γ p en u πd πs Td T} :
      Γ[p.root]? = some en →
      declaredPrefix P.decls en.ty p.path = some (πd, πs) →
      en.st.get πd = some u → u.fullyOwned = true →
      en.ty.atPath P.decls πd = some Td →
      linearResidue P.decls Td πs = false →
      en.ty.atPath P.decls p.path = some T →
      noDtorPrefix P.decls en.ty p.path = true →
      p.noIdx = true →
      Typed P R Γ (.drop p) .unit (Γ.set p.root (en.setSt (en.st.setAt πd .movedOut)))
  /-- (Let) + §5.6 scope exit: the binder enters `Owned`; at the body's end
  its residual state must not be an unconsumed linear value (the leak check).
  An `Owned` affine residue is dropped by the machine (§6.7); `MovedOut` needs
  nothing. -/
  | letIn {Γ Γ₁ Γ₂ m e₁ e₂ T₁ T₂ en'} :
      Typed P R Γ e₁ T₁ Γ₁ →
      Typed P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ T₂ (en' :: Γ₂) →
      residualLinear P.decls en'.st en'.ty = false →
      Typed P R Γ (.letIn m e₁ e₂) T₂ Γ₂
  /-- (Assign) §5.2, at a place: the root must be a `μ = mut` binding (§5
  preamble), the RHS runs first, the overwrite of live linear content is
  ill-formed (`3.8:77`, checked on the **post-RHS** state — the RUE-387
  premise, and the `Σ1` reading that makes `p = f(p)` legal), and the subtree
  at `p` becomes `Owned` afterward (reinitialization, `3.8:55`). The `get`
  premises are `Owned-Base` (`3.8:53`) at both states: a path under a moved
  prefix is not a path to assign to, which the compiler reports as E0205.

  The `3.8:77` premise is §5.2's disjunction **as written** —
  `Σ1(p) = MovedOut ∨ ¬carries_linear(T)`, on the destination's declared type —
  and not §5.6's residual reading. `overwriteOk`'s docstring says why: an
  overwrite discharges nothing, so the argument that made §5.5 and §5.6
  state-keyed (RUE-526, RUE-1591) does not transfer, and the compiler rejects
  the shape the residual reading would accept (E0493 on
  `@drop(v.linearField); v = …`; corpus case `overwrite_past_partial_linear`).

  One **deviation** (N3): `Owned-Base` is demanded on the *incoming* state as
  well as the post-RHS one, so this rule is one premise stricter than §5.2,
  which states neither (U4 reads §5.1's "in any context" side condition for the
  post-RHS lookup). Nothing a program can observe turns on it: only an RHS that
  reinitialises the target's own moved-out prefix could make the incoming
  lookup fail where the post-RHS one succeeds. -/
  | assign {Γ Γ₁ p e en₀ en₁ u₀ u₁ T} :
      Γ[p.root]? = some en₀ → en₀.mu = true →
      en₀.st.get p.path = some u₀ →
      en₀.ty.atPath P.decls p.path = some T →
      Typed P R Γ e T Γ₁ →
      Γ₁[p.root]? = some en₁ →
      en₁.st.get p.path = some u₁ →
      (u₁ = .movedOut ∨ T.mult P.decls ≠ .linear) →
      Typed P R Γ (.assign p e) .unit (Γ₁.set p.root (en₁.setSt (en₁.st.setAt p.path .owned)))
  /-- (Seq): the discarded value must not carry a linear value (`3.8:64`). -/
  | seq {Γ Γ₁ Γ₂ e₁ e₂ T₁ T₂} :
      Typed P R Γ e₁ T₁ Γ₁ → T₁.mult P.decls ≠ .linear →
      Typed P R Γ₁ e₂ T₂ Γ₂ →
      Typed P R Γ (.seq e₁ e₂) T₂ Γ₂
  /-- (If): both arms from the post-scrutinee state; outgoing state is the
  §5.5 join. -/
  | ite {Γ Γ₀ Γ₁ Γ₂ Γ' c e₁ e₂ T} :
      Typed P R Γ c .bool Γ₀ →
      Typed P R Γ₀ e₁ T Γ₁ → Typed P R Γ₀ e₂ T Γ₂ →
      Ctx.join P.decls Γ₁ Γ₂ = some Γ' →
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
      NoResidualLinear P.decls Γ₁ →
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

/-- (Match) §5.5's arm premises: one arm per variant, **each from the same
post-scrutinee state `Σ0`** (a `match` is a branch, not a sequence, so Σ is not
threaded from arm to arm) and each at the one type `T` the rule concludes at.

An arm carries two premises of its own. Its body is typed under the variant's
payload locals (`armCtx`), and at its end those locals leave scope under §5.6
— `NoResidualLinear` over the `ai` entries the arm pops is the leak check
`Typed.letIn` makes for its single binder, read over the whole payload
(`6.3:17`: a `Linear` payload an arm neither moves nor consumes is a leak; an
`Affine` one the machine drops once). The arm's contribution to the join is what
is left after popping them. -/
inductive TypedArms (P : Program) (R : Ty) : Ctx → List Expr → List (List Ty) → Ty →
    List Ctx → Prop where
  /-- No arms left to type, and so no state to contribute. -/
  | noArms {Γ₀ T} : TypedArms P R Γ₀ [] [] T []
  /-- The arm for the next variant: its body typed under that variant's payload
  locals, those locals discharged by §5.6 at the arm's end, and the rest of the
  arms typed from the same `Σ0`. -/
  | arm {Γ₀ Γb Γs e es Ts Tss T} :
      Typed P R (armCtx Ts Γ₀) e T Γb →
      NoResidualLinear P.decls (Γb.take Ts.length) →
      TypedArms P R Γ₀ es Tss T Γs →
      TypedArms P R Γ₀ (e :: es) (Ts :: Tss) T (Γb.drop Ts.length :: Γs)
end

/-- **(Match) §5.5's premises for the arm a tag selects.** Read at the variant
index `k`: the arm's body is typed under that variant's payload locals, its
locals are discharged by §5.6 at the arm's end, and what it contributes to the
n-way join is one of the states the join was taken over. This is the inversion
`soundness` performs once (D-Match) §6.6 has read the tag (helper). -/
theorem TypedArms.at_index {P : Program} {R : Ty} {Γ₀ : Ctx} {T : Ty} :
    ∀ {arms : List Expr} {Tss : List (List Ty)} {Γs : List Ctx},
      TypedArms P R Γ₀ arms Tss T Γs →
      ∀ (k : Nat) {body : Expr} {Ts : List Ty}, arms[k]? = some body → Tss[k]? = some Ts →
        ∃ Γb, Typed P R (armCtx Ts Γ₀) body T Γb ∧
          NoResidualLinear P.decls (Γb.take Ts.length) ∧ (Γb.drop Ts.length) ∈ Γs
  | _, _, _, .noArms, _, _, _, ha, _ => by simp at ha
  | _, _, _, .arm hbody hres _, 0, _, _, ha, ht => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at ha ht
      subst ha; subst ht
      exact ⟨_, hbody, hres, List.mem_cons_self⟩
  | _, _, _, .arm _ _ hrest, (k + 1), _, _, ha, ht => by
      simp only [List.getElem?_cons_succ] at ha ht
      obtain ⟨Γb, h₁, h₂, h₃⟩ := TypedArms.at_index hrest k ha ht
      exact ⟨Γb, h₁, h₂, List.mem_cons_of_mem _ h₃⟩

/-- **Exhaustiveness gives the tag an arm** (§5.5): a `match` has exactly one
arm per variant, so a variant index the declaration has is an index the arm list
has. This is what progress at a `match` rests on — §7's own words: "a well-typed
enum value carries one of the declared tags, and the arm list covers every one,
so a `match` is never stuck on an uncovered tag" (`4.7:9`, `4.7:10`). Named for
what it says rather than for the form it is about, because Lean reserves the
`match_` prefix for the declarations its own `match` elaborator generates
(helper). -/
theorem exhaustive_arm_exists {arms : List Expr} {Tss : List (List Ty)} {k : Nat} {Ts : List Ty}
    (hlen : arms.length = Tss.length) (hv : Tss[k]? = some Ts) :
    ∃ body, arms[k]? = some body := by
  have hk : k < Tss.length := (List.getElem?_eq_some_iff.mp hv).1
  have hk' : k < arms.length := by omega
  exact ⟨arms[k], List.getElem?_eq_getElem hk'⟩

/-- (Fn) §5.8: a function is well-formed when its body checks at its declared
return type from the entry context `Γ0;Σ0` (`fnCtx`), and the body's normal
exit edge discharges §5.6's obligation for every by-value parameter and every
still-open body-local binding (`3.8:62` — a by-value parameter carrying a
linear value must be consumed on every non-diverging path). The rule's early
exits are covered by `Typed.ret`, which carries the same premise at the edge
where the frame's scopes end (§5.7's `⊥_exit`). -/
def WfFn (P : Program) (fd : FnDef) : Prop :=
  ∃ Γf, Typed P fd.ret (fnCtx fd) fd.body fd.ret Γf ∧ NoResidualLinear P.decls Γf

/-- A well-formed program: §3's class assignment holds of every declaration and
(Fn) §5.8 of every function. Recursion is ordinary — a body may call any
function of the program, itself included, since (Call) reads only the callee's
signature (§5.8, "the core is fully monomorphic") — while *declarations* are
not recursive at all (`3.0:5`, `WfNames`). -/
structure WfProgram (P : Program) : Prop where
  /-- §3's class assignment, for every declaration of either kind. -/
  decls : WfDecls P.decls
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
theorem Entry.join_skel {D : Decls} {a b e : Entry} (h : a.join D b = some e) :
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

/-- A `match` arm's entry context has the arm's payload locals on top of the
incoming skeleton, so popping them leaves that skeleton (helper). -/
theorem Ctx.skel_armCtx (Ts : List Ty) (Γ : Ctx) :
    Ctx.skel (armCtx Ts Γ) = (Ts.map fun T => (T, false)).reverse ++ Ctx.skel Γ := by
  simp [Ctx.skel, armCtx, Entry.skel, List.map_append, List.map_reverse]

/-- The context an arm hands the §5.5 join — its body's outgoing context with
the payload locals popped — has the skeleton the arm started from (helper). -/
theorem skel_drop_armCtx {Γb : Ctx} {Ts : List Ty} {Γ₀ : Ctx}
    (h : Ctx.skel Γb = Ctx.skel (armCtx Ts Γ₀)) :
    Ctx.skel (Γb.drop Ts.length) = Ctx.skel Γ₀ := by
  have hmap : Ctx.skel (Γb.drop Ts.length) = (Ctx.skel Γb).drop Ts.length := by
    simp [Ctx.skel, List.map_drop]
  rw [hmap, h, Ctx.skel_armCtx]
  have hlen : ((Ts.map fun T => (T, false)).reverse).length = Ts.length := by simp
  rw [← hlen, List.drop_left]

/-- The §5.5 join preserves the context skeleton (helper). -/
theorem Ctx.join_skel {D : Decls} : ∀ {Γ₁ Γ₂ Γ' : Ctx}, Ctx.join D Γ₁ Γ₂ = some Γ' →
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

/-- Every context in the list has the skeleton `Γ₀` has: what (Match) §5.5's
arms all share, since each extends `Σ0` and pops what it added. Written by
recursion rather than as `∀ Γᵢ ∈ Γs` so that the recursor's `match` case reads
it as a conjunction (helper). -/
def Ctx.SameSkel (Γ₀ : Ctx) : List Ctx → Prop
  | [] => True
  | Γ :: Γs => Ctx.skel Γ = Ctx.skel Γ₀ ∧ Ctx.SameSkel Γ₀ Γs

/-- `Ctx.SameSkel` read against another context of the same skeleton — which is
what lets the n-way join's accumulator stand in for `Σ0` (helper). -/
theorem Ctx.SameSkel.transport {Γ₀ Γ₁ : Ctx} (h : Ctx.skel Γ₁ = Ctx.skel Γ₀) :
    ∀ {Γs : List Ctx}, Ctx.SameSkel Γ₀ Γs → Ctx.SameSkel Γ₁ Γs
  | [], _ => trivial
  | _ :: _, hs => ⟨hs.1.trans h.symm, Ctx.SameSkel.transport h hs.2⟩

/-- `Ctx.SameSkel`, read at a member of the list (helper). -/
theorem Ctx.SameSkel.mem {Γ₀ : Ctx} : ∀ {Γs : List Ctx}, Ctx.SameSkel Γ₀ Γs →
    ∀ Γᵢ ∈ Γs, Ctx.skel Γᵢ = Ctx.skel Γ₀
  | [], _, _, hmem => by cases hmem
  | Γ :: Γs, h, Γᵢ, hmem => by
      cases hmem with
      | head => exact h.1
      | tail _ hrest => exact Ctx.SameSkel.mem h.2 Γᵢ hrest

/-- The accumulator step of (Match) §5.5's n-way join preserves the skeleton it
starts from (helper). -/
theorem Ctx.joinFold_skel {D : Decls} : ∀ (Γs : List Ctx) {acc Γ' : Ctx},
    Ctx.joinFold D acc Γs = some Γ' → Γ'.skel = acc.skel
  | [], acc, Γ', h => by
      simp only [Ctx.joinFold, Option.some.injEq] at h; rw [h]
  | Γ :: Γs, acc, Γ', h => by
      simp only [Ctx.joinFold] at h
      cases hj : Ctx.join D acc Γ with
      | none => rw [hj] at h; cases h
      | some acc' =>
          rw [hj] at h
          exact (Ctx.joinFold_skel Γs h).trans (Ctx.join_skel hj)

/-- (Match) §5.5's n-way join runs over a **non-empty** arm list — every core
enum has at least one variant (§5.5) — and preserves the first arm's skeleton
(helper). -/
theorem Ctx.joinAll_skel {D : Decls} : ∀ {Γs : List Ctx} {Γ' : Ctx},
    Ctx.joinAll D Γs = some Γ' → ∃ Γ₁ Γrest, Γs = Γ₁ :: Γrest ∧ Γ'.skel = Γ₁.skel
  | [], Γ', h => by simp [Ctx.joinAll] at h
  | Γ₁ :: Γrest, Γ', h => ⟨Γ₁, Γrest, rfl, Ctx.joinFold_skel Γrest h⟩

/-- Every rule preserves the context skeleton: only ownership states flow.
This is the fused context's image of §5's convention that `Γ` is fixed while
`Σ` is threaded through the judgment. The `ret` rule's arbitrary outgoing
context (§5.7's `⊥`) is restricted to the same skeleton for exactly this
reason. -/
theorem Typed.skel_preserved {P R} {Γ Γ' : Ctx} {e T} (h : Typed P R Γ e T Γ') :
    Γ'.skel = Γ.skel := by
  induction h using Typed.rec
    (motive_2 := fun Γ _ _ Γ' _ => Ctx.skel Γ' = Ctx.skel Γ)
    (motive_3 := fun Γ₀ _ _ _ Γs _ => Ctx.SameSkel Γ₀ Γs) with
  | intLit _ => rfl
  | boolLit => rfl
  | unitLit => rfl
  | useCopy _ _ _ _ _ _ => rfl
  | useMove hget _ _ _ _ _ _ _ => exact skel_set_setSt hget _
  | useDeclared hget _ _ _ _ _ _ _ _ => exact skel_set_setSt hget _
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
  | mkArray _ ih => exact ih
  | repeatArray _ _ ih => exact ih
  | indexRead _ _ _ _ _ _ _ ih => exact ih
  | indexWrite _ _ _ _ _ _ hget₁ _ _ _ ih₁ ih₂ =>
      exact (skel_set_setSt hget₁ _).trans (ih₂.trans ih₁)
  | mkEnum _ _ _ ih => exact ih
  | «match» _ _ _ _ hjoin ihs iharms =>
      obtain ⟨Γ₁, rest, rfl, hsk⟩ := Ctx.joinAll_skel hjoin
      exact hsk.trans (iharms.1.trans ihs)
  | noArms => trivial
  | arm _ _ _ ihbody iharms => exact ⟨skel_drop_armCtx ihbody, iharms⟩
  | dropCopy _ _ _ _ _ _ => rfl
  | dropRes hget _ _ _ _ _ _ _ _ => exact skel_set_setSt hget _
  | dropDeclared hget _ _ _ _ _ _ _ _ => exact skel_set_setSt hget _
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
    (motive_1 := fun Γ _ _ Γ' _ => Ctx.skel Γ' = Ctx.skel Γ)
    (motive_3 := fun Γ₀ _ _ _ Γs _ => Ctx.SameSkel Γ₀ Γs) with
  | intLit _ => rfl
  | boolLit => rfl
  | unitLit => rfl
  | useCopy _ _ _ _ _ _ => rfl
  | useMove hget _ _ _ _ _ _ _ => exact skel_set_setSt hget _
  | useDeclared hget _ _ _ _ _ _ _ _ => exact skel_set_setSt hget _
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
  | mkArray _ ih => exact ih
  | repeatArray _ _ ih => exact ih
  | indexRead _ _ _ _ _ _ _ ih => exact ih
  | indexWrite _ _ _ _ _ _ hget₁ _ _ _ ih₁ ih₂ =>
      exact (skel_set_setSt hget₁ _).trans (ih₂.trans ih₁)
  | mkEnum _ _ _ ih => exact ih
  | «match» _ _ _ _ hjoin ihs iharms =>
      obtain ⟨Γ₁, rest, rfl, hsk⟩ := Ctx.joinAll_skel hjoin
      exact hsk.trans (iharms.1.trans ihs)
  | noArms => trivial
  | arm _ _ _ ihbody iharms => exact ⟨skel_drop_armCtx ihbody, iharms⟩
  | dropCopy _ _ _ _ _ _ => rfl
  | dropRes hget _ _ _ _ _ _ _ _ => exact skel_set_setSt hget _
  | dropDeclared hget _ _ _ _ _ _ _ _ => exact skel_set_setSt hget _
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

/-- **Every arm of a `match` hands the §5.5 join a context with the skeleton the
arm started from** (§5's convention that `Γ` is fixed): the arm's payload locals
are popped, and the body preserved the rest. This is what lets the n-way join
read either the accumulated state or an arm's, which is the `match` case of
`soundness` (`Soundness.lean`) (helper). -/
theorem TypedArms.arm_skel {P R} {Γ₀ : Ctx} {arms Tss T} {Γs : List Ctx}
    (h : TypedArms P R Γ₀ arms Tss T Γs) : Ctx.SameSkel Γ₀ Γs := by
  induction h using TypedArms.rec
    (motive_1 := fun Γ _ _ Γ' _ => Ctx.skel Γ' = Ctx.skel Γ)
    (motive_2 := fun Γ _ _ Γ' _ => Ctx.skel Γ' = Ctx.skel Γ) with
  | intLit _ => rfl
  | boolLit => rfl
  | unitLit => rfl
  | useCopy _ _ _ _ _ _ => rfl
  | useMove hget _ _ _ _ _ _ _ => exact skel_set_setSt hget _
  | useDeclared hget _ _ _ _ _ _ _ _ => exact skel_set_setSt hget _
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
  | mkArray _ ih => exact ih
  | repeatArray _ _ ih => exact ih
  | indexRead _ _ _ _ _ _ _ ih => exact ih
  | indexWrite _ _ _ _ _ _ hget₁ _ _ _ ih₁ ih₂ =>
      exact (skel_set_setSt hget₁ _).trans (ih₂.trans ih₁)
  | mkEnum _ _ _ ih => exact ih
  | «match» _ _ _ _ hjoin ihs iharms =>
      obtain ⟨Γ₁, rest, rfl, hsk⟩ := Ctx.joinAll_skel hjoin
      exact hsk.trans (iharms.1.trans ihs)
  | noArms => trivial
  | arm _ _ _ ihbody iharms => exact ⟨skel_drop_armCtx ihbody, iharms⟩
  | dropCopy _ _ _ _ _ _ => rfl
  | dropRes hget _ _ _ _ _ _ _ _ => exact skel_set_setSt hget _
  | dropDeclared hget _ _ _ _ _ _ _ _ => exact skel_set_setSt hget _
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
