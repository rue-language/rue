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
  is complete. It is not complete on the two never-typed forms, `return` and
  `@panic`: a diverging arm of an `if` contributes its own state to §5.5's
  join, where §5.7 excludes a diverging arm's state entirely, so a binding
  that arm moved out is unusable after the `if` and a program the calculus
  derives — and the compiler accepts — is rejected here (`Checker.lean`,
  "what completeness costs"). That shape would be a *false* bridge failure,
  so nothing produces it: the seed cases below use `return` and `@panic` only
  where `check` is complete, and `Gen.lean` emits neither.
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
  compiler rejects the program first. A rejected program the machine
  nonetheless runs to completion carries the `ok` or `panic` outcome of the
  executed path instead, so a compiler that accepts it unsoundly is still
  compared against what the machine does. There are two ways to be one. The
  refusal can lie on a path the program does not take — a §5.5 join
  disagreement, or a refusal inside the arm the condition skips — which is
  what the generator (`Gen.lean`) produces. Or the rule the checker applies
  can have **no dynamic counterpart at all**: `3.9:34`'s restriction on moving
  a field out of a destructor-bearing value (E0456), (@Drop) §5.3's residual
  side condition (E0406), and (Assign) §5.2's `3.8:77` premise — keyed on the
  destination's *type*, where the machine's `linearOverwrite` monitor reads the
  residue it is about to drop (E0493) — are static disciplines no monitor
  enforces, so a program they reject still runs. `partial_under_dtor`,
  `linear_field_stranded`, `overwrite_past_partial_linear` and
  `overwrite_field_past_partial_linear` below are those cases.

  Every line is a bare integer or `true`/`false`, so the projection is not
  injective: a destructor line `n` swapped with a `@dbg` line `n` or a value
  line `n` would not be told apart. Accepted at fragment scope.

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

