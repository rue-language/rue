import RueCore.Checker

/-!
# RueCore.Examples — executable demos (the oracle correspondence, in miniature)

`run` is runnable with `#eval`: the Lean semantics is an executable artifact,
exactly as `docs/formal/README.md` demands of the oracle. These examples show
the machine's observable outcomes (value + drop trace + panics + refusals) and
double as seed vectors for a future Lean↔`rue-oracle` differential harness.

A program is a struct environment and a list of function definitions, and
`run P fuel` calls function index `0` with no arguments (§6.12's top-level
result), so a one-expression example is `Program.entry D T e`: `main`,
declared to return `T`, with `e` as its body, over the declarations `D`.
`demoFuel` is the bound every `#eval` here uses; `fuel_mono` (`Soundness.lean`)
is why any sufficient bound gives the same answer.

## The fixture declarations

`structEnv` is one struct environment shared by every example that needs a
struct, so a reader learns it once. It spans the classes §3 assigns and the
two things `3.9` makes a declaration's choice — whether it declares a
destructor, and therefore whether its drops are observable at all and whether
a field may be read out of it (`3.9:34`).

The declarations here are programs, not rules (`xref: examples` for
`scripts/validate-lean-xref-index.py`).
-/

namespace RueCore.Examples

open Expr

/-- The fuel every demo runs at: far more than the deepest of them spends. -/
def demoFuel : Nat := 400

/-- The model every demo runs at: `Float.exactOps` (`Float.lean`), the
constructive instance the corpus and the printer also use. The witnesses in
this file are *executable* demos, so they are pinned at one model rather than
quantified over all of them; because `exactOps` is built from `Nat`/`Int`
arithmetic and never touches Lean's `Float`, pinning them costs no axiom
(`TRUST.md`). The float **trap** witnesses at the bottom of the file are the
exception: they are stated over an arbitrary `FloatModel` and proved from its
laws, which is what makes them claims about IEEE 754 rather than about this
instance. -/
abbrev demoOps : FloatOps := Float.exactOps

/-! ## The fixture vocabulary

Most of these programs are about ownership rather than about a width, so they
are written at one integer type, `int(64, signed)` — Rue's `i64`. The cases
that *are* about a width name it themselves. -/

/-- `int(64, signed)`, Rue's `i64` (helper). -/
abbrev tI64 : Ty := .int .w64 .signed

/-- An `int(64, signed)` literal. Elaboration resolves a literal's width
before the core (`4.1:2`), so the core form carries it (helper). -/
abbrev lit (n : Int) : Expr := .intLit .w64 .signed n

