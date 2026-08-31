/-!
# RueCore.Syntax — abstract syntax of the spike fragment

A fragment of the Rue core calculus (`docs/formal/01-core-calculus.md` §2–§3),
scoped for the mechanization spike:

* Types: `int` (modeled as `int(64, signed)`), `bool`, `unit`, and an abstract
  resource type `res κ` standing in for a monomorphic struct whose multiplicity
  class is `κ`. Structs-with-fields, enums, and arrays (and with them paths,
  partial moves, and the declared-linear destructure) are out of the spike and
  tracked in the project outline.
* Expressions: literals, place use (§4.2), `+`/`/`/`<` primitives (§5.8 with
  the §6.4 trap dynamics), resource intro/elim, `@drop` (§5.3), `let` (§5.6
  scope exit), assignment with reinitialization (§5.2), sequencing with the
  discard check (§5.3), and `if` with the branch join (§5.5).
* Variables are de Bruijn indices: the calculus reaches the core only through
  elaboration, and name resolution is elaboration's job.

No borrows/loans (Λ is ambiently empty in the current core anyway — §5
preamble), no calls, no loops: those are the next milestones, not spike scope.
-/

namespace RueCore

/-- The multiplicity lattice (§3): `Copy ⊑ Affine ⊑ Linear`. -/
inductive Mult where
  | copy
  | affine
  | linear
deriving DecidableEq, Repr

/-- Types (§2, fragment). `res κ` is an abstract non-scalar type of class `κ`
carrying an integer payload — the smallest stand-in for a monomorphic struct
that exercises the Affine/Linear discipline. -/
inductive Ty where
  | int
  | bool
  | unit
  | res (κ : Mult)
deriving DecidableEq, Repr

/-- `class(T)` (§3). Scalars are `Copy`; a resource has its declared class. -/
def Ty.mult : Ty → Mult
  | .int | .bool | .unit => .copy
  | .res κ => κ

/-- In the fragment, `carries_linear(T) ⟺ class(T) = Linear` (§5.3 note). -/
abbrev Ty.carriesLinear (T : Ty) : Prop := T.mult = .linear

/-- `int(64, signed)` bounds. Arithmetic traps outside them (§6.4). -/
def intMin : Int := -(2 ^ 63)
def intMax : Int := 2 ^ 63 - 1

def InBounds (n : Int) : Prop := intMin ≤ n ∧ n ≤ intMax

instance (n : Int) : Decidable (InBounds n) := by
  unfold InBounds; infer_instance

/-- Expressions (§2, fragment). `use i` is the `e ::= p` production — a place
(here: a whole binding) appearing in value context, i.e. a *use* (§4.2).
`drop i` is `@drop(p)`. `letIn` carries the binding's `μ ∈ {∅, mut}` mark. -/
inductive Expr where
  | intLit (n : Int)
  | boolLit (b : Bool)
  | unitLit
  | use (i : Nat)
  | add (e₁ e₂ : Expr)
  | div (e₁ e₂ : Expr)
  | lt (e₁ e₂ : Expr)
  | mkres (κ : Mult) (e : Expr)
  | consume (e : Expr)
  | drop (i : Nat)
  | letIn (m : Bool) (e₁ e₂ : Expr)
  | assign (i : Nat) (e : Expr)
  | seq (e₁ e₂ : Expr)
  | ite (c e₁ e₂ : Expr)
deriving DecidableEq, Repr

end RueCore
