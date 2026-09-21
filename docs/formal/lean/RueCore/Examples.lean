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

/-! ## The fixture struct declarations -/

/-- `S0`: `@copy struct { x0: i64 }`. Class `Copy`; a `@copy` type declares no
destructor, so its drops are silent and its field is readable. -/
def dCopy : StructDecl := { attr := .copy, fields := [.int], dtor := false, cls := .copy }

/-- `S1`: `struct { x0: i64 }` with a destructor. Class `Affine`, and the
destructor is what makes each of its drops observable. -/
def dAffine : StructDecl := { attr := .none, fields := [.int], dtor := true, cls := .affine }

/-- `S2`: `linear struct { x0: i64 }`, no destructor. Class `Linear`; its
field can be read out, so it is the linear type the fragment's whole-value
elimination works on, and its drops are silent. -/
def dLinear : StructDecl := { attr := .linear, fields := [.int], dtor := false, cls := .linear }

/-- `S3`: `linear struct { x0: i64 }` with a destructor. Class `Linear`, drops
observable; nothing may be read out of it (`3.9:34`), so it is discharged by
`@drop` or by a move. -/
def dLinearDtor : StructDecl :=
  { attr := .linear, fields := [.int], dtor := true, cls := .linear }

/-- `S4`: `struct { x0: i64, x1: S3 }`, no attribute and no destructor. Its
class is `Linear` *through a field* — §3's join, `3.8:58`'s infectiousness —
which is the shape the linear-carrying-struct cases are about. -/
def dCarry : StructDecl :=
  { attr := .none, fields := [.int, .struct 3], dtor := false, cls := .linear }

/-- `S5`: `struct { x0: i64, x1: S1 }` with a destructor. Class `Affine`;
dropping it runs its own destructor first and then its fields in declaration
order (§6.11), so it is the nesting case. -/
def dOuter : StructDecl :=
  { attr := .none, fields := [.int, .struct 1], dtor := true, cls := .affine }

/-- `S6`: `@copy struct { x0: i64, x1: i64 }`. Class `Copy`, two fields, so a
use of it copies and the whole-value elimination reads the first. -/
def dPair : StructDecl :=
  { attr := .copy, fields := [.int, .int], dtor := false, cls := .copy }

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

/-- `let x = 2 + 3; x + x` — well-typed scalar flow (no `*` in the
fragment; use `+`). -/
def scalars : Expr :=
  letIn false (add (intLit 2) (intLit 3)) (add (use 0) (use 0))

/-- An affine resource silently dropped at scope exit — legal, and the trace
shows the drop (its destructor). -/
def affineDrop : Expr :=
  letIn false (resA (intLit 7)) (intLit 1)

/-- A linear resource, consumed exactly once — legal. -/
def linearConsumed : Expr :=
  letIn false (resL (intLit 7)) (consume (use 0))

/-- A linear resource leaked at scope exit — the machine REFUSES
(`linearLeak`), and no typing derivation exists for it. -/
def linearLeaked : Expr :=
  letIn false (resL (intLit 7)) (intLit 1)

/-- Use after move: `let r = S1{1}; let s = r; @drop(r)` — refused
dynamically, rejected statically. -/
def useAfterMove : Expr :=
  letIn false (resA (intLit 1))
    (letIn false (use 0) (seq (drop 1) (intLit 0)))

/-- Reinitialization: move out, assign back in, consume — legal (`3.8:55`). -/
def reinit : Expr :=
  letIn true (resL (intLit 1))
    (seq (consume (use 0))
      (seq (assign 0 (resL (intLit 2)))
        (consume (use 0))))

/-- Branch join: consume a linear value in only one arm — no typing
derivation exists (the §5.5 join rejects it); dynamically it leaks on the
`false` path. -/
def linearHalfConsumed : Expr :=
  letIn false (resL (intLit 9))
    (seq (ite (boolLit false) (consume (use 0)) (intLit 0))
      (intLit 0))

/-- Overflow trap (§6.4): `intMax + 1` panics. -/
def overflow : Expr :=
  add (intLit intMax) (intLit 1)

/-- Division by zero panics. -/
def divZero : Expr :=
  div (intLit 1) (intLit 0)

/-! ## Structs with fields (RUE-2230)

