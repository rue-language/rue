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
closed, well-scoped, and simply typed by construction: every `use p` and
`drop p` names a place rooted at a binder in scope, operator operands are
`int`, both arms of an `if` have the wanted type, `assign p e` targets a place
rooted at a `mut` binder and `e` has the place's type, and every literal is in
bounds (`InBounds`). A program's struct **and enum** declarations are drawn
first and are well-formed by construction (§3's class equation and `6.3:19`'s
payload join, `3.8:18`'s `@copy` restriction, and fields and payload
components naming only declarations drawn earlier, which is `3.0:5`'s
acyclicity); a struct literal supplies one initializer per declared field, an
enum literal one argument per declared payload component of the variant its
tag names (`6.3:16`), a `match` has **exactly one arm per variant in
declaration order** typed under that variant's payload locals, and a
projection names a declared slot at the type it is asked for. So `Print.tyOf`
succeeds on every generated program, and whatever the verified checker
rejects, it rejects for an ownership reason — a use after move, a use of a
partially moved value, a linear leak, a linear discard or overwrite, a
disagreeing join, a move out of a destructor-bearing value — which is what the
bridge's refusal table covers.

Exhaustiveness is a property of the *draw* rather than a premise the draw
might miss: `expr` builds the arm list by mapping over the declaration's own
variant list, so the arm count and the arm order are the declaration's by
construction and (Match) §5.5's `arms.length = ed.variants.length` cannot
fail. All arms are drawn at the `match`'s own type, so `firstArmTy`'s choice
(`Checker.lean`) is never the reason a generated case is refused.

## How deep a place goes

A **use** and a **`@drop`** are drawn at a path of one *or two* field steps —
a field of a field — wherever the declarations reach that far and the place
rules admit the path (`paths2`, `pathOk`, which is `projSlots`' legality test
read at a whole path). Depth 2 is where the path machinery actually recurses:
`OwnSt.get` and `setAt`'s padding, `ContentsMatches.readAt`/`.writeAt`, and
§6.11's nested `⊘`-skip. One seed case (`deep_path`) is not coverage of it.

An **assignment target** is at most **one** field step deep, and that is a
deliberate exception: a reinitialising assignment at depth ≥ 2 is a compiler
defect (RUE-2319) — it runs the overwrite-drop on the already moved-out
position and then leaks the value it stored — so the model and the compiler
disagree there for a reason that is not the model's, and a generated case with
the shape would be a false bridge failure rather than a finding. Depth-2 uses
and drops are unaffected by it: the defect is in the assignment path, and
depth-2 use, `@drop` and assignment-under-a-live-value were all checked by
hand against the compiler.

One shape is not drawn at any depth, and it is a gap rather than a guard now
that RUE-2236 has landed: a projection through a proper prefix of
declared-`linear` struct type selects §4.2's `Declared(d, π_s)` plan, which
§5.1 and §6.3 now discharge — `projSlots` and `pathOk` simply never draw one.
The seed cases cover the shape; drawing it is RUE-2339.

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

That is why a `match` **arm** does not draw one either, although a diverging
arm is otherwise the obvious shape to generate: an arm is a branch like an
`if`'s, so a `return` or a `@panic` in one costs §5.5's join the same two
things — the arm's *type* choice, which `firstArmTy` fixes from whichever arm
`check` reads first, and the arm's *state*, which `Ctx.joinAll` folds in where
§5.7 contributes `⊥`. Both were measured against the compiler on RUE-2320 (a
diverging arm in first, middle and last position, and a linear binding a
diverging arm discharged): the compiler accepts every one of them and this
fragment rejects them, so a drawn one would be a false bridge failure rather
than a finding. The seed corpus carries `ret_past_payload` instead, which is
the `return`-past-payload-locals shape at a position where `check` *is*
complete.

## What it deliberately does not guarantee

Ownership. Moves, drops, assignments, `match` arms and scope exits are chosen
at random, so a large minority of the programs are rejected by the checker and
refused by the machine — 86 of 200 at `--gen 200 --seed 7` and 411 of 1,000 at
`--gen 1000 --seed 23`, the figures the weights below are tuned against. Both
are recorded (`Corpus.caseJson` reads them off `checkProgram` and `run` as for
any case), never filtered: a rejected program checks that the compiler rejects
it too, an accepted one that the three implementations agree with the
interpreter's trace. At those two settings 94 of 200 and 465 of 1,000 programs
contain a `match`.

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
  and `@drop` live; `@drop` prefers a binder §6.11 walks into — a struct or an
  enum — but may name any binder, since the calculus allows `@drop` of a place
  of any class;
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
  observation channels;
* an enum's payload components are mostly **struct** types, because a struct
  is what carries a destructor and a declared attribute — so the payload
  classes span `Copy`, `Affine` and `Linear`, `6.3:19`'s join has something to
  say, and an enum one of whose variants carries a `linear` payload is itself
  `Linear` whichever variant a value holds;
