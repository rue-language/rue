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
its type, and every literal is in bounds (`InBounds`). A program's struct
declarations are drawn first and are well-formed by construction (§3's class
equation, `3.8:18`'s `@copy` restriction, and fields naming only earlier
declarations), a struct literal supplies one initializer per declared field,
and the whole-value elimination is only applied to a `Consumable`
declaration. So `Print.tyOf` succeeds on every generated program, and
whatever the verified checker rejects, it rejects for an ownership reason — a
use after move, a linear leak, a linear discard or overwrite, a disagreeing
join — which is what the bridge's refusal table covers.

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

* when a struct binder of the wanted type is in scope, using it (a move)
  is preferred to building a fresh value — including inside one arm of an
  `if`, which is how join disagreements arise;
* `let` binders are mostly structs and mostly `mut`, so linear values
  reach scope exit and assignments have targets;
* about half of the declarations carry a destructor, so a drop is as often
  observable as not, and a declaration whose fields join to `Linear` is
  `Linear` whatever its attribute says (§3), which is how the
  linear-through-a-field shapes arise;
* a sequence's discarded statement is mostly unit-typed, where `assign`
  (whose right-hand side may use the target binder itself, so
  reinitialisation after a move and overwrite of a live value both arise)
  and `@drop` live; `@drop` prefers a struct binder but may name any
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

/-- (helper) Whether a type is a struct type. -/
def isStruct : Ty → Bool
  | .struct _ => true
  | _ => false

/-- (helper) An integer literal: small, with an occasional bound so that
`+` and `/` can trap (§6.4). -/
def intLiteral : G Expr := do
  let k ← nat 0 39
  return intLit (if k = 0 then intMax else if k = 1 then intMin else Int.ofNat (k % 10))

/-! ## Struct declarations

A generated program declares its own structs (`Syntax.lean`), and the
declarations are drawn so that `WfStructs` holds by construction: a field
names only an earlier declaration, the recorded class is §3's join lifted by
the attribute, and a `@copy` draw is downgraded to no attribute when the join
is not already `Copy` or the declaration has a destructor (`3.8:18`,
`3.9:31`). So whatever the checker rejects, it rejects for an ownership
reason, never for an ill-formed declaration. -/

/-- (helper) §3's field join, for a field list read against `D`. -/
def fieldJoin (D : StructEnv) (fields : List Ty) : Mult :=
  fields.foldl (fun m T => m.join (Ty.mult D T)) .copy

/-- (helper) One field type of declaration `s`: a scalar, or an earlier
declaration — never `s` itself or a later one, so the environment is
acyclic. -/
def fieldTy (s : Nat) : G Ty := do
  if s = 0 then
    weighted .int [(4, .int), (1, .bool), (1, .unit)]
  else
    let k ← nat 1 10
    if k ≤ 4 then
      let j ← nat 0 (s - 1)
      return .struct j
    else
      weighted .int [(4, .int), (1, .bool), (1, .unit)]

/-- (helper) One declaration, well-formed by construction. -/
def genDecl (D : StructEnv) (s : Nat) : G StructDecl := do
  let k ← nat 1 3
  let fields ← (List.range k).mapM (fun _ => fieldTy s)
  let dtor ← chance 1 2
  let drawn ← weighted Attr.none [(4, Attr.none), (1, Attr.copy), (2, Attr.linear)]
  let base := fieldJoin D fields
  let attr := match drawn with
    | .copy => if base = .copy && !dtor then Attr.copy else Attr.none
    | a => a
  return { attr := attr, fields := fields, dtor := dtor, cls := attr.lift base }

/-- (helper) A struct environment of `n` declarations, built left to right so
each one sees the ones before it. -/
def genEnv : Nat → StructEnv → G StructEnv
  | 0, acc => return acc
  | n + 1, acc => do
      let sd ← genDecl acc acc.length
      genEnv n (acc ++ [sd])

/-- (helper) The declaration indices whose values the fragment's whole-value
elimination can take apart (`StructDecl.Consumable`). -/
def consumableIdxs (D : StructEnv) : List Nat :=
  (List.range D.length).filter (fun s =>
    match D[s]? with
    | some sd => decide sd.Consumable
    | none => false)

/-- (helper) The type of a fresh `let` binder: mostly structs, when the
program has any. -/
def binderTy (D : StructEnv) : G Ty := do
  if D.isEmpty then
    weighted .int [(2, .int), (1, .bool)]
  else
    let k ← nat 1 10
    if k ≤ 6 then
      let s ← nat 0 (D.length - 1)
      return .struct s
    else
      weighted .int [(2, .int), (1, .bool)]

