import RueCore.Checker

/-!
# RueCore.Examples — executable demos (the oracle correspondence, in miniature)

`run` is runnable with `#eval`: the Lean semantics is an executable artifact,
exactly as `docs/formal/README.md` demands of the oracle. These examples show
the machine's observable outcomes (value + drop trace + panics + refusals) and
double as seed vectors for a future Lean↔`rue-oracle` differential harness.

A program is a list of function definitions and `run P fuel` calls index `0`
with no arguments (§6.12's top-level result), so a one-expression example is
`Program.entry T e`: `main`, declared to return `T`, with `e` as its body.
`demoFuel` is the bound every `#eval` here uses; `fuel_mono` (`Soundness.lean`)
is why any sufficient bound gives the same answer.

The declarations here are programs, not rules (`xref: examples` for
`scripts/validate-lean-xref-index.py`).
-/

namespace RueCore.Examples

open Expr

/-- The fuel every demo runs at: far more than the deepest of them spends. -/
def demoFuel : Nat := 200

/-- `let x = 2 + 3; x + x` — well-typed scalar flow (no `*` in the
fragment; use `+`). -/
def scalars : Expr :=
  letIn false (add (intLit 2) (intLit 3)) (add (use 0) (use 0))

/-- An affine resource silently dropped at scope exit — legal, and the trace
shows the drop. `let r = res(7); 1` -/
def affineDrop : Expr :=
  letIn false (mkres .affine (intLit 7)) (intLit 1)

/-- A linear resource, consumed exactly once — legal. -/
def linearConsumed : Expr :=
  letIn false (mkres .linear (intLit 7)) (consume (use 0))

/-- A linear resource leaked at scope exit — the machine REFUSES
(`linearLeak`), and no typing derivation exists for it. -/
def linearLeaked : Expr :=
  letIn false (mkres .linear (intLit 7)) (intLit 1)

/-- Use after move: `let r = res(1); let s = r; consume(r)` — refused
dynamically, rejected statically. -/
def useAfterMove : Expr :=
  letIn false (mkres .affine (intLit 1))
    (letIn false (use 0) (consume (use 1)))

/-- Reinitialization: move out, assign back in, consume — legal (`3.8:55`). -/
def reinit : Expr :=
  letIn true (mkres .linear (intLit 1))
    (seq (consume (use 0))
      (seq (assign 0 (mkres .linear (intLit 2)))
        (consume (use 0))))

/-- Branch join: consume a linear value in only one arm — no typing
derivation exists (the §5.5 join rejects it); dynamically it leaks on the
`false` path. -/
def linearHalfConsumed : Expr :=
  letIn false (mkres .linear (intLit 9))
    (seq (ite (boolLit false) (consume (use 0)) (intLit 0))
      (intLit 0))

/-- Overflow trap (§6.4): `intMax + 1` panics. -/
def overflow : Expr :=
  add (intLit intMax) (intLit 1)

/-- Division by zero panics. -/
def divZero : Expr :=
  div (intLit 1) (intLit 0)

/-! ## Calls, frames, and `return` (RUE-2233)

Each of these needs more than one function, so it is written as a whole
`Program` rather than an `Expr`. Index `0` is the entry point. -/

/-- A plain call: `f0()` calls `f1(2, 3)`, which adds its parameters. The
first parameter is the outermost binder, so it is `use 1` inside the body. -/
def callPlain : Program :=
  [{ params := [], ret := .int, body := call 1 [intLit 2, intLit 3] },
   { params := [⟨.int, false⟩, ⟨.int, false⟩], ret := .int,
     body := add (use 1) (use 0) }]

/-- An early `return` past two live affine bindings: the frame unwinds
newest-first (§6.9's (D-Return)), so the trace is `4` then `3`, then the
value `7`. -/
def returnPastAffine : Program :=
  Program.entry .int
    (letIn false (mkres .affine (intLit 3))
      (letIn false (mkres .affine (intLit 4))
        (ret (intLit 7))))

/-- An early `return` past a live **linear** binding: §5.6's obligation is
undischarged at the `⊥_exit` edge, so (Return-Value) §5.7 rejects it, and the
unwind refuses with `linearLeak`. -/
def returnPastLinear : Program :=
  Program.entry .int
    (letIn false (mkres .linear (intLit 5)) (ret (intLit 1)))

/-- A by-value affine argument the callee never consumes: the callee's frame
owes its drop, and (D-Return-Value)'s `run-all-scope-drops` runs it at the
frame pop — `2`, then the value `1`. -/
def paramDroppedAtPop : Program :=
  [{ params := [], ret := .int, body := call 1 [mkres .affine (intLit 2)] },
   { params := [⟨.res .affine, false⟩], ret := .int, body := intLit 1 }]

/-- A by-value **linear** parameter the callee never consumes: (Fn) §5.8's
second clause rejects the callee (`3.8:62`), and the frame pop refuses with
`linearLeak`. -/
def linearParamLeaked : Program :=
  [{ params := [], ret := .int, body := call 1 [mkres .linear (intLit 5)] },
   { params := [⟨.res .linear, false⟩], ret := .int, body := intLit 1 }]

/-- Recursion to a trap: `f1(3)` counts down and divides by zero at the
bottom, four frames deep. -/
def recursionTrap : Program :=
  [{ params := [], ret := .int, body := call 1 [intLit 3] },
   { params := [⟨.int, false⟩], ret := .int,
     body := ite (lt (use 0) (intLit 1))
       (div (intLit 1) (intLit 0))
       (call 1 [add (use 0) (intLit (-1))]) }]

/-- A recursive countdown: `4 + 3 + 2 + 1 + 0 = 10`. -/
def countdown : Program :=
  [{ params := [], ret := .int, body := call 1 [intLit 4] },
   { params := [⟨.int, false⟩], ret := .int,
     body := ite (lt (use 0) (intLit 1))
       (intLit 0)
       (add (use 0) (call 1 [add (use 0) (intLit (-1))])) }]

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

`f1`'s first parameter is linear and its body consumes it, so (Fn) §5.8 is
satisfied. `main` moves its linear binding into the first argument and then
diverges in the second. (Return-Value) §5.7's frame-wide residual-linear
premise holds at the `return`, because the move already marked the *context*
`MovedOut` — the obligation has migrated to a value the context does not name.
`checkProgram` accepts, so `checkProgram_sound`, `run_safe`, `no_violation`
and `no_linear_leak` all apply to it, and the run destroys `res linear 7` with
an empty drop trace. -/
def linearLostAtCallArg : Program :=
  [{ params := [], ret := .int,
     body := letIn false (mkres .linear (intLit 7))
       (call 1 [use 0, ret (intLit 0)]) },
   { params := [⟨.res .linear, false⟩, ⟨.int, false⟩], ret := .int,
     body := add (consume (use 1)) (use 0) }]

/-- The affine twin, where the same loss is *observable*: an affine value's
drop is the trace event the printed program turns into a destructor's output
line, and here there is none. The Rue compiler agrees — the `RAffine`
destructor does not run — which is why no bridge case could catch this and why
none is added. -/
def affineLostAtCallArg : Program :=
  [{ params := [], ret := .int,
     body := letIn false (mkres .affine (intLit 7))
       (call 1 [use 0, ret (intLit 0)]) },
   { params := [⟨.res .affine, false⟩, ⟨.int, false⟩], ret := .int,
     body := add (consume (use 1)) (use 0) }]

#eval run (Program.entry .int scalars) demoFuel            -- ok: 10, trace: []
#eval run (Program.entry .int affineDrop) demoFuel         -- ok: 1, drop (res affine 7)
#eval run (Program.entry .int linearConsumed) demoFuel     -- ok: 7, trace: []
#eval run (Program.entry .int linearLeaked) demoFuel       -- STUCK: linearLeak
#eval run (Program.entry .int useAfterMove) demoFuel       -- STUCK: useAfterMove
#eval run (Program.entry .int reinit) demoFuel             -- ok: 2, trace: []
#eval run (Program.entry .int linearHalfConsumed) demoFuel -- STUCK: linearLeak
#eval run (Program.entry .int overflow) demoFuel           -- panic: overflow
#eval run (Program.entry .int divZero) demoFuel            -- panic: divZero
#eval run callPlain demoFuel                               -- ok: 5
#eval run returnPastAffine demoFuel                        -- ok: 7, drops 4 then 3
#eval run returnPastLinear demoFuel                        -- STUCK: linearLeak
#eval run paramDroppedAtPop demoFuel                       -- ok: 1, drop (res affine 2)
#eval run linearParamLeaked demoFuel                       -- STUCK: linearLeak
#eval run recursionTrap demoFuel                           -- panic: divZero
#eval run countdown demoFuel                               -- ok: 10
#eval run countdown 12                                     -- outOfFuel
#eval run linearLostAtCallArg demoFuel                     -- ok: 0, EMPTY trace
#eval run affineLostAtCallArg demoFuel                     -- ok: 0, EMPTY trace

/-!
## Static acceptance and rejection, mechanically

The well-typed examples are accepted by the verified checker — so the §7
theorems apply to them; the violating ones are rejected by the same checker
that `checkProgram_sound` ties to the judgment. `rfl`/`decide` makes these
kernel-checked facts, not test assertions.
-/

example : ProgramTyped (Program.entry .int scalars) := checkProgram_sound (by rfl)
example : ProgramTyped (Program.entry .int affineDrop) := checkProgram_sound (by rfl)
example : ProgramTyped (Program.entry .int linearConsumed) := checkProgram_sound (by rfl)
example : ProgramTyped (Program.entry .int reinit) := checkProgram_sound (by rfl)
example : ProgramTyped (Program.entry .int overflow) := checkProgram_sound (by rfl)
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
`dropTemp`, and no `Violation`. `no_linear_leak` holds of this program and
says nothing about it. -/
example : run linearLostAtCallArg demoFuel = .ok [.dead] (.int 0) [] := by rfl

/-- The affine value likewise: the trace a destructor would have printed is
empty. -/
example : run affineLostAtCallArg demoFuel = .ok [.dead] (.int 0) [] := by rfl

example : checkProgram (Program.entry .int linearLeaked) = false := by rfl
example : checkProgram (Program.entry .int useAfterMove) = false := by rfl
example : checkProgram (Program.entry .int linearHalfConsumed) = false := by rfl
example : checkProgram returnPastLinear = false := by rfl
example : checkProgram linearParamLeaked = false := by rfl

/-!
## Refusals and traps, kernel-checked

In interpreter form a violation is a positive result, so `soundness` is only
as strong as `eval`'s refusal enumeration. These witnesses pin every refusal
and trap to a program, or an open machine state, that reaches it, checked
by the kernel rather than observed by `#eval` (ADR-0097; the bridge cannot
observe refusals, because the compiler rejects those programs first).
-/

example : run (Program.entry .int linearLeaked) demoFuel = .stuck .linearLeak := by rfl
example : run (Program.entry .int useAfterMove) demoFuel = .stuck .useAfterMove := by rfl
example : run (Program.entry .int linearHalfConsumed) demoFuel = .stuck .linearLeak := by rfl
example : run (Program.entry .int overflow) demoFuel = .panic .overflow := by rfl
example : run (Program.entry .int divZero) demoFuel = .panic .divZero := by rfl
example : run (Program.entry .int (use 0)) demoFuel = .stuck .unbound := by rfl
example : run (Program.entry .int (add (boolLit true) (intLit 1))) demoFuel
    = .stuck .typeConfusion := by rfl
example : run returnPastLinear demoFuel = .stuck .linearLeak := by rfl
example : run linearParamLeaked demoFuel = .stuck .linearLeak := by rfl

/-- The unwind order, pinned: an early `return` past two live affine bindings
drops the newer one first (§6.9's (D-Return); `3.9:18`). -/
example : run returnPastAffine demoFuel
    = .ok [.dead, .dead] (.int 7)
        [.drop 1 (.res .affine 4), .drop 0 (.res .affine 3)] := by rfl

/-- A by-value parameter the callee never consumes is dropped at the frame
pop ((D-Return-Value) §6.9), not at the caller. -/
example : run paramDroppedAtPop demoFuel
    = .ok [.dead] (.int 1) [.drop 0 (.res .affine 2)] := by rfl

/-- A call whose argument count does not match the callee's parameter list is
`typeConfusion` (§5.8, `4.10:3`); no well-typed program reaches it. -/
example : run [{ params := [], ret := .int, body := call 1 [] },
               { params := [⟨.int, false⟩], ret := .int, body := intLit 0 }] demoFuel
    = .stuck .typeConfusion := by rfl

/-- A call of a function the program does not have is `unbound`; elaboration
resolves every name before the core (§2). -/
example : run (Program.entry .int (call 7 [])) demoFuel = .stuck .unbound := by rfl

/-! ## Fuel, as an outcome

`outOfFuel` is not a machine state: it is the interpreter saying it stopped
early. `fuel_mono` says that raising the bound never changes an answer, and
`no_masking` that no bound turns a violation into exhaustion — so the two
lines below are a bound that is too small and the same program at a bound
that is not, with the same answer at every larger bound. -/

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
is a value here, while §6's integers are bounded and `check` rejects it.
-/

example : run (Program.entry .int (add (boolLit true) (div (intLit 1) (intLit 0)))) demoFuel
    = .stuck .typeConfusion := by rfl
example : run (Program.entry .int (intLit (2 ^ 64))) demoFuel
    = .ok [] (.int (2 ^ 64)) [] := by rfl
example : checkProgram (Program.entry .int (intLit (2 ^ 64))) = false := by rfl

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

example : eval demoFuel [] [.dead] { env := [0], scope := [] } (use 0)
    = .stuck .useAfterDrop := by rfl
example : eval demoFuel [] [.dead] { env := [0], scope := [] } (drop 0)
    = .stuck .useAfterDrop := by rfl
example : eval demoFuel [] [.dead] { env := [0], scope := [] } (assign 0 (intLit 1))
    = .stuck .useAfterDrop := by rfl

/-- The same guard on the unwind path: a frame whose scope record names a
retired cell refuses instead of retiring it twice (§6.9). `FrameMatches` is
what excludes this state for a well-typed program. -/
example : eval demoFuel [] [.dead] { env := [0], scope := [0] } (ret (intLit 1))
    = .stuck .useAfterDrop := by rfl

#eval checkProgram (Program.entry .int scalars)
#eval checkProgram (Program.entry .int linearLeaked)

end RueCore.Examples