Each of these needs more than one field, or a field that is itself a struct,
so they are where §3's join and §6.11's order become visible. -/

/-- A struct that is `Linear` only through a field (`S4`), left to scope exit:
§5.6's obligation is undischarged, so the machine refuses. -/
def structLinearFieldLeaked : Expr :=
  letIn false (mkStruct sCarry [intLit 1, resLD (intLit 2)]) (intLit 0)

/-- The same value discharged by `@drop` (§5.3's only non-move discharge of a
linear obligation): the whole value's glue runs, so the linear field's
destructor prints. -/
def structLinearFieldDropped : Expr :=
  letIn false (mkStruct sCarry [intLit 1, resLD (intLit 2)]) (seq (drop 0) (intLit 0))

/-- A nested destructor-bearing struct at scope exit: §6.11 runs the outer
destructor first and then the fields in declaration order, so the trace is
`1` then `2`. -/
def structNestedDrop : Expr :=
  letIn false (mkStruct sOuter [intLit 1, resA (intLit 2)]) (intLit 9)

/-- The §5.5 join on a linear-carrying struct entry: discharged in one arm
only, which `3.8:50` makes ill-formed; on the path taken it leaks. -/
def structJoinDisagrees : Expr :=
  letIn false (mkStruct sCarry [intLit 1, resLD (intLit 2)])
    (seq (ite (boolLit false) (drop 0) unitLit) (intLit 0))

/-- A `@copy` struct used twice: contraction is legal at `Copy` (§3), and
nothing is ever dropped. -/
def structCopyTwice : Expr :=
  letIn false (mkStruct sPair [intLit 5, intLit 6])
    (add (consume (use 0)) (consume (use 0)))

/-- Two destructor-bearing fields in one struct with no destructor of its own:
scope exit drops them in **declaration** order (§6.11), so the trace is `1`
then `2`. -/
def structFieldOrder : Expr :=
  letIn false (mkStruct sTwoAffine [resA (intLit 1), resA (intLit 2)]) (intLit 0)

/-! ## Calls, frames, and `return` (RUE-2233)

Each of these needs more than one function, so it is written as a whole
`Program` rather than an `Expr`. Function index `0` is the entry point. -/

/-- A plain call: `f0()` calls `f1(2, 3)`, which adds its parameters. The
first parameter is the outermost binder, so it is `use 1` inside the body. -/
def callPlain : Program :=
  { structs := [],
    fns := [{ params := [], ret := .int, body := call 1 [intLit 2, intLit 3] },
            { params := [⟨.int, false⟩, ⟨.int, false⟩], ret := .int,
              body := add (use 1) (use 0) }] }

/-- An early `return` past two live affine bindings: the frame unwinds
newest-first (§6.9's (D-Return)), so the trace is `4` then `3`, then the
value `7`. -/
def returnPastAffine : Program :=
  prog .int
    (letIn false (resA (intLit 3))
      (letIn false (resA (intLit 4))
        (ret (intLit 7))))

/-- An early `return` past a live **linear** binding: §5.6's obligation is
undischarged at the `⊥_exit` edge, so (Return-Value) §5.7 rejects it, and the
unwind refuses with `linearLeak`. -/
def returnPastLinear : Program :=
  prog .int (letIn false (resL (intLit 5)) (ret (intLit 1)))

/-- A by-value affine argument the callee never consumes: the callee's frame
owes its drop, and (D-Return-Value)'s `run-all-scope-drops` runs it at the
frame pop — `2`, then the value `1`. -/
def paramDroppedAtPop : Program :=
  { structs := structEnv,
    fns := [{ params := [], ret := .int, body := call 1 [resA (intLit 2)] },
            { params := [⟨.struct sAffine, false⟩], ret := .int, body := intLit 1 }] }

/-- A by-value **linear** parameter the callee never consumes: (Fn) §5.8's
second clause rejects the callee (`3.8:62`), and the frame pop refuses with
`linearLeak`. -/
def linearParamLeaked : Program :=
  { structs := structEnv,
    fns := [{ params := [], ret := .int, body := call 1 [resL (intLit 5)] },
            { params := [⟨.struct sLinear, false⟩], ret := .int, body := intLit 1 }] }

/-- Recursion to a trap: `f1(3)` counts down and divides by zero at the
bottom, four frames deep. -/
def recursionTrap : Program :=
  { structs := [],
    fns := [{ params := [], ret := .int, body := call 1 [intLit 3] },
            { params := [⟨.int, false⟩], ret := .int,
              body := ite (lt (use 0) (intLit 1))
                (div (intLit 1) (intLit 0))
                (call 1 [add (use 0) (intLit (-1))]) }] }

/-- A recursive countdown: `4 + 3 + 2 + 1 + 0 = 10`. -/
def countdown : Program :=
  { structs := [],
    fns := [{ params := [], ret := .int, body := call 1 [intLit 4] },
            { params := [⟨.int, false⟩], ret := .int,
              body := ite (lt (use 0) (intLit 1))
                (intLit 0)
                (add (use 0) (call 1 [add (use 0) (intLit (-1))])) }] }

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
    fns := [{ params := [], ret := .int,
              body := letIn false (resL (intLit 7)) (call 1 [use 0, ret (intLit 0)]) },
            { params := [⟨.struct sLinear, false⟩, ⟨.int, false⟩], ret := .int,
              body := add (consume (use 1)) (use 0) }] }