mutual
/-- (helper) The smallest expression of a type: a literal, a use of a binder
of that type, or a struct literal with a leaf per field. -/
def atom (D : StructEnv) (Γ : Scope) : Ty → Nat → G Expr
  | .int, _ => do
      let uses := indicesWhere Γ (fun b => b.ty == .int)
      if !uses.isEmpty && (← chance 1 2) then return use (← pick 0 uses)
      intLiteral
  | .bool, _ => do
      let uses := indicesWhere Γ (fun b => b.ty == .bool)
      if !uses.isEmpty && (← chance 1 2) then return use (← pick 0 uses)
      return boolLit (← bool)
  | .unit, _ => return unitLit
  | .struct s, depth => do
      let uses := indicesWhere Γ (fun b => b.ty == .struct s)
      if !uses.isEmpty && (← chance 2 3) then return use (← pick 0 uses)
      match D[s]?, depth with
      | some sd, d + 1 => return mkStruct s (← sd.fields.mapM (fun T => atom D Γ T d))
      | _, _ => return mkStruct s []

/-- (helper) A leaf of the wanted type, one level at most: an atom, a
whole-value elimination of a consumable struct binder, a `@drop`, or an
assignment of an atom. -/
def leaf (D : StructEnv) (Γ : Scope) : Ty → Nat → G Expr
  | .int, depth => do
      let cons := indicesWhere Γ (fun b =>
        match b.ty with
        | .struct s => (consumableIdxs D).contains s
        | _ => false)
      if !cons.isEmpty && (← chance 1 2) then return consume (use (← pick 0 cons))
      atom D Γ .int depth
  | .unit, depth => do
      let structs := indicesWhere Γ (fun b => isStruct b.ty)
      let muts := indicesWhere Γ (fun b => b.mu)
      let form ← weighted 0
        [(1, 0), (if Γ.isEmpty then 0 else 6, 1), (if muts.isEmpty then 0 else 5, 2)]
      match form with
      | 1 =>
          if !structs.isEmpty && (← chance 3 4) then return drop (← pick 0 structs)
          return drop (← nat 0 (Γ.length - 1))
      | 2 =>
          let i ← pick 0 muts
          let b := Γ[i]?.getD ⟨.int, true⟩
          return assign i (← atom D Γ b.ty depth)
      | _ => return unitLit
  | T, depth => atom D Γ T depth
end

/-- (helper) A struct class to eliminate: a consumable declaration a binder in
scope already has, when there is one, so the operand is usually a move. -/
def consumeTarget (D : StructEnv) (Γ : Scope) : G (Option Nat) := do
  let inScope := (indicesWhere Γ (fun b =>
    match b.ty with
    | .struct s => (consumableIdxs D).contains s
    | _ => false)).filterMap (fun i =>
      match Γ[i]? with
      | some b => (match b.ty with | .struct s => some s | _ => none)
      | none => none)
  match inScope with
  | [] =>
      match consumableIdxs D with
      | [] => return none
      | cs => return some (← pick 0 cs)
  | _ => return some (← pick 0 inScope)

/-- (helper) An expression of the wanted type under `Γ`, at most `fuel`
levels deep. The weights here are the bias the module docstring
describes. -/
def expr (D : StructEnv) : Scope → Ty → Nat → G Expr
  | Γ, T, 0 => leaf D Γ T 2
  | Γ, T, fuel + 1 => do
      if !Γ.isEmpty && (← chance 1 6) then return (← leaf D Γ T 2)
      let form ← weighted 3 [(4, 0), (3, 1), (3, 2), (4, 3)]
      match form with
      | 0 =>
          let T₁ ← binderTy D
          let m ← chance 2 3
          let e₁ ← expr D Γ T₁ fuel
          let e₂ ← expr D ({ ty := T₁, mu := m } :: Γ) T fuel
          return letIn m e₁ e₂
      | 1 =>
          let muts := indicesWhere Γ (fun b => b.mu)
          let T₁ ← weighted .unit
            [(if muts.isEmpty then 4 else 7, .unit), (2, ← binderTy D), (2, .int)]
          let e₁ ← expr D Γ T₁ fuel
          let e₂ ← expr D Γ T fuel
          return seq e₁ e₂
      | 2 =>
          let c ← expr D Γ .bool fuel
          let e₁ ← expr D Γ T fuel
          let e₂ ← expr D Γ T fuel
          return ite c e₁ e₂
      | _ =>
          match T with
          | .int =>
              let op ← weighted 0 [(2, 0), (1, 1), (3, 2)]
              match op with
              | 0 => return add (← expr D Γ .int fuel) (← expr D Γ .int fuel)
              | 1 => return div (← expr D Γ .int fuel) (← expr D Γ .int fuel)
              | _ =>
                  match ← consumeTarget D Γ with
                  | some s => return consume (← expr D Γ (.struct s) fuel)
                  | none => return add (← expr D Γ .int fuel) (← expr D Γ .int fuel)
          | .bool => return lt (← expr D Γ .int fuel) (← expr D Γ .int fuel)
          | .unit =>
              let muts := indicesWhere Γ (fun b => b.mu)
              if !muts.isEmpty && (← chance 3 4) then
                let i ← pick 0 muts
                let b := Γ[i]?.getD ⟨.int, true⟩
                return assign i (← expr D Γ b.ty fuel)
              if Γ.isEmpty && !D.isEmpty then
                let T₁ ← binderTy D
                return seq (← expr D Γ T₁ fuel) unitLit
              leaf D Γ .unit 2
          | .struct s =>
              let uses := indicesWhere Γ (fun b => b.ty == .struct s)
              if !uses.isEmpty && (← chance 1 2) then return use (← pick 0 uses)
              match D[s]? with
              | some sd => return mkStruct s (← sd.fields.mapM (fun T' => expr D Γ T' fuel))
              | none => return mkStruct s []

