module

public import RueCore.Soundness.Defs

@[expose] public section

/-!
# RueCore.Spec.Safety — type safety over the interpreter (Spec layer)

The statements of §7's type-safety bullet and its decomposed memory-safety
bullets, as the mechanization proves them: over `eval`, the definitional
interpreter (ADR-0097 decision 3), at every fuel. `Spec.lean` lists every
statement of the Spec layer and the theorem that proves it; `Spine.lean` is
where the kernel checks each proof against its statement.

Each `…_stmt` is a `Prop` written over the definitions of layers L0 and L1
alone. Its doc-comment gives the English reading and the calculus paragraph
it realizes, and says where the reading is narrower than the paragraph.
-/

namespace RueCore.Spec

/-- **Type safety over `eval`** (§7 "Type safety", in the interpreter form it
names). A typed expression of a well-formed program, run at any fuel from a
frame and store agreeing with its context, ends in `EvalOk`: a well-typed
value, an unwinding `return` or `break` §5.3's `Ω` allows, a defined panic,
or exhausted fuel — never `.stuck`. -/
def soundness_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : WfProgram P) (fuel : Nat) {R : Ty} {Γ : Ctx}
    {Ω : Out} {e : Expr} {T : Ty}, Typed P R Γ e T Ω →
      ∀ {φ : Frame} {H : Store}, FrameMatches P.decls Γ φ H →
        EvalOk P.decls T R Ω.norm Ω.brk φ H (eval M.toFloatOps fuel P H φ e)

/-- **Program safety** (§7 "Type safety"). A well-formed program whose entry
point (`P.fns[0]?`) takes no parameters, run at any fuel, exhausts it,
panics, or returns a value of its entry point's type. -/
def run_safe_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} {fd : FnDef} (_ : WfProgram P)
    (_ : P.fns[0]? = some fd) (_ : fd.params = []) (fuel : Nat),
    run M.toFloatOps P fuel = .outOfFuel ∨ (∃ k tr, run M.toFloatOps P fuel = .panic k tr) ∨
      (∃ H v tr, run M.toFloatOps P fuel = .ok H v tr ∧ HasTy P.decls v fd.ret)

/-- **No refusal of any kind** (§7's memory-safety bullets). A checked
program's run is never `.stuck`. Narrower than the bullets: a value built for
a sibling operand that a later one abandons by `return` or `break` is dropped
by nobody (RUE-2316), and a `@panic` runs no drop (§5.7's `⊥_panic`). Like
every "never `.stuck`" statement, it holds because `eval`'s checks and
monitors never fire: what it rules out is what they watch (R3 of
`REDTEAM-LOG.md`; RUE-2469). -/
def no_violation_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P) (fuel : Nat) (w : Violation),
    run M.toFloatOps P fuel ≠ .stuck w

/-- **No use-after-move** (§7 "No use-after-move"): `run` never refuses with
`useAfterMove`, the tag `eval` raises when it reads a `⊘`. It is
`no_violation` at one tag, so it says no read of a moved-out place happens
only as far as `eval` checks every read and labels it so: what it rules out is
what that monitor watches (R3 of `REDTEAM-LOG.md`; RUE-2469). -/
def no_use_after_move_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P) (fuel : Nat),
    run M.toFloatOps P fuel ≠ .stuck .useAfterMove

/-- **No use-after-drop** (§7 "No use-after-drop / no leak of drops", "never
read afterward"): `run` never refuses with `useAfterDrop`, the tag `eval`
raises when it reaches a retired cell. It is `no_violation` at one tag, so it
says no retired cell is accessed only as far as `eval` checks every access
and labels it so: what it rules out is what that monitor watches (R3 of
`REDTEAM-LOG.md`; RUE-2469). The buffer half of the bullet, use-after-free,
has no statement (§6.13 is outside the fragment). -/
def no_use_after_drop_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P) (fuel : Nat),
    run M.toFloatOps P fuel ≠ .stuck .useAfterDrop

/-- **No linear leak** (§7 "Linear values are consumed exactly once", §5.6): no
scope exit or unwind meets a live linear value. -/
def no_linear_leak_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P) (fuel : Nat),
    run M.toFloatOps P fuel ≠ .stuck .linearLeak

/-- **No linear overwrite** (§7, the same bullet, `3.8:77`): no assignment drops
a live linear value. -/
def no_linear_overwrite_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P) (fuel : Nat),
    run M.toFloatOps P fuel ≠ .stuck .linearOverwrite

/-- **No linear discard** (§7, the same bullet, `3.8:64`): no sequence discards
a linear value. The three linear statements hold because `eval`'s monitors
never fire; what they rule out is what those monitors watch (R3 of
`REDTEAM-LOG.md`). -/
def no_linear_discard_stmt : Prop :=
  ∀ (M : FloatModel) {P : Program} (_ : ProgramTyped P) (fuel : Nat),
    run M.toFloatOps P fuel ≠ .stuck .linearDiscard

/-- **Fuel monotonicity** (§6 as `eval` runs it; `03-metatheory.md` "Fuel").
An answer other than `outOfFuel` is the answer at every larger fuel. -/
def fuel_mono_stmt : Prop :=
  ∀ (M : FloatOps) {P : Program} {H : Store} {φ : Frame} {e : Expr},
    ∀ {n m : Nat}, n ≤ m → eval M n P H φ e ≠ .outOfFuel →
      eval M m P H φ e = eval M n P H φ e

/-- **No masking** (§6 as `eval` runs it; `03-metatheory.md` "Fuel"). A
refusal at one fuel is the answer at every fuel that answers. -/
def no_masking_stmt : Prop :=
  ∀ (M : FloatOps) {P : Program} {H : Store} {φ : Frame} {e : Expr} {n m : Nat}
    {w : Violation} (_ : eval M n P H φ e = .stuck w) (_ : eval M m P H φ e ≠ .outOfFuel),
    eval M m P H φ e = .stuck w

/-- **No outcome is an unwinding `return`** ((D-Return-Main) §6.9). -/
def run_ne_returned_stmt : Prop :=
  ∀ (M : FloatOps) {P : Program} {fuel : Nat}, ∀ H v tr, run M P fuel ≠ .returned H v tr

end RueCore.Spec