/-- The affine twin, where the same loss is *observable*: `S1` declares a
destructor, so a drop of it is the trace event the printed program turns into
an output line — and here there is none. The callee discharges its parameter
with `@drop`, which would print; the value never reaches the callee. The Rue
compiler agrees — `S1`'s destructor does not run — which is why no bridge case
could catch this and why none is added. -/
def affineLostAtCallArg : Program :=
  { structs := structEnv,
    fns := [{ params := [], ret := .int,
              body := letIn false (resA (intLit 7)) (call 1 [use 0, ret (intLit 0)]) },
            { params := [⟨.struct sAffine, false⟩, ⟨.int, false⟩], ret := .int,
              body := seq (drop 1) (use 0) }] }

#eval run (scalarProg .int scalars) demoFuel            -- ok: 10, trace: []
#eval run (prog .int affineDrop) demoFuel               -- ok: 1, drop + dtor of S1{7}
#eval run (prog .int linearConsumed) demoFuel           -- ok: 7, trace: []
#eval run (prog .int linearLeaked) demoFuel             -- STUCK: linearLeak
#eval run (prog .int useAfterMove) demoFuel             -- STUCK: useAfterMove
#eval run (prog .int reinit) demoFuel                   -- ok: 2, trace: []
#eval run (prog .int linearHalfConsumed) demoFuel       -- STUCK: linearLeak
#eval run (scalarProg .int overflow) demoFuel           -- panic: overflow
#eval run (scalarProg .int divZero) demoFuel            -- panic: divZero
#eval run (prog .int structLinearFieldLeaked) demoFuel  -- STUCK: linearLeak
#eval run (prog .int structLinearFieldDropped) demoFuel -- ok: 0, dtor of S3{2}
#eval run (prog .int structNestedDrop) demoFuel         -- ok: 9, dtors 1 then 2
#eval run (prog .int structJoinDisagrees) demoFuel      -- STUCK: linearLeak
#eval run (prog .int structCopyTwice) demoFuel          -- ok: 10, trace: []
#eval run (prog .int structFieldOrder) demoFuel         -- ok: 0, dtors 1 then 2
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

example : ProgramTyped (scalarProg .int scalars) := checkProgram_sound (by rfl)
example : ProgramTyped (prog .int affineDrop) := checkProgram_sound (by rfl)
example : ProgramTyped (prog .int linearConsumed) := checkProgram_sound (by rfl)
example : ProgramTyped (prog .int reinit) := checkProgram_sound (by rfl)
example : ProgramTyped (scalarProg .int overflow) := checkProgram_sound (by rfl)
example : ProgramTyped (prog .int structLinearFieldDropped) := checkProgram_sound (by rfl)
example : ProgramTyped (prog .int structNestedDrop) := checkProgram_sound (by rfl)
example : ProgramTyped (prog .int structCopyTwice) := checkProgram_sound (by rfl)
example : ProgramTyped (prog .int structFieldOrder) := checkProgram_sound (by rfl)
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
example : run linearLostAtCallArg demoFuel = .ok [.dead] (.int 0) [] := by rfl

