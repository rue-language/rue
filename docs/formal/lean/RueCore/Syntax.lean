import RueCore.Float

/-!
# RueCore.Syntax — abstract syntax of the spike fragment

A fragment of the Rue core calculus (`docs/formal/01-core-calculus.md` §2–§3),
scoped for the mechanization spike:

* Types: `int(w, s)` at every width `w ∈ {8, 16, 32, 64}` and both
  signednesses, `float(w)` at both widths (§2's `𝔽_w`, mechanized in
  `Float.lean`), `bool`, `unit`, the monomorphic struct and **enum** types
  a declaration of the program's declaration environment names, and `[T; n]` —
  the fixed-length array of §2, whose class is §3's lift of `class(T)`.
* Places: §5's `Path ::= x | Path.f | Path[c]` — a root binding, a chain of
  field projections, and **constant** array index steps (`Place`). §5's `Path`
  tracks an index only when it is a compile-time constant, which is the whole
  of §9's item 4; a *dynamic* index is not a path at all and reaches its
  element through `Expr.indexRead`/`Expr.indexWrite` instead. An enum's payload
  is **not** a path: §5.6 says outright that payload paths are not statically
  tracked, and the only way into a payload is a `match` arm's binding, so
  `Place` gains no enum step and `Ty.fieldAt` is `none` at an enum type.
