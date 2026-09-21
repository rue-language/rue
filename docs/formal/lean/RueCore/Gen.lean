import RueCore.Corpus

/-!
# RueCore.Gen — a seeded generator of fragment programs (RUE-2229)

xref: examples

The bridge corpus (`Corpus.lean`) is hand-written, so it only exercises the
shapes its authors thought of. This module generates fragment programs from a
seed, so the differential bridge (ADR-0097, decision 4) can also run programs
nobody wrote: the verification-guided-development loop of generate, run the
model and the implementations, compare. `lake exe ruecore-corpus --gen N
--seed S` appends `N` generated cases to the corpus JSON in the schema
`Corpus.lean` documents.

## What a generated program is guaranteed to be

Generation is type-directed under a scope of binders, so every program is
closed, well-scoped, and simply typed by construction: every `use i` and
`drop i` names a binder in scope, operator operands are `int`, both arms of
an `if` have the wanted type, `assign i e` targets a `mut` binder and `e` has
its type, and every literal is in bounds (`InBounds`). So `Print.tyOf`
succeeds on every generated program, and whatever the verified checker
rejects, it rejects for an ownership reason — a use after move, a linear
leak, a linear discard or overwrite, a disagreeing join — which is what the
bridge's refusal table covers.

Calls and `return` are **not** generated yet: every generated case is a
one-function program (`Program.entry`), so the shapes RUE-2233 added — a
frame unwound by an early `return`, a by-value parameter dropped at a frame
pop, recursion — are covered by the hand-written seed cases only. Generating
them needs a signature environment to draw callees from and a fuel bound that
recursion cannot escape; it is the follow-up this module's `expr` is shaped
for (`Corpus.Case` already holds a whole `Program`).

`return` also has to wait for a reason of its own. A generated case's
`reject` verdict is only as good as `check`'s completeness, and `check` is
not complete on `return`: a `return` arm of an `if` contributes its
post-operand state to §5.5's join where §5.7 excludes it, so the first
generated program with that shape would be a *false* bridge failure —
rejected here, accepted by the calculus and by the compiler
(`Checker.lean`, "what completeness costs"; `Corpus.lean`'s verdict
contract). Emitting `ret` waits on a `check` that carries the ⊥
provenance.

## What it deliberately does not guarantee

Ownership. Moves, drops, assignments and scope exits are chosen at random,
so about half of the programs are rejected by the checker and refused by the
machine. Both are recorded (`Corpus.caseJson` reads them off `checkProgram`
and `run` as for any case), never filtered: a rejected program checks that the
compiler rejects it too, an accepted one that the three implementations
agree with the interpreter's trace.

## Bias

The choices are weighted toward the shapes the safety theorems
(`Soundness.lean`) are about, all in one place (`expr`, `leaf`, `atom`) so
the weights can be read and changed:

* when a resource binder of the wanted type is in scope, using it (a move)
  is preferred to minting a fresh resource — including inside one arm of an
  `if`, which is how join disagreements arise;
* `let` binders are mostly resources and mostly `mut`, so linear values
  reach scope exit and assignments have targets;
* a sequence's discarded statement is mostly unit-typed, where `assign`
  (whose right-hand side may use the target binder itself, so
  reinitialisation after a move and overwrite of a live value both arise)
  and `@drop` live; `@drop` prefers a resource binder but may name any
  binder, since the calculus allows `@drop` of a place of any class;
* integer literals are small, `0` among them, with an occasional
  `intMax`/`intMin`, so `/` and `+` can trap.

Programs are fuel-bounded: a fuel of two or three is drawn per program and
every compound form spends one unit on its operands, so the nesting a case
reaches is a few levels and it stays readable; shrinking is out of scope.

## Determinism

The generator is a pure function of `(n, seed)` for the pinned toolchain:
`StdGen` from `Init` is threaded through a `StateM`, and nothing reads the
environment (a toolchain bump that changes `StdGen` changes every case, so
a finding filed from a generated run records the seed and quotes the
program). The first `i` cases of a run are the same for every `n > i`, so a
case named `gen_<seed>_<i>` can always be regenerated from its name alone.
-/

