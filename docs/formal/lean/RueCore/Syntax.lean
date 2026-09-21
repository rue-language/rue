/-!
# RueCore.Syntax — abstract syntax of the spike fragment

A fragment of the Rue core calculus (`docs/formal/01-core-calculus.md` §2–§3),
scoped for the mechanization spike:

* Types: `int(w, s)` at every width `w ∈ {8, 16, 32, 64}` and both
  signednesses, `bool`, `unit`, and monomorphic struct types naming a
  declaration of the program's struct environment. Floats are in the core
  (§5.8, §6.4) and are the next slice's; enums and
  arrays (and with them `match`, indexing, and the element-wise `3.8:73`
  forms) are out of the spike and tracked in the project outline; so are
  projections and partial moves, which are the paths RUE-2231 brings.
* Expressions: literals carrying their resolved type (`4.1:2`), place use
  (§4.2), the whole §2 `⊕`/`⋚` integer operator set and the `⊖` unary set
  (§5.8 with the §6.4 trap dynamics), `@intCast` (`4.13:24`–`4.13:28`),
  `@panic` and `@dbg` (§5.8's (Panic) and (Dbg), §6.12), struct literals
  (§5.8's (Struct-Intro)), the fragment's whole-value struct elimination,
  `@drop` (§5.3), `let` (§5.6 scope exit), assignment with reinitialization
  (§5.2), sequencing with the discard check (§5.3), `if` with the branch join
  (§5.5), by-value calls (§5.8's (Call), §6.9), and `return` (§5.7's
  (Return-Value), §6.9).
* Variables are de Bruijn indices: the calculus reaches the core only through
  elaboration, and name resolution is elaboration's job.
* Functions and struct declarations are likewise named by their index in the
  program, the same way bindings are named by their de Bruijn index:
  elaboration resolves the name and the field order (`3.6:15`).

No borrows/loans (Λ is ambiently empty in the current core anyway — §5
preamble), no by-reference parameters, no loops, no accessor calls: those are
the next milestones, not this slice's scope.

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
  cases w <;> cases s <;>
    simp only [valOf, InBounds, intMin, intMax, IntWidth.modulus, IntWidth.bits] at hmod ⊢ <;>
    first
      | omega
      | (split <;> omega)

/-- The same for `wrapInt` (helper). -/
theorem wrapInt_inBounds (w : IntWidth) (s : Sign) (n : Int) : InBounds w s (wrapInt w s n) :=
  valOf_inBounds w s _

/-! ## Types -/

/-- Types (§2, fragment). `int w s` is §2's `int(w, s)`; `struct s` names the
declaration at index `s` of the program's struct environment, which
elaboration resolves the surface name to. -/
inductive Ty where
  | int (w : IntWidth) (s : Sign)
  | bool
  | unit
  | struct (s : Nat)
deriving DecidableEq, Repr

/-- Whether a type is an integer type (§2's `int(w, s)`) (helper). -/
def Ty.isInt : Ty → Bool
  | .int _ _ => true
  | .bool | .unit | .struct _ => false

/-- Whether a type is one `@dbg` renders, which §5.8's (Dbg) restricts to
`int(w,s)`, `float(w)` and `bool` — the compiler's own restriction (E0702).
The fragment has no floats, so its two cases are the integers and `bool`
(helper). -/
def Ty.observable : Ty → Bool
  | .int _ _ | .bool => true
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
`Copy` at every width and signedness; a struct type has the class its
declaration records. -/
def Ty.mult (D : StructEnv) : Ty → Mult
  | .int _ _ | .bool | .unit => .copy
  | .struct s => D.classOf s

/-- `carries_linear(T)` (§5.3): `class(T) = Linear`, which §5.3 states is the
same predicate as "Linear lifted through the aggregates" because `class` *is*
that join (§3). `struct_carriesLinear_iff` (`Statics.lean`) is the lifting,
proved through the field join. -/
abbrev Ty.carriesLinear (D : StructEnv) (T : Ty) : Prop := T.mult D = .linear

/-! ## Operators -/

/-- §2's binary operator sets on integers: the arithmetic and bitwise `⊕`
(`+ - * / %`, `& | ^`, `<< >>`) typed by (Arith) §5.8, and the ordering
compares `⋚` (`< > <= >=`) typed by (Ord) §5.8. Equality `≟` is not here: it
*borrows* its operands (`4.3:3f`), and the fragment has no loans. -/
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
deriving DecidableEq, Repr

/-- Whether the operator is one of §2's ordering compares `⋚`, which yield
`bool` rather than the operand type (helper). -/
def BinOp.isCompare : BinOp → Bool
  | .lt | .le | .gt | .ge => true
  | _ => false

/-- The type a binary operator concludes at, given its shared operand type:
`bool` for an ordering compare ((Ord) §5.8), the operand type for every
arithmetic and bitwise operator ((Arith) §5.8) (helper). -/
def BinOp.resultTy (op : BinOp) (T : Ty) : Ty := if op.isCompare then .bool else T

/-- §2's unary operator set `⊖`: `neg`, `not`, `bitnot`, typed by (Neg),
(Not) and (BitNot) §5.8. -/
inductive UnOp where
  | neg
  | not
  | bitnot
deriving DecidableEq, Repr

/-! ## Expressions -/

/-- Expressions (§2, fragment). `use i` is the `e ::= p` production — a place
(here: a whole binding) appearing in value context, i.e. a *use* (§4.2).
`drop i` is `@drop(p)`. `letIn` carries the binding's `μ ∈ {∅, mut}` mark.
`mkStruct s args` is §2's `S { f1: e1, …, fk: ek }`, presented in declaration
order (`3.6:15`) with one initializer per field. `call f args` is §2's
`g(a1, …, am)` with every argument by value (§6.9's by-reference modes are not
in the fragment), `f` the callee's index in the `Program`. `ret e` is §2's
`return e`.

`intLit w s n` carries the type elaboration resolved for it (`4.1:2`).
`binop`/`unop` are §2's `e1 ⊕ e2` / `e1 ⋚ e2` and `⊖ e`. `intCast w s e` is
`@intCast(e)` with the target type elaboration took from the use site
(`4.13:26`). `panic msg` is `@panic(s)` at a string-literal message: the
fragment has no string type, so the message is a field of the form rather than
an operand expression, which is also why no `(Panic-Operand)` case is needed.
`dbg e` is `@dbg(e)`. -/
inductive Expr where
  | intLit (w : IntWidth) (s : Sign) (n : Int)
  | boolLit (b : Bool)
  | unitLit
  | use (i : Nat)
  | binop (op : BinOp) (e₁ e₂ : Expr)
  | unop (op : UnOp) (e : Expr)
  | intCast (w : IntWidth) (s : Sign) (e : Expr)
  | panic (msg : String)
  | dbg (e : Expr)
  | mkStruct (s : Nat) (args : List Expr)
  | consume (e : Expr)
  | drop (i : Nat)
  | letIn (m : Bool) (e₁ e₂ : Expr)
  | assign (i : Nat) (e : Expr)
  | seq (e₁ e₂ : Expr)
  | ite (c e₁ e₂ : Expr)
  | call (f : Nat) (args : List Expr)
  | ret (e : Expr)
deriving Repr

/-- The fragment's whole-value struct elimination, as a side condition on a
declaration: the struct has at least one field, every field is an integer
type, and it declares no destructor. `Expr.consume` reads the first field's
payload and **destroys the value without running its drop glue**
(`Dynamics.lean`'s `.consume` arm calls no `dropValue`), so each clause keeps
that honest: a field of any other type would be discarded with its own drop
glue unrun, and a declaration with a destructor would lose the `dtor` event
§6.11 owes — while the printed program's consumer lets its by-value parameter
drop at the function's end, so that destructor *would* print and the two views
would disagree by a line. `3.9:34` is not the reason and does not forbid the
read: it forbids *moving* a field out and permits borrowing one, and the
compiler accepts `fn consume_S(s: S) -> i64 { s.x0 }` on a destructor-bearing
`S`.

This is **not** a calculus rule — the calculus eliminates a struct through a
projection, which is RUE-2231 — so the restriction is the fragment's, stated
here rather than cited. -/
def StructDecl.Consumable (sd : StructDecl) : Prop :=
  sd.fields ≠ [] ∧ (∀ T ∈ sd.fields, T.isInt = true) ∧ sd.dtor = false

instance (sd : StructDecl) : Decidable sd.Consumable := by
  unfold StructDecl.Consumable; infer_instance

/-- The first field's type of a `Consumable` declaration, which is the type
`Expr.consume` yields. The default is `int(64, signed)`, which no `Consumable`
declaration reaches — its field list is non-empty by definition (helper). -/
def StructDecl.payloadTy (sd : StructDecl) : Ty :=
  match sd.fields with
  | T :: _ => T
  | [] => .int .w64 .signed

/-- The payload type is the first field's, whenever there is one — which is
what makes `Typed.consume`'s conclusion readable off the declaration
(helper). -/
theorem StructDecl.payloadTy_of_head {sd : StructDecl} {T : Ty} (h : sd.fields.head? = some T) :
    sd.payloadTy = T := by
  simp only [StructDecl.payloadTy]
  split
  · next T' rest hf => rw [hf] at h; simp only [List.head?_cons, Option.some_inj] at h; exact h
  · next hf => rw [hf] at h; cases h

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
