import RueCore.Checker
import RueCore.Examples
import RueCore.Print

/-!
# RueCore.Corpus — the bridge corpus (ADR-0097, RUE-2227)

Every case pairs a fragment program with what the mechanization says about
it: the verified checker's verdict (§5, `checkProgram_sound`) and the
interpreter's outcome (§6, `run`). The exporter prints each program as Rue
source
(`Print.lean`) and emits the cases as JSON for `crates/rue-oracle-diff`'s
consumer (RUE-2228), which runs the compiler, the oracle, and the native
binary on the source and reports every pairwise disagreement.

## The JSON contract

One array of case objects. Fields:

* `name`, `description`, `rules` — identity, one sentence for a reader, and
  the calculus rules the case exercises.
* `source` — the complete Rue program.
* `verdict` — `{"accept": {"type": <Rue type name>}}` when the checker
  accepts the program — every function well-formed by (Fn) §5.8 and the
  entry point taking no parameters (`checkProgram`) — so §7's theorems apply
  to it and the compiler must accept it; or `{"reject": {}}` (the compiler
  must reject it with an ownership diagnostic). The type is the entry
  function's declared return type.

  An `accept` verdict is backed by a proof (`checkProgram_sound` plus §7), so
  a compiler that rejects one is wrong. A `reject` verdict is **not**: it is
  the absence of an acceptance from an algorithm that is deliberately
  narrower than the rule, so it is only trustworthy on shapes where `check`
  is complete. It is not complete on `return`: a `return` arm of an `if`
  contributes its post-operand state to §5.5's join, where §5.7 excludes a
  diverging arm's state entirely, so a binding that arm moved out is
  unusable after the `if` and a program the calculus derives — and the
  compiler accepts — is rejected here (`Checker.lean`, "what completeness
  costs"). That shape would be a *false* bridge failure, so nothing produces
  it: the seed cases below use `return` only where `check` is complete, and
  `Gen.lean` emits no `ret` at all.
* `expected` — the interpreter's outcome for an accepted program:
  `{"kind": "ok", "stdout": [<line>...], "exit": 0}`, where the lines are the
  run's **observable events** in trace order — one per user destructor and one
  per `@dbg` — followed by the lines `main` shows for the program's value
  (`Print.lean` documents the mapping: a scalar prints itself, and a struct
  value is dropped, so its lines are the ones its drop emits). Both channels
  come off the one trace, so a `@dbg` between two drops is between them here
  too. A drop with no destructor anywhere in it is unobservable in Rue and
  contributes no line; the interpreter's `drop ℓ v` and `dropTemp v` events
  mark where a drop *starts* and are likewise not lines. Or
  `{"kind": "panic", "panic": <name>, "stdout": [<line>...]}` for a §6.12
  trap, where the lines are the ones the run produced **before** the trap:
  §6.12's outcome keeps the observable output a trapping run emitted, and the
  process prints what it printed and then exits 101. For a rejected program
  `expected` is `{"kind": "stuck", "violation":
  <name>}`: the refusal the machine reaches, kernel-checked in
  `Examples.lean` and below, which the bridge cannot observe because the
  compiler rejects the program first. A rejected program whose executed path
  never reaches the refusal — it lies on a path the program does not take,
  whether a §5.5 join disagreement or a refusal inside the arm the condition
  skips — carries the `ok` or `panic` outcome of the executed path instead,
  so a compiler that accepts it unsoundly is still compared against what the
  machine does. The seed corpus has no such case; the generator (`Gen.lean`)
  produces them. Every line is a bare integer or `true`/`false`, so the
  projection is not injective: a destructor line `n` swapped with a `@dbg`
  line `n` or a value line `n` would not be told apart. Accepted at fragment
  scope.

Every case is evaluated at one fixed bound, `exportFuel`, and a case the
bound does not complete is **not exported**: `outOfFuel` is the interpreter
admitting it stopped early, not a claim about the program, and there is no
honest `expected` to compare an implementation against. `fuel_mono`
(`Soundness.lean`) is why one bound is enough to speak for every larger one.
The seed corpus completes far inside the bound; a generated program that did
not would simply be absent from the output.

The output is deterministic: cases are listed in a fixed order and nothing
depends on the environment.

The declarations here are cases and their export, not rules (`xref: examples`
for `scripts/validate-lean-xref-index.py`, which indexes a case's citations
without requiring them).
-/

namespace RueCore.Corpus

open Expr

/-- A corpus case: a closed fragment program with its documentation. A
program is a struct environment and a list of function definitions, entered at
function index `0` (§6.12). -/
structure Case where
  name : String
  description : String
  rules : List String
  prog : Program

/-- The fuel every exported case is evaluated at. Deep enough for every seed
case by a wide margin; a case the bound does not complete is left out of the
export rather than given an outcome (module docstring). -/
def exportFuel : Nat := 100000

/-! ## The seed corpus

`Examples.lean`'s programs, plus one witness per §7 bullet the fragment
covers and one per drop point of the machine. -/

def cases : List Case := [
  { name := "scalars",
    description := "Well-typed scalar flow: a binding used twice by copy.",
    rules := ["(Use-Copy) §5.1", "(Let) §5.3"],
    prog := Examples.scalarProg Examples.tI64 Examples.scalars
    },
  { name := "affine_scope_drop",
    description := "An affine resource silently dropped at scope exit; its destructor prints before the value.",
    rules := ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7", "§6.11"],
    prog := Examples.prog Examples.tI64 Examples.affineDrop
    },
  { name := "linear_consumed",
    description := "A linear resource consumed exactly once; no drop event.",
    rules := ["(Use-Move) §5.1", "§5.8 whole-value elimination"],
    prog := Examples.prog Examples.tI64 Examples.linearConsumed
    },
  { name := "linear_leaked",
    description := "A linear resource reaching scope exit unconsumed: rejected statically (E0406) and refused dynamically (linearLeak).",
    rules := ["§5.6 residual-linear leak check", "3.8:32"],
    prog := Examples.prog Examples.tI64 Examples.linearLeaked
    },
  { name := "use_after_move",
    description := "A moved affine binding dropped again: rejected statically (E0205) and refused dynamically (useAfterMove).",
    rules := ["(Use-Move) §5.1", "3.8:5"],
    prog := Examples.prog Examples.tI64 Examples.useAfterMove
    },
  { name := "reinit",
    description := "Move a linear value out, assign a new one back in, consume it: legal reinitialization.",
    rules := ["(Assign) §5.2", "3.8:55"],
    prog := Examples.prog Examples.tI64 Examples.reinit
    },
  { name := "linear_half_consumed",
    description := "A linear value consumed in one arm of an if only: the §5.5 join rejects it; dynamically it leaks on the other path.",
    rules := ["(If) §5.5 join", "3.8:50"],
    prog := Examples.prog Examples.tI64 Examples.linearHalfConsumed
    },
  { name := "overflow",
    description := "intMax + 1 traps with a defined overflow panic.",
    rules := ["§6.4 arithmetic traps", "3.1:6"],
    prog := Examples.scalarProg Examples.tI64 Examples.overflow
    },
  { name := "div_zero",
    description := "Division by zero traps with a defined panic.",
    rules := ["§6.4 arithmetic traps", "4.2:11"],
    prog := Examples.scalarProg Examples.tI64 Examples.divZero
    },
  { name := "copy_resource",
    description := "A copy struct: @drop is a no-op, uses copy, nothing is ever printed for it.",
    rules := ["(Use-Copy) §5.1", "(@Drop-Copy) §5.3", "3.9:31"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resC (Examples.lit 5))
      (seq (drop 0) (binop .add (consume (use 0)) (consume (use 0))))
    },
  { name := "affine_explicit_drop",
    description := "An affine resource dropped explicitly with @drop: one destructor line, at the @drop site, nothing at scope exit.",
    rules := ["(@Drop) §5.3", "§6.11", "3.9:37"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resA (Examples.lit 8)) (seq (drop 0) (Examples.lit 2))
    },
  { name := "linear_explicit_drop",
    description := "A linear resource discharged by @drop (the only non-move discharge of a linear obligation); its destructor prints at the drop site.",
    rules := ["(@Drop) §5.3", "3.9:39", "§6.11"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resLD (Examples.lit 9)) (seq (drop 0) (Examples.lit 3))
    },
  { name := "affine_temporary_discarded",
    description := "An affine value produced and discarded by a sequence: the machine drops the temporary at the end of the statement.",
    rules := ["(Seq) §5.3", "§6.7 temporary drop"],
    prog := Examples.prog Examples.tI64 <| seq (Examples.resA (Examples.lit 3)) (Examples.lit 4)
    },
  { name := "linear_temporary_discarded",
    description := "A linear value produced and discarded by a sequence: rejected statically (3.8:64) and refused dynamically (linearDiscard).",
    rules := ["(Seq) §5.3", "3.8:64"],
    prog := Examples.prog Examples.tI64 <| seq (Examples.resL (Examples.lit 3)) (Examples.lit 4)
    },
  { name := "affine_overwrite",
    description := "Assigning over a live affine value drops the old value at the assignment, then the new one at scope exit.",
    rules := ["(Assign) §5.2", "§6.8 overwrite-drop", "3.9:18"],
    prog := Examples.prog Examples.tI64 <| letIn true (Examples.resA (Examples.lit 1))
      (seq (assign 0 (Examples.resA (Examples.lit 2))) (Examples.lit 9))
    },
  { name := "linear_overwrite",
    description := "Assigning over a live linear value: rejected statically (3.8:77, the RUE-387 premise) and refused dynamically (linearOverwrite).",
    rules := ["(Assign) §5.2", "3.8:77"],
    prog := Examples.prog Examples.tI64 <| letIn true (Examples.resL (Examples.lit 1))
      (seq (assign 0 (Examples.resL (Examples.lit 2))) (consume (use 0)))
    },
  { name := "join_agrees",
    description := "A linear value consumed in both arms of an if: the join agrees, the program is accepted, and the value chosen is the taken arm's.",
    rules := ["(If) §5.5 join", "3.8:50"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resL (Examples.lit 6))
      (ite (binop .lt (Examples.lit 1) (Examples.lit 2)) (consume (use 0)) (binop .add (consume (use 0)) (Examples.lit 1)))
    },
  { name := "nested_scopes",
    description := "Two affine bindings in nested scopes drop innermost first, each at its own scope's close.",
    rules := ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7", "3.9:2"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resA (Examples.lit 1))
      (letIn false (Examples.resA (Examples.lit 2)) (Examples.lit 0))
    },
  { name := "resource_result",
    description := "The program's value is a struct: main lets it drop, so its destructor is the value line.",
    rules := ["§4.3 expression value", "§6.11"],
    prog := Examples.prog (.struct Examples.sAffine) <|
      letIn false (Examples.lit 4) (Examples.resA (use 0))
    },
  { name := "cond_drop_affine",
    description := "An affine resource dropped explicitly in one arm of an if and left to scope exit on the other: accepted (the join sends it to MovedOut), one destructor line either way. The bridge found the compiler ICEing on this (RUE-2290, fixed); the case stays as the regression signal.",
    rules := ["(@Drop) §5.3", "(If) §5.5 join", "3.9:38"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resA (Examples.lit 5))
      (seq (ite (boolLit true) (drop 0) unitLit) (Examples.lit 9))
    },
  { name := "bool_result",
    description := "A boolean value from a comparison.",
    rules := ["§5.8 operator statics", "§6.4"],
    prog := Examples.scalarProg .bool <| binop .lt (Examples.lit 3) (Examples.lit 2)
    },
  { name := "struct_copy_twice",
    description := "A @copy struct with two integer fields, used twice: contraction is legal at Copy and nothing is dropped.",
    rules := ["(Struct-Intro) §5.8", "(Use-Copy) §5.1", "3.8:18"],
    prog := Examples.prog Examples.tI64 Examples.structCopyTwice
    },
  { name := "struct_linear_field_leaked",
    description := "An attribute-less struct holding a declared-linear field is Linear by §3's join: left to scope exit it is rejected (E0406) and refused (linearLeak).",
    rules := ["(Struct-Intro) §5.8", "§5.6 residual-linear leak check", "3.8:58"],
    prog := Examples.prog Examples.tI64 Examples.structLinearFieldLeaked
    },
  { name := "struct_linear_field_dropped",
    description := "The same linear-carrying struct discharged by @drop: the whole value's glue runs, so the linear field's destructor prints.",
    rules := ["(Struct-Intro) §5.8", "(@Drop) §5.3", "§6.11", "3.9:39"],
    prog := Examples.prog Examples.tI64 Examples.structLinearFieldDropped
    },
  { name := "struct_nested_dtor_drop",
    description := "A destructor-bearing struct holding a destructor-bearing struct, dropped at scope exit: the outer destructor runs first, then the fields in declaration order, so the trace is 1 then 2.",
    rules := ["(Struct-Intro) §5.8", "§6.11", "§5.6 scope exit", "3.9:2"],
    prog := Examples.prog Examples.tI64 Examples.structNestedDrop
    },
  { name := "struct_field_drop_order",
    description := "A struct with no destructor of its own holding two destructor-bearing fields: scope exit drops them in declaration order, 1 then 2.",
    rules := ["(Struct-Intro) §5.8", "§6.11", "3.9:2"],
    prog := Examples.prog Examples.tI64 Examples.structFieldOrder
    },
  { name := "struct_join_disagrees",
    description := "The §5.5 join on a linear-carrying struct entry: discharged in one arm only, which 3.8:50 makes ill-formed; on the path taken it leaks.",
    rules := ["(If) §5.5 join", "3.8:50", "3.8:58"],
    prog := Examples.prog Examples.tI64 Examples.structJoinDisagrees
    },
  { name := "call_plain",
    description := "A plain call by value: main calls a two-parameter function that adds its parameters.",
    rules := ["(Call) §5.8", "(D-Call) §6.9", "(D-Return-Value) §6.9"],
    prog := Examples.callPlain },
  { name := "return_past_affine",
    description := "An early return past two live affine bindings: the frame unwinds newest-first, so the destructors print 4 then 3, then the value 7.",
    rules := ["(Return-Value) §5.7", "(D-Return) §6.9", "3.9:18", "3.9:4"],
    prog := Examples.returnPastAffine },
  { name := "return_past_linear",
    description := "An early return past a live linear binding: rejected statically (E0406, the §5.6 obligation at the ⊥_exit edge) and refused dynamically (linearLeak).",
    rules := ["(Return-Value) §5.7", "3.8:62", "§5.6 residual-linear leak check"],
    prog := Examples.returnPastLinear },
  { name := "param_dropped_at_frame_pop",
    description := "A by-value affine argument the callee never consumes: its drop runs at the frame pop, before the caller sees the value.",
    rules := ["(Call) §5.8", "(D-Call) §6.9", "(D-Return-Value) §6.9", "3.8:62"],
    prog := Examples.paramDroppedAtPop },
  { name := "linear_param_leaked",
    description := "A by-value linear parameter the callee never consumes: (Fn) §5.8's second clause rejects the function (E0406) and the frame pop refuses with linearLeak.",
    rules := ["(Fn) §5.8", "3.8:62"],
    prog := Examples.linearParamLeaked },
  { name := "recursion_trap",
    description := "Recursion four frames deep, ending in a division by zero: a defined trap reached through a call chain.",
    rules := ["(Call) §5.8", "(D-Call) §6.9", "§6.4 arithmetic traps"],
    prog := Examples.recursionTrap },
  { name := "i8_overflow",
    description := "max_T + 1 at i8: the arithmetic trap at the narrowest width, where the bound is 127.",
    rules := ["(Arith) §5.8", "(D-Arith-Trap) §6.4", "3.1:6"],
    prog := Examples.scalarProg (.int .w8 .signed) Examples.i8Overflow
    },
  { name := "u8_underflow",
    description := "0 - 1 at u8: the same trap reached downward, since Rue has no wrapping subtraction.",
    rules := ["(Arith) §5.8", "(D-Arith-Trap) §6.4", "3.1:6"],
    prog := Examples.scalarProg (.int .w8 .unsigned) Examples.u8Underflow
    },
  { name := "i8_div_min_by_neg_one",
    description := "min_T / -1 at i8: the quotient that is not representable.",
    rules := ["(Arith) §5.8", "(D-Div-Overflow) §6.4", "8.1:3"],
    prog := Examples.scalarProg (.int .w8 .signed) Examples.i8DivMinByNegOne
    },
  { name := "i8_rem_min_by_neg_one",
    description := "min_T % -1 at i8: §6.4 traps although the mathematical remainder is 0.",
    rules := ["(Arith) §5.8", "(D-Div-Overflow) §6.4", "8.1:3"],
    prog := Examples.scalarProg (.int .w8 .signed) Examples.i8RemMinByNegOne
    },
  { name := "i8_rem_zero",
    description := "% by a zero divisor: the rem-zero trap, §6.12's own category beside div-zero.",
    rules := ["(Arith) §5.8", "§6.4 arithmetic traps", "§6.12"],
    prog := Examples.scalarProg (.int .w8 .signed) Examples.i8RemZero
    },
  { name := "int_cast_out_of_range",
    description := "@intCast of 300 to u8: the value does not fit the target type, so the conversion traps.",
    rules := ["(Int-Cast) §5.8", "(D-Int-Cast-Trap) §6.4", "4.13:28"],
    prog := Examples.scalarProg (.int .w8 .unsigned) Examples.u8CastOutOfRange
    },
  { name := "int_cast_in_range",
    description := "@intCast of 200 to u8: the value fits, so the conversion carries it across.",
    rules := ["(Int-Cast) §5.8", "(D-Int-Cast) §6.4", "4.13:26"],
    prog := Examples.scalarProg (.int .w8 .unsigned) Examples.u8CastInRange
    },
  { name := "shift_masks_width",
    description := "1 << 8 at u8: the shift amount is reduced modulo the width, so this shifts by zero and prints 1. Shifting never traps.",
    rules := ["(Arith) §5.8", "(D-Shl) §6.4", "4.3a:10"],
    prog := Examples.scalarProg (.int .w8 .unsigned) Examples.u8ShiftMasks
    },
  { name := "bitwise_at_u8",
    description := "(12 & 10) | ~240 at u8: the bit rules read the w-bit pattern back at the operand's width, so the complement is 15 and the result is 15.",
    rules := ["(Arith) §5.8", "(BitNot) §5.8", "(D-Bit) §6.4", "4.3a:1"],
    prog := Examples.scalarProg (.int .w8 .unsigned) Examples.u8Bitwise
    },
  { name := "negate_at_i16",
    description := "-(3 * 7) at i16: multiplication and the unary negation (Neg) §5.8 admits on a signed type only.",
    rules := ["(Arith) §5.8", "(Neg) §5.8", "(D-Arith) §6.4", "4.2:6"],
    prog := Examples.scalarProg (.int .w16 .signed) Examples.i16Negate
    },
  { name := "unsigned_compare_at_u64",
    description := "max_T > 0 at u64: an unsigned ordering of a value whose signed reading would be negative, so the case tells the two orderings apart.",
    rules := ["(Ord) §5.8", "§6.4", "4.3:5"],
    prog := Examples.scalarProg .bool Examples.u64Compare
    },
  { name := "bool_negate",
    description := "!(3 <= 3): (Not) §5.8 on the one type it admits.",
    rules := ["(Not) §5.8", "(Ord) §5.8", "4.4:2"],
    prog := Examples.scalarProg .bool Examples.boolNegate
    },
  { name := "dbg_scalars",
    description := "@dbg of a negative i8, a u64 and a bool: the observable output §6.12 compares, one line each.",
    rules := ["(Dbg) §5.8", "§6.12"],
    prog := Examples.scalarProg Examples.tI64 Examples.dbgScalars
    },
  { name := "dbg_between_drops",
    description := "A @dbg between two destructors: the two observation channels are one trace, so the line comes out where it happened.",
    rules := ["(Dbg) §5.8", "§6.11", "§6.12", "(@Drop) §5.3"],
    prog := Examples.prog Examples.tI64 Examples.dbgBetweenDrops
    },
  { name := "panic_after_drop",
    description := "A user @panic after an affine @drop: the destructor line survives the trap, and no scope exit runs after it (§5.7 exempts the panic edge).",
    rules := ["(Panic) §5.8", "(D-Panic) §6.12", "(@Drop) §5.3", "§6.11"],
    prog := Examples.prog Examples.tI64 Examples.panicAfterDrop
    },
  { name := "dbg_before_trap",
    description := "A @dbg before a division by zero: the same claim for a trap the program did not ask for.",
    rules := ["(Dbg) §5.8", "(D-Div-Zero) §6.4", "§6.12"],
    prog := Examples.scalarProg Examples.tI64 Examples.dbgBeforeTrap
    },
  { name := "countdown",
    description := "A recursive countdown summing 4+3+2+1+0: every frame pops normally and the value comes back through five call boundaries.",
    rules := ["(Call) §5.8", "(D-Call) §6.9", "(D-Return-Value) §6.9"],
    prog := Examples.countdown }
]

/-! ## Witnesses for the refusals no `Examples.lean` program reaches -/

example : run (Examples.prog Examples.tI64 (letIn true (Examples.resL (Examples.lit 1))
    (seq (assign 0 (Examples.resL (Examples.lit 2))) (consume (use 0))))) exportFuel
    = .stuck .linearOverwrite := by rfl
example : run (Examples.prog Examples.tI64 (seq (Examples.resL (Examples.lit 3)) (Examples.lit 4))) exportFuel
    = .stuck .linearDiscard := by rfl
example : checkProgram (Examples.prog Examples.tI64 (seq (Examples.resL (Examples.lit 3)) (Examples.lit 4)))
    = false := by rfl

/-! ## Outcomes, from the mechanization -/

/-- How `@dbg` renders a value (§5.8's (Dbg); the compiler prints an integer
as its decimal and a `bool` as `true`/`false`, one line each). The calculus
fixes no rendering, so this is the compiler's, verified by hand and compared
by the bridge. A type `@dbg` does not accept has no line, which the statics
exclude (`Ty.observable`). -/
def dbgLine : Val → Option String
  | .int _ _ n => some (toString n)
  | .bool b => some (if b then "true" else "false")
  | .unit | .struct _ _ => none

/-- The line a user destructor prints (`Print.structItem`): the struct's
first field, when that field is an integer. A declaration whose first field is
not an integer has nothing to print, in the printed Rue program and here
alike. -/
def dtorLine : Val → Option String
  | .struct _ (.int w s n :: _) => dbgLine (.int w s n)
  | _ => none

/-- One stdout line per *observable* event, in trace order. Two events are
observable in Rue: a user destructor (`Print.lean`) and `@dbg` (§6.12's
observable output). `drop ℓ v` and `dropTemp v` mark where a drop starts
(§6.11, §6.7) and a drop with no destructor inside it prints nothing. Because
both channels are read off the one trace, a `@dbg` line between two drops
comes out between them. The projection is total and never panics — an event
with no line is simply absent from stdout — so no export can be aborted by a
shape this function did not expect. -/
def eventLine : Event → Option String
  | .dtor _ v => dtorLine v
  | .dbg v => dbgLine v
  | .drop _ _ | .dropTemp _ => none

/-- The lines `main` shows for the program's value (`Print.observeValue`): a
scalar prints itself, `()` prints nothing, and a struct value is dropped — so
its lines are the ones its own drop emits, in §6.11's order. An `error` is a
struct naming a declaration the program does not have, which the verdict
already rejects. -/
def valueLines (D : StructEnv) (v : Val) : List String :=
  match v with
  | .int w s n => (dbgLine (.int w s n)).toList
  | .bool b => (dbgLine (.bool b)).toList
  | .unit => []
  | .struct _ _ =>
      match dropValue D v with
      | .ok evs => evs.filterMap eventLine
      | .error _ => []

def panicName : PanicKind → String
  | .overflow => "overflow"
  | .divZero => "divZero"
  | .remZero => "remZero"
  | .castOverflow => "castOverflow"
  | .user => "user"

def violationName : Violation → String
  | .useAfterMove => "useAfterMove"
  | .useAfterDrop => "useAfterDrop"
  | .linearLeak => "linearLeak"
  | .linearOverwrite => "linearOverwrite"
  | .linearDiscard => "linearDiscard"
  | .unbound => "unbound"
  | .typeConfusion => "typeConfusion"

/-- The stdout the bridge compares, for a completed run: one line per user
destructor the run executed, in trace order, then the lines `main` shows for
the program's value. -/
def outLines (D : StructEnv) (v : Val) (tr : List Event) : List String :=
  tr.filterMap eventLine ++ valueLines D v

/-- A one-line reading of the outcome, for the program's header comment. -/
def outcomeSummary (c : Case) : String :=
  match checkProgram c.prog, run c.prog exportFuel with
  | false, .stuck w => "rejected by the checker; the machine would refuse with " ++ violationName w
  | false, .ok _ v tr =>
      let lines := outLines c.prog.structs v tr
      "rejected by the checker; the refusal lies on a path not taken, and the executed path prints " ++
        (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++ "; exit 0"
  | false, .panic k tr =>
      let lines := tr.filterMap eventLine
      "rejected by the checker; the refusal lies on a path not taken, and the executed path prints " ++
        (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++
        " and then traps with " ++ panicName k
  | true, .ok _ v tr =>
      let lines := outLines c.prog.structs v tr
      "accepted; prints " ++ (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++ "; exit 0"
  | true, .panic k tr =>
      let lines := tr.filterMap eventLine
      "accepted; prints " ++
        (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++
        " and traps with " ++ panicName k
  | true, .stuck w => "accepted yet refused with " ++ violationName w ++ " (impossible by soundness)"
  | _, .returned _ _ _ => "the entry call handed on a return (impossible: the call boundary absorbs it)"
  | _, .outOfFuel => "not completed at the export fuel; this case is not exported"

/-! ## JSON -/

def jsonString (s : String) : String :=
  let escaped := s.foldl (fun acc c =>
    acc ++ match c with
      | '"' => "\\\""
      | '\\' => "\\\\"
      | '\n' => "\\n"
      | '\t' => "\\t"
      | '\r' => "\\r"
      | c => String.singleton c) ""
  "\"" ++ escaped ++ "\""

def jsonArray (items : List String) : String :=
  "[" ++ String.intercalate ", " items ++ "]"

/-- The entry function's declared return type, which is the type the checker
gives the whole program (helper). -/
def resultTyName (c : Case) : String :=
  match c.prog.fns[0]? with
  | some fd => Print.tyName fd.ret
  | none => Print.tyName Examples.tI64

def verdictJson (c : Case) : String :=
  if checkProgram c.prog then
    "{\"accept\": {\"type\": " ++ jsonString (resultTyName c) ++ "}}"
  else "{\"reject\": {}}"

/-- The `expected` field. Two of its arms are unreachable in the exported
document and are here because the function must be total: a `returned` result,
which the entry call's own frame boundary absorbs (`run_ne_returned`), and an
`outOfFuel` one, which `jsonOf` filters out — the consumer knows only `ok`,
`panic` and `stuck` (`crates/rue-oracle-diff/src/lean_corpus.rs`). -/
def expectedJson (c : Case) : String :=
  match run c.prog exportFuel with
  | .ok _ v tr =>
      "{\"kind\": \"ok\", \"stdout\": " ++ jsonArray ((outLines c.prog.structs v tr).map jsonString) ++
        ", \"exit\": 0}"
  | .returned _ v tr =>
      "{\"kind\": \"ok\", \"stdout\": " ++ jsonArray ((outLines c.prog.structs v tr).map jsonString) ++
        ", \"exit\": 0}"
  | .panic k tr =>
      "{\"kind\": \"panic\", \"panic\": " ++ jsonString (panicName k) ++
        ", \"stdout\": " ++ jsonArray ((tr.filterMap eventLine).map jsonString) ++ "}"
  | .stuck w => "{\"kind\": \"stuck\", \"violation\": " ++ jsonString (violationName w) ++ "}"
  | .outOfFuel => "{\"kind\": \"outOfFuel\"}"

/-- Whether a case completed at the export fuel. A case that did not is left
out of the JSON: the interpreter has no outcome to claim for it (module
docstring). -/
def completed (c : Case) : Bool :=
  match run c.prog exportFuel with
  | .outOfFuel => false
  | _ => true

def caseJson (c : Case) : String :=
  "  {\n" ++
  "    \"name\": " ++ jsonString c.name ++ ",\n" ++
  "    \"description\": " ++ jsonString c.description ++ ",\n" ++
  "    \"rules\": " ++ jsonArray (c.rules.map jsonString) ++ ",\n" ++
  "    \"source\": " ++ jsonString (Print.program c.name c.description c.rules (outcomeSummary c) c.prog) ++ ",\n" ++
  "    \"verdict\": " ++ verdictJson c ++ ",\n" ++
  "    \"expected\": " ++ expectedJson c ++ "\n" ++
  "  }"

/-- A list of cases as one JSON document, the cases the export fuel did not
complete left out. -/
def jsonOf (cs : List Case) : String :=
  "[\n" ++ String.intercalate ",\n" ((cs.filter completed).map caseJson) ++ "\n]\n"

/-- The seed corpus as one JSON document. -/
def json : String := jsonOf cases

end RueCore.Corpus