/-- The affine value likewise: the destructor line the printed program would
have shown is absent. -/
example : run affineLostAtCallArg demoFuel = .ok [.dead] (.int 0) [] := by rfl

example : checkProgram (prog .int linearLeaked) = false := by rfl
example : checkProgram (prog .int useAfterMove) = false := by rfl
example : checkProgram (prog .int linearHalfConsumed) = false := by rfl
example : checkProgram (prog .int structLinearFieldLeaked) = false := by rfl
example : checkProgram (prog .int structJoinDisagrees) = false := by rfl
example : checkProgram returnPastLinear = false := by rfl
example : checkProgram linearParamLeaked = false := by rfl

/-- A declaration whose recorded class disagrees with §3's join is rejected by
the same pass: `class(S)` is not a free parameter of the syntax. -/
example : checkStructs [{ attr := .none, fields := [.int], dtor := false, cls := .copy }]
    = false := by rfl

/-- `3.8:18` and `3.9:31`: a `@copy` declaration whose field join is not
`Copy`, or which declares a destructor, is ill-formed. -/
example : checkStructs (structEnv ++
    [{ attr := .copy, fields := [.struct 1], dtor := false, cls := .copy }]) = false := by rfl
example : checkStructs [{ attr := .copy, fields := [.int], dtor := true, cls := .copy }]
    = false := by rfl

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

example : run (prog .int linearLeaked) demoFuel = .stuck .linearLeak := by rfl
example : run (prog .int useAfterMove) demoFuel = .stuck .useAfterMove := by rfl
example : run (prog .int linearHalfConsumed) demoFuel = .stuck .linearLeak := by rfl
example : run (prog .int structLinearFieldLeaked) demoFuel = .stuck .linearLeak := by rfl
example : run (prog .int structJoinDisagrees) demoFuel = .stuck .linearLeak := by rfl
example : run (scalarProg .int overflow) demoFuel = .panic .overflow := by rfl
example : run (scalarProg .int divZero) demoFuel = .panic .divZero := by rfl
example : run (scalarProg .int (use 0)) demoFuel = .stuck .unbound := by rfl
example : run (scalarProg .int (add (boolLit true) (intLit 1))) demoFuel
    = .stuck .typeConfusion := by rfl
example : run returnPastLinear demoFuel = .stuck .linearLeak := by rfl
example : run linearParamLeaked demoFuel = .stuck .linearLeak := by rfl