namespace RueCore.Gen

open Expr

/-- (helper) A binder in scope: its type and its `mut` mark. The list is
innermost first, as `Ctx` is, so a binder's position is its de Bruijn
index. -/
structure Binder where
  ty : Ty
  mu : Bool

/-- (helper) The binders in scope, innermost first. -/
abbrev Scope := List Binder

/-- (helper) The generation monad: a `StdGen` threaded through. -/
abbrev G := StateM StdGen

/-- (helper) A uniform natural number in `[lo, hi]`. -/
def nat (lo hi : Nat) : G Nat :=
  modifyGet fun g => randNat g lo hi

/-- (helper) A uniform boolean. -/
def bool : G Bool :=
  modifyGet fun g => randBool g

/-- (helper) True with probability `num / den`. -/
def chance (num den : Nat) : G Bool := do
  let k ← nat 1 den
  return k ≤ num

/-- (helper) One element of a non-empty list, uniformly; `default` for an
empty one. -/
def pick {α : Type} (default : α) (xs : List α) : G α := do
  match xs with
  | [] => return default
  | _ =>
      let i ← nat 0 (xs.length - 1)
      return xs[i]?.getD default

/-- (helper) Walk a weighted list with a number drawn in `[1, total]`. -/
def pickWeighted {α : Type} (default : α) (r : Nat) : List (Nat × α) → α
  | [] => default
  | (w, a) :: rest => if r ≤ w then a else pickWeighted default (r - w) rest

/-- (helper) One element of a weighted list; a weight of zero is never
chosen, so an unavailable form is listed with weight zero rather than
omitted. -/
def weighted {α : Type} (default : α) (choices : List (Nat × α)) : G α := do
  let total := choices.foldl (fun acc (w, _) => acc + w) 0
  if total = 0 then return default
  let r ← nat 1 total
  return pickWeighted default r choices

/-- (helper) The de Bruijn indices of the binders satisfying `p`. -/
def indicesWhere (Γ : Scope) (p : Binder → Bool) : List Nat :=
  let rec go : List Binder → Nat → List Nat
    | [], _ => []
    | b :: bs, i => if p b then i :: go bs (i + 1) else go bs (i + 1)
  go Γ 0

/-- (helper) Whether a type is a resource type. -/
def isRes : Ty → Bool
  | .res _ => true
  | _ => false

/-- (helper) An integer literal: small, with an occasional bound so that
`+` and `/` can trap (§6.4). -/
def intLiteral : G Expr := do
  let k ← nat 0 39
  return intLit (if k = 0 then intMax else if k = 1 then intMin else Int.ofNat (k % 10))

/-- (helper) The type of a fresh `let` binder: mostly resources. -/
def binderTy : G Ty :=
  weighted .int [(3, .res .linear), (3, .res .affine), (1, .res .copy), (2, .int), (1, .bool)]

/-- (helper) The smallest expression of a type: a literal, a use of a binder
of that type, or a resource with a literal payload. -/
def atom (Γ : Scope) : Ty → G Expr
  | .int => do
      let uses := indicesWhere Γ (fun b => b.ty == .int)
      if !uses.isEmpty && (← chance 1 2) then return use (← pick 0 uses)
      intLiteral
  | .bool => do
      let uses := indicesWhere Γ (fun b => b.ty == .bool)
      if !uses.isEmpty && (← chance 1 2) then return use (← pick 0 uses)
      return boolLit (← bool)
  | .unit => return unitLit
  | .res κ => do
      let uses := indicesWhere Γ (fun b => b.ty == .res κ)
      if !uses.isEmpty && (← chance 2 3) then return use (← pick 0 uses)
      return mkres κ (← intLiteral)