* a `match` is weighted up sharply when the scope already holds a place of
  enum type, and its scrutinee is then that place rather than a fresh
  temporary: a projection first (`3.8:22`'s partial move at a field, whose
  sibling fields still drop at scope exit) and a binder next (the move that
  makes a second `match` on the same place the E0205 the compiler reports).
  That weight on its own starved the shape it is for, because the branch it
  fired on was the rarer one: with the weight alone only 25 of 171 `match` sites
  at `--gen 200 --seed 7` and 73 of 754 at `--gen 1000 --seed 23` had an enum
  place in scope at all. So where the scope offers no place of the drawn enum's
  type the draw **makes** one half the time — `let v = <the temporary> in
  match v`, sometimes with one statement in between — instead of matching a
  temporary, and the arms are then typed under a scope that still holds the
  consumed binding. Measured with it: 116 of 185 sites and 566 of 895 have a
  place in scope, 111 and 533 scrutinize one (the weight alone reached 21 of 171
  and 60 of 754), and the E0205 a second `match` on the same place is becomes
  the *deepest* refusal of 13 of 200 and 108 of 1,000 cases, where the weight
  alone reached 2 and 12. The n-way **join** conflict stays rare at either
  setting — 1 case in 1,000 at seed 23, the same order as the binary (If)
  join's — because it needs two arms to disagree about an entry that carries a
  linear value and outlives the `match`, which random arms seldom do;
* a `let` binder is biased toward a declaration that holds an enum in
  a field, which is what puts a projection of enum type in scope at all;
* an arm's body is an ordinary drawn expression over the payload locals, so
  what the arm does with a payload is whatever the existing weights do with
  any binder: move it into an outer `mut` binding or into a fresh aggregate,
  `@drop` it, read a `Copy` one twice, or leave it — which at an `Affine`
  payload is the drop `6.3:17` times at the arm's end and at a `Linear` one is
  the leak §5.6 rejects (E0406).

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

/-- (helper) Whether a type is an enum type. -/
def isEnum : Ty → Bool
  | .enum _ => true
  | _ => false

/-- (helper) Whether a type is one §6.11 drops by walking into it: a struct,
whose droppable fields go in declaration order (`3.9:13`), or an enum, whose
**active** variant's payload goes and nothing else (`6.3:20`). This is the set
a drawn `@drop` prefers, because a drop of a scalar observes nothing. -/
def isAggregate (T : Ty) : Bool := isStruct T || isEnum T

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

/-! ## Struct and enum declarations

A generated program declares its own structs and enums (`Syntax.lean`), and the
declarations are drawn so that `WfDecls` holds by construction: a field or a
payload component names only a declaration drawn **before** it, which is
`3.0:5`'s acyclicity (E0483) restricted to the draw order; a struct's recorded
class is §3's join lifted by the attribute and an enum's is `6.3:19`'s payload
join over every variant, with no attribute to lift; a destructor is dropped
from the draw when a field carries a linear value (`3.9:44`), and a `@copy`
draw is downgraded to no attribute when the join is not already `Copy` or the
declaration has a destructor (`3.8:18`, `3.9:31`). So whatever the checker
rejects, it rejects for an ownership reason, never for an ill-formed
declaration.

The draw order is structs, then enums, then a few more structs — `genCase`. A
first-round struct names only earlier structs, an enum names any of those and
any earlier enum, and a last-round struct may also name an enum, which is what
puts an enum in a **field** and so makes a `match` scrutinee a projection. No
cycle is expressible, because every one of those relations points strictly
backwards in one global order. -/

/-- (helper) §3's field join, for a field list read against `D`. -/
def fieldJoin (D : Decls) (fields : List Ty) : Mult :=
  fields.foldl (fun m T => m.join (Ty.mult D T)) .copy

/-- (helper) One field type of declaration `s`, drawn against the `nEnums`
enums already in the environment: a scalar, an earlier struct declaration, or
one of those enums — never `s` itself, a later struct, or a later enum, so the
environment stays acyclic (`3.0:5`). The enum share is `0` for the structs
drawn before any enum exists, which is every first-round declaration. -/
def fieldTy (s nEnums : Nat) : G Ty := do
  let scalar : G Ty := do weighted (← intTy) [(4, ← intTy), (1, .bool), (1, .unit)]
  if s = 0 && nEnums = 0 then
    scalar
  else
    let k ← nat 1 10
    if k ≤ 4 && s ≠ 0 then
      let j ← nat 0 (s - 1)
      return .struct j
    else if k ≤ 8 && nEnums ≠ 0 then
      let j ← nat 0 (nEnums - 1)
      return .enum j
    else
      scalar

/-- (helper) One struct declaration, well-formed by construction. -/
def genDecl (D : Decls) (s nEnums : Nat) : G StructDecl := do
  let k ← nat 1 3
  let fields ← (List.range k).mapM (fun _ => fieldTy s nEnums)
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

/-- (helper) `n` more struct declarations, built left to right so each one sees
the ones before it, and drawn against the `nEnums` enums already in the
environment. -/
def genEnv (nEnums : Nat) : Nat → Decls → G Decls
  | 0, acc => return acc
  | n + 1, acc => do
      let sd ← genDecl acc acc.structs.length nEnums
      genEnv nEnums n { acc with structs := acc.structs ++ [sd] }