/-- The model every exported case is evaluated at: `Float.exactOps`
(`Float.lean`), the constructive instance whose `σ_NaN` is **positive** — the
AArch64 choice of `3.12:44`, spelled `nanSign := false` because the field is
the sign *bit* of `FloatDatum.nan`. `σ_NaN` is a target parameter (§2), so it would
be a divergence if it reached an expectation; it does not, because `@dbg`
renders a NaN as `NaN` whatever its sign (`3.12:42`) and nothing reads one
through `@total_cmp`, the only form that can see it — no seed case applies
`@total_cmp` to anything but a float literal, and `Gen` draws its operands as
literals, which are finite by construction. -/
def exportOps : FloatOps := Float.exactOps

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
    description := "A linear resource moved into a new binding and discharged there: the move transfers the obligation and @drop is what finally satisfies it; S2 declares no destructor, so nothing prints.",
    rules := ["(Use-Move) §5.1", "(@Drop) §5.3", "3.9:39"],
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
    description := "Discharge a linear value, assign a new one back in, discharge that: legal reinitialization.",
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
      (seq (drop (.var 0))
        (binop .add (use (.proj (.var 0) 0)) (use (.proj (.var 0) 0))))
    },
  { name := "affine_explicit_drop",
    description := "An affine resource dropped explicitly with @drop: one destructor line, at the @drop site, nothing at scope exit.",
    rules := ["(@Drop) §5.3", "§6.11", "3.9:37"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resA (Examples.lit 8)) (seq (drop (.var 0)) (Examples.lit 2))
    },
  { name := "linear_explicit_drop",
    description := "A linear resource discharged by @drop (the only non-move discharge of a linear obligation); its destructor prints at the drop site.",
    rules := ["(@Drop) §5.3", "3.9:39", "§6.11"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resLD (Examples.lit 9)) (seq (drop (.var 0)) (Examples.lit 3))
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
      (seq (assign (.var 0) (Examples.resA (Examples.lit 2))) (Examples.lit 9))
    },
  { name := "linear_overwrite",
    description := "Assigning over a live linear value: rejected statically (3.8:77, the RUE-387 premise) and refused dynamically (linearOverwrite).",
    rules := ["(Assign) §5.2", "3.8:77"],
    prog := Examples.prog Examples.tI64 <| letIn true (Examples.resL (Examples.lit 1))
      (seq (assign (.var 0) (Examples.resL (Examples.lit 2)))
        (seq (drop (.var 0)) (Examples.lit 0)))
    },
  { name := "join_agrees",
    description := "A linear value consumed in both arms of an if: the join agrees, the program is accepted, and the value chosen is the taken arm's.",
    rules := ["(If) §5.5 join", "3.8:50"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resL (Examples.lit 6))
      (ite (binop .lt (Examples.lit 1) (Examples.lit 2))
        (seq (drop (.var 0)) (Examples.lit 6))
        (seq (drop (.var 0)) (Examples.lit 7)))
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
      letIn false (Examples.lit 4) (Examples.resA (use (.var 0)))
    },
  { name := "cond_drop_affine",
    description := "An affine resource dropped explicitly in one arm of an if and left to scope exit on the other: accepted (the join sends it to MovedOut), one destructor line either way. The bridge found the compiler ICEing on this (RUE-2290, fixed); the case stays as the regression signal.",
    rules := ["(@Drop) §5.3", "(If) §5.5 join", "3.9:38"],
    prog := Examples.prog Examples.tI64 <| letIn false (Examples.resA (Examples.lit 5))
      (seq (ite (boolLit true) (drop (.var 0)) unitLit) (Examples.lit 9))
    },
  { name := "bool_result",
    description := "A boolean value from a comparison.",
    rules := ["§5.8 operator statics", "§6.4"],
    prog := Examples.scalarProg .bool <| binop .lt (Examples.lit 3) (Examples.lit 2)
    },
  { name := "struct_copy_twice",
    description := "A @copy struct's field read twice through a projection: contraction is legal at Copy, the read leaves the base Owned, and nothing is dropped.",
    rules := ["(Struct-Intro) §5.8", "(Use-Copy) §5.1", "3.8:18", "(Owned-Base) §5.1"],
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
  { name := "i64_min_times_neg1",
    description := "min_T * -1 at i64: an overflow trap, because -min_T is one past max_T. The bridge found the compiler's constant folder wrapping this one and exiting 0 where every non-constant spelling of it traps (RUE-2318); the case stays as the regression signal, so the seed run is red on it until that is fixed.",
    rules := ["(Arith) §5.8", "(D-Arith-Trap) §6.4", "8.1:3"],
    prog := Examples.scalarProg Examples.tI64 Examples.i64MinTimesNeg1
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
  { name := "partial_move_residue",
    description := "A field moved out of a two-field struct and discharged, the rest left to scope exit: the moved field's destructor prints at the @drop and the remaining one's at the scope exit, and the hole is skipped so nothing is dropped twice.",
    rules := ["(Use-Move) §5.1", "§4.2 partial move", "3.8:22", "§6.11", "3.8:60"],
    prog := Examples.prog Examples.tI64 Examples.partialMoveResidue
    },
  { name := "partial_then_whole",
    description := "A field moved out and then the whole value: fully-owned(Σ, p) fails at the whole place, so the use is rejected (E0205, use of partially moved value).",
    rules := ["(Use-Move) §5.1", "3.8:26", "3.8:5"],
    prog := Examples.prog Examples.tI64 Examples.partialThenWhole
    },
  { name := "partial_under_dtor",
    description := "A field moved out of a value whose type declares a destructor: 3.9:34 forbids it (E0456), because the destructor runs on the whole value and would observe the hole.",
    rules := ["(Use-Move) §5.1", "3.9:34"],
    prog := Examples.prog Examples.tI64 Examples.partialUnderDtor
    },
  { name := "copy_through_partial",
    description := "A Copy field read through a partially moved base: Σ(p) = Owned holds at the base although a path under it is MovedOut, so the read is legal.",
    rules := ["(Use-Copy) §5.1", "(Owned-Base) §5.1", "3.8:53"],
    prog := Examples.prog Examples.tI64 Examples.copyThroughPartial
    },
  { name := "drop_field_then_whole",
    description := "@drop at a field and then @drop of the whole: §5.3 asks only Σ(p) = Owned of the second, and §6.11's walk drops the owned residue and skips the hole.",
    rules := ["(@Drop) §5.3", "§6.11", "3.8:60"],
    prog := Examples.prog Examples.tI64 Examples.dropFieldThenWhole
    },
  { name := "reinit_field",
    description := "A moved-out field reinitialized by assignment and the whole value then moved: the subtree at the path is Owned again, so fully-owned holds.",
    rules := ["(Assign) §5.2", "3.8:55", "(Use-Move) §5.1"],
    prog := Examples.prog Examples.tI64 Examples.reinitField
    },
  { name := "overwrite_field",
    description := "Assignment over a live affine field: §6.8's overwrite-drop runs the old field's destructor before the store, and the new one drops at scope exit.",
    rules := ["(Assign) §5.2", "§6.8 overwrite-drop"],
    prog := Examples.prog Examples.tI64 Examples.overwriteField
    },
  { name := "partial_move_one_arm",
    description := "A field dropped in one arm of an if only: the §5.5 join sends that path to MovedOut and the sibling stays Owned; the machine drops whatever the taken path left.",
    rules := ["(If) §5.5 join", "3.8:60", "(@Drop) §5.3"],
    prog := Examples.prog Examples.tI64 Examples.partialMoveOneArm
    },
  { name := "partial_move_other_arm",
    description := "The same program on the path that does not move the field: the drop is path-specific, so the observable output is the same either way.",
    rules := ["(If) §5.5 join", "3.8:60", "§6.11"],
    prog := Examples.prog Examples.tI64 Examples.partialMoveOtherArm
    },
  { name := "deep_path",
    description := "A path two field steps deep: @drop(v.x0.x1) moves exactly that leaf, and the scope exit drops the rest of the tree in declaration order.",
    rules := ["(@Drop) §5.3", "§4.2 partial move", "§6.11", "3.9:13"],
    prog := Examples.prog Examples.tI64 Examples.deepPath
    },
  { name := "linear_field_residue",
    description := "The RUE-1591 idiom at a path: consume exactly the linear field of an infectious carrier and let the non-linear residue drop — §5.6's obligation is keyed on the residual state, so the scope exit is legal.",
    rules := ["(@Drop) §5.3", "§5.6 residual-linear leak check", "3.8:74"],
    prog := Examples.prog Examples.tI64 Examples.linearFieldResidue
    },
  { name := "linear_field_stranded",
    description := "The other order: @drop of the affine field first leaves a still-owned linear sub-place under a partially moved place, and (@Drop) §5.3's last premise rejects the whole-value drop (E0406).",
    rules := ["(@Drop) §5.3", "3.8:32"],
    prog := Examples.prog Examples.tI64 Examples.linearFieldStranded
    },
  { name := "join_whole_against_partial",
    description := "The §5.5 join of a whole move against a partial one, on a carrier whose only linear content is the field the other arm consumed: both paths discharge the obligation, so the join is MovedOut rather than ill-formed.",
    rules := ["(If) §5.5 join", "3.8:50", "§5.6 residual-linear leak check"],
    prog := Examples.prog Examples.tI64 Examples.joinWholeAgainstPartial
    },
  { name := "join_linear_field_one_arm",
    description := "The same shape where the linear field survives on one path: 3.8:50 makes the join ill-formed (E0443, not consumed on all paths).",
    rules := ["(If) §5.5 join", "3.8:50"],
    prog := Examples.prog Examples.tI64 Examples.joinLinearFieldOneArm
    },
  { name := "overwrite_past_partial_linear",
    description := "A root whose type carries a linear value, reassigned after a @drop took the linear field out: (Assign)'s 3.8:77 premise is keyed on the destination's type, so it is rejected (E0493) although the residue carries nothing and the machine runs it.",
    rules := ["(Assign) §5.2", "3.8:77", "§6.8 overwrite-drop"],
    prog := Examples.prog Examples.tI64 Examples.overwritePastPartialLinear
    },
  { name := "overwrite_field_past_partial_linear",
    description := "The same divergence one field step down: the assignment target is a field whose own type carries a linear value, with that linear leaf already moved out — rejected on the type (E0493), and the machine runs it.",
    rules := ["(Assign) §5.2", "3.8:77", "3.8:60"],
    prog := Examples.nestCarryProg Examples.tI64 Examples.overwriteFieldPastPartialLinear
    },
  { name := "enum_match_affine",
    description := "An affine payload with a destructor, matched and bound: the binding drops at the arm's end (6.3:17's timing), and the enum the match moved out drops nothing at scope exit.",
    rules := ["(Match) §5.5", "(D-Match) §6.6", "(D-EndScope) §6.7", "6.3:17"],
    prog := Examples.enumProg Examples.tI64 Examples.enumMatchAffine
    },
  { name := "enum_drop_unmatched",
    description := "The same enum never matched, dropped at scope exit beside a discriminant-only value: §6.11 reads the tag and drops the active variant's payload only, and nothing at all for the tag-only one.",
    rules := ["§5.6 scope exit", "§6.11", "6.3:20"],
    prog := Examples.enumProg Examples.tI64 Examples.enumDropUnmatched
    },
  { name := "enum_match_one_arm",
    description := "A linear-payload enum consumed by a match in one arm of an if only: class(E) is the payload join over every variant, so the Owned side is residual and the §5.5 join rejects it (E0443); the executed path runs.",
    rules := ["(Match) §5.5", "(If) §5.5 join", "3.8:50", "6.3:19"],
    prog := Examples.enumProg Examples.tI64 Examples.enumMatchOneArm
    },
  { name := "enum_arm_leaks_payload",
    description := "An arm binds a linear payload and leaves it: §5.6's check at the arm's end is the leak (E0406 on the binding), and the machine refuses with linearLeak.",
    rules := ["(Match) §5.5", "§5.6 residual-linear leak check", "6.3:17", "3.8:32"],
    prog := Examples.enumProg Examples.tI64 Examples.enumArmLeaksPayload
    },
  { name := "enum_arm_drops_payload",
    description := "The same arm with the payload discharged by @drop: consuming the payload discharges the enum's own obligation, and the destructor prints before the value.",
    rules := ["(Match) §5.5", "(@Drop) §5.3", "6.3:19"],
    prog := Examples.enumProg Examples.tI64 Examples.enumArmDropsPayload
    },
  { name := "enum_copy_matched_twice",
    description := "A discriminant-only enum and a Copy-payload one, each matched twice: the empty and the scalar join are Copy (6.3:19), so the scrutinee is a copy that leaves the binding Owned and the payload binding drops nothing.",
    rules := ["(Match) §5.5", "(Use-Copy) §5.1", "6.3:19"],
    prog := Examples.enumProg Examples.tI64 Examples.enumCopyMatchedTwice
    },
  { name := "enum_match_projection",
    description := "The scrutinee is a projection of a struct holding the enum beside an affine sibling: the match takes the partial move of 3.8:22 at that path, and the scope exit drops the fields in declaration order with the moved one skipped.",
    rules := ["(Match) §5.5", "(Use-Move) §5.1", "3.8:22", "§6.11", "3.9:13"],
    prog := Examples.enumProg Examples.tI64 Examples.enumMatchProjection
    },
  { name := "enum_two_payload_bindings",
    description := "Two payload bindings, both affine with destructors: at the arm's end they drop newest-first, so the second component goes before the first (3.9:4).",
    rules := ["(Match) §5.5", "(D-Match) §6.6", "(D-EndScope) §6.7", "3.9:4"],
    prog := Examples.enumProg Examples.tI64 Examples.enumTwoPayloadBindings
    },
  { name := "enum_payload_moved_into_call",
    description := "A Linear payload moved into a call in the one arm of an if that matches: the other path leaves the enum Owned, so the §5.5 join is ill-formed (E0443); the executed path runs and the callee's @drop prints.",
    rules := ["(Match) §5.5", "(If) §5.5 join", "(Call) §5.8", "3.8:50", "6.3:19"],
    prog := Examples.enumPayloadMovedIntoCall
    },
  { name := "enum_arm_moves_affine_drops_linear",
    description := "One arm, two payload components of different classes: the affine one is moved into an outer mut binding, whose overwrite-drop runs first, and the linear one is @dropped, which discharges class(E).",
    rules := ["(Match) §5.5", "(Assign) §5.2", "(@Drop) §5.3", "§6.8 overwrite-drop", "6.3:19"],
    prog := Examples.enumProg Examples.tI64 Examples.enumArmMovesAffineDropsLinear
    },
  { name := "enum_return_past_payload",
    description := "A return out of an arm, past the arm's two payload locals and an outer binding: §6.9's unwind walks σ newest-first, and (D-Match) appended the payload cells to the innermost scope record.",
    rules := ["(Match) §5.5", "(Return-Value) §5.7", "(D-Return) §6.9", "(D-Match) §6.6", "3.9:4"],
    prog := Examples.enumProg Examples.tI64 Examples.enumReturnPastPayload
    },
  { name := "enum_temporary_scrutinee",
    description := "A temporary scrutinee: the enum is built in scrutinee position and never bound, so the arm's payload binding is the only owner there is.",
    rules := ["(Match) §5.5", "(Enum-Intro) §5.5", "(D-Match) §6.6", "6.3:17"],
    prog := Examples.enumProg Examples.tI64 Examples.enumTemporaryScrutinee
    },
  { name := "enum_call_scrutinee",
    description := "A call in scrutinee position: the enum comes back across a frame boundary and (D-Match) binds its payload in the caller's frame, so the payload's drop is owed to the caller's arm and not to the callee's pop.",
    rules := ["(Match) §5.5", "(Call) §5.8", "(D-Call) §6.9", "(D-Match) §6.6"],
    prog := Examples.enumCallScrutinee
    },
  { name := "enum_matched_twice_moving",
    description := "Two matches on the same non-Copy binding: a match scrutinee is a value context, so the first match moves the binding out (3.8:7, 3.8:76; 6.3:17 for the payload it binds out of it), and the second is the use of a moved-out place (E0205) and the machine refuses with useAfterMove.",
    rules := ["(Match) §5.5", "(Use-Move) §5.1", "3.8:7", "6.3:17", "3.8:5"],
    prog := Examples.enumProg Examples.tI64 Examples.enumMatchedTwiceMoving
    },
  { name := "enum_two_linear_values",
    description := "Two values of one Linear-payload enum, one at each variant, each consumed by its own match: the K0 payload is @dropped and the K1 arm has none, and both obligations are met because the match consumed each value.",
    rules := ["(Match) §5.5", "(@Drop) §5.3", "6.3:19", "6.3:14"],
    prog := Examples.enumProg Examples.tI64 Examples.enumTwoLinearValues
    },
  { name := "enum_holder_partial_then_drop",
    description := "The carrier a match partially moved, dropped explicitly: (@Drop) §5.3 asks only that the place be Owned, and §6.11's walk skips the ⊘ the match left at the enum field.",
    rules := ["(Match) §5.5", "(@Drop) §5.3", "3.8:22", "§6.11", "3.9:13"],
    prog := Examples.enumProg Examples.tI64 Examples.enumHolderPartialThenDrop
    },
  { name := "destructure_copy_leaf",
    description := "A Copy field read out of a declared-linear struct: §4.2's central override consumes the whole struct for a Copy leaf, and the affine residue drops at the access rather than at scope exit.",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "(D-Use-Declared-Linear) §6.3", "3.8:33"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureCopyLeaf
    },
  { name := "destructure_affine_leaf",
    description := "The other half of the same struct: the selected leaf is the droppable field, so it lives on in its own binding and drops at that binding's scope exit, while the Copy residue is destroyed silently at the access.",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "§6.3 split", "3.8:33"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureAffineLeaf
    },
  { name := "destructure_through_plain",
    description := "The plan consumes the smallest enclosing declared-linear place: h.x0.x0 destructures h.x0 only, so h.x1 is still readable afterwards and drops at scope exit.",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "§4.2 dl(Γ,p)", "3.8:33"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureThroughPlain
    },
  { name := "destructure_two_levels",
    description := "Two declared-linear levels, selected one at a time: the outer destructure hands on the inner struct whole and destroys the outer residue, and the inner one then takes a Copy leaf.",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "§4.2 dl(Γ,p)", "3.8:33"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureTwoLevels
    },
  { name := "drop_declared_copy_leaf",
    description := "@drop at a declared-linear plan consumes the whole place even at a Copy leaf: the residue is destroyed at the @drop and nothing is left to drop at scope exit. The citation is the calculus §5.3 and 3.8:33's destructure, not §3.9: 3.9:37-39 describe @drop at the named place only, and 3.9:39's no-op on a @copy value is about that place, not about a Copy leaf reached through a declared-linear prefix. The spec has no paragraph for the enclosing declared-linear place yet (RUE-2338); the compiler and the model already agree on this trace.",
    rules := ["(@Drop) §5.3", "(D-Use-Declared-Linear) §6.3", "3.8:33"],
    prog := Examples.destrProg Examples.tI64 Examples.dropDeclaredCopyLeaf
    },
  { name := "drop_declared_residue_first",
    description := "The order §6.3 fixes: drop* destroys the residue first and §6.11 then drops the selected leaf, so the earlier field's destructor prints before the selected field's.",
    rules := ["(@Drop) §5.3", "§6.3 destructure", "§6.11", "3.8:33"],
    prog := Examples.destrProg Examples.tI64 Examples.dropDeclaredResidueFirst
    },
  { name := "destructure_residue_order",
    description := "The residue drops in declaration order around the selected leaf: the field before it, then the field after it.",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "§6.3 split", "3.8:33"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureResidueOrder
    },
  { name := "destructure_nested_residue",
    description := "The residue traversal recurses into the selected field before it reaches the later sibling, so nested residue is destroyed first.",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "§6.3 split", "3.8:33"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureNestedResidue
    },
  { name := "destructure_array_residue",
    description := "An array in the residue: x.v selects past arr: [S1; 2], which split retains whole and drop* destroys at the access by §6.11's array rule, elements in ascending index order (3.8:73). A retained array is an ordinary residue place; only a selected path through an index step would need §5.1's array clause, which this part does not state (RUE-2327).",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "§6.3 split", "§6.11", "3.8:73"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureArrayResidue
    },
  { name := "destructure_linear_residue",
    description := "A destructure whose residue carries a linear value: rejected statically (3.8:60, E0474) and refused dynamically by the residue monitor (linearLeak).",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "3.8:60"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureLinearResidue
    },
  { name := "destructure_under_dtor",
    description := "A destructure out of a value whose type declares a destructor: 3.9:34 forbids it at every enclosing value, d included (E0456), and no monitor enforces it, so the machine runs the program.",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "3.9:34"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureUnderDtor
    },
  { name := "destructure_one_arm",
    description := "A destructure in one arm of an if only: the §5.5 join meets MovedOut against Owned at a declared-linear place and is ill-formed (E0443); the taken path runs.",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "(If) §5.5 join", "3.8:50"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureOneArm
    },
  { name := "destructure_ancestor_dropped",
    description := "After an inner declared-linear place is destructured, @drop of the declared-linear ancestor discharges it and §6.11 drops exactly the ancestor's own residue. The bridge is red on this one: the compiler rejects it with E0406, and which of the two is right is RUE-2335. The case stays seeded until that is decided.",
    rules := ["(Use-Declared-Linear-Destructure) §5.1", "(@Drop) §5.3", "§5.6 declared clause", "3.8:74"],
    prog := Examples.destrProg Examples.tI64 Examples.destructureAncestorDropped
    },
  { name := "countdown",
    description := "A recursive countdown summing 4+3+2+1+0: every frame pops normally and the value comes back through five call boundaries.",
    rules := ["(Call) §5.8", "(D-Call) §6.9", "(D-Return-Value) §6.9"],
    prog := Examples.countdown },
  { name := "float_arith",
    description := "(1.5 + 2.25) * 2.0 at f64: exact operands, exact result, and no trap rule in sight.",
    rules := ["(Float-Arith) §5.8", "(D-Float-Arith) §6.4", "3.12:21"],
    prog := Examples.scalarProg Examples.tF64 Examples.floatArith },
  { name := "float_copy_class",
    description := "A float binding used twice: class(float(w)) = Copy, so the second use copies and no drop is owed.",
    rules := ["(Use-Copy) §5.1", "3.12:2a", "§3"],
    prog := Examples.scalarProg Examples.tF64 Examples.floatCopy },
  { name := "float_div_by_zero",
    description := "1.0 / 0.0 is +inf, not a trap: no arithmetic trap rule is stated over a float redex.",
    rules := ["(D-Float-Arith) §6.4", "3.12:22", "3.12:23"],
    prog := Examples.scalarProg Examples.tF64 Examples.floatDivZero },
  { name := "float_zero_div_zero",
    description := "0.0 / 0.0 is a NaN, which @dbg renders NaN whatever its sign.",
    rules := ["(D-Float-Arith) §6.4", "3.12:22", "3.12:42"],
    prog := Examples.scalarProg Examples.tF64 Examples.floatZeroDivZero },
  { name := "float_nan_unordered",
    description := "Every ordering compare against a NaN is false, nan <= nan included: the two are unordered.",
    rules := ["(Float-Ord) §5.8", "(D-Float-Ord) §6.4", "3.12:27", "3.12:29"],
    prog := Examples.scalarProg Examples.tI64 Examples.floatNanUnordered },
  { name := "float_signed_zeros",
    description := "-0.0 and +0.0 compare equal, @total_cmp orders -0.0 first, and @dbg tells them apart.",
    rules := ["(D-Float-Ord) §6.4", "(D-Total-Cmp) §6.4", "3.12:28", "3.12:32", "3.12:42"],
    prog := Examples.scalarProg Examples.tI64 Examples.floatSignedZeros },
  { name := "float_infinities",
    description := "The infinities sit at the ends of the ordering and print as inf and -inf.",
    rules := ["(D-Float-Ord) §6.4", "3.12:22", "3.12:42"],
    prog := Examples.scalarProg Examples.tI64 Examples.floatInfinities },
  { name := "float_to_int_trunc",
    description := "@float_to_int truncates toward zero, on both signs.",
    rules := ["(Float-To-Int) §5.8", "(D-Float-To-Int) §6.4", "3.12:17"],
    prog := Examples.scalarProg Examples.tI64 Examples.floatToIntTrunc },
  { name := "float_to_int_trap_nan",
    description := "@float_to_int of a NaN traps with overflow, after the stdout the run had already produced.",
    rules := ["(D-Float-To-Int-Trap) §6.4", "3.12:18", "8.1:7"],
    prog := Examples.scalarProg (.int .w32 .signed) Examples.floatToIntTrapNan },
  { name := "float_to_int_trap_inf",
    description := "@float_to_int of +inf traps: 3.12:18's guard admits both infinities as failures.",
    rules := ["(D-Float-To-Int-Trap) §6.4", "3.12:18"],
    prog := Examples.scalarProg (.int .w32 .signed) Examples.floatToIntTrapInf },
  { name := "float_to_int_trap_range",
    description := "@float_to_int of 1000.0 at i8 traps: the truncation leaves the target's range.",
    rules := ["(D-Float-To-Int-Trap) §6.4", "3.12:18"],
    prog := Examples.scalarProg (.int .w8 .signed) Examples.floatToIntTrapRange },
  { name := "int_to_float_rounds",
    description := "@int_to_float rounds: 2^53 + 1 has no f64, so it lands on 2^53.",
    rules := ["(Int-To-Float) §5.8", "(D-Int-To-Float) §6.4", "3.12:16"],
    prog := Examples.scalarProg Examples.tF64 Examples.intToFloatRounds },
  { name := "float_cast_narrow",
    description := "@float_cast to f32 rounds, and the f32 prints the digits that identify it as an f32.",
    rules := ["(Float-Cast) §5.8", "(D-Float-Cast) §6.4", "3.12:19", "3.12:40"],
    prog := Examples.scalarProg Examples.tF32 Examples.floatCastNarrow },
  { name := "float_cast_widen",
    description := "@float_cast to f64 is exact, so the f64 rendering shows the whole of the f32 value.",
    rules := ["(Float-Cast) §5.8", "(D-Float-Cast) §6.4", "3.12:19"],
    prog := Examples.scalarProg Examples.tF64 Examples.floatCastWiden },
  { name := "float_round_halfway",
    description := "@round rounds ties away from zero, where rnd_w rounds ties to even; @floor, @ceil and @trunc on the same magnitude.",
    rules := ["(Float-Round) §5.8", "(D-Float-Round) §6.4", "3.12:36"],
    prog := Examples.scalarProg Examples.tI64 Examples.floatRoundHalfway },
  { name := "float_sqrt",
    description := "@sqrt is correctly rounded, so the square root of two prints every digit that identifies it.",
    rules := ["(Float-Round) §5.8", "(D-Float-Round) §6.4", "3.12:35"],
    prog := Examples.scalarProg Examples.tF64 Examples.floatSqrt },
  { name := "total_cmp_order",
    description := "@total_cmp is a total order: -0.0 precedes +0.0, a datum equals itself, a larger value follows.",
    rules := ["(Total-Cmp) §5.8", "(D-Total-Cmp) §6.4", "3.12:31", "3.12:32"],
    prog := Examples.scalarProg Examples.tI64 Examples.totalCmpOrder },
  { name := "float_dbg_layouts",
    description := "3.12:41's two layouts and the boundary between them: 1e15 positional, 1e16 scientific, 1e-6 just past the low end.",
    rules := ["(Dbg) §5.8", "3.12:39", "3.12:40", "3.12:41"],
    prog := Examples.scalarProg Examples.tI64 Examples.floatDbgLayouts },
  { name := "f32_shortest_roundtrip",
    description := "An f32 prints the digits that identify it as an f32, not the digits of the f64 with the same value.",
    rules := ["(Dbg) §5.8", "3.12:40"],
    prog := Examples.scalarProg Examples.tI64 Examples.f32Shortest },
  { name := "array_copy_reads",
    description := "An array literal and the repeat form at a Copy element type, read at three constant indices: 1 + 3 + 7.",
    rules := ["(Array-Intro) §5.8", "(Use-Copy) §5.1", "(D-Array) §6.5", "7.1:38"],
    prog := Examples.prog Examples.tI64 Examples.arrayCopyReads },
  { name := "array_drop_order",
    description := "An array of destructor-bearing elements left to scope exit: §6.11 drops the elements in ascending index order, so the destructors print 1, 2, 3 after the @dbg.",
    rules := ["(Array-Intro) §5.8", "§6.11", "3.9:15", "3.8:73"],
    prog := Examples.prog Examples.tI64 Examples.arrayAffineDropOrder },
  { name := "array_elem_overwrite",
    description := "A write at a constant index over a live affine element: §6.8's overwrite-drop runs the old element's destructor at the assignment, and the scope exit then drops the new element and the untouched one, ascending.",
    rules := ["(Assign) §5.2", "§6.8 overwrite-drop", "§6.11", "3.8:55"],
    prog := Examples.prog Examples.tI64 Examples.arrayElemOverwrite },
  { name := "array_whole_drop",
    description := "@drop of a whole affine array runs the same walk scope exit would — the elements ascending — and leaves a hole the scope exit skips.",
    rules := ["(@Drop) §5.3", "§6.11", "3.9:15"],
    prog := Examples.prog Examples.tI64 Examples.arrayWholeDrop },
  { name := "array_bounds_trap",
    description := "A dynamic-index read in bounds and then past the end: the bounds trap of (D-Index-Trap) §6.5 is a defined outcome, and §6.12 keeps the output the run produced before it, so the 20 prints and the process then exits 101.",
    rules := ["(Use-Untrackable-Dynamic-Copy) §5.1", "(D-Index) §6.5", "(D-Index-Trap) §6.5", "§6.12", "7.1:10"],
    prog := Examples.arrayBoundsTrap },
  { name := "array_in_struct",
    description := "An array held as a struct field, read through a projection, an index and a projection again: h.a[1].x0 + h.a[0].x1 = 5.",
    rules := ["(Struct-Intro) §5.8", "(Array-Intro) §5.8", "(Use-Copy) §5.1", "3.8:53", "7.1:9"],
    prog := Examples.arrHolderProg Examples.tI64 Examples.arrayInStruct },
  { name := "array_dyn_write_affine",
    description := "A write at a dynamic index over a live affine element: the destination of an assignment is not a use, so (Assign)'s own linear-overwrite premise admits it, and §6.8's overwrite-drop runs the old element's destructor at the assignment — 1, then the scope exit's 9 and 2, then 7.",
    rules := ["(Assign) §5.2", "(D-Assign) §6.8", "§6.11", "3.8:77"],
    prog := Examples.arrayDynWriteAffine },
  { name := "array_dyn_write_trap",
    description := "A write at a dynamic index, then the same write at -1: a negative index is out of range exactly as an oversized one is, so the second call takes (D-Index-Trap) §6.5's bounds trap and the process exits 101 after printing the 10.",
    rules := ["(Assign) §5.2", "(D-Assign) §6.8", "(D-Index-Trap) §6.5", "§6.12", "7.1:11", "4.11:9"],
    prog := Examples.arrayDynWriteTrap }
]

/-! ## Witnesses for the refusals no `Examples.lean` program reaches -/

example : run exportOps (Examples.prog Examples.tI64 (letIn true (Examples.resL (Examples.lit 1))
    (seq (assign (.var 0) (Examples.resL (Examples.lit 2)))
      (seq (drop (.var 0)) (Examples.lit 0))))) exportFuel
    = .stuck .linearOverwrite := by rfl
example : run exportOps (Examples.prog Examples.tI64 (seq (Examples.resL (Examples.lit 3)) (Examples.lit 4))) exportFuel
    = .stuck .linearDiscard := by rfl
example : checkProgram (Examples.prog Examples.tI64 (seq (Examples.resL (Examples.lit 3)) (Examples.lit 4)))
    = false := by rfl

/-- §6.3's residue monitor, as a refusal: a destructure whose residue holds a
live declared-`linear` value is `linearLeak` rather than a silent drop
(`3.8:60`, E0474). §5.1's `¬ linear-residue(S, π_s)` premise is what makes it
unreachable for a program the checker accepts. -/
example : run exportOps (Examples.destrProg Examples.tI64 Examples.destructureLinearResidue)
    exportFuel = .stuck .linearLeak := by rfl

/-! ## Outcomes, from the mechanization -/

/-- How `@dbg` renders a value (§5.8's (Dbg); the compiler prints an integer
as its decimal and a `bool` as `true`/`false`, one line each). The calculus
fixes no rendering, so this is the compiler's, verified by hand and compared
by the bridge. A float's text is `3.12:40`–`3.12:42`'s — the shortest decimal
that round-trips at its own width, or `NaN`/`inf`/`-inf`/`-0.0`
(`FloatDatum.render`, `Float.lean`); the compiler reaches the same text
through the vendored `zmij` formatter. A type `@dbg` does not accept has no line, which the statics
exclude (`Ty.observable`). -/
def dbgLine : Val → Option String
  | .int _ _ n => some (toString n)
  | .float w f => some (f.render w)
  | .bool b => some (if b then "true" else "false")
  | .unit | .struct _ _ | .enum _ _ _ | .array _ _ => none

/-- The line a user destructor prints (`Print.structItem`): the struct's
first field, when that field is an integer. A declaration whose first field is
not an integer has nothing to print, in the printed Rue program and here
alike — and neither has one whose first field has been moved out, which
`3.9:34` makes unreachable anyway (a destructor-bearing value has no partial
moves). -/
def dtorLine : Contents → Option String
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
scalar prints itself, `()` prints nothing, and a struct or enum value is dropped
— so its lines are the ones its own drop emits, in §6.11's order, which for an
enum is its **active** variant's payload's (`6.3:20`). An `error` is a struct
naming a declaration the program does not have, which the verdict already
rejects. -/
def valueLines (D : Decls) (v : Val) : List String :=
  match v with
  | .int w s n => (dbgLine (.int w s n)).toList
  | .float w f => (dbgLine (.float w f)).toList
  | .bool b => (dbgLine (.bool b)).toList
  | .unit => []
  -- A struct, an enum or an array value: `main` lets it drop, so its lines are
  -- the ones its own drop emits — a struct's fields in declaration order
  -- (`3.9:13`), an enum's active payload (`6.3:20`), an array's elements in
  -- ascending index order (`3.9:15`).
  | .struct _ _ | .enum _ _ _ | .array _ _ =>
      match dropContents D (Contents.ofVal v) with
      | .ok evs => evs.filterMap eventLine
      | .error _ => []

def panicName : PanicKind → String
  | .overflow => "overflow"
  | .divZero => "divZero"
  | .remZero => "remZero"
  | .castOverflow => "castOverflow"
  | .bounds => "bounds"
  | .user => "user"

def violationName : Violation → String
  | .useAfterMove => "useAfterMove"
  | .useAfterDrop => "useAfterDrop"
  | .linearLeak => "linearLeak"
  | .linearOverwrite => "linearOverwrite"
  | .linearDiscard => "linearDiscard"
  | .unbound => "unbound"
  | .typeConfusion => "typeConfusion"

/-- The stdout the bridge compares, for a completed run: one line per
observable event the run executed — a user destructor or a `@dbg`
(`eventLine`) — in trace order, then the lines `main` shows for the program's
value. A trapping run has no value line, so the panic arms of
`outcomeSummary` and `expectedJson` project the trace alone rather than
calling this. -/
def outLines (D : Decls) (v : Val) (tr : List Event) : List String :=
  tr.filterMap eventLine ++ valueLines D v

/-- **The residual drop is path-specific** (`3.8:60`). The §5.5 join marks a
field `MovedOut` because one arm moved it; the machine keeps the path-specific
state, so the arm that did *not* move it still drops it at scope exit. The two
runs therefore print the same lines, which is what a conservatively joined Σ
costs at the observable level: nothing. -/
example :
    (match run exportOps (Examples.prog Examples.tI64 Examples.partialMoveOneArm) exportFuel with
     | .ok _ v tr => outLines (Decls.ofStructs Examples.structEnv) v tr | _ => [])
    = (match run exportOps (Examples.prog Examples.tI64 Examples.partialMoveOtherArm) exportFuel with
       | .ok _ v tr => outLines (Decls.ofStructs Examples.structEnv) v tr | _ => []) := by rfl

/-- A one-line reading of the outcome, for the program's header comment. -/
def outcomeSummary (c : Case) : String :=
  match checkProgram c.prog, run exportOps c.prog exportFuel with
  | false, .stuck w => "rejected by the checker; the machine would refuse with " ++ violationName w
  | false, .ok _ v tr =>
      let lines := outLines c.prog.decls v tr
      "rejected by the checker; the machine reaches no refusal on the executed path, which prints " ++
        (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++ "; exit 0"
  | false, .panic k tr =>
      let lines := tr.filterMap eventLine
      "rejected by the checker; the machine reaches no refusal on the executed path, which prints " ++
        (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++
        " and then traps with " ++ panicName k
  | true, .ok _ v tr =>
      let lines := outLines c.prog.decls v tr
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
  match run exportOps c.prog exportFuel with
  | .ok _ v tr =>
      "{\"kind\": \"ok\", \"stdout\": " ++ jsonArray ((outLines c.prog.decls v tr).map jsonString) ++
        ", \"exit\": 0}"
  | .returned _ v tr =>
      "{\"kind\": \"ok\", \"stdout\": " ++ jsonArray ((outLines c.prog.decls v tr).map jsonString) ++
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
  match run exportOps c.prog exportFuel with
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
