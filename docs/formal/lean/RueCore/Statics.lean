module

public import RueCore.Syntax

@[expose] public section

/-!
# RueCore.Statics — ownership-threading typing (§5)

This module holds definitions only (layer L1, README "Layers"); the theorems
about them are in `Statics/Lemmas.lean` (layer L2), moved there verbatim (RUE-2460).

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

/-- §3's field join, over the field types of one declaration: `⊔ { class(Ti) }`
read left to right. `Attr.lift` then lifts it by the declared attribute. -/
def StructDecl.baseOf (D : Decls) (sd : StructDecl) : Mult :=
  sd.fields.foldl (fun m T => m.join (Ty.mult D T)) .copy

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

/-- Every context in the list has the skeleton `Γ₀` has: what (Match) §5.5's
arms all share, since each extends `Σ0` and pops what it added. Written by
recursion rather than as `∀ Γᵢ ∈ Γs` so that the recursor's `match` case reads
it as a conjunction (helper). -/
def Ctx.SameSkel (Γ₀ : Ctx) : List Ctx → Prop
  | [] => True
  | Γ :: Γs => Ctx.skel Γ = Ctx.skel Γ₀ ∧ Ctx.SameSkel Γ₀ Γs

/-- A delivered context **extends** `Γ`: it is `Γ`'s skeleton with zero or
more bindings pushed on top. A `⟨break, Σ⟩` delivery records the whole
context in force at the `break` (`Typed.brk`), so between the loop that reads
it and the `break` that made it sit the bindings of every `let` and every
`match` arm the `break` is inside (helper). -/
def Ctx.Extends (Γb Γ : Ctx) : Prop := ∃ pre, Γb.skel = pre ++ Γ.skel

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

/-! ### The shape invariant, judgment-wide (RUE-2340)

`Ctx.Wf` — every entry's ownership state a shape of its declared type — is
the premise §5.5's associativity (`OwnSt.join_assoc`, `Ctx.joinAll_perm`)
carries. With §5.7's `⊥` an arbitrary context of the incoming skeleton, as it
was before the judgment carried `Ω`, a judgment-wide preservation theorem was
false: a `return` arm could feed the join a state no rule writes. §5.3's `Ω`
gives `⊥` no state at all, so every normal outgoing state is one a rule
wrote, and `Typed.wf` below proves the invariant is preserved. The premise is
then discharged once, for every derivation from a well-formed context. -/

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

end RueCore
