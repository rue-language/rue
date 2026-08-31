import RueCore.Checker

/-!
# RueCore.Examples — executable demos (the oracle correspondence, in miniature)

`eval` is runnable with `#eval`: the Lean semantics is an executable artifact,
exactly as `docs/formal/README.md` demands of the oracle. These examples show
the machine's observable outcomes (value + drop trace + panics + refusals) and
double as seed vectors for a future Lean↔`rue-oracle` differential harness.
-/

namespace RueCore.Examples

open Expr

/-- `let x = 2 + 3; x * ... ` — well-typed scalar flow (no `*` in the
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

#eval eval [] [] scalars            -- ok: 10, trace: []
#eval eval [] [] affineDrop         -- ok: 1, trace: [drop ℓ0 (res affine 7)]
#eval eval [] [] linearConsumed     -- ok: 7, trace: []
#eval eval [] [] linearLeaked       -- STUCK: linearLeak (and statically ill-typed)
#eval eval [] [] useAfterMove       -- STUCK: useAfterMove (and statically ill-typed)
#eval eval [] [] reinit             -- ok: 2, trace: []
#eval eval [] [] linearHalfConsumed -- STUCK: linearLeak (join would reject statically)
#eval eval [] [] overflow           -- panic: overflow
#eval eval [] [] divZero            -- panic: divZero

/-!
## Static acceptance and rejection, mechanically

The well-typed examples are accepted by the verified checker — so the §7
theorems apply to them; the violating ones are rejected by the same checker
that `check_sound` ties to the judgment. `rfl`/`decide` makes these
kernel-checked facts, not test assertions.
-/

example : Typed [] scalars .int [] := check_sound (by rfl)
example : Typed [] affineDrop .int [] := check_sound (by rfl)
example : Typed [] linearConsumed .int [] := check_sound (by rfl)
example : Typed [] reinit .int [] := check_sound (by rfl)
example : Typed [] overflow .int [] := check_sound (by rfl)

example : check [] linearLeaked = none := by rfl
example : check [] useAfterMove = none := by rfl
example : check [] linearHalfConsumed = none := by rfl

#eval check [] scalars
#eval check [] linearLeaked

end RueCore.Examples
