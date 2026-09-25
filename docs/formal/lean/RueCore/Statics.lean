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

§5.7 types `return e`, `@panic`, `break` and a `break`-less `loop` at `never`
and lets (Sub-Never) coerce them to any type, with a divergent outgoing state
`⊥` that §5.5's join excludes. The judgment carries §5.3's outgoing result
`Ω` (`Out`), whose `norm = none` is that `⊥`; the fragment folds the type
half into each never-typed rule, which concludes at **any** type `T`. `Ty`
therefore needs no `never` constructor and `HasTy` (`Soundness/Defs.lean`) no case
for it — sound because `never` has no values (`3.4:1`), so nothing is ever
typed at it dynamically. Adding the constructor would buy nothing here and
cost something: every rule that demands two equal types (§5.5's arms,
(Assign)'s target) would have to admit a subsumption it can never observe.
`INDEX.md` records (Sub-Never) as mechanized at the rules that fold it in.

The forms differ in what they owe, and the difference is §5.7's provenance.
`return` carries `⊥_exit`, which is the §5.6 scope-exit obligation taken
frame-wide, so `Typed.ret` demands `NoResidualLinear`. `@panic` carries
`⊥_panic`, which §5.7 exempts from that check — "§5.6 performs no scope-exit
check or drop on that edge" — so `Typed.panic` demands nothing of the
context, and §6.12's dynamics run no drop to match. `break` also carries
`⊥_exit`, but its scopes end at the loop it targets, so it delivers its state
there (`Typed.brk`) and the loop discharges the obligation for the scopes the
exit ends (`Typed.loopBreak`). A loop that never exits carries `⊥_diverge`,
checked frame-wide where it fires (`Typed.loopDiv`).

An *algorithm* cannot leave a type free, so `check` (`Checker/Defs.lean`) returns
`never` where a rule concludes at every type, and `Checker.lean`'s module docstring says
what completeness that costs.
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
lookup §3's join, and it is what `checkStructs` (`Checker/Defs.lean`) decides. -/
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
(`Checker/Defs.lean`) decides. -/
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
`checkNoCycle` (`Checker/Defs.lean`) decides it by peeling. -/

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
(`Checker/Defs.lean`) decides. -/
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

mutual
/-- Equality of ownership states is decidable, by structure; Lean's deriving
handler does not cover the nested `List OwnSt`, so it is written out. `check`
compares two states at §5.7's loop head (helper). -/
def OwnSt.decEq : (a b : OwnSt) → Decidable (a = b)
  | .owned, .owned => isTrue rfl
  | .movedOut, .movedOut => isTrue rfl
  | .fields as, .fields bs =>
      match OwnSt.decEqList as bs with
      | isTrue h => isTrue (h ▸ rfl)
      | isFalse h => isFalse (fun h' => by cases h'; exact h rfl)
  | .owned, .movedOut | .owned, .fields _ | .movedOut, .owned | .movedOut, .fields _
  | .fields _, .owned | .fields _, .movedOut => isFalse (fun h => by cases h)

/-- The same over a slot list (helper). -/
def OwnSt.decEqList : (as bs : List OwnSt) → Decidable (as = bs)
  | [], [] => isTrue rfl
  | [], _ :: _ | _ :: _, [] => isFalse (fun h => by cases h)
  | a :: as, b :: bs =>
      match OwnSt.decEq a b, OwnSt.decEqList as bs with
      | isTrue h₁, isTrue h₂ => isTrue (h₁ ▸ h₂ ▸ rfl)
      | isFalse h₁, _ => isFalse (fun h => by cases h; exact h₁ rfl)
      | _, isFalse h₂ => isFalse (fun h => by cases h; exact h₂ rfl)
end

instance : DecidableEq OwnSt := OwnSt.decEq

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
answers those slots at the **type** level, `Ts.any (·.mult D = .linear)`, and
that reading is **exact**, not conservative. An element Σ has no record for
is one no path has touched, and nothing below a dynamic index is ever moved:
the calculus has no rule for a move or a `@drop` of an affine or linear place
there, nor for a declared-`linear` plan (§4.2's `Untrackable` plans, E0904;
probes q02, q11, q15 of RUE-2342); a `@drop` of a `Copy` place there
(`Typed.indexDrop`) moves nothing, and a write there consumes nothing. So an untracked element is `Owned`, and
an `Owned` element carries a linear value exactly when `class(T) = Linear`. -/
def residualLinear (D : Decls) : OwnSt → Ty → Bool
  | .movedOut, _ => false
  | .owned, T => decide (T.mult D = .linear)
  | .fields ts, .struct s =>
      (match D.structs[s]? with
       | some sd => sd.attr = .linear || residualLinearFields D ts sd.fields
       | none => false)
  -- The array clause, and §5.6's second disjunct: see the docstring above. A
  -- partially-written array node (`a[0] = …`) and a partially *moved* one
  -- (`3.8:68`'s element move, `Syntax.lean`) both reach it.
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

/-- **(Assign) §5.2's array side condition** (`3.8:72`, `7.1:46`, E0480): a
write whose destination steps *into* an array demands that the array be
`fully-owned`. §5.2 states it in prose — "writing *into* an array while any
element is moved out is rejected by a side condition (`3.8:72`)" — and
`arrayPrefix` (`Syntax.lean`) is the path to the array it speaks of.

This is **one premise stricter** than §5.2's own disjunction read at the
element. `Σ1(p) = MovedOut ∨ ¬carries_linear(T)` at `a[c]` would admit
reinitializing exactly the element that was moved out; the spec forbids it
outright — "assigning into the array — to an element or through an element — is
itself an error … including at the exact constant index that was moved out"
(`3.8:77`, and `7.1:46`'s "an element write does not reinstate per-element
ownership") — and the compiler agrees: `a[0] = …` after `a[0]` moved is E0480
(probe a5), and so is `a[1].x0 = …` after `a[0]` moved (probe b1) and
`a[0].s = …` after `a[0].s` moved (probe b10). The model follows the spec; the
deviation from the calculus as written is recorded in §5.2 itself.

The premise applies to an array anywhere in the place tree (`3.8:71`), and
the compiler follows it there too: its E0480 check keys on the outermost array
the write steps into, wherever it sits (RUE-2341; it used to fire only when the
root binding was an array). Once a declared-linear destructure has holed
`h.arr[0]` through a struct root, a write *to* the element (`h.arr[0] = …`),
one *through* it (`h.arr[0].x0 = …`, `array_write_after_destructure_via_field`)
and one below a dynamic index (`h.arr[i].x0 = …`,
`array_dyn_write_after_destructure_via_field`) are all E0480.
`overwriteOk` alone would admit the write; this premise is what refuses it.
`soundness` does not use the premise. It is pinned by the refusal witnesses in
`Examples.lean` and by that corpus case.

Three things it deliberately does **not** forbid. Whole-array reassignment
`a = […]` is (Assign)'s ordinary case and is the spec's own recovery path
(`7.1:46`; probes b8, c3). An element write reached through a projection is
fine as long as the array is whole (`h.a[0] = …`, probe b6; `a[1][0] = …`,
probe c6) — a destination is not a move, so `rootIdxOnly` does not apply to it.
And the check is on the **post-RHS** state, like (Assign)'s other premises: the
compiler refuses `a[0] = g(a[0])` and `a[0] = g(a[1])`, whose only move is in
the right-hand side (probes c1, c2). -/
def assignArrayOk (D : Decls) (t : OwnSt) (T : Ty) (π : List Nat) : Bool :=
  match arrayPrefix D T π with
  | none => true
  | some πa =>
      match t.get πa with
      | some ua => ua.fullyOwned
      | none => false

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
deriving Repr, DecidableEq

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

/-! #### The states a type has (`OwnSt.wf`)

§5.5's join is associative over the states that are *shapes of their declared
type*, and not over the others: `.fields` at a scalar is a state no rule can
write, and the pair of associations it separates is pinned in `Examples.lean`.
`OwnSt.wf` is that invariant. `Owned` and `MovedOut` are states of every type;
a field record belongs to a declared `struct` or to an `array`, records no more
slots than that type has, and carries at each recorded slot a state of the
slot's own type. Those are exactly the shapes `OwnSt.setAt`'s padding writes
and `OwnSt.joinList` returns.
-/

mutual
/-- Whether an ownership state is a shape of the type it is recorded at (§5
preamble): `Owned` and `MovedOut` at every type, a field record only at a
declared `struct` or an `array`, no longer than that type's slots and with
every recorded slot a state of its own type. This is the invariant §5.5's
associativity is stated over (`OwnSt.join_assoc`). -/
def OwnSt.wf (D : Decls) : OwnSt → Ty → Bool
  | .owned, _ => true
  | .movedOut, _ => true
  | .fields ts, .struct s =>
      (match D.structs[s]? with
       | some sd => OwnSt.wfList D ts sd.fields
       | none => false)
  -- The array node, read element by element (`3.8:73`), as §5.5's own clauses
  -- read it.
  | .fields ts, .array T n => OwnSt.wfList D ts (List.replicate n T)
  | .fields _, _ => false

/-- The same over a declaration's slots: a record no longer than the slot list,
each recorded slot a state of its slot's type (helper). -/
def OwnSt.wfList (D : Decls) : List OwnSt → List Ty → Bool
  | [], _ => true
  | _ :: _, [] => false
  | t :: ts, T :: Ts => OwnSt.wf D t T && OwnSt.wfList D ts Ts
end

/-- One entry of §5's fused context is well formed when its state is a shape of
its declared type (helper). -/
def Entry.wf (D : Decls) (en : Entry) : Bool := OwnSt.wf D en.st en.ty

/-- §5.5's join is associative over contexts whose every entry is a shape of
its declared type; `Ctx.Wf` reads that invariant over a whole frame, the way
`NoResidualLinear` reads §5.6's. -/
def Ctx.Wf (D : Decls) (Γ : Ctx) : Prop := ∀ en ∈ Γ, Entry.wf D en = true

instance (D : Decls) (Γ : Ctx) : Decidable (Ctx.Wf D Γ) := by
  unfold Ctx.Wf; infer_instance

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

/-! ### The join is symmetric, and associative over well-formed states

§5.5 writes `join(Σ1, …, Σn)` with no order and no bracketing, and the
mechanization computes it as a left fold (below), so the two readings agree
exactly to the extent that the binary join is commutative and associative.
Both are proved.

**Commutativity.** `OwnSt.join_comm` holds of arbitrary states, and
`Entry.join_comm`/`Ctx.join_comm` lift it to same-skeleton entries and contexts
— which is every pair the rule joins, since the arms of a `match` or an `if`
extend one incoming context and `Typed.skel_preserved` keeps their skeletons
equal.

**Associativity.** `OwnSt.join_assoc` holds of states that are shapes of their
type (`OwnSt.wf`) under §3's class assignment for the struct layer
(`WfStructs`), and `Entry.join_assoc`/`Ctx.join_assoc` lift it the same way.
Both hypotheses are needed, and `Examples.lean` pins a counterexample to each.
Drop `OwnSt.wf` and `.fields` at a scalar type is a state no rule can write:
`ownedJoinOk` refuses it while `residualLinear` sees nothing in it, so at `int`
the two associations of `MovedOut`, `Owned`, `fields [Owned]` are `MovedOut`
and ill-formed respectively. Drop `WfStructs` and a declaration whose recorded
class is `Affine` over a `Linear` field separates the two associations of
`MovedOut`, `Owned`, `fields [MovedOut]` the same way — which is the
declaration `checkStructs` rejects, so the premise is one a well-formed program
already carries.

Two facts about a successful join carry the proof, and both are §5.5's reading
of §5.6 made precise. `OwnSt.join_ownedJoinOk`: a join neither adds nor removes
an inadmissible `MovedOut`, so both operands and the result answer
`ownedJoinOk` alike — which is what makes the `Owned` cases associate.
`OwnSt.join_residualLinear`: a join succeeds only between operands carrying the
same residual linear content, and the result carries the same; with
`OwnSt.join_exists`, its converse (two residue-free states always join), that
is what makes the `MovedOut` cases associate.

`Ctx.joinAll_perm` (the n-way section below) is the corollary §5.5's unordered
notation needs: the fold is invariant under a permutation of the arms.
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
arm the algorithm reads first is immaterial; `Ctx.join_assoc` gives the
bracketing, and `Ctx.joinAll_perm` the arm order of the n-way fold. -/
theorem Ctx.join_comm {D : Decls} : ∀ (Γ₁ Γ₂ : Ctx), Γ₁.skel = Γ₂.skel →
    Ctx.join D Γ₁ Γ₂ = Ctx.join D Γ₂ Γ₁
  | [], [], _ => rfl
  | [], _ :: _, h => by simp [Ctx.skel] at h
  | _ :: _, [], h => by simp [Ctx.skel] at h
  | a :: as, b :: bs, h => by
      simp only [Ctx.skel, List.map_cons, List.cons.injEq] at h
      simp only [Ctx.join, Entry.join_comm h.1, Ctx.join_comm as bs h.2]

/-- §3's class of an array type reaches `Linear` exactly through a nonempty
array of a `Linear` element type — `Ty.mult`'s own four-line table read as the
biconditional the join proofs need (`3.8:74`; the zero-length reading is
RUE-526's) (helper). -/
theorem Ty.array_mult_linear (D : Decls) (T : Ty) (n : Nat) :
    (Ty.array T n).mult D = .linear ↔ (n ≠ 0 ∧ T.mult D = .linear) := by
  cases n <;> cases h : Ty.mult D T <;> simp [Ty.mult, h]

/-- The same fact in the shape §5.5's array clauses use it: an array node's slot
types are `List.replicate n T`, so asking whether any slot carries a linear
value is asking `class([T; n]) = Linear` (helper). -/
theorem Ty.any_replicate_mult_linear (D : Decls) (T : Ty) (n : Nat) :
    ((List.replicate n T).any fun U => decide (U.mult D = .linear))
      = decide ((Ty.array T n).mult D = .linear) := by
  simp [List.any_replicate, Ty.array_mult_linear]

mutual
/-- **Residue is only ever found where §3's class puts it.** §5.6's
`residual-linear` is read on the state, not on the type, but it can report an
obligation only at a path whose type carries one, so a state with residue is a
state of a `Linear` type (`3.8:58`, through `struct_carriesLinear_iff`). This
is one half of the correspondence §5.5's `Owned` arm turns on. -/
theorem residualLinear_mult_linear {D : Decls} (hD : WfStructs D) :
    ∀ (t : OwnSt) (T : Ty), residualLinear D t T = true → T.mult D = .linear
  | .owned, _, h => by simpa [residualLinear] using h
  | .movedOut, _, h => by simp [residualLinear] at h
  | .fields ts, T, h => by
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [residualLinear, hd] at h
          | some sd =>
              simp only [residualLinear, hd, Bool.or_eq_true, decide_eq_true_eq] at h
              refine (struct_carriesLinear_iff hd (hD s sd hd)).2 ?_
              rcases h with hattr | hfields
              · exact Or.inl hattr
              · exact Or.inr (List.any_eq_true.1 (residualLinearFields_mult_linear hD ts sd.fields hfields)
                  |>.imp fun U hU => ⟨hU.1, of_decide_eq_true hU.2⟩)
      | array T' n =>
          simp only [residualLinear] at h
          have := residualLinearFields_mult_linear hD ts (List.replicate n T') h
          rw [Ty.any_replicate_mult_linear] at this
          exact of_decide_eq_true this
      | int _ _ => simp [residualLinear] at h
      | float _ => simp [residualLinear] at h
      | bool => simp [residualLinear] at h
      | unit => simp [residualLinear] at h
      | enum _ => simp [residualLinear] at h

/-- The same over a declaration's slots (helper). -/
theorem residualLinearFields_mult_linear {D : Decls} (hD : WfStructs D) :
    ∀ (ts : List OwnSt) (Ts : List Ty), residualLinearFields D ts Ts = true →
      (Ts.any fun U => decide (U.mult D = .linear)) = true
  | [], _, h => by simpa [residualLinearFields] using h
  | _ :: _, [], h => by simp [residualLinearFields] at h
  | t :: ts, T :: Ts, h => by
      simp only [residualLinearFields, Bool.or_eq_true] at h
      simp only [List.any_cons, Bool.or_eq_true]
      rcases h with h | h
      · exact Or.inl (decide_eq_true (residualLinear_mult_linear hD t T h))
      · exact Or.inr (residualLinearFields_mult_linear hD ts Ts h)
end

mutual
/-- **The other half: what an `Owned` arm may absorb still carries the type's
obligation.** `ownedJoinOk` admits exactly the `MovedOut` paths whose type is
not `Linear` (`3.8:50`), so a state it admits at a `Linear` type still has
residue somewhere — a wholly moved-out linear subtree is what it refuses. -/
theorem ownedJoinOk_residualLinear {D : Decls} (hD : WfStructs D) :
    ∀ (t : OwnSt) (T : Ty), ownedJoinOk D t T = true → T.mult D = .linear →
      residualLinear D t T = true
  | .owned, _, _, hlin => by simp [residualLinear, hlin]
  | .movedOut, _, h, hlin => by simp [ownedJoinOk, hlin] at h
  | .fields ts, T, h, hlin => by
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [ownedJoinOk, hd] at h
          | some sd =>
              simp only [ownedJoinOk, hd] at h
              simp only [residualLinear, hd, Bool.or_eq_true]
              rcases (struct_carriesLinear_iff hd (hD s sd hd)).1 hlin with hattr | ⟨U, hmem, hU⟩
              · exact Or.inl (decide_eq_true hattr)
              · exact Or.inr (ownedJoinOkList_residualLinearFields hD ts sd.fields h
                  (List.any_eq_true.2 ⟨U, hmem, decide_eq_true hU⟩))
      | array T' n =>
          simp only [ownedJoinOk] at h
          simp only [residualLinear]
          exact ownedJoinOkList_residualLinearFields hD ts (List.replicate n T') h
            (by rw [Ty.any_replicate_mult_linear]; exact decide_eq_true hlin)
      | int _ _ => simp [ownedJoinOk] at h
      | float _ => simp [ownedJoinOk] at h
      | bool => simp [ownedJoinOk] at h
      | unit => simp [ownedJoinOk] at h
      | enum _ => simp [ownedJoinOk] at h

/-- The same over a declaration's slots (helper). -/
theorem ownedJoinOkList_residualLinearFields {D : Decls} (hD : WfStructs D) :
    ∀ (ts : List OwnSt) (Ts : List Ty), ownedJoinOkList D ts Ts = true →
      (Ts.any fun U => decide (U.mult D = .linear)) = true →
      residualLinearFields D ts Ts = true
  | [], _, _, hany => by simpa [residualLinearFields] using hany
  | _ :: _, [], _, hany => by simp at hany
  | t :: ts, T :: Ts, h, hany => by
      simp only [ownedJoinOkList, Bool.and_eq_true] at h
      simp only [List.any_cons, Bool.or_eq_true] at hany
      simp only [residualLinearFields, Bool.or_eq_true]
      rcases hany with hT | hTs
      · exact Or.inl (ownedJoinOk_residualLinear hD t T h.1 (of_decide_eq_true hT))
      · exact Or.inr (ownedJoinOkList_residualLinearFields hD ts Ts h.2 hTs)
end

mutual
/-- **A residue-free state answers `ownedJoinOk` exactly as `MovedOut` does**:
joining it with a wholly `Owned` arm is admissible exactly when `class(T)` is
not `Linear`, which is the same test §5.5 applies at the `MovedOut`/`Owned`
disagreement (`3.8:50`). This is the step that needs `OwnSt.wf`: at a type with
no slots, `.fields` is a state `ownedJoinOk` refuses and `residualLinear`
cannot see, and that gap is `Examples.lean`'s counterexample to associativity
without the invariant. -/
theorem ownedJoinOk_of_residualLinear_false {D : Decls} (hD : WfStructs D) :
    ∀ (t : OwnSt) (T : Ty), OwnSt.wf D t T = true → residualLinear D t T = false →
      ownedJoinOk D t T = decide (T.mult D ≠ .linear)
  | .owned, _, _, h => by
      simp only [residualLinear, decide_eq_false_iff_not] at h
      simp [ownedJoinOk, h]
  | .movedOut, _, _, _ => rfl
  | .fields ts, T, hwf, h => by
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [OwnSt.wf, hd] at hwf
          | some sd =>
              have hiff := struct_carriesLinear_iff hd (hD s sd hd)
              simp only [OwnSt.wf, hd] at hwf
              simp only [residualLinear, hd, Bool.or_eq_false_iff, decide_eq_false_iff_not] at h
              simp only [ownedJoinOk, hd]
              rw [ownedJoinOkList_of_residualLinearFields_false hD ts sd.fields hwf h.2]
              have key : (sd.fields.any fun U => decide (U.mult D = .linear))
                  = decide ((Ty.struct s).mult D = .linear) := by
                rw [Bool.eq_iff_iff, decide_eq_true_eq, List.any_eq_true]
                constructor
                · rintro ⟨U, hmem, hU⟩
                  exact hiff.2 (Or.inr ⟨U, hmem, of_decide_eq_true hU⟩)
                · intro hlin
                  rcases hiff.1 hlin with h' | ⟨U, hmem, hU⟩
                  · exact absurd h' h.1
                  · exact ⟨U, hmem, decide_eq_true hU⟩
              rw [key]
              simp
      | array T' n =>
          simp only [OwnSt.wf] at hwf
          simp only [residualLinear] at h
          simp only [ownedJoinOk]
          rw [ownedJoinOkList_of_residualLinearFields_false hD ts (List.replicate n T') hwf h, Ty.any_replicate_mult_linear]
          simp
      | int _ _ => simp [OwnSt.wf] at hwf
      | float _ => simp [OwnSt.wf] at hwf
      | bool => simp [OwnSt.wf] at hwf
      | unit => simp [OwnSt.wf] at hwf
      | enum _ => simp [OwnSt.wf] at hwf

/-- The same over a declaration's slots (helper). -/
theorem ownedJoinOkList_of_residualLinearFields_false {D : Decls} (hD : WfStructs D) :
    ∀ (ts : List OwnSt) (Ts : List Ty), OwnSt.wfList D ts Ts = true →
      residualLinearFields D ts Ts = false →
      ownedJoinOkList D ts Ts = !(Ts.any fun U => decide (U.mult D = .linear))
  | [], _, _, h => by
      simp only [residualLinearFields] at h
      simp [ownedJoinOkList, h]
  | _ :: _, [], hwf, _ => by simp [OwnSt.wfList] at hwf
  | t :: ts, T :: Ts, hwf, h => by
      simp only [OwnSt.wfList, Bool.and_eq_true] at hwf
      simp only [residualLinearFields, Bool.or_eq_false_iff] at h
      simp only [ownedJoinOkList, List.any_cons]
      rw [ownedJoinOk_of_residualLinear_false hD t T hwf.1 h.1, ownedJoinOkList_of_residualLinearFields_false hD ts Ts hwf.2 h.2]
      simp
end

/-- §5.5's wholly-`Owned` arm on the left, as one equation over every state of
the other arm (helper). -/
theorem OwnSt.join_owned_left (D : Decls) (b : OwnSt) (T : Ty) :
    OwnSt.join D .owned b T = if ownedJoinOk D b T then some b else none := rfl

/-- §5.5's wholly-`Owned` arm on the right; the join reads the same from either
side (`OwnSt.join_comm`) (helper). -/
theorem OwnSt.join_owned_right (D : Decls) (a : OwnSt) (T : Ty) :
    OwnSt.join D a .owned T = if ownedJoinOk D a T then some a else none := by
  cases a <;> rfl

/-- The one clause the two readings share: joining `MovedOut` with a wholly
`Owned` arm is admissible exactly when the arm has no residue, because
`ownedJoinOk` at `MovedOut` and `residualLinear` at `Owned` are complementary
tests of `class(T)` (helper). -/
theorem OwnSt.join_movedOut_owned_eq (D : Decls) (T : Ty) :
    (if ownedJoinOk D .movedOut T then some OwnSt.movedOut else none)
      = if residualLinear D .owned T then none else some OwnSt.movedOut := by
  by_cases h : Ty.mult D T = .linear <;> simp [ownedJoinOk, residualLinear, h]

/-- §5.5's `MovedOut` arm on the left, as one equation over every state of the
other arm — including the `Owned` one, by the clause above (helper). -/
theorem OwnSt.join_movedOut_left (D : Decls) (b : OwnSt) (T : Ty) :
    OwnSt.join D .movedOut b T = if residualLinear D b T then none else some .movedOut := by
  cases b with
  | owned => exact OwnSt.join_movedOut_owned_eq D T
  | movedOut => rfl
  | fields ts => rfl

/-- §5.5's `MovedOut` arm on the right (helper). -/
theorem OwnSt.join_movedOut_right (D : Decls) (a : OwnSt) (T : Ty) :
    OwnSt.join D a .movedOut T = if residualLinear D a T then none else some .movedOut := by
  cases a with
  | owned => exact OwnSt.join_movedOut_owned_eq D T
  | movedOut => rfl
  | fields ts => rfl

/-- §5.5's slot join where the left arm records no slot: the other arm's record
survives subject to `ownedJoinOk` (helper). -/
theorem OwnSt.joinList_nil_left (D : Decls) (bs : List OwnSt) (T : Ty) (Ts : List Ty) :
    OwnSt.joinList D [] bs (T :: Ts)
      = if ownedJoinOkList D bs (T :: Ts) then some bs else none := rfl

/-- The same where the right arm records no slot (helper). -/
theorem OwnSt.joinList_nil_right (D : Decls) (as : List OwnSt) (T : Ty) (Ts : List Ty) :
    OwnSt.joinList D as [] (T :: Ts)
      = if ownedJoinOkList D as (T :: Ts) then some as else none := by
  cases as <;> rfl

/-- Joining a third field record after two is joining their slot lists after two
(helper). -/
theorem OwnSt.join_fields_bind_left (D : Decls) (as bs cs : List OwnSt) (Ts : List Ty) (T : Ty)
    (hj : ∀ xs ys, OwnSt.join D (.fields xs) (.fields ys) T
      = (OwnSt.joinList D xs ys Ts).map OwnSt.fields) :
    (OwnSt.join D (.fields as) (.fields bs) T).bind (fun x => OwnSt.join D x (.fields cs) T)
      = ((OwnSt.joinList D as bs Ts).bind (fun rs => OwnSt.joinList D rs cs Ts)).map
          OwnSt.fields := by
  rw [hj]
  cases h : OwnSt.joinList D as bs Ts with
  | none => simp
  | some rs => simp [hj]

/-- The same for the other bracketing (helper). -/
theorem OwnSt.join_fields_bind_right (D : Decls) (as bs cs : List OwnSt) (Ts : List Ty) (T : Ty)
    (hj : ∀ xs ys, OwnSt.join D (.fields xs) (.fields ys) T
      = (OwnSt.joinList D xs ys Ts).map OwnSt.fields) :
    (OwnSt.join D (.fields bs) (.fields cs) T).bind (fun y => OwnSt.join D (.fields as) y T)
      = ((OwnSt.joinList D bs cs Ts).bind (fun rs => OwnSt.joinList D as rs Ts)).map
          OwnSt.fields := by
  rw [hj]
  cases h : OwnSt.joinList D bs cs Ts with
  | none => simp
  | some rs => simp [hj]

/-- A slot list joins slot by slot, so one bracketing of three lists factors into
that bracketing of the heads and of the tails — which is what carries the
induction in `OwnSt.joinList_assoc` (helper). -/
theorem OwnSt.joinList_cons_bind_left (D : Decls) (a b c : OwnSt) (as bs cs : List OwnSt) (T : Ty) (Ts : List Ty) :
    (OwnSt.joinList D (a :: as) (b :: bs) (T :: Ts)).bind
        (fun xs => OwnSt.joinList D xs (c :: cs) (T :: Ts))
      = (match (OwnSt.join D a b T).bind (fun x => OwnSt.join D x c T),
               (OwnSt.joinList D as bs Ts).bind (fun xs => OwnSt.joinList D xs cs Ts) with
         | some x, some xs => some (x :: xs)
         | _, _ => none) := by
  cases hab : OwnSt.join D a b T with
  | none => simp [OwnSt.joinList, hab]
  | some e =>
      cases hl : OwnSt.joinList D as bs Ts with
      | none => simp [OwnSt.joinList, hab, hl]
      | some rest =>
          simp [OwnSt.joinList, hab, hl]

/-- The same for the other bracketing (helper). -/
theorem OwnSt.joinList_cons_bind_right (D : Decls) (a b c : OwnSt) (as bs cs : List OwnSt) (T : Ty) (Ts : List Ty) :
    (OwnSt.joinList D (b :: bs) (c :: cs) (T :: Ts)).bind
        (fun ys => OwnSt.joinList D (a :: as) ys (T :: Ts))
      = (match (OwnSt.join D b c T).bind (fun y => OwnSt.join D a y T),
               (OwnSt.joinList D bs cs Ts).bind (fun ys => OwnSt.joinList D as ys Ts) with
         | some x, some xs => some (x :: xs)
         | _, _ => none) := by
  cases hbc : OwnSt.join D b c T with
  | none => simp [OwnSt.joinList, hbc]
  | some e =>
      cases hl : OwnSt.joinList D bs cs Ts with
      | none => simp [OwnSt.joinList, hbc, hl]
      | some rest =>
          simp [OwnSt.joinList, hbc, hl]

/-- §5.6's residue of a state a wholly `Owned` arm may absorb *is* `class(T) =
Linear`: the two halves above, taken together (helper). -/
theorem residualLinear_of_ownedJoinOk {D : Decls} (hD : WfStructs D) (t : OwnSt) (T : Ty)
    (h : ownedJoinOk D t T = true) :
    residualLinear D t T = decide (T.mult D = .linear) := by
  by_cases hlin : T.mult D = .linear
  · rw [ownedJoinOk_residualLinear hD t T h hlin, decide_eq_true hlin]
  · rw [decide_eq_false hlin]
    cases hr : residualLinear D t T with
    | false => rfl
    | true => exact absurd (residualLinear_mult_linear hD t T hr) hlin

/-- The same over a declaration's slots (helper). -/
theorem residualLinearFields_of_ownedJoinOkList {D : Decls} (hD : WfStructs D) (ts : List OwnSt) (Ts : List Ty)
    (h : ownedJoinOkList D ts Ts = true) :
    residualLinearFields D ts Ts = (Ts.any fun U => decide (U.mult D = .linear)) := by
  cases hany : (Ts.any fun U => decide (U.mult D = .linear)) with
  | true => exact ownedJoinOkList_residualLinearFields hD ts Ts h hany
  | false =>
      cases hr : residualLinearFields D ts Ts with
      | false => rfl
      | true => exact absurd (residualLinearFields_mult_linear hD ts Ts hr) (by simp [hany])

mutual
/-- **A successful §5.5 join neither adds nor removes an inadmissible
`MovedOut`**: both arms and the result answer `ownedJoinOk` alike, so joining a
third, wholly `Owned` arm before or after asks the same question (`3.8:50`).
One of the two facts `OwnSt.join_assoc` turns on. -/
theorem OwnSt.join_ownedJoinOk {D : Decls} (hD : WfStructs D) :
    ∀ (b c : OwnSt) (T : Ty) (r : OwnSt), OwnSt.wf D b T = true → OwnSt.wf D c T = true →
      OwnSt.join D b c T = some r →
      ownedJoinOk D b T = ownedJoinOk D r T ∧ ownedJoinOk D c T = ownedJoinOk D r T
  | .owned, _, _, _, _, _, hj => by
      rw [OwnSt.join_owned_left] at hj
      split at hj
      · rename_i hok
        obtain rfl := Option.some.inj hj
        exact ⟨hok.symm, rfl⟩
      · exact absurd hj (by simp)
  | _, .owned, _, _, _, _, hj => by
      rw [OwnSt.join_owned_right] at hj
      split at hj
      · rename_i hok
        obtain rfl := Option.some.inj hj
        exact ⟨rfl, hok.symm⟩
      · exact absurd hj (by simp)
  | .movedOut, c, T, _, _, hc, hj => by
      rw [OwnSt.join_movedOut_left] at hj
      split at hj
      · exact absurd hj (by simp)
      · rename_i hres
        obtain rfl := Option.some.inj hj
        exact ⟨rfl, ownedJoinOk_of_residualLinear_false hD c T hc (by simpa using hres)⟩
  | b, .movedOut, T, _, hb, _, hj => by
      rw [OwnSt.join_movedOut_right] at hj
      split at hj
      · exact absurd hj (by simp)
      · rename_i hres
        obtain rfl := Option.some.inj hj
        exact ⟨ownedJoinOk_of_residualLinear_false hD b T hb (by simpa using hres), rfl⟩
  | .fields bs, .fields cs, T, _, hb, hc, hj => by
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [OwnSt.join, hd] at hj
          | some sd =>
              simp only [OwnSt.join, hd, Option.map_eq_some_iff] at hj
              obtain ⟨rs, hjl, rfl⟩ := hj
              simp only [OwnSt.wf, hd] at hb hc
              simp only [ownedJoinOk, hd]
              exact OwnSt.joinList_ownedJoinOkList hD bs cs sd.fields rs hb hc hjl
      | array T' n =>
          simp only [OwnSt.join, Option.map_eq_some_iff] at hj
          obtain ⟨rs, hjl, rfl⟩ := hj
          simp only [OwnSt.wf] at hb hc
          simp only [ownedJoinOk]
          exact OwnSt.joinList_ownedJoinOkList hD bs cs (List.replicate n T') rs hb hc hjl
      | int _ _ => simp [OwnSt.join] at hj
      | float _ => simp [OwnSt.join] at hj
      | bool => simp [OwnSt.join] at hj
      | unit => simp [OwnSt.join] at hj
      | enum _ => simp [OwnSt.join] at hj

/-- The same over a declaration's slots (helper). -/
theorem OwnSt.joinList_ownedJoinOkList {D : Decls} (hD : WfStructs D) :
    ∀ (bs cs : List OwnSt) (Ts : List Ty) (rs : List OwnSt),
      OwnSt.wfList D bs Ts = true → OwnSt.wfList D cs Ts = true →
      OwnSt.joinList D bs cs Ts = some rs →
      ownedJoinOkList D bs Ts = ownedJoinOkList D rs Ts ∧
        ownedJoinOkList D cs Ts = ownedJoinOkList D rs Ts
  | bs, cs, [], _, _, _, hj => by
      simp only [OwnSt.joinList] at hj
      obtain rfl := Option.some.inj hj
      exact ⟨by cases bs <;> rfl, by cases cs <;> rfl⟩
  | [], _, _ :: _, _, _, _, hj => by
      rw [OwnSt.joinList_nil_left] at hj
      split at hj
      · rename_i hok
        obtain rfl := Option.some.inj hj
        exact ⟨hok.symm, rfl⟩
      · exact absurd hj (by simp)
  | _, [], _ :: _, _, _, _, hj => by
      rw [OwnSt.joinList_nil_right] at hj
      split at hj
      · rename_i hok
        obtain rfl := Option.some.inj hj
        exact ⟨rfl, hok.symm⟩
      · exact absurd hj (by simp)
  | b :: bs, c :: cs, T :: Ts, _, hb, hc, hj => by
      simp only [OwnSt.wfList, Bool.and_eq_true] at hb hc
      cases he : OwnSt.join D b c T with
      | none => simp [OwnSt.joinList, he] at hj
      | some e =>
          cases hr : OwnSt.joinList D bs cs Ts with
          | none => simp [OwnSt.joinList, he, hr] at hj
          | some rest =>
              simp only [OwnSt.joinList, he, hr, Option.some.injEq] at hj
              subst hj
              obtain ⟨h1, h2⟩ := OwnSt.join_ownedJoinOk hD b c T e hb.1 hc.1 he
              obtain ⟨h3, h4⟩ := OwnSt.joinList_ownedJoinOkList hD bs cs Ts rest hb.2 hc.2 hr
              exact ⟨by simp only [ownedJoinOkList, h1, h3],
                     by simp only [ownedJoinOkList, h2, h4]⟩
end

mutual
/-- **A successful §5.5 join is between arms carrying the same residue, and the
result carries the same.** The disagreement clause refuses `MovedOut` against
residual linear content and the `Owned` clause refuses a moved-out linear path,
so a join that succeeds has already equated the two arms' §5.6 obligations
(`3.8:50`, `3.8:60`). The other fact `OwnSt.join_assoc` turns on. -/
theorem OwnSt.join_residualLinear {D : Decls} (hD : WfStructs D) :
    ∀ (b c : OwnSt) (T : Ty) (r : OwnSt), OwnSt.join D b c T = some r →
      residualLinear D b T = residualLinear D c T ∧
        residualLinear D r T = residualLinear D b T
  | .owned, c, T, _, hj => by
      rw [OwnSt.join_owned_left] at hj
      split at hj
      · rename_i hok
        obtain rfl := Option.some.inj hj
        have h := residualLinear_of_ownedJoinOk hD c T hok
        exact ⟨h.symm, h⟩
      · exact absurd hj (by simp)
  | b, .owned, T, _, hj => by
      rw [OwnSt.join_owned_right] at hj
      split at hj
      · rename_i hok
        obtain rfl := Option.some.inj hj
        exact ⟨residualLinear_of_ownedJoinOk hD b T hok, rfl⟩
      · exact absurd hj (by simp)
  | .movedOut, c, T, _, hj => by
      rw [OwnSt.join_movedOut_left] at hj
      split at hj
      · exact absurd hj (by simp)
      · rename_i hres
        obtain rfl := Option.some.inj hj
        have h : residualLinear D c T = false := by simpa using hres
        exact ⟨h.symm, rfl⟩
  | b, .movedOut, T, _, hj => by
      rw [OwnSt.join_movedOut_right] at hj
      split at hj
      · exact absurd hj (by simp)
      · rename_i hres
        obtain rfl := Option.some.inj hj
        have h : residualLinear D b T = false := by simpa using hres
        exact ⟨h, h.symm⟩
  | .fields bs, .fields cs, T, _, hj => by
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [OwnSt.join, hd] at hj
          | some sd =>
              simp only [OwnSt.join, hd, Option.map_eq_some_iff] at hj
              obtain ⟨rs, hjl, rfl⟩ := hj
              obtain ⟨h1, h2⟩ := OwnSt.joinList_residualLinearFields hD bs cs sd.fields rs hjl
              simp only [residualLinear, hd, h1, h2]
              exact ⟨trivial, trivial⟩
      | array T' n =>
          simp only [OwnSt.join, Option.map_eq_some_iff] at hj
          obtain ⟨rs, hjl, rfl⟩ := hj
          obtain ⟨h1, h2⟩ := OwnSt.joinList_residualLinearFields hD bs cs (List.replicate n T') rs hjl
          simp only [residualLinear, h1, h2]
          exact ⟨trivial, trivial⟩
      | int _ _ => simp [OwnSt.join] at hj
      | float _ => simp [OwnSt.join] at hj
      | bool => simp [OwnSt.join] at hj
      | unit => simp [OwnSt.join] at hj
      | enum _ => simp [OwnSt.join] at hj

/-- The same over a declaration's slots (helper). -/
theorem OwnSt.joinList_residualLinearFields {D : Decls} (hD : WfStructs D) :
    ∀ (bs cs : List OwnSt) (Ts : List Ty) (rs : List OwnSt),
      OwnSt.joinList D bs cs Ts = some rs →
      residualLinearFields D bs Ts = residualLinearFields D cs Ts ∧
        residualLinearFields D rs Ts = residualLinearFields D bs Ts
  | bs, cs, [], _, hj => by
      simp only [OwnSt.joinList] at hj
      obtain rfl := Option.some.inj hj
      exact ⟨by cases bs <;> cases cs <;> rfl, by cases bs <;> rfl⟩
  | [], cs, T :: Ts, _, hj => by
      rw [OwnSt.joinList_nil_left] at hj
      split at hj
      · rename_i hok
        obtain rfl := Option.some.inj hj
        have h := residualLinearFields_of_ownedJoinOkList hD cs (T :: Ts) hok
        exact ⟨h.symm, h⟩
      · exact absurd hj (by simp)
  | bs, [], T :: Ts, _, hj => by
      rw [OwnSt.joinList_nil_right] at hj
      split at hj
      · rename_i hok
        obtain rfl := Option.some.inj hj
        exact ⟨residualLinearFields_of_ownedJoinOkList hD bs (T :: Ts) hok, rfl⟩
      · exact absurd hj (by simp)
  | b :: bs, c :: cs, T :: Ts, _, hj => by
      cases he : OwnSt.join D b c T with
      | none => simp [OwnSt.joinList, he] at hj
      | some e =>
          cases hr : OwnSt.joinList D bs cs Ts with
          | none => simp [OwnSt.joinList, he, hr] at hj
          | some rest =>
              simp only [OwnSt.joinList, he, hr, Option.some.injEq] at hj
              subst hj
              obtain ⟨h1, h2⟩ := OwnSt.join_residualLinear hD b c T e he
              obtain ⟨h3, h4⟩ := OwnSt.joinList_residualLinearFields hD bs cs Ts rest hr
              simp only [residualLinearFields, h1, h2, h3, h4]
              exact ⟨trivial, trivial⟩
end

mutual
/-- **Two residue-free states always join**, the converse of
`OwnSt.join_residualLinear` and what makes §5.5's `MovedOut` cases associate:
re-bracketing cannot turn a join that succeeds into one that fails. -/
theorem OwnSt.join_exists {D : Decls} (hD : WfStructs D) :
    ∀ (b c : OwnSt) (T : Ty), OwnSt.wf D b T = true → OwnSt.wf D c T = true →
      residualLinear D b T = false → residualLinear D c T = false →
      ∃ r, OwnSt.join D b c T = some r
  | .owned, c, T, _, hc, hb0, hc0 => by
      have hlin : Ty.mult D T ≠ .linear := by
        simpa [residualLinear] using hb0
      have h : ownedJoinOk D c T = true := by
        rw [ownedJoinOk_of_residualLinear_false hD c T hc hc0]; exact decide_eq_true hlin
      exact ⟨c, by rw [OwnSt.join_owned_left, if_pos h]⟩
  | b, .owned, T, hb, _, hb0, hc0 => by
      have hlin : Ty.mult D T ≠ .linear := by
        simpa [residualLinear] using hc0
      have h : ownedJoinOk D b T = true := by
        rw [ownedJoinOk_of_residualLinear_false hD b T hb hb0]; exact decide_eq_true hlin
      exact ⟨b, by rw [OwnSt.join_owned_right, if_pos h]⟩
  | .movedOut, c, T, _, _, _, hc0 =>
      ⟨.movedOut, by rw [OwnSt.join_movedOut_left, if_neg (by simp [hc0])]⟩
  | b, .movedOut, T, _, _, hb0, _ =>
      ⟨.movedOut, by rw [OwnSt.join_movedOut_right, if_neg (by simp [hb0])]⟩
  | .fields bs, .fields cs, T, hb, hc, hb0, hc0 => by
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [OwnSt.wf, hd] at hb
          | some sd =>
              simp only [OwnSt.wf, hd] at hb hc
              simp only [residualLinear, hd, Bool.or_eq_false_iff] at hb0 hc0
              obtain ⟨rs, hrs⟩ := OwnSt.joinList_exists hD bs cs sd.fields hb hc hb0.2 hc0.2
              exact ⟨.fields rs, by simp [OwnSt.join, hd, hrs]⟩
      | array T' n =>
          simp only [OwnSt.wf] at hb hc
          simp only [residualLinear] at hb0 hc0
          obtain ⟨rs, hrs⟩ := OwnSt.joinList_exists hD bs cs (List.replicate n T') hb hc hb0 hc0
          exact ⟨.fields rs, by simp [OwnSt.join, hrs]⟩
      | int _ _ => simp [OwnSt.wf] at hb
      | float _ => simp [OwnSt.wf] at hb
      | bool => simp [OwnSt.wf] at hb
      | unit => simp [OwnSt.wf] at hb
      | enum _ => simp [OwnSt.wf] at hb

/-- The same over a declaration's slots (helper). -/
theorem OwnSt.joinList_exists {D : Decls} (hD : WfStructs D) :
    ∀ (bs cs : List OwnSt) (Ts : List Ty), OwnSt.wfList D bs Ts = true →
      OwnSt.wfList D cs Ts = true → residualLinearFields D bs Ts = false →
      residualLinearFields D cs Ts = false →
      ∃ rs, OwnSt.joinList D bs cs Ts = some rs
  | _, _, [], _, _, _, _ => ⟨[], by simp [OwnSt.joinList]⟩
  | [], cs, T :: Ts, _, hc, hb0, hc0 => by
      have hany : ((T :: Ts).any fun U => decide (U.mult D = .linear)) = false := by
        simpa [residualLinearFields] using hb0
      have h : ownedJoinOkList D cs (T :: Ts) = true := by
        rw [ownedJoinOkList_of_residualLinearFields_false hD cs (T :: Ts) hc hc0, hany]; rfl
      exact ⟨cs, by rw [OwnSt.joinList_nil_left, if_pos h]⟩
  | bs, [], T :: Ts, hb, _, hb0, hc0 => by
      have hany : ((T :: Ts).any fun U => decide (U.mult D = .linear)) = false := by
        simpa [residualLinearFields] using hc0
      have h : ownedJoinOkList D bs (T :: Ts) = true := by
        rw [ownedJoinOkList_of_residualLinearFields_false hD bs (T :: Ts) hb hb0, hany]; rfl
      exact ⟨bs, by rw [OwnSt.joinList_nil_right, if_pos h]⟩
  | b :: bs, c :: cs, T :: Ts, hb, hc, hb0, hc0 => by
      simp only [OwnSt.wfList, Bool.and_eq_true] at hb hc
      simp only [residualLinearFields, Bool.or_eq_false_iff] at hb0 hc0
      obtain ⟨e, he⟩ := OwnSt.join_exists hD b c T hb.1 hc.1 hb0.1 hc0.1
      obtain ⟨rest, hr⟩ := OwnSt.joinList_exists hD bs cs Ts hb.2 hc.2 hb0.2 hc0.2
      exact ⟨e :: rest, by simp [OwnSt.joinList, he, hr]⟩
end

mutual
/-- **§5.5's join stays inside the shapes of the type**: joining two states of
`T` yields a state of `T`, so the invariant associativity is stated over
survives the n-way fold (`Ctx.joinAll_perm`). -/
theorem OwnSt.join_wf {D : Decls} :
    ∀ (b c : OwnSt) (T : Ty) (r : OwnSt), OwnSt.wf D b T = true → OwnSt.wf D c T = true →
      OwnSt.join D b c T = some r → OwnSt.wf D r T = true
  | .owned, _, _, _, _, hc, hj => by
      rw [OwnSt.join_owned_left] at hj
      split at hj
      · obtain rfl := Option.some.inj hj; exact hc
      · exact absurd hj (by simp)
  | _, .owned, _, _, hb, _, hj => by
      rw [OwnSt.join_owned_right] at hj
      split at hj
      · obtain rfl := Option.some.inj hj; exact hb
      · exact absurd hj (by simp)
  | .movedOut, _, _, _, _, _, hj => by
      rw [OwnSt.join_movedOut_left] at hj
      split at hj
      · exact absurd hj (by simp)
      · obtain rfl := Option.some.inj hj; rfl
  | _, .movedOut, _, _, _, _, hj => by
      rw [OwnSt.join_movedOut_right] at hj
      split at hj
      · exact absurd hj (by simp)
      · obtain rfl := Option.some.inj hj; rfl
  | .fields bs, .fields cs, T, _, hb, hc, hj => by
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [OwnSt.join, hd] at hj
          | some sd =>
              simp only [OwnSt.join, hd, Option.map_eq_some_iff] at hj
              obtain ⟨rs, hjl, rfl⟩ := hj
              simp only [OwnSt.wf, hd] at hb hc ⊢
              exact OwnSt.joinList_wf bs cs sd.fields rs hb hc hjl
      | array T' n =>
          simp only [OwnSt.join, Option.map_eq_some_iff] at hj
          obtain ⟨rs, hjl, rfl⟩ := hj
          simp only [OwnSt.wf] at hb hc ⊢
          exact OwnSt.joinList_wf bs cs (List.replicate n T') rs hb hc hjl
      | int _ _ => simp [OwnSt.join] at hj
      | float _ => simp [OwnSt.join] at hj
      | bool => simp [OwnSt.join] at hj
      | unit => simp [OwnSt.join] at hj
      | enum _ => simp [OwnSt.join] at hj

/-- The same over a declaration's slots (helper). -/
theorem OwnSt.joinList_wf {D : Decls} :
    ∀ (bs cs : List OwnSt) (Ts : List Ty) (rs : List OwnSt),
      OwnSt.wfList D bs Ts = true → OwnSt.wfList D cs Ts = true →
      OwnSt.joinList D bs cs Ts = some rs → OwnSt.wfList D rs Ts = true
  | _, _, [], _, _, _, hj => by
      simp only [OwnSt.joinList] at hj
      obtain rfl := Option.some.inj hj
      rfl
  | [], _, _ :: _, _, _, hc, hj => by
      rw [OwnSt.joinList_nil_left] at hj
      split at hj
      · obtain rfl := Option.some.inj hj; exact hc
      · exact absurd hj (by simp)
  | _, [], _ :: _, _, hb, _, hj => by
      rw [OwnSt.joinList_nil_right] at hj
      split at hj
      · obtain rfl := Option.some.inj hj; exact hb
      · exact absurd hj (by simp)
  | b :: bs, c :: cs, T :: Ts, _, hb, hc, hj => by
      simp only [OwnSt.wfList, Bool.and_eq_true] at hb hc
      cases he : OwnSt.join D b c T with
      | none => simp [OwnSt.joinList, he] at hj
      | some e =>
          cases hr : OwnSt.joinList D bs cs Ts with
          | none => simp [OwnSt.joinList, he, hr] at hj
          | some rest =>
              simp only [OwnSt.joinList, he, hr, Option.some.injEq] at hj
              subst hj
              simp only [OwnSt.wfList, Bool.and_eq_true]
              exact ⟨OwnSt.join_wf b c T e hb.1 hc.1 he, OwnSt.joinList_wf bs cs Ts rest hb.2 hc.2 hr⟩
end

/-- §5.5's join of two field records at a declared struct type (helper). -/
theorem OwnSt.join_fields_struct (D : Decls) (s : Nat) (sd : StructDecl) (hd : D.structs[s]? = some sd)
    (xs ys : List OwnSt) :
    OwnSt.join D (.fields xs) (.fields ys) (Ty.struct s)
      = (OwnSt.joinList D xs ys sd.fields).map OwnSt.fields := by
  simp [OwnSt.join, hd]

/-- §5.5's join of two field records at an array type, element by element
(`3.8:73`) (helper). -/
theorem OwnSt.join_fields_array (D : Decls) (T' : Ty) (n : Nat) (xs ys : List OwnSt) :
    OwnSt.join D (.fields xs) (.fields ys) (Ty.array T' n)
      = (OwnSt.joinList D xs ys (List.replicate n T')).map OwnSt.fields := by
  simp [OwnSt.join]

/-- A field record is a shape of a declared struct type exactly when its slots
are shapes of the fields (helper). -/
theorem OwnSt.wf_fields_struct (D : Decls) (s : Nat) (sd : StructDecl) (hd : D.structs[s]? = some sd)
    (xs : List OwnSt) :
    OwnSt.wf D (.fields xs) (Ty.struct s) = OwnSt.wfList D xs sd.fields := by
  simp [OwnSt.wf, hd]

/-- The array form of the same (helper). -/
theorem OwnSt.wf_fields_array (D : Decls) (T' : Ty) (n : Nat) (xs : List OwnSt) :
    OwnSt.wf D (.fields xs) (Ty.array T' n) = OwnSt.wfList D xs (List.replicate n T') := by
  simp [OwnSt.wf]

mutual
/-- **The §5.5 join is associative**, at one path and its subtree, over states
that are shapes of their type (`OwnSt.wf`) and under §3's class assignment for
the struct layer (`WfStructs`, of which only the class-is-join clause is read).
Neither premise can be dropped: the section docstring above says which
counterexample each rules out; `WfStructs` is one a
well-formed program already carries (`checkStructs_sound`).

The nine outer cases reduce to three shapes. Where an arm is wholly `Owned` the
join is the other arm subject to `ownedJoinOk`, and `OwnSt.join_ownedJoinOk`
says the result answers that test as its operands do. Where an arm is
`MovedOut` the join is `MovedOut` subject to the other's residue, and
`OwnSt.join_residualLinear` with `OwnSt.join_exists` says the two bracketings
fail on exactly the same residue. Two field records join slot by slot, which is
the induction. -/
theorem OwnSt.join_assoc {D : Decls} (hD : WfStructs D) :
    ∀ (a b c : OwnSt) (T : Ty), OwnSt.wf D a T = true → OwnSt.wf D b T = true →
      OwnSt.wf D c T = true →
      (OwnSt.join D a b T).bind (fun x => OwnSt.join D x c T)
        = (OwnSt.join D b c T).bind (fun y => OwnSt.join D a y T)
  | .owned, b, c, T, _, hb, hc => by
      simp only [OwnSt.join_owned_left]
      cases hbc : OwnSt.join D b c T with
      | none => cases hob : ownedJoinOk D b T <;> simp [hbc]
      | some r =>
          obtain ⟨h1, _⟩ := OwnSt.join_ownedJoinOk hD b c T r hb hc hbc
          rw [h1]
          cases hor : ownedJoinOk D r T <;> simp [hor, hbc]
  | .movedOut, b, c, T, _, hb, hc => by
      simp only [OwnSt.join_movedOut_left]
      cases hbc : OwnSt.join D b c T with
      | none =>
          cases hrb : residualLinear D b T with
          | true => simp
          | false =>
              cases hrc : residualLinear D c T with
              | true => simp [hrc, OwnSt.join_movedOut_left]
              | false =>
                  obtain ⟨r, hr⟩ := OwnSt.join_exists hD b c T hb hc hrb hrc
                  rw [hr] at hbc
                  exact absurd hbc (by simp)
      | some r =>
          obtain ⟨h1, h2⟩ := OwnSt.join_residualLinear hD b c T r hbc
          simp only [Option.bind_some, h2, h1]
          cases hrc : residualLinear D c T with
          | true => simp
          | false => simp [hrc, OwnSt.join_movedOut_left]
  | a, b, .movedOut, T, ha, hb, _ => by
      simp only [OwnSt.join_movedOut_right]
      cases hab : OwnSt.join D a b T with
      | none =>
          cases hrb : residualLinear D b T with
          | true => simp
          | false =>
              cases hra : residualLinear D a T with
              | true => simp [hra, OwnSt.join_movedOut_right]
              | false =>
                  obtain ⟨r, hr⟩ := OwnSt.join_exists hD a b T ha hb hra hrb
                  rw [hr] at hab
                  exact absurd hab (by simp)
      | some r =>
          obtain ⟨h1, h2⟩ := OwnSt.join_residualLinear hD a b T r hab
          simp only [Option.bind_some, h2, h1]
          cases hrb : residualLinear D b T with
          | true => simp
          | false => simp [hrb, h1, OwnSt.join_movedOut_right]
  | a, .movedOut, c, T, _, _, _ => by
      cases hra : residualLinear D a T <;> cases hrc : residualLinear D c T <;>
        simp [OwnSt.join_movedOut_left, OwnSt.join_movedOut_right, hra, hrc]
  | a, .owned, c, T, ha, _, hc => by
      simp only [OwnSt.join_owned_right, OwnSt.join_owned_left]
      cases hac : OwnSt.join D a c T with
      | none =>
          cases hoa : ownedJoinOk D a T <;> cases hoc : ownedJoinOk D c T <;> simp [hac]
      | some r =>
          obtain ⟨h1, h2⟩ := OwnSt.join_ownedJoinOk hD a c T r ha hc hac
          rw [h1, h2]
          cases hor : ownedJoinOk D r T <;> simp [hac]
  | a, b, .owned, T, ha, hb, _ => by
      simp only [OwnSt.join_owned_right]
      cases hab : OwnSt.join D a b T with
      | none => cases hob : ownedJoinOk D b T <;> simp [hab]
      | some r =>
          obtain ⟨_, h2⟩ := OwnSt.join_ownedJoinOk hD a b T r ha hb hab
          rw [h2]
          cases hor : ownedJoinOk D r T <;> simp [hor, hab]
  | .fields as, .fields bs, .fields cs, T, ha, hb, hc => by
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [OwnSt.wf, hd] at ha
          | some sd =>
              rw [OwnSt.wf_fields_struct D s sd hd] at ha hb hc
              rw [OwnSt.join_fields_bind_left D as bs cs sd.fields _ (OwnSt.join_fields_struct D s sd hd),
                  OwnSt.join_fields_bind_right D as bs cs sd.fields _ (OwnSt.join_fields_struct D s sd hd),
                  OwnSt.joinList_assoc hD as bs cs sd.fields ha hb hc]
      | array T' n =>
          rw [OwnSt.wf_fields_array D T' n] at ha hb hc
          rw [OwnSt.join_fields_bind_left D as bs cs (List.replicate n T') _ (OwnSt.join_fields_array D T' n),
              OwnSt.join_fields_bind_right D as bs cs (List.replicate n T') _ (OwnSt.join_fields_array D T' n),
              OwnSt.joinList_assoc hD as bs cs (List.replicate n T') ha hb hc]
      | int _ _ => simp [OwnSt.wf] at ha
      | float _ => simp [OwnSt.wf] at ha
      | bool => simp [OwnSt.wf] at ha
      | unit => simp [OwnSt.wf] at ha
      | enum _ => simp [OwnSt.wf] at ha

/-- The same over a declaration's slots (helper). -/
theorem OwnSt.joinList_assoc {D : Decls} (hD : WfStructs D) :
    ∀ (as bs cs : List OwnSt) (Ts : List Ty), OwnSt.wfList D as Ts = true →
      OwnSt.wfList D bs Ts = true → OwnSt.wfList D cs Ts = true →
      (OwnSt.joinList D as bs Ts).bind (fun xs => OwnSt.joinList D xs cs Ts)
        = (OwnSt.joinList D bs cs Ts).bind (fun ys => OwnSt.joinList D as ys Ts)
  | _, _, _, [], _, _, _ => by simp [OwnSt.joinList]
  | [], bs, cs, T :: Ts, _, hb, hc => by
      simp only [OwnSt.joinList_nil_left]
      cases hbc : OwnSt.joinList D bs cs (T :: Ts) with
      | none => cases hob : ownedJoinOkList D bs (T :: Ts) <;> simp [hbc]
      | some rs =>
          obtain ⟨h1, _⟩ := OwnSt.joinList_ownedJoinOkList hD bs cs (T :: Ts) rs hb hc hbc
          rw [h1]
          cases hor : ownedJoinOkList D rs (T :: Ts) <;> simp [hor, hbc]
  | as, [], cs, T :: Ts, ha, _, hc => by
      simp only [OwnSt.joinList_nil_right, OwnSt.joinList_nil_left]
      cases hac : OwnSt.joinList D as cs (T :: Ts) with
      | none =>
          cases hoa : ownedJoinOkList D as (T :: Ts) <;>
            cases hoc : ownedJoinOkList D cs (T :: Ts) <;> simp [hac]
      | some rs =>
          obtain ⟨h1, h2⟩ := OwnSt.joinList_ownedJoinOkList hD as cs (T :: Ts) rs ha hc hac
          rw [h1, h2]
          cases hor : ownedJoinOkList D rs (T :: Ts) <;> simp [hac]
  | as, bs, [], T :: Ts, ha, hb, _ => by
      simp only [OwnSt.joinList_nil_right]
      cases hab : OwnSt.joinList D as bs (T :: Ts) with
      | none => cases hob : ownedJoinOkList D bs (T :: Ts) <;> simp [hab]
      | some rs =>
          obtain ⟨_, h2⟩ := OwnSt.joinList_ownedJoinOkList hD as bs (T :: Ts) rs ha hb hab
          rw [h2]
          cases hor : ownedJoinOkList D rs (T :: Ts) <;> simp [hor, hab]
  | a :: as, b :: bs, c :: cs, T :: Ts, ha, hb, hc => by
      simp only [OwnSt.wfList, Bool.and_eq_true] at ha hb hc
      rw [OwnSt.joinList_cons_bind_left, OwnSt.joinList_cons_bind_right, OwnSt.join_assoc hD a b c T ha.1 hb.1 hc.1,
          OwnSt.joinList_assoc hD as bs cs Ts ha.2 hb.2 hc.2]
end

/-- Renaming the result of a partial computation before continuing is renaming
after it (helper). -/
theorem optionMapBind {α β γ : Type} (o : Option α) (f : α → β) (g : β → Option γ) :
    (o.map f).bind g = o.bind (fun x => g (f x)) := by cases o <;> rfl

/-- The same on the other side of the bind (helper). -/
theorem optionBindMap {α β γ : Type} (o : Option α) (f : α → Option β) (g : β → γ) :
    (o.bind fun x => (f x).map g) = (o.bind f).map g := by
  cases o with
  | none => rfl
  | some x => cases f x <;> rfl

/-- Re-marking an entry records the state it was given (helper). -/
theorem Entry.setSt_st (en : Entry) (u : OwnSt) : (en.setSt u).st = u := rfl

/-- Re-marking an entry leaves its declared type alone (helper). -/
theorem Entry.setSt_ty (en : Entry) (u : OwnSt) : (en.setSt u).ty = en.ty := rfl

/-- Re-marking twice is re-marking once: `Entry.setSt` writes the whole `Σ` part
of the row (helper). -/
theorem Entry.setSt_setSt (en : Entry) (u : OwnSt) : (en.setSt u).setSt = en.setSt := by
  funext v; rfl

/-- Two entries with one skeleton have one declared type (helper). -/
theorem Entry.ty_of_skel {a b : Entry} (h : a.skel = b.skel) : b.ty = a.ty := by
  simp only [Entry.skel, Prod.mk.injEq] at h
  exact h.1.symm

/-- **The §5.5 join is associative on one entry**, whose skeleton the three arms
share — the declared type and `mut` mark come from the incoming context, so
only the state differs, and the state's associativity is `OwnSt.join_assoc`. -/
theorem Entry.join_assoc {D : Decls} (hD : WfStructs D) {a b c : Entry}
    (hab : a.skel = b.skel) (hbc : b.skel = c.skel)
    (ha : Entry.wf D a = true) (hb : Entry.wf D b = true) (hc : Entry.wf D c = true) :
    (Entry.join D a b).bind (fun x => Entry.join D x c)
      = (Entry.join D b c).bind (fun y => Entry.join D a y) := by
  have hbty : b.ty = a.ty := Entry.ty_of_skel hab
  have hcty : c.ty = a.ty := (Entry.ty_of_skel hbc).trans hbty
  have hwa : OwnSt.wf D a.st a.ty = true := ha
  have hwb : OwnSt.wf D b.st a.ty = true := by rw [← hbty]; exact hb
  have hwc : OwnSt.wf D c.st a.ty = true := by rw [← hcty]; exact hc
  simp only [Entry.join, optionMapBind, Entry.setSt_st, Entry.setSt_ty, Entry.setSt_setSt,
    optionBindMap, hbty]
  rw [OwnSt.join_assoc hD a.st b.st c.st a.ty hwa hwb hwc]

/-- **The §5.5 join of two well-formed entries is well formed**, `OwnSt.join_wf`
read at the entry's declared type. -/
theorem Entry.join_wf {D : Decls} {a b e : Entry} (hab : a.skel = b.skel)
    (ha : Entry.wf D a = true) (hb : Entry.wf D b = true) (h : Entry.join D a b = some e) :
    Entry.wf D e = true := by
  have hbty : b.ty = a.ty := Entry.ty_of_skel hab
  simp only [Entry.join, Option.map_eq_some_iff] at h
  obtain ⟨u, hu, rfl⟩ := h
  have hwb : OwnSt.wf D b.st a.ty = true := by rw [← hbty]; exact hb
  exact OwnSt.join_wf a.st b.st a.ty u ha hwb hu

/-- A context joins entry by entry, so one bracketing of three contexts factors
into that bracketing of the heads and of the tails (helper). -/
theorem Ctx.join_cons_bind_left (D : Decls) (a b c : Entry) (as bs cs : Ctx) :
    (Ctx.join D (a :: as) (b :: bs)).bind (fun xs => Ctx.join D xs (c :: cs))
      = (match (Entry.join D a b).bind (fun x => Entry.join D x c),
               (Ctx.join D as bs).bind (fun xs => Ctx.join D xs cs) with
         | some x, some xs => some (x :: xs)
         | _, _ => none) := by
  cases hab : Entry.join D a b with
  | none => simp [Ctx.join, hab]
  | some e =>
      cases hl : Ctx.join D as bs with
      | none => simp [Ctx.join, hab, hl]
      | some rest =>
          simp [Ctx.join, hab, hl]

/-- The same for the other bracketing (helper). -/
theorem Ctx.join_cons_bind_right (D : Decls) (a b c : Entry) (as bs cs : Ctx) :
    (Ctx.join D (b :: bs) (c :: cs)).bind (fun ys => Ctx.join D (a :: as) ys)
      = (match (Entry.join D b c).bind (fun y => Entry.join D a y),
               (Ctx.join D bs cs).bind (fun ys => Ctx.join D as ys) with
         | some x, some xs => some (x :: xs)
         | _, _ => none) := by
  cases hbc : Entry.join D b c with
  | none => simp [Ctx.join, hbc]
  | some e =>
      cases hl : Ctx.join D bs cs with
      | none => simp [Ctx.join, hbc, hl]
      | some rest =>
          simp [Ctx.join, hbc, hl]

/-- **The §5.5 join is associative on a whole context**, pointwise, whenever the
three arms carry the same skeleton and every entry is a shape of its declared
type — which `Typed.skel_preserved` and `Ctx.Wf` give of the outgoing contexts
of one incoming one. So the bracketing of (Match) §5.5's `join(Σ1, …, Σn)` is
immaterial, which with `Ctx.join_comm` is what `Ctx.joinAll_perm` needs. -/
theorem Ctx.join_assoc {D : Decls} (hD : WfStructs D) :
    ∀ (Γ₁ Γ₂ Γ₃ : Ctx), Γ₁.skel = Γ₂.skel → Γ₂.skel = Γ₃.skel →
      Ctx.Wf D Γ₁ → Ctx.Wf D Γ₂ → Ctx.Wf D Γ₃ →
      (Ctx.join D Γ₁ Γ₂).bind (fun Γ => Ctx.join D Γ Γ₃)
        = (Ctx.join D Γ₂ Γ₃).bind (fun Γ => Ctx.join D Γ₁ Γ)
  | [], [], [], _, _, _, _, _ => rfl
  | [], [], _ :: _, _, h, _, _, _ => by simp [Ctx.skel] at h
  | [], _ :: _, _, h, _, _, _, _ => by simp [Ctx.skel] at h
  | _ :: _, [], _, h, _, _, _, _ => by simp [Ctx.skel] at h
  | _ :: _, _ :: _, [], _, h, _, _, _ => by simp [Ctx.skel] at h
  | a :: as, b :: bs, c :: cs, h₁, h₂, w₁, w₂, w₃ => by
      simp only [Ctx.skel, List.map_cons, List.cons.injEq] at h₁ h₂
      simp only [Ctx.Wf, List.mem_cons, forall_eq_or_imp] at w₁ w₂ w₃
      rw [Ctx.join_cons_bind_left, Ctx.join_cons_bind_right,
          Entry.join_assoc hD h₁.1 h₂.1 w₁.1 w₂.1 w₃.1,
          Ctx.join_assoc hD as bs cs h₁.2 h₂.2 w₁.2 w₂.2 w₃.2]

/-! ### The join is idempotent and absorbs its right arm

§5.7's loop-head state is a fixpoint of `Σ_h = join(Σ, Σ_e)`, where `Σ_e` is
the back-edge state of the body typed at `Σ_h`. Re-entering the loop at `Σ_h`
must solve the same equation, `Σ_h = join(Σ_h, Σ_e)`, and that is
`join(join(Σ, Σ_e), Σ_e) = join(Σ, Σ_e)`: the join **absorbs** a second copy of
its right arm. Idempotence is the special case the absorption needs at a
wholly `Owned` left arm. Both hold of a right arm that is a shape of its type
(`OwnSt.wf`); nothing is asked of the left one, which is what lets
`soundness` re-enter a loop from an entry state it knows nothing about but its
agreement with the store. -/

mutual
/-- **§5.5's join is idempotent** on a state that is a shape of its type: a
path joined with itself is unchanged (helper). -/
theorem OwnSt.join_idem {D : Decls} : ∀ (b : OwnSt) (T : Ty), OwnSt.wf D b T = true →
    OwnSt.join D b b T = some b
  | .owned, _, _ => by simp [OwnSt.join_owned_left, ownedJoinOk]
  | .movedOut, _, _ => by simp [OwnSt.join_movedOut_left, residualLinear]
  | .fields bs, .struct s, hw => by
      cases hd : D.structs[s]? with
      | none => simp [OwnSt.wf, hd] at hw
      | some sd =>
          rw [OwnSt.wf_fields_struct D s sd hd] at hw
          rw [OwnSt.join_fields_struct D s sd hd, OwnSt.joinList_idem bs sd.fields hw]
          rfl
  | .fields bs, .array T n, hw => by
      rw [OwnSt.wf_fields_array] at hw
      rw [OwnSt.join_fields_array, OwnSt.joinList_idem bs _ hw]
      rfl
  | .fields _, .int _ _, hw | .fields _, .float _, hw | .fields _, .bool, hw
  | .fields _, .unit, hw | .fields _, .enum _, hw => by simp [OwnSt.wf] at hw

/-- The same over a slot list (helper). -/
theorem OwnSt.joinList_idem {D : Decls} : ∀ (bs : List OwnSt) (Ts : List Ty),
    OwnSt.wfList D bs Ts = true → OwnSt.joinList D bs bs Ts = some bs
  | [], [], _ => rfl
  | [], T :: Ts, _ => by rw [OwnSt.joinList_nil_left]; simp [ownedJoinOkList]
  | _ :: _, [], hw => by simp [OwnSt.wfList] at hw
  | b :: bs, T :: Ts, hw => by
      simp only [OwnSt.wfList, Bool.and_eq_true] at hw
      simp [OwnSt.joinList, OwnSt.join_idem b T hw.1, OwnSt.joinList_idem bs Ts hw.2]
end

mutual
/-- **§5.5's join absorbs its right arm**: joining the result with the right
arm again changes nothing, given the right arm is a shape of its type
(helper). -/
theorem OwnSt.join_absorb {D : Decls} : ∀ (a b c : OwnSt) (T : Ty), OwnSt.wf D b T = true →
    OwnSt.join D a b T = some c → OwnSt.join D c b T = some c
  | .owned, b, c, T, hw, h => by
      rw [OwnSt.join_owned_left] at h
      split at h
      · cases h; exact OwnSt.join_idem _ T hw
      · cases h
  | .movedOut, b, c, T, _, h => by
      rw [OwnSt.join_movedOut_left] at h
      split at h
      · cases h
      · cases h; rw [OwnSt.join_movedOut_left, if_neg (by assumption)]
  | .fields as, .owned, c, T, _, h => by
      rw [OwnSt.join_owned_right] at h ⊢
      split at h
      · cases h; rw [if_pos (by assumption)]
      · cases h
  | .fields as, .movedOut, c, T, _, h => by
      rw [OwnSt.join_movedOut_right] at h
      split at h
      · cases h
      · cases h; simp [OwnSt.join_movedOut_left, residualLinear]
  | .fields as, .fields bs, c, .struct s, hw, h => by
      cases hd : D.structs[s]? with
      | none => simp [OwnSt.wf, hd] at hw
      | some sd =>
          rw [OwnSt.wf_fields_struct D s sd hd] at hw
          rw [OwnSt.join_fields_struct D s sd hd] at h
          cases hl : OwnSt.joinList D as bs sd.fields with
          | none => rw [hl] at h; cases h
          | some cs =>
              rw [hl] at h
              simp only [Option.map_some, Option.some.injEq] at h
              subst h
              rw [OwnSt.join_fields_struct D s sd hd,
                OwnSt.joinList_absorb as bs cs sd.fields hw hl]
              rfl
  | .fields as, .fields bs, c, .array T n, hw, h => by
      rw [OwnSt.wf_fields_array] at hw
      rw [OwnSt.join_fields_array] at h
      cases hl : OwnSt.joinList D as bs (List.replicate n T) with
      | none => rw [hl] at h; cases h
      | some cs =>
          rw [hl] at h
          simp only [Option.map_some, Option.some.injEq] at h
          subst h
          rw [OwnSt.join_fields_array, OwnSt.joinList_absorb as bs cs _ hw hl]
          rfl
  | .fields _, .fields _, _, .int _ _, hw, _ | .fields _, .fields _, _, .float _, hw, _
  | .fields _, .fields _, _, .bool, hw, _ | .fields _, .fields _, _, .unit, hw, _
  | .fields _, .fields _, _, .enum _, hw, _ => by simp [OwnSt.wf] at hw

/-- The same over a slot list (helper). -/
theorem OwnSt.joinList_absorb {D : Decls} : ∀ (as bs cs : List OwnSt) (Ts : List Ty),
    OwnSt.wfList D bs Ts = true →
    OwnSt.joinList D as bs Ts = some cs → OwnSt.joinList D cs bs Ts = some cs
  | _, bs, cs, [], _, h => by
      cases cs with
      | nil => cases bs <;> rfl
      | cons _ _ => simp [OwnSt.joinList] at h
  | [], bs, cs, T :: Ts, hw, h => by
      rw [OwnSt.joinList_nil_left] at h
      split at h
      · cases h; exact OwnSt.joinList_idem _ _ hw
      · cases h
  | a :: as, [], cs, T :: Ts, _, h => by
      rw [OwnSt.joinList_nil_right] at h ⊢
      split at h
      · cases h; rw [if_pos (by assumption)]
      · cases h
  | a :: as, b :: bs, cs, T :: Ts, hw, h => by
      simp only [OwnSt.wfList, Bool.and_eq_true] at hw
      simp only [OwnSt.joinList] at h
      cases hab : OwnSt.join D a b T with
      | none => rw [hab] at h; cases h
      | some c =>
          cases hl : OwnSt.joinList D as bs Ts with
          | none => rw [hab, hl] at h; cases h
          | some cs' =>
              rw [hab, hl] at h
              cases h
              simp [OwnSt.joinList, OwnSt.join_absorb a b c T hw.1 hab,
                OwnSt.joinList_absorb as bs cs' Ts hw.2 hl]
end

/-- §5.5's per-entry join absorbs its right arm, given the two entries share a
skeleton and the right one is well formed (helper). -/
theorem Entry.join_absorb {D : Decls} {a b c : Entry} (hsk : a.skel = b.skel)
    (hw : Entry.wf D b = true) (h : a.join D b = some c) : c.join D b = some c := by
  have hty : b.ty = a.ty := Entry.ty_of_skel hsk
  unfold Entry.join at h ⊢
  cases hj : OwnSt.join D a.st b.st a.ty with
  | none => rw [hj] at h; cases h
  | some u =>
      rw [hj] at h
      simp only [Option.map_some, Option.some.injEq] at h
      subst h
      have hw' : OwnSt.wf D b.st a.ty = true := by rw [← hty]; exact hw
      simp [Entry.setSt_st, Entry.setSt_ty, OwnSt.join_absorb a.st b.st u a.ty hw' hj,
        Entry.setSt_setSt]

/-- **§5.5's join absorbs its right arm**, context-wide: `join(join(Γ, Γe), Γe)
= join(Γ, Γe)` whenever `Γe` is well formed and has `Γ`'s skeleton. This is
the lattice fact behind re-entering a loop at its head (`LoopHead.reenter`). -/
theorem Ctx.join_absorb {D : Decls} : ∀ {Γ Γe Γh : Ctx}, Γ.skel = Γe.skel → Ctx.Wf D Γe →
    Ctx.join D Γ Γe = some Γh → Ctx.join D Γh Γe = some Γh
  | [], [], _, _, _, h => by cases h; rfl
  | [], _ :: _, _, hs, _, _ => by simp [Ctx.skel] at hs
  | _ :: _, [], _, hs, _, _ => by simp [Ctx.skel] at hs
  | a :: as, b :: bs, Γh, hs, hw, h => by
      simp only [Ctx.skel, List.map_cons, List.cons.injEq] at hs
      simp only [Ctx.Wf, List.mem_cons, forall_eq_or_imp] at hw
      unfold Ctx.join at h
      cases he : a.join D b with
      | none => simp [he] at h
      | some e =>
          cases hr : Ctx.join D as bs with
          | none => simp [he, hr] at h
          | some rest =>
              simp only [he, hr, Option.some.injEq] at h
              subst h
              have he' := Entry.join_absorb hs.1 hw.1 he
              have hr' := Ctx.join_absorb (Γh := rest) hs.2 hw.2 hr
              simp [Ctx.join, he', hr']

/-- **The §5.5 join of two well-formed contexts is well formed**, so the
accumulator of the n-way fold keeps the invariant associativity is stated
over. -/
theorem Ctx.join_wf {D : Decls} : ∀ (Γ₁ Γ₂ Γ' : Ctx), Γ₁.skel = Γ₂.skel →
    Ctx.Wf D Γ₁ → Ctx.Wf D Γ₂ → Ctx.join D Γ₁ Γ₂ = some Γ' → Ctx.Wf D Γ'
  | [], [], Γ', _, _, _, h => by
      simp only [Ctx.join, Option.some.injEq] at h
      subst h
      simp [Ctx.Wf]
  | [], _ :: _, _, h, _, _, _ => by simp [Ctx.skel] at h
  | _ :: _, [], _, h, _, _, _ => by simp [Ctx.skel] at h
  | a :: as, b :: bs, Γ', h₁, w₁, w₂, h => by
      simp only [Ctx.skel, List.map_cons, List.cons.injEq] at h₁
      simp only [Ctx.Wf, List.mem_cons, forall_eq_or_imp] at w₁ w₂
      cases he : Entry.join D a b with
      | none => simp [Ctx.join, he] at h
      | some e =>
          cases hr : Ctx.join D as bs with
          | none => simp [Ctx.join, he, hr] at h
          | some rest =>
              simp only [Ctx.join, he, hr, Option.some.injEq] at h
              subst h
              simp only [Ctx.Wf, List.mem_cons, forall_eq_or_imp]
              exact ⟨Entry.join_wf h₁.1 w₁.1 w₂.1 he,
                     Ctx.join_wf as bs rest h₁.2 w₁.2 w₂.2 hr⟩

/-- Writing a state of a slot's own type into a record leaves the record a shape
of its type: the `owned` padding `OwnSt.setField` inserts before the slot is a
state of every type it passes (helper). -/
theorem OwnSt.setField_wf {D : Decls} : ∀ (ts : List OwnSt) (f : Nat) (v : OwnSt)
    (Ts : List Ty) (T' : Ty), OwnSt.wfList D ts Ts = true → Ts[f]? = some T' →
    OwnSt.wf D v T' = true → OwnSt.wfList D (OwnSt.setField ts f v) Ts = true
  | [], 0, v, Ts, T', _, hf, hv => by
      cases Ts with
      | nil => simp at hf
      | cons T₀ Ts =>
          simp only [List.getElem?_cons_zero, Option.some.injEq] at hf
          subst hf
          simp only [OwnSt.setField, OwnSt.wfList, Bool.and_eq_true]
          exact ⟨hv, trivial⟩
  | [], f + 1, v, Ts, T', _, hf, hv => by
      cases Ts with
      | nil => simp at hf
      | cons T₀ Ts =>
          simp only [List.getElem?_cons_succ] at hf
          simp only [OwnSt.setField, OwnSt.wfList, Bool.and_eq_true]
          exact ⟨rfl, OwnSt.setField_wf [] f v Ts T' rfl hf hv⟩
  | t :: ts, 0, v, Ts, T', ht, hf, hv => by
      cases Ts with
      | nil => simp at hf
      | cons T₀ Ts =>
          simp only [List.getElem?_cons_zero, Option.some.injEq] at hf
          subst hf
          simp only [OwnSt.wfList, Bool.and_eq_true] at ht
          simp only [OwnSt.setField, OwnSt.wfList, Bool.and_eq_true]
          exact ⟨hv, ht.2⟩
  | t :: ts, f + 1, v, Ts, T', ht, hf, hv => by
      cases Ts with
      | nil => simp at hf
      | cons T₀ Ts =>
          simp only [List.getElem?_cons_succ] at hf
          simp only [OwnSt.wfList, Bool.and_eq_true] at ht
          simp only [OwnSt.setField, OwnSt.wfList, Bool.and_eq_true]
          exact ⟨ht.1, OwnSt.setField_wf ts f v Ts T' ht.2 hf hv⟩

/-- Reading a slot of a well-formed record gives a state of that slot's type; a
slot no partial move has touched reads as `owned`, which is a state of every
type (helper). -/
theorem OwnSt.fieldAt_wf {D : Decls} : ∀ (ts : List OwnSt) (f : Nat) (Ts : List Ty) (T' : Ty),
    OwnSt.wfList D ts Ts = true → Ts[f]? = some T' →
    OwnSt.wf D (OwnSt.fieldAt ts f) T' = true
  | [], _, _, _, _, _ => rfl
  | t :: ts, 0, Ts, T', ht, hf => by
      cases Ts with
      | nil => simp at hf
      | cons T₀ Ts =>
          simp only [List.getElem?_cons_zero, Option.some.injEq] at hf
          subst hf
          simp only [OwnSt.wfList, Bool.and_eq_true] at ht
          exact ht.1
  | t :: ts, f + 1, Ts, T', ht, hf => by
      cases Ts with
      | nil => simp at hf
      | cons T₀ Ts =>
          simp only [List.getElem?_cons_succ] at hf
          simp only [OwnSt.wfList, Bool.and_eq_true] at ht
          exact OwnSt.fieldAt_wf ts f Ts T' ht.2 hf

/-- `OwnSt.setAt` takes its first step the same way from a wholly `Owned` node as
from a field record, because a node with no record of its own has every field
`owned` (helper). -/
theorem OwnSt.setAt_cons_owned (f : Nat) (π : List Nat) (u : OwnSt) :
    OwnSt.setAt .owned (f :: π) u
      = .fields (OwnSt.setField (OwnSt.fieldStates .owned) f
          ((OwnSt.fieldAt (OwnSt.fieldStates .owned) f).setAt π u)) := rfl

/-- The field-record form of the same step (helper). -/
theorem OwnSt.setAt_cons_fields (ts : List OwnSt) (f : Nat) (π : List Nat) (u : OwnSt) :
    OwnSt.setAt (.fields ts) (f :: π) u
      = .fields (OwnSt.setField (OwnSt.fieldStates (.fields ts)) f
          ((OwnSt.fieldAt (OwnSt.fieldStates (.fields ts)) f).setAt π u)) := rfl

/-- The recorded slots of a state well formed at a declared struct type are
themselves well formed at the field types — trivially so for a node with no
record of its own (helper). -/
theorem OwnSt.wfList_fieldStates_struct {D : Decls} {t : OwnSt} {s : Nat} {sd : StructDecl}
    (hd : D.structs[s]? = some sd) (h : OwnSt.wf D t (Ty.struct s) = true) :
    OwnSt.wfList D t.fieldStates sd.fields = true := by
  cases t with
  | owned => rfl
  | movedOut => rfl
  | fields ts => rw [OwnSt.wf_fields_struct D s sd hd] at h; exact h

/-- The array form of the same (helper). -/
theorem OwnSt.wfList_fieldStates_array {D : Decls} {t : OwnSt} {T₁ : Ty} {n : Nat}
    (h : OwnSt.wf D t (Ty.array T₁ n) = true) :
    OwnSt.wfList D t.fieldStates (List.replicate n T₁) = true := by
  cases t with
  | owned => rfl
  | movedOut => rfl
  | fields ts => rw [OwnSt.wf_fields_array D T₁ n] at h; exact h

/-- **§5's `Σ[ p ↦ u ]` stays inside the shapes of the type.** Writing a state of
`p`'s own type at a path the root's declared type has (`Ty.atPath`, §5
preamble's `Γ ⊢ p : T`) leaves a state of the root's type, the `owned` padding
`OwnSt.setField` inserts included. This is what makes `OwnSt.wf` an invariant
of the rules that write — (Use-Move) §5.1, (@Drop) §5.3 and (Assign) §5.2 all
write at a path their own `Ty.atPath` premise typed — rather than a condition
they would have to carry. -/
theorem OwnSt.setAt_wf {D : Decls} (T' : Ty) (u : OwnSt) (hu : OwnSt.wf D u T' = true) :
    ∀ (π : List Nat) (t : OwnSt) (T : Ty), OwnSt.wf D t T = true →
      T.atPath D π = some T' → OwnSt.wf D (t.setAt π u) T = true := by
  intro π
  induction π with
  | nil =>
      intro t T _ hp
      simp only [Ty.atPath, Option.some.injEq] at hp
      subst hp
      exact hu
  | cons f π ih =>
      intro t T ht hp
      have key : ∀ (Ts : List Ty) (T₁ : Ty), OwnSt.wfList D t.fieldStates Ts = true →
          Ts[f]? = some T₁ → T₁.atPath D π = some T' →
          OwnSt.wfList D (OwnSt.setField t.fieldStates f
            ((OwnSt.fieldAt t.fieldStates f).setAt π u)) Ts = true := by
        intro Ts T₁ hts hf hpp
        exact OwnSt.setField_wf t.fieldStates f _ Ts T₁ hts hf
          (ih _ T₁ (OwnSt.fieldAt_wf t.fieldStates f Ts T₁ hts hf) hpp)
      cases T with
      | struct s =>
          cases hd : D.structs[s]? with
          | none => simp [Ty.atPath, Ty.fieldAt, hd] at hp
          | some sd =>
              simp only [Ty.atPath, Ty.fieldAt, hd] at hp
              split at hp
              · rename_i T₁ hf
                have hts := OwnSt.wfList_fieldStates_struct hd ht
                cases t with
                | movedOut => rfl
                | owned =>
                    rw [OwnSt.setAt_cons_owned, OwnSt.wf_fields_struct D s sd hd]
                    exact key sd.fields T₁ hts hf hp
                | fields ts =>
                    rw [OwnSt.setAt_cons_fields, OwnSt.wf_fields_struct D s sd hd]
                    exact key sd.fields T₁ hts hf hp
              · exact absurd hp (by simp)
      | array T₁ n =>
          by_cases hlt : f < n
          · simp only [Ty.atPath, Ty.fieldAt, if_pos hlt] at hp
            have hts := OwnSt.wfList_fieldStates_array ht
            have hf : (List.replicate n T₁)[f]? = some T₁ := by
              simp [hlt]
            cases t with
            | movedOut => rfl
            | owned =>
                rw [OwnSt.setAt_cons_owned, OwnSt.wf_fields_array D T₁ n]
                exact key (List.replicate n T₁) T₁ hts hf hp
            | fields ts =>
                rw [OwnSt.setAt_cons_fields, OwnSt.wf_fields_array D T₁ n]
                exact key (List.replicate n T₁) T₁ hts hf hp
          · simp [Ty.atPath, Ty.fieldAt, hlt] at hp
      | int _ _ => simp [Ty.atPath, Ty.fieldAt] at hp
      | float _ => simp [Ty.atPath, Ty.fieldAt] at hp
      | bool => simp [Ty.atPath, Ty.fieldAt] at hp
      | unit => simp [Ty.atPath, Ty.fieldAt] at hp
      | enum _ => simp [Ty.atPath, Ty.fieldAt] at hp

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
exact, in both halves: the binary join is **commutative** (`OwnSt.join_comm`,
`Ctx.join_comm`), so which of two arms is taken first does not matter, and it
is **associative** over states that are shapes of their type
(`OwnSt.join_assoc`, `Ctx.join_assoc`), so the bracketing does not either.
`Ctx.joinAll_perm` below is the two together: the fold is invariant under a
permutation of the arms, which is what licenses reading `Ctx.joinAll` as the
unordered `join(Σ1, …, Σn)` the calculus writes.

Its premise — every arm's outgoing context a shape of its declared types
(`Ctx.Wf`) — is one the rules that *write* keep: `OwnSt.setAt_wf` for (Use-Move)
§5.1, (@Drop) §5.3 and (Assign) §5.2, `Ctx.joinAll_wf` for a nested (Match), and
`fnCtx`/`armCtx` push `Owned`. §5.7's `⊥` has no state to break it: the
judgment carries §5.3's `Ω`, so a diverging arm contributes nothing to the
join, and `Typed.wf` (end of this module) proves the invariant preserved
judgment-wide. The theorems below keep `Ctx.Wf` as a premise, which `Typed.wf`
discharges for every derivation from a well-formed context (RUE-2340: when ⊥
concluded at an arbitrary context, that preservation theorem was false).
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

/-! ### The outgoing result `Ω` (§5.3)

§5.3 gives every judgment an outgoing result `Ω ::= Σ;Δ | ⊥;Δ`: an optional
normal ownership state together with the **edge deliveries** `Δ` the
expression's reachable diverging edges make. `Out` is that pair. `norm` is the
normal state — `some Σ'` when evaluation can reach the next expression, `none`
for §5.7's `⊥` — and `brk` is the part of `Δ` a later consumer reads a *state*
from.

Which deliveries `brk` carries is a choice §5.7's closing note leaves to the
mechanization: "check each edge where it fires — so long as the sets `B` and
`X` it computes are the ones these rules define". The fragment checks a
`return` where it fires (`Typed.ret` carries (Fn) §5.8's residual-linear
obligation at the edge), and §5.7 exempts a `@panic` from §5.6, so neither
needs a consumer and neither is recorded. The deliveries that do need one are
`⟨break, Σ⟩`, which (Loop-Break) §5.7 joins at the loop's exit: (Break)
(`Typed.brk`) makes one, recording the whole context in force at the edge, and
every rule carries its premises' deliveries into its conclusion, as §5.3's
**Threading** paragraph says, until the innermost enclosing loop consumes
them. The list is the calculus's set: order and repetition carry no meaning
(`Ctx.joinAll_perm` is why the exit join may read it in order). -/

/-- §5.3's outgoing result `Ω`: `norm = some Σ'` is `Σ';Δ` and `norm = none`
is `⊥;Δ`, with `brk` the recorded deliveries `Δ` (section docstring). -/
structure Out where
  /-- The normal outgoing state, or `none` for §5.7's `⊥`. -/
  norm : Option Ctx
  /-- The `⟨break, Σ⟩` deliveries, each with the state in force at its edge. -/
  brk : List Ctx

/-- §5.3's `Ω ⊕ Δ`: add a continuing prefix's deliveries to an outcome, which
keeps its own continuing-or-divergent shape — `(Σ';Δ') ⊕ Δ = Σ';(Δ' ∪ Δ)` and
`(⊥;Δ') ⊕ Δ = ⊥;(Δ' ∪ Δ)`. -/
def Out.add (Ω : Out) (Δ : List Ctx) : Out := ⟨Ω.norm, Ω.brk ++ Δ⟩

/-- §5.5's branch join over `Ω` for two arms: "the normal state is `join` of
the continuing arms' normal states (`⊥` when no arm continues)". A divergent
arm contributes nothing, which is how (Sub-Never) §5.7 lets it sit beside a
continuing one; `none` is a join the continuing arms disagree on. -/
def Ctx.joinOpt (D : Decls) : Option Ctx → Option Ctx → Option (Option Ctx)
  | none, o => some o
  | some a, none => some (some a)
  | some a, some b => (Ctx.join D a b).map some

/-- (Match) §5.5's n-way join over `Ω`: the fold `Ctx.joinAll` over the
normal states of the arms that **continue**, or `⊥` when none does. -/
def Ctx.joinOpts (D : Decls) (os : List (Option Ctx)) : Option (Option Ctx) :=
  match os.filterMap id with
  | [] => some none
  | Γ :: Γs => (Ctx.joinFold D Γ Γs).map some

/-! ### The loop head and the loop's exits (§5.7)

§5.7 types a loop body once, at the **loop-head state** `Σ_h`, "the entry
state joined with the states at the body's own reachable back edges":

```
  head(Σ, e) = Σ_h   where
    Γ;Σ_h;Λ ⊢ e ⇒ unit ⊣ Ω_h
    B_h = { Σ_e | Ω_h = Σ_e;Δ_h } ∪ { Σ_c | ⟨continue, Σ_c⟩ ∈ Δ_h }
    outside_loop(Σ_h) = join({ outside_loop(Σ) } ∪ { outside_loop(Σ_b) | Σ_b ∈ B_h })
```

The core has no `continue` (§2 elaborates it to the back edge), so `B_h` is at
most the body's own normal completion state, and that state has the head's
skeleton — a body's `let`s close before it completes — so `outside_loop` is
the identity on it. `LoopHead` is the equation, stated over the body's normal
outgoing state `o`: `Σ_h = Σ` when the body never completes (`B_h = ∅`), and
`Σ_h = join(Σ, Σ_e)` when it completes at `Σ_e`. It is a **fixpoint** premise:
`o` is read off the judgment that types the body *at* `Σ_h`. Any solution is
admitted, as the calculus admits any, the non-least ones included — an
affine outer binding the body never touches may be `MovedOut` at such a head,
since `join(Owned, MovedOut) = MovedOut`. A non-least head can only reject
more: its linear-carrying paths equal the entry's (the join is undefined where
they differ), every rule is antitone in `MovedOut` on the other paths (a use,
`@drop` and `fully-owned` want `Owned`; an assignment takes either;
`NoResidualLinear` and the overwrite premise read linear content only), and
its post-loop state is only more moved. `check` computes the least one by
iteration (`Checker.lean`).

The second clause asks that `Σ_h`, when a back edge produced it, be a state
of its declared types (`Ctx.Wf`). That is not a premise the calculus writes,
because §5's states *are* shapes of their types; here it is the invariant
`Typed.wf` proves of every state a rule writes, and a join of two such states
is one (`Ctx.join_wf`). The equation alone does not give it: `Σ_h` appears on
both sides, and a field record at a scalar type, which no rule writes, can
solve it. With it, re-entering the loop at `Σ_h` solves the equation again
(`LoopHead.reenter`), which is what the back-edge proof in `soundness` needs.
-/

/-- §5.7's loop-head equation `Σ_h = head(Σ, e)`, over the body's normal
outgoing state `o` (section docstring): `Σ_h` is the §5.5 join of the entry
state `Γ` with the body's back-edge state when it has one, and is `Γ` itself
when it has none; a head a back edge produced is a state of its types. -/
def LoopHead (D : Decls) (Γ : Ctx) (o : Option Ctx) (Γh : Ctx) : Prop :=
  Ctx.joinOpt D (some Γ) o = some (some Γh) ∧ ∀ Γe, o = some Γe → Ctx.Wf D Γh

/-- The loop-local part of a `⟨break, Σ_x⟩` delivery made by a loop body typed
at `Γh`: the bindings the body opened and had not closed where the `break`
fired, innermost first. §5.7 discharges their §5.6 obligation "at the exit
itself, where their scopes end" (helper). -/
def Ctx.loopLocals (Γh Γb : Ctx) : Ctx := Γb.take (Γb.length - Γh.length)

/-- §5.7's `outside_loop(Σ_x)` for a delivery made by a body typed at `Γh`:
the bindings in scope at the loop's entry, which are the delivered context's
outermost `|Γh|` entries (helper). -/
def Ctx.outsideLoop (Γh Γb : Ctx) : Ctx := Γb.drop (Γb.length - Γh.length)

mutual
/-- `Γ ; Σ ⊢ e ⇒ T ⊣ Ω` (§5), over the fused context, under the program `P`
and the enclosing function's return type `R`, with §5.3's outgoing result
`Ω` (`Out`).

**Reachability is in the rules' shape**, as §5.7 says: the `-Bottom` rules
type nothing past a diverging subexpression. `binopBot`, `floatBinopBot`,
`indexReadBot`, `indexWriteBotRhs`, `indexWriteBotIdx`, `assignBot`,
`matchBot`, `iteBot` and `TypedArgs.consBot` are (Strict-Bottom) §5.3 at the
strict contexts the fragment has; `seqBot` and `letBot` are (Seq-Bottom) and
(Let-Bottom); `letInDiv` is (Let) whose tail diverges; `retBot` is
(Return-Bottom) §5.7; `TypedArms.armDiv` is a `match` arm that diverges. A
rule with one operand and nothing after it (`neg`, `dbg`, `call`, …) passes
the operand's `Ω` on unchanged, which is §5.3's threading convention and
(Strict-Bottom) at once. (Strict-Bottom) concludes at the construct's own
type `T_E`, so its variants carry the premises that name that type and
nothing more.

**(Sub-Never) §5.7 is folded in**, because the fragment has no `never` type:
a rule whose conclusion §5.7 types at `never` — (Return-Value),
(Return-Bottom), (Panic), (Seq-Bottom), (Let-Bottom), and the (Strict-Bottom)
of a condition or a scrutinee, whose `T_E` is the arms' type — concludes at
every type instead. (Sub-Never) leaves `Ω` untouched, so every one of them
concludes at `⊥`.

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
and `panic` is (Panic), each with (Sub-Never) folded in (§5.7, §5.8); `brk` is
(Break), `loopDiv` is (Loop-Div-Backedge) and (Loop-Div), and `loopBreak` and
`loopBreakDiv` are (Loop-Break) with and without a reachable exit (§5.7). -/
inductive Typed (P : Program) (R : Ty) : Ctx → Expr → Ty → Out → Prop where
  /-- (Lit) §5.8: an integer literal at the `int(w,s)` elaboration resolved
  for it (`4.1:2`), denoting a value of that type (§6.1's `n_T` bound). -/
  | intLit {Γ w s n} :
      InBounds w s n →
      Typed P R Γ (.intLit w s n) (.int w s) ⟨some Γ, []⟩
  /-- (Lit) §5.8: a boolean literal. -/
  | boolLit {Γ b} :
      Typed P R Γ (.boolLit b) .bool ⟨some Γ, []⟩
  /-- (Lit) §5.8: the unit literal. -/
  | unitLit {Γ} :
      Typed P R Γ .unitLit .unit ⟨some Γ, []⟩
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
      Typed P R Γ (.use p) T ⟨some Γ, []⟩
  /-- (Use-Move) §5.1: a use of an `Affine`/`Linear` place moves it out — at a
  projection, the **partial move** of `3.8:22`, which marks exactly `p` and
  removes every path under it while leaving `p`'s siblings alone.
  `fully-owned(Σ, p)` is the premise (`3.8:26`: handing an aggregate with a
  hole to a new owner is ill-formed), and `noDtorPrefix` is `3.9:34`'s
  restriction (E0456). `rootIdxOnly` is §4.2's third restriction, `3.8:68`'s
  "element moves only at the root" (E0904): the move may take one element out
  of the **root binding**'s array, and out of no array reached through a
  further step (`Syntax.lean`). `declaredPrefix … = none` is §5.1's
  `Ordinary` plan premise, exactly as the `Copy` rule above carries it. -/
  | useMove {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.fullyOwned = true →
      en.ty.atPath P.decls p.path = some T →
      T.mult P.decls ≠ .copy →
      noDtorPrefix P.decls en.ty p.path = true →
      declaredPrefix P.decls en.ty p.path = none →
      rootIdxOnly P.decls en.ty p.path = true →
      Typed P R Γ (.use p) T ⟨some (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut))), []⟩
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

  `rootIdxOnly` is deliberately **not** a premise here, where (Use-Move)
  carries it. §4.2's `dl` is explicit that "the selected path may pass through
  nested structs and constant-index arrays", `3.8:71` says that consuming the
  linear sub-places of an array reached through a field projection discharges
  the array field's obligation, and the compiler accepts every shape that
  admits: `x.arr[0]` on a declared-`linear` `x` (probe b3), `h.arr[0].x0`
  whose *consumed* place is an element of an array reached through a field
  (probe d1b), and `a[0][0].x0` whose consumed place sits at a nested index
  (probe d2b) — although the same `a[0][0]` moved **ordinarily** is E0904
  (probe e1). A retained *array* in the residue needs nothing of the sort
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
      Typed P R Γ (.use p) T ⟨some (Γ.set p.root (en.setSt (en.st.setAt πd .movedOut))), []⟩
  /-- (Arith) and (Ord) §5.8, in one rule because they differ only in the
  type they conclude at (`BinOp.resultTy`): both operands share one
  `int(w,s)`, typed left to right with Σ threaded (`4.2:1`), and the result is
  that same type for the arithmetic, bitwise and shift operators and `bool`
  for the ordering compares (`4.3:1`). The shift operators take their amount
  at the shifted operand's own type, which is `4.3a:9` and is why they need no
  second operand type here. -/
  | binop {Γ Γ₁ Ω₂ Δ₁ op e₁ e₂ w s} :
      Typed P R Γ e₁ (.int w s) ⟨some Γ₁, Δ₁⟩ → Typed P R Γ₁ e₂ (.int w s) Ω₂ →
      op.intAdmits = true →
      Typed P R Γ (.binop op e₁ e₂) (op.resultTy (.int w s)) (Ω₂.add Δ₁)
  /-- (Strict-Bottom) §5.3 at `binop`'s left operand: once `e₁` diverges the
  right operand is never reached, so it is not typed, and the form concludes
  at `⊥` with `e₁`'s deliveries and at its own type `T_E`, (Arith)/(Ord)'s
  `op.resultTy (int(w,s))` — not at `never`. A right operand that diverges
  needs no rule of its own: `binop` passes `e₂`'s `Ω` on. -/
  | binopBot {Γ Δ₁ op e₁ e₂ w s} :
      Typed P R Γ e₁ (.int w s) ⟨none, Δ₁⟩ →
      op.intAdmits = true →
      Typed P R Γ (.binop op e₁ e₂) (op.resultTy (.int w s)) ⟨none, Δ₁⟩
  /-- (Float-Arith), (Float-Ord) and (Total-Cmp) §5.8, in one rule for the
  same reason `binop` fuses (Arith) and (Ord): they differ only in the type
  they conclude at (`BinOp.resultTy` — `float(w)`, `bool`, `int(32,signed)`).
  Both operands share **one** `float(w)`: `3.12:13` gives no implicit
  widening, so an `f32`/`f64` mix has no derivation, and `3.12:14` relates no
  float operand to an integer one — the only bridges are the intrinsics.
  `BinOp.floatAdmits` is §5.8's "rejected by the absence of a rule" for `%`
  (`3.12:25`) and for the bitwise and shift operators, written as a side
  condition because one constructor stands for the three rule groups. -/
  | floatBinop {Γ Γ₁ Ω₂ Δ₁ op e₁ e₂ w} :
      Typed P R Γ e₁ (.float w) ⟨some Γ₁, Δ₁⟩ → Typed P R Γ₁ e₂ (.float w) Ω₂ →
      op.floatAdmits = true →
      Typed P R Γ (.binop op e₁ e₂) (op.resultTy (.float w)) (Ω₂.add Δ₁)
  /-- (Strict-Bottom) §5.3 at a float `binop`'s left operand, exactly as
  `binopBot` is at an integer one. -/
  | floatBinopBot {Γ Δ₁ op e₁ e₂ w} :
      Typed P R Γ e₁ (.float w) ⟨none, Δ₁⟩ →
      op.floatAdmits = true →
      Typed P R Γ (.binop op e₁ e₂) (op.resultTy (.float w)) ⟨none, Δ₁⟩
  /-- (Neg) §5.8: negation demands a **signed** operand (`4.2:6`; rejecting it
  on an unsigned type is `4.2:14`) and concludes at that type. -/
  | neg {Γ Ω e w} :
      Typed P R Γ e (.int w .signed) Ω →
      Typed P R Γ (.unop .neg e) (.int w .signed) Ω
  /-- (Float-Neg) §5.8: float negation applies at **every** float type, where
  (Neg) restricts the integer case to a signed one (`3.12:24`, `4.2:14`), and
  §6.4 makes it total rather than trapping on a minimum — a sign flip, on
  `-0.0` and on a NaN alike. -/
  | floatNeg {Γ Ω e w} :
      Typed P R Γ e (.float w) Ω →
      Typed P R Γ (.unop .neg e) (.float w) Ω
  /-- (Not) §5.8: logical negation demands `bool` (`4.4:2`). The bitwise
  operators do not accept `bool` at all (`4.3a:18`, `4.3a:19`), which is why
  `binop` above is stated only at `int(w,s)`. -/
  | notOp {Γ Ω e} :
      Typed P R Γ e .bool Ω →
      Typed P R Γ (.unop .not e) .bool Ω
  /-- (BitNot) §5.8: the bitwise complement takes any integer type
  (`4.3a:3`, `4.3a:4`) and concludes at it. -/
  | bitnot {Γ Ω e w s} :
      Typed P R Γ e (.int w s) Ω →
      Typed P R Γ (.unop .bitnot e) (.int w s) Ω
  /-- (Int-Cast) §5.8 (`4.13:24`–`4.13:27`): the operand is any integer type
  and the result is the one elaboration took from the use site, which the form
  carries. Whether the value survives the conversion is dynamic (`4.13:28`,
  §6.4's own trap rule), not a typing question. -/
  | intCast {Γ Ω w s w' s' e} :
      Typed P R Γ e (.int w' s') Ω →
      Typed P R Γ (.intCast w s e) (.int w s) Ω
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
      Typed P R Γ (.floatLit w l) (.float w) ⟨some Γ, []⟩
  /-- (Int-To-Float) §5.8: the operand is an integer of any width and
  signedness (`3.12:16`, `4.13:139`) and the result is the `float(w)`
  elaboration took from the use site. It never traps (§6.4). -/
  | intToFloat {Γ Ω w w' s' e} :
      Typed P R Γ e (.int w' s') Ω →
      Typed P R Γ (.fintrin (.intToFloat w) e) (.float w) Ω
  /-- (Float-To-Int), (Float-Cast) and (Float-Round) §5.8, in one rule: each
  takes one `float(w)` operand and concludes at the type the form carries
  (`FloatIntrin.resTy`). `FloatIntrin.floatSrc` carries (Float-Cast)'s
  `w' ≠ w` side condition (`3.12:19`) and keeps `@int_to_float`, whose operand
  is an integer, on its own rule above. Whether a `@float_to_int` *survives*
  is dynamic, not a typing question: `3.12:18` and §6.4's
  (D-Float-To-Int-Trap). -/
  | floatIntrin {Γ Ω k w e} :
      Typed P R Γ e (.float w) Ω → k.floatSrc w = true →
      Typed P R Γ (.fintrin k e) (k.resTy w) Ω
  /-- (Panic) §5.8 with (Sub-Never) folded in (§5.7), the same fold
  `Typed.ret` makes: `@panic` is `never`-typed, so the rule concludes at an
  arbitrary type, and at `⊥` with no delivery the fragment records (a
  `⟨panic, _⟩` delivery has no consumer; §5.7 exempts it). Unlike `ret` it imposes no
  residual-linear premise: §5.7 exempts the `⊥_panic` edge from §5.6's
  scope-exit check, and §6.12's own rule runs no drop. The message is a string
  literal the form carries rather than an operand, because the fragment has no
  string type, which is also why §5.8's operand-diverging companion has no
  instance. -/
  | panic {Γ T msg} :
      Typed P R Γ (.panic msg) T ⟨none, []⟩
  /-- (Dbg) §5.8: the operand is a value-context use of a type `@dbg` renders
  — `int(w,s)` or `bool` in this fragment (`Ty.observable`; the compiler
  rejects an aggregate with E0702) — and the form itself is `unit`. -/
  | dbg {Γ Ω e T} :
      Typed P R Γ e T Ω → T.observable = true →
      Typed P R Γ (.dbg e) .unit Ω
  /-- (Struct-Intro) §5.8: one initializer per declared field, typed in
  declaration order at its field's type with Σ threaded left to right
  (`3.6:5`, `3.6:6`, `3.6:15`), and the result owns every field — which is why
  `class(S)` is the field join of §3. -/
  | mkStruct {Γ Ω s args sd} :
      P.decls.structs[s]? = some sd →
      TypedArgs P R Γ args sd.fields Ω →
      Typed P R Γ (.mkStruct s args) (.struct s) Ω
  /-- (Enum-Intro) §5.5: one payload argument per declared component of the
  variant the tag names, typed left to right at its component's type with Σ
  threaded (§6.2's order, the same `TypedArgs` (Struct-Intro) uses), and the
  result owns the tag and the supplied payload — which is why `class(E)` is the
  payload join of §3 (`6.3:19`). The tag is the variant's declaration slot, so
  `variants[k]? = some Ts` is both §5.5's `E = enum { …, Kj(T̄j), … }` premise
  and `6.3:16`'s "the variant exists" (E0420 otherwise); the argument count is
  `6.3:16`'s arity premise, carried by `TypedArgs`' own shape. -/
  | mkEnum {Γ Ω e k args ed Ts} :
      P.decls.enums[e]? = some ed →
      ed.variants[k]? = some Ts →
      TypedArgs P R Γ args Ts Ω →
      Typed P R Γ (.mkEnum e k args) (.enum e) Ω
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
  arm satisfies through (Sub-Never), which the `⊥` rules fold in, exactly as
  an `ite` arm does. An arm that continues leaves its payload locals' scope
  under §5.6: `TypedArms` carries the same residual-linear check `Typed.letIn`
  carries for its one binder, over the `ai` entries the arm pops. §5.5 joins
  the **continuing** arms' outgoing states n-way (`Ctx.joinOpts`, the fold
  `Ctx.joinAll` over them), and a diverging arm is "excluded from the state
  join" and contributes only its deliveries. The delivery set is the
  scrutinee's `Δ_0` with every arm's, continuing or not. -/
  | «match» {Γ Γ₀ Δ₀ o os Δs scrut arms e ed T} :
      Typed P R Γ scrut (.enum e) ⟨some Γ₀, Δ₀⟩ →
      P.decls.enums[e]? = some ed →
      arms.length = ed.variants.length →
      TypedArms P R Γ₀ arms ed.variants T os Δs →
      Ctx.joinOpts P.decls os = some o →
      Typed P R Γ (.«match» scrut arms) T ⟨o, Δs ++ Δ₀⟩
  /-- (Strict-Bottom) §5.3 at a `match` scrutinee: a scrutinee that diverges
  reaches no arm, so no arm is typed. `T_E` is the arms' common type, which
  nothing then constrains, so the rule concludes at any type — the reading of
  §5.7's (Sub-Never) the `⊥` rules share.

  **Premises omitted, deliberately.** (Strict-Bottom) as §5.3 writes it keeps
  only the hole's premise, so this rule has none of the construct's own
  syntactic premises either: not that the enum is declared, and not
  (Match)'s exhaustiveness (one arm per variant). No arm is reached, so no arm
  is read; `check` keeps neither premise either, but the compiler reports a
  missing arm in a `match` whose scrutinee diverges (E0600), so this is one of
  the dead-code shapes `Checker.lean`'s docstring lists. -/
  | matchBot {Γ Δ₀ scrut arms e T} :
      Typed P R Γ scrut (.enum e) ⟨none, Δ₀⟩ →
      Typed P R Γ (.«match» scrut arms) T ⟨none, Δ₀⟩
  /-- (Array-Intro) §5.8: all `n` elements share one element type `T`
  (`3.5:3`, `7.1:3`), are typed left to right with Σ threaded, and the array
  owns all of them — which is why `class([T; n])` is §3's lift of `class(T)`.
  `n` is the literal's own length (`7.1:4` — the declared size must match), and
  `n = 0` is admitted: `[]` is the zero-sized `[T; 0]` and uses nothing. The
  element-type list is `List.replicate n T`, so this rule is
  (Struct-Intro)'s `TypedArgs` at a constant field list. -/
  | mkArray {Γ Ω T args} :
      TypedArgs P R Γ args (List.replicate args.length T) Ω →
      Typed P R Γ (.mkArray T args) (.array T args.length) Ω
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
  | repeatArray {Γ Ω T e n} :
      Typed P R Γ e T Ω → T.mult P.decls = .copy →
      Typed P R Γ (.repeatArray T e n) (.array T n) Ω
  /-- (Use-Untrackable-Dynamic-Copy) §5.1, at a read `p[e₁]π₁…[eₖ]πₖ` below one
  or more indices that are not compile-time constants: §4.2's
  `Untrackable(OrdinaryDynamic)` plan, and the *only* successful static rule
  for it. The place is `p` (a constant `Place`), then `k ≥ 1` dynamic steps,
  each followed by a constant path of field slots and constant indices, so
  `a[i]`, `a[i].x0`, `h.arr[i].x0`, `a[i][j]` and `a[i][0].x1` are all this
  form (probes q01, q08, q09, q10). `Place` stays constant-only: a dynamic step
  is never a path of Σ, which is what keeps Σ finite (`3.8:68`).

  The premises, in the order the rule reads them.
  * The index expressions are typed **left to right** at integer types, with Σ
    threaded (`TypedArgs` at a list of `int(w,s)`; `4.11:4` admits any integer
    type), and the place is read on the resulting context; `eval` runs them in
    the same order. `4.11:14` puts a full index expression's *base* before its
    index, and that is unobservable here because the base is a `Place`: reading
    one runs nothing and threads no Σ.
  * `fully-owned(Σ, p)` at the array the **first** dynamic step indexes —
    stronger than §5.1's `Σ(p) = Owned`, and `3.8:70`/`7.1:45`'s own rule: it
    is an error "to index the array with a non-constant index" while an element
    is moved out (E0205; probes q06, q19). It is `p`, not the root binding:
    `a[0][i]` after `a[1]` moved reads a whole `a[0]`, and the compiler accepts
    it (probe r01). A later dynamic step needs nothing more, because
    `fully-owned` at `p` is `fully-owned` at everything under it. A moved
    inner element under a second dynamic step is not merely untested but
    inexpressible: a nested element move such as `a[0][1]` is itself E0904
    (`rootIdxOnly`; review probes a1–a3).
  * `Γ ⊢ p[…]… : T` is `Ty.atPath` to `p` and then `Ty.atDyn` through the
    dynamic tail, which fails unless every dynamic step is taken at an array.
  * `class(T) = Copy` is the rule's own premise, and §4.2's "there is no
    successful static rule … when `class(T) ∈ {Affine,Linear}`" is that
    premise's absence rather than a rejection of its own (E0904; probes q02,
    q15).
  * No declared-`linear` proper prefix anywhere along the complete path:
    `declaredPrefix … = none` above the first dynamic step and
    `Ty.dynNoDeclared` below it keep §4.2's
    `Untrackable(DeclaredLinearDynamic)` — ill-formed there — without an
    instance (E0904; probes q11, r07).

  The read copies, so the outgoing state is the indices'. Whether each index
  is *in range* is dynamic (`7.1:10`, §6.5's (D-Index-Trap)), not a typing
  question. -/
  | indexRead {Γ Γ₁ Δ p idx πs Ts en u Ta T} :
      TypedArgs P R Γ idx Ts ⟨some Γ₁, Δ⟩ → Ts.all Ty.isInt = true →
      idx.length = πs.length → πs ≠ [] →
      Γ₁[p.root]? = some en →
      en.st.get p.path = some u → u.fullyOwned = true →
      en.ty.atPath P.decls p.path = some Ta →
      Ta.atDyn P.decls πs = some T →
      T.mult P.decls = .copy →
      declaredPrefix P.decls en.ty p.path = none →
      Ta.dynNoDeclared P.decls πs = true →
      Typed P R Γ (.indexRead p idx πs) T ⟨some Γ₁, Δ⟩
  /-- (Strict-Bottom) §5.3 at a dynamic index: an index expression diverges,
  so the place is never navigated and no premise about its state is read.
  The premises left are the ones that name `T_E`, the leaf's type — the
  index list's shape, and `Γ ⊢ p[…]… : T` read on the incoming context, whose
  skeleton is the one every later state has. -/
  | indexReadBot {Γ Δ p idx πs Ts en Ta T} :
      TypedArgs P R Γ idx Ts ⟨none, Δ⟩ → Ts.all Ty.isInt = true →
      idx.length = πs.length → πs ≠ [] →
      Γ[p.root]? = some en →
      en.ty.atPath P.decls p.path = some Ta →
      Ta.atDyn P.decls πs = some T →
      Typed P R Γ (.indexRead p idx πs) T ⟨none, Δ⟩
  /-- (Assign) §5.2 below a dynamic index, `p[e₁]π₁…[eₖ]πₖ = e` (`7.1:30`,
  `4.11:12`): an in-place mutation that modifies the array without moving it.
  The root must be a `μ = mut` binding (§5 preamble).

  **The right-hand side is typed first**, then the index expressions left to
  right, with Σ threaded in that order: `5.2:14` is normative ("the right-hand
  side `expression` is evaluated first … any index subexpressions appearing in
  the target … are evaluated after the right-hand side, in source order"),
  §6.2's `assign p = E` context says the same, and the compiler agrees (probes
  q14, q20, r10).

  The destination is **not** a use, so the read's `class(T) = Copy` premise
  does not transfer here: what (Assign) demands of a destination is its own
  last premise, `Σ1(p) = MovedOut ∨ ¬carries_linear(T)` at the leaf. A place
  under a runtime index can never be proven `MovedOut` (`3.8:77`), so the
  disjunction is its right half, `class(T) ≠ Linear`: an affine, even
  destructor-bearing, leaf is admitted and the machine's overwrite-drop runs
  its glue (probe q04), while a linear-carrying one is E0493 (probe q05).

  There is **no plan premise**: §4.2's plans classify value-context uses, and
  an assignment destination is not one. The compiler admits a dynamic-index
  write under a declared-`linear` prefix above the index (`v0.x0[i] = 9`,
  second-review probe c3; `v.arr[i].x1 = 9`, probe r06) **and** below it
  (`a[i].x0 = 5` on `[L; 2]` with `L` declared `linear`, probe r05, which
  prints `6`). The write lands on a leaf the declared-`linear` place still
  owns whole — `fully-owned` below guards that — and consumes nothing, so
  `Untrackable(DeclaredLinearDynamic)` has no instance at a write.

  `3.8:72`/`7.1:46` — "while one or more elements of an array are moved out,
  it is a compile-time error to assign into the array" — is `fully-owned(Σ, p)`
  on the post-operand state at the array the first dynamic step indexes, and
  `assignArrayOk` at any array the constant place stepped through to reach it
  (`a[0][i].k = 5` after a move of `a[1]` is E0480, probe r02; `a[i].k = 5`
  after `a[0]` moved, probes q07, q18). And `3.8:55`'s reinitialization is
  (Assign)'s own `Σ1[p ↦ Owned]`, taken at the **whole array** `p`: `7.1:46`
  says an element write "does not reinstate per-element ownership", and on
  the `fully-owned` premise there is nothing to reinstate, so writing `Owned`
  at `p` changes no path's state.

  `en₀.st.get p.path = some u₀` constrains `u₀` nowhere, and deliberately: it
  is (Assign)'s own incoming `Σ(p)` lookup, whose content is that the
  destination path is *reachable* — `OwnSt.get` is `none` under a moved-out
  prefix — while every condition on the state itself is read after the operands
  have run, on `u₁`, because that is the state the write overwrites. -/
  | indexWrite {Γ Γ₁ Γ₂ Δ₁ Δ₂ p idx πs e en₀ en₁ u₀ u₁ Ts Ta T} :
      Γ[p.root]? = some en₀ → en₀.mu = true →
      en₀.st.get p.path = some u₀ →
      en₀.ty.atPath P.decls p.path = some Ta →
      Ta.atDyn P.decls πs = some T →
      idx.length = πs.length → πs ≠ [] →
      Typed P R Γ e T ⟨some Γ₁, Δ₁⟩ →
      TypedArgs P R Γ₁ idx Ts ⟨some Γ₂, Δ₂⟩ → Ts.all Ty.isInt = true →
      Γ₂[p.root]? = some en₁ →
      en₁.st.get p.path = some u₁ → u₁.fullyOwned = true →
      assignArrayOk P.decls en₁.st en₁.ty p.path = true →
      T.mult P.decls ≠ .linear →
      Typed P R Γ (.indexWrite p idx πs e) .unit
        ⟨some (Γ₂.set p.root (en₁.setSt (en₁.st.setAt p.path .owned))), Δ₂ ++ Δ₁⟩
  /-- (Strict-Bottom) §5.3 at a dynamic-index write's right-hand side, which
  `5.2:14` evaluates first: it diverges, so neither the indices nor the
  destination are reached. `T_E` is `unit`, and (Strict-Bottom) puts no type
  on the hole, so nothing else is premised.

  **Premises omitted, deliberately.** (Strict-Bottom) as §5.3 writes it keeps
  only the hole's premise, so this rule has none of the construct's own
  syntactic premises either: not the `μ = mut` root, not the root's scope,
  and not the index list's shape. The destination is never written; `check`
  still demands the root and its mark, as `assignBot`'s docstring says. -/
  | indexWriteBotRhs {Γ Δ p idx πs e T} :
      Typed P R Γ e T ⟨none, Δ⟩ →
      Typed P R Γ (.indexWrite p idx πs e) .unit ⟨none, Δ⟩
  /-- (Strict-Bottom) §5.3 at a dynamic-index write's index list: the
  right-hand side ran and is a value in the evaluation context, so it is typed
  at the leaf's type, and an index diverges; the destination is never reached
  (the RHS value is the pending value `Dynamics.lean` describes, RUE-2316).
  The premises that name the leaf's type are kept, because the RHS is typed
  at it; (Assign)'s `μ = mut` and the index list's shape are omitted, for
  `indexWriteBotRhs`'s reason. -/
  | indexWriteBotIdx {Γ Γ₁ Δ₁ Δ₂ p idx πs e en₀ Ts Ta T} :
      Γ[p.root]? = some en₀ →
      en₀.ty.atPath P.decls p.path = some Ta →
      Ta.atDyn P.decls πs = some T →
      Typed P R Γ e T ⟨some Γ₁, Δ₁⟩ →
      TypedArgs P R Γ₁ idx Ts ⟨none, Δ₂⟩ → Ts.all Ty.isInt = true →
      Typed P R Γ (.indexWrite p idx πs e) .unit ⟨none, Δ₂ ++ Δ₁⟩
  /-- (@Drop-Copy) §5.3 at a `Copy` place below a dynamic index,
  `@drop(p[e₁]π₁…[eₖ]πₖ)`. §5.3's rule has no index premise and its prose
  admits `@drop(a[i])` on a `Copy`-element array at a dynamic index; the
  compiler accepts the form (probe d1), runs the indices and bounds-checks them
  (probe d3 traps), and gives it exactly the read's premises: an affine or
  linear place there is E0904, as its read is (probe d4). The premise is
  therefore the read's whole derivation, (Use-Untrackable-Dynamic-Copy) §5.1
  at the same place — `Copy` leaf, `fully-owned(Σ, p)`, no declared-`linear`
  prefix, integer indices typed left to right — and the conclusion is the
  read's outgoing context at type `unit`: a `Copy` place is moved by nothing,
  so there is no ownership effect to add. -/
  | indexDrop {Γ Ω p idx πs T} :
      Typed P R Γ (.indexRead p idx πs) T Ω →
      Typed P R Γ (.indexDrop p idx πs) .unit Ω
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
      Typed P R Γ (.drop p) .unit ⟨some Γ, []⟩
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
  and the compiler enforces it (E0406). `rootIdxOnly` is §4.2's root-index
  restriction, exactly as on (Use-Move) above: `@drop(a[0])` at the root drops
  exactly that element (probe a8) while `@drop(h.a[0])` through a field is
  E0904 (probe b18). -/
  | dropRes {Γ p en u T} :
      Γ[p.root]? = some en →
      en.st.get p.path = some u → u.isOwned = true →
      en.ty.atPath P.decls p.path = some T →
      T.mult P.decls ≠ .copy →
      noDtorPrefix P.decls en.ty p.path = true →
      declaredPrefix P.decls en.ty p.path = none →
      (u.fullyOwned = true ∨ residualLinearBelow P.decls u T = false) →
      rootIdxOnly P.decls en.ty p.path = true →
      Typed P R Γ (.drop p) .unit ⟨some (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut))), []⟩
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
  (probe d6c fixes the order — residue first, leaf second). `rootIdxOnly` is
  **not** carried, for the reason `useDeclared` above does not carry it;
  `@drop(a[0].x0)` on an `[T0; 2]` compiles and consumes the element (probe
  d3). -/
  | dropDeclared {Γ p en u πd πs Td T} :
      Γ[p.root]? = some en →
      declaredPrefix P.decls en.ty p.path = some (πd, πs) →
      en.st.get πd = some u → u.fullyOwned = true →
      en.ty.atPath P.decls πd = some Td →
      linearResidue P.decls Td πs = false →
      en.ty.atPath P.decls p.path = some T →
      noDtorPrefix P.decls en.ty p.path = true →
      Typed P R Γ (.drop p) .unit ⟨some (Γ.set p.root (en.setSt (en.st.setAt πd .movedOut))), []⟩
  /-- (Let) + §5.6 scope exit: the binder enters `Owned`; at the body's end
  its residual state must not be an unconsumed linear value (the leak check).
  An `Owned` affine residue is dropped by the machine (§6.7); `MovedOut` needs
  nothing. The body's deliveries keep the binder on top of the state they
  record, which is the state in force at their edge; the conclusion is §5.3's
  `Ω_2 ⊕ Δ_1`. -/
  | letIn {Γ Γ₁ Γ₂ Δ₁ Δ₂ m e₁ e₂ T₁ T₂ en'} :
      Typed P R Γ e₁ T₁ ⟨some Γ₁, Δ₁⟩ →
      Typed P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ T₂ ⟨some (en' :: Γ₂), Δ₂⟩ →
      residualLinear P.decls en'.st en'.ty = false →
      Typed P R Γ (.letIn m e₁ e₂) T₂ ⟨some Γ₂, Δ₂ ++ Δ₁⟩
  /-- (Let) §5.3 with a tail that diverges, `Ω_2 = ⊥;Δ_2`: the whole `let` is
  divergent, `⊥;(Δ_2 ∪ Δ_1)`. No scope exit is reached on a normal path, so
  §5.6's check has nothing to read here; a `return` in the tail discharged it
  where it fired (`Typed.ret`). -/
  | letInDiv {Γ Γ₁ Δ₁ Δ₂ m e₁ e₂ T₁ T₂} :
      Typed P R Γ e₁ T₁ ⟨some Γ₁, Δ₁⟩ →
      Typed P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ T₂ ⟨none, Δ₂⟩ →
      Typed P R Γ (.letIn m e₁ e₂) T₂ ⟨none, Δ₂ ++ Δ₁⟩
  /-- (Let-Bottom) §5.3 with (Sub-Never) §5.7: the initializer diverges, so no
  binding is made and the body is not typed; the form is `never`, at any
  type. -/
  | letBot {Γ Δ₁ m e₁ e₂ T₁ T} :
      Typed P R Γ e₁ T₁ ⟨none, Δ₁⟩ →
      Typed P R Γ (.letIn m e₁ e₂) T ⟨none, Δ₁⟩
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

  `assignArrayOk` is §5.2's own array side condition (`3.8:72`, E0480), read
  on the post-RHS state like the `3.8:77` premise beside it: a destination that
  steps into an array demands the whole array, so an element is never
  reinitialized and the whole-array reassignment is the only recovery
  (`7.1:46`). Its docstring records the deviation from §5.2's disjunction as
  written.

  One **deviation** (N3): `Owned-Base` is demanded on the *incoming* state as
  well as the post-RHS one, so this rule is one premise stricter than §5.2,
  which states neither (U4 reads §5.1's "in any context" side condition for the
  post-RHS lookup). Nothing a program can observe turns on it: only an RHS that
  reinitialises the target's own moved-out prefix could make the incoming
  lookup fail where the post-RHS one succeeds. -/
  | assign {Γ Γ₁ Δ p e en₀ en₁ u₀ u₁ T} :
      Γ[p.root]? = some en₀ → en₀.mu = true →
      en₀.st.get p.path = some u₀ →
      en₀.ty.atPath P.decls p.path = some T →
      Typed P R Γ e T ⟨some Γ₁, Δ⟩ →
      Γ₁[p.root]? = some en₁ →
      en₁.st.get p.path = some u₁ →
      assignArrayOk P.decls en₁.st en₁.ty p.path = true →
      (u₁ = .movedOut ∨ T.mult P.decls ≠ .linear) →
      Typed P R Γ (.assign p e) .unit
        ⟨some (Γ₁.set p.root (en₁.setSt (en₁.st.setAt p.path .owned))), Δ⟩
  /-- (Strict-Bottom) §5.3 at an assignment's right-hand side: it diverges, so
  nothing is stored and no premise about the destination is read. `T_E` is
  `unit`, and (Strict-Bottom) puts no type on the hole.

  **Premises omitted, deliberately.** (Strict-Bottom) as §5.3 writes it keeps
  only the hole's premise, so this rule has none of the construct's own
  syntactic premises either: not (Assign)'s `μ = mut` root, and not even
  that the root is in scope (`Γ[p.root]?`) — the calculus assumes well-scoped
  syntax, elaborated before §5. So `Typed` derives `let x = 1; x = return 2`,
  which the compiler rejects (E0203). The destination is never written, so
  nothing unsound follows, and `check` still demands the root and its mark,
  so it refuses the shape: `check ⊊ Typed` here. Whether (Strict-Bottom)
  should keep a construct's syntactic premises is a question for the
  calculus. -/
  | assignBot {Γ Δ p e T} :
      Typed P R Γ e T ⟨none, Δ⟩ →
      Typed P R Γ (.assign p e) .unit ⟨none, Δ⟩
  /-- (Seq): the discarded value must not carry a linear value (`3.8:64`).
  Only the prefix must continue; the tail's `Ω_2` is the form's, with the
  prefix's deliveries added (`Ω_2 ⊕ Δ_1`, §5.3). -/
  | seq {Γ Γ₁ Δ₁ Ω₂ e₁ e₂ T₁ T₂} :
      Typed P R Γ e₁ T₁ ⟨some Γ₁, Δ₁⟩ → T₁.mult P.decls ≠ .linear →
      Typed P R Γ₁ e₂ T₂ Ω₂ →
      Typed P R Γ (.seq e₁ e₂) T₂ (Ω₂.add Δ₁)
  /-- (Seq-Bottom) §5.3 with (Sub-Never) §5.7: the prefix diverges, so the
  tail is unreachable and not typed, and the form is `never`, at any type. -/
  | seqBot {Γ Δ₁ e₁ e₂ T₁ T} :
      Typed P R Γ e₁ T₁ ⟨none, Δ₁⟩ →
      Typed P R Γ (.seq e₁ e₂) T ⟨none, Δ₁⟩
  /-- (If): both arms from the post-condition state, at one type `T` (a
  diverging arm meets it by (Sub-Never) §5.7). The outgoing state is the §5.5
  join of the arms that **continue** (`Ctx.joinOpt`: a diverging arm is
  excluded, `3.8:51`, and `⊥` when neither continues), and the deliveries are
  the condition's `Δ_0` with both arms'. -/
  | ite {Γ Γ₀ Δ₀ Ω₁ Ω₂ o c e₁ e₂ T} :
      Typed P R Γ c .bool ⟨some Γ₀, Δ₀⟩ →
      Typed P R Γ₀ e₁ T Ω₁ → Typed P R Γ₀ e₂ T Ω₂ →
      Ctx.joinOpt P.decls Ω₁.norm Ω₂.norm = some o →
      Typed P R Γ (.ite c e₁ e₂) T ⟨o, Ω₁.brk ++ Ω₂.brk ++ Δ₀⟩
  /-- (Strict-Bottom) §5.3 at a condition: it diverges, so neither arm is
  reached or typed. `T_E` is the arms' type, which nothing then constrains, so
  the rule concludes at any type. -/
  | iteBot {Γ Δ₀ c e₁ e₂ T} :
      Typed P R Γ c .bool ⟨none, Δ₀⟩ →
      Typed P R Γ (.ite c e₁ e₂) T ⟨none, Δ₀⟩
  /-- (Call) §5.8, by value: the callee's signature is looked up in the
  program, the arguments are checked against the parameter list in order with
  Σ threaded left to right, and the call's type is the callee's return type
  (`4.10:5`, `4.10:3`, `4.10:4`). The rule's by-reference clauses, `Λ_call`
  and its consistency and entry-recheck premises (§5.4), and the `Tr ≠ never`
  side condition with its (Call-Bottom) companion are not modelled: the
  fragment has no borrows and no `never` type. -/
  | call {Γ Ω f args fd} :
      P.fns[f]? = some fd →
      TypedArgs P R Γ args (fd.params.map Param.ty) Ω →
      Typed P R Γ (.call f args) fd.ret Ω
  /-- (Return-Value) §5.7 with (Sub-Never) folded in: the operand is checked
  against the enclosing function's declared return type `R`; §5.6's `⊥_exit`
  obligation is the frame-wide residual-linear premise (no binding of the
  current frame still carries residual linear content — `3.8:62`, and (Fn) §5.8's
  second clause, which is why an early `return` past a live linear is
  rejected). The conclusion is at an arbitrary type, which is (Sub-Never)
  §5.7 applied to `never`, and at `⊥` with the operand's deliveries. The
  `⟨ret, Σ_e⟩` delivery itself is not recorded: its one consumer, (Fn) §5.8's
  residual check, is this rule's premise, read where the edge fires — the
  architecture §5.7's closing note allows. -/
  | ret {Γ Γ₁ Δ e T} :
      Typed P R Γ e R ⟨some Γ₁, Δ⟩ →
      NoResidualLinear P.decls Γ₁ →
      Typed P R Γ (.ret e) T ⟨none, Δ⟩
  /-- (Return-Bottom) §5.7 with (Sub-Never): the operand itself diverges, so
  the `return` never fires, makes no delivery and reads no state. -/
  | retBot {Γ Δ e T} :
      Typed P R Γ e R ⟨none, Δ⟩ →
      Typed P R Γ (.ret e) T ⟨none, Δ⟩
  /-- **(Break) §5.7** with (Sub-Never) folded in: `break` yields no value to
  its own context, so it concludes at every type and at `⊥`, and it delivers
  `⟨break, Σ⟩` — the **whole** context in force where it fires, loop-local
  bindings included — to the innermost enclosing loop, which is the one
  consumer that reads it (`loopBreak`). Every rule between the two carries the
  delivery outward by §5.3's threading, whether or not the `break` is in tail
  position. "Well-formed only inside a loop" is (Fn)'s premise that a body
  delivers no `break` (`WfFn`). -/
  | brk {Γ T} :
      Typed P R Γ .brk T ⟨none, [Γ]⟩
  /-- **(Loop-Div-Backedge) and (Loop-Div) §5.7**, in one rule, because they
  differ only in whether the body reaches its back edge — which is what the
  body's own `Ω` says, so the rule reads it rather than splitting on it. The
  body syntactically contains no `break` targeting this loop (`4.8:21`,
  `Expr.breaks`), so the loop is `never`-typed, with (Sub-Never) folded in.

  The body is typed once, at the **loop-head state** `Σ_h = head(Σ, e)`
  (`3.8:79`): `LoopHead` is §5.7's defining equation, `Σ_h` the join of the
  entry state with the states at the body's own reachable back edges, read off
  the very judgment that types the body at `Σ_h`. When the body continues it
  re-enters itself forever, so it delivers `⟨diverge, Σ_h⟩`; the fragment
  checks that delivery where it fires, frame-wide, by `NoResidualLinear` —
  §5.6/§5.7's retained non-panic residual check, which the compiler enforces
  as E0406 for a linear local or a by-value parameter live at `loop { }`
  (`../03-metatheory.md` records the reading). When the body never completes
  (Loop-Div), `B_h = ∅`, so `Σ_h = Σ` and there is no diverge delivery: the
  loop is left only by the body's own `return`/`@panic`, which were checked
  where they fired. Either way the loop concludes at `⊥` and delivers no
  `break` outward (`Δ_out` removes this loop's own edges, and the syntactic
  premise says there are none — `Typed.brk_nil`).

  The diverge premise is there for fidelity to §5.7 and agreement with the
  compiler (E0406), not for safety: `soundness` does not use it, since a loop
  that never exits cannot leak in a way the machine sees — the premise `ret`'s
  residual check has at a `return` has no dynamic counterpart here. The same
  holds of `loopBreakDiv`'s. -/
  | loopDiv {Γ Γh Ωe e T} :
      Typed P R Γh e .unit Ωe →
      LoopHead P.decls Γ Ωe.norm Γh →
      e.breaks = false →
      (∀ Γe, Ωe.norm = some Γe → NoResidualLinear P.decls Γh) →
      Typed P R Γ (.loop e) T ⟨none, []⟩
  /-- **(Loop-Break) §5.7 with a reachable exit** (`X ≠ ∅`): the body contains
  a `break` targeting this loop (`4.8:21`), so the loop is `unit`-typed; the
  body is typed at the loop-head state (`LoopHead`, as for `loopDiv`), and the
  exits are read off that one judgment. `X` is the body's `brk`: each
  delivery is the whole context at its `break`, so the loop splits it at the
  loop's own depth. The loop-local bindings still open there (the prefix,
  `Ctx.loopLocals`) end at the exit, which discharges §5.6 for them
  ("discharged at the exit itself"; dynamically, §6.10's unwind); the rest
  (`Ctx.outsideLoop`) is `outside_loop(Σ_x)`, and the loop's normal outgoing
  state is §5.5's join over those (`3.8:80`), `Ctx.joinAll` in delivery
  order (the order is immaterial: `Ctx.joinAll_perm`). The body's normal
  completion is the back edge, which `LoopHead` already reads; it is not an
  exit. The loop consumes its own `break` deliveries (`Δ_out`), and the
  fragment has no others, so it delivers none. -/
  | loopBreak {Γ Γh Ωe Γx e} :
      Typed P R Γh e .unit Ωe →
      LoopHead P.decls Γ Ωe.norm Γh →
      e.breaks = true →
      (∀ Γb ∈ Ωe.brk, NoResidualLinear P.decls (Ctx.loopLocals Γh Γb)) →
      Ctx.joinAll P.decls (Ωe.brk.map (Ctx.outsideLoop Γh)) = some Γx →
      Typed P R Γ (.loop e) .unit ⟨some Γx, []⟩
  /-- **(Loop-Break) §5.7 with no reachable exit** (`X = ∅`): every targeting
  `break` is unreachable, so the loop is still `unit`-typed by `4.8:21`'s
  syntactic classification but has no post-loop state, `⊥`. With a reachable
  back edge it re-enters itself forever and delivers `⟨diverge, Σ_h⟩` exactly
  as `loopDiv` does, checked the same way; with none, the body's own
  `return`/`@panic` are its only exits. "The two forms differ only in
  `4.8:21`'s syntactic type, never in what they deliver." -/
  | loopBreakDiv {Γ Γh Ωe e} :
      Typed P R Γh e .unit Ωe →
      LoopHead P.decls Γ Ωe.norm Γh →
      e.breaks = true →
      Ωe.brk = [] →
      (∀ Γe, Ωe.norm = some Γe → NoResidualLinear P.decls Γh) →
      Typed P R Γ (.loop e) .unit ⟨none, []⟩

/-- An expression list typed left to right against a list of expected types
with Σ threaded (`Σ0 = Σ`, then `Γ;Σ_{i-1};Λ ⊢ e ⇒ Ti ⊣ Σi` for each `i`).
Both §5.8 forms that take one use it: (Call)'s by-value argument list and
(Struct-Intro)'s field initializers. (Call)'s by-reference argument forms, and
with them `Λ_call`, its consistency premise and the call-entry recheck, are
not in the fragment. -/
inductive TypedArgs (P : Program) (R : Ty) : Ctx → List Expr → List Ty → Out → Prop where
  /-- The empty list leaves Σ alone (`Σ0 = Σ`, §5.8). -/
  | nil {Γ} : TypedArgs P R Γ [] [] ⟨some Γ, []⟩
  /-- One member is a value-context use at its expected type (§4.2),
  threading Σ into the rest of the list (§5.8), whose outcome is the list's
  with this member's deliveries added (§5.3's `Ω ⊕ Δ`). -/
  | cons {Γ Γ₁ Δ₁ Ω e es T Ts} :
      Typed P R Γ e T ⟨some Γ₁, Δ₁⟩ → TypedArgs P R Γ₁ es Ts Ω →
      TypedArgs P R Γ (e :: es) (T :: Ts) (Ω.add Δ₁)
  /-- (Strict-Bottom) §5.3 at a list member: it diverges, so the members after
  it are never evaluated and not typed. The list still has one expected type
  per member — its arity is the construct's (`4.10:3`, `3.6:5`), a fact about
  the syntax rather than about a reachable state. -/
  | consBot {Γ Δ e es T Ts} :
      Typed P R Γ e T ⟨none, Δ⟩ → es.length = Ts.length →
      TypedArgs P R Γ (e :: es) (T :: Ts) ⟨none, Δ⟩

/-- (Match) §5.5's arm premises: one arm per variant, **each from the same
post-scrutinee state `Σ0`** (a `match` is a branch, not a sequence, so Σ is not
threaded from arm to arm) and each at the one type `T` the rule concludes at.

An arm that continues carries two premises of its own. Its body is typed under
the variant's payload locals (`armCtx`), and at its end those locals leave
scope under §5.6 — `NoResidualLinear` over the `ai` entries the arm pops is
the leak check `Typed.letIn` makes for its single binder, read over the whole
payload (`6.3:17`: a `Linear` payload an arm neither moves nor consumes is a
leak; an `Affine` one the machine drops once). The arm's contribution to the
join is what is left after popping them. An arm that diverges contributes `⊥`
to the join (`none`), and only its deliveries. The result is one optional
state per arm, in declaration order, and the arms' deliveries. -/
inductive TypedArms (P : Program) (R : Ty) : Ctx → List Expr → List (List Ty) → Ty →
    List (Option Ctx) → List Ctx → Prop where
  /-- No arms left to type, and so no state to contribute. -/
  | noArms {Γ₀ T} : TypedArms P R Γ₀ [] [] T [] []
  /-- The arm for the next variant, continuing: its body typed under that
  variant's payload locals, those locals discharged by §5.6 at the arm's end,
  and the rest of the arms typed from the same `Σ0`. -/
  | arm {Γ₀ Γb Δb os Δs e es Ts Tss T} :
      Typed P R (armCtx Ts Γ₀) e T ⟨some Γb, Δb⟩ →
      NoResidualLinear P.decls (Γb.take Ts.length) →
      TypedArms P R Γ₀ es Tss T os Δs →
      TypedArms P R Γ₀ (e :: es) (Ts :: Tss) T (some (Γb.drop Ts.length) :: os) (Δb ++ Δs)
  /-- The arm for the next variant, diverging: its body is `⊥`, so no scope
  exit is reached on a normal path and it contributes no state to the join
  (§5.5, `3.8:51`), only its deliveries. -/
  | armDiv {Γ₀ Δb os Δs e es Ts Tss T} :
      Typed P R (armCtx Ts Γ₀) e T ⟨none, Δb⟩ →
      TypedArms P R Γ₀ es Tss T os Δs →
      TypedArms P R Γ₀ (e :: es) (Ts :: Tss) T (none :: os) (Δb ++ Δs)
end

/-- **(Match) §5.5's premises for the arm a tag selects.** Read at the variant
index `k`: the arm's body is typed under that variant's payload locals, and
when it continues its locals are discharged by §5.6 at the arm's end and what
it contributes to the n-way join is one of the states the join was taken
over, and its deliveries are among the arms'. This is the inversion
`soundness` performs once (D-Match) §6.6 has read the tag (helper). -/
theorem TypedArms.at_index {P : Program} {R : Ty} {Γ₀ : Ctx} {T : Ty} :
    ∀ {arms : List Expr} {Tss : List (List Ty)} {os : List (Option Ctx)} {Δs : List Ctx},
      TypedArms P R Γ₀ arms Tss T os Δs →
      ∀ (k : Nat) {body : Expr} {Ts : List Ty}, arms[k]? = some body → Tss[k]? = some Ts →
        ∃ ob Δb, Typed P R (armCtx Ts Γ₀) body T ⟨ob, Δb⟩ ∧
          (∀ Γb, ob = some Γb →
            NoResidualLinear P.decls (Γb.take Ts.length) ∧ some (Γb.drop Ts.length) ∈ os) ∧
          Δb ⊆ Δs
  | _, _, _, _, .noArms, _, _, _, ha, _ => by simp at ha
  | _, _, _, _, .arm hbody hres _, 0, _, _, ha, ht => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at ha ht
      subst ha; subst ht
      refine ⟨_, _, hbody, fun Γb h => ?_, List.subset_append_left _ _⟩
      cases h
      exact ⟨hres, List.mem_cons_self⟩
  | _, _, _, _, .armDiv hbody _, 0, _, _, ha, ht => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at ha ht
      subst ha; subst ht
      exact ⟨_, _, hbody, fun Γb h => (by cases h), List.subset_append_left _ _⟩
  | _, _, _, _, .arm _ _ hrest, (k + 1), _, _, ha, ht => by
      simp only [List.getElem?_cons_succ] at ha ht
      obtain ⟨ob, Δb, h₁, h₂, h₃⟩ := TypedArms.at_index hrest k ha ht
      exact ⟨ob, Δb, h₁, fun Γb h => ⟨(h₂ Γb h).1, List.mem_cons_of_mem _ (h₂ Γb h).2⟩,
        h₃.trans (List.subset_append_right _ _)⟩
  | _, _, _, _, .armDiv _ hrest, (k + 1), _, _, ha, ht => by
      simp only [List.getElem?_cons_succ] at ha ht
      obtain ⟨ob, Δb, h₁, h₂, h₃⟩ := TypedArms.at_index hrest k ha ht
      exact ⟨ob, Δb, h₁, fun Γb h => ⟨(h₂ Γb h).1, List.mem_cons_of_mem _ (h₂ Γb h).2⟩,
        h₃.trans (List.subset_append_right _ _)⟩

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
where the frame's scopes end (§5.7's `⊥_exit`). A body with no normal exit,
`Ωf = ⊥;Δf`, owes nothing at one. `Δf` has no `⟨break, _⟩`: "a break outside
a loop is ill-formed" (§5.7), which is (Fn)'s own premise. -/
def WfFn (P : Program) (fd : FnDef) : Prop :=
  ∃ Ωf, Typed P fd.ret (fnCtx fd) fd.body fd.ret Ωf ∧
    (∀ Γf, Ωf.norm = some Γf → NoResidualLinear P.decls Γf) ∧ Ωf.brk = []

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

/-- One step of (Match) §5.5's fold, with the accumulator allowed to have failed
already: taking the next arm in is joining it into the accumulator (helper). -/
theorem Ctx.joinFold_bind_cons (D : Decls) (o : Option Ctx) (Γ : Ctx) (Γs : List Ctx) :
    (o.bind fun a => Ctx.joinFold D a (Γ :: Γs))
      = ((o.bind fun a => Ctx.join D a Γ).bind fun a => Ctx.joinFold D a Γs) := by
  cases o with
  | none => rfl
  | some a =>
      simp only [Option.bind_some, Ctx.joinFold]
      cases Ctx.join D a Γ with
      | none => rfl
      | some a' => rfl

/-- (Match) §5.5's fold over the remaining arms **does not depend on their
order**, whatever the accumulator: joining two arms in either order is
`Ctx.join_comm`, and moving one past the accumulator is `Ctx.join_assoc`. The
premise is the one the two lemmas need — one skeleton, every entry a shape of
its declared type — and `Ctx.join_skel`/`Ctx.join_wf` carry it to the next
accumulator. -/
theorem Ctx.joinFold_perm {D : Decls} (hD : WfStructs D) {sk : List (Ty × Bool)} :
    ∀ {Γs Γs' : List Ctx}, Γs.Perm Γs' →
      (∀ Γ ∈ Γs, Γ.skel = sk ∧ Ctx.Wf D Γ) →
      ∀ o : Option Ctx, (∀ Γ, o = some Γ → Γ.skel = sk ∧ Ctx.Wf D Γ) →
        (o.bind fun a => Ctx.joinFold D a Γs) = (o.bind fun a => Ctx.joinFold D a Γs') := by
  intro Γs Γs' hperm
  induction hperm with
  | nil => intro _ o _; rfl
  | cons Γ _ ih =>
      intro hinv o ho
      rw [Ctx.joinFold_bind_cons, Ctx.joinFold_bind_cons]
      refine ih (fun Δ hΔ => hinv Δ (List.mem_cons_of_mem _ hΔ)) _ ?_
      intro Δ hΔ
      cases o with
      | none => simp at hΔ
      | some a =>
          simp only [Option.bind_some] at hΔ
          obtain ⟨hsk, hwf⟩ := ho a rfl
          obtain ⟨hsk', hwf'⟩ := hinv Γ List.mem_cons_self
          exact ⟨(Ctx.join_skel hΔ).trans hsk,
                 Ctx.join_wf a Γ Δ (hsk.trans hsk'.symm) hwf hwf' hΔ⟩
  | swap x y l =>
      intro hinv o ho
      obtain ⟨hsky, hwfy⟩ := hinv y List.mem_cons_self
      obtain ⟨hskx, hwfx⟩ := hinv x (List.mem_cons_of_mem _ List.mem_cons_self)
      simp only [Ctx.joinFold_bind_cons]
      have key : ((o.bind fun a => Ctx.join D a y).bind fun a => Ctx.join D a x)
          = ((o.bind fun a => Ctx.join D a x).bind fun a => Ctx.join D a y) := by
        cases o with
        | none => rfl
        | some a =>
            obtain ⟨hska, hwfa⟩ := ho a rfl
            simp only [Option.bind_some]
            rw [Ctx.join_assoc hD a y x (hska.trans hsky.symm) (hsky.trans hskx.symm)
                  hwfa hwfy hwfx,
                Ctx.join_comm y x (hsky.trans hskx.symm),
                Ctx.join_assoc hD a x y (hska.trans hskx.symm) (hskx.trans hsky.symm)
                  hwfa hwfx hwfy]
      rw [key]
  | trans hp₁ _ ih₁ ih₂ =>
      intro hinv o ho
      rw [ih₁ hinv o ho, ih₂ (fun Δ hΔ => hinv Δ (hp₁.mem_iff.2 hΔ)) o ho]

/-- **(Match) §5.5's `join(Σ1, …, Σn)` is invariant under a permutation of the
arms**, over arms that share a skeleton and whose entries are shapes of their
declared types. §5.5 writes the n-way join with no order and no bracketing and
the mechanization computes it as a left fold; this is the theorem that says the
two readings agree, so `Ctx.joinAll` may be read as the calculus writes it. -/
theorem Ctx.joinAll_perm {D : Decls} (hD : WfStructs D) {sk : List (Ty × Bool)} :
    ∀ {Γs Γs' : List Ctx}, Γs.Perm Γs' → (∀ Γ ∈ Γs, Γ.skel = sk ∧ Ctx.Wf D Γ) →
      Ctx.joinAll D Γs = Ctx.joinAll D Γs' := by
  intro Γs Γs' hperm
  induction hperm with
  | nil => intro _; rfl
  | cons Γ hp _ =>
      intro hinv
      have h := Ctx.joinFold_perm hD hp (fun Δ hΔ => hinv Δ (List.mem_cons_of_mem _ hΔ)) (some Γ)
        (fun Δ hΔ => by cases hΔ; exact hinv Γ List.mem_cons_self)
      simpa [Ctx.joinAll] using h
  | swap x y l =>
      intro hinv
      obtain ⟨hsky, _⟩ := hinv y List.mem_cons_self
      obtain ⟨hskx, _⟩ := hinv x (List.mem_cons_of_mem _ List.mem_cons_self)
      show Ctx.joinFold D y (x :: l) = Ctx.joinFold D x (y :: l)
      simp only [Ctx.joinFold, Ctx.join_comm y x (hsky.trans hskx.symm)]
  | trans hp₁ _ ih₁ ih₂ =>
      intro hinv
      rw [ih₁ hinv, ih₂ (fun Δ hΔ => hinv Δ (hp₁.mem_iff.2 hΔ))]

/-- (Match) §5.5's fold keeps the invariant: joined into a well-formed
accumulator, a well-formed arm leaves a well-formed accumulator (helper). -/
theorem Ctx.joinFold_wf {D : Decls} {sk : List (Ty × Bool)} :
    ∀ (Γs : List Ctx) (acc Γ' : Ctx), (∀ Γ ∈ Γs, Γ.skel = sk ∧ Ctx.Wf D Γ) →
      acc.skel = sk → Ctx.Wf D acc → Ctx.joinFold D acc Γs = some Γ' → Ctx.Wf D Γ'
  | [], acc, Γ', _, _, hacc, h => by
      simp only [Ctx.joinFold, Option.some.injEq] at h
      subst h
      exact hacc
  | Γ :: Γs, acc, Γ', hinv, hsk, hacc, h => by
      simp only [Ctx.joinFold] at h
      cases hj : Ctx.join D acc Γ with
      | none => rw [hj] at h; exact absurd h (by simp)
      | some acc' =>
          rw [hj] at h
          obtain ⟨hskΓ, hwfΓ⟩ := hinv Γ List.mem_cons_self
          exact Ctx.joinFold_wf Γs acc' Γ'
            (fun Δ hΔ => hinv Δ (List.mem_cons_of_mem _ hΔ))
            ((Ctx.join_skel hj).trans hsk)
            (Ctx.join_wf acc Γ acc' (hsk.trans hskΓ.symm) hacc hwfΓ hj) h

/-- **(Match) §5.5's n-way join of well-formed arms is well formed**, so a joined
context may be joined again — which is what makes `Ctx.joinAll_perm`'s premise
composable across nested `match`es. -/
theorem Ctx.joinAll_wf {D : Decls} {sk : List (Ty × Bool)} {Γs : List Ctx} {Γ' : Ctx}
    (hinv : ∀ Γ ∈ Γs, Γ.skel = sk ∧ Ctx.Wf D Γ) (h : Ctx.joinAll D Γs = some Γ') :
    Ctx.Wf D Γ' := by
  cases Γs with
  | nil => simp [Ctx.joinAll] at h
  | cons Γ Γs =>
      obtain ⟨hsk, hwf⟩ := hinv Γ List.mem_cons_self
      exact Ctx.joinFold_wf Γs Γ Γ' (fun Δ hΔ => hinv Δ (List.mem_cons_of_mem _ hΔ)) hsk hwf h

/-- The two-arm §5.5 join over `Ω` preserves a skeleton both continuing arms
have (helper). -/
theorem Ctx.joinOpt_skel {D : Decls} {a b : Option Ctx} {Γ' : Ctx} {S : List (Ty × Bool)}
    (h : Ctx.joinOpt D a b = some (some Γ'))
    (ha : ∀ x, a = some x → x.skel = S) (hb : ∀ x, b = some x → x.skel = S) :
    Γ'.skel = S := by
  cases a with
  | none => simp only [Ctx.joinOpt, Option.some.injEq] at h; exact hb _ h
  | some x =>
    cases b with
    | none =>
        simp only [Ctx.joinOpt, Option.some.injEq] at h
        cases h; exact ha _ rfl
    | some y =>
        simp only [Ctx.joinOpt] at h
        cases hj : Ctx.join D x y with
        | none => rw [hj] at h; cases h
        | some z =>
            rw [hj] at h
            simp only [Option.map_some, Option.some.injEq] at h
            cases h
            exact (Ctx.join_skel hj).trans (ha _ rfl)

/-- The n-way §5.5 join over `Ω` preserves the skeleton every continuing arm
has (helper). -/
theorem Ctx.joinOpts_skel {D : Decls} {os : List (Option Ctx)} {Γ₀ Γ' : Ctx}
    (h : Ctx.joinOpts D os = some (some Γ')) (hs : Ctx.SameSkel Γ₀ (os.filterMap id)) :
    Γ'.skel = Γ₀.skel := by
  unfold Ctx.joinOpts at h
  revert hs
  cases hf : os.filterMap id with
  | nil => rw [hf] at h; simp at h
  | cons Γ₁ Γs =>
      rw [hf] at h
      intro hs
      cases hj : Ctx.joinFold D Γ₁ Γs with
      | none => simp [hj] at h
      | some z =>
          simp only [hj, Option.map_some, Option.some.injEq] at h
          cases h
          exact (Ctx.joinFold_skel Γs hj).trans hs.1

/-- A delivered context **extends** `Γ`: it is `Γ`'s skeleton with zero or
more bindings pushed on top. A `⟨break, Σ⟩` delivery records the whole
context in force at the `break` (`Typed.brk`), so between the loop that reads
it and the `break` that made it sit the bindings of every `let` and every
`match` arm the `break` is inside (helper). -/
def Ctx.Extends (Γb Γ : Ctx) : Prop := ∃ pre, Γb.skel = pre ++ Γ.skel

/-- A context extends itself (helper). -/
theorem Ctx.Extends.refl (Γ : Ctx) : Ctx.Extends Γ Γ := ⟨[], rfl⟩

/-- Extension is read against the skeleton only (helper). -/
theorem Ctx.Extends.skel {Γb Γ₁ Γ : Ctx} (h : Ctx.Extends Γb Γ₁) (hs : Γ₁.skel = Γ.skel) :
    Ctx.Extends Γb Γ := by
  obtain ⟨pre, hp⟩ := h
  exact ⟨pre, hp.trans (by rw [hs])⟩

/-- Extending a context with one more binding on top extends the context
under it: a delivery from a `let` body extends the `let`'s own context
(helper). -/
theorem Ctx.Extends.pop {Γb Γ : Ctx} {en : Entry} (h : Ctx.Extends Γb (en :: Γ)) :
    Ctx.Extends Γb Γ := by
  obtain ⟨pre, hp⟩ := h
  exact ⟨pre ++ [en.skel], by simpa [Ctx.skel] using hp⟩

/-- The same for a `match` arm's payload locals (helper). -/
theorem Ctx.Extends.armCtx {Γb Γ₀ : Ctx} {Ts : List Ty} (h : Ctx.Extends Γb (armCtx Ts Γ₀)) :
    Ctx.Extends Γb Γ₀ := by
  obtain ⟨pre, hp⟩ := h
  exact ⟨pre ++ (Ts.map fun T => (T, false)).reverse, by rw [hp, Ctx.skel_armCtx]; simp⟩

/-- An extension is at least as long as what it extends (helper). -/
theorem Ctx.Extends.length_le {Γb Γ : Ctx} (h : Ctx.Extends Γb Γ) : Γ.length ≤ Γb.length := by
  obtain ⟨pre, hp⟩ := h
  have := congrArg List.length hp
  simp [Ctx.skel] at this
  omega

/-- `outside_loop(Σ_x)` has the loop's own skeleton: popping the loop-local
bindings off a delivery that extends the head leaves the head's bindings
(helper). -/
theorem Ctx.outsideLoop_skel {Γh Γb : Ctx} (h : Ctx.Extends Γb Γh) :
    (Ctx.outsideLoop Γh Γb).skel = Γh.skel := by
  obtain ⟨pre, hp⟩ := h
  have hlen : Γb.length = pre.length + Γh.length := by
    have := congrArg List.length hp
    simpa [Ctx.skel] using this
  have hmap : (Ctx.outsideLoop Γh Γb).skel = Γb.skel.drop (Γb.length - Γh.length) := by
    simp [Ctx.outsideLoop, Ctx.skel, List.map_drop]
  rw [hmap, hp, show Γb.length - Γh.length = pre.length by omega, List.drop_left]

/-- The skeleton half of §5's convention that `Γ` is fixed while `Σ` is
threaded, read over `Ω`: a normal outgoing state, when there is one, has the
incoming skeleton, and every `⟨break, Σ⟩` delivery **extends** it — the
bindings in force at the edge, on top of the incoming ones. `⊥` has no state,
so it constrains only the deliveries (helper). -/
structure Out.SkelOk (Γ : Ctx) (Ω : Out) : Prop where
  /-- The normal outgoing state has the incoming skeleton. -/
  norm : ∀ Γ', Ω.norm = some Γ' → Γ'.skel = Γ.skel
  /-- Every delivered state extends it. -/
  brk : ∀ Γb ∈ Ω.brk, Ctx.Extends Γb Γ

/-- A `⊥` outcome whose deliveries extend the context preserves its skeleton
(helper). -/
theorem Out.skelOk_bot {Γ : Ctx} {Δ : List Ctx} (h : ∀ Γb ∈ Δ, Ctx.Extends Γb Γ) :
    Out.SkelOk Γ ⟨none, Δ⟩ :=
  ⟨fun _ h => (by cases h), h⟩

/-- An outcome that continues at the incoming context itself and delivers
nothing preserves its skeleton (helper). -/
theorem Out.skelOk_same {Γ : Ctx} : Out.SkelOk Γ ⟨some Γ, []⟩ :=
  ⟨fun _ h => by cases h; rfl, fun _ h => by cases h⟩

/-- An outcome that continues at a context of the incoming skeleton and
delivers nothing preserves it (helper). -/
theorem Out.skelOk_of {Γ Γ' : Ctx} (h : Γ'.skel = Γ.skel) : Out.SkelOk Γ ⟨some Γ', []⟩ :=
  ⟨fun _ hn => by cases hn; exact h, fun _ h => by cases h⟩

/-- §5.3's threading, read over skeletons: a prefix that continues at `Γ₁`
followed by a subexpression typed from `Γ₁` preserves the skeleton the
prefix started from, `Ω ⊕ Δ₁` included (helper). -/
theorem Out.SkelOk.then {Γ Γ₁ : Ctx} {Δ₁ : List Ctx} {Ω : Out}
    (h₁ : Out.SkelOk Γ ⟨some Γ₁, Δ₁⟩) (h₂ : Out.SkelOk Γ₁ Ω) : Out.SkelOk Γ (Ω.add Δ₁) := by
  have hs := h₁.norm Γ₁ rfl
  refine ⟨fun Γ' h => (h₂.norm Γ' h).trans hs, fun Γb hb => ?_⟩
  rcases List.mem_append.mp hb with hb | hb
  · exact (h₂.brk Γb hb).skel hs
  · exact h₁.brk Γb hb

/-- The loop-head state has the entry's skeleton: it is the entry itself or
its §5.5 join with the back-edge state (helper). -/
theorem LoopHead.skel {D : Decls} {Γ Γh : Ctx} {o : Option Ctx} (h : LoopHead D Γ o Γh) :
    Γh.skel = Γ.skel := by
  cases o with
  | none =>
      have h1 := h.1
      simp only [Ctx.joinOpt, Option.some.injEq] at h1
      cases h1; rfl
  | some Γe =>
      have h1 := h.1
      simp only [Ctx.joinOpt] at h1
      cases hj : Ctx.join D Γ Γe with
      | none => rw [hj] at h1; cases h1
      | some z =>
          rw [hj] at h1
          simp only [Option.map_some, Option.some.injEq] at h1
          cases h1
          exact Ctx.join_skel hj

/-- **Re-entering a loop at its head solves the head equation again** (§5.7):
if `Σ_h = head(Σ, e)` with the body typed at `Σ_h`, then `Σ_h = head(Σ_h, e)`
with the same body judgment. Without a back edge `Σ_h` is `Σ_h`'s own entry;
with one it is `join(Σ, Σ_e)`, and the join absorbs a second `Σ_e`
(`Ctx.join_absorb`). This is the lattice step of the back-edge proof: the
derivation that typed the loop at entry types it again at every later
iteration, so `soundness`'s fuel induction applies to the next turn. -/
theorem LoopHead.reenter {D : Decls} {Γ Γh : Ctx} {o : Option Ctx} (h : LoopHead D Γ o Γh)
    (ho : ∀ Γe, o = some Γe → Γe.skel = Γh.skel ∧ Ctx.Wf D Γe) : LoopHead D Γh o Γh := by
  cases o with
  | none => exact ⟨rfl, fun _ h => by cases h⟩
  | some Γe =>
      have h1 := h.1
      simp only [Ctx.joinOpt] at h1
      cases hj : Ctx.join D Γ Γe with
      | none => rw [hj] at h1; cases h1
      | some z =>
          rw [hj] at h1
          simp only [Option.map_some, Option.some.injEq] at h1
          subst h1
          obtain ⟨hsk, hw⟩ := ho Γe rfl
          have hsk' : Γ.skel = Γe.skel := (Ctx.join_skel hj).symm.trans hsk.symm
          have hj' := Ctx.join_absorb hsk' hw hj
          exact ⟨by simp [Ctx.joinOpt, hj'], h.2⟩

mutual
/-- Every rule preserves the context skeleton: only ownership states flow.
This is the fused context's image of §5's convention that `Γ` is fixed while
`Σ` is threaded through the judgment, read over `Ω` (`Out.SkelOk`): the
normal outgoing state has the incoming skeleton, and every `⟨break, Σ⟩`
delivery extends it. The three judgments are proved together, by recursion
on the derivation. -/
theorem Typed.skel_preserved {P R} : ∀ {Γ : Ctx} {e T} {Ω : Out},
    Typed P R Γ e T Ω → Out.SkelOk Γ Ω
  | _, _, _, _, .intLit _ => Out.skelOk_same
  | _, _, _, _, .boolLit => Out.skelOk_same
  | _, _, _, _, .unitLit => Out.skelOk_same
  | _, _, _, _, .floatLit _ => Out.skelOk_same
  | _, _, _, _, .useCopy _ _ _ _ _ _ => Out.skelOk_same
  | _, _, _, _, .dropCopy _ _ _ _ _ _ => Out.skelOk_same
  | _, _, _, _, .useMove hget _ _ _ _ _ _ _ => Out.skelOk_of (skel_set_setSt hget _)
  | _, _, _, _, .useDeclared hget _ _ _ _ _ _ _ => Out.skelOk_of (skel_set_setSt hget _)
  | _, _, _, _, .dropRes hget _ _ _ _ _ _ _ _ => Out.skelOk_of (skel_set_setSt hget _)
  | _, _, _, _, .dropDeclared hget _ _ _ _ _ _ _ => Out.skelOk_of (skel_set_setSt hget _)
  | _, _, _, _, .binop h₁ h₂ _ => (Typed.skel_preserved h₁).then (Typed.skel_preserved h₂)
  | _, _, _, _, .binopBot h₁ _ => Typed.skel_preserved h₁
  | _, _, _, _, .floatBinop h₁ h₂ _ => (Typed.skel_preserved h₁).then (Typed.skel_preserved h₂)
  | _, _, _, _, .floatBinopBot h₁ _ => Typed.skel_preserved h₁
  | _, _, _, _, .neg h => Typed.skel_preserved h
  | _, _, _, _, .floatNeg h => Typed.skel_preserved h
  | _, _, _, _, .notOp h => Typed.skel_preserved h
  | _, _, _, _, .bitnot h => Typed.skel_preserved h
  | _, _, _, _, .intCast h => Typed.skel_preserved h
  | _, _, _, _, .intToFloat h => Typed.skel_preserved h
  | _, _, _, _, .floatIntrin h _ => Typed.skel_preserved h
  | _, _, _, _, .panic => Out.skelOk_bot (by simp)
  | _, _, _, _, .dbg h _ => Typed.skel_preserved h
  | _, _, _, _, .mkStruct _ hta => TypedArgs.skel_preserved hta
  | _, _, _, _, .mkEnum _ _ hta => TypedArgs.skel_preserved hta
  | _, _, _, _, .mkArray hta => TypedArgs.skel_preserved hta
  | _, _, _, _, .repeatArray h _ => Typed.skel_preserved h
  | _, _, _, _, .«match» hscrut _ _ harms hjoin => by
      have ks := Typed.skel_preserved hscrut
      have ka := TypedArms.skel_all harms
      have hs := ks.norm _ rfl
      refine ⟨fun _ h => ?_, fun Γb hb => ?_⟩
      · cases h; exact (Ctx.joinOpts_skel hjoin ka.1).trans hs
      · rcases List.mem_append.mp hb with hb | hb
        · exact (ka.2 Γb hb).skel hs
        · exact ks.brk Γb hb
  | _, _, _, _, .matchBot h => Typed.skel_preserved h
  | _, _, _, _, .indexRead hta _ _ _ _ _ _ _ _ _ _ _ => TypedArgs.skel_preserved hta
  | _, _, _, _, .indexReadBot hta _ _ _ _ _ _ => TypedArgs.skel_preserved hta
  | _, _, _, _, .indexWrite _ _ _ _ _ _ _ h₁ hta _ hget₁ _ _ _ _ => by
      have k := (Typed.skel_preserved h₁).then (TypedArgs.skel_preserved hta)
      exact ⟨fun _ h => by cases h; exact (skel_set_setSt hget₁ _).trans (k.norm _ rfl), k.brk⟩
  | _, _, _, _, .indexWriteBotRhs h => Typed.skel_preserved h
  | _, _, _, _, .indexWriteBotIdx _ _ _ h₁ hta _ =>
      (Typed.skel_preserved h₁).then (TypedArgs.skel_preserved hta)
  | _, _, _, _, .indexDrop h => Typed.skel_preserved h
  | _, _, _, _, .letIn h₁ h₂ _ => by
      have k₁ := Typed.skel_preserved h₁
      have k₂ := Typed.skel_preserved h₂
      have hs := k₁.norm _ rfl
      refine ⟨fun _ h => ?_, fun Γb hb => ?_⟩
      · cases h
        have := k₂.norm _ rfl
        simp only [Ctx.skel, List.map_cons, List.cons.injEq] at this
        exact this.2.trans hs
      · rcases List.mem_append.mp hb with hb | hb
        · exact ((k₂.brk Γb hb).pop).skel hs
        · exact k₁.brk Γb hb
  | _, _, _, _, .letInDiv h₁ h₂ => by
      have k₁ := Typed.skel_preserved h₁
      have k₂ := Typed.skel_preserved h₂
      have hs := k₁.norm _ rfl
      refine Out.skelOk_bot fun Γb hb => ?_
      rcases List.mem_append.mp hb with hb | hb
      · exact ((k₂.brk Γb hb).pop).skel hs
      · exact k₁.brk Γb hb
  | _, _, _, _, .letBot h => Typed.skel_preserved h
  | _, _, _, _, .assign _ _ _ _ h hget₁ _ _ _ => by
      have k := Typed.skel_preserved h
      exact ⟨fun _ hn => by cases hn; exact (skel_set_setSt hget₁ _).trans (k.norm _ rfl), k.brk⟩
  | _, _, _, _, .assignBot h => Typed.skel_preserved h
  | _, _, _, _, .seq h₁ _ h₂ => (Typed.skel_preserved h₁).then (Typed.skel_preserved h₂)
  | _, _, _, _, .seqBot h => Typed.skel_preserved h
  | _, _, _, _, .ite hc h₁ h₂ hjoin => by
      have kc := Typed.skel_preserved hc
      have k₁ := Typed.skel_preserved h₁
      have k₂ := Typed.skel_preserved h₂
      have hs := kc.norm _ rfl
      refine ⟨fun _ h => ?_, fun Γb hb => ?_⟩
      · cases h
        exact Ctx.joinOpt_skel hjoin (fun x hx => (k₁.norm x hx).trans hs)
          (fun x hx => (k₂.norm x hx).trans hs)
      · simp only [List.mem_append] at hb
        rcases hb with (hb | hb) | hb
        · exact (k₁.brk Γb hb).skel hs
        · exact (k₂.brk Γb hb).skel hs
        · exact kc.brk Γb hb
  | _, _, _, _, .iteBot h => Typed.skel_preserved h
  | _, _, _, _, .call _ hta => TypedArgs.skel_preserved hta
  | _, _, _, _, .ret h _ => Out.skelOk_bot (Typed.skel_preserved h).brk
  | _, _, _, _, .retBot h => Typed.skel_preserved h
  | _, _, _, _, .brk => Out.skelOk_bot fun Γb hb => by
      simp only [List.mem_singleton] at hb
      subst hb
      exact Ctx.Extends.refl _
  | _, _, _, _, .loopDiv _ _ _ _ => Out.skelOk_bot (by simp)
  | _, _, _, _, .loopBreakDiv _ _ _ _ _ => Out.skelOk_bot (by simp)
  | _, _, _, _, .loopBreak hbody hhead _ _ hjoin => by
      have kb := Typed.skel_preserved hbody
      refine ⟨fun _ h => ?_, fun _ h => by cases h⟩
      cases h
      obtain ⟨Γ₁, Γrest, hΓs, hsk⟩ := Ctx.joinAll_skel hjoin
      have hmem : Γ₁ ∈ _ := hΓs ▸ List.mem_cons_self
      obtain ⟨Γb, hb, rfl⟩ := List.mem_map.mp hmem
      exact hsk.trans ((Ctx.outsideLoop_skel (kb.brk Γb hb)).trans hhead.skel)

/-- A typed expression list preserves the context skeleton too (helper). -/
theorem TypedArgs.skel_preserved {P R} : ∀ {Γ : Ctx} {es Ts} {Ω : Out},
    TypedArgs P R Γ es Ts Ω → Out.SkelOk Γ Ω
  | _, _, _, _, .nil => Out.skelOk_same
  | _, _, _, _, .cons h hs => (Typed.skel_preserved h).then (TypedArgs.skel_preserved hs)
  | _, _, _, _, .consBot h _ => Typed.skel_preserved h

/-- **Every continuing arm of a `match` hands the §5.5 join a context with the
skeleton the arm started from**, and every delivery an arm makes extends it:
the arm's payload locals are popped on its normal path, and sit on top of the
arm's context at a `break` inside it (helper). -/
theorem TypedArms.skel_all {P R} : ∀ {Γ₀ : Ctx} {arms Tss T} {os : List (Option Ctx)}
    {Δs : List Ctx}, TypedArms P R Γ₀ arms Tss T os Δs →
    Ctx.SameSkel Γ₀ (os.filterMap id) ∧ ∀ Γb ∈ Δs, Ctx.Extends Γb Γ₀
  | _, _, _, _, _, _, .noArms => ⟨trivial, by simp⟩
  | _, _, _, _, _, _, .arm hbody _ hrest => by
      have kb := Typed.skel_preserved hbody
      have kr := TypedArms.skel_all hrest
      refine ⟨⟨skel_drop_armCtx (kb.norm _ rfl), kr.1⟩, fun Γb hb => ?_⟩
      rcases List.mem_append.mp hb with hb | hb
      · exact (kb.brk Γb hb).armCtx
      · exact kr.2 Γb hb
  | _, _, _, _, _, _, .armDiv hbody hrest => by
      have kb := Typed.skel_preserved hbody
      have kr := TypedArms.skel_all hrest
      refine ⟨kr.1, fun Γb hb => ?_⟩
      rcases List.mem_append.mp hb with hb | hb
      · exact (kb.brk Γb hb).armCtx
      · exact kr.2 Γb hb
end

/-- **Every continuing arm of a `match` hands the §5.5 join a context with the
skeleton the arm started from** (§5's convention that `Γ` is fixed): the arm's
payload locals are popped, and the body preserved the rest. This is what lets
the n-way join read either the accumulated state or an arm's, which is the
`match` case of `soundness` (`Soundness.lean`) (helper). -/
theorem TypedArms.arm_skel {P R} {Γ₀ : Ctx} {arms Tss T} {os : List (Option Ctx)} {Δs : List Ctx}
    (h : TypedArms P R Γ₀ arms Tss T os Δs) : Ctx.SameSkel Γ₀ (os.filterMap id) :=
  (TypedArms.skel_all h).1


/-- The skeleton of a continuing outcome, read off a derivation (helper). -/
theorem Typed.skel_of {P R} {Γ Γ' : Ctx} {e T} {Δ : List Ctx}
    (h : Typed P R Γ e T ⟨some Γ', Δ⟩) : Γ'.skel = Γ.skel :=
  h.skel_preserved.norm Γ' rfl

/-- The same, for an expression list (helper). -/
theorem TypedArgs.skel_of {P R} {Γ Γ' : Ctx} {es Ts} {Δ : List Ctx}
    (h : TypedArgs P R Γ es Ts ⟨some Γ', Δ⟩) : Γ'.skel = Γ.skel :=
  h.skel_preserved.norm Γ' rfl

/-- Two contexts with one skeleton agree on every entry's type and mark
(helper). -/
theorem skel_lookup {Γ Γ' : Ctx} (h : Ctx.skel Γ' = Ctx.skel Γ) {i : Nat} {en en'}
    (h1 : Γ[i]? = some en) (h2 : Γ'[i]? = some en') :
    en'.ty = en.ty ∧ en'.mu = en.mu := by
  have hm : (Ctx.skel Γ')[i]? = (Ctx.skel Γ)[i]? := by rw [h]
  simp only [Ctx.skel, List.getElem?_map, h1, h2, Option.map_some,
    Option.some_inj] at hm
  exact ⟨congrArg Prod.fst hm, congrArg Prod.snd hm⟩

/-! ### The shape invariant, judgment-wide (RUE-2340)

`Ctx.Wf` — every entry's ownership state a shape of its declared type — is
the premise §5.5's associativity (`OwnSt.join_assoc`, `Ctx.joinAll_perm`)
carries. With §5.7's `⊥` an arbitrary context of the incoming skeleton, as it
was before the judgment carried `Ω`, a judgment-wide preservation theorem was
false: a `return` arm could feed the join a state no rule writes. §5.3's `Ω`
gives `⊥` no state at all, so every normal outgoing state is one a rule
wrote, and `Typed.wf` below proves the invariant is preserved. The premise is
then discharged once, for every derivation from a well-formed context. -/

/-- (helper) Every state `fnCtx`/`armCtx`/`let` push is `Owned`, a shape of
every type. -/
theorem Entry.wf_owned (D : Decls) (T : Ty) (m : Bool) :
    Entry.wf D { ty := T, mu := m, st := .owned } = true := by
  simp [Entry.wf, OwnSt.wf]

/-- (helper) A `let` binder enters `Owned`, so pushing it keeps a frame
well-formed. -/
theorem Ctx.Wf.cons_owned {D : Decls} {Γ : Ctx} (h : Ctx.Wf D Γ) (T : Ty) (m : Bool) :
    Ctx.Wf D ({ ty := T, mu := m, st := .owned } :: Γ) := by
  intro en hen
  rcases List.mem_cons.mp hen with rfl | hm
  · exact Entry.wf_owned D T m
  · exact h en hm

/-- (helper) Re-marking one entry at a path of its type with a state that is
a shape of that path's type keeps a frame well-formed — (Use-Move),
(Use-Declared-Linear-Destructure), (@Drop) and (Assign) all write this way. -/
theorem Ctx.Wf.set_setAt {D : Decls} {Γ : Ctx} {i : Nat} {en : Entry} {π : List Nat}
    {u : OwnSt} {T' : Ty} (hΓ : Ctx.Wf D Γ) (hget : Γ[i]? = some en)
    (hty : en.ty.atPath D π = some T') (hu : OwnSt.wf D u T' = true) :
    Ctx.Wf D (Γ.set i (en.setSt (en.st.setAt π u))) := by
  intro en' hmem
  rcases List.mem_or_eq_of_mem_set hmem with hm | rfl
  · exact hΓ en' hm
  · have hen : Entry.wf D en = true := hΓ en (List.mem_of_getElem? hget)
    exact OwnSt.setAt_wf T' u hu π en.st en.ty hen hty

/-- (helper) A `match` arm's entry context is well-formed when `Σ0` is. -/
theorem Ctx.Wf.armCtx {D : Decls} {Γ₀ : Ctx} (Ts : List Ty) (h : Ctx.Wf D Γ₀) :
    Ctx.Wf D (armCtx Ts Γ₀) := by
  intro en hmem
  unfold RueCore.armCtx at hmem
  simp only [List.mem_append, List.mem_reverse, List.mem_map] at hmem
  rcases hmem with ⟨T, _, rfl⟩ | hm
  · exact Entry.wf_owned D T false
  · exact h en hm

/-- (helper) The two-arm join over `Ω` keeps the invariant. -/
theorem Ctx.joinOpt_wf {D : Decls} {a b : Option Ctx} {Γ' : Ctx} {S : List (Ty × Bool)}
    (h : Ctx.joinOpt D a b = some (some Γ'))
    (ha : ∀ x, a = some x → x.skel = S ∧ Ctx.Wf D x)
    (hb : ∀ x, b = some x → x.skel = S ∧ Ctx.Wf D x) : Ctx.Wf D Γ' := by
  cases a with
  | none => simp only [Ctx.joinOpt, Option.some.injEq] at h; exact (hb _ h).2
  | some x =>
    cases b with
    | none =>
        simp only [Ctx.joinOpt, Option.some.injEq] at h
        cases h; exact (ha _ rfl).2
    | some y =>
        simp only [Ctx.joinOpt] at h
        cases hj : Ctx.join D x y with
        | none => rw [hj] at h; cases h
        | some z =>
            rw [hj] at h
            simp only [Option.map_some, Option.some.injEq] at h
            cases h
            exact Ctx.join_wf x y _ ((ha _ rfl).1.trans (hb _ rfl).1.symm) (ha _ rfl).2
              (hb _ rfl).2 hj

/-- (helper) The n-way join over `Ω` keeps the invariant. -/
theorem Ctx.joinOpts_wf {D : Decls} {os : List (Option Ctx)} {Γ' : Ctx} {S : List (Ty × Bool)}
    (h : Ctx.joinOpts D os = some (some Γ'))
    (hinv : ∀ Γ ∈ os.filterMap id, Γ.skel = S ∧ Ctx.Wf D Γ) : Ctx.Wf D Γ' := by
  unfold Ctx.joinOpts at h
  revert hinv
  cases hf : os.filterMap id with
  | nil => rw [hf] at h; simp at h
  | cons Γ₁ Γs =>
      rw [hf] at h
      intro hinv
      cases hj : Ctx.joinFold D Γ₁ Γs with
      | none => simp [hj] at h
      | some z =>
          simp only [hj, Option.map_some, Option.some.injEq] at h
          cases h
          obtain ⟨hsk, hw⟩ := hinv Γ₁ List.mem_cons_self
          exact Ctx.joinFold_wf Γs Γ₁ _ (fun Γ hΓ => hinv Γ (List.mem_cons_of_mem _ hΓ)) hsk hw hj

/-- (helper) Every state an outcome carries — its normal outgoing state and
every delivered one — is a shape of its declared types. -/
structure Out.Wf (D : Decls) (Ω : Out) : Prop where
  /-- The normal outgoing state. -/
  norm : ∀ Γ', Ω.norm = some Γ' → Ctx.Wf D Γ'
  /-- Every `⟨break, Σ⟩` delivery. -/
  brk : ∀ Γb ∈ Ω.brk, Ctx.Wf D Γb

/-- (helper) `Typed.wf`'s statement for one judgment: a well-formed incoming
context gives a well-formed normal outgoing state and well-formed deliveries. -/
def Out.WfPres (D : Decls) (Γ : Ctx) (Ω : Out) : Prop :=
  Ctx.Wf D Γ → Out.Wf D Ω

/-- (helper) The same for a `match`'s arms: every continuing arm's state and
every arm's deliveries. -/
def Out.WfArms (D : Decls) (Γ₀ : Ctx) (os : List (Option Ctx)) (Δs : List Ctx) : Prop :=
  Ctx.Wf D Γ₀ → (∀ Γ ∈ os.filterMap id, Ctx.Wf D Γ) ∧ ∀ Γb ∈ Δs, Ctx.Wf D Γb

/-- (helper) A `⊥` outcome is well formed when its deliveries are. -/
theorem Out.Wf.bot {D : Decls} {Δ : List Ctx} (h : ∀ Γb ∈ Δ, Ctx.Wf D Γb) :
    Out.Wf D ⟨none, Δ⟩ := ⟨fun _ h => (by cases h), h⟩

/-- (helper) An outcome that continues at a well-formed state and delivers
nothing is well formed. -/
theorem Out.Wf.of {D : Decls} {Γ : Ctx} (h : Ctx.Wf D Γ) : Out.Wf D ⟨some Γ, []⟩ :=
  ⟨fun _ hn => by cases hn; exact h, fun _ h => by cases h⟩

/-- (helper) §5.3's threading, read over the shape invariant. -/
theorem Out.Wf.then {D : Decls} {Γ₁ : Ctx} {Δ₁ : List Ctx} {Ω : Out}
    (h₁ : Out.Wf D ⟨some Γ₁, Δ₁⟩) (h₂ : Ctx.Wf D Γ₁ → Out.Wf D Ω) : Out.Wf D (Ω.add Δ₁) := by
  have k₂ := h₂ (h₁.norm _ rfl)
  refine ⟨k₂.norm, fun Γb hb => ?_⟩
  rcases List.mem_append.mp hb with hb | hb
  · exact k₂.brk Γb hb
  · exact h₁.brk Γb hb

/-- (helper) The loop-head state is well formed when the entry is: it is the
entry itself, or a head `LoopHead` asks to be well formed. -/
theorem LoopHead.wf {D : Decls} {Γ Γh : Ctx} {o : Option Ctx} (h : LoopHead D Γ o Γh)
    (hw : Ctx.Wf D Γ) : Ctx.Wf D Γh := by
  cases o with
  | none =>
      have h1 := h.1
      simp only [Ctx.joinOpt, Option.some.injEq] at h1
      cases h1; exact hw
  | some Γe => exact h.2 Γe rfl

/-- (helper) Dropping bindings off the top keeps a frame well formed. -/
theorem Ctx.Wf.drop {D : Decls} {Γ : Ctx} (h : Ctx.Wf D Γ) (n : Nat) : Ctx.Wf D (Γ.drop n) :=
  fun en hen => h en (List.mem_of_mem_drop hen)

mutual
/-- **The shape invariant is preserved judgment-wide** (RUE-2340): from a
well-formed incoming context, every normal outgoing state a derivation
concludes at, and every state it delivers to a loop, is well-formed. With
`fnCtx_wf` it discharges `Ctx.Wf`, the premise §5.5's associativity carries
(`Ctx.joinAll_perm`), at every normal outgoing state of a function body
(`Typed.wf_fnCtx`). The recursion carries the invariant into every arm of
every `match` and `if` and into every loop body it passes through, which is
where associativity is read; what is stated as a theorem is that
outgoing-state form, not a separate corollary per join. It holds because
§5.3's `Ω` gives §5.7's `⊥` no state: before the judgment carried `Ω`,
`return` and `@panic` concluded at an arbitrary context and the statement was
false. A loop body is typed at the loop-head state, which `LoopHead` asks to
be well formed when a back edge produced it (`LoopHead.wf`); (Loop-Break)'s
exit state is the join of the deliveries' `outside_loop` parts, each a
suffix of a well-formed delivery. -/
theorem Typed.wf {P R} : ∀ {Γ : Ctx} {e T} {Ω : Out},
    Typed P R Γ e T Ω → Out.WfPres P.decls Γ Ω
  | _, _, _, _, .intLit _ => Out.Wf.of
  | _, _, _, _, .boolLit => Out.Wf.of
  | _, _, _, _, .unitLit => Out.Wf.of
  | _, _, _, _, .floatLit _ => Out.Wf.of
  | _, _, _, _, .useCopy _ _ _ _ _ _ => Out.Wf.of
  | _, _, _, _, .dropCopy _ _ _ _ _ _ => Out.Wf.of
  | _, _, _, _, .useMove hget _ _ hty _ _ _ _ => fun hw =>
      Out.Wf.of (hw.set_setAt hget hty (by simp [OwnSt.wf]))
  | _, _, _, _, .useDeclared hget _ _ _ htd _ _ _ => fun hw =>
      Out.Wf.of (hw.set_setAt hget htd (by simp [OwnSt.wf]))
  | _, _, _, _, .dropRes hget _ _ hty _ _ _ _ _ => fun hw =>
      Out.Wf.of (hw.set_setAt hget hty (by simp [OwnSt.wf]))
  | _, _, _, _, .dropDeclared hget _ _ _ htd _ _ _ => fun hw =>
      Out.Wf.of (hw.set_setAt hget htd (by simp [OwnSt.wf]))
  | _, _, _, _, .binop h₁ h₂ _ => fun hw => (Typed.wf h₁ hw).then (Typed.wf h₂)
  | _, _, _, _, .floatBinop h₁ h₂ _ => fun hw => (Typed.wf h₁ hw).then (Typed.wf h₂)
  | _, _, _, _, .binopBot h₁ _ => Typed.wf h₁
  | _, _, _, _, .floatBinopBot h₁ _ => Typed.wf h₁
  | _, _, _, _, .neg h => Typed.wf h
  | _, _, _, _, .floatNeg h => Typed.wf h
  | _, _, _, _, .notOp h => Typed.wf h
  | _, _, _, _, .bitnot h => Typed.wf h
  | _, _, _, _, .intCast h => Typed.wf h
  | _, _, _, _, .intToFloat h => Typed.wf h
  | _, _, _, _, .floatIntrin h _ => Typed.wf h
  | _, _, _, _, .panic => fun _ => Out.Wf.bot (by simp)
  | _, _, _, _, .dbg h _ => Typed.wf h
  | _, _, _, _, .mkStruct _ hta => TypedArgs.wf hta
  | _, _, _, _, .mkEnum _ _ hta => TypedArgs.wf hta
  | _, _, _, _, .mkArray hta => TypedArgs.wf hta
  | _, _, _, _, .repeatArray h _ => Typed.wf h
  | _, _, _, _, .indexRead hta _ _ _ _ _ _ _ _ _ _ _ => TypedArgs.wf hta
  | _, _, _, _, .indexReadBot hta _ _ _ _ _ _ => TypedArgs.wf hta
  | _, _, _, _, .indexDrop h => Typed.wf h
  | _, _, _, _, .indexWrite hget₀ _ _ hty₀ _ _ _ h₁ hta _ hget₁ _ _ _ _ => fun hw => by
      have k := (Typed.wf h₁ hw).then (TypedArgs.wf hta)
      have hsk : Ctx.skel _ = Ctx.skel _ := hta.skel_of.trans h₁.skel_of
      have hty₁ := (skel_lookup hsk hget₀ hget₁).1 ▸ hty₀
      exact ⟨fun _ h => by
        cases h; exact (k.norm _ rfl).set_setAt hget₁ hty₁ (by simp [OwnSt.wf]), k.brk⟩
  | _, _, _, _, .indexWriteBotRhs h => Typed.wf h
  | _, _, _, _, .indexWriteBotIdx _ _ _ h₁ hta _ => fun hw => (Typed.wf h₁ hw).then (TypedArgs.wf hta)
  | _, _, _, _, .«match» hscrut _ _ harms hjoin => fun hw => by
      have ks := Typed.wf hscrut hw
      have ka := TypedArms.wf harms (ks.norm _ rfl)
      refine ⟨fun _ h => ?_, fun Γb hb => ?_⟩
      · cases h
        exact Ctx.joinOpts_wf hjoin fun Γ hΓ =>
          ⟨Ctx.SameSkel.mem harms.arm_skel Γ hΓ, ka.1 Γ hΓ⟩
      · rcases List.mem_append.mp hb with hb | hb
        · exact ka.2 Γb hb
        · exact ks.brk Γb hb
  | _, _, _, _, .matchBot h => Typed.wf h
  | _, _, _, _, .letIn h₁ h₂ _ => fun hw => by
      have k₁ := Typed.wf h₁ hw
      have k₂ := Typed.wf h₂ ((k₁.norm _ rfl).cons_owned _ _)
      refine ⟨fun _ h => ?_, fun Γb hb => ?_⟩
      · cases h
        intro en hen
        exact k₂.norm _ rfl en (List.mem_cons_of_mem _ hen)
      · rcases List.mem_append.mp hb with hb | hb
        · exact k₂.brk Γb hb
        · exact k₁.brk Γb hb
  | _, _, _, _, .letInDiv h₁ h₂ => fun hw => by
      have k₁ := Typed.wf h₁ hw
      have k₂ := Typed.wf h₂ ((k₁.norm _ rfl).cons_owned _ _)
      refine Out.Wf.bot fun Γb hb => ?_
      rcases List.mem_append.mp hb with hb | hb
      · exact k₂.brk Γb hb
      · exact k₁.brk Γb hb
  | _, _, _, _, .letBot h => Typed.wf h
  | _, _, _, _, .assign hget₀ _ _ hty₀ h₁ hget₁ _ _ _ => fun hw => by
      have k := Typed.wf h₁ hw
      have hty₁ := (skel_lookup h₁.skel_of hget₀ hget₁).1 ▸ hty₀
      exact ⟨fun _ h => by
        cases h; exact (k.norm _ rfl).set_setAt hget₁ hty₁ (by simp [OwnSt.wf]), k.brk⟩
  | _, _, _, _, .assignBot h => Typed.wf h
  | _, _, _, _, .seq h₁ _ h₂ => fun hw => (Typed.wf h₁ hw).then (Typed.wf h₂)
  | _, _, _, _, .seqBot h => Typed.wf h
  | _, _, _, _, .ite hc h₁ h₂ hjoin => fun hw => by
      have kc := Typed.wf hc hw
      have hw₀ := kc.norm _ rfl
      have k₁ := Typed.wf h₁ hw₀
      have k₂ := Typed.wf h₂ hw₀
      refine ⟨fun _ h => ?_, fun Γb hb => ?_⟩
      · cases h
        exact Ctx.joinOpt_wf hjoin (fun x hx => ⟨h₁.skel_preserved.norm x hx, k₁.norm x hx⟩)
          (fun x hx => ⟨h₂.skel_preserved.norm x hx, k₂.norm x hx⟩)
      · simp only [List.mem_append] at hb
        rcases hb with (hb | hb) | hb
        · exact k₁.brk Γb hb
        · exact k₂.brk Γb hb
        · exact kc.brk Γb hb
  | _, _, _, _, .iteBot h => Typed.wf h
  | _, _, _, _, .call _ hta => TypedArgs.wf hta
  | _, _, _, _, .ret h _ => fun hw => Out.Wf.bot (Typed.wf h hw).brk
  | _, _, _, _, .retBot h => Typed.wf h
  | _, _, _, _, .brk => fun hw => Out.Wf.bot fun Γb hb => by
      simp only [List.mem_singleton] at hb
      subst hb; exact hw
  | _, _, _, _, .loopDiv _ _ _ _ => fun _ => Out.Wf.bot (by simp)
  | _, _, _, _, .loopBreakDiv _ _ _ _ _ => fun _ => Out.Wf.bot (by simp)
  | _, _, _, _, .loopBreak (Γh := Γh) hbody hhead _ _ hjoin => fun hw => by
      have kb := Typed.wf hbody (hhead.wf hw)
      have ks := Typed.skel_preserved hbody
      refine ⟨fun _ h => ?_, fun _ h => by cases h⟩
      cases h
      refine Ctx.joinAll_wf (sk := Γh.skel) (fun Γo hΓo => ?_) hjoin
      obtain ⟨Γb, hb, rfl⟩ := List.mem_map.mp hΓo
      exact ⟨Ctx.outsideLoop_skel (ks.brk Γb hb), (kb.brk Γb hb).drop _⟩

/-- (helper) The same, for an expression list. -/
theorem TypedArgs.wf {P R} : ∀ {Γ : Ctx} {es Ts} {Ω : Out},
    TypedArgs P R Γ es Ts Ω → Out.WfPres P.decls Γ Ω
  | _, _, _, _, .nil => Out.Wf.of
  | _, _, _, _, .cons h hs => fun hw => (Typed.wf h hw).then (TypedArgs.wf hs)
  | _, _, _, _, .consBot h _ => Typed.wf h

/-- (helper) The same, for a `match`'s arms. -/
theorem TypedArms.wf {P R} : ∀ {Γ₀ : Ctx} {arms Tss T} {os : List (Option Ctx)}
    {Δs : List Ctx}, TypedArms P R Γ₀ arms Tss T os Δs → Out.WfArms P.decls Γ₀ os Δs
  | _, _, _, _, _, _, .noArms => fun _ => ⟨by simp, by simp⟩
  | _, _, _, _, _, _, .arm hbody _ hrest => fun hw => by
      have kb := Typed.wf hbody (hw.armCtx _)
      have kr := TypedArms.wf hrest hw
      refine ⟨fun Γ hΓ => ?_, fun Γb hb => ?_⟩
      · simp only [List.filterMap_cons, id, List.mem_cons] at hΓ
        rcases hΓ with rfl | hΓ
        · exact (kb.norm _ rfl).drop _
        · exact kr.1 Γ hΓ
      · rcases List.mem_append.mp hb with hb | hb
        · exact kb.brk Γb hb
        · exact kr.2 Γb hb
  | _, _, _, _, _, _, .armDiv hbody hrest => fun hw => by
      have kb := Typed.wf hbody (hw.armCtx _)
      have kr := TypedArms.wf hrest hw
      refine ⟨kr.1, fun Γb hb => ?_⟩
      rcases List.mem_append.mp hb with hb | hb
      · exact kb.brk Γb hb
      · exact kr.2 Γb hb
end

/-- `LoopHead.reenter` for the loop rules' own premises: the body judgment
typed at the head gives the back-edge state the head's skeleton
(`Typed.skel_preserved`) and makes it well formed from the well-formed head
(`Typed.wf`) (helper). -/
theorem LoopHead.reenter_body {P R} {Γ Γh : Ctx} {e : Expr} {Ωe : Out}
    (h : LoopHead P.decls Γ Ωe.norm Γh) (hbody : Typed P R Γh e .unit Ωe) :
    LoopHead P.decls Γh Ωe.norm Γh :=
  h.reenter fun Γe hn =>
    ⟨hbody.skel_preserved.norm Γe hn, (hbody.wf (h.2 Γe hn)).norm Γe hn⟩

/-! ### No `break`, no delivery

§5.7's (Loop-Div) rules are selected by a *syntactic* premise — the body
contains no `break` targeting the loop — while the loop's outgoing `Δ_out`
removes the deliveries the body made. The two agree: a derivation of a body
with no such `break` makes no `⟨break, _⟩` delivery at all, because the only
rule that makes one is (Break) and every loop consumes its own. So the
`break`-less loop rules deliver nothing without a premise saying so, and
`soundness` uses this to rule out a `break` escaping one. -/

mutual
/-- **A body with no `break` targeting its loop delivers none** (§5.7): every
delivery in `Ω.brk` comes from a `break` the syntax has, outside any nested
loop (`Expr.breaks`). -/
theorem Typed.brk_nil {P R} : ∀ {Γ : Ctx} {e T} {Ω : Out},
    Typed P R Γ e T Ω → e.breaks = false → Ω.brk = []
  | _, _, _, _, .intLit _, _ | _, _, _, _, .boolLit, _ | _, _, _, _, .unitLit, _
  | _, _, _, _, .floatLit _, _ | _, _, _, _, .useCopy _ _ _ _ _ _, _
  | _, _, _, _, .dropCopy _ _ _ _ _ _, _ | _, _, _, _, .useMove _ _ _ _ _ _ _ _, _
  | _, _, _, _, .useDeclared _ _ _ _ _ _ _ _, _ | _, _, _, _, .dropRes _ _ _ _ _ _ _ _ _, _
  | _, _, _, _, .dropDeclared _ _ _ _ _ _ _ _, _ | _, _, _, _, .panic, _
  | _, _, _, _, .loopDiv _ _ _ _, _ | _, _, _, _, .loopBreakDiv _ _ _ _ _, _
  | _, _, _, _, .loopBreak _ _ _ _ _, _ => rfl
  | _, _, _, _, .brk, hb => by simp [Expr.breaks] at hb
  | _, _, _, _, .binop h₁ h₂ _, hb | _, _, _, _, .floatBinop h₁ h₂ _, hb
  | _, _, _, _, .seq h₁ _ h₂, hb => by
      simp only [Expr.breaks, Bool.or_eq_false_iff] at hb
      simp [Out.add, Typed.brk_nil h₁ hb.1, Typed.brk_nil h₂ hb.2]
  | _, _, _, _, .binopBot h₁ _, hb | _, _, _, _, .floatBinopBot h₁ _, hb
  | _, _, _, _, .seqBot h₁, hb | _, _, _, _, .letBot h₁, hb => by
      simp only [Expr.breaks, Bool.or_eq_false_iff] at hb
      exact Typed.brk_nil h₁ hb.1
  | _, _, _, _, .neg h, hb | _, _, _, _, .floatNeg h, hb | _, _, _, _, .notOp h, hb
  | _, _, _, _, .bitnot h, hb | _, _, _, _, .intCast h, hb | _, _, _, _, .intToFloat h, hb
  | _, _, _, _, .floatIntrin h _, hb | _, _, _, _, .dbg h _, hb
  | _, _, _, _, .repeatArray h _, hb | _, _, _, _, .assignBot h, hb
  | _, _, _, _, .retBot h, hb => by
      simp only [Expr.breaks] at hb
      exact Typed.brk_nil h hb
  | _, _, _, _, .indexWriteBotRhs h, hb => by
      simp only [Expr.breaks, Bool.or_eq_false_iff] at hb
      exact Typed.brk_nil h hb.1
  | _, _, _, _, .ret h _, hb | _, _, _, _, .assign _ _ _ _ h _ _ _ _, hb => by
      simp only [Expr.breaks] at hb
      have h' := Typed.brk_nil h hb
      simpa using h'
  | _, _, _, _, .indexDrop h, hb => by
      simp only [Expr.breaks] at hb
      exact Typed.brk_nil h (by simpa [Expr.breaks] using hb)
  | _, _, _, _, .mkStruct _ hta, hb | _, _, _, _, .mkEnum _ _ hta, hb
  | _, _, _, _, .mkArray hta, hb | _, _, _, _, .call _ hta, hb
  | _, _, _, _, .indexRead hta _ _ _ _ _ _ _ _ _ _ _, hb
  | _, _, _, _, .indexReadBot hta _ _ _ _ _ _, hb => by
      simp only [Expr.breaks] at hb
      exact TypedArgs.brk_nil hta hb
  | _, _, _, _, .indexWrite _ _ _ _ _ _ _ h₁ hta _ _ _ _ _ _, hb
  | _, _, _, _, .indexWriteBotIdx _ _ _ h₁ hta _, hb => by
      simp only [Expr.breaks, Bool.or_eq_false_iff] at hb
      have h₁' := Typed.brk_nil h₁ hb.1
      have h₂' := TypedArgs.brk_nil hta hb.2
      simp only at h₁' h₂'
      simp [h₁', h₂']
  | _, _, _, _, .«match» hscrut _ _ harms _, hb => by
      simp only [Expr.breaks, Bool.or_eq_false_iff] at hb
      have h₁' := Typed.brk_nil hscrut hb.1
      have h₂' := TypedArms.brk_nil harms hb.2
      simp only at h₁'
      simp [h₁', h₂']
  | _, _, _, _, .matchBot h, hb | _, _, _, _, .iteBot h, hb => by
      simp only [Expr.breaks, Bool.or_eq_false_iff] at hb
      exact Typed.brk_nil h (by simp [hb])
  | _, _, _, _, .letIn h₁ h₂ _, hb | _, _, _, _, .letInDiv h₁ h₂, hb => by
      simp only [Expr.breaks, Bool.or_eq_false_iff] at hb
      have h₁' := Typed.brk_nil h₁ hb.1
      have h₂' := Typed.brk_nil h₂ hb.2
      simp only at h₁' h₂'
      simp [h₁', h₂']
  | _, _, _, _, .ite hc h₁ h₂ _, hb => by
      simp only [Expr.breaks, Bool.or_eq_false_iff] at hb
      have hc' := Typed.brk_nil hc hb.1.1
      simp only at hc'
      simp [hc', Typed.brk_nil h₁ hb.1.2, Typed.brk_nil h₂ hb.2]

/-- (helper) The same, for an expression list. -/
theorem TypedArgs.brk_nil {P R} : ∀ {Γ : Ctx} {es Ts} {Ω : Out},
    TypedArgs P R Γ es Ts Ω → Expr.breaksList es = false → Ω.brk = []
  | _, _, _, _, .nil, _ => rfl
  | _, _, _, _, .cons h hs, hb => by
      simp only [Expr.breaksList, Bool.or_eq_false_iff] at hb
      have h' := Typed.brk_nil h hb.1
      simp only at h'
      simp [Out.add, h', TypedArgs.brk_nil hs hb.2]
  | _, _, _, _, .consBot h _, hb => by
      simp only [Expr.breaksList, Bool.or_eq_false_iff] at hb
      exact Typed.brk_nil h hb.1

/-- (helper) The same, for a `match`'s arms. -/
theorem TypedArms.brk_nil {P R} : ∀ {Γ₀ : Ctx} {arms Tss T} {os : List (Option Ctx)}
    {Δs : List Ctx}, TypedArms P R Γ₀ arms Tss T os Δs → Expr.breaksList arms = false → Δs = []
  | _, _, _, _, _, _, .noArms, _ => rfl
  | _, _, _, _, _, _, .arm h _ hs, hb | _, _, _, _, _, _, .armDiv h hs, hb => by
      simp only [Expr.breaksList, Bool.or_eq_false_iff] at hb
      have h' := Typed.brk_nil h hb.1
      simp only at h'
      simp [h', TypedArms.brk_nil hs hb.2]
end

/-- (Fn) §5.8's entry context is well-formed: every parameter enters
`Owned`, a shape of every type (helper). -/
theorem fnCtx_wf (D : Decls) (fd : FnDef) : Ctx.Wf D (fnCtx fd) := by
  intro en hen
  simp only [fnCtx, List.mem_reverse, List.mem_map] at hen
  obtain ⟨p, _, rfl⟩ := hen
  exact Entry.wf_owned D p.ty p.mu

/-- **The shape invariant holds at every normal outgoing state of a function
body** (RUE-2340): `Typed.wf` from (Fn) §5.8's entry context, which
`fnCtx_wf` makes well-formed. This is the end-to-end form: no `Ctx.Wf`
hypothesis is left for a caller to supply. -/
theorem Typed.wf_fnCtx {P R} {fd : FnDef} {e T} {Ω : Out}
    (h : Typed P R (fnCtx fd) e T Ω) : ∀ Γ', Ω.norm = some Γ' → Ctx.Wf P.decls Γ' :=
  (h.wf (fnCtx_wf P.decls fd)).norm

end RueCore
