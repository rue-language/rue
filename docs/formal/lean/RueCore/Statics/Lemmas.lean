module

public import RueCore.Statics

@[expose] public section

/-!
# RueCore.Statics.Lemmas — the lemmas about `Statics.lean`'s definitions (layer L2)

Every theorem `Statics.lean` held, moved here verbatim and in source order so
that the definitions layer holds definitions only (RUE-2460; README, "Layers").
The section headings are `Statics.lean`'s own, repeated where a moved
theorem sits under one; the definitions they are about stay in `Statics.lean`.
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

/-- `overwriteOk` is §5.2's disjunction, spelled as the rule spells it: the
`Prop` form is what `Typed.assign` carries, the `Bool` form what `check`
decides. -/
theorem overwriteOk_iff {D : Decls} {u : OwnSt} {T : Ty} :
    overwriteOk D u T = true ↔ (u = .movedOut ∨ T.mult D ≠ .linear) := by
  cases u <;> simp [overwriteOk]

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
          simp [OwnSt.joinList, hab, hl] <;> rfl

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
          simp [OwnSt.joinList, hbc, hl] <;> rfl

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
          simp [Ctx.join, hab, hl] <;> rfl

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
          simp [Ctx.join, hbc, hl] <;> rfl

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