/-- (helper) One payload component type of enum declaration `e`: a scalar, any
of the `nStructs` struct declarations — none of which names an enum, since they
are drawn first — or an **earlier** enum. Struct components are the common draw
because they are what carries a destructor and a declared attribute, so the
payload classes span `Copy`, `Affine` and `Linear` and `6.3:19`'s join has
something to say. -/
def payloadTy (nStructs e : Nat) : G Ty := do
  let scalar : G Ty := do weighted (← intTy) [(3, ← intTy), (1, .bool), (1, .unit)]
  let k ← nat 1 10
  if k ≤ 6 && nStructs ≠ 0 then
    let j ← nat 0 (nStructs - 1)
    return .struct j
  else if k ≤ 8 && e ≠ 0 then
    let j ← nat 0 (e - 1)
    return .enum j
  else
    scalar

/-- (helper) One enum declaration, well-formed by construction: two or three
variants in declaration order — the order a tag indexes and (Match) §5.5's arms
are presented in — each carrying `0`–`2` payload components, and the recorded
class exactly `6.3:19`'s join over every component of every variant, which is
what `checkEnumDecl` pins. An arity-`0` variant is `6.3:14`'s
discriminant-only case, and a declaration all of whose variants are arity `0`
is the duplicable tag enum of `3.8:2`. There is no attribute to draw and no
destructor: §3 gives an enum neither (E0417). -/
def genEnumDecl (D : Decls) (e : Nat) : G EnumDecl := do
  let nv ← weighted 2 [(5, 2), (2, 3)]
  let variants ← (List.range nv).mapM (fun _ => do
    let a ← weighted 1 [(3, 0), (5, 1), (2, 2)]
    (List.range a).mapM (fun _ => payloadTy D.structs.length e))
  let ed : EnumDecl := { variants := variants, cls := .copy }
  return { ed with cls := ed.payloadJoin D }

/-- (helper) An enum environment of `n` declarations, built left to right so
each one sees every struct and the enums before it — which is what keeps
`3.0:5`'s cycle out of the draw. -/
def genEnums : Nat → Decls → G Decls
  | 0, acc => return acc
  | n + 1, acc => do
      let ed ← genEnumDecl acc acc.enums.length
      genEnums n { acc with enums := acc.enums ++ [ed] }

/-- (helper) A canonical value of any type, for the one place a type-directed
draw can run out of depth and still owe a well-typed expression: a literal at a
scalar (§5.8's (Lit)), one initializer per **declared** field at a struct
(`3.6:15`, (Struct-Intro) §5.8), and the first variant applied to its own
payload at an enum (`6.3:16`, (Enum-Intro) §5.5). The fuel is
`|structs| + |enums|`, which `3.0:5`'s acyclicity makes enough — the same bound
`checkNoCycle` peels with — because a component names a declaration strictly
earlier in the draw order.

Before this existed the depth-exhausted fallback was `mkStruct s []`, which is
a struct literal missing its initializers: an **ill-typed** program, rejected
by the checker for a reason that is not ownership and by the compiler for a
field-count error rather than the ownership diagnostic the bridge's refusal
table covers (two of the 200 cases at `--gen 200 --seed 7` were that shape). -/
def leastValue (D : Decls) : Nat → Ty → Expr
  | _, .int w sg => intLit w sg 0
  | _, .float w => floatLit w { sig := 0, negExp := false, e := 0 }
  | _, .bool => boolLit false
  | _, .unit => unitLit
  | 0, .struct s => mkStruct s []
  | fuel + 1, .struct s =>
      match D.structs[s]? with
      | some sd => mkStruct s (sd.fields.map (leastValue D fuel))
      | none => mkStruct s []
  | 0, .enum e => mkEnum e 0 []
  | fuel + 1, .enum e =>
      match D.enums[e]? with
      | some ed => mkEnum e 0 (((ed.variants[0]?).getD []).map (leastValue D fuel))
      | none => mkEnum e 0 []
  -- Unreachable: no array type is drawn (RUE-2331). (Array-Intro) §5.8's own
  -- literal would be `n` copies of the element's least value.
  | _, .array T _ => mkArray T []

/-- (helper) The fuel `leastValue` is called at: one round per declaration, the
bound `3.0:5`'s acyclicity makes sufficient. -/
def declFuel (D : Decls) : Nat := D.structs.length + D.enums.length

/-- (helper) The field slots of a declaration whose type is `T` and which this
fragment may project. A step is drawn only where §5.1 and §5.3 admit it: a
path whose proper prefix is a struct declared `linear` is the declared-linear
destructure of §4.2's `Declared(d, π_s)` plan (`Syntax.lean`), which the
generator does not draw (RUE-2339), and
`3.9:34` forbids a *move* out of a value whose type declares a destructor — a
`Copy` read of such a field stays legal. -/
def projSlots (D : Decls) (s : Nat) (T : Ty) : List Nat :=
  match D.structs[s]? with
  | none => []
  | some sd =>
      if sd.attr == .linear then []
      else if sd.dtor && T.mult D != .copy then []
      else (List.range sd.fields.length).filter (fun f => sd.fields[f]? == some T)

/-- (helper) A place from a root binder and a path of field steps, read from
the root outward — the inverse of `Place.path` (`Syntax.lean`). -/
def placeOfPath (i : Nat) (π : List Nat) : Place :=
  π.foldl (fun p f => Place.proj p f) (.var i)

/-- (helper) The field slots a type has, as single steps. -/
def fieldSlots (D : Decls) : Ty → List Nat
  | .struct s =>
      (match D.structs[s]? with
       | some sd => List.range sd.fields.length
       | none => [])
  -- The generator draws no array types yet (RUE-2331), so an array's index
  -- steps are deliberately not offered here: `placeOfPath` builds `Place.proj`
  -- steps, and an index step is `Place.idx`.
  | .array _ _ => []
  -- An **enum** has no step either, and that one is the fragment's shape
  -- rather than a gap: §5.6 tracks no path into a payload, `Ty.fieldAt` is
  -- `none` at an enum type, and the only way to a payload component is a
  -- `match` arm's binding (`Syntax.lean`, `6.3:17`).
  | .int _ _ | .float _ | .bool | .unit | .enum _ => []

/-- (helper) Every path of **one or two** field steps under a binder's
declared type. Depth 2 is where the path machinery actually recurses —
`OwnSt.get`/`setAt`'s padding, `readAt`/`writeAt`, and §6.11's nested `⊘`-skip
— and one seed case (`deep_path`) is not coverage of it. -/
def paths2 (D : Decls) (T₀ : Ty) : List (List Nat) :=
  (fieldSlots D T₀).flatMap fun f =>
    [f] :: (match T₀.fieldAt D f with
            | some T' => (fieldSlots D T').map (fun g => [f, g])
            | none => [])