* Expressions: literals carrying their resolved type (`4.1:2`), place use
  (§4.2), the whole §2 `⊕`/`⋚` integer operator set and the `⊖` unary set
  (§5.8 with the §6.4 trap dynamics), `@intCast` (`4.13:24`–`4.13:28`),
  `@panic` and `@dbg` (§5.8's (Panic) and (Dbg), §6.12), float literals and
  the float intrinsics (§5.8's (Float-Arith), (Float-Neg), (Float-Ord),
  (Total-Cmp), (Int-To-Float), (Float-To-Int), (Float-Cast), (Float-Round),
  with the §6.4 dynamics), struct literals
  (§5.8's (Struct-Intro)) and the projection that eliminates one, the array
  literal of §5.8's (Array-Intro) with the surface repeat form, the
  dynamic-index read and write whose bounds check is §6.5's
  (D-Index)/(D-Index-Trap),
  `@drop` (§5.3), `let` (§5.6 scope exit), assignment at a place with
  reinitialization (§5.2), sequencing with the discard check (§5.3), `if` with
  the branch join (§5.5), enum construction and the `match` that eliminates it
  (§5.5's (Enum-Intro) and (Match), §6.6), by-value calls (§5.8's (Call),
  §6.9), and `return` (§5.7's (Return-Value), §6.9).
* Variables are de Bruijn indices: the calculus reaches the core only through
  elaboration, and name resolution is elaboration's job.
* Functions and struct declarations are likewise named by their index in the
  program, the same way bindings are named by their de Bruijn index:
  elaboration resolves the name and the field order (`3.6:15`).

No borrows/loans (Λ is ambiently empty in the current core anyway — §5
preamble), no by-reference parameters, no loops, no accessor calls: those are
the next milestones, not this slice's scope.

## Which projections this fragment may move

§4.2 gives a projection in value context three plans, and the fragment
mechanizes two of them. `Ordinary` — the partial move of `3.8:22` — is
(Use-Copy)/(Use-Move) here. `Declared(d, π_s)`, the declared-linear
destructure of `3.8:33`, is `declaredPrefix` below: §4.2's `dl(Γ, p) =
(d, π_s)`, the split of the path at its **longest proper prefix** of
declared-`linear` struct type, which (Use-Declared-Linear-Destructure) §5.1 and
(D-Use-Declared-Linear) §6.3 then discharge. `Untrackable(OrdinaryDynamic)` is
the dynamic index *read*, whose one successful rule is
(Use-Untrackable-Dynamic-Copy) §5.1: `Expr.indexRead` carries its
`class(T) = Copy` premise, and §4.2's "there is no
successful static rule … when `class(T) ∈ {Affine,Linear}`" is that premise's
absence rather than a rule of its own (E0904). A dynamic-index *write* is not
a use at all — §4.2 classifies value-context uses, and an assignment
destination is neither — so `Expr.indexWrite` carries (Assign) §5.2's own
`Σ1(p) = MovedOut ∨ ¬carries_linear(T)` at the element type instead
(`3.8:77`, E0493), and an affine element is written in place.
`Untrackable(DeclaredLinearDynamic)` is ill-formed by §4.2, and its one
instance here is the dynamic *read* under a declared-`linear` prefix, which
(Use-Untrackable-Dynamic-Copy) refuses through `declaredPrefix … = none`
(E0904); the dynamic *write* under such a prefix is not a use and is admitted,
as the compiler admits it. A dynamic index is never a `Place` step, so no
plan is ever computed through one.

The plan is therefore a **function of the type and the path**, and the four
Ordinary rules ((Use-Copy), (Use-Move), (@Drop), (@Drop-Copy)) carry
`declaredPrefix … = none` as a premise — §5.1's "read only with an `Ordinary`
plan", which is what keeps the two groups from overlapping when the selected
leaf is `Copy`. A use of a declared-`linear` place *whole* is ordinary (the
plan looks for a **proper** prefix), so such a struct is still moved, dropped
and reinitialized by the ordinary rules; and linearity *by infection*
(`3.8:58`) draws no plan at all, because `Ty.declaredLinear` reads the declared
attribute rather than the class (`3.8:60`'s second paragraph).

`linearResidue` is §5.1's `linear-residue(S, π_s)` on the same split: the
ordered residue traversal's retained places, tested at their **types**. It is
type-level rather than Σ-level deliberately — the compiler reads the type of a
sibling that has itself been moved out (probe d5c) — and it is what rejects
a destructure that would silently drop a linear sibling (`3.8:60`, E0474).

A destructure whose *selected path* passes through an **index** step —
`x.arr[0]` on a declared-`linear` `x` — is admitted: §4.2's `dl` says "the
selected path may pass through nested structs and constant-index arrays",
`3.8:71` says consuming the linear sub-places of an array reached through a
field projection discharges the array field's obligation, and the compiler
accepts both that shape (probe `b3`) and the one whose *consumed* place is an
element (`a[0].x0` on `[T0; 2]`, probe `b4`; `h.arr[0].x0` through a field,
probe `d1b`; `a[0][0].x0` at a nested index, probe `d2b`). So neither
destructure rule carries the root-index restriction, and the residue
traversal's array clause — retained elements in ascending index order — is
reached (`linearResidue`; probe `b9` is its E0474, probe `b20` its order). A
retained *array* is an ordinary residue place and needs nothing extra
(probe d9).

## Arrays, part 2: element-wise partial moves at constant-index paths

`[T; n]` is a value, a literal, a constant-index path step, a dynamic-index
read or write, **and** a place a move or an `@drop` may take one element out
of: `3.8:68`'s constant-index element move, the `MovedOut` element state it
leaves, and `3.8:73`'s path-specific element drop (RUE-2327; part 1,
RUE-2322, held the array whole). `rootIdxOnly` is §4.2's own bound on that
move — "an index step `[c]` may appear in a moved path only applied directly
to the root binding" — carried by (Use-Move) §5.1 and (@Drop) §5.3, the two
rules that move a path out, and computed from the **type** the path reaches
rather than from the place's spelling (its docstring says why). The compiler
reports its failures as E0904 (probes `a6`, `a6b`, `a9`, `e1`, `e2`, `e4`).

Nothing new is needed of the dynamics. A constant index is a step of `π` like
a field slot, so `readAt`/`writeAt` already navigate it, the `⊘` (D-Use-Move)
writes at the element *is* `3.8:73`'s per-path drop flag, and §6.11's
`⊘`-skip over an array's elements is the element-wise drop (probes `a1`,
`a4`/`a4b`, `a8`, `b19`).

What a **dynamic** index still cannot reach is any step below it. §2's place
grammar has `p [ e ]`, so `a[i].x0` is a place of the calculus and the
compiler reads and writes it; here `Expr.indexRead` yields the element *value*
and a dynamic index is not a `Place` step, so neither `a[i].x0` nor
`a[i].x0 = 50` has a form at all. That debt is RUE-2342.

An array's ownership state is a `Path ⇀ {Owned, MovedOut}` tree like a
struct's, so every §5 predicate that recurses into a node's children — §5.6's
`residual-linear`, §5.5's join and `ownedJoinOk`, §5.1's residue traversal,
and `Soundness.lean`'s `ContentsMatches` — carries an array clause that reads
the element type `n` times (`List.replicate n T`). Part 1 wrote them out
before any of them could see a `MovedOut` element; this part is where they
all can.

**(Assign) is not a move**, and its destination is bounded differently: an
element write may be reached through any projection (`h.a[0] = …`, probe `b6`;
`a[1][0] = …`, probe `c6`), but `3.8:72`/`7.1:46` forbid *any* write into an
array one of whose elements has been moved out, at the moved index as much as
at a sibling. `assignArrayOk` (`Statics.lean`) is that side condition, E0480.

## An array value carries its element type

`Val.array` and `Contents.array` carry `T`, for the reason `Val.int` carries
`(w, s)`: the machine's drop and copy decisions read a value's class, and the
class of `[T; n]` is **not** a function of the elements present. `3.8:74`
grants a zero-length array of a non-`Copy` element type droppability but not
duplicability, so §3 classes `[NC; 0]` `Affine` while the value `[]` holds
nothing at all (RUE-526). Carrying `T` is what makes `Val.mult` agree with
`Ty.mult` there; §6.1 writes the value as `[v1, …, vn]` because its `class` is
never read in the same breath.

## An integer value carries its type

§6.1's value form is `n_T` at `T = int(w,s)`, "because overflow, comparison
signedness, and bitwise width all depend on it". `Ty.int` and `Val.int`
therefore carry the width and signedness, `InBounds` is the `min_T ≤ n ≤ max_T`
side condition §6.1 states for `n_T`, and every §6.4 rule reads `w` and `s`
off its operands. A literal carries its type for the same reason: elaboration
has already resolved the surface default (`4.1:2`, `4.1:3`), so the core sees
a concrete `int(w,s)` and never an unresolved one.

`wrapInt` is §6.4's `val_{w,s}(β_w(·))` composed: the reinterpretation of a
`w`-bit pattern at signedness `s`, which is what the bitwise and shift rules
compute in and what makes their results total. Arithmetic does **not** wrap —
`3.1:6` traps instead — so `wrapInt` is used by (D-Bit)/(D-Shl)/(D-Shr) only.

## Where a type's class lives

§3 fixes `class(S)` as the join of the field classes lifted by the declared
attribute. A declaration *records* that class, and the program well-formedness
judgment `WfStructs` (`Statics.lean`) is §3's equation: the recorded class is
the lifted join, and a `@copy` declaration's fields are all `Copy` and it
declares no destructor (`3.8:18`, `3.9:31`). Recording it is what lets
`Ty.mult` be a lookup rather than a recursion over the environment.

An enum declaration records its class the same way, and §3 gives it a simpler
equation: no attribute to lift and no destructor to declare, just the join over
**every** payload component of **every** variant (`6.3:19`), because the active
variant is a run-time fact. `WfEnums` is that equation and
`enum_carriesLinear_iff` is the biconditional it buys — which is why an enum one
of whose variants carries a `linear` payload must be consumed even when the
value in hand is the other variant.

What makes both equations definitions rather than fixpoint conditions is
`3.0:5` (E0483): no struct or enum contains itself by value, directly or
through a cycle of struct fields and enum payloads. The condition is joint over
the two layers because the recursion is — a field may name an enum and a
payload may name a struct — and `class_unique` (`Statics.lean`) is the proof it
buys, unconditionally: on declarations of the same shapes, at most one
assignment of classes satisfies §3's equations.

`Expr` and `Val` derive `Repr` but not `DecidableEq`: both carry a nested
inductive occurrence (`List Expr`, `List Val`), for which Lean's `DecidableEq`
deriving handler has no instance, and nothing in the package compares
expressions or values.
-/

namespace RueCore

/-- The multiplicity lattice (§3): `Copy ⊑ Affine ⊑ Linear`. -/
inductive Mult where
  | copy
  | affine
  | linear
deriving DecidableEq, Repr

/-- The lattice order as a number, `Copy ⊑ Affine ⊑ Linear` (§3) (helper). -/
def Mult.rank : Mult → Nat
  | .copy => 0
  | .affine => 1
  | .linear => 2

/-- The join `⊔` of §3's lattice: the least upper bound, which is what makes a
struct at least as restrictive as its most restrictive field ("infectiousness
is just the join"). -/
def Mult.join (a b : Mult) : Mult := if a.rank ≤ b.rank then b else a

/-- A struct's declared attribute (§3's `attr(S)`): none, `@copy` (`3.8:18`),
or `linear` (`3.8:57`). -/
inductive Attr where
  | none
  | copy
  | linear
deriving DecidableEq, Repr

/-- §3's lifting of the field join by the declared attribute: `linear` forces
`Linear` (`3.8:58`, `3.8:57`), `@copy` forces `Copy` (well-formed only when
the join is already `Copy` and the struct declares no destructor — `3.8:18`,
`3.9:31`, which `WfStructs` requires), and a struct with no attribute is
`Linear` when its fields join to `Linear` and `Affine` otherwise (`3.8:3`:
structs are affine by default). -/
def Attr.lift : Attr → Mult → Mult
  | .linear, _ => .linear
  | .copy, _ => .copy
  | .none, base => if base = .linear then .linear else .affine

/-! ## Integer types: widths, signedness, and the `n_T` bounds -/

/-- The integer widths §2's `int(w, s)` ranges over: `w ∈ {8, 16, 32, 64}`. -/
inductive IntWidth where
  | w8
  | w16
  | w32
  | w64
deriving DecidableEq, Repr

/-- The signedness §2's `int(w, s)` ranges over. -/
inductive Sign where
  | signed
  | unsigned
deriving DecidableEq, Repr

/-- `w` as a number of bits (§2) (helper). -/
def IntWidth.bits : IntWidth → Nat
  | .w8 => 8
  | .w16 => 16
  | .w32 => 32
  | .w64 => 64

/-- `2^w`: the number of `w`-bit patterns, the modulus §6.4's `β_w` works in
(helper). -/
def IntWidth.modulus (w : IntWidth) : Nat := 2 ^ w.bits

/-- `min_T` for `T = int(w,s)`, the lower end of §6.1's `n_T` range: `0` for an
unsigned type and `-2^(w-1)` for a signed one. -/
def intMin : IntWidth → Sign → Int
  | _, .unsigned => 0
  | w, .signed => -(2 ^ (w.bits - 1))

/-- `max_T` for `T = int(w,s)`, the upper end of §6.1's `n_T` range: `2^w - 1`
for an unsigned type and `2^(w-1) - 1` for a signed one. -/
def intMax : IntWidth → Sign → Int
  | w, .unsigned => 2 ^ w.bits - 1
  | w, .signed => 2 ^ (w.bits - 1) - 1

/-- `min_T ≤ n ≤ max_T`: the side condition §6.1 states for its `n_T` values,
and the range §6.4's arithmetic traps outside of (`3.1:6`: Rue arithmetic
never wraps). -/
def InBounds (w : IntWidth) (s : Sign) (n : Int) : Prop :=
  intMin w s ≤ n ∧ n ≤ intMax w s

instance (w : IntWidth) (s : Sign) (n : Int) : Decidable (InBounds w s n) := by
  unfold InBounds; infer_instance

/-- `β_w(n)`: the `w`-bit two's-complement pattern of `n`, as a number below
`2^w` (§6.4's bitwise and shift rules) (helper). -/
def bitsOf (w : IntWidth) (n : Int) : Nat := (n % (w.modulus : Int)).toNat

/-- `val_{w,s}(β)`: a `w`-bit pattern read back at signedness `s` (§6.4). The
pattern is reduced modulo `2^w` first, so the function is total on every
number and lands in `[min_T, max_T]` by construction (`valOf_inBounds`)
(helper). -/
def valOf (w : IntWidth) (s : Sign) (b : Nat) : Int :=
  let m : Nat := b % w.modulus
  match s with
  | .unsigned => (m : Int)
  | .signed => if 2 * m < w.modulus then (m : Int) else (m : Int) - (w.modulus : Int)

/-- `val_{w,s}(β_w(n))`: the number `n` read as a `w`-bit pattern at
signedness `s`. This is what §6.4's `(D-Bit)`, `(D-Shl)` and `(D-Shr)` compute
in; arithmetic never routes through it, because `3.1:6` traps where this would
wrap (helper). -/
def wrapInt (w : IntWidth) (s : Sign) (n : Int) : Int := valOf w s (bitsOf w n)

/-- Every `w`-bit pattern read at signedness `s` denotes a value of
`int(w,s)`, which is what makes §6.4's bitwise and shift rules total
(helper). -/
theorem valOf_inBounds (w : IntWidth) (s : Sign) (b : Nat) : InBounds w s (valOf w s b) := by
  have hmod : b % w.modulus < w.modulus := by
    refine Nat.mod_lt _ ?_
    cases w <;> simp [IntWidth.modulus, IntWidth.bits]
  -- Each conjunct is discharged on its own: `omega` on a conjunction goal
  -- reaches for `Classical.choice`, which this package does not allow itself
  -- (`TRUST.md`), while the same arithmetic per conjunct is constructive.
  cases w <;> cases s <;>
    simp only [valOf, InBounds, intMin, intMax, IntWidth.modulus, IntWidth.bits] at hmod ⊢ <;>
    first
      | (refine ⟨?_, ?_⟩ <;> omega)
      | (split <;> refine ⟨?_, ?_⟩ <;> omega)

/-- The same for `wrapInt` (helper). -/
theorem wrapInt_inBounds (w : IntWidth) (s : Sign) (n : Int) : InBounds w s (wrapInt w s n) :=
  valOf_inBounds w s _

/-! ## Types -/

/-- Types (§2, fragment). `int w s` is §2's `int(w, s)`; `struct s` names the
declaration at index `s` of the program's struct environment and `enum e` the
declaration at index `e` of its enum environment, which elaboration resolves
the surface names to; `array T n` is §2's `[T; n]`, the
fixed-length array of `n ≥ 0` elements of one type (`3.5:1`, `3.5:3`,
`7.1:3`); `3.5:2` is the premise that lets the core see a `Nat` — the length
is a non-negative integer known at compile time, which elaboration has already
folded (`7.1:14`'s `array_length` is the grammar it folds). -/
inductive Ty where
  | int (w : IntWidth) (s : Sign)
  | float (w : FloatWidth)
  | bool
  | unit
  | struct (s : Nat)
  | enum (e : Nat)
  | array (elem : Ty) (n : Nat)
deriving DecidableEq, Repr

/-- Whether a type is an integer type (§2's `int(w, s)`) (helper). -/
def Ty.isInt : Ty → Bool
  | .int _ _ => true
  | .float _ | .bool | .unit | .struct _ | .enum _ | .array _ _ => false

/-- Whether a type is one `@dbg` renders, which §5.8's (Dbg) restricts to
`int(w,s)`, `float(w)` and `bool` — the compiler's own restriction (E0702).
The `float(w)` case is `3.12:39`, and the text it produces is `3.12:40`–
`3.12:42` (`FloatDatum.render`, `Float.lean`) (helper). -/
def Ty.observable : Ty → Bool
  | .int _ _ | .float _ | .bool => true
  | .unit | .struct _ | .enum _ | .array _ _ => false

/-- A monomorphic struct declaration: §2's `S { f1: T1, …, fk: Tk }` with its
declared attribute (§3), whether it declares a destructor (`3.9`), and the
class §3 assigns it. Fields are listed in **declaration order**, which is the
order §6.11 drops them in (`3.9:13`, after the user destructor — `3.9:28`) and
the order (Struct-Intro) §5.8's initializers are presented in (`3.6:15`); they
are named by position, as bindings are, because elaboration resolves field
names. -/
structure StructDecl where
  /-- `attr(S)` (§3): `none`, `@copy` (`3.8:18`) or `linear` (`3.8:57`). -/
  attr : Attr
  /-- The field types, in declaration order: the order `3.9:13` drops them
  in, and the order a literal's initializers are presented in (`3.6:15`). -/
  fields : List Ty
  /-- Whether `S` declares `drop fn S(self)` (`3.9`), which §6.11 runs before
  the fields (`3.9:28`). -/
  dtor : Bool
  /-- `class(S)` (§3), the field join lifted by `attr`; `WfStructs`
  (`Statics.lean`) is the equation that pins it. -/
  cls : Mult
deriving DecidableEq, Repr

/-- A monomorphic enum declaration: §2's `enum E { K1(T̄1), …, Kn(T̄n) }`, one
payload tuple per variant in **declaration order** — the order a tag `Kj`
indexes and the order (Match) §5.5's arms are presented in — together with the
class §3 assigns it. A variant with an empty tuple is §2's discriminant-only
case (`ai = 0`, `6.3:14`). An enum declares **no attribute** and **no
destructor**: §3 gives it no `@copy`/`linear` mark, its class is exactly the
payload join (`6.3:19`), and the compiler rejects `drop fn E(self)` because a destructor names a
struct type (E0417), so there is nothing here for §6.11 to run before the
payload. -/
structure EnumDecl where
  /-- The payload types of each variant, in declaration order; variant `j`'s
  tuple is `variants[j]`, and `[]` is the discriminant-only case (`6.3:14`). -/
  variants : List (List Ty)
  /-- `class(E)` (§3, `6.3:19`), the join over **every** payload component of
  **every** variant; `WfEnums` (`Statics.lean`) is the equation that pins it,
  and `enum_carriesLinear_iff` is `6.3:19` read as a biconditional. -/
  cls : Mult
deriving DecidableEq, Repr

/-- The program's declaration environment: §2's type-declaration production
`D ::= struct S { … } | enum E { … }`, one list per kind, each indexed the way
`Ty.struct`/`Ty.enum` names it. The two layers are separate lists rather than
one list of a sum because a type names one or the other and never both, and
because §3 assigns their classes by two different equations. -/
structure Decls where
  /-- The struct declarations §2's `S` names, indexed by `Ty.struct`. -/
  structs : List StructDecl
  /-- The enum declarations §2's `E` names, indexed by `Ty.enum`. -/
  enums : List EnumDecl
deriving DecidableEq, Repr

/-- A declaration environment with no enum in it: the shape every program of
the fragment had before enums, and the one a generated program still has
(`Gen.lean`) (helper). -/
def Decls.ofStructs (D : List StructDecl) : Decls := { structs := D, enums := [] }

/-- `class(S)` for a declared struct type (§3), read off the declaration. An
index the environment does not have is `Affine`, the class of a struct with no
attribute and no linear field — the conservative reading of a program
`WfStructs` rejects anyway (helper). -/
def Decls.classOf (D : Decls) (s : Nat) : Mult :=
  match D.structs[s]? with
  | some sd => sd.cls
  | none => .affine

/-- `class(E)` for a declared enum type (§3, `6.3:19`), read off the
declaration. An index the environment does not have is `Affine`, the
conservative reading of a program `WfEnums` rejects anyway — `Copy` would let
such a type be duplicated (helper). -/
def Decls.enumClassOf (D : Decls) (e : Nat) : Mult :=
  match D.enums[e]? with
  | some ed => ed.cls
  | none => .affine

/-- `class(T)` (§3), against the program's declaration environment. Scalars are
`Copy` at every width and signedness, floats included (`3.12:2a` classifies
both float types `Copy` and `3.8:2` lists them, so the core takes it
directly); a struct type has the class its declaration records, and so does an
enum type — whose record is the payload join over every variant (`6.3:19`),
because the active variant is not a static fact.

`class([T; n])` is §3's own four-line table, read as one `if`: `Copy` whenever
`class(T)` is (which covers every `n`, the empty array included), `Affine`
when `n = 0` and `class(T)` is not — a zero-length array of a non-`Copy`
element type carries nothing, so `3.8:74` grants it droppability and says
nothing about duplicability (RUE-526: an earlier table classed every `[T; 0]`
`Copy` and over-granted contraction; the compiler agrees with the current
reading, `let b = a; let c = a;` on an `[NC; 0]` is E0205) — and `class(T)`
itself otherwise, which is §3's "infectiousness is just the join" with the
element type as the only member. -/
def Ty.mult (D : Decls) : Ty → Mult
  | .int _ _ | .float _ | .bool | .unit => .copy
  | .struct s => D.classOf s
  | .enum e => D.enumClassOf e
  | .array T n =>
      match Ty.mult D T with
      | .copy => .copy
      | m => if n = 0 then .affine else m

/-- `carries_linear(T)` (§5.3): `class(T) = Linear`, which §5.3 states is the
same predicate as "Linear lifted through the aggregates" because `class` *is*
that join (§3). `struct_carriesLinear_iff` (`Statics.lean`) is the lifting,
proved through the field join. -/
abbrev Ty.carriesLinear (D : Decls) (T : Ty) : Prop := T.mult D = .linear

/-! ## Places: §5's `Path`, and the type a path reaches -/

/-- A place (§5's `Path ::= x | Path.f | Path[c]`, and §2's `p` production): a
root binding named by its de Bruijn index, under a chain of field projections
named by their declaration slot (`3.6:15` — elaboration resolves the surface
field name to the slot) and **constant** array index steps. §5's `Path` tracks
an index only when it is a compile-time constant (§9's item 4: "what keeps the
ownership analysis decidable without dependent types"), so `Place.idx` carries
a `Nat` rather than an expression, and a dynamic index is not a path
(`Expr.indexRead`/`Expr.indexWrite`). -/
inductive Place where
  | var (i : Nat)
  | proj (p : Place) (f : Nat)
  | idx (p : Place) (c : Nat)
deriving DecidableEq, Repr

/-- The binding a place is rooted at (§5's `root(p)`) (helper). -/
def Place.root : Place → Nat
  | .var i => i
  | .proj p _ | .idx p _ => p.root

/-- The projection steps of a place, from the root outward — the `π` §6.3
navigates a stored aggregate with, "field indices and already-reduced array
indices". A field slot and a constant index are one kind of step here, and
which one a step is is decided by the type it is taken at (`Ty.fieldAt`)
(helper). -/
def Place.path : Place → List Nat
  | .var _ => []
  | .proj p f => p.path ++ [f]
  | .idx p c => p.path ++ [c]

/-- The type one **step** of a path reaches: a declaration's field at a slot,
or an array's element at a constant index within its length (`7.1:9` — a
constant index is bounds-checked at compile time, so an out-of-range one has
no type and therefore no derivation, which is probe `a9`'s E0902). `none`
where the step is not a step of the type reached so far — an enum type among
them, since a payload is not a path. The two kinds that do step share
one function because §5's `Path` puts them on one production, and §6.3's `π`
is likewise "field indices and already-reduced array indices" (helper). -/
def Ty.fieldAt (D : Decls) : Ty → Nat → Option Ty
  | .struct s, f =>
      match D.structs[s]? with
      | some sd => sd.fields[f]?
      | none => none
  | .array T n, c => if c < n then some T else none
  | .int _ _, _ | .float _, _ | .bool, _ | .unit, _ | .enum _, _ => none

/-- `Γ ⊢ p : T` for a path read off the root's declared type: follow the
steps — field slots and constant indices alike (`Ty.fieldAt`) — failing where
a step is not a step of the type reached so far. Types
are not flow-sensitive, so this is the whole of the place's typing (§5
preamble: `Γ` is fixed at the binder), and a constant index out of range fails
*here*, which is `7.1:9`'s compile-time bounds check. -/
def Ty.atPath (D : Decls) : Ty → List Nat → Option Ty
  | T, [] => some T
  | T, f :: π =>
      match T.fieldAt D f with
      | some T' => T'.atPath D π
      | none => none

/-- Navigating a concatenated path is navigating the two halves in turn. This
is what lets §5.1's premises at `d` — reached by `π_d` — and the leaf's type —
reached by `π_d · π_s` — be read off one another (helper). -/
theorem Ty.atPath_append (D : Decls) : ∀ (T : Ty) (π ρ : List Nat),
    T.atPath D (π ++ ρ) = (T.atPath D π).bind fun T' => T'.atPath D ρ
  | _, [], _ => by simp [Ty.atPath]
  | T, f :: π, ρ => by
      simp only [List.cons_append, Ty.atPath]
      cases T.fieldAt D f with
      | none => simp
      | some T' => exact Ty.atPath_append D T' π ρ

/-- Whether the path takes **no** step at an array node — the tail of
`3.8:68`'s root-index rule, read from the type the walk has reached. A step
whose type is not a step of what has been reached so far ends the walk at
`true`: the path is untypeable there (`Ty.atPath` is `none`) and the rule that
consults this has already failed its `Γ ⊢ p : T` premise (helper). -/
def noArrayStep (D : Decls) : Ty → List Nat → Bool
  | _, [] => true
  | T, f :: π =>
      match T with
      | .array _ _ => false
      | .struct _ | .int _ _ | .float _ | .bool | .unit | .enum _ =>
        (match T.fieldAt D f with
         | some T' => noArrayStep D T' π
         | none => true)

/-- **§4.2's "element moves only at the root"** (`3.8:68`, E0904): "an index
step `[c]` may appear in a moved path only applied directly to the root
binding — `x[c]` or `x[c].f…` moves are legal (for constant `c`), but an array
reached through any projection (`x.f[c]`) … cannot be moved out of". Read as a
walk: no step is taken at an `.array` node **except** the first step off the
root, whatever that first step is. A nested index (`x[c][c']`) is refused by
the same reading, because its second index is taken at the array `x[c]` rather
than at the root binding; the spec states it as "an array reached through
another projection … cannot be moved out of" and the compiler agrees (probes
`a9`, `e1`, `e2`: E0904).

The test is keyed on the **type**, not on the place's spelling. `Place.path`
and `Ty.fieldAt` are constructor-blind — a field slot and a constant index are
one production of §5's `Path` and one function here — so reading `.idx` vs
`.proj` off the place would answer for `h.a[0]` spelled with `Place.idx` and
not for the same place spelled with `Place.proj`, and would accept an element
move the compiler rejects (probes `a6`/`a6b`, E0904). It is the type at the
node that makes a step an index step, so that is what this reads.

The premise is (Use-Move) §5.1's and (@Drop) §5.3's, the two rules that move a
path out. It is **not** carried by the declared-linear destructure: §4.2's `dl`
"may pass through nested structs and constant-index arrays", `3.8:71` says
outright that consuming the linear sub-places of an array reached through a
field projection discharges the array's obligation, and the compiler accepts
both (`h.arr[0].x0`, probe `d1b`; `a[0][0].x0` at a nested index, probe `d2b`).
Nor is it carried by (Assign) §5.2, whose destination is not a move at all
(`h.a[0] = …` and `a[1][0] = …` both compile; probes `b6`, `c6`). -/
def rootIdxOnly (D : Decls) : Ty → List Nat → Bool
  | _, [] => true
  | T, f :: π =>
      match T.fieldAt D f with
      | some T' => noArrayStep D T' π
      | none => true

/-- **The array an assignment destination steps *into***, as a prefix of the
path: `some π_a` where `π_a` is the outermost prefix whose type is an array
node the path then takes a step at, and `none` where the path reaches no array
node with a step left to take (the whole-array destination `a = …` among
them). `3.8:72`/`7.1:46` speak of "the array" an element write goes into, and
this is the path to it.

The outermost such array is enough for `assignArrayOk`, because
`fully-owned` at a node is `fully-owned` at everything under it: a write at
`a[1][0]` that demands the whole of `a` has demanded the whole of `a[1]` too
(probe `c8`). A step that is not a step of the type reached so far ends the
walk at `none`, as `rootIdxOnly` ends at `true`: the path is untypeable there
and the rule has already failed `Γ ⊢ p : T` (helper). -/
def arrayPrefix (D : Decls) : Ty → List Nat → Option (List Nat)
  | _, [] => none
  | T, f :: π =>
      match T with
      | .array _ _ => some []
      | .struct _ | .int _ _ | .float _ | .bool | .unit | .enum _ =>
        (match T.fieldAt D f with
         | some T' => (arrayPrefix D T' π).map (fun πa => f :: πa)
         | none => none)

/-- No **proper prefix** of the path names a value whose type declares a
destructor: (Use-Move) §5.1's and (@Drop) §5.3's `3.9:34` premise (E0456).
Moving or dropping the whole value is fine — the empty path has no proper
prefix — because the restriction exists so that a destructor never observes a
hole in the value it runs on.

An **array** step declares no destructor of its own: `3.9:14` gives `[T; n]` a
destructor exactly when `T` has one, and `3.9:34` speaks of a type that
*declares* one, so the array node imposes nothing and the walk continues into
the element. Verified twice over: probe `a1` moves an element out of an
`[S1; 3]` whose `S1` declares a destructor and the compiler accepts it, and
probe `b3b` refuses `x.arr[0].x0` with E0456 because the element's *own* type
declares one. -/
def noDtorPrefix (D : Decls) : Ty → List Nat → Bool
  | _, [] => true
  | T, f :: π =>
      match T with
      | .struct s =>
          (match D.structs[s]? with
           | some sd =>
               !sd.dtor &&
                 (match sd.fields[f]? with
                  | some T' => noDtorPrefix D T' π
                  | none => true)
           | none => true)
      | .array T' _ => noDtorPrefix D T' π
      | .int _ _ | .float _ | .bool | .unit | .enum _ => true

/-! ## §4.2's use plan, as a function of the type and the path -/

/-- Whether a type is a struct **declared** `linear` (`3.8:57`) — the mark
§4.2's `dl` looks for along a path and §5.6's `3.8:74` clause reads at a
binding. Linearity *by infection* (`3.8:58`) is deliberately **not** this
predicate: a carrier reaches `Linear` through a field, its projections stay
ordinary partial moves, and `3.8:60`'s second paragraph says so (helper). -/
def Ty.declaredLinear (D : Decls) : Ty → Bool
  | .struct s =>
      (match D.structs[s]? with
       | some sd => sd.attr = .linear
       | none => false)
  | .array _ _ | .int _ _ | .float _ | .bool | .unit | .enum _ => false

/-- **§4.2's `dl(Γ, p) = (d, π_s)`**: the declared-linear use plan, computed
from the root's declared type and the path. `declaredPrefix D T π` is
`some (π_d, π_s)` when `π_d` is the **longest proper prefix** of `π` whose type
is a struct declared `linear` and `π_s` — nonempty, which is what makes the
prefix proper — is the path from `d` to the selected leaf; it is `none` exactly
when no such prefix exists, which is §4.2's `Ordinary` plan and the premise the
four ordinary place rules carry (§5.1: they "are read only with an `Ordinary`
plan").

"Longest" is the recursion's shape: the deeper split wins, and the split at
this node is the fallback. So `d` is the **smallest enclosing** declared-linear
place, not necessarily the root binding (`3.8:33`: "when the access chain
passes through several declared-`linear` levels, the smallest (innermost) one
is destructured"), and a declared-linear ancestor above it keeps its own
obligation (§5.6). A step that is not a field of the type reached so far makes
the whole path untypeable (`Ty.atPath` is `none` there), so the fallback at
such a node is harmless.

§4.2's `dl` is "defined only when every index in the complete path is a
compile-time constant". Every index a `Place` can carry here *is* a constant
(`Place.idx`), and a dynamic index is not a path step at all
(`Expr.indexRead`/`Expr.indexWrite`), so the side condition holds of every
path this walks. An array step is not a struct declared `linear`, so it
imposes no split of its own and the walk continues into the element — a
declared-`linear` struct *above* an array step is still found, which is what
leaves §4.2's `Untrackable(DeclaredLinearDynamic)` (ill-formed there, E0904 in
the compiler) no instance at a dynamic index. -/
def declaredPrefix (D : Decls) : Ty → List Nat → Option (List Nat × List Nat)
  | _, [] => none
  | T, f :: π =>
      match T.fieldAt D f with
      | some T' =>
          (match declaredPrefix D T' π with
           | some r => some (f :: r.1, r.2)
           | none => if T.declaredLinear D then some ([], f :: π) else none)
      | none => if T.declaredLinear D then some ([], f :: π) else none

/-- **The plan really splits the path** (§4.2): `π = π_d · π_s` with `π_s`
nonempty, so `d` is a *proper* prefix of `p` and the leaf really is under it.
This is what lets §5.1's premises at `d` and §6.3's traversal from `d` be read
against the one place the program wrote (helper). -/
theorem declaredPrefix_split (D : Decls) : ∀ (T : Ty) (π πd πs : List Nat),
    declaredPrefix D T π = some (πd, πs) → π = πd ++ πs ∧ πs ≠ []
  | _, [], _, _, h => by simp [declaredPrefix] at h
  | T, f :: π, πd, πs, h => by
      have hfall : ∀ a b : List Nat,
          (if T.declaredLinear D then some (([] : List Nat), f :: π) else none) = some (a, b) →
            f :: π = a ++ b ∧ b ≠ [] := by
        intro a b hh
        split at hh
        · simp only [Option.some.injEq, Prod.mk.injEq] at hh
          obtain ⟨rfl, rfl⟩ := hh
          exact ⟨rfl, by simp⟩
        · exact absurd hh (by simp)
      rw [declaredPrefix] at h
      cases hfa : T.fieldAt D f with
      | none => simp only [hfa] at h; exact hfall _ _ h
      | some T' =>
          simp only [hfa] at h
          cases hrec : declaredPrefix D T' π with
          | none => simp only [hrec] at h; exact hfall _ _ h
          | some r =>
              obtain ⟨πd', πs'⟩ := r
              simp only [hrec, Option.some.injEq, Prod.mk.injEq] at h
              obtain ⟨rfl, rfl⟩ := h
              obtain ⟨heq, hne⟩ := declaredPrefix_split D T' π πd' πs' hrec
              exact ⟨by simp [heq], hne⟩

/-- **The type a place below a dynamic index reaches.** A dynamic form is
`p[e₁]π₁[e₂]π₂…[eₖ]πₖ`: the constant place `p`, then `k ≥ 1` dynamic steps,
each followed by a constant path `πⱼ` of field slots and constant indices
(`Expr.indexRead`/`Expr.indexWrite`). `Ty.atDyn D Ta [π₁, …, πₖ]` walks that
tail from `p`'s type `Ta`: each dynamic step must be taken at an array node,
reaches its element type, and `πⱼ` is then read off the element as
`Ty.atPath` reads any path. `none` where a dynamic step is taken at anything
but an array, or where a constant path is not a path of the type reached so
far — which is `Γ ⊢ p : T` failing for the complete place (helper). -/
def Ty.atDyn (D : Decls) : Ty → List (List Nat) → Option Ty
  | T, [] => some T
  | .array E _, π :: πs =>
      match E.atPath D π with
      | some T' => T'.atDyn D πs
      | none => none
  | .int _ _, _ :: _ | .float _, _ :: _ | .bool, _ :: _ | .unit, _ :: _
  | .struct _, _ :: _ | .enum _, _ :: _ => none

/-- **No declared-`linear` proper prefix below the dynamic index** (§4.2's
`Untrackable(DeclaredLinearDynamic)`, ill-formed there and E0904 in the
compiler; probe q11). A proper prefix of the complete place that lies below
the first dynamic step is an element `a[i]` itself or a prefix of one
segment's constant path, and `declaredPrefix D E π = none` at each segment is
exactly "neither the element nor any proper prefix of `π` under it is a struct
declared `linear`" — the segment's own end is excluded, as a leaf is, and
where a later segment follows, that end is an array, which is never declared
`linear`. The prefixes **above** the first dynamic step are `p`'s own, and
`declaredPrefix D T p.path = none` states them (helper). -/
def Ty.dynNoDeclared (D : Decls) : Ty → List (List Nat) → Bool
  | _, [] => true
  | .array E _, π :: πs =>
      (declaredPrefix D E π).isNone &&
        (match E.atPath D π with
         | some T' => T'.dynNoDeclared D πs
         | none => true)
  | .int _ _, _ :: _ | .float _, _ :: _ | .bool, _ :: _ | .unit, _ :: _
  | .struct _, _ :: _ | .enum _, _ :: _ => true

/-- **The consumed place is a declared-`linear` struct** (§5.1's
`Γ ⊢ d : S`, `S` declared `linear`). The rule states it as a premise; here it
is a *consequence* of the plan, so `Typed.useDeclared`/`Typed.dropDeclared`
need not carry it (helper). -/
theorem declaredPrefix_declaredLinear (D : Decls) : ∀ (T : Ty) (π πd πs : List Nat),
    declaredPrefix D T π = some (πd, πs) →
      ∃ Td, T.atPath D πd = some Td ∧ Td.declaredLinear D = true
  | _, [], _, _, h => by simp [declaredPrefix] at h
  | T, f :: π, πd, πs, h => by
      have hfall : ∀ a b : List Nat,
          (if T.declaredLinear D then some (([] : List Nat), f :: π) else none) = some (a, b) →
            ∃ Td, T.atPath D a = some Td ∧ Td.declaredLinear D = true := by
        intro a b hh
        split at hh
        · simp only [Option.some.injEq, Prod.mk.injEq] at hh
          obtain ⟨rfl, rfl⟩ := hh
          exact ⟨T, rfl, ‹_›⟩
        · exact absurd hh (by simp)
      rw [declaredPrefix] at h
      cases hfa : T.fieldAt D f with
      | none => simp only [hfa] at h; exact hfall _ _ h
      | some T' =>
          simp only [hfa] at h
          cases hrec : declaredPrefix D T' π with
          | none => simp only [hrec] at h; exact hfall _ _ h
          | some r =>
              obtain ⟨πd', πs'⟩ := r
              simp only [hrec, Option.some.injEq, Prod.mk.injEq] at h
              obtain ⟨rfl, rfl⟩ := h
              obtain ⟨Td, hpath, hdl⟩ := declaredPrefix_declaredLinear D T' π πd' πs' hrec
              exact ⟨Td, by simp only [Ty.atPath, hfa]; exact hpath, hdl⟩

/-- §5.1's residue traversal at one struct step, read on the **retained**
fields: every field but the selected one, tested at its declared type
(helper). -/
def anyLinearOther (D : Decls) : List Ty → Nat → Bool
  | [], _ => false
  | _ :: Ts, 0 => Ts.any fun T => decide (T.mult D = .linear)
  | T :: Ts, f + 1 => decide (T.mult D = .linear) || anyLinearOther D Ts f

/-- **§5.1's `linear-residue(S, π_s)`**: the ordered residue traversal of
`residue(S, π_s)` — "at a struct step, visit fields in declaration order,
recurse into the selected field, and retain every unselected field" — asking
whether one of the *retained* places carries a linear value. The traversal ends
at the selected leaf, which is not residue, so the empty path is `false`.

It is keyed on the retained field's **type**, not on Σ: the compiler reads the
declared type of a sibling that has itself already been moved out (probe d5c,
E0474 on a moved-out linear sibling), and `3.8:60` states the check
"recursively through nested fields" of the declared-linear place, which is this
recursion.

**The array step** is §5.1's own clause, "at an array step, visit elements in
ascending constant-index order, recurse into the selected element, and retain
every unselected element", and it is the same sentence as the struct step's
with `n` copies of the element type in place of the declaration's fields
(`List.replicate n T`, the shape `Ty.fieldAt_inv` hands every array arm in this
package). An out-of-range index retains nothing and recurses nowhere: the path
is untypeable there (`7.1:9`, E0902), so no derivation reaches it. Verified
against the compiler in both directions: probe `b9` — `x.arr[0]` on
`linear struct L4 { arr: [T0; 2], k: i64 }` — is E0474 "would implicitly drop
linear field 'array element [1]'", and probe `b3`, the same shape with an
affine element type, compiles and drops the residue in the traversal's order
(`arr[1]` before the later sibling `k`, probe `b20` at three elements and two
siblings).

This is the premise that rejects a destructure before it can silently drop a
linear sibling; the compiler reports E0474. -/
def linearResidue (D : Decls) : Ty → List Nat → Bool
  | _, [] => false
  | T, f :: π =>
      match T with
      | .struct s =>
          (match D.structs[s]? with
           | some sd =>
               anyLinearOther D sd.fields f ||
                 (match sd.fields[f]? with
                  | some Tf => linearResidue D Tf π
                  | none => false)
           | none => false)
      | .array Te n =>
          anyLinearOther D (List.replicate n Te) f ||
            (match (List.replicate n Te)[f]? with
             | some Tf => linearResidue D Tf π
             | none => false)
      | .int _ _ | .float _ | .bool | .unit | .enum _ => false

/-! ## Operators -/

/-- §2's binary operator sets: the arithmetic and bitwise `⊕`
(`+ - * / %`, `& | ^`, `<< >>`) typed by (Arith) §5.8, the ordering
compares `⋚` (`< > <= >=`) typed by (Ord) and by (Float-Ord) §5.8, and
`@total_cmp`. Equality `≟` is not here: it *borrows* its operands
(`4.3:3f`), and the fragment has no loans.

`@total_cmp` sits here rather than among the float intrinsics below because
it is the one float intrinsic with **two operands of one type** — exactly the
shape (Arith)/(Ord) already have — so it needs no second expression form and
no second evaluation-context rule. Its surface spelling is still the
intrinsic's (`Print.lean` writes `@total_cmp(a, b)`, not an infix). -/
inductive BinOp where
  | add
  | sub
  | mul
  | div
  | rem
  | bitAnd
  | bitOr
  | bitXor
  | shl
  | shr
  | lt
  | le
  | gt
  | ge
  | totalCmp
deriving DecidableEq, Repr

/-- Whether the operator is one of §2's ordering compares `⋚`, which yield
`bool` rather than the operand type (helper). -/
def BinOp.isCompare : BinOp → Bool
  | .lt | .le | .gt | .ge => true
  | _ => false

/-- Which operators §5.8's (Arith)/(Ord) admit at an integer type: everything
except `@total_cmp`, whose operands `3.12:31` makes floats (helper). -/
def BinOp.intAdmits : BinOp → Bool
  | .totalCmp => false
  | _ => true

/-- Which operators §5.8 admits at a float type: the four arithmetic
operators of (Float-Arith), the four ordering compares of (Float-Ord), and
`@total_cmp` (Total-Cmp). `%` is excluded by (Float-Arith)'s omission of it
(`3.12:25`), and the bitwise and shift operators by (Arith)/(BitNot) being
stated only at `int(w,s)` — a float is a datum rather than a bit pattern
(§2). §5.8 calls this rejection "by the absence of a rule"; here it is a side
condition, because one `Typed` constructor stands for the two rule groups
(helper). -/
def BinOp.floatAdmits : BinOp → Bool
  | .add | .sub | .mul | .div | .lt | .le | .gt | .ge | .totalCmp => true
  | .rem | .bitAnd | .bitOr | .bitXor | .shl | .shr => false

/-- The type a binary operator concludes at, given its shared operand type:
`bool` for an ordering compare ((Ord), (Float-Ord) §5.8), `int(32, signed)`
for `@total_cmp` (`3.12:31`, (Total-Cmp) §5.8), and the operand type for
every arithmetic and bitwise operator ((Arith), (Float-Arith) §5.8)
(helper). -/
def BinOp.resultTy (op : BinOp) (T : Ty) : Ty :=
  match op with
  | .totalCmp => .int .w32 .signed
  | _ => if op.isCompare then .bool else T

/-- §2's unary operator set `⊖`: `neg`, `not`, `bitnot`, typed by (Neg),
(Float-Neg), (Not) and (BitNot) §5.8. `neg` is the one of the three that
spans both scalar kinds: (Neg) restricts the integer case to a *signed* type
and traps on `min_T`, while (Float-Neg) applies at every float type and
§6.4 makes it total — a sign flip, on `-0.0` and on a NaN alike
(`3.12:24`, `4.2:14`). -/
inductive UnOp where
  | neg
  | not
  | bitnot
deriving DecidableEq, Repr

/-! ## The float intrinsics `@f` -/

/-- §2's `@f` production, minus `@total_cmp` (which is a `BinOp`, above):
the one-operand float intrinsics of §5.8's (Int-To-Float), (Float-To-Int),
(Float-Cast) and (Float-Round). Each takes its **result** type from context
(`3.12:16`, `3.12:17`, `3.12:19`), which elaboration has already resolved, so
the form carries it; the operand's own type comes from the operand. -/
inductive FloatIntrin where
  /-- `@int_to_float(e)` at `float(w)`: the operand is an integer of any width
  and signedness (`3.12:16`, `4.13:139`). -/
  | intToFloat (w : FloatWidth)
  /-- `@float_to_int(e)` at `int(w', s')`, signed or unsigned (`3.12:17`,
  `4.13:140`). The one float form whose dynamics can trap (`3.12:18`). -/
  | floatToInt (w : IntWidth) (s : Sign)
  /-- `@float_cast(e)` at `float(w')`, `w' ≠ w` (`3.12:19`, `4.13:141`). -/
  | floatCast (w : FloatWidth)
  /-- `@sqrt`, `@floor`, `@ceil`, `@trunc`, `@round` (`3.12:34`), each at the
  operand's own type. -/
  | roundOp (k : FloatUnIntrin)
deriving DecidableEq, Repr

/-- The type §5.8's rule concludes at, given the *operand*'s float width —
which is the only thing the form does not carry (helper). -/
def FloatIntrin.resTy : FloatIntrin → FloatWidth → Ty
  | .intToFloat w, _ => .float w
  | .floatToInt w s, _ => .int w s
  | .floatCast w', _ => .float w'
  | .roundOp _, w => .float w

/-- Whether the intrinsic's rule applies to a `float(w)` operand:
`@int_to_float` takes an *integer* operand and so has its own rule, and
(Float-Cast) carries `w' ≠ w` (`3.12:19`: `@float_cast` converts between the
two widths and only between them) (helper). -/
def FloatIntrin.floatSrc : FloatIntrin → FloatWidth → Bool
  | .intToFloat _, _ => false
  | .floatCast w', w => w' != w
  | .floatToInt _ _, _ => true
  | .roundOp _, _ => true

/-! ## Expressions -/

/-- Expressions (§2, fragment). `use p` is the `e ::= p` production — a place
appearing in value context, i.e. a *use* (§4.2), which at a projection is the
partial move of `3.8:22`. `drop p` is `@drop(p)` and `assign p e` is
`assign p = e`, both at a place too (§5.2, §5.3). `letIn` carries the binding's
`μ ∈ {∅, mut}` mark, which is what makes an assignment's root mutable.
`mkStruct s args` is §2's `S { f1: e1, …, fk: ek }`, presented in declaration
order (`3.6:15`) with one initializer per field. `call f args` is §2's
`g(a1, …, am)` with every argument by value (§6.9's by-reference modes are not
in the fragment), `f` the callee's index in the `Program`. `ret e` is §2's
`return e`.

`intLit w s n` carries the type elaboration resolved for it (`4.1:2`);
`floatLit w l` carries the width the same way (`3.12:7`) and holds the
literal's **decimal** rather than its datum, because `3.12:9` makes the value
the correctly-rounded reading of that decimal — which is the model's
(`Float.lean`). `fintrin k e` is §2's one-operand `@f` production.
`binop`/`unop` are §2's `e1 ⊕ e2` / `e1 ⋚ e2` and `⊖ e`. `intCast w s e` is
`@intCast(e)` with the target type elaboration took from the use site
(`4.13:26`). `panic msg` is `@panic(s)` at a string-literal message: the
fragment has no string type, so the message is a field of the form rather than
an operand expression, which is also why no `(Panic-Operand)` case is needed.
`dbg e` is `@dbg(e)`.

`mkEnum e k args` is §2's `E::Kj(e1, …, e_{aj})`, the introduction form
(Enum-Intro) §5.5 types: the enum's index, the variant's **0-based tag** (the
`Kj` of §6.1's value form, which is the variant's declaration slot) and one
payload argument per declared component, presented left to right. `match scrut
arms` is §2's `match e0 { pat1 => e1, … }` in the canonical form §5.5 fixes:
**exactly one arm per variant, in declaration order**, so the patterns are not
represented at all — arm `j` is the arm for variant `j`, and the `a_j` payload
locals it binds are de Bruijn binders of its body, bound the way `letIn` binds
its one binder (payload component 1 outermost, component `a_j` innermost, which
is `fnCtx`'s order for a parameter list). No wildcard, no guard, no ordering:
§5.5 makes each of those an elaboration obligation. Lean spells the
constructor `«match»` because `match` is one of its own keywords.

`mkArray T args` is §2's `[ e1, …, en ]`, typed by (Array-Intro) §5.8; it
carries the element type because `n = 0` leaves no element to read one off
(`[]` is the zero-sized `[T; 0]`, and *which* `T` is elaboration's answer, the
same way `intLit` carries the width `4.1:2` resolved). `repeatArray T e n` is
the surface's repeat form `[e; n]` (`7.1:36`–`7.1:39`). `indexRead p e` and
`indexWrite p e₁ e₂` are the **dynamic**-index read `p[e]` and write
`p[e₁] = e₂`: a constant index is a step of the place (`Place.idx`), so these
two forms exist for the index §5's `Path` cannot track — §4.2's
`Untrackable(OrdinaryDynamic)` plan for the read, restricted to
`class(T) = Copy` by §5.1's only successful rule for it, and (Assign) §5.2's
linear-overwrite premise for the write — both bounds-checked at run time by
§6.5's (D-Index)/(D-Index-Trap). -/
inductive Expr where
  | intLit (w : IntWidth) (s : Sign) (n : Int)
  | floatLit (w : FloatWidth) (l : FloatLit)
  | boolLit (b : Bool)
  | unitLit
  | use (p : Place)
  | binop (op : BinOp) (e₁ e₂ : Expr)
  | unop (op : UnOp) (e : Expr)
  | intCast (w : IntWidth) (s : Sign) (e : Expr)
  | fintrin (k : FloatIntrin) (e : Expr)
  | panic (msg : String)
  | dbg (e : Expr)
  | mkStruct (s : Nat) (args : List Expr)
  | mkEnum (e : Nat) (k : Nat) (args : List Expr)
  | «match» (scrut : Expr) (arms : List Expr)
  | mkArray (elem : Ty) (args : List Expr)
  | repeatArray (elem : Ty) (e : Expr) (n : Nat)
  | indexRead (p : Place) (idx : List Expr) (πs : List (List Nat))
  | indexWrite (p : Place) (idx : List Expr) (πs : List (List Nat)) (e : Expr)
  | drop (p : Place)
  | letIn (m : Bool) (e₁ e₂ : Expr)
  | assign (p : Place) (e : Expr)
  | seq (e₁ e₂ : Expr)
  | ite (c e₁ e₂ : Expr)
  | call (f : Nat) (args : List Expr)
  | ret (e : Expr)
deriving Repr

/-- A by-value parameter (§5.8's `mi = ∅` mode): its declared type and its `μ`
mark, which is what lets a body assign to it (§5.2). `borrow`/`inout`
parameters, which the caller owns and which owe no drop (`3.8:62`, §6.9), are
not in the fragment. -/
structure Param where
  ty : Ty
  mu : Bool
deriving DecidableEq, Repr

/-- A function definition: §5.8's `fn g(m1 x1:T1, …, mm xm:Tm) -> Tr { e_body }`
with every mode by value. Parameters are listed left to right, as the
signature writes them. -/
structure FnDef where
  params : List Param
  ret : Ty
  body : Expr
deriving Repr

/-- A program: the declaration environment §2's `S` and `E` — and with them
§5.8's (Struct-Intro), §5.5's (Enum-Intro) and (Match) — look a declaration up
in, and the top-level function environment §5.8's (Call) looks a callee up in,
each indexed the way the syntax names it. Function index `0` is the entry
point, which `Dynamics.run` calls with no arguments. -/
structure Program where
  /-- The type declarations: structs indexed by `Ty.struct`/`Expr.mkStruct`,
  enums by `Ty.enum`/`Expr.mkEnum`. -/
  decls : Decls
  /-- The function definitions, indexed by `Expr.call`; `0` is the entry
  point. -/
  fns : List FnDef

/-- A one-function program over a declaration environment: the entry point,
with no parameters and declared return type `T`, whose body is `e`. This is the
shape of every fragment program that calls nothing, which is how the pre-call
corpus cases are read as programs (helper). -/
def Program.entry (D : Decls) (T : Ty) (e : Expr) : Program :=
  { decls := D, fns := [{ params := [], ret := T, body := e }] }

end RueCore
