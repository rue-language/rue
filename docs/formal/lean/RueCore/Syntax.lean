import RueCore.Float

/-!
# RueCore.Syntax — abstract syntax of the spike fragment

A fragment of the Rue core calculus (`docs/formal/01-core-calculus.md` §2–§3),
scoped for the mechanization spike:

* Types: `int(w, s)` at every width `w ∈ {8, 16, 32, 64}` and both
  signednesses, `float(w)` at both widths (§2's `𝔽_w`, mechanized in
  `Float.lean`), `bool`, `unit`, and monomorphic struct types naming a
  declaration of the program's struct environment. Enums and
  arrays (and with them `match`, indexing, and the element-wise `3.8:73`
  forms) are out of the spike and tracked in the project outline.
* Places: §5's `Path ::= x | Path.f` — a root binding and a chain of field
  projections (`Place`). Array index steps `Path[c]` are not in the fragment
  (RUE-2235), so a place's every step is a field.
* Expressions: literals carrying their resolved type (`4.1:2`), place use
  (§4.2), the whole §2 `⊕`/`⋚` integer operator set and the `⊖` unary set
  (§5.8 with the §6.4 trap dynamics), `@intCast` (`4.13:24`–`4.13:28`),
  `@panic` and `@dbg` (§5.8's (Panic) and (Dbg), §6.12), float literals and
  the float intrinsics (§5.8's (Float-Arith), (Float-Neg), (Float-Ord),
  (Total-Cmp), (Int-To-Float), (Float-To-Int), (Float-Cast), (Float-Round),
  with the §6.4 dynamics), struct literals
  (§5.8's (Struct-Intro)) and the projection that eliminates one,
  `@drop` (§5.3), `let` (§5.6 scope exit), assignment at a place with
  reinitialization (§5.2), sequencing with the discard check (§5.3), `if` with
  the branch join (§5.5), by-value calls (§5.8's (Call), §6.9), and `return`
  (§5.7's (Return-Value), §6.9).
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
mechanizes one of them. `Ordinary` — the partial move of `3.8:22` — is
(Use-Copy)/(Use-Move) here. `Untrackable` needs an array index and so has no
instance without arrays (RUE-2235). `Declared(d, π)`, the declared-linear
destructure of `3.8:33`, needs `(Use-Declared-Linear-Destructure)` §5.1 and
its residue traversal, which is RUE-2236; until then a place whose path has a
**proper prefix** of declared-`linear` struct type is rejected here
(`noLinearPrefix`), as a stated restriction of the fragment rather than as a
rule of the calculus. A use of such a place *whole* is ordinary (the plan looks
for a proper prefix), so a declared-linear struct is still moved, dropped and
reinitialized by the rules below.

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

## Where a struct's class lives

§3 fixes `class(S)` as the join of the field classes lifted by the declared
attribute. A declaration *records* that class, and the program well-formedness
judgment `WfStructs` (`Statics.lean`) is §3's equation: the recorded class is
the lifted join, a `@copy` declaration's fields are all `Copy` and it declares
no destructor (`3.8:18`, `3.9:31`), and a field may name only an earlier
declaration. Recording it is what lets `Ty.mult` be a lookup rather than a
recursion over the environment, and `struct_class_unique` (`Statics.lean`) is
the proof that the record is determined rather than free: on an environment
whose fields name only earlier declarations, at most one assignment of classes
satisfies §3's equation.

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
declaration at index `s` of the program's struct environment, which
elaboration resolves the surface name to. -/
inductive Ty where
  | int (w : IntWidth) (s : Sign)
  | float (w : FloatWidth)
  | bool
  | unit
  | struct (s : Nat)
deriving DecidableEq, Repr

/-- Whether a type is an integer type (§2's `int(w, s)`) (helper). -/
def Ty.isInt : Ty → Bool
  | .int _ _ => true
  | .float _ | .bool | .unit | .struct _ => false

/-- Whether a type is one `@dbg` renders, which §5.8's (Dbg) restricts to
`int(w,s)`, `float(w)` and `bool` — the compiler's own restriction (E0702).
The `float(w)` case is `3.12:39`, and the text it produces is `3.12:40`–
`3.12:42` (`FloatDatum.render`, `Float.lean`) (helper). -/
def Ty.observable : Ty → Bool
  | .int _ _ | .float _ | .bool => true
  | .unit | .struct _ => false

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

/-- The program's struct environment: the declarations §2's type production
`S` names, indexed the way `Ty.struct` names them. -/
abbrev StructEnv := List StructDecl

/-- `class(S)` for a declared struct type (§3), read off the declaration. An
index the environment does not have is `Affine`, the class of a struct with no
attribute and no linear field — the conservative reading of a program
`WfStructs` rejects anyway (helper). -/
def StructEnv.classOf (D : StructEnv) (s : Nat) : Mult :=
  match D[s]? with
  | some sd => sd.cls
  | none => .affine

/-- `class(T)` (§3), against the program's struct environment. Scalars are
`Copy` at every width and signedness, floats included (`3.12:2a` classifies
both float types `Copy` and `3.8:2` lists them, so the core takes it
directly); a struct type has the class its declaration records. -/
def Ty.mult (D : StructEnv) : Ty → Mult
  | .int _ _ | .float _ | .bool | .unit => .copy
  | .struct s => D.classOf s

/-- `carries_linear(T)` (§5.3): `class(T) = Linear`, which §5.3 states is the
same predicate as "Linear lifted through the aggregates" because `class` *is*
that join (§3). `struct_carriesLinear_iff` (`Statics.lean`) is the lifting,
proved through the field join. -/
abbrev Ty.carriesLinear (D : StructEnv) (T : Ty) : Prop := T.mult D = .linear

/-! ## Places: §5's `Path`, and the type a path reaches -/

/-- A place (§5's `Path ::= x | Path.f`, and §2's `p` production): a root
binding named by its de Bruijn index, under a chain of field projections named
by their declaration slot (`3.6:15` — elaboration resolves the surface field
name to the slot). `Path[c]`, the array-element step, is not in the fragment
(RUE-2235), so `3.8:68`'s root-index restriction has no instance here. -/
inductive Place where
  | var (i : Nat)
  | proj (p : Place) (f : Nat)
deriving DecidableEq, Repr

/-- The binding a place is rooted at (§5's `root(p)`) (helper). -/
def Place.root : Place → Nat
  | .var i => i
  | .proj p _ => p.root

/-- The projection steps of a place, from the root outward — the `π` §6.3
navigates a stored aggregate with (helper). -/
def Place.path : Place → List Nat
  | .var _ => []
  | .proj p f => p.path ++ [f]

/-- The type of a declaration's field at a slot, or `none` when the type is
not a struct or the slot is not a field (helper). -/
def Ty.fieldAt (D : StructEnv) : Ty → Nat → Option Ty
  | .struct s, f =>
      match D[s]? with
      | some sd => sd.fields[f]?
      | none => none
  | .int _ _, _ | .float _, _ | .bool, _ | .unit, _ => none

/-- `Γ ⊢ p : T` for a path read off the root's declared type: follow the field
slots, failing where a step is not a field of the type reached so far. Types
are not flow-sensitive, so this is the whole of the place's typing (§5
preamble: `Γ` is fixed at the binder). -/
def Ty.atPath (D : StructEnv) : Ty → List Nat → Option Ty
  | T, [] => some T
  | T, f :: π =>
      match T.fieldAt D f with
      | some T' => T'.atPath D π
      | none => none

/-- No **proper prefix** of the path names a value whose type declares a
destructor: (Use-Move) §5.1's and (@Drop) §5.3's `3.9:34` premise (E0456).
Moving or dropping the whole value is fine — the empty path has no proper
prefix — because the restriction exists so that a destructor never observes a
hole in the value it runs on. -/
def noDtorPrefix (D : StructEnv) : Ty → List Nat → Bool
  | _, [] => true
  | T, f :: π =>
      match T with
      | .struct s =>
          (match D[s]? with
           | some sd =>
               !sd.dtor &&
                 (match sd.fields[f]? with
                  | some T' => noDtorPrefix D T' π
                  | none => true)
           | none => true)
      | .int _ _ | .float _ | .bool | .unit => true

/-- No **proper prefix** of the path is a struct declared `linear`. This is not
a premise of any §5 rule: it is the fragment's own restriction, standing in for
the `Declared(d, π)` use plan §4.2 selects for such a path and
(Use-Declared-Linear-Destructure) §5.1 discharges (RUE-2236, module
docstring). -/
def noLinearPrefix (D : StructEnv) : Ty → List Nat → Bool
  | _, [] => true
  | T, f :: π =>
      match T with
      | .struct s =>
          (match D[s]? with
           | some sd =>
               decide (sd.attr ≠ .linear) &&
                 (match sd.fields[f]? with
                  | some T' => noLinearPrefix D T' π
                  | none => true)
           | none => true)
      | .int _ _ | .float _ | .bool | .unit => true

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
`dbg e` is `@dbg(e)`. -/
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

/-- A program: the struct environment §2's `S` and §5.8's (Struct-Intro) look
a declaration up in, and the top-level function environment §5.8's (Call)
looks a callee up in, each indexed the way the syntax names it. Function index
`0` is the entry point, which `Dynamics.run` calls with no arguments. -/
structure Program where
  /-- The struct declarations, indexed by `Ty.struct` and `Expr.mkStruct`. -/
  structs : StructEnv
  /-- The function definitions, indexed by `Expr.call`; `0` is the entry
  point. -/
  fns : List FnDef

/-- A one-function program over a struct environment: the entry point, with no
parameters and declared return type `T`, whose body is `e`. This is the shape
of every fragment program that calls nothing, which is how the pre-call corpus
cases are read as programs (helper). -/
def Program.entry (D : StructEnv) (T : Ty) (e : Expr) : Program :=
  { structs := D, fns := [{ params := [], ret := T, body := e }] }

end RueCore
