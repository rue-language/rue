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

/-- `S2`: `linear struct { x0: i64 }`, no destructor. Class `Linear`; its
field can be read out, so it is the linear type the fragment's whole-value
elimination works on, and its drops are silent. -/
def dLinear : StructDecl := { attr := .linear, fields := [tI64], dtor := false, cls := .linear }

/-- `S3`: `linear struct { x0: i64 }` with a destructor. Class `Linear`, drops
observable; nothing may be read out of it (`3.9:34`), so it is discharged by
`@drop` or by a move. -/
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
use of it copies and the whole-value elimination reads the first. -/
def dPair : StructDecl :=
  { attr := .copy, fields := [tI64, tI64], dtor := false, cls := .copy }

/-- `S7`: `struct { x0: S1, x1: S1 }`, no destructor. Class `Affine`; dropping
it drops both fields in declaration order (§6.11) and nothing else. -/
def dTwoAffine : StructDecl :=
  { attr := .none, fields := [.struct 1, .struct 1], dtor := false, cls := .affine }

/-- The fixture environment: every field type names an earlier declaration, so
`WfStructs` holds (checked below) and §3's class assignment is the one
recorded. -/
def structEnv : StructEnv :=
  [dCopy, dAffine, dLinear, dLinearDtor, dCarry, dOuter, dPair, dTwoAffine]

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
def prog (T : Ty) (e : Expr) : Program := Program.entry structEnv T e

/-- A program with no struct declarations at all, for the scalar examples. -/
def scalarProg (T : Ty) (e : Expr) : Program := Program.entry [] T e

/-! ## Scalars, resources, and the ownership discipline -/

/-- `let x = 2 + 3; x + x` — well-typed scalar flow. -/
def scalars : Expr :=
  letIn false (binop .add (lit 2) (lit 3)) (binop .add (use 0) (use 0))

/-- An affine resource silently dropped at scope exit — legal, and the trace
shows the drop (its destructor). -/
def affineDrop : Expr :=
  letIn false (resA (lit 7)) (lit 1)

/-- A linear resource, consumed exactly once — legal. -/
def linearConsumed : Expr :=
  letIn false (resL (lit 7)) (consume (use 0))

/-- A linear resource leaked at scope exit — the machine REFUSES
(`linearLeak`), and no typing derivation exists for it. -/
def linearLeaked : Expr :=
  letIn false (resL (lit 7)) (lit 1)

/-- Use after move: `let r = S1{1}; let s = r; @drop(r)` — refused
dynamically, rejected statically. -/
def useAfterMove : Expr :=
  letIn false (resA (lit 1))
    (letIn false (use 0) (seq (drop 1) (lit 0)))

/-- Reinitialization: move out, assign back in, consume — legal (`3.8:55`). -/
def reinit : Expr :=
  letIn true (resL (lit 1))
    (seq (consume (use 0))
      (seq (assign 0 (resL (lit 2)))
        (consume (use 0))))