/-- An `int(64, signed)` machine value (§6.1's `n_T`) (helper). -/
abbrev v64 (n : Int) : Val := .int .w64 .signed n

/-- The same as stored contents — what a cell holds and what a drop event
records, now that a cell's contents is a tree with `⊘` at any node
(helper). -/
abbrev c64 (n : Int) : Contents := .int .w64 .signed n

/-- `min_T` and `max_T` at `int(64, signed)`, the bounds §6.4's arithmetic
traps outside of (helper). -/
abbrev min64 : Int := intMin .w64 .signed
/-- The upper `int(64, signed)` bound (helper). -/
abbrev max64 : Int := intMax .w64 .signed

/-! ## The fixture struct declarations -/

/-- `S0`: `@copy struct { x0: i64 }`. Class `Copy`; a `@copy` type declares no
destructor, so its drops are silent and its field is readable. -/
def dCopy : StructDecl := { attr := .copy, fields := [tI64], dtor := false, cls := .copy }

/-- `S1`: `struct { x0: i64 }` with a destructor. Class `Affine`, and the
destructor is what makes each of its drops observable. -/
def dAffine : StructDecl := { attr := .none, fields := [tI64], dtor := true, cls := .affine }

/-- `S2`: `linear struct { x0: i64 }`, no destructor. Class `Linear`, drops
silent. It is *declared* linear, so a projection out of it would select §4.2's
`Declared(d, π)` plan — which this fragment rejects (RUE-2236) — and the
obligation is discharged by a move of the whole value or by `@drop`. -/
def dLinear : StructDecl := { attr := .linear, fields := [tI64], dtor := false, cls := .linear }

/-- `S3`: `linear struct { x0: i64 }` with a destructor. Class `Linear`, drops
observable; nothing may be moved out of it (`3.9:34`), so it is discharged by
`@drop` or by a move of the whole value. -/
def dLinearDtor : StructDecl :=
  { attr := .linear, fields := [tI64], dtor := true, cls := .linear }

/-- `S4`: `struct { x0: i64, x1: S3 }`, no attribute and no destructor. Its
class is `Linear` *through a field* — §3's join, `3.8:58`'s infectiousness —
which is the shape the linear-carrying-struct cases are about. -/
def dCarry : StructDecl :=
  { attr := .none, fields := [tI64, .struct 3], dtor := false, cls := .linear }

/-- `S5`: `struct { x0: i64, x1: S1 }` with a destructor. Class `Affine`;
dropping it runs its own destructor first and then its fields in declaration
order (§6.11), so it is the nesting case. -/
def dOuter : StructDecl :=
  { attr := .none, fields := [tI64, .struct 1], dtor := true, cls := .affine }

/-- `S6`: `@copy struct { x0: i64, x1: i64 }`. Class `Copy`, two fields, so a
use of it copies and a projection of either field is a `Copy` read that leaves
the base `Owned`. -/
def dPair : StructDecl :=
  { attr := .copy, fields := [tI64, tI64], dtor := false, cls := .copy }

/-- `S7`: `struct { x0: S1, x1: S1 }`, no destructor. Class `Affine`; dropping
it drops both fields in declaration order (§6.11) and nothing else. -/
def dTwoAffine : StructDecl :=
  { attr := .none, fields := [.struct 1, .struct 1], dtor := false, cls := .affine }

/-- `S8`: `struct { x0: S1, x1: i64 }`, no destructor. Class `Affine`; its
first field is droppable and its second is `Copy`, so it is the shape a partial
move leaves a readable sibling in (`3.8:53` reads it through the hole). -/
def dAffineInt : StructDecl :=
  { attr := .none, fields := [.struct 1, tI64], dtor := false, cls := .affine }

/-- `S9`: `struct { x0: S7, x1: i64 }`, no destructor. Class `Affine`; it
nests `S7`, so a path into it is two field steps deep. -/
def dNested : StructDecl :=
  { attr := .none, fields := [.struct 7, tI64], dtor := false, cls := .affine }

/-- `S10`: `struct { x0: S3, x1: S1 }`, no destructor. Class `Linear` through
its first field; its second is affine and destructor-bearing, so the two halves
of §5.6's residual obligation are separable at a path. -/
def dCarryAffine : StructDecl :=
  { attr := .none, fields := [.struct 3, .struct 1], dtor := false, cls := .linear }

/-- The fixture environment: every field type names an earlier declaration, so
`WfStructs` holds (checked below) and §3's class assignment is the one
recorded. -/
def structEnv : List StructDecl :=
  [dCopy, dAffine, dLinear, dLinearDtor, dCarry, dOuter, dPair, dTwoAffine,
   dAffineInt, dNested, dCarryAffine]

/-- `S0`'s index in `structEnv`. -/
def sCopy : Nat := 0
/-- `S1`'s index in `structEnv`. -/
def sAffine : Nat := 1
/-- `S2`'s index in `structEnv`. -/
def sLinear : Nat := 2
/-- `S3`'s index in `structEnv`. -/
def sLinearDtor : Nat := 3
/-- `S4`'s index in `structEnv`. -/
def sCarry : Nat := 4
/-- `S5`'s index in `structEnv`. -/
def sOuter : Nat := 5
/-- `S6`'s index in `structEnv`. -/
def sPair : Nat := 6
/-- `S7`'s index in `structEnv`. -/
def sTwoAffine : Nat := 7
/-- `S8`'s index in `structEnv`. -/
def sAffineInt : Nat := 8
/-- `S9`'s index in `structEnv`. -/
def sNested : Nat := 9
/-- `S10`'s index in `structEnv`. -/
def sCarryAffine : Nat := 10

/-- A `Copy` struct literal with the given payload. -/
def resC (e : Expr) : Expr := mkStruct sCopy [e]
/-- An `Affine`, destructor-bearing struct literal. -/
def resA (e : Expr) : Expr := mkStruct sAffine [e]
/-- A `Linear` struct literal whose field can be read out. -/
def resL (e : Expr) : Expr := mkStruct sLinear [e]
/-- A `Linear`, destructor-bearing struct literal. -/
def resLD (e : Expr) : Expr := mkStruct sLinearDtor [e]

/-- A program over the fixture declarations, entered at a no-parameter `main`
returning `T`. -/
def prog (T : Ty) (e : Expr) : Program := Program.entry (Decls.ofStructs structEnv) T e

/-- A program with no struct declarations at all, for the scalar examples. -/
def scalarProg (T : Ty) (e : Expr) : Program := Program.entry (Decls.ofStructs []) T e

/-! ## The float vocabulary (§2's `float(w)`, §5.8, §6.4)

A float literal carries a **decimal** rather than a datum (`Syntax.lean`):
`3.12:9` makes the value the correctly-rounded reading of that decimal at the
form's width, which is the model's `ofLit`. `fl w sig e` is the decimal
`sig · 10^(-e)`, the shape every literal below is written in. A literal is
non-negative, as Rue's grammar writes one; `-0.0` is `neg` applied to `0.0`,
which `3.12:24` makes a sign flip. -/

/-- `float(64)`, Rue's `f64` (helper). -/
abbrev tF64 : Ty := .float .w64
/-- `float(32)`, Rue's `f32` (helper). -/
abbrev tF32 : Ty := .float .w32

/-- The literal `sig · 10^(-e)` at `float(w)` (helper). -/
abbrev fl (w : FloatWidth) (sig e : Nat) : Expr :=
  .floatLit w { sig := sig, negExp := true, e := e }
/-- The literal `sig · 10^e` at `float(w)` (helper). -/
abbrev flE (w : FloatWidth) (sig e : Nat) : Expr :=
  .floatLit w { sig := sig, negExp := false, e := e }

/-- `+inf` as a core expression: `1.0 / 0.0`, which `3.12:22` makes the
infinity of the xor sign. The fragment has no infinite literal — `3.12:10`
rejects one at compile time — so an infinity is always computed. -/
def posInf (w : FloatWidth) : Expr := binop .div (flE w 1 0) (flE w 0 0)
/-- `-inf`, the same division with a negated numerator. -/
def negInf (w : FloatWidth) : Expr := binop .div (unop .neg (flE w 1 0)) (flE w 0 0)
/-- A NaN: `0.0 / 0.0` (`3.12:22`). Its *sign* is `σ_NaN`, a target parameter
(§2), so no case reads it — `@dbg` renders a NaN as `NaN` whatever its sign
(`3.12:42`) and only `@total_cmp` could tell them apart. -/
def aNaN (w : FloatWidth) : Expr := binop .div (flE w 0 0) (flE w 0 0)

/-- `(1.5 + 2.25) * 2.0` at `f64` — (Float-Arith) §5.8 with (D-Float-Arith)
§6.4; every operand is exactly representable, so the answer is exact. -/
def floatArith : Expr := binop .mul (binop .add (fl .w64 15 1) (fl .w64 225 2)) (flE .w64 2 0)

/-- A float binding used twice: `class(float(w)) = Copy` (`3.12:2a`), so the
second use copies and no drop is owed. -/
def floatCopy : Expr := letIn false (fl .w64 15 1) (binop .add (use (.var 0)) (use (.var 0)))

/-- `1.0 / 0.0` — a finite non-zero over a zero is the infinity of the xor
sign (`3.12:22`), **not** a trap: no arithmetic trap rule is stated over a
float redex (§6.4). -/
def floatDivZero : Expr := posInf .w64

/-- `0.0 / 0.0` is `NaN(σ_NaN)` (`3.12:22`), again without trapping. -/
def floatZeroDivZero : Expr := aNaN .w64

/-- Every ordering compare against a NaN is `false` — the two are *unordered*
(`3.12:27`) — and that includes `nan <= nan`, which is why `≈` loses
reflexivity (§6.4). -/
def floatNanUnordered : Expr :=
  seq (dbg (binop .lt (aNaN .w64) (flE .w64 1 0)))
    (seq (dbg (binop .le (aNaN .w64) (aNaN .w64)))
      (seq (dbg (binop .gt (aNaN .w64) (flE .w64 1 0))) (lit 0)))

/-- `-0.0` and `+0.0` compare **equal** and neither is below the other
(`3.12:28`), while `@total_cmp` orders `-0.0` first (`3.12:32`) and `@dbg`
tells them apart (`3.12:42`). Three readings of one pair of data. -/
def floatSignedZeros : Expr :=
  seq (dbg (binop .lt (unop .neg (flE .w64 0 0)) (flE .w64 0 0)))
    (seq (dbg (binop .le (unop .neg (flE .w64 0 0)) (flE .w64 0 0)))
      (seq (dbg (unop .neg (flE .w64 0 0)))
        (seq (dbg (binop .totalCmp (unop .neg (flE .w64 0 0)) (flE .w64 0 0))) (lit 0))))

/-- The infinities sit at the ends of the ordering (`3.12:27`), and `@dbg`
spells them `inf` and `-inf` (`3.12:42`). -/
def floatInfinities : Expr :=
  seq (dbg (binop .gt (posInf .w64) (flE .w64 1 0)))
    (seq (dbg (binop .lt (negInf .w64) (flE .w64 0 0)))
      (seq (dbg (posInf .w64)) (seq (dbg (negInf .w64)) (lit 0))))

/-- `@float_to_int` truncates **toward zero** (`3.12:17`), on both signs. -/
def floatToIntTrunc : Expr :=
  seq (dbg (fintrin (.floatToInt .w32 .signed) (fl .w64 29 1)))
    (seq (dbg (fintrin (.floatToInt .w32 .signed) (unop .neg (fl .w64 29 1)))) (lit 0))

/-- `@float_to_int` of a NaN traps (`3.12:18`), with `↯overflow` — the same
category §6.12 already lists, reported as `integer overflow` (`8.1:7`). The
`@dbg` before it is what makes the stdout the trap carries comparable. -/
def floatToIntTrapNan : Expr :=
  seq (dbg (lit 1)) (fintrin (.floatToInt .w32 .signed) (aNaN .w64))

/-- `@float_to_int` of `+inf` traps: `3.12:18`'s guard "admits both infinities
as failures". -/
def floatToIntTrapInf : Expr := fintrin (.floatToInt .w32 .signed) (posInf .w64)

/-- `@float_to_int` of a value whose truncation leaves the target's range
traps — the third arm of `3.12:18`'s premise, here at `i8`. -/
def floatToIntTrapRange : Expr := fintrin (.floatToInt .w8 .signed) (flE .w64 1000 0)

/-- `@int_to_float` rounds (`3.12:16`): `2^53 + 1` has no `f64`, so it lands
on `2^53`, and the shortest round-trip rendering shows it. -/
def intToFloatRounds : Expr :=
  fintrin (.intToFloat .w64) (intLit .w64 .signed 9007199254740993)

/-- `@float_cast` narrowing to `f32` rounds (`3.12:19`), and the `f32` prints
the digits that identify it *as an `f32`* (`3.12:40`). -/
def floatCastNarrow : Expr := fintrin (.floatCast .w32) (fl .w64 1 1)

/-- `@float_cast` widening is **exact** (`3.12:19`), which is why the `f64`
rendering of an `f32` `0.1` shows the whole of the `f32` value. -/
def floatCastWiden : Expr := fintrin (.floatCast .w64) (fl .w32 1 1)

/-- The four exact rounding intrinsics on a half-way value, and on both signs:
`@round` rounds ties **away** from zero (`3.12:36`), which is where it parts
from `rnd_w`'s ties-to-even. -/
def floatRoundHalfway : Expr :=
  seq (dbg (fintrin (.roundOp (.round .round)) (fl .w64 25 1)))
    (seq (dbg (fintrin (.roundOp (.round .round)) (unop .neg (fl .w64 25 1))))
      (seq (dbg (fintrin (.roundOp (.round .floor)) (unop .neg (fl .w64 15 1))))
        (seq (dbg (fintrin (.roundOp (.round .ceil)) (unop .neg (fl .w64 15 1))))
          (seq (dbg (fintrin (.roundOp (.round .trunc)) (unop .neg (fl .w64 15 1))))
            (lit 0)))))

/-- `@sqrt` is correctly rounded (`3.12:35`), so `√2` prints all seventeen
digits that identify it. -/
def floatSqrt : Expr := fintrin (.roundOp .sqrt) (flE .w64 2 0)

/-- `@total_cmp` is a **total** order (`3.12:32`): `-0.0` precedes `+0.0`, a
datum equals itself, and a larger value follows. Every operand here is a float
*literal*, so none of them is a NaN, and no other seed case applies
`@total_cmp` at all; the generator draws its operands as literals too
(`Gen.lean`), so it cannot produce a NaN operand either. That is what keeps the
corpus target-independent: `@total_cmp` is the only form that can see a NaN's
sign, and that sign is `σ_NaN`, a target parameter (§2), so the answer there
would differ between x86-64 and AArch64. -/
def totalCmpOrder : Expr :=
  seq (dbg (binop .totalCmp (unop .neg (flE .w64 0 0)) (flE .w64 0 0)))
    (seq (dbg (binop .totalCmp (flE .w64 1 0) (flE .w64 1 0)))
      (seq (dbg (binop .totalCmp (flE .w64 1 0) (flE .w64 0 0))) (lit 0)))

/-- `3.12:41`'s two layouts and the boundary between them: `1e15` is
positional, `1e16` is scientific, and `1e-6` is just past the low end. -/
def floatDbgLayouts : Expr :=
  seq (dbg (flE .w64 1 15))
    (seq (dbg (flE .w64 1 16))
      (seq (dbg (fl .w64 1 6)) (seq (dbg (fl .w64 1 5)) (lit 0))))

/-- `3.12:40`: an `f32` prints the digits that identify it as an `f32`, not
the digits of the `f64` with the same numeric value. -/
def f32Shortest : Expr :=
  letIn false (binop .div (flE .w32 1 0) (flE .w32 3 0))
    (seq (dbg (use (.var 0))) (seq (dbg (fl .w32 1 1)) (lit 0)))

/-! ## Scalars, resources, and the ownership discipline -/

/-- `let x = 2 + 3; x + x` — well-typed scalar flow. -/
def scalars : Expr :=
  letIn false (binop .add (lit 2) (lit 3)) (binop .add (use (.var 0)) (use (.var 0)))

/-- An affine resource silently dropped at scope exit — legal, and the trace
shows the drop (its destructor). -/
def affineDrop : Expr :=
  letIn false (resA (lit 7)) (lit 1)

/-- A linear resource moved exactly once and then discharged — legal. The
move is (Use-Move) §5.1 at a whole place, which transfers the obligation to
the new binding; `@drop` (§5.3) is what finally discharges it. `S2` declares no
destructor, so nothing is observable. -/
def linearConsumed : Expr :=
  letIn false (resL (lit 7))
    (letIn false (use (.var 0)) (seq (drop (.var 0)) (lit 7)))

/-- A linear resource leaked at scope exit — the machine REFUSES
(`linearLeak`), and no typing derivation exists for it. -/
def linearLeaked : Expr :=
  letIn false (resL (lit 7)) (lit 1)

/-- Use after move: `let r = S1{1}; let s = r; @drop(r)` — refused
dynamically, rejected statically. -/
def useAfterMove : Expr :=
  letIn false (resA (lit 1))
    (letIn false (use (.var 0)) (seq (drop (.var 1)) (lit 0)))

/-- Reinitialization: discharge the value, assign a new one back in, discharge
that — legal (`3.8:55`). -/
def reinit : Expr :=
  letIn true (resL (lit 1))
    (seq (drop (.var 0))
      (seq (assign (.var 0) (resL (lit 2)))
        (seq (drop (.var 0)) (lit 2))))

/-- Branch join: discharge a linear value in only one arm — no typing
derivation exists (the §5.5 join rejects it); dynamically it leaks on the
`false` path. -/
def linearHalfConsumed : Expr :=
  letIn false (resL (lit 9))
    (seq (ite (boolLit false) (seq (drop (.var 0)) (lit 0)) (lit 0))
      (lit 0))

/-- Overflow trap (§6.4): `max_T + 1` panics. -/
def overflow : Expr :=
  binop .add (lit max64) (lit 1)

/-- Division by zero panics. -/
def divZero : Expr :=
  binop .div (lit 1) (lit 0)

/-! ## Structs with fields (RUE-2230)

Each of these needs more than one field, or a field that is itself a struct,
so they are where §3's join and §6.11's order become visible. -/

/-- A struct that is `Linear` only through a field (`S4`), left to scope exit:
§5.6's obligation is undischarged, so the machine refuses. -/
def structLinearFieldLeaked : Expr :=
  letIn false (mkStruct sCarry [lit 1, resLD (lit 2)]) (lit 0)

/-- The same value discharged by `@drop` (§5.3's only non-move discharge of a
linear obligation): the whole value's glue runs, so the linear field's
destructor prints. -/
def structLinearFieldDropped : Expr :=
  letIn false (mkStruct sCarry [lit 1, resLD (lit 2)]) (seq (drop (.var 0)) (lit 0))

/-- A nested destructor-bearing struct at scope exit: §6.11 runs the outer
destructor first and then the fields in declaration order, so the trace is
`1` then `2`. -/
def structNestedDrop : Expr :=
  letIn false (mkStruct sOuter [lit 1, resA (lit 2)]) (lit 9)

/-- The §5.5 join on a linear-carrying struct entry: discharged in one arm
only, which `3.8:50` makes ill-formed; on the path taken it leaks. -/
def structJoinDisagrees : Expr :=
  letIn false (mkStruct sCarry [lit 1, resLD (lit 2)])
    (seq (ite (boolLit false) (drop (.var 0)) unitLit) (lit 0))

/-- A `@copy` struct used twice: contraction is legal at `Copy` (§3), and
nothing is ever dropped. -/
def structCopyTwice : Expr :=
  letIn false (mkStruct sPair [lit 5, lit 6])
    (binop .add (use (.proj (.var 0) 0)) (use (.proj (.var 0) 0)))

/-- Two destructor-bearing fields in one struct with no destructor of its own:
scope exit drops them in **declaration** order (§6.11), so the trace is `1`
then `2`. -/
def structFieldOrder : Expr :=
  letIn false (mkStruct sTwoAffine [resA (lit 1), resA (lit 2)]) (lit 0)

/-! ## Paths, projections and partial moves (RUE-2231)

Each of these is about a place that is not a whole binding: a field moved out
on its own (`3.8:22`), the residue that then drops at scope exit (`3.8:60`),
and the premises §5.1 and §5.3 put on which projections may be moved. -/

/-- Move one field out of a two-field struct, discharge it, and let the rest
drop at scope exit: the trace shows the moved field's destructor at the
`@drop` and the *remaining* field's at the scope exit — the `⊘`-skip of
§6.11 means the moved one is not dropped twice. -/
def partialMoveResidue : Expr :=
  letIn false (mkStruct sTwoAffine [resA (lit 1), resA (lit 2)])
    (letIn false (use (.proj (.var 0) 0)) (seq (drop (.var 0)) (lit 9)))

/-- Move a field out and then the whole value: `fully-owned(Σ, p)` fails at
the whole place, so (Use-Move) §5.1 has no derivation (`3.8:26`, the
compiler's E0205 "use of partially moved value"). -/
def partialThenWhole : Expr :=
  letIn false (mkStruct sTwoAffine [resA (lit 1), resA (lit 2)])
    (letIn false (use (.proj (.var 0) 0))
      (seq (drop (.var 0)) (letIn false (use (.var 1)) (seq (drop (.var 0)) (lit 9)))))

/-- Move a field out of a value whose type declares a destructor: `3.9:34`
(E0456) forbids it, because the destructor runs on the whole value and would
observe the hole. -/
def partialUnderDtor : Expr :=
  letIn false (mkStruct sOuter [lit 1, resA (lit 2)])
    (letIn false (use (.proj (.var 0) 1)) (seq (drop (.var 0)) (lit 9)))

/-- A `Copy` sibling read through a partially moved base: `Σ(p) = Owned` holds
at the base although a path under it is `MovedOut` (§5 preamble), so the read
is legal (`3.8:53`). -/
def copyThroughPartial : Expr :=
  letIn false (mkStruct sAffineInt [resA (lit 1), lit 7])
    (letIn false (use (.proj (.var 0) 0))
      (seq (drop (.var 0)) (use (.proj (.var 1) 1))))

/-- `@drop` at a field, then `@drop` of the whole: §5.3 asks only
`Σ(p) = Owned` of the second, and §6.11's walk drops the owned residue and
skips the hole. -/
def dropFieldThenWhole : Expr :=
  letIn false (mkStruct sTwoAffine [resA (lit 1), resA (lit 2)])
    (seq (drop (.proj (.var 0) 0)) (seq (drop (.var 0)) (lit 9)))

/-- Reinitialize a moved-out field and then move the whole: assignment at a
path restores the subtree to `Owned` (`3.8:55`), so `fully-owned` holds again
and the whole value may be moved. -/
def reinitField : Expr :=
  letIn true (mkStruct sTwoAffine [resA (lit 1), resA (lit 2)])
    (letIn false (use (.proj (.var 0) 0))
      (seq (drop (.var 0))
        (seq (assign (.proj (.var 1) 0) (resA (lit 5)))
          (letIn false (use (.var 1)) (seq (drop (.var 0)) (lit 9))))))

/-- Overwrite a live affine field: §6.8's overwrite-drop runs the old field's
destructor before the store, and the new one drops at scope exit. -/
def overwriteField : Expr :=
  letIn true (mkStruct sTwoAffine [resA (lit 1), resA (lit 2)])
    (seq (assign (.proj (.var 0) 0) (resA (lit 5))) (lit 9))

/-- A field moved out in one arm of an `if` only: the §5.5 join sends that
path to `MovedOut` while the sibling stays `Owned`, and the machine drops
whatever the taken path left (`3.8:60`). -/
def partialMoveOneArm : Expr :=
  letIn false (mkStruct sTwoAffine [resA (lit 1), resA (lit 2)])
    (seq (ite (boolLit true) (drop (.proj (.var 0) 0)) unitLit) (lit 9))

/-- The same on the path that does **not** move the field: the drop is
path-specific, so both fields drop at scope exit and the observable output is
the same either way. -/
def partialMoveOtherArm : Expr :=
  letIn false (mkStruct sTwoAffine [resA (lit 1), resA (lit 2)])
    (seq (ite (boolLit false) (drop (.proj (.var 0) 0)) unitLit) (lit 9))

/-- A path two field steps deep: `@drop(v.x0.x1)` moves exactly that leaf, and
the scope exit drops the rest of the tree in declaration order. -/
def deepPath : Expr :=
  letIn false (mkStruct sNested [mkStruct sTwoAffine [resA (lit 1), resA (lit 2)], lit 3])
    (seq (drop (.proj (.proj (.var 0) 0) 1)) (lit 9))

/-- The RUE-1591 idiom at a path: consume exactly the **linear** field of an
infectious carrier and let the non-linear residue drop. §5.6's obligation is
keyed on the residual state, so the scope exit is legal and the affine
sibling's destructor prints. -/
def linearFieldResidue : Expr :=
  letIn false (mkStruct sCarryAffine [resLD (lit 1), resA (lit 2)])
    (seq (drop (.proj (.var 0) 0)) (lit 9))

/-- The premise that forbids the other order: `@drop` of the **affine** field
first leaves a still-owned linear sub-place under a partially moved place, and
(@Drop) §5.3's last premise rejects the whole-value drop that would silently
destroy it (E0406). -/
def linearFieldStranded : Expr :=
  letIn false (mkStruct sCarryAffine [resLD (lit 1), resA (lit 2)])
    (seq (drop (.proj (.var 0) 1)) (seq (drop (.var 0)) (lit 9)))

/-- The §5.5 join of a whole move against a partial one, on a carrier whose
only linear content is the field the other arm consumed: both paths leave the
obligation discharged, so the join is `MovedOut` rather than ill-formed — the
residual reading of `3.8:50`, which is what the compiler does. -/
def joinWholeAgainstPartial : Expr :=
  letIn false (mkStruct sCarryAffine [resLD (lit 1), resA (lit 2)])
    (seq (ite (boolLit true) (drop (.var 0)) (drop (.proj (.var 0) 0))) (lit 9))

/-- The same shape where the linear field survives on one path: `3.8:50` makes
the join ill-formed (the compiler's E0443, "not consumed on all paths"). -/
def joinLinearFieldOneArm : Expr :=
  letIn false (mkStruct sCarryAffine [resLD (lit 1), resA (lit 2)])
    (seq (ite (boolLit true) (drop (.proj (.var 0) 0)) unitLit) (lit 9))

/-- (Assign)'s `3.8:77` premise at a **root** whose type carries a linear
value, past a field `@drop` that already took the linear part out. §5.2 keys
the premise on the destination's *type* — `Σ1(p) = MovedOut ∨
¬carries_linear(T)`, `overwriteOk` — so the reassignment is ill-formed although
the residue carries nothing, and the compiler agrees (E0493, "assignment would
overwrite a live linear value"). The machine does **not** refuse: its
`linearOverwrite` monitor reads the residue, because the residue is what the
overwrite-drop is about to walk, and there is no live linear value in it. So
this is a rejected program that runs to completion — the third shape of that
kind in the corpus, beside `3.9:34`'s and (@Drop)'s statics-only premises. -/
def overwritePastPartialLinear : Expr :=
  letIn true (mkStruct sCarryAffine [resLD (lit 1), resA (lit 2)])
    (seq (drop (.proj (.var 0) 0))
      (seq (assign (.var 0) (mkStruct sCarryAffine [resLD (lit 5), resA (lit 6)]))
        (seq (drop (.var 0)) (lit 9))))

/-- `S11`: `struct { x0: S10, x1: i64 }`, no attribute and no destructor.
Class `Linear` through `S10`, so a **field** of it is a linear-carrying place
one field step down — which is where (Assign)'s type-keyed premise is tested
below a root. Held out of `structEnv` so that only the one case that needs it
prints it. -/
def dNestCarry : StructDecl :=
  { attr := .none, fields := [.struct 10, tI64], dtor := false, cls := .linear }

/-- `S11`'s index in `structEnv ++ [dNestCarry]`. -/
def sNestCarry : Nat := 11

/-- A program over the fixture declarations plus `S11`. -/
def nestCarryProg (T : Ty) (e : Expr) : Program :=
  Program.entry (Decls.ofStructs (structEnv ++ [dNestCarry])) T e

/-- The same divergence one field step down: `@drop(v.x0.x0)` takes the linear
leaf out and `v.x0 = S10{…}` is still ill-formed, because `S10` — the
*destination's* declared type, not its residue — carries a linear value. The
compiler reports E0493 here too. -/
def overwriteFieldPastPartialLinear : Expr :=
  letIn true (mkStruct sNestCarry
      [mkStruct sCarryAffine [resLD (lit 1), resA (lit 2)], lit 3])
    (seq (drop (.proj (.proj (.var 0) 0) 0))
      (seq (assign (.proj (.var 0) 0)
            (mkStruct sCarryAffine [resLD (lit 5), resA (lit 6)]))
        (seq (drop (.var 0)) (lit 9))))

/-! ## Widths, the operator set, and the intrinsics (RUE-2282)

The cases above are about ownership and are written at `int(64, signed)`.
These are about the leaves themselves: each §6.4 trap at the narrowest width
that reaches it, the operators that never trap, `@intCast`, and the two
intrinsics whose effect is on §6.12's observable outcome rather than on a
value. -/

/-- `max_T + 1` at `i8`: (D-Arith-Trap) §6.4 at the narrowest width, where the
bound is 127 rather than `2^63 - 1`. -/
def i8Overflow : Expr :=
  binop .add (intLit .w8 .signed (intMax .w8 .signed)) (intLit .w8 .signed 1)

/-- `0 - 1` at `u8`: the same trap reached downward. Rue has no wrapping
subtraction outside the `@wrapping_*` intrinsics (`3.1:6`), so an unsigned
type's own zero is a trap boundary. -/
def u8Underflow : Expr :=
  binop .sub (intLit .w8 .unsigned 0) (intLit .w8 .unsigned 1)

/-- `min_T / -1` at `i8`: (D-Div-Overflow) §6.4, the quotient that is not
representable. -/
def i8DivMinByNegOne : Expr :=
  binop .div (intLit .w8 .signed (intMin .w8 .signed)) (intLit .w8 .signed (-1))

/-- `min_T * -1` at `i64`: (D-Arith-Trap) §6.4 again, because `-min_T` is one
past `max_T` at every signed width (`8.1:3` lists multiplication among the
operations that may overflow).

**The compiler disagrees with this one**, and the model is right: its
constant folder wraps the product and the program prints `min_T` and exits 0,
where every non-constant spelling of the same multiplication traps. RUE-2318.
The corpus seeds the shape so the bridge is red on it until that is fixed. -/
def i64MinTimesNeg1 : Expr :=
  binop .mul (intLit .w64 .signed (intMin .w64 .signed)) (intLit .w64 .signed (-1))

/-- `min_T % -1` at `i8`: §6.4 traps here too, although the mathematical
remainder is `0` — the hardware `idiv` faults on it. -/
def i8RemMinByNegOne : Expr :=
  binop .rem (intLit .w8 .signed (intMin .w8 .signed)) (intLit .w8 .signed (-1))

/-- `%` by zero: `↯rem-zero`, §6.12's own category beside `div-zero`. -/
def i8RemZero : Expr :=
  binop .rem (intLit .w8 .signed 5) (intLit .w8 .signed 0)

/-- `@intCast` out of the target's range: `4.13:28`'s trap, §6.4's
`(D-Int-Cast-Trap)`. -/
def u8CastOutOfRange : Expr := intCast .w8 .unsigned (intLit .w32 .signed 300)

/-- `@intCast` whose value fits: the conversion carries it across. -/
def u8CastInRange : Expr := intCast .w8 .unsigned (intLit .w32 .signed 200)

/-- `1 << 8` at `u8`: the shift amount is reduced modulo the width
((D-Shl) §6.4, `4.3a:10`), so this shifts by zero and does **not** trap. -/
def u8ShiftMasks : Expr :=
  binop .shl (intLit .w8 .unsigned 1) (intLit .w8 .unsigned 8)

/-- `(12 & 10) | ~240` at `u8`: (D-Bit) §6.4 over the `w`-bit pattern, which
is `8 | 15 = 15`. The complement is the case that shows the width: `~240` is
`15` at `u8` and a large negative number at any wider signed type. -/
def u8Bitwise : Expr :=
  binop .bitOr (binop .bitAnd (intLit .w8 .unsigned 12) (intLit .w8 .unsigned 10))
    (unop .bitnot (intLit .w8 .unsigned 240))

/-- `-(3 * 7)` at `i16`: (D-Arith)'s unary case on a signed type, which is the
only type (Neg) §5.8 admits. -/
def i16Negate : Expr :=
  unop .neg (binop .mul (intLit .w16 .signed 3) (intLit .w16 .signed 7))

/-- `max_T > 0` at `u64`: an *unsigned* compare of a value whose signed
reading would be negative, so the case tells the two orderings apart. -/
def u64Compare : Expr :=
  binop .gt (intLit .w64 .unsigned (intMax .w64 .unsigned)) (intLit .w64 .unsigned 0)

/-- `!(3 <= 3)`: (Not) §5.8 on `bool`, the one type it admits (`4.4:2`). -/
def boolNegate : Expr := unop .not (binop .le (lit 3) (lit 3))

/-- `@dbg` of each scalar the fragment renders (§5.8's (Dbg)): a negative
`i8`, a large `u64`, and a `bool`. -/
def dbgScalars : Expr :=
  seq (dbg (intLit .w8 .signed (-5)))
    (seq (dbg (intLit .w64 .unsigned 42))
      (seq (dbg (boolLit true)) (lit 0)))

/-- A `@dbg` between two drops: the two observation channels are one trace, so
the line comes out where it happened (§6.12's observable output). -/
def dbgBetweenDrops : Expr :=
  letIn false (resA (lit 1))
    (seq (drop (.var 0)) (seq (dbg (lit 2)) (letIn false (resA (lit 3)) (lit 0))))

/-- A user `@panic` after an affine drop: the destructor has already run, so
the trap carries it out. §5.7 exempts the `⊥_panic` edge from §5.6's
obligation and §6.12 abandons the configuration, so the binding's own scope
exit never happens — the drop that shows is the explicit one. -/
def panicAfterDrop : Expr :=
  letIn false (resA (lit 7)) (seq (drop (.var 0)) (panic "boom"))

/-- A `@dbg` before a machine trap: the same claim for a trap the program did
not ask for. -/
def dbgBeforeTrap : Expr :=
  seq (dbg (lit 1)) (binop .div (lit 1) (lit 0))

/-- The same `@panic` past a live affine binding, with no explicit `@drop`:
§5.7 exempts the `⊥_panic` edge from §5.6's obligation, so the binding's
scope exit never runs and its destructor never fires. The compiler agrees.
This is the contrast `panicAfterDrop` is read against, and it is why the line
that survives the trap there is the explicit drop's. -/
def panicPastAffine : Expr :=
  letIn false (resA (lit 7)) (panic "boom")

/-- The same past a live **linear** binding, which is the class where
`Typed.panic` and `Typed.ret` actually differ: `ret` would need
`NoOwnedLinear` here and `panic` does not, so the judgment derives this
program (`panicPastLinear_typed`) and the machine runs it to a trap with an
empty trace. `check` rejects it all the same — its state choice hands the
`let`'s leak check the incoming `Owned` state — which is the third thing the
algorithm's narrowness costs (`Checker.lean`). The compiler accepts and runs
it: `panic: boom`, exit 101, nothing on stdout, so `S3`'s destructor does not
run there either (verified by hand). -/
def panicPastLinear : Expr :=
  letIn false (resLD (lit 7)) (panic "boom")

/-! ## Enums and `match` (RUE-2320)

The fixture enums, the holder struct that puts one in a field, and the eight
programs the bridge seeds. Every one was run against the compiler before it was
committed; the probe it reproduces is named in its doc-comment.

`class(E)` is the payload join over **every** variant (`6.3:19`), so `E1` — one
variant of which carries a declared-`linear` payload — is itself `Linear`
whichever variant a value holds, which is the fact `enum_carriesLinear_iff`
states and the fact probe e11 pins against the compiler (E0406). -/

/-- `E0`: `enum { K0(S1), K1 }`. Class `Affine` through `S1`, whose destructor
is what makes a payload's drop observable. -/
def eAffine : EnumDecl := { variants := [[.struct 1], []], cls := .affine }

/-- `E1`: `enum { K0(S3), K1 }`. Class `Linear` through `S3`, a declared-linear
payload with a destructor — the must-consume enum. -/
def eLinear : EnumDecl := { variants := [[.struct 3], []], cls := .linear }

/-- `E2`: `enum { K0(S1, S1), K1 }`. Two payload components, which is what pins
the arm's newest-first drop order. -/
def ePair : EnumDecl := { variants := [[.struct 1, .struct 1], []], cls := .affine }

/-- `E3`: `enum { K0, K1 }`, discriminant-only. The empty join is `Copy`
(`6.3:19`, `3.8:2`), so a value of it may be matched any number of times. -/
def eTag : EnumDecl := { variants := [[], []], cls := .copy }

/-- `E4`: `enum { K0(i64), K1 }`. A `Copy` payload, so the enum is `Copy` too
and its payload binding drops nothing at the arm's end. -/
def eInt : EnumDecl := { variants := [[tI64], []], cls := .copy }

/-- `S11`: `struct { x0: E0, x1: S1 }`, no destructor. Class `Affine`; it is
what makes a `match` scrutinee a **projection**, so the partial move the match
takes is one field of a struct whose sibling still drops at scope exit. -/
def dHolder : StructDecl :=
  { attr := .none, fields := [.enum 0, .struct 1], dtor := false, cls := .affine }

/-- The declaration environment the enum cases run in: the fixture structs plus
`S11`, and the five enums above. -/
def enumDecls : Decls :=
  { structs := structEnv ++ [dHolder], enums := [eAffine, eLinear, ePair, eTag, eInt] }

/-- `E0`'s index. -/
def eAffineIdx : Nat := 0
/-- `E1`'s index. -/
def eLinearIdx : Nat := 1
/-- `E2`'s index. -/
def ePairIdx : Nat := 2
/-- `E3`'s index. -/
def eTagIdx : Nat := 3
/-- `E4`'s index. -/
def eIntIdx : Nat := 4
/-- `S11`'s index. -/
def sHolder : Nat := 11

/-- A program over the enum declarations, entered at a no-parameter `main`
returning `T`. -/
def enumProg (T : Ty) (e : Expr) : Program := Program.entry enumDecls T e

/-- **Probe e1.** An affine payload with a destructor, matched and bound, and
the binding dropped at the arm's end (`6.3:17`'s timing): `10`, then the
payload's `1` when the arm closes, then `20`, then the value. The enum binding
itself drops nothing at scope exit — the match moved it out (§6.3's `⊘`), which
is what keeps `6.3:20` from dropping the payload twice. -/
def enumMatchAffine : Expr :=
  letIn false (mkEnum eAffineIdx 0 [resA (lit 1)])
    (letIn false
      («match» (use (.var 0)) [seq (dbg (lit 10)) (lit 5), lit 6])
      (seq (dbg (lit 20)) (use (.var 0))))

/-- **Probe e1b.** The same enum never matched, dropped at scope exit, with a
discriminant-only value beside it: `20`, then the **active** payload's `1`, then
the value. The `K1` binding drops nothing at all — an inactive variant has no
storage and a discriminant-only active one has no payload (`6.3:20`). -/
def enumDropUnmatched : Expr :=
  letIn false (mkEnum eAffineIdx 0 [resA (lit 1)])
    (letIn false (mkEnum eAffineIdx 1 [])
      (seq (dbg (lit 20)) (lit 7)))

/-- **Probe e2.** A `Linear`-payload enum consumed by a `match` in one arm of an
`if` only: §5.5's join sees the binding `MovedOut` on one path and `Owned` at a
`Linear` type on the other, so it is ill-formed (E0443). `class(E1)` is what
makes the `Owned` side residual — the payload paths are not tracked, so §5.6
reads the type (`6.3:19`). -/
def enumMatchOneArm : Expr :=
  letIn false (mkEnum eLinearIdx 0 [resLD (lit 1)])
    (seq
      (ite (boolLit true)
        («match» (use (.var 0)) [drop (.var 0), unitLit])
        unitLit)
      (lit 7))

/-- **Probe e3.** An arm binds a `Linear` payload and leaves it: §5.6's check at
the arm's end is the leak (E0406 on the binding), and it is `Typed.letIn`'s own
premise read over a payload tuple (`6.3:17`). -/
def enumArmLeaksPayload : Expr :=
  letIn false (mkEnum eLinearIdx 0 [resLD (lit 1)])
    («match» (use (.var 0)) [lit 5, lit 6])

/-- **Probe e3b.** The same arm with the payload discharged: accepted, and the
`@drop` runs the payload's destructor (`1`) before the value (`5`). Consuming the
payload is what discharges the *enum's* obligation too — §5.5's "consuming the
enum, for example by a `match` that binds and consumes the payload". -/
def enumArmDropsPayload : Expr :=
  letIn false (mkEnum eLinearIdx 0 [resLD (lit 1)])
    («match» (use (.var 0)) [seq (drop (.var 0)) (lit 5), lit 6])

/-- **Probe e4.** A discriminant-only enum and a `Copy`-payload one, each
matched **twice**: both are `Copy` by the empty and the scalar join (`6.3:19`),
so the scrutinee is a (Use-Copy) read that leaves the binding `Owned`, and the
payload binding drops nothing. `2 + 20 + 4 + 104 = 130`. -/
def enumCopyMatchedTwice : Expr :=
  letIn false (mkEnum eTagIdx 1 [])
    (letIn false (mkEnum eIntIdx 0 [lit 4])
      (letIn false («match» (use (.var 1)) [lit 1, lit 2])
        (letIn false («match» (use (.var 2)) [lit 10, lit 20])
          (letIn false («match» (use (.var 2)) [use (.var 0), lit 0])
            (letIn false
              («match» (use (.var 3)) [binop .add (use (.var 0)) (lit 100), lit 0])
              (binop .add (binop .add (binop .add (use (.var 3)) (use (.var 2)))
                (use (.var 1))) (use (.var 0))))))))

/-- **Probe e5.** The scrutinee is a **projection**: `v0.x0` of a struct holding
the enum in one field and an affine sibling in the other. The match takes the
partial move of `3.8:22` at that path, so the scope exit drops the struct's
fields in declaration order with the moved one skipped — `10`, the payload's `1`
at the arm's end, `20`, then the sibling's `2`, then the value. -/
def enumMatchProjection : Expr :=
  letIn false (mkStruct sHolder [mkEnum eAffineIdx 0 [resA (lit 1)], resA (lit 2)])
    (letIn false
      («match» (use (.proj (.var 0) 0)) [seq (dbg (lit 10)) (lit 5), lit 6])
      (seq (dbg (lit 20)) (use (.var 0))))

/-- **Probe e7.** Two payload bindings, both affine with destructors: at the
arm's end they drop **newest-first**, so the second component goes before the
first — `10`, `2`, `1`, `20`, then the value. That is (D-EndScope)'s order
(§6.1, `3.9:4`) read over the cells (D-Match) §6.6 appended. -/
def enumTwoPayloadBindings : Expr :=
  letIn false (mkEnum ePairIdx 0 [resA (lit 1), resA (lit 2)])
    (letIn false
      («match» (use (.var 0)) [seq (dbg (lit 10)) (lit 5), lit 6])
      (seq (dbg (lit 20)) (use (.var 0))))

/-! ## Calls, frames, and `return` (RUE-2233)

Each of these needs more than one function, so it is written as a whole
`Program` rather than an `Expr`. Function index `0` is the entry point. -/

/-- A plain call: `f0()` calls `f1(2, 3)`, which adds its parameters. The
first parameter is the outermost binder, so it is `use (.var 1)` inside the body. -/
def callPlain : Program :=
  { decls := Decls.ofStructs [],
    fns := [{ params := [], ret := tI64, body := call 1 [lit 2, lit 3] },
            { params := [⟨tI64, false⟩, ⟨tI64, false⟩], ret := tI64,
              body := binop .add (use (.var 1)) (use (.var 0)) }] }

/-- An early `return` past two live affine bindings: the frame unwinds
newest-first (§6.9's (D-Return)), so the trace is `4` then `3`, then the
value `7`. -/
def returnPastAffine : Program :=
  prog tI64
    (letIn false (resA (lit 3))
      (letIn false (resA (lit 4))
        (ret (lit 7))))

/-- An early `return` past a live **linear** binding: §5.6's obligation is
undischarged at the `⊥_exit` edge, so (Return-Value) §5.7 rejects it, and the
unwind refuses with `linearLeak`. -/
def returnPastLinear : Program :=
  prog tI64 (letIn false (resL (lit 5)) (ret (lit 1)))

/-- A by-value affine argument the callee never consumes: the callee's frame
owes its drop, and (D-Return-Value)'s `run-all-scope-drops` runs it at the
frame pop — `2`, then the value `1`. -/
def paramDroppedAtPop : Program :=
  { decls := Decls.ofStructs structEnv,
    fns := [{ params := [], ret := tI64, body := call 1 [resA (lit 2)] },
            { params := [⟨.struct sAffine, false⟩], ret := tI64, body := lit 1 }] }

/-- A by-value **linear** parameter the callee never consumes: (Fn) §5.8's
second clause rejects the callee (`3.8:62`), and the frame pop refuses with
`linearLeak`. -/
def linearParamLeaked : Program :=
  { decls := Decls.ofStructs structEnv,
    fns := [{ params := [], ret := tI64, body := call 1 [resL (lit 5)] },
            { params := [⟨.struct sLinear, false⟩], ret := tI64, body := lit 1 }] }

/-- Recursion to a trap: `f1(3)` counts down and divides by zero at the
bottom, four frames deep. -/
def recursionTrap : Program :=
  { decls := Decls.ofStructs [],
    fns := [{ params := [], ret := tI64, body := call 1 [lit 3] },
            { params := [⟨tI64, false⟩], ret := tI64,
              body := ite (binop .lt (use (.var 0)) (lit 1))
                (binop .div (lit 1) (lit 0))
                (call 1 [binop .add (use (.var 0)) (lit (-1))]) }] }

/-- A recursive countdown: `4 + 3 + 2 + 1 + 0 = 10`. -/
def countdown : Program :=
  { decls := Decls.ofStructs [],
    fns := [{ params := [], ret := tI64, body := call 1 [lit 4] },
            { params := [⟨tI64, false⟩], ret := tI64,
              body := ite (binop .lt (use (.var 0)) (lit 1))
                (lit 0)
                (binop .add (use (.var 0)) (call 1 [binop .add (use (.var 0)) (lit (-1))])) }] }

/-! ## The one edge no monitor covers (RUE-2316)

A by-value argument's value lives in no cell and in no scope record between
the `use` that produced it and the `mintParams` that gives it one (§6.9's
(D-Call)). If a *later* argument of the same call unwinds by `return`,
(D-Return) §6.9 discards the evaluation context — the pending arguments with
it — and runs `run-all-scope-drops` on the frame's records, which never named
that value. Its drop is neither run nor monitored.

`Dynamics.lean`'s "Pending arguments" section says why `eval` models it that
way rather than patching it: the calculus has the gap — the unwinding rule
walks only σ, and §5.7's strict-context bottom rule (`Strict-Bottom` there,
which the fragment does not mechanize) imposes no discard check on the
siblings already evaluated — and the Rue compiler behaves the same. Closing it
is an open spec decision, RUE-2316. These two programs are the kernel-checked
witnesses, and the reason `no_violation`'s docstring names the carve-out. -/

/-- A **linear** value consumed *zero* times, with no refusal anywhere.

`f1`'s first parameter is the consumable linear struct `S2` and its body takes
it apart, so (Fn) §5.8 is satisfied. `main` moves its linear binding into the
first argument and then diverges in the second. (Return-Value) §5.7's
frame-wide residual-linear premise holds at the `return`, because the move
already marked the *context* `MovedOut` — the obligation has migrated to a
value the context does not name. `checkProgram` accepts, so
`checkProgram_sound`, `run_safe`, `no_violation` and `no_linear_leak` all
apply to it, and the run destroys `S2 { 7 }` with an empty drop trace. -/
def linearLostAtCallArg : Program :=
  { decls := Decls.ofStructs structEnv,
    fns := [{ params := [], ret := tI64,
              body := letIn false (resL (lit 7)) (call 1 [use (.var 0), ret (lit 0)]) },
            { params := [⟨.struct sLinear, false⟩, ⟨tI64, false⟩], ret := tI64,
              body := seq (drop (.var 1)) (use (.var 0)) }] }

/-- The affine twin, where the same loss is *observable*: `S1` declares a
destructor, so a drop of it is the trace event the printed program turns into
an output line — and here there is none. The callee discharges its parameter
with `@drop`, which would print; the value never reaches the callee. The Rue
compiler agrees — `S1`'s destructor does not run — which is why no bridge case
could catch this and why none is added. -/
def affineLostAtCallArg : Program :=
  { decls := Decls.ofStructs structEnv,
    fns := [{ params := [], ret := tI64,
              body := letIn false (resA (lit 7)) (call 1 [use (.var 0), ret (lit 0)]) },
            { params := [⟨.struct sAffine, false⟩, ⟨tI64, false⟩], ret := tI64,
              body := seq (drop (.var 1)) (use (.var 0)) }] }

#eval run demoOps (scalarProg (.int .w8 .signed) i8Overflow) demoFuel        -- panic: overflow
#eval run demoOps (scalarProg (.int .w8 .unsigned) u8Underflow) demoFuel     -- panic: overflow
#eval run demoOps (scalarProg (.int .w8 .signed) i8DivMinByNegOne) demoFuel  -- panic: overflow
#eval run demoOps (scalarProg (.int .w8 .signed) i8RemMinByNegOne) demoFuel  -- panic: overflow
#eval run demoOps (scalarProg (.int .w8 .signed) i8RemZero) demoFuel         -- panic: remZero
#eval run demoOps (scalarProg (.int .w8 .unsigned) u8CastOutOfRange) demoFuel -- panic: castOverflow
#eval run demoOps (scalarProg (.int .w8 .unsigned) u8CastInRange) demoFuel   -- ok: 200
#eval run demoOps (scalarProg (.int .w8 .unsigned) u8ShiftMasks) demoFuel    -- ok: 1
#eval run demoOps (scalarProg (.int .w8 .unsigned) u8Bitwise) demoFuel       -- ok: 15
#eval run demoOps (scalarProg (.int .w16 .signed) i16Negate) demoFuel        -- ok: -21
#eval run demoOps (scalarProg .bool u64Compare) demoFuel                     -- ok: true
#eval run demoOps (scalarProg .bool boolNegate) demoFuel                     -- ok: false
#eval run demoOps (scalarProg tI64 dbgScalars) demoFuel                      -- ok: 0, dbg -5, 42, true
#eval run demoOps (prog tI64 dbgBetweenDrops) demoFuel                       -- ok: 0, dtor/dbg/dtor
#eval run demoOps (prog tI64 panicAfterDrop) demoFuel                        -- panic: user, after dtor 7
#eval run demoOps (scalarProg tI64 dbgBeforeTrap) demoFuel                   -- panic: divZero, after dbg 1
#eval run demoOps (scalarProg tI64 scalars) demoFuel            -- ok: 10, trace: []
#eval run demoOps (prog tI64 affineDrop) demoFuel               -- ok: 1, drop + dtor of S1{7}
#eval run demoOps (prog tI64 linearConsumed) demoFuel           -- ok: 7, trace: []
#eval run demoOps (prog tI64 linearLeaked) demoFuel             -- STUCK: linearLeak
#eval run demoOps (prog tI64 useAfterMove) demoFuel             -- STUCK: useAfterMove
#eval run demoOps (prog tI64 reinit) demoFuel                   -- ok: 2, trace: []
#eval run demoOps (prog tI64 linearHalfConsumed) demoFuel       -- STUCK: linearLeak
#eval run demoOps (scalarProg tI64 overflow) demoFuel           -- panic: overflow
#eval run demoOps (scalarProg tI64 divZero) demoFuel            -- panic: divZero
#eval run demoOps (prog tI64 structLinearFieldLeaked) demoFuel  -- STUCK: linearLeak
#eval run demoOps (prog tI64 structLinearFieldDropped) demoFuel -- ok: 0, dtor of S3{2}
#eval run demoOps (prog tI64 structNestedDrop) demoFuel         -- ok: 9, dtors 1 then 2
#eval run demoOps (prog tI64 structJoinDisagrees) demoFuel      -- STUCK: linearLeak
#eval run demoOps (prog tI64 structCopyTwice) demoFuel          -- ok: 10, trace: []
#eval run demoOps (prog tI64 structFieldOrder) demoFuel         -- ok: 0, dtors 1 then 2
#eval run demoOps callPlain demoFuel                            -- ok: 5
#eval run demoOps returnPastAffine demoFuel                     -- ok: 7, drops 4 then 3
#eval run demoOps returnPastLinear demoFuel                     -- STUCK: linearLeak
#eval run demoOps paramDroppedAtPop demoFuel                    -- ok: 1, dtor of S1{2}
#eval run demoOps linearParamLeaked demoFuel                    -- STUCK: linearLeak
#eval run demoOps recursionTrap demoFuel                        -- panic: divZero
#eval run demoOps countdown demoFuel                            -- ok: 10
#eval run demoOps countdown 12                                  -- outOfFuel
#eval run demoOps (prog tI64 partialMoveResidue) demoFuel       -- ok: 9, dtors 1 then 2
#eval run demoOps (prog tI64 copyThroughPartial) demoFuel       -- ok: 7, dtor 1 only
#eval run demoOps (prog tI64 dropFieldThenWhole) demoFuel       -- ok: 9, dtors 1 then 2
#eval run demoOps (prog tI64 reinitField) demoFuel              -- ok: 9, dtors 1, 5, 2
#eval run demoOps (prog tI64 overwriteField) demoFuel           -- ok: 9, dtors 1, 5, 2
#eval run demoOps (prog tI64 partialMoveOneArm) demoFuel        -- ok: 9, dtors 1 then 2
#eval run demoOps (prog tI64 partialMoveOtherArm) demoFuel      -- ok: 9, dtors 1 then 2
#eval run demoOps (prog tI64 deepPath) demoFuel                 -- ok: 9, dtors 2 then 1
#eval run demoOps (prog tI64 linearFieldResidue) demoFuel       -- ok: 9, dtors 1 then 2
#eval run demoOps (prog tI64 joinWholeAgainstPartial) demoFuel  -- ok: 9, dtors 1 then 2
#eval run demoOps linearLostAtCallArg demoFuel                  -- ok: 0, EMPTY trace
#eval run demoOps affineLostAtCallArg demoFuel                  -- ok: 0, EMPTY trace

/-!
## Static acceptance and rejection, mechanically

The well-typed examples are accepted by the verified checker — so the §7
theorems apply to them; the violating ones are rejected by the same checker
that `checkProgram_sound` ties to the judgment. `rfl`/`decide` makes these
kernel-checked facts, not test assertions.
-/

/-- §3's class assignment holds of the fixture declarations, so `Ty.mult`'s
lookup is the join §3 defines (`checkStructs_sound`). -/
example : WfStructs (Decls.ofStructs structEnv) := checkStructs_sound (by rfl)

example : ProgramTyped (scalarProg tI64 scalars) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 affineDrop) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 linearConsumed) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 reinit) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tI64 overflow) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 structLinearFieldDropped) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 structNestedDrop) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 structCopyTwice) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 structFieldOrder) := checkProgram_sound (by rfl)
example : ProgramTyped callPlain := checkProgram_sound (by rfl)
example : ProgramTyped returnPastAffine := checkProgram_sound (by rfl)
example : ProgramTyped paramDroppedAtPop := checkProgram_sound (by rfl)
example : ProgramTyped recursionTrap := checkProgram_sound (by rfl)
example : ProgramTyped countdown := checkProgram_sound (by rfl)

/-! The partial-move programs (RUE-2231): the accepted ones, so the §7
theorems apply to a store whose cells hold trees with holes in them. -/

example : ProgramTyped (prog tI64 partialMoveResidue) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 copyThroughPartial) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 dropFieldThenWhole) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 reinitField) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 overwriteField) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 partialMoveOneArm) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 partialMoveOtherArm) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 deepPath) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 linearFieldResidue) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 joinWholeAgainstPartial) := checkProgram_sound (by rfl)

/-- `fully-owned(Σ, p)` (§5.1): the whole value may not be moved once a path
under it is `MovedOut` (`3.8:26`; the compiler reports E0205 "use of partially
moved value"). -/
example : checkProgram (prog tI64 partialThenWhole) = false := by rfl

/-- `3.9:34` (E0456): no field may be moved out of a value whose type declares
a destructor. -/
example : checkProgram (prog tI64 partialUnderDtor) = false := by rfl

/-- (@Drop) §5.3's residual side condition: a partially moved place may not be
dropped whole while a **linear** sub-place under it is still `Owned`
(E0406). -/
example : checkProgram (prog tI64 linearFieldStranded) = false := by rfl

/-- The §5.5 join with a linear sub-place consumed on one path only
(`3.8:50`; the compiler reports E0443). -/
example : checkProgram (prog tI64 joinLinearFieldOneArm) = false := by rfl

/-- §3's class assignment holds of the enum declarations too (`6.3:19`), so
`Ty.mult`'s lookup is the payload join at an enum type (`checkEnums_sound`). -/
example : WfEnums enumDecls := checkEnums_sound (by rfl)
example : WfStructs enumDecls := checkStructs_sound (by rfl)

/-- The accepted enum programs (RUE-2320's probes e1, e1b, e3b, e4, e5, e7),
each run against the compiler before it was committed. -/
example : ProgramTyped (enumProg tI64 enumMatchAffine) := checkProgram_sound (by rfl)
example : ProgramTyped (enumProg tI64 enumDropUnmatched) := checkProgram_sound (by rfl)
example : ProgramTyped (enumProg tI64 enumArmDropsPayload) := checkProgram_sound (by rfl)
example : ProgramTyped (enumProg tI64 enumCopyMatchedTwice) := checkProgram_sound (by rfl)
example : ProgramTyped (enumProg tI64 enumMatchProjection) := checkProgram_sound (by rfl)
example : ProgramTyped (enumProg tI64 enumTwoPayloadBindings) := checkProgram_sound (by rfl)

/-- (Match) §5.5's join with the enum consumed on one path only: `class(E)` is
the payload join over every variant, so the `Owned` side is residual and the
join is ill-formed (`3.8:50`, `6.3:19`; the compiler reports E0443 — probe
e2). -/
example : checkProgram (enumProg tI64 enumMatchOneArm) = false := by rfl

/-- (Match) §5.5's per-arm §5.6 obligation: an arm that binds a `Linear`
payload and neither moves nor consumes it leaks (`6.3:17`, `3.8:32`; the
compiler reports E0406 on the binding — probe e3). -/
example : checkProgram (enumProg tI64 enumArmLeaksPayload) = false := by rfl

/-- A `match` whose arms are not exactly the variants is not exhaustive
((Match) §5.5, `4.7:9`): two variants, one arm. -/
example : checkProgram (enumProg tI64
    (letIn false (mkEnum eAffineIdx 1 []) («match» (use (.var 0)) [lit 5]))) = false := by rfl

/-- A tag the declaration does not have (`6.3:16`; the compiler reports
E0420). -/
example : checkProgram (enumProg tI64
    (letIn false (mkEnum eAffineIdx 2 []) (lit 5))) = false := by rfl

/-- A second `match` on the same non-`Copy` enum: the first one moved it out,
so the second is the use of a moved-out place (`3.8:5`; the compiler reports
E0205 — probe e10). -/
example : checkProgram (enumProg tI64
    (letIn false (mkEnum eAffineIdx 0 [resA (lit 1)])
      (seq («match» (use (.var 0)) [unitLit, unitLit])
        («match» (use (.var 0)) [lit 5, lit 6])))) = false := by rfl

/-- An enum one variant of which carries a `linear` payload, constructed as the
**other** variant and left to scope exit: `class(E)` is the join over every
variant, so the obligation is the type's and the value's own emptiness does not
discharge it (`6.3:19`; the compiler reports E0406 — probe e11). -/
example : checkProgram (enumProg tI64
    (letIn false (mkEnum eLinearIdx 1 []) (lit 7))) = false := by rfl

/-- The fragment's own restriction: a path whose proper prefix is a struct
declared `linear` selects §4.2's `Declared(d, π)` plan, whose rule
(Use-Declared-Linear-Destructure) §5.1 is RUE-2236's. The *compiler* accepts
this program, so nothing the corpus or the generator emits may have the shape
(`Syntax.lean`, `Gen.lean`). -/
example : checkProgram (prog tI64
    (letIn false (resL (lit 7))
      (letIn false (use (.proj (.var 0) 0)) (seq (drop (.var 1)) (use (.var 0)))))) = false := by
  rfl

/-! The two RUE-2316 witnesses are accepted — which is the point: the §7
theorems apply to them, and the run below still loses the resource. -/

example : ProgramTyped linearLostAtCallArg := checkProgram_sound (by rfl)
example : ProgramTyped affineLostAtCallArg := checkProgram_sound (by rfl)

/-- The linear value is destroyed with an empty trace: no `drop`, no
`dropTemp`, no `dtor`, and no `Violation`. `no_linear_leak` holds of this
program and says nothing about it. -/
example : run demoOps linearLostAtCallArg demoFuel = .ok [.dead] (v64 0) [] := by rfl

/-- The affine value likewise: the destructor line the printed program would
have shown is absent. -/
example : run demoOps affineLostAtCallArg demoFuel = .ok [.dead] (v64 0) [] := by rfl

/-! The width, operator and intrinsic cases are accepted, so the §7 theorems
apply to the traps they reach: a trap is a *defined* outcome. -/

example : ProgramTyped (scalarProg (.int .w8 .signed) i8Overflow) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg (.int .w8 .unsigned) u8Underflow) :=
  checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg (.int .w8 .signed) i8RemZero) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg (.int .w8 .unsigned) u8CastOutOfRange) :=
  checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tI64 dbgScalars) := checkProgram_sound (by rfl)
example : ProgramTyped (prog tI64 panicAfterDrop) := checkProgram_sound (by rfl)

/-! ## The float programs, accepted and run

The static side first: `checkProgram_sound` turns each acceptance into a §5
derivation, so `soundness` applies to every one of them. The dynamic side is
pinned at `demoOps`, the model the corpus runs; each expectation below is
also checked against the compiler, case by case, by the corpus driver. -/

example : ProgramTyped (scalarProg tF64 floatArith) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tF64 floatCopy) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tF64 floatDivZero) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tI64 floatNanUnordered) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tI64 floatSignedZeros) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tI64 floatToIntTrunc) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg (.int .w32 .signed) floatToIntTrapInf) :=
  checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tF32 floatCastNarrow) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tF64 floatSqrt) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg tI64 totalCmpOrder) := checkProgram_sound (by rfl)

/-- `%` has no float rule — `(Float-Arith)` §5.8 omits it and `3.12:25` says
why — so the checker rejects it, "by the absence of a rule" made a side
condition (`BinOp.floatAdmits`). -/
example : checkProgram (scalarProg tF64 (binop .rem (flE .w64 1 0) (flE .w64 2 0))) = false := by
  rfl

/-- Nor do the bitwise operators: a float is a datum rather than a bit
pattern (§2), so `(Arith)`/`(BitNot)` never reach one. -/
example : checkProgram (scalarProg tF64 (binop .bitAnd (flE .w64 1 0) (flE .w64 2 0))) = false := by
  rfl
example : checkProgram (scalarProg tF64 (unop .bitnot (flE .w64 1 0))) = false := by rfl

/-- `3.12:13` gives no implicit widening, so an `f32`/`f64` mix has no
derivation. -/
example : checkProgram (scalarProg tF64 (binop .add (flE .w64 1 0) (flE .w32 1 0))) = false := by
  rfl

/-- `3.12:14` relates no float operand to an integer one either: the only
bridges are the conversion intrinsics. -/
example : checkProgram (scalarProg tF64 (binop .add (flE .w64 1 0) (lit 1))) = false := by rfl

/-- `(Float-Cast)` carries `w' ≠ w` (`3.12:19`): `@float_cast` converts
between the two widths and only between them. -/
example : checkProgram (scalarProg tF64 (fintrin (.floatCast .w64) (flE .w64 1 0))) = false := by
  rfl

/-- `@total_cmp` has no integer rule (`3.12:31`), which `BinOp.intAdmits`
carries on (Arith)/(Ord). -/
example : checkProgram (scalarProg (.int .w32 .signed) (binop .totalCmp (lit 1) (lit 2)))
    = false := by rfl

/-! `3.12:10`: a float literal whose value rounds to an **infinity** at its
width is a compile-time rejection (`E0206`), which (Lit) §5.8 carries, on its float half, as
`FloatLit.RoundsFinite`. `1e300` has an `f64` and no `f32`, so the same literal
is accepted at one width and refused at the other — and the compiler agrees at
both, on the printed program.

The threshold is `max_{𝔽_w}` plus *half an ulp*, not `max_{𝔽_w}`: an exact
value above the largest finite `f32` still rounds down to it while it stays
below `2^128 - 2^103`, and `3.12:10` refuses it only from there up. Both sides
of that boundary were probed against the compiler, which accepts `…447` and
rejects `…448`. *Underflow* carries no premise at all — `3.12:10` speaks of an
infinity only, and a literal too small for the width rounds to zero in the
model and in the compiler alike.

`exponentiation.threshold` is Lean's guard against folding a large `Nat` power
in a simproc; `10 ^ 300` is exactly what the first pair of witnesses is about,
so it is raised for them. -/

section
set_option exponentiation.threshold 400

example : checkProgram (scalarProg tF32 (flE .w32 1 300)) = false := by rfl
example : checkProgram (scalarProg tF64 (flE .w64 1 300)) = true := by rfl

end

example : checkProgram (scalarProg tF32
    (flE .w32 340282356779733661637539395458142568447 0)) = true := by rfl
example : checkProgram (scalarProg tF32
    (flE .w32 340282356779733661637539395458142568448 0)) = false := by rfl
example : checkProgram (scalarProg tF32 (fl .w32 1 49)) = true := by rfl

/-! The float programs' **runs** are not pinned here. Reducing one in the
kernel means reducing `Float.exactOps`'s exact rational arithmetic — `2^1074`
and a correctly-rounded division — which exceeds the elaborator's recursion
budget without buying anything: the run is checked where it means something,
by *executing* it and comparing the printed program against the compiler, case
by case (`Corpus.lean`; every float case agrees). What is kernel-checked here
is the static side above, and the trap witnesses below, which quantify over
every model and compute no float at all. -/

/-- (Neg) §5.8 negates a signed operand only (`4.2:6`, `4.2:14`), so the
checker rejects `neg` on an unsigned type — there is no value for it to
produce. -/
example : checkProgram (scalarProg (.int .w8 .unsigned)
    (unop .neg (intLit .w8 .unsigned 1))) = false := by rfl

/-- (Arith) §5.8 gives both operands **one** `int(w,s)`: Rue has no implicit
widening, so a mixed-width operator has no derivation. -/
example : checkProgram (scalarProg (.int .w8 .signed)
    (binop .add (intLit .w8 .signed 1) (intLit .w16 .signed 1))) = false := by rfl

/-- The bitwise operators take no `bool` (`4.3a:18`, `4.3a:19`), and (Not)
§5.8 takes nothing else (`4.4:2`). -/
example : checkProgram (scalarProg .bool
    (binop .bitAnd (boolLit true) (boolLit false))) = false := by rfl
example : checkProgram (scalarProg .bool (unop .not (lit 1))) = false := by rfl

/-- (Dbg) §5.8 renders a scalar only; an aggregate operand is the compiler's
E0702. -/
example : checkProgram (prog tI64
    (seq (dbg (resA (lit 1))) (lit 0))) = false := by rfl

example : checkProgram (prog tI64 linearLeaked) = false := by rfl
example : checkProgram (prog tI64 useAfterMove) = false := by rfl
example : checkProgram (prog tI64 linearHalfConsumed) = false := by rfl
example : checkProgram (prog tI64 structLinearFieldLeaked) = false := by rfl
example : checkProgram (prog tI64 structJoinDisagrees) = false := by rfl
example : checkProgram returnPastLinear = false := by rfl
example : checkProgram linearParamLeaked = false := by rfl

/-- (Assign) §5.2's `3.8:77` premise, at the shape where the type-keyed
reading and the residual one diverge: a root whose *type* carries a linear
value, reassigned after a field `@drop` took the linear part out. `check`
refuses, and so does the compiler (E0493). -/
example : checkProgram (prog tI64 overwritePastPartialLinear) = false := by rfl

/-- The same one field step down. `S11`'s presence is not the reason: the
extended environment is well-formed. -/
example : checkStructs (Decls.ofStructs (structEnv ++ [dNestCarry])) = true := by rfl
example : checkProgram (nestCarryProg tI64 overwriteFieldPastPartialLinear) = false := by rfl

/-- And the premise, not the program's shape, is what refuses them: discharge
the **whole** carrier instead of one field and `Σ1(v0) = MovedOut` satisfies
the first disjunct, so the very same assignment is accepted. -/
example : checkProgram (prog tI64
    (letIn true (mkStruct sCarryAffine [resLD (lit 1), resA (lit 2)])
      (seq (drop (.var 0))
        (seq (assign (.var 0) (mkStruct sCarryAffine [resLD (lit 5), resA (lit 6)]))
          (seq (drop (.var 0)) (lit 9)))))) = true := by rfl

/-- A declaration whose recorded class disagrees with §3's join is rejected by
the same pass: `class(S)` is not a free parameter of the syntax. -/
example : checkStructs (Decls.ofStructs
    [{ attr := .none, fields := [tI64], dtor := false, cls := .copy }]) = false := by rfl

/-- `3.8:18` and `3.9:31`: a `@copy` declaration whose field join is not
`Copy`, or which declares a destructor, is ill-formed. -/
example : checkStructs (Decls.ofStructs (structEnv ++
    [{ attr := .copy, fields := [.struct 1], dtor := false, cls := .copy }])) = false := by rfl
example : checkStructs (Decls.ofStructs
    [{ attr := .copy, fields := [tI64], dtor := true, cls := .copy }]) = false := by rfl

/-- `3.9:44` (E0462): a declaration whose field carries a linear value may not
declare a destructor — `3.9:34` forbids moving the field out, so the field's
obligation could only ever be met by the glue that runs after the destructor.
A *declared*-linear struct with no linear field may have one (`S3` above). -/
example : checkStructs (Decls.ofStructs (structEnv ++
    [{ attr := .none, fields := [.struct 2], dtor := true, cls := .linear }])) = false := by rfl

/-- No recursive structs: a field may name only an earlier declaration, which
is what makes §3's class equation solvable in one pass. -/
example : checkStructs (Decls.ofStructs
    [{ attr := .none, fields := [.struct 0], dtor := false, cls := .affine }]) = false := by rfl

/-!
## Refusals and traps, kernel-checked

In interpreter form a violation is a positive result, so `soundness` is only
as strong as `eval`'s refusal enumeration. These witnesses pin every refusal
and trap to a program, or an open machine state, that reaches it, checked
by the kernel rather than observed by `#eval` (ADR-0097; the bridge cannot
observe refusals, because the compiler rejects those programs first).
-/

example : run demoOps (prog tI64 linearLeaked) demoFuel = .stuck .linearLeak := by rfl
example : run demoOps (prog tI64 useAfterMove) demoFuel = .stuck .useAfterMove := by rfl
example : run demoOps (prog tI64 linearHalfConsumed) demoFuel = .stuck .linearLeak := by rfl
example : run demoOps (prog tI64 structLinearFieldLeaked) demoFuel = .stuck .linearLeak := by rfl
example : run demoOps (prog tI64 structJoinDisagrees) demoFuel = .stuck .linearLeak := by rfl
example : run demoOps (scalarProg tI64 overflow) demoFuel = .panic .overflow [] := by rfl
example : run demoOps (scalarProg (.int .w8 .signed) i8Overflow) demoFuel
    = .panic .overflow [] := by rfl
example : run demoOps (scalarProg (.int .w8 .unsigned) u8Underflow) demoFuel
    = .panic .overflow [] := by rfl
example : run demoOps (scalarProg (.int .w8 .signed) i8DivMinByNegOne) demoFuel
    = .panic .overflow [] := by rfl
example : run demoOps (scalarProg (.int .w8 .signed) i8RemMinByNegOne) demoFuel
    = .panic .overflow [] := by rfl

/-- `min_T * -1` traps at `i64` as it does at every other signed width. The
compiler's constant folder does not (RUE-2318); the model is not changed to
match it, and `Corpus`'s `i64_min_times_neg1` is the case that says so to the
bridge. -/
example : run demoOps (scalarProg tI64 i64MinTimesNeg1) demoFuel
    = .panic .overflow [] := by rfl
example : run demoOps (scalarProg (.int .w8 .signed) i8RemZero) demoFuel
    = .panic .remZero [] := by rfl
example : run demoOps (scalarProg (.int .w8 .unsigned) u8CastOutOfRange) demoFuel
    = .panic .castOverflow [] := by rfl

/-- §6.4's bit rules are total: the shift amount is masked (`4.3a:10`) and the
complement is read back at the operand's own width, so `1 << 8` at `u8` is `1`
and `~240` is `15`. -/
example : run demoOps (scalarProg (.int .w8 .unsigned) u8ShiftMasks) demoFuel
    = .ok [] (.int .w8 .unsigned 1) [] := by rfl
example : run demoOps (scalarProg (.int .w8 .unsigned) u8Bitwise) demoFuel
    = .ok [] (.int .w8 .unsigned 15) [] := by rfl

/-- An unsigned compare orders by the unsigned value: `max_T > 0` at `u64`,
whose signed reading would be `-1`. -/
example : run demoOps (scalarProg .bool u64Compare) demoFuel = .ok [] (.bool true) [] := by rfl

/-- **A trap carries the observable output that ran before it.** The
destructor has already printed when the `@panic` fires, and §6.12's outcome
keeps it: the process prints what it printed and then exits 101. -/
example : run demoOps (prog tI64 panicAfterDrop) demoFuel
    = .panic .user
        [.drop 0 (.struct sAffine [c64 7]), .dtor sAffine (.struct sAffine [c64 7])] := by rfl

/-- The same for a trap the program did not ask for. -/
example : run demoOps (scalarProg tI64 dbgBeforeTrap) demoFuel
    = .panic .divZero [.dbg (v64 1)] := by rfl

/-- **A `@panic` runs no drop.** §5.7 exempts the `⊥_panic` edge from §5.6's
obligation and §6.12 abandons the configuration, so the live affine binding's
destructor never fires and the trap carries an empty trace — where the very
same program with an explicit `@drop` carries the destructor out. -/
example : run demoOps (prog tI64 panicPastAffine) demoFuel = .panic .user [] := by rfl

/-- **`Typed` derives a `@panic` past a live linear binding.** (Panic) §5.8
imposes no residual-linear premise — §5.7 exempts the `⊥_panic` edge from
§5.6's obligation — so the `let`'s own scope-exit check is discharged by the
free outgoing context (Sub-Never) licenses, which the rule may take
`MovedOut`. This is the one shape where `Typed.panic` and `Typed.ret` differ:
at an affine binding `Typed.letIn`'s premise is vacuous, so there is nothing
to drop. -/
theorem panicPastLinear_typed :
    Typed (prog tI64 panicPastLinear) tI64 [] panicPastLinear tI64 [] := by
  refine .letIn (T₁ := .struct sLinearDtor) (Γ₁ := [])
    (en' := { ty := .struct sLinearDtor, mu := false, st := .movedOut }) ?_ ?_ ?_
  · exact .mkStruct (sd := dLinearDtor) rfl (.cons (.intLit (by decide)) .nil)
  · exact .panic rfl
  · decide

/-- **And `check` rejects it**, which `check_sound` permits and completeness
would not: the algorithm gives `@panic` the state in force at the form, so the
`let` sees `v0` still `Owned` at a `Linear` type and refuses. The compiler
accepts the same program. `Checker.lean`'s "what completeness costs" names
this shape. -/
example : checkProgram (prog tI64 panicPastLinear) = false := by rfl

/-- **The linear value is consumed zero times, with no violation.** The trap
carries an empty trace: `S3` declares a destructor and it does not run,
because §6.12 abandons the configuration where a `return` would have unwound
the frame. `no_violation` holds of this program and says nothing about it —
the `@panic` exit its docstring now names. -/
example : run demoOps (prog tI64 panicPastLinear) demoFuel = .panic .user [] := by rfl

/-- The two observation channels are one trace, so a `@dbg` between two drops
comes out between them (`Corpus.outLines` reads exactly this order). -/
example : run demoOps (prog tI64 dbgBetweenDrops) demoFuel
    = .ok [.dead, .dead] (v64 0)
        [.drop 0 (.struct sAffine [c64 1]), .dtor sAffine (.struct sAffine [c64 1]),
         .dbg (v64 2),
         .drop 1 (.struct sAffine [c64 3]), .dtor sAffine (.struct sAffine [c64 3])] := by rfl
example : run demoOps (scalarProg tI64 divZero) demoFuel = .panic .divZero [] := by rfl
example : run demoOps (scalarProg tI64 (use (.var 0))) demoFuel = .stuck .unbound := by rfl
example : run demoOps (scalarProg tI64 (binop .add (boolLit true) (lit 1))) demoFuel
    = .stuck .typeConfusion := by rfl
example : run demoOps returnPastLinear demoFuel = .stuck .linearLeak := by rfl
example : run demoOps linearParamLeaked demoFuel = .stuck .linearLeak := by rfl

/-- A struct literal with the wrong number of initializers is `typeConfusion`
((Struct-Intro) §5.8's `3.6:5`/`3.6:6`; no well-typed program reaches it). -/
example : run demoOps (prog tI64 (seq (mkStruct sPair [lit 1]) (lit 0))) demoFuel
    = .stuck .typeConfusion := by rfl

/-- A struct literal naming a declaration the program does not have is
`unbound`; elaboration resolves every type name before the core (§2). -/
example : run demoOps (prog tI64 (seq (mkStruct 99 []) (lit 0))) demoFuel
    = .stuck .unbound := by rfl

/-- A call whose argument count does not match the callee's parameter list is
`typeConfusion` (§5.8, `4.10:3`); no well-typed program reaches it. -/
example : run M
      { decls := Decls.ofStructs [],
        fns := [{ params := [], ret := tI64, body := call 1 [] },
                { params := [⟨tI64, false⟩], ret := tI64, body := lit 0 }] } demoFuel
    = .stuck .typeConfusion := by rfl

/-- A call of a function the program does not have is `unbound`; elaboration
resolves every name before the core (§2). -/
example : run demoOps (scalarProg tI64 (call 7 [])) demoFuel = .stuck .unbound := by rfl

/-! ## Drop order, pinned

§6.11 fixes the order in which a drop's events come out: a value's own
destructor first, then its fields in declaration order, recursively; and a
frame's teardown reads its scope record newest-first (§6.9). These pin both.
-/

/-- The unwind order: an early `return` past two live affine bindings drops
the newer one first (§6.9's (D-Return); `3.9:18`). -/
example : run demoOps returnPastAffine demoFuel
    = .ok [.dead, .dead] (v64 7)
        [.drop 1 (.struct sAffine [c64 4]), .dtor sAffine (.struct sAffine [c64 4]),
         .drop 0 (.struct sAffine [c64 3]), .dtor sAffine (.struct sAffine [c64 3])] := by rfl

/-- §6.11's order inside one value: the outer destructor, then the fields in
declaration order — so the nested destructor runs **after** the outer one. -/
example : run demoOps (prog tI64 structNestedDrop) demoFuel
    = .ok [.dead] (v64 9)
        [.drop 0 (.struct sOuter [c64 1, .struct sAffine [c64 2]]),
         .dtor sOuter (.struct sOuter [c64 1, .struct sAffine [c64 2]]),
         .dtor sAffine (.struct sAffine [c64 2])] := by rfl

/-- Fields drop in declaration order, not in reverse: the struct here has no
destructor of its own, so its trace is exactly its two fields' (§6.11). -/
example : run demoOps (prog tI64 structFieldOrder) demoFuel
    = .ok [.dead] (v64 0)
        [.drop 0 (.struct sTwoAffine [.struct sAffine [c64 1], .struct sAffine [c64 2]]),
         .dtor sAffine (.struct sAffine [c64 1]),
         .dtor sAffine (.struct sAffine [c64 2])] := by rfl

/-- `@drop` of a value that is linear only through a field runs the whole
value's glue: the field's destructor is the one observable event. -/
example : run demoOps (prog tI64 structLinearFieldDropped) demoFuel
    = .ok [.dead] (v64 0)
        [.drop 0 (.struct sCarry [c64 1, .struct sLinearDtor [c64 2]]),
         .dtor sLinearDtor (.struct sLinearDtor [c64 2])] := by rfl

/-- **The `⊘`-skip, pinned.** A field is moved out and discharged on its own;
the scope exit then drops the *residue* — the cell holds a struct with a hole
where the moved field was, and §6.11's walk skips it, so the moved value is not
dropped a second time (`3.8:60`). -/
example : run demoOps (prog tI64 partialMoveResidue) demoFuel
    = .ok [.dead, .dead] (v64 9)
        [.drop 1 (.struct sAffine [c64 1]), .dtor sAffine (.struct sAffine [c64 1]),
         .drop 0 (.struct sTwoAffine [.hole, .struct sAffine [c64 2]]),
         .dtor sAffine (.struct sAffine [c64 2])] := by rfl

/-- The same at a path two field steps deep: `@drop(v.x0.x1)` writes `⊘` at
exactly that leaf, and the scope exit drops the rest of the tree in
declaration order (`3.9:13`). -/
example : run demoOps (prog tI64 deepPath) demoFuel
    = .ok [.dead] (v64 9)
        [.drop 0 (.struct sAffine [c64 2]), .dtor sAffine (.struct sAffine [c64 2]),
         .drop 0 (.struct sNested
             [.struct sTwoAffine [.struct sAffine [c64 1], .hole], c64 3]),
         .dtor sAffine (.struct sAffine [c64 1])] := by rfl

/-- A by-value parameter the callee never consumes is dropped at the frame
pop ((D-Return-Value) §6.9), not at the caller. -/
example : run demoOps paramDroppedAtPop demoFuel
    = .ok [.dead] (v64 1)
        [.drop 0 (.struct sAffine [c64 2]), .dtor sAffine (.struct sAffine [c64 2])] := by rfl

/-! ## Fuel, as an outcome

`outOfFuel` is not a machine state: it is the interpreter saying it stopped
early. `fuel_mono` says that raising the bound never changes an answer, and
`no_masking` that no bound turns a violation into exhaustion — so the two
lines below are a bound that is too small and the same program at a bound
that is not. -/

/-- Sixteen units of fuel is one too few for `countdown`: the interpreter
stops early and says so. -/
example : run demoOps countdown 16 = .outOfFuel := by rfl

/-- Seventeen is enough, and the answer is a value with five retired
parameter cells — one per frame the recursion pushed. -/
theorem countdown_at_17 :
    run demoOps countdown 17 = .ok [.dead, .dead, .dead, .dead, .dead] (v64 10) [] := by rfl

/-- `fuel_mono` in use: every larger bound gives that same answer, so the
∀-fuel shape of `soundness` is a statement about one outcome. -/
example : run demoOps countdown demoFuel = run demoOps countdown 17 :=
  fuel_mono demoOps (by decide) (fun h => absurd (countdown_at_17.symm.trans h) (by simp))

/-! ## The one float trap, witnessed under the model's laws

§6.4 gives floats exactly one trapping form, `@float_to_int`, and §7 owes one
lemma for them — "the premises of `(D-Float-To-Int)` and
`(D-Float-To-Int-Trap)` partition `𝔽_w`". That partition is a *theorem* here
(`floatToInt_partition`, `Float.lean`), because the datum model makes
truncation exact integer arithmetic, so the witnesses below need no arithmetic
of their own.

What they do need is a way for a core program to *reach* an infinity, and
that is a law: `3.12:22`'s "a finite non-zero over a zero is the infinity of
the xor sign", which §6.4 quotes as a consequence of `⊕_w`. The witnesses are
therefore stated over an **arbitrary** `FloatModel` and proved from its
laws rather than by computing with `Float.exactOps` — which is what makes them
claims about IEEE 754 instead of claims about this package's instance, and
what keeps them (and everything above them) free of `Classical.choice`. The
compiler agreement is checked the other way, case by case, by the corpus. -/

/-- **`@float_to_int` of an infinity traps** — `3.12:18`'s guard "admits both
infinities as failures" — and the category is `↯overflow`, the one §6.12
already lists (`8.1:7`), not a new one. -/
theorem floatToInt_inf_traps (M : FloatModel) (w : FloatWidth) (w' : IntWidth) (s' : Sign)
    (b : Bool) :
    evalFintrin M.toFloatOps (.floatToInt w' s') (.float w (.inf b)) = .trap .overflow := rfl

/-- **`@float_to_int` of a NaN traps**, the other half of
`(D-Float-To-Int-Trap)`'s premise (`3.12:18`). -/
theorem floatToInt_nan_traps (M : FloatModel) (w : FloatWidth) (w' : IntWidth) (s' : Sign)
    (b : Bool) :
    evalFintrin M.toFloatOps (.floatToInt w' s') (.float w (.nan b)) = .trap .overflow := rfl

/-- **A whole redex: `@float_to_int(1.0 / 0.0)` traps at every model.** The
division is `3.12:22`'s (`FloatModel.div_by_zero`), the literals are
`3.12:9`'s (`ofLit_one`, `ofLit_zero`), and the trap is the partition. No
float arithmetic is computed anywhere in the proof, and the theorem holds for
every model satisfying the laws — including, but not only, `Float.exactOps`,
which the corpus runs and the compiler agrees with. -/
theorem floatDivZeroToInt_traps (M : FloatModel) (P : Program) (H : Store) (φ : Frame)
    (w : FloatWidth) (w' : IntWidth) (s' : Sign) :
    eval M.toFloatOps 8 P H φ
        (fintrin (.floatToInt w' s') (binop .div (flE w 1 0) (flE w 0 0)))
      = .panic .overflow [] := by
  have hdiv : M.toFloatOps.arith w .div (.num false 1 0) (.num false 0 0) = .inf false :=
    M.div_by_zero w (.num false 1 0) false 1 0 (one_wf w) rfl (by decide) false
  simp [eval, EvalRes.andThen, EvalRes.withTrace, evalBinOp, binOpFloat, evalFintrin,
    OpRes.toRes, M.ofLit_one, M.ofLit_zero, hdiv, FloatDatum.toIntIn, FloatDatum.truncToInt]

/-! ## Where `eval` and §6 part on invalid input

`eval` is a model of §6 on the programs `check` accepts (`Dynamics.lean`,
"the correspondence with §6"). Off that domain the two can differ, and
these pin the ways they do, so nobody mistakes the machine for the paper
relation on raw `Expr`: an out-of-range literal is a value here, while §6's
integer domain is bounded (§6.1's `n_T`) and `check` rejects the literal; and
two operands of different integer types are a redex no §6.4 rule has, which
the machine names rather than leaving stuck silently.

A shape that *used* to part them no longer does. `eval` reduces both operands
before it inspects either, which is §6.2's own order, so
`true + (1 / 0)` traps with the division by zero §6 reaches rather than being
refused on the left operand's shape first.
-/

example : run demoOps (scalarProg tI64 (binop .add (boolLit true) (binop .div (lit 1) (lit 0)))) demoFuel
    = .panic .divZero [] := by rfl
example : run demoOps (scalarProg tI64
    (binop .add (lit 1) (.intLit .w8 .signed 1))) demoFuel
    = .stuck .typeConfusion := by rfl
example : run demoOps (scalarProg tI64 (lit (2 ^ 64))) demoFuel
    = .ok [] (v64 (2 ^ 64)) [] := by rfl
example : checkProgram (scalarProg tI64 (lit (2 ^ 64))) = false := by rfl

/-!
## The retired-cell refusal, witnessed from an open machine state

`useAfterDrop` is the machine's guard on a retired (`†`) cell (§6.1): a use,
an explicit `@drop`, or an assignment through a binding whose cell has been
retired is refused. With frames the guard is load-bearing on the unwind path
too — `run-all-scope-drops` walks the frame's scope record, and it is
`FrameMatches` (the record *is* the environment, whose cells are pairwise
distinct and never retired) that keeps that walk off a `†` cell, which is
what `no_use_after_drop` now rests on. No *closed* fragment program reaches
the guard through the syntax, so the witnesses below start the machine in an
open state — a store holding one retired cell and a frame naming it — which
is the state the guard exists for.
-/

example : eval demoOps demoFuel (scalarProg tI64 unitLit) [.dead] { env := [0], scope := [] }
    (use (.var 0))
    = .stuck .useAfterDrop := by rfl
example : eval demoOps demoFuel (scalarProg tI64 unitLit) [.dead] { env := [0], scope := [] }
    (drop (.var 0))
    = .stuck .useAfterDrop := by rfl
example : eval demoOps demoFuel (scalarProg tI64 unitLit) [.dead] { env := [0], scope := [] }
    (assign (.var 0) (lit 1)) = .stuck .useAfterDrop := by rfl

/-! ### The refusals a *path* reaches

A use at a projection has three ways to fail that a whole-binding use does
not, and each is a state the statics exclude: the path runs into a `⊘` on the
way down (`3.8:53`, (Owned-Base) §5.1), the value it reaches has a `⊘`
somewhere inside it (`3.8:26`, `fully-owned`), or a step of the path is not a
field of what is stored (which elaboration resolves, §2). The first two are
`useAfterMove`; the third is `typeConfusion`. As with the retired-cell guard,
no *closed* fragment program reaches them, so the witnesses start the machine
in an open state. -/

/-- Reading through a `⊘`: the base was moved out as a whole, so the path has
nowhere to go (`3.8:53`). -/
example : eval demoOps demoFuel (prog tI64 unitLit) [.full .hole] { env := [0], scope := [] }
    (use (.proj (.var 0) 0)) = .stuck .useAfterMove := by rfl

/-- Reading a value **with** a `⊘` in it: the place itself is there, but
handing it on would hand on an aggregate with a hole, which `fully-owned`
(§5.1, `3.8:26`) is exactly the premise against. -/
example : eval demoOps demoFuel (prog tI64 unitLit)
    [.full (.struct sTwoAffine [.hole, .struct sAffine [c64 2]])]
    { env := [0], scope := [] } (use (.var 0)) = .stuck .useAfterMove := by rfl

/-- A path step that is not a field of what is stored: no elaborated program
has one (§2 resolves field names to declaration slots, `3.6:15`). -/
example : eval demoOps demoFuel (prog tI64 unitLit) [.full (c64 7)] { env := [0], scope := [] }
    (use (.proj (.var 0) 0)) = .stuck .typeConfusion := by rfl

/-- `@drop` of a place that is already `⊘`: §5.3 demands `Σ(p) = Owned`, so
the machine refuses rather than treating the drop as a silent no-op. -/
example : eval demoOps demoFuel (prog tI64 unitLit)
    [.full (.struct sTwoAffine [.hole, .struct sAffine [c64 2]])]
    { env := [0], scope := [] } (drop (.proj (.var 0) 0)) = .stuck .useAfterMove := by rfl

/-- The same guard on the unwind path: a frame whose scope record names a
retired cell refuses instead of retiring it twice (§6.9). `FrameMatches` is
what excludes this state for a well-typed program. -/
example : eval demoOps demoFuel (scalarProg tI64 unitLit) [.dead] { env := [0], scope := [0] }
    (ret (lit 1)) = .stuck .useAfterDrop := by rfl

#eval checkProgram (scalarProg tI64 scalars)
#eval checkProgram (prog tI64 linearLeaked)

end RueCore.Examples