/-- (helper) Whether the four place rules admit a path from a binder of type
`T₀` to a leaf of type `T`: the path types (`atPath`), no **proper prefix** is a
struct declared `linear` — so §4.2 records the `Ordinary` plan and not the
`Declared(d, π_s)` one (`declaredPrefix`, `Syntax.lean`) — and, where the leaf
is not `Copy` and so the rule is (Use-Move) or (@Drop) rather than their `Copy`
twins, no proper prefix declares a destructor (`3.9:34`). This is `projSlots`'
test read at a whole path rather than at one step, so it stays right at depth
2.

The generator therefore draws **no** declared-linear destructure, although the
rule §5.1 gives one is now mechanized: `projSlots` already refuses to step into
a declared-`linear` struct, so the shape is out of the grammar it draws from
rather than filtered out of it. Drawing destructures is RUE-2339. -/
def pathOk (D : Decls) (T₀ : Ty) (π : List Nat) (T : Ty) : Bool :=
  Ty.atPath D T₀ π == some T && (declaredPrefix D T₀ π).isNone &&
    (T.mult D == .copy || noDtorPrefix D T₀ π)

/-- (helper) Every place of the wanted type one **or two** field steps under a
binder in scope: the projections a use may name. -/
def projPlaces (D : Decls) (Γ : Scope) (T : Ty) : List Place :=
  ((List.range Γ.length).map (fun i =>
    match Γ[i]? with
    | some b =>
        ((paths2 D b.ty).filter (fun π => pathOk D b.ty π T)).map (placeOfPath i)
    | none => [])).flatten

/-- (helper) Draw a place, biased toward the **deeper** one: where the list
offers a path of two field steps it is taken half the time. Depth 2 is in the
tail without it, because a two-step path needs a nesting declaration, a binder
of the outer type in scope, and both prefixes free of a declared-`linear`
attribute and of a destructor at once. Measured on the current draws, `--gen 200
--seed 7` reaches 4 depth-2 places across 3 programs with the bias and 3 across
3 without it, and `--gen 300 --seed 23` 13 across 8 with and 11 across 6
without. This is one of the module's weights; it lives here rather than at the
six draw sites. -/
def pickPlace (default : Place) (ps : List Place) : G Place := do
  let deep := ps.filter (fun p => 2 ≤ p.path.length)
  if !deep.isEmpty && (← chance 1 2) then pick default deep else pick default ps

/-- (helper) Every place one **or two** field steps under a binder in scope,
whatever its type: the projections a `@drop` may name. -/
def dropPlaces (D : Decls) (Γ : Scope) : List Place :=
  ((List.range Γ.length).map (fun i =>
    match Γ[i]? with
    | some b =>
        (paths2 D b.ty).filterMap (fun π =>
          match Ty.atPath D b.ty π with
          | some T => if pathOk D b.ty π T then some (placeOfPath i π) else none
          | none => none)
    | none => [])).flatten