/-- (helper) Every subexpression, the expression itself first. -/
def subexprs : Expr → List Expr
  | e@(add e₁ e₂) | e@(div e₁ e₂) | e@(lt e₁ e₂) | e@(seq e₁ e₂) | e@(letIn _ e₁ e₂) =>
      e :: subexprs e₁ ++ subexprs e₂
  | e@(.ite c e₁ e₂) => e :: subexprs c ++ subexprs e₁ ++ subexprs e₂
  | e@(consume e₁) | e@(assign _ e₁) | e@(ret e₁) => e :: subexprs e₁
  | e@(call _ args) | e@(mkStruct _ args) => e :: (args.map subexprs).flatten
  | e => [e]

/-- (helper) The number of nodes. -/
def size (e : Expr) : Nat := (subexprs e).length

/-- (helper) The rule labels a program exercises, in the seed corpus's
spellings where it has one, deduplicated in traversal order. `Γ` lists the
binder types innermost first, as `Print.tyOf` reads them, so a use or a
`@drop` is labeled copy or move by its binder's class. -/
def rulesOf (D : StructEnv) (e : Expr) : List String :=
  let P : Program := { structs := D, fns := [] }
  let rec go (Γ : List Ty) : Expr → List String
    | use i =>
        match Γ[i]? with
        | some T => if T.mult D == .copy then ["(Use-Copy) §5.1"] else ["(Use-Move) §5.1"]
        | none => []
    | add e₁ e₂ | div e₁ e₂ | lt e₁ e₂ =>
        ["§5.8 operator statics", "§6.4 arithmetic traps"] ++ go Γ e₁ ++ go Γ e₂
    | mkStruct _ args => ["(Struct-Intro) §5.8"] ++ (args.map (go Γ)).flatten
    | consume e₁ => ["§5.8 whole-value elimination"] ++ go Γ e₁
    | drop i =>
        match Γ[i]? with
        | some T => if T.mult D == .copy then ["(@Drop-Copy) §5.3"] else ["(@Drop) §5.3", "§6.11"]
        | none => []
    | letIn _ e₁ e₂ =>
        ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7"] ++ go Γ e₁ ++
          go ((Print.tyOf P .int Γ e₁).getD .int :: Γ) e₂
    | assign _ e₁ => ["(Assign) §5.2", "§6.8 overwrite-drop"] ++ go Γ e₁
    | seq e₁ e₂ => ["(Seq) §5.3", "§6.7 temporary drop"] ++ go Γ e₁ ++ go Γ e₂
    | .ite c e₁ e₂ => ["(If) §5.5 join"] ++ go Γ c ++ go Γ e₁ ++ go Γ e₂
    | call _ args => ["(Call) §5.8", "(D-Call) §6.9"] ++ (args.map (go Γ)).flatten
    | ret e₁ => ["(Return-Value) §5.7", "(D-Return) §6.9"] ++ go Γ e₁
    | _ => []
  (go [] e).foldl (fun acc l => if acc.contains l then acc else acc ++ [l]) []

/-- (helper) The result type of a generated program: mostly `int`, so the
value line is usually present. -/
def resultTy (D : StructEnv) : G Ty := do
  let k ← nat 1 10
  if k ≤ 3 && !D.isEmpty then
    let s ← nat 0 (D.length - 1)
    return .struct s
  weighted .int [(5, .int), (1, .bool), (1, .unit)]

/-- (helper) One generated case: a struct environment and a one-function
program whose entry point takes no parameters and returns the drawn type. -/
def genCase (seed i : Nat) : G Corpus.Case := do
  let nDecls ← weighted 2 [(2, 1), (4, 2), (3, 3)]
  let D ← genEnv nDecls []
  let depth ← weighted 3 [(4, 2), (3, 3)]
  let T ← resultTy D
  let e ← expr D [] T depth
  return {
    name := s!"gen_{seed}_{i}",
    description := s!"Generated program {i} of seed {seed} ({D.length} struct " ++
      s!"declarations, {size e} nodes); " ++
      s!"regenerate with `lake exe ruecore-corpus --gen N --seed {seed}` for any N > {i}.",
    rules := rulesOf D e,
    prog := Program.entry D T e }

/-- (helper) `n` generated cases from `seed`, in order; a pure function of
its arguments. -/
def generate (n seed : Nat) : List Corpus.Case :=
  ((List.range n).mapM (genCase seed)).run' (mkStdGen seed)

end RueCore.Gen