/-- (helper) A leaf of the wanted type, one level at most: an atom, a
`consume` of a resource binder, a `@drop`, or an assignment of an atom. -/
def leaf (Γ : Scope) : Ty → G Expr
  | .int => do
      let ress := indicesWhere Γ (fun b => isRes b.ty)
      if !ress.isEmpty && (← chance 1 2) then return consume (use (← pick 0 ress))
      atom Γ .int
  | .unit => do
      let ress := indicesWhere Γ (fun b => isRes b.ty)
      let muts := indicesWhere Γ (fun b => b.mu)
      let form ← weighted 0
        [(1, 0), (if Γ.isEmpty then 0 else 6, 1), (if muts.isEmpty then 0 else 5, 2)]
      match form with
      | 1 =>
          if !ress.isEmpty && (← chance 3 4) then return drop (← pick 0 ress)
          return drop (← nat 0 (Γ.length - 1))
      | 2 =>
          let i ← pick 0 muts
          let b := Γ[i]?.getD ⟨.int, true⟩
          return assign i (← atom Γ b.ty)
      | _ => return unitLit
  | T => atom Γ T

/-- (helper) The class of a resource to consume: that of a resource binder in
scope when there is one (so the operand is usually a move), else random. -/
def resClass (Γ : Scope) (ress : List Nat) : G Mult :=
  match ress with
  | [] => weighted Mult.affine [(1, .copy), (2, .affine), (2, .linear)]
  | _ => do
      let i ← pick 0 ress
      pure (match Γ[i]? with
        | some ⟨.res κ, _⟩ => κ
        | _ => .affine)

/-- (helper) An expression of the wanted type under `Γ`, at most `fuel`
levels deep. The weights here are the bias the module docstring
describes. -/
def expr : Scope → Ty → Nat → G Expr
  | Γ, T, 0 => leaf Γ T
  | Γ, T, fuel + 1 => do
      if !Γ.isEmpty && (← chance 1 6) then return (← leaf Γ T)
      let form ← weighted 3 [(4, 0), (3, 1), (3, 2), (4, 3)]
      match form with
      | 0 =>
          let T₁ ← binderTy
          let m ← chance 2 3
          let e₁ ← expr Γ T₁ fuel
          let e₂ ← expr ({ ty := T₁, mu := m } :: Γ) T fuel
          return letIn m e₁ e₂
      | 1 =>
          let muts := indicesWhere Γ (fun b => b.mu)
          let T₁ ← weighted .unit
            [(if muts.isEmpty then 4 else 7, .unit), (1, .res .affine), (1, .res .linear), (2, .int)]
          let e₁ ← expr Γ T₁ fuel
          let e₂ ← expr Γ T fuel
          return seq e₁ e₂
      | 2 =>
          let c ← expr Γ .bool fuel
          let e₁ ← expr Γ T fuel
          let e₂ ← expr Γ T fuel
          return ite c e₁ e₂
      | _ =>
          match T with
          | .int =>
              let ress := indicesWhere Γ (fun b => isRes b.ty)
              let op ← weighted 0 [(2, 0), (1, 1), (3, 2)]
              match op with
              | 0 => return add (← expr Γ .int fuel) (← expr Γ .int fuel)
              | 1 => return div (← expr Γ .int fuel) (← expr Γ .int fuel)
              | _ =>
                  let κ ← resClass Γ ress
                  return consume (← expr Γ (.res κ) fuel)
          | .bool => return lt (← expr Γ .int fuel) (← expr Γ .int fuel)
          | .unit =>
              let muts := indicesWhere Γ (fun b => b.mu)
              if !muts.isEmpty && (← chance 3 4) then
                let i ← pick 0 muts
                let b := Γ[i]?.getD ⟨.int, true⟩
                return assign i (← expr Γ b.ty fuel)
              if Γ.isEmpty then
                let T₁ ← weighted .int [(2, .res .affine), (2, .res .linear), (1, .int)]
                return seq (← expr Γ T₁ fuel) unitLit
              leaf Γ .unit
          | .res κ =>
              let uses := indicesWhere Γ (fun b => b.ty == .res κ)
              if !uses.isEmpty && (← chance 1 2) then return use (← pick 0 uses)
              return mkres κ (← expr Γ .int fuel)