/-- (helper) The binders an arm's body is drawn under: (Match) §5.5's payload
locals on top of the enclosing scope, in `armCtx`'s order — the tuple
**reversed**, so component `ai` has de Bruijn index `0` — and unmarked, because
§2 gives a pattern binding no `μ` (the compiler's parser rejects `mut` there,
which is what makes `armCtx`'s `mu := false` faithful). -/
def armScope (Ts : List Ty) (Γ : Scope) : Scope :=
  (Ts.map (fun T => ({ ty := T, mu := false } : Binder))).reverse ++ Γ

/-- (helper) Which enum a drawn `match` scrutinizes: one a binder or a
projection in scope already holds, where there is one, so the scrutinee is a
**place** and typing it is (Use-Move)/(Use-Copy) §5.1 at that place — a move
for a non-`Copy` enum, because a scrutinee is a value context (`3.8:7`,
`3.8:76`; `6.3:17` for the payload the arm binds out of it), and what makes a
second `match` on it the E0205 the compiler reports. Otherwise any declared
enum, which the draw then builds as a temporary. -/
def pickEnumIdx (D : Decls) (Γ : Scope) : G Nat := do
  let inScope := (List.range D.enums.length).filter (fun e =>
    !(indicesWhere Γ (fun b => b.ty == .enum e)).isEmpty ||
      !(projPlaces D Γ (.enum e)).isEmpty)
  if !inScope.isEmpty && (← chance 3 4) then pick 0 inScope
  else pick 0 (List.range D.enums.length)

/-- (helper) Whether the scope already holds a place of enum type — a binder,
or a field of a binder. Where it does, a drawn `match` is weighted up, because
a `match` on a **place** is where the interesting ownership lives: the move
that consumes the binding (`3.8:7`, `3.8:76`, `6.3:17`), the partial move at a
field (`3.8:22`), and the E0205 a second `match` on the same place is. A `match` on
a temporary exercises (D-Match) §6.6 and the arm teardown but leaves no state
behind for the join to read. -/
def enumPlaceInScope (D : Decls) (Γ : Scope) : Bool :=
  (List.range D.enums.length).any (fun e =>
    !(indicesWhere Γ (fun b => b.ty == .enum e)).isEmpty ||
      !(projPlaces D Γ (.enum e)).isEmpty)

/-- (helper) The type of a fresh `let` binder: mostly aggregates, when the
program declares any — structs a little more often than enums, since a struct
is also what an enum's payload is usually made of. -/
def binderTy (D : Decls) : G Ty := do
  let scalar : G Ty := do weighted (← intTy) [(2, ← intTy), (1, ← floatTy), (1, .bool)]
  let structTy : G Ty := do
    if D.structs.isEmpty then scalar
    else
      -- A declaration that holds an enum in a field is preferred, because a
      -- binder of that type is what makes a drawn `match` scrutinize a
      -- **projection** — `3.8:22`'s partial move at a field, whose residue
      -- the sibling fields still drop at scope exit (§6.11).
      let holders := (List.range D.structs.length).filter (fun s =>
        ((D.structs[s]?).map (fun sd => sd.fields.any isEnum)).getD false)
      if !holders.isEmpty && (← chance 1 2) then return .struct (← pick 0 holders)
      return .struct (← nat 0 (D.structs.length - 1))
  let enumTy : G Ty := do
    if D.enums.isEmpty then structTy else return .enum (← nat 0 (D.enums.length - 1))
  if D.structs.isEmpty && D.enums.isEmpty then
    scalar
  else
    let k ← nat 1 10
    if k ≤ 4 then structTy
    else if k ≤ 7 then enumTy
    else scalar

mutual
/-- (helper) The smallest expression of a type: a literal, a use of a binder
of that type, or a struct literal with a leaf per field. -/
def atom (D : Decls) (Γ : Scope) : Ty → Nat → G Expr
  | .int w sg, _ => do
      let projs := projPlaces D Γ (.int w sg)
      if !projs.isEmpty && (← chance 1 2) then return use (← pickPlace (.var 0) projs)
      let uses := indicesWhere Γ (fun b => b.ty == .int w sg)
      if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
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
      let projs := projPlaces D Γ (.struct s)
      if !projs.isEmpty && (← chance 1 2) then return use (← pickPlace (.var 0) projs)
      let uses := indicesWhere Γ (fun b => b.ty == .struct s)
      if !uses.isEmpty && (← chance 2 3) then return use (.var (← pick 0 uses))
      match D.structs[s]?, depth with
      | some sd, d + 1 => return mkStruct s (← sd.fields.mapM (fun T => atom D Γ T d))
      | _, _ => return leastValue D (declFuel D) (.struct s)
  | .enum e, depth => do
      -- (Enum-Intro) §5.5, or a use of an enum already in scope. The tag is
      -- drawn uniformly over the declared variants, so a discriminant-only one
      -- (`6.3:14`) is as likely as a payload-carrying one at the same
      -- declaration.
      let projs := projPlaces D Γ (.enum e)
      if !projs.isEmpty && (← chance 1 2) then return use (← pickPlace (.var 0) projs)
      let uses := indicesWhere Γ (fun b => b.ty == .enum e)
      if !uses.isEmpty && (← chance 2 3) then return use (.var (← pick 0 uses))
      match D.enums[e]?, depth with
      | some ed, d + 1 =>
          let k ← nat 0 (ed.variants.length - 1)
          return mkEnum e k (← ((ed.variants[k]?).getD []).mapM (fun T => atom D Γ T d))
      | _, _ => return leastValue D (declFuel D) (.enum e)
  -- Unreachable: no array type is drawn (RUE-2331). The arm is still the
  -- literal (Array-Intro) §5.8 concludes `[T; n]` at, one atom per element,
  -- with the same depth-exhausted fallback the struct arm above has.
  | .array T n, depth => do
      match depth with
      | d + 1 => return mkArray T (← (List.replicate n T).mapM (fun T' => atom D Γ T' d))
      | 0 => return mkArray T []

/-- (helper) A leaf of the wanted type, one level at most: an atom, a `@drop`
of a place, or an assignment of an atom to one. -/
def leaf (D : Decls) (Γ : Scope) : Ty → Nat → G Expr
  | .unit, depth => do
      let aggregates := indicesWhere Γ (fun b => isAggregate b.ty)
      let muts := indicesWhere Γ (fun b => b.mu)
      let drops := dropPlaces D Γ
      let form ← weighted 0
        [(1, 0), (if Γ.isEmpty then 0 else 6, 1), (if muts.isEmpty then 0 else 5, 2)]
      match form with
      | 1 =>
          if !drops.isEmpty && (← chance 1 2) then return drop (← pickPlace (.var 0) drops)
          if !aggregates.isEmpty && (← chance 3 4) then return drop (.var (← pick 0 aggregates))
          return drop (.var (← nat 0 (Γ.length - 1)))
      | 2 =>
          let i ← pick 0 muts
          let b := Γ[i]?.getD ⟨.int .w64 .signed, true⟩
          match b.ty with
          | .struct s =>
              let slots := (List.range ((D.structs[s]?).map (·.fields.length) |>.getD 0)).filter
                (fun f => (projSlots D s ((D.structs[s]?).bind (·.fields[f]?) |>.getD .unit)).contains f)
              if !slots.isEmpty && (← chance 1 2) then
                let f ← pick 0 slots
                let Tf := ((D.structs[s]?).bind (·.fields[f]?)).getD (.int .w64 .signed)
                return assign (.proj (.var i) f) (← atom D Γ Tf depth)
              return assign (.var i) (← atom D Γ b.ty depth)
          | _ => return assign (.var i) (← atom D Γ b.ty depth)
      | _ => return unitLit
  | T, depth => atom D Γ T depth
end

/-- (helper) An expression of the wanted type under `Γ`, at most `fuel`
levels deep. The weights here are the bias the module docstring
describes. -/
def expr (D : Decls) : Scope → Ty → Nat → G Expr
  | Γ, T, 0 => leaf D Γ T 2
  | Γ, T, fuel + 1 => do
      if !Γ.isEmpty && (← chance 1 6) then return (← leaf D Γ T 2)
      let form ← weighted 3
        [(4, 0), (3, 1), (3, 2), (4, 3),
          (if D.enums.isEmpty then 0 else if enumPlaceInScope D Γ then 14 else 3, 4)]
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
      | 4 =>
          -- (Match) §5.5 in expression position: the scrutinee at the drawn
          -- enum type, then **exactly one arm per variant in declaration
          -- order**, each drawn at the `match`'s own type `T` under that
          -- variant's payload locals (`armScope`). Exhaustiveness is the arm
          -- list's shape, so it is a property of the draw rather than a
          -- premise the draw could miss; what the arm does with its payload
          -- is left to chance, exactly as every other ownership choice is.
          let e ← pickEnumIdx D Γ
          match D.enums[e]? with
          | some ed =>
              -- The scrutinee is a **place** wherever the scope offers one, so
              -- that typing it is the (Use-Move)/(Use-Copy) §5.1 read at that
              -- place rather than the construction of a fresh temporary: a
              -- projection is `3.8:22`'s partial move out of a struct field,
              -- and a binder is what makes a second `match` on it the E0205
              -- use of a moved-out place. A temporary is the remaining draw,
              -- and the one the empty scope always takes.
              let projs := projPlaces D Γ (.enum e)
              let uses := indicesWhere Γ (fun b => b.ty == .enum e)
              if projs.isEmpty && uses.isEmpty && (← chance 1 2) then
                -- The scope offers no place of this type, so half the time the
                -- draw **makes** one: `let v = <the temporary> in match v`,
                -- sometimes with one statement in between. Without this the
                -- place-scrutinee shapes are starved — a `match` is drawn far
                -- more often where no enum place is in scope than where one is
                -- — and with it the arms are typed under a scope that still
                -- holds the consumed binding, which is what a nested `match` on
                -- `v` (E0205) and the join over an entry an arm moved need.
                let m ← chance 1 3
                let init ← expr D Γ (.enum e) fuel
                let Γ' : Scope := { ty := .enum e, mu := m } :: Γ
                let arms ← ed.variants.mapM (fun Ts => expr D (armScope Ts Γ') T fuel)
                let m' := «match» (use (.var 0)) arms
                let body ← if ← chance 1 3 then
                    pure (seq (← leaf D Γ' .unit 2) m')
                  else pure m'
                return letIn m init body
              let scrut ←
                if !projs.isEmpty && (← chance 3 5) then
                  pure (use (← pickPlace (.var 0) projs))
                else if !uses.isEmpty && (← chance 4 5) then
                  pure (use (.var (← pick 0 uses)))
                else
                  expr D Γ (.enum e) fuel
              let arms ← ed.variants.mapM (fun Ts => expr D (armScope Ts Γ) T fuel)
              return «match» scrut arms
          | none => leaf D Γ T 2
      | _ =>
          match T with
          | .enum e =>
              -- The same (Enum-Intro) §5.5 draw `atom` makes, one level up: a
              -- use of an enum in scope, a projection of one out of a struct
              -- field, or a fresh tagged value whose payload arguments are
              -- themselves drawn expressions (§6.2's left-to-right order).
              let uses := indicesWhere Γ (fun b => b.ty == .enum e)
              if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
              let projs := projPlaces D Γ (.enum e)
              if !projs.isEmpty && (← chance 1 3) then return use (← pickPlace (.var 0) projs)
              match D.enums[e]? with
              | some ed =>
                  let k ← nat 0 (ed.variants.length - 1)
                  return mkEnum e k
                    (← ((ed.variants[k]?).getD []).mapM (fun T' => expr D Γ T' fuel))
              | none => return leastValue D (declFuel D) (.enum e)
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
                  if !projs.isEmpty then return use (← pickPlace (.var 0) projs)
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
              let drops := dropPlaces D Γ
              -- A `@drop` or an assignment *at a projection* is the shape this
              -- slice is about (§4.2's partial move), so it is drawn first.
              if !drops.isEmpty && (← chance 2 5) then return drop (← pickPlace (.var 0) drops)
              if !muts.isEmpty && (← chance 3 4) then
                let i ← pick 0 muts
                let b := Γ[i]?.getD ⟨.int .w64 .signed, true⟩
                match b.ty with
                | .struct s =>
                    let slots := (List.range ((D.structs[s]?).map (·.fields.length) |>.getD 0)).filter
                      (fun f => (projSlots D s (((D.structs[s]?).bind (·.fields[f]?)).getD .unit)).contains f)
                    if !slots.isEmpty && (← chance 1 2) then
                      let f ← pick 0 slots
                      let Tf := ((D.structs[s]?).bind (·.fields[f]?)).getD (.int .w64 .signed)
                      return assign (.proj (.var i) f) (← expr D Γ Tf fuel)
                    return assign (.var i) (← expr D Γ b.ty fuel)
                | _ => return assign (.var i) (← expr D Γ b.ty fuel)
              if ← chance 1 3 then
                let To ← weighted (← intTy) [(3, ← intTy), (2, ← floatTy), (1, .bool)]
                return dbg (← expr D Γ To fuel)
              if Γ.isEmpty && !D.structs.isEmpty then
                let T₁ ← binderTy D
                return seq (← expr D Γ T₁ fuel) unitLit
              leaf D Γ .unit 2
          | .struct s =>
              let uses := indicesWhere Γ (fun b => b.ty == .struct s)
              if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
              let projs := projPlaces D Γ (.struct s)
              if !projs.isEmpty && (← chance 1 3) then return use (← pickPlace (.var 0) projs)
              match D.structs[s]? with
              | some sd => return mkStruct s (← sd.fields.mapM (fun T' => expr D Γ T' fuel))
              | none => return mkStruct s []
          -- Unreachable for the same reason `atom`'s array arm is (RUE-2331).
          | .array _ _ => leaf D Γ T 2

/-- (helper) Every subexpression, the expression itself first. -/
def subexprs : Expr → List Expr
  | e@(binop _ e₁ e₂) | e@(seq e₁ e₂) | e@(letIn _ e₁ e₂) =>
      e :: subexprs e₁ ++ subexprs e₂
  | e@(.ite c e₁ e₂) => e :: subexprs c ++ subexprs e₁ ++ subexprs e₂
  | e@(assign _ e₁) | e@(ret e₁) | e@(unop _ e₁) | e@(intCast _ _ e₁)
  | e@(fintrin _ e₁) | e@(dbg e₁) => e :: subexprs e₁
  | e@(call _ args) | e@(mkStruct _ args) | e@(mkEnum _ _ args) =>
      e :: (args.map subexprs).flatten
  | e@(.«match» scrut arms) => e :: subexprs scrut ++ (arms.map subexprs).flatten
  | e => [e]

/-- (helper) The number of nodes. -/
def size (e : Expr) : Nat := (subexprs e).length

mutual
/-- (helper) The rule labels one expression exercises, in the seed corpus's
spellings where it has one and in traversal order. `Γ` lists the binder types
innermost first, as `Print.tyOf` reads them, so a use or a `@drop` is labeled
copy or move by the class of the type its place reaches. -/
def rulesIn (D : Decls) (Γ : List Ty) : Expr → List String
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
       else ["(Arith) §5.8", "§6.4 arithmetic traps"]) ++ rulesIn D Γ e₁ ++ rulesIn D Γ e₂
  | unop op e₁ =>
      (match op with
       | .neg => ["(Neg) §5.8", "§6.4 arithmetic traps"]
       | .not => ["(Not) §5.8"]
       | .bitnot => ["(BitNot) §5.8"]) ++ rulesIn D Γ e₁
  | intCast _ _ e₁ => ["(Int-Cast) §5.8", "(D-Int-Cast-Trap) §6.4"] ++ rulesIn D Γ e₁
  | Expr.panic _ => ["(Panic) §5.8", "(D-Panic) §6.12"]
  | dbg e₁ => ["(Dbg) §5.8"] ++ rulesIn D Γ e₁
  | mkStruct _ args => ["(Struct-Intro) §5.8"] ++ (args.map (rulesIn D Γ)).flatten
  | mkEnum _ _ args =>
      ["(Enum-Intro) §5.5", "(D-Enum-Intro) §6.6"] ++ (args.map (rulesIn D Γ)).flatten
  | .«match» scrut arms =>
      -- The arms are read under their own payload locals, which is what makes
      -- a use of one labeled by the payload's class rather than by whatever
      -- binder happens to sit at that index outside the arm.
      let P : Program := { decls := D, fns := [] }
      let variants := match Print.tyOf P (.int .w64 .signed) Γ scrut with
        | some (.enum e) => ((D.enums[e]?).map EnumDecl.variants).getD []
        | _ => []
      ["(Match) §5.5", "(D-Match) §6.6", "6.3:17", "§5.6 scope exit"] ++
        rulesIn D Γ scrut ++ rulesArms D Γ arms variants
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
      let P : Program := { decls := D, fns := [] }
      ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7"] ++ rulesIn D Γ e₁ ++
        rulesIn D ((Print.tyOf P (.int .w64 .signed) Γ e₁).getD (.int .w64 .signed) :: Γ) e₂
  | assign _ e₁ => ["(Assign) §5.2", "§6.8 overwrite-drop"] ++ rulesIn D Γ e₁
  | seq e₁ e₂ => ["(Seq) §5.3", "§6.7 temporary drop"] ++ rulesIn D Γ e₁ ++ rulesIn D Γ e₂
  | .ite c e₁ e₂ =>
      ["(If) §5.5 join"] ++ rulesIn D Γ c ++ rulesIn D Γ e₁ ++ rulesIn D Γ e₂
  | call _ args => ["(Call) §5.8", "(D-Call) §6.9"] ++ (args.map (rulesIn D Γ)).flatten
  | ret e₁ => ["(Return-Value) §5.7", "(D-Return) §6.9"] ++ rulesIn D Γ e₁
  | _ => []

/-- (helper) The labels a `match`'s arms exercise, walked alongside the
declaration's variant list: arm `j` is read under variant `j`'s payload locals
(`armScope`'s order, which is `armCtx`'s). A `match` whose scrutinee
`Print.tyOf` could not type has no variant list, and its arms are then read
under the enclosing binders alone. -/
def rulesArms (D : Decls) (Γ₀ : List Ty) : List Expr → List (List Ty) → List String
  | [], _ => []
  | a :: rest, [] => rulesIn D Γ₀ a ++ rulesArms D Γ₀ rest []
  | a :: rest, Ts :: Tss => rulesIn D (Ts.reverse ++ Γ₀) a ++ rulesArms D Γ₀ rest Tss
end

/-- (helper) The rule labels a program exercises, deduplicated in traversal
order: `rulesIn` under the empty scope of a no-parameter entry point. -/
def rulesOf (D : Decls) (e : Expr) : List String :=
  (rulesIn D [] e).foldl (fun acc l => if acc.contains l then acc else acc ++ [l]) []

/-- (helper) The result type of a generated program: mostly `int`, so the
value line is usually present, with a float often enough that `main` prints a
shortest round-trip rendering (`3.12:40`) as well, and an aggregate often
enough that `main`'s own observation of the value is exercised — for an enum
that is §6.11's tag read, dropping the **active** variant's payload only
(`6.3:20`), or, where `class(E)` is `Linear`, the explicit `@drop`
`Print.observeValue` emits instead. -/
def resultTy (D : Decls) : G Ty := do
  let k ← nat 1 10
  if k ≤ 2 && !D.structs.isEmpty then
    let s ← nat 0 (D.structs.length - 1)
    return .struct s
  if k ≤ 4 && !D.enums.isEmpty then
    let e ← nat 0 (D.enums.length - 1)
    return .enum e
  weighted (← intTy) [(5, ← intTy), (3, ← floatTy), (1, .bool), (1, .unit)]

/-- (helper) One generated case: a declaration environment and a one-function
program whose entry point takes no parameters and returns the drawn type. The
environment is drawn in three rounds — structs, then enums over them, then a
few more structs that may hold an enum in a field — which is the order the
declarations section describes and the reason no draw can build `3.0:5`'s
cycle. -/
def genCase (seed i : Nat) : G Corpus.Case := do
  let nDecls ← weighted 2 [(2, 1), (4, 2), (3, 3)]
  let D₀ ← genEnv 0 nDecls (Decls.ofStructs [])
  let nEnums ← weighted 1 [(2, 0), (4, 1), (3, 2)]
  let D₁ ← genEnums nEnums D₀
  let nHolders ← weighted 0 [(2, 0), (3, 1)]
  let D ← genEnv D₁.enums.length nHolders D₁
  let depth ← weighted 3 [(4, 2), (3, 3)]
  let T ← resultTy D
  let e ← expr D [] T depth
  return {
    name := s!"gen_{seed}_{i}",
    description := s!"Generated program {i} of seed {seed} ({D.structs.length} struct " ++
      s!"and {D.enums.length} enum declarations, {size e} nodes); " ++
      s!"regenerate with `lake exe ruecore-corpus --gen N --seed {seed}` for any N > {i}.",
    rules := rulesOf D e,
    prog := Program.entry D T e }

/-- (helper) `n` generated cases from `seed`, in order; a pure function of
its arguments. -/
def generate (n seed : Nat) : List Corpus.Case :=
  ((List.range n).mapM (genCase seed)).run' (mkStdGen seed)

end RueCore.Gen