/-- A struct literal with the wrong number of initializers is `typeConfusion`
((Struct-Intro) §5.8's `3.6:5`/`3.6:6`; no well-typed program reaches it). -/
example : run (prog .int (seq (mkStruct sPair [intLit 1]) (intLit 0))) demoFuel
    = .stuck .typeConfusion := by rfl

/-- A struct literal naming a declaration the program does not have is
`unbound`; elaboration resolves every type name before the core (§2). -/
example : run (prog .int (seq (mkStruct 99 []) (intLit 0))) demoFuel
    = .stuck .unbound := by rfl

/-- A call whose argument count does not match the callee's parameter list is
`typeConfusion` (§5.8, `4.10:3`); no well-typed program reaches it. -/
example : run { structs := [],
                fns := [{ params := [], ret := .int, body := call 1 [] },
                        { params := [⟨.int, false⟩], ret := .int, body := intLit 0 }] } demoFuel
    = .stuck .typeConfusion := by rfl

/-- A call of a function the program does not have is `unbound`; elaboration
resolves every name before the core (§2). -/
example : run (scalarProg .int (call 7 [])) demoFuel = .stuck .unbound := by rfl

/-! ## Drop order, pinned

§6.11 fixes the order in which a drop's events come out: a value's own
destructor first, then its fields in declaration order, recursively; and a
frame's teardown reads its scope record newest-first (§6.9). These pin both.
-/

/-- The unwind order: an early `return` past two live affine bindings drops
the newer one first (§6.9's (D-Return); `3.9:18`). -/
example : run returnPastAffine demoFuel
    = .ok [.dead, .dead] (.int 7)
        [.drop 1 (.struct sAffine [.int 4]), .dtor sAffine (.struct sAffine [.int 4]),
         .drop 0 (.struct sAffine [.int 3]), .dtor sAffine (.struct sAffine [.int 3])] := by rfl

/-- §6.11's order inside one value: the outer destructor, then the fields in
declaration order — so the nested destructor runs **after** the outer one. -/
example : run (prog .int structNestedDrop) demoFuel
    = .ok [.dead] (.int 9)
        [.drop 0 (.struct sOuter [.int 1, .struct sAffine [.int 2]]),
         .dtor sOuter (.struct sOuter [.int 1, .struct sAffine [.int 2]]),
         .dtor sAffine (.struct sAffine [.int 2])] := by rfl

/-- Fields drop in declaration order, not in reverse: the struct here has no
destructor of its own, so its trace is exactly its two fields' (§6.11). -/
example : run (prog .int structFieldOrder) demoFuel
    = .ok [.dead] (.int 0)
        [.drop 0 (.struct sTwoAffine [.struct sAffine [.int 1], .struct sAffine [.int 2]]),
         .dtor sAffine (.struct sAffine [.int 1]),
         .dtor sAffine (.struct sAffine [.int 2])] := by rfl

/-- `@drop` of a value that is linear only through a field runs the whole
value's glue: the field's destructor is the one observable event. -/
example : run (prog .int structLinearFieldDropped) demoFuel
    = .ok [.dead] (.int 0)
        [.drop 0 (.struct sCarry [.int 1, .struct sLinearDtor [.int 2]]),
         .dtor sLinearDtor (.struct sLinearDtor [.int 2])] := by rfl

/-- A by-value parameter the callee never consumes is dropped at the frame
pop ((D-Return-Value) §6.9), not at the caller. -/
example : run paramDroppedAtPop demoFuel
    = .ok [.dead] (.int 1)
        [.drop 0 (.struct sAffine [.int 2]), .dtor sAffine (.struct sAffine [.int 2])] := by rfl

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
    run countdown 17 = .ok [.dead, .dead, .dead, .dead, .dead] (.int 10) [] := by rfl

/-- `fuel_mono` in use: every larger bound gives that same answer, so the
∀-fuel shape of `soundness` is a statement about one outcome. -/
example : run countdown demoFuel = run countdown 17 :=
  fuel_mono (by decide) (fun h => absurd (countdown_at_17.symm.trans h) (by simp))

/-! ## Where `eval` and §6 part on invalid input

`eval` is a model of §6 on the programs `check` accepts (`Dynamics.lean`,
"the correspondence with §6"). Off that domain the two can differ, and
these pin the ways they do, so nobody mistakes the machine for the paper
relation on raw `Expr`: an ill-typed left operand is refused before the
right operand runs, where §6.2's `v ⊕ E` context would reduce the right
operand to its division-by-zero panic first; and an out-of-range literal
is a value here, while §6's integer domain is bounded and `check` rejects it.
-/

example : run (scalarProg .int (add (boolLit true) (div (intLit 1) (intLit 0)))) demoFuel
    = .stuck .typeConfusion := by rfl
example : run (scalarProg .int (intLit (2 ^ 64))) demoFuel
    = .ok [] (.int (2 ^ 64)) [] := by rfl
example : checkProgram (scalarProg .int (intLit (2 ^ 64))) = false := by rfl

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

example : eval demoFuel (scalarProg .int unitLit) [.dead] { env := [0], scope := [] } (use 0)
    = .stuck .useAfterDrop := by rfl
example : eval demoFuel (scalarProg .int unitLit) [.dead] { env := [0], scope := [] } (drop 0)
    = .stuck .useAfterDrop := by rfl
example : eval demoFuel (scalarProg .int unitLit) [.dead] { env := [0], scope := [] }
    (assign 0 (intLit 1)) = .stuck .useAfterDrop := by rfl

/-- The same guard on the unwind path: a frame whose scope record names a
retired cell refuses instead of retiring it twice (§6.9). `FrameMatches` is
what excludes this state for a well-typed program. -/
example : eval demoFuel (scalarProg .int unitLit) [.dead] { env := [0], scope := [0] }
    (ret (intLit 1)) = .stuck .useAfterDrop := by rfl

#eval checkProgram (scalarProg .int scalars)
#eval checkProgram (prog .int linearLeaked)

end RueCore.Examples
