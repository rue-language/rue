/-!
# RueCore.Syntax — abstract syntax of the spike fragment

A fragment of the Rue core calculus (`docs/formal/01-core-calculus.md` §2–§3),
scoped for the mechanization spike:

* Types: `int` (modeled as `int(64, signed)`), `bool`, `unit`, and monomorphic
  struct types naming a declaration of the program's struct environment.
  Enums and arrays (and with them `match`, indexing, and the element-wise
  `3.8:73` forms) are out of the spike and tracked in the project outline; so
  are projections and partial moves, which are the paths RUE-2231 brings.
* Expressions: literals, place use (§4.2), `+`/`/`/`<` primitives (§5.8 with
  the §6.4 trap dynamics), struct literals (§5.8's (Struct-Intro)), the
  fragment's whole-value struct elimination, `@drop` (§5.3), `let` (§5.6
  scope exit), assignment with reinitialization (§5.2), sequencing with the
  discard check (§5.3), `if` with the branch join (§5.5), by-value calls
  (§5.8's (Call), §6.9), and `return` (§5.7's (Return-Value), §6.9).
* Variables are de Bruijn indices: the calculus reaches the core only through
  elaboration, and name resolution is elaboration's job.
* Functions and struct declarations are likewise named by their index in the
  program, the same way bindings are named by their de Bruijn index:
  elaboration resolves the name and the field order (`3.6:15`).

No borrows/loans (Λ is ambiently empty in the current core anyway — §5
preamble), no by-reference parameters, no loops, no accessor calls: those are
the next milestones, not this slice's scope.

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

/-- Types (§2, fragment). `struct s` names the declaration at index `s` of the
program's struct environment; elaboration resolves the surface name. -/
inductive Ty where
  | int
  | bool
  | unit
  | struct (s : Nat)
deriving DecidableEq, Repr

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
`Copy`; a struct type has the class its declaration records. -/
def Ty.mult (D : StructEnv) : Ty → Mult
  | .int | .bool | .unit => .copy
  | .struct s => D.classOf s

/-- `carries_linear(T)` (§5.3): `class(T) = Linear`, which §5.3 states is the
same predicate as "Linear lifted through the aggregates" because `class` *is*
that join (§3). `struct_carriesLinear_iff` (`Statics.lean`) is the lifting,
proved through the field join. -/
abbrev Ty.carriesLinear (D : StructEnv) (T : Ty) : Prop := T.mult D = .linear

/-- `int(64, signed)` bounds. Arithmetic traps outside them (§6.4). -/
def intMin : Int := -(2 ^ 63)
/-- The upper `int(64, signed)` bound (§6.4). -/
def intMax : Int := 2 ^ 63 - 1

/-- `min_T ≤ n ≤ max_T` for `T = int(64, signed)`: the side condition of
§6.1's `n_T` values, outside which §6.4's arithmetic traps. -/
def InBounds (n : Int) : Prop := intMin ≤ n ∧ n ≤ intMax

instance (n : Int) : Decidable (InBounds n) := by
  unfold InBounds; infer_instance

/-- Expressions (§2, fragment). `use i` is the `e ::= p` production — a place
(here: a whole binding) appearing in value context, i.e. a *use* (§4.2).
`drop i` is `@drop(p)`. `letIn` carries the binding's `μ ∈ {∅, mut}` mark.
`mkStruct s args` is §2's `S { f1: e1, …, fk: ek }`, presented in declaration
order (`3.6:15`) with one initializer per field. `call f args` is §2's
`g(a1, …, am)` with every argument by value (§6.9's by-reference modes are not
in the fragment), `f` the callee's index in the `Program`. `ret e` is §2's
`return e`. -/
inductive Expr where
  | intLit (n : Int)
  | boolLit (b : Bool)
  | unitLit
  | use (i : Nat)
  | add (e₁ e₂ : Expr)
  | div (e₁ e₂ : Expr)
  | lt (e₁ e₂ : Expr)
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
declaration: the struct has at least one field, every field is `int`, and it
declares no destructor. `Expr.consume` reads the first field's payload and
**destroys the value without running its drop glue** (`Dynamics.lean`'s
`.consume` arm calls no `dropValue`), so each clause keeps that honest: a
field of any other type would be discarded with its own drop glue unrun, and a
declaration with a destructor would lose the `dtor` event §6.11 owes — while
the printed program's consumer lets its by-value parameter drop at the
function's end, so that destructor *would* print and the two views would
disagree by a line. `3.9:34` is not the reason and does not forbid the read:
it forbids *moving* a field out and permits borrowing one, and the compiler
accepts `fn consume_S(s: S) -> i64 { s.x0 }` on a destructor-bearing `S`.

This is **not** a calculus rule — the calculus eliminates a struct through a
projection, which is RUE-2231 — so the restriction is the fragment's, stated
here rather than cited. -/
def StructDecl.Consumable (sd : StructDecl) : Prop :=
  sd.fields ≠ [] ∧ (∀ T ∈ sd.fields, T = .int) ∧ sd.dtor = false

instance (sd : StructDecl) : Decidable sd.Consumable := by
  unfold StructDecl.Consumable; infer_instance

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
