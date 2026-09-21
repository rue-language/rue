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

Ownership. Moves, drops, assignments and scope exits are chosen at random, so
a large minority of the programs are rejected by the checker and refused by
the machine — a quarter of them at `--gen 200 --seed 7`, the figure the
weights below are tuned against. Both are recorded (`Corpus.caseJson` reads
them off `checkProgram` and `run` as for any case), never filtered: a rejected
program checks that the compiler rejects it too, an accepted one that the
three implementations agree with the interpreter's trace.

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
* integer literals are small, `0` among them, with an occasional `min_T` or
  `max_T` at the drawn type, so every arithmetic operator can trap — and the
  narrow types make that likely rather than rare;
* types are drawn from every width and both signednesses, `i64` a little more
  often than the rest, and an operator's operands share the drawn type
  (`4.2:1`, and `4.3a:9` for a shift's amount);
* `@intCast` draws its *source* type independently of its target, so most
  casts are between two different types and a good share of them trap;
* `@dbg` is drawn in unit position, where it competes with `@drop` and
  assignment, so a generated program's stdout usually interleaves the two
  observation channels.

One draw is deliberately **narrower** than the type would allow, and it is not
a bias but an invariant. `@total_cmp`'s two operands are drawn as float
*literals* rather than as arbitrary float expressions, because `@total_cmp` is
the only form in the fragment that can observe a NaN's *sign*, and that sign is
`σ_NaN` — a target parameter (§2, `3.12:44`), negative on x86-64 and positive
on AArch64. An exported expectation that read one would be right on one target
and wrong on the other. `floatLiteral` draws only finite decimals, so no NaN
can reach a `@total_cmp` operand *by construction*; NaNs are still generated
freely everywhere else (a `0.0 / 0.0` draw does reach `@dbg`, which renders
`NaN` sign-blind, `3.12:42`). Widening this draw means giving the exporter a
`σ_NaN` that follows the target.

`@panic` is **not** generated, for `return`'s reason: it is never-typed, so
`check` has to pick a type for it (`Checker.lean`), and a generated `@panic`
in a position whose type is not the enclosing return type would be a *false*
`reject` verdict.

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

/-- (helper) An integer type: every width and signedness of §2's `int(w, s)`,
with `i64` a little more likely so most cases stay at the width a reader
expects. -/
def intTy : G Ty := do
  let w ← weighted IntWidth.w64 [(1, IntWidth.w8), (1, .w16), (1, .w32), (2, .w64)]
  let sg ← weighted Sign.signed [(2, Sign.signed), (1, .unsigned)]
  return .int w sg

/-- (helper) An integer literal of the wanted type: small, with an occasional
`min_T`/`max_T` so that `+ - * /` can trap (§6.4). A small value is in range
at every width, so the draw needs no per-width case. -/
def intLiteral (w : IntWidth) (sg : Sign) : G Expr := do
  let k ← nat 0 39
  let n : Int :=
    if k = 0 then intMax w sg else if k = 1 then intMin w sg else Int.ofNat (k % 10)
  return intLit w sg n

/-- (helper) A float width: `f64` more often than `f32`, the way `3.12:8`
defaults an unsuffixed literal. -/
def floatWidth : G FloatWidth := weighted FloatWidth.w64 [(1, FloatWidth.w32), (2, FloatWidth.w64)]

/-- (helper) A float type at a drawn width. -/
def floatTy : G Ty := do
  return .float (← floatWidth)

/-- (helper) A float literal (§2's decimal form): a small decimal, sometimes
with a negative exponent so the value is inexact at both widths, and
sometimes `0` so that a division can reach an infinity or a NaN (`3.12:22`).
Every draw is finite and far inside both ranges, which is what `3.12:10`
requires of a source literal — and, because it is finite, **never a NaN**,
which is what makes a literal operand safe under `@total_cmp` (the
`@total_cmp` arm of `expr`). -/
def floatLiteral (w : FloatWidth) : G Expr := do
  let k ← nat 0 19
  let l : FloatLit :=
    if k = 0 then { sig := 0, negExp := false, e := 0 }
    else if k ≤ 6 then { sig := k, negExp := false, e := 0 }
    else if k ≤ 13 then { sig := k, negExp := true, e := 1 }
    else { sig := k, negExp := true, e := 2 }
  return floatLit w l

/-! ## Struct declarations

A generated program declares its own structs (`Syntax.lean`), and the
declarations are drawn so that `WfStructs` holds by construction: a field
names only an earlier declaration, the recorded class is §3's join lifted by
the attribute, a destructor is dropped from the draw when a field carries a
linear value (`3.9:44`), and a `@copy` draw is downgraded to no attribute when
the join is not already `Copy` or the declaration has a destructor (`3.8:18`,
`3.9:31`). So whatever the checker rejects, it rejects for an ownership
reason, never for an ill-formed declaration. -/

/-- (helper) §3's field join, for a field list read against `D`. -/
def fieldJoin (D : StructEnv) (fields : List Ty) : Mult :=
  fields.foldl (fun m T => m.join (Ty.mult D T)) .copy

/-- (helper) One field type of declaration `s`: a scalar, or an earlier
declaration — never `s` itself or a later one, so the environment is
acyclic. -/
def fieldTy (s : Nat) : G Ty := do
  let scalar : G Ty := do weighted (← intTy) [(4, ← intTy), (1, .bool), (1, .unit)]
  if s = 0 then
    scalar
  else
    let k ← nat 1 10
    if k ≤ 4 then
      let j ← nat 0 (s - 1)
      return .struct j
    else
      scalar

/-- (helper) One declaration, well-formed by construction. -/
def genDecl (D : StructEnv) (s : Nat) : G StructDecl := do
  let k ← nat 1 3
  let fields ← (List.range k).mapM (fun _ => fieldTy s)
  let drawnDtor ← chance 1 2
  let drawn ← weighted Attr.none [(4, Attr.none), (1, Attr.copy), (2, Attr.linear)]
  let base := fieldJoin D fields
  -- `3.9:44` (E0462): a destructor is only legal when no field carries a
  -- linear value; `3.8:18`/`3.9:31`: `@copy` needs a `Copy` join and no
  -- destructor. A draw the rules forbid falls back rather than being retried,
  -- so generation stays a pure function of the seed.
  let dtor := drawnDtor && base != .linear
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

/-- (helper) The field slots of a declaration whose type is `T` and which this
fragment may project. A step is drawn only where §5.1 and §5.3 admit it: a
path whose proper prefix is a struct declared `linear` is the declared-linear
destructure this fragment does not mechanize (RUE-2236, `Syntax.lean`), and
`3.9:34` forbids a *move* out of a value whose type declares a destructor — a
`Copy` read of such a field stays legal. -/
def projSlots (D : StructEnv) (s : Nat) (T : Ty) : List Nat :=
  match D[s]? with
  | none => []
  | some sd =>
      if sd.attr == .linear then []
      else if sd.dtor && T.mult D != .copy then []
      else (List.range sd.fields.length).filter (fun f => sd.fields[f]? == some T)

/-- (helper) Every place of the wanted type one field step under a binder in
scope: the projections a use or an assignment may name. -/
def projPlaces (D : StructEnv) (Γ : Scope) (T : Ty) : List Place :=
  ((List.range Γ.length).map (fun i =>
    match Γ[i]? with
    | some b =>
        (match b.ty with
         | .struct s => (projSlots D s T).map (fun f => Place.proj (.var i) f)
         | _ => [])
    | none => [])).flatten

/-- (helper) Every place one field step under a binder in scope, whatever its
type: the projections a `@drop` may name. -/
def dropPlaces (D : StructEnv) (Γ : Scope) : List Place :=
  ((List.range Γ.length).map (fun i =>
    match Γ[i]? with
    | some b =>
        (match b.ty with
         | .struct s =>
             (match D[s]? with
              | some sd =>
                  ((List.range sd.fields.length).map (fun f =>
                    match sd.fields[f]? with
                    | some T => (projSlots D s T).filter (· == f) |>.map
                        (fun _ => Place.proj (.var i) f)
                    | none => [])).flatten
              | none => [])
         | _ => [])
    | none => [])).flatten

/-- (helper) The type of a fresh `let` binder: mostly structs, when the
program has any. -/
def binderTy (D : StructEnv) : G Ty := do
  let scalar : G Ty := do weighted (← intTy) [(2, ← intTy), (1, ← floatTy), (1, .bool)]
  if D.isEmpty then
    scalar
  else
    let k ← nat 1 10
    if k ≤ 6 then
      let s ← nat 0 (D.length - 1)
      return .struct s
    else
      scalar

mutual
/-- (helper) The smallest expression of a type: a literal, a use of a binder
of that type, or a struct literal with a leaf per field. -/
def atom (D : StructEnv) (Γ : Scope) : Ty → Nat → G Expr
  | .int w sg, _ => do
      let uses := indicesWhere Γ (fun b => b.ty == .int w sg)
      if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
      let projs := projPlaces D Γ (.int w sg)
      if !projs.isEmpty && (← chance 1 3) then return use (← pick (.var 0) projs)
      intLiteral w sg
  | .float w, _ => do
      let uses := indicesWhere Γ (fun b => b.ty == .float w)
      if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
      floatLiteral w
  | .bool, _ => do
      let uses := indicesWhere Γ (fun b => b.ty == .bool)
      if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
      return boolLit (← bool)
  | .unit, _ => return unitLit
  | .struct s, depth => do
      let uses := indicesWhere Γ (fun b => b.ty == .struct s)
      if !uses.isEmpty && (← chance 2 3) then return use (.var (← pick 0 uses))
      let projs := projPlaces D Γ (.struct s)
      if !projs.isEmpty && (← chance 1 3) then return use (← pick (.var 0) projs)
      match D[s]?, depth with
      | some sd, d + 1 => return mkStruct s (← sd.fields.mapM (fun T => atom D Γ T d))
      | _, _ => return mkStruct s []

/-- (helper) A leaf of the wanted type, one level at most: an atom, a `@drop`
of a place, or an assignment of an atom to one. -/
def leaf (D : StructEnv) (Γ : Scope) : Ty → Nat → G Expr
  | .unit, depth => do
      let structs := indicesWhere Γ (fun b => isStruct b.ty)
      let muts := indicesWhere Γ (fun b => b.mu)
      let drops := dropPlaces D Γ
      let form ← weighted 0
        [(1, 0), (if Γ.isEmpty then 0 else 6, 1), (if muts.isEmpty then 0 else 5, 2)]
      match form with
      | 1 =>
          if !drops.isEmpty && (← chance 1 3) then return drop (← pick (.var 0) drops)
          if !structs.isEmpty && (← chance 3 4) then return drop (.var (← pick 0 structs))
          return drop (.var (← nat 0 (Γ.length - 1)))
      | 2 =>
          let i ← pick 0 muts
          let b := Γ[i]?.getD ⟨.int .w64 .signed, true⟩
          match b.ty with
          | .struct s =>
              let slots := (List.range ((D[s]?).map (·.fields.length) |>.getD 0)).filter
                (fun f => (projSlots D s ((D[s]?).bind (·.fields[f]?) |>.getD .unit)).contains f)
              if !slots.isEmpty && (← chance 1 3) then
                let f ← pick 0 slots
                let Tf := ((D[s]?).bind (·.fields[f]?)).getD (.int .w64 .signed)
                return assign (.proj (.var i) f) (← atom D Γ Tf depth)
              return assign (.var i) (← atom D Γ b.ty depth)
          | _ => return assign (.var i) (← atom D Γ b.ty depth)
      | _ => return unitLit
  | T, depth => atom D Γ T depth
end

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
            [(if muts.isEmpty then 4 else 7, .unit), (2, ← binderTy D), (2, ← intTy)]
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
          | .int w sg =>
              let self := expr D Γ (.int w sg) fuel
              let form ← weighted 0 [(5, 0), (3, 1), (2, 2), (3, 3), (2, 4)]
              match form with
              | 0 =>
                  let op ← pick BinOp.add [BinOp.add, .sub, .mul, .div, .rem]
                  return binop op (← self) (← self)
              | 1 =>
                  let op ← pick BinOp.bitAnd [BinOp.bitAnd, .bitOr, .bitXor, .shl, .shr]
                  return binop op (← self) (← self)
              | 2 =>
                  -- (Neg) §5.8 takes a signed operand only (`4.2:6`), so the
                  -- unsigned draw falls back to the complement.
                  if sg == .signed && (← chance 1 2) then return unop .neg (← self)
                  return unop .bitnot (← self)
              | 3 =>
                  let projs := projPlaces D Γ (.int w sg)
                  if !projs.isEmpty then return use (← pick (.var 0) projs)
                  return binop .add (← self) (← self)
              | _ =>
                  -- `@intCast` from an integer, or `@float_to_int` from a
                  -- float — the one float form that can trap (`3.12:18`) —
                  -- or `@total_cmp`, whose result is `i32` (`3.12:31`).
                  if w == .w32 && sg == .signed && (← chance 1 3) then
                    -- Both operands are **float literals**, not arbitrary
                    -- expressions. `@total_cmp` is the only form that can see
                    -- a NaN's sign, and that sign is `σ_NaN`, a *target*
                    -- parameter (§2, `3.12:44`) — so an expectation that read
                    -- one would be right on one target and wrong on the other.
                    -- `floatLiteral` draws only finite decimals, so no draw
                    -- here can be a NaN, and the restriction is what makes
                    -- that structural rather than a property of the seed.
                    let wf ← floatWidth
                    return binop .totalCmp (← floatLiteral wf) (← floatLiteral wf)
                  if ← chance 1 3 then
                    let Tf ← floatTy
                    return fintrin (.floatToInt w sg) (← expr D Γ Tf fuel)
                  let src ← intTy
                  return intCast w sg (← expr D Γ src fuel)
          | .float w =>
              -- (Float-Arith), (Float-Neg), (Int-To-Float), (Float-Cast) and
              -- (Float-Round) §5.8, with §6.4's trap-free dynamics.
              let self := expr D Γ (.float w) fuel
              let form ← weighted 0 [(5, 0), (2, 1), (2, 2), (2, 3), (2, 4)]
              match form with
              | 0 =>
                  let op ← pick BinOp.add [BinOp.add, .sub, .mul, .div]
                  return binop op (← self) (← self)
              | 1 => return unop .neg (← self)
              | 2 =>
                  let src ← intTy
                  return fintrin (.intToFloat w) (← expr D Γ src fuel)
              | 3 =>
                  -- `3.12:19` converts between the two widths and only
                  -- between them, so the source is the other one.
                  let src : FloatWidth := match w with | .w32 => .w64 | .w64 => .w32
                  return fintrin (.floatCast w) (← expr D Γ (.float src) fuel)
              | _ =>
                  let k ← pick FloatUnIntrin.sqrt
                    [FloatUnIntrin.sqrt, .round .floor, .round .ceil, .round .trunc,
                      .round .round]
                  return fintrin (.roundOp k) (← self)
          | .bool =>
              if ← chance 1 5 then return unop .not (← expr D Γ .bool fuel)
              let op ← pick BinOp.lt [BinOp.lt, .le, .gt, .ge]
              if ← chance 1 3 then
                let Tf ← floatTy
                return binop op (← expr D Γ Tf fuel) (← expr D Γ Tf fuel)
              let Tc ← intTy
              return binop op (← expr D Γ Tc fuel) (← expr D Γ Tc fuel)
          | .unit =>
              let muts := indicesWhere Γ (fun b => b.mu)
              if !muts.isEmpty && (← chance 3 4) then
                let i ← pick 0 muts
                let b := Γ[i]?.getD ⟨.int .w64 .signed, true⟩
                return assign (.var i) (← expr D Γ b.ty fuel)
              if ← chance 1 3 then
                let To ← weighted (← intTy) [(3, ← intTy), (2, ← floatTy), (1, .bool)]
                return dbg (← expr D Γ To fuel)
              if Γ.isEmpty && !D.isEmpty then
                let T₁ ← binderTy D
                return seq (← expr D Γ T₁ fuel) unitLit
              leaf D Γ .unit 2
          | .struct s =>
              let uses := indicesWhere Γ (fun b => b.ty == .struct s)
              if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
              let projs := projPlaces D Γ (.struct s)
              if !projs.isEmpty && (← chance 1 4) then return use (← pick (.var 0) projs)
              match D[s]? with
              | some sd => return mkStruct s (← sd.fields.mapM (fun T' => expr D Γ T' fuel))
              | none => return mkStruct s []

/-- (helper) Every subexpression, the expression itself first. -/
def subexprs : Expr → List Expr
  | e@(binop _ e₁ e₂) | e@(seq e₁ e₂) | e@(letIn _ e₁ e₂) =>
      e :: subexprs e₁ ++ subexprs e₂
  | e@(.ite c e₁ e₂) => e :: subexprs c ++ subexprs e₁ ++ subexprs e₂
  | e@(assign _ e₁) | e@(ret e₁) | e@(unop _ e₁) | e@(intCast _ _ e₁)
  | e@(fintrin _ e₁) | e@(dbg e₁) => e :: subexprs e₁
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
    | use pl =>
        (match Γ[pl.root]? with
         | some T =>
             (match T.atPath D pl.path with
              | some T' =>
                  (if T'.mult D == .copy then ["(Use-Copy) §5.1"] else ["(Use-Move) §5.1"]) ++
                    (if pl.path.isEmpty then [] else ["§4.2 partial move", "3.8:22"])
              | none => [])
         | none => [])
    | binop op e₁ e₂ =>
        (if op.isCompare then ["(Ord) §5.8", "§6.4"]
         else ["(Arith) §5.8", "§6.4 arithmetic traps"]) ++ go Γ e₁ ++ go Γ e₂
    | unop op e₁ =>
        (match op with
         | .neg => ["(Neg) §5.8", "§6.4 arithmetic traps"]
         | .not => ["(Not) §5.8"]
         | .bitnot => ["(BitNot) §5.8"]) ++ go Γ e₁
    | intCast _ _ e₁ => ["(Int-Cast) §5.8", "(D-Int-Cast-Trap) §6.4"] ++ go Γ e₁
    | Expr.panic _ => ["(Panic) §5.8", "(D-Panic) §6.12"]
    | dbg e₁ => ["(Dbg) §5.8"] ++ go Γ e₁
    | mkStruct _ args => ["(Struct-Intro) §5.8"] ++ (args.map (go Γ)).flatten
    | drop pl =>
        (match Γ[pl.root]? with
         | some T =>
             (match T.atPath D pl.path with
              | some T' =>
                  (if T'.mult D == .copy then ["(@Drop-Copy) §5.3"]
                   else ["(@Drop) §5.3", "§6.11"]) ++
                    (if pl.path.isEmpty then [] else ["§4.2 partial move", "3.8:22"])
              | none => [])
         | none => [])
    | letIn _ e₁ e₂ =>
        ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7"] ++ go Γ e₁ ++
          go ((Print.tyOf P (.int .w64 .signed) Γ e₁).getD (.int .w64 .signed) :: Γ) e₂
    | assign _ e₁ => ["(Assign) §5.2", "§6.8 overwrite-drop"] ++ go Γ e₁
    | seq e₁ e₂ => ["(Seq) §5.3", "§6.7 temporary drop"] ++ go Γ e₁ ++ go Γ e₂
    | .ite c e₁ e₂ => ["(If) §5.5 join"] ++ go Γ c ++ go Γ e₁ ++ go Γ e₂
    | call _ args => ["(Call) §5.8", "(D-Call) §6.9"] ++ (args.map (go Γ)).flatten
    | ret e₁ => ["(Return-Value) §5.7", "(D-Return) §6.9"] ++ go Γ e₁
    | _ => []
  (go [] e).foldl (fun acc l => if acc.contains l then acc else acc ++ [l]) []

/-- (helper) The result type of a generated program: mostly `int`, so the
value line is usually present, with a float often enough that `main` prints a
shortest round-trip rendering (`3.12:40`) as well. -/
def resultTy (D : StructEnv) : G Ty := do
  let k ← nat 1 10
  if k ≤ 3 && !D.isEmpty then
    let s ← nat 0 (D.length - 1)
    return .struct s
  weighted (← intTy) [(5, ← intTy), (3, ← floatTy), (1, .bool), (1, .unit)]

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