/-- Branch join: consume a linear value in only one arm — no typing
derivation exists (the §5.5 join rejects it); dynamically it leaks on the
`false` path. -/
def linearHalfConsumed : Expr :=
  letIn false (resL (lit 9))
    (seq (ite (boolLit false) (consume (use 0)) (lit 0))
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
  letIn false (mkStruct sCarry [lit 1, resLD (lit 2)]) (seq (drop 0) (lit 0))

/-- A nested destructor-bearing struct at scope exit: §6.11 runs the outer
destructor first and then the fields in declaration order, so the trace is
`1` then `2`. -/
def structNestedDrop : Expr :=
  letIn false (mkStruct sOuter [lit 1, resA (lit 2)]) (lit 9)

/-- The §5.5 join on a linear-carrying struct entry: discharged in one arm
only, which `3.8:50` makes ill-formed; on the path taken it leaks. -/
def structJoinDisagrees : Expr :=
  letIn false (mkStruct sCarry [lit 1, resLD (lit 2)])
    (seq (ite (boolLit false) (drop 0) unitLit) (lit 0))

/-- A `@copy` struct used twice: contraction is legal at `Copy` (§3), and
nothing is ever dropped. -/
def structCopyTwice : Expr :=
  letIn false (mkStruct sPair [lit 5, lit 6])
    (binop .add (consume (use 0)) (consume (use 0)))

/-- Two destructor-bearing fields in one struct with no destructor of its own:
scope exit drops them in **declaration** order (§6.11), so the trace is `1`
then `2`. -/
def structFieldOrder : Expr :=
  letIn false (mkStruct sTwoAffine [resA (lit 1), resA (lit 2)]) (lit 0)

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
    (seq (drop 0) (seq (dbg (lit 2)) (letIn false (resA (lit 3)) (lit 0))))

/-- A user `@panic` after an affine drop: the destructor has already run, so
the trap carries it out. §5.7 exempts the `⊥_panic` edge from §5.6's
obligation and §6.12 abandons the configuration, so the binding's own scope
exit never happens — the drop that shows is the explicit one. -/
def panicAfterDrop : Expr :=
  letIn false (resA (lit 7)) (seq (drop 0) (panic "boom"))

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

/-! ## Calls, frames, and `return` (RUE-2233)

Each of these needs more than one function, so it is written as a whole
`Program` rather than an `Expr`. Function index `0` is the entry point. -/

/-- A plain call: `f0()` calls `f1(2, 3)`, which adds its parameters. The
first parameter is the outermost binder, so it is `use 1` inside the body. -/
def callPlain : Program :=
  { structs := [],
    fns := [{ params := [], ret := tI64, body := call 1 [lit 2, lit 3] },
            { params := [⟨tI64, false⟩, ⟨tI64, false⟩], ret := tI64,
              body := binop .add (use 1) (use 0) }] }

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
  { structs := structEnv,
    fns := [{ params := [], ret := tI64, body := call 1 [resA (lit 2)] },
            { params := [⟨.struct sAffine, false⟩], ret := tI64, body := lit 1 }] }

/-- A by-value **linear** parameter the callee never consumes: (Fn) §5.8's
second clause rejects the callee (`3.8:62`), and the frame pop refuses with
`linearLeak`. -/
def linearParamLeaked : Program :=
  { structs := structEnv,
    fns := [{ params := [], ret := tI64, body := call 1 [resL (lit 5)] },
            { params := [⟨.struct sLinear, false⟩], ret := tI64, body := lit 1 }] }

/-- Recursion to a trap: `f1(3)` counts down and divides by zero at the
bottom, four frames deep. -/
def recursionTrap : Program :=
  { structs := [],
    fns := [{ params := [], ret := tI64, body := call 1 [lit 3] },
            { params := [⟨tI64, false⟩], ret := tI64,
              body := ite (binop .lt (use 0) (lit 1))
                (binop .div (lit 1) (lit 0))
                (call 1 [binop .add (use 0) (lit (-1))]) }] }

/-- A recursive countdown: `4 + 3 + 2 + 1 + 0 = 10`. -/
def countdown : Program :=
  { structs := [],
    fns := [{ params := [], ret := tI64, body := call 1 [lit 4] },
            { params := [⟨tI64, false⟩], ret := tI64,
              body := ite (binop .lt (use 0) (lit 1))
                (lit 0)
                (binop .add (use 0) (call 1 [binop .add (use 0) (lit (-1))])) }] }

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
  { structs := structEnv,
    fns := [{ params := [], ret := tI64,
              body := letIn false (resL (lit 7)) (call 1 [use 0, ret (lit 0)]) },
            { params := [⟨.struct sLinear, false⟩, ⟨tI64, false⟩], ret := tI64,
              body := binop .add (consume (use 1)) (use 0) }] }

/-- The affine twin, where the same loss is *observable*: `S1` declares a
destructor, so a drop of it is the trace event the printed program turns into
an output line — and here there is none. The callee discharges its parameter
with `@drop`, which would print; the value never reaches the callee. The Rue
compiler agrees — `S1`'s destructor does not run — which is why no bridge case
could catch this and why none is added. -/
def affineLostAtCallArg : Program :=
  { structs := structEnv,
    fns := [{ params := [], ret := tI64,
              body := letIn false (resA (lit 7)) (call 1 [use 0, ret (lit 0)]) },
            { params := [⟨.struct sAffine, false⟩, ⟨tI64, false⟩], ret := tI64,
              body := seq (drop 1) (use 0) }] }

#eval run (scalarProg (.int .w8 .signed) i8Overflow) demoFuel        -- panic: overflow
#eval run (scalarProg (.int .w8 .unsigned) u8Underflow) demoFuel     -- panic: overflow
#eval run (scalarProg (.int .w8 .signed) i8DivMinByNegOne) demoFuel  -- panic: overflow
#eval run (scalarProg (.int .w8 .signed) i8RemMinByNegOne) demoFuel  -- panic: overflow
#eval run (scalarProg (.int .w8 .signed) i8RemZero) demoFuel         -- panic: remZero
#eval run (scalarProg (.int .w8 .unsigned) u8CastOutOfRange) demoFuel -- panic: castOverflow
#eval run (scalarProg (.int .w8 .unsigned) u8CastInRange) demoFuel   -- ok: 200
#eval run (scalarProg (.int .w8 .unsigned) u8ShiftMasks) demoFuel    -- ok: 1
#eval run (scalarProg (.int .w8 .unsigned) u8Bitwise) demoFuel       -- ok: 15
#eval run (scalarProg (.int .w16 .signed) i16Negate) demoFuel        -- ok: -21
#eval run (scalarProg .bool u64Compare) demoFuel                     -- ok: true
#eval run (scalarProg .bool boolNegate) demoFuel                     -- ok: false
#eval run (scalarProg tI64 dbgScalars) demoFuel                      -- ok: 0, dbg -5, 42, true
#eval run (prog tI64 dbgBetweenDrops) demoFuel                       -- ok: 0, dtor/dbg/dtor
#eval run (prog tI64 panicAfterDrop) demoFuel                        -- panic: user, after dtor 7
#eval run (scalarProg tI64 dbgBeforeTrap) demoFuel                   -- panic: divZero, after dbg 1
#eval run (scalarProg tI64 scalars) demoFuel            -- ok: 10, trace: []
#eval run (prog tI64 affineDrop) demoFuel               -- ok: 1, drop + dtor of S1{7}
#eval run (prog tI64 linearConsumed) demoFuel           -- ok: 7, trace: []
#eval run (prog tI64 linearLeaked) demoFuel             -- STUCK: linearLeak
#eval run (prog tI64 useAfterMove) demoFuel             -- STUCK: useAfterMove
#eval run (prog tI64 reinit) demoFuel                   -- ok: 2, trace: []
#eval run (prog tI64 linearHalfConsumed) demoFuel       -- STUCK: linearLeak
#eval run (scalarProg tI64 overflow) demoFuel           -- panic: overflow
#eval run (scalarProg tI64 divZero) demoFuel            -- panic: divZero
#eval run (prog tI64 structLinearFieldLeaked) demoFuel  -- STUCK: linearLeak
#eval run (prog tI64 structLinearFieldDropped) demoFuel -- ok: 0, dtor of S3{2}
#eval run (prog tI64 structNestedDrop) demoFuel         -- ok: 9, dtors 1 then 2
#eval run (prog tI64 structJoinDisagrees) demoFuel      -- STUCK: linearLeak
#eval run (prog tI64 structCopyTwice) demoFuel          -- ok: 10, trace: []
#eval run (prog tI64 structFieldOrder) demoFuel         -- ok: 0, dtors 1 then 2
#eval run callPlain demoFuel                            -- ok: 5
#eval run returnPastAffine demoFuel                     -- ok: 7, drops 4 then 3
#eval run returnPastLinear demoFuel                     -- STUCK: linearLeak
#eval run paramDroppedAtPop demoFuel                    -- ok: 1, dtor of S1{2}
#eval run linearParamLeaked demoFuel                    -- STUCK: linearLeak
#eval run recursionTrap demoFuel                        -- panic: divZero
#eval run countdown demoFuel                            -- ok: 10
#eval run countdown 12                                  -- outOfFuel
#eval run linearLostAtCallArg demoFuel                  -- ok: 0, EMPTY trace
#eval run affineLostAtCallArg demoFuel                  -- ok: 0, EMPTY trace

/-!
## Static acceptance and rejection, mechanically

The well-typed examples are accepted by the verified checker — so the §7
theorems apply to them; the violating ones are rejected by the same checker
that `checkProgram_sound` ties to the judgment. `rfl`/`decide` makes these
kernel-checked facts, not test assertions.
-/

/-- §3's class assignment holds of the fixture declarations, so `Ty.mult`'s
lookup is the join §3 defines (`checkStructs_sound`). -/
example : WfStructs structEnv := checkStructs_sound (by rfl)

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

/-! The two RUE-2316 witnesses are accepted — which is the point: the §7
theorems apply to them, and the run below still loses the resource. -/

example : ProgramTyped linearLostAtCallArg := checkProgram_sound (by rfl)
example : ProgramTyped affineLostAtCallArg := checkProgram_sound (by rfl)

/-- The linear value is destroyed with an empty trace: no `drop`, no
`dropTemp`, no `dtor`, and no `Violation`. `no_linear_leak` holds of this
program and says nothing about it. -/
example : run linearLostAtCallArg demoFuel = .ok [.dead] (v64 0) [] := by rfl

/-- The affine value likewise: the destructor line the printed program would
have shown is absent. -/
example : run affineLostAtCallArg demoFuel = .ok [.dead] (v64 0) [] := by rfl

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

/-- A declaration whose recorded class disagrees with §3's join is rejected by
the same pass: `class(S)` is not a free parameter of the syntax. -/
example : checkStructs [{ attr := .none, fields := [tI64], dtor := false, cls := .copy }]
    = false := by rfl

/-- `3.8:18` and `3.9:31`: a `@copy` declaration whose field join is not
`Copy`, or which declares a destructor, is ill-formed. -/
example : checkStructs (structEnv ++
    [{ attr := .copy, fields := [.struct 1], dtor := false, cls := .copy }]) = false := by rfl
example : checkStructs [{ attr := .copy, fields := [tI64], dtor := true, cls := .copy }]
    = false := by rfl

/-- `3.9:44` (E0462): a declaration whose field carries a linear value may not
declare a destructor — `3.9:34` forbids moving the field out, so the field's
obligation could only ever be met by the glue that runs after the destructor.
A *declared*-linear struct with no linear field may have one (`S3` above). -/
example : checkStructs (structEnv ++
    [{ attr := .none, fields := [.struct 2], dtor := true, cls := .linear }]) = false := by rfl

/-- No recursive structs: a field may name only an earlier declaration, which
is what makes §3's class equation solvable in one pass. -/
example : checkStructs [{ attr := .none, fields := [.struct 0], dtor := false, cls := .affine }]
    = false := by rfl

/-!
## Refusals and traps, kernel-checked

In interpreter form a violation is a positive result, so `soundness` is only
as strong as `eval`'s refusal enumeration. These witnesses pin every refusal
and trap to a program, or an open machine state, that reaches it, checked
by the kernel rather than observed by `#eval` (ADR-0097; the bridge cannot
observe refusals, because the compiler rejects those programs first).
-/

example : run (prog tI64 linearLeaked) demoFuel = .stuck .linearLeak := by rfl
example : run (prog tI64 useAfterMove) demoFuel = .stuck .useAfterMove := by rfl
example : run (prog tI64 linearHalfConsumed) demoFuel = .stuck .linearLeak := by rfl
example : run (prog tI64 structLinearFieldLeaked) demoFuel = .stuck .linearLeak := by rfl
example : run (prog tI64 structJoinDisagrees) demoFuel = .stuck .linearLeak := by rfl
example : run (scalarProg tI64 overflow) demoFuel = .panic .overflow [] := by rfl
example : run (scalarProg (.int .w8 .signed) i8Overflow) demoFuel
    = .panic .overflow [] := by rfl
example : run (scalarProg (.int .w8 .unsigned) u8Underflow) demoFuel
    = .panic .overflow [] := by rfl
example : run (scalarProg (.int .w8 .signed) i8DivMinByNegOne) demoFuel
    = .panic .overflow [] := by rfl
example : run (scalarProg (.int .w8 .signed) i8RemMinByNegOne) demoFuel
    = .panic .overflow [] := by rfl
example : run (scalarProg (.int .w8 .signed) i8RemZero) demoFuel
    = .panic .remZero [] := by rfl
example : run (scalarProg (.int .w8 .unsigned) u8CastOutOfRange) demoFuel
    = .panic .castOverflow [] := by rfl

/-- §6.4's bit rules are total: the shift amount is masked (`4.3a:10`) and the
complement is read back at the operand's own width, so `1 << 8` at `u8` is `1`
and `~240` is `15`. -/
example : run (scalarProg (.int .w8 .unsigned) u8ShiftMasks) demoFuel
    = .ok [] (.int .w8 .unsigned 1) [] := by rfl
example : run (scalarProg (.int .w8 .unsigned) u8Bitwise) demoFuel
    = .ok [] (.int .w8 .unsigned 15) [] := by rfl

/-- An unsigned compare orders by the unsigned value: `max_T > 0` at `u64`,
whose signed reading would be `-1`. -/
example : run (scalarProg .bool u64Compare) demoFuel = .ok [] (.bool true) [] := by rfl

/-- **A trap carries the observable output that ran before it.** The
destructor has already printed when the `@panic` fires, and §6.12's outcome
keeps it: the process prints what it printed and then exits 101. -/
example : run (prog tI64 panicAfterDrop) demoFuel
    = .panic .user
        [.drop 0 (.struct sAffine [v64 7]), .dtor sAffine (.struct sAffine [v64 7])] := by rfl

/-- The same for a trap the program did not ask for. -/
example : run (scalarProg tI64 dbgBeforeTrap) demoFuel
    = .panic .divZero [.dbg (v64 1)] := by rfl

/-- **A `@panic` runs no drop.** §5.7 exempts the `⊥_panic` edge from §5.6's
obligation and §6.12 abandons the configuration, so the live affine binding's
destructor never fires and the trap carries an empty trace — where the very
same program with an explicit `@drop` carries the destructor out. -/
example : run (prog tI64 panicPastAffine) demoFuel = .panic .user [] := by rfl

/-- The two observation channels are one trace, so a `@dbg` between two drops
comes out between them (`Corpus.outLines` reads exactly this order). -/
example : run (prog tI64 dbgBetweenDrops) demoFuel
    = .ok [.dead, .dead] (v64 0)
        [.drop 0 (.struct sAffine [v64 1]), .dtor sAffine (.struct sAffine [v64 1]),
         .dbg (v64 2),
         .drop 1 (.struct sAffine [v64 3]), .dtor sAffine (.struct sAffine [v64 3])] := by rfl
example : run (scalarProg tI64 divZero) demoFuel = .panic .divZero [] := by rfl
example : run (scalarProg tI64 (use 0)) demoFuel = .stuck .unbound := by rfl
example : run (scalarProg tI64 (binop .add (boolLit true) (lit 1))) demoFuel
    = .stuck .typeConfusion := by rfl
example : run returnPastLinear demoFuel = .stuck .linearLeak := by rfl
example : run linearParamLeaked demoFuel = .stuck .linearLeak := by rfl

/-- A struct literal with the wrong number of initializers is `typeConfusion`
((Struct-Intro) §5.8's `3.6:5`/`3.6:6`; no well-typed program reaches it). -/
example : run (prog tI64 (seq (mkStruct sPair [lit 1]) (lit 0))) demoFuel
    = .stuck .typeConfusion := by rfl

/-- A struct literal naming a declaration the program does not have is
`unbound`; elaboration resolves every type name before the core (§2). -/
example : run (prog tI64 (seq (mkStruct 99 []) (lit 0))) demoFuel
    = .stuck .unbound := by rfl

/-- A call whose argument count does not match the callee's parameter list is
`typeConfusion` (§5.8, `4.10:3`); no well-typed program reaches it. -/
example : run { structs := [],
                fns := [{ params := [], ret := tI64, body := call 1 [] },
                        { params := [⟨tI64, false⟩], ret := tI64, body := lit 0 }] } demoFuel
    = .stuck .typeConfusion := by rfl

/-- A call of a function the program does not have is `unbound`; elaboration
resolves every name before the core (§2). -/
example : run (scalarProg tI64 (call 7 [])) demoFuel = .stuck .unbound := by rfl

/-! ## Drop order, pinned

§6.11 fixes the order in which a drop's events come out: a value's own
destructor first, then its fields in declaration order, recursively; and a
frame's teardown reads its scope record newest-first (§6.9). These pin both.
-/

/-- The unwind order: an early `return` past two live affine bindings drops
the newer one first (§6.9's (D-Return); `3.9:18`). -/
example : run returnPastAffine demoFuel
    = .ok [.dead, .dead] (v64 7)
        [.drop 1 (.struct sAffine [v64 4]), .dtor sAffine (.struct sAffine [v64 4]),
         .drop 0 (.struct sAffine [v64 3]), .dtor sAffine (.struct sAffine [v64 3])] := by rfl

/-- §6.11's order inside one value: the outer destructor, then the fields in
declaration order — so the nested destructor runs **after** the outer one. -/
example : run (prog tI64 structNestedDrop) demoFuel
    = .ok [.dead] (v64 9)
        [.drop 0 (.struct sOuter [v64 1, .struct sAffine [v64 2]]),
         .dtor sOuter (.struct sOuter [v64 1, .struct sAffine [v64 2]]),
         .dtor sAffine (.struct sAffine [v64 2])] := by rfl

/-- Fields drop in declaration order, not in reverse: the struct here has no
destructor of its own, so its trace is exactly its two fields' (§6.11). -/
example : run (prog tI64 structFieldOrder) demoFuel
    = .ok [.dead] (v64 0)
        [.drop 0 (.struct sTwoAffine [.struct sAffine [v64 1], .struct sAffine [v64 2]]),
         .dtor sAffine (.struct sAffine [v64 1]),
         .dtor sAffine (.struct sAffine [v64 2])] := by rfl

/-- `@drop` of a value that is linear only through a field runs the whole
value's glue: the field's destructor is the one observable event. -/
example : run (prog tI64 structLinearFieldDropped) demoFuel
    = .ok [.dead] (v64 0)
        [.drop 0 (.struct sCarry [v64 1, .struct sLinearDtor [v64 2]]),
         .dtor sLinearDtor (.struct sLinearDtor [v64 2])] := by rfl

/-- A by-value parameter the callee never consumes is dropped at the frame
pop ((D-Return-Value) §6.9), not at the caller. -/
example : run paramDroppedAtPop demoFuel
    = .ok [.dead] (v64 1)
        [.drop 0 (.struct sAffine [v64 2]), .dtor sAffine (.struct sAffine [v64 2])] := by rfl

/-! ## Fuel, as an outcome

`outOfFuel` is not a machine state: it is the interpreter saying it stopped
early. `fuel_mono` says that raising the bound never changes an answer, and
`no_masking` that no bound turns a violation into exhaustion — so the two
lines below are a bound that is too small and the same program at a bound
that is not. -/

/-- Sixteen units of fuel is one too few for `countdown`: the interpreter
stops early and says so. -/
example : run countdown 16 = .outOfFuel := by rfl

/-- Seventeen is enough, and the answer is a value with five retired
parameter cells — one per frame the recursion pushed. -/
theorem countdown_at_17 :
    run countdown 17 = .ok [.dead, .dead, .dead, .dead, .dead] (v64 10) [] := by rfl

/-- `fuel_mono` in use: every larger bound gives that same answer, so the
∀-fuel shape of `soundness` is a statement about one outcome. -/
example : run countdown demoFuel = run countdown 17 :=
  fuel_mono (by decide) (fun h => absurd (countdown_at_17.symm.trans h) (by simp))

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

example : run (scalarProg tI64 (binop .add (boolLit true) (binop .div (lit 1) (lit 0)))) demoFuel
    = .panic .divZero [] := by rfl
example : run (scalarProg tI64
    (binop .add (lit 1) (.intLit .w8 .signed 1))) demoFuel
    = .stuck .typeConfusion := by rfl
example : run (scalarProg tI64 (lit (2 ^ 64))) demoFuel
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

example : eval demoFuel (scalarProg tI64 unitLit) [.dead] { env := [0], scope := [] } (use 0)
    = .stuck .useAfterDrop := by rfl
example : eval demoFuel (scalarProg tI64 unitLit) [.dead] { env := [0], scope := [] } (drop 0)
    = .stuck .useAfterDrop := by rfl
example : eval demoFuel (scalarProg tI64 unitLit) [.dead] { env := [0], scope := [] }
    (assign 0 (lit 1)) = .stuck .useAfterDrop := by rfl

/-- The same guard on the unwind path: a frame whose scope record names a
retired cell refuses instead of retiring it twice (§6.9). `FrameMatches` is
what excludes this state for a well-typed program. -/
example : eval demoFuel (scalarProg tI64 unitLit) [.dead] { env := [0], scope := [0] }
    (ret (lit 1)) = .stuck .useAfterDrop := by rfl

#eval checkProgram (scalarProg tI64 scalars)
#eval checkProgram (prog tI64 linearLeaked)

end RueCore.Examples