/-- (helper) Every subexpression, the expression itself first. -/
def subexprs : Expr → List Expr
  | e@(add e₁ e₂) | e@(div e₁ e₂) | e@(lt e₁ e₂) | e@(seq e₁ e₂) | e@(letIn _ e₁ e₂) =>
      e :: subexprs e₁ ++ subexprs e₂
  | e@(.ite c e₁ e₂) => e :: subexprs c ++ subexprs e₁ ++ subexprs e₂
  | e@(mkres _ e₁) | e@(consume e₁) | e@(assign _ e₁) | e@(ret e₁) => e :: subexprs e₁
  | e@(call _ args) => e :: (args.map subexprs).flatten
  | e => [e]

/-- (helper) The number of nodes. -/
def size (e : Expr) : Nat := (subexprs e).length

/-- (helper) The rule labels a program exercises, in the seed corpus's
spellings where it has one, deduplicated in traversal order. `Γ` lists the
binder types innermost first, as `Print.tyOf` reads them, so a use or a
`@drop` is labeled copy or move by its binder's class. -/
def rulesOf (e : Expr) : List String :=
  let rec go (Γ : List Ty) : Expr → List String
    | use i =>
        match Γ[i]? with
        | some T => if T.mult == .copy then ["(Use-Copy) §5.1"] else ["(Use-Move) §5.1"]
        | none => []
    | add e₁ e₂ | div e₁ e₂ | lt e₁ e₂ =>
        ["§5.8 operator statics", "§6.4 arithmetic traps"] ++ go Γ e₁ ++ go Γ e₂
    | mkres _ e₁ => ["§5.8 aggregate introduction (mkres stand-in)"] ++ go Γ e₁
    | consume e₁ => ["consume §5.1"] ++ go Γ e₁
    | drop i =>
        match Γ[i]? with
        | some T => if T.mult == .copy then ["(@Drop-Copy) §5.3"] else ["(@Drop) §5.3", "§6.11"]
        | none => []
    | letIn _ e₁ e₂ =>
        ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7"] ++ go Γ e₁ ++
          go ((Print.tyOf [] .int Γ e₁).getD .int :: Γ) e₂
    | assign _ e₁ => ["(Assign) §5.2", "§6.8 overwrite-drop"] ++ go Γ e₁
    | seq e₁ e₂ => ["(Seq) §5.3", "§6.7 temporary drop"] ++ go Γ e₁ ++ go Γ e₂
    | .ite c e₁ e₂ => ["(If) §5.5 join"] ++ go Γ c ++ go Γ e₁ ++ go Γ e₂
    | call _ args => ["(Call) §5.8", "(D-Call) §6.9"] ++ (args.map (go Γ)).flatten
    | ret e₁ => ["(Return-Value) §5.7", "(D-Return) §6.9"] ++ go Γ e₁
    | _ => []
  (go [] e).foldl (fun acc l => if acc.contains l then acc else acc ++ [l]) []

/-- (helper) The result type of a generated program: mostly `int`, so the
value line is usually present. -/
def resultTy : G Ty :=
  weighted .int [(5, .int), (1, .bool), (1, .unit), (2, .res .affine), (1, .res .linear), (1, .res .copy)]

/-- (helper) One generated case: a one-function program whose entry point
takes no parameters and returns the drawn type. -/
def genCase (seed i : Nat) : G Corpus.Case := do
  let depth ← weighted 3 [(4, 2), (3, 3)]
  let T ← resultTy
  let e ← expr [] T depth
  return {
    name := s!"gen_{seed}_{i}",
    description := s!"Generated program {i} of seed {seed} ({size e} nodes); " ++
      s!"regenerate with `lake exe ruecore-corpus --gen N --seed {seed}` for any N > {i}.",
    rules := rulesOf e,
    prog := Program.entry T e }

/-- (helper) `n` generated cases from `seed`, in order; a pure function of
its arguments. -/
def generate (n seed : Nat) : List Corpus.Case :=
  ((List.range n).mapM (genCase seed)).run' (mkStdGen seed)

end RueCore.Gen
