module

public import RueCore.Float
public import RueCore.Checker.Defs
public import RueCore.Soundness.Defs
public import RueCore.Trace.Defs
public import RueCore.Adequacy.Defs

@[expose] public section

/-!
# RueCore.Spec.Sharp — the spine's hypotheses are needed (Spec layer)

A witness (`Spec/Nonvacuous.lean`, RUE-2469) shows a statement's hypotheses
satisfiable; it does not show them needed. Each statement here is a
**counter-example** to a spine statement with one hypothesis dropped
(RUE-2485): a program, written out in the statement, of which every other
hypothesis of that spine statement holds, the dropped one fails, and the
conclusion fails. So the hypothesis does work: the statement with it removed
is false. This is the *sharpness* of the statement (ours, pending audit; see
`GLOSSARY.md`).

Most of the programs are unchecked, and several are refused by one of
`eval`'s monitors (`linearLeak`, `linearOverwrite`, `linearDiscard`,
`ownedUnderCopy`), so these statements also pin the monitors: a machine with a
monitor removed makes the matching statement false (R3 of `REDTEAM-LOG.md`;
the five monitor mutants of `MUTATION.md`). Four more (`uncut_drop`,
`ill_typed_halt`, `out_of_range_halt`, `float_halt`; RUE-2500) pin the
definitions the spine is written in the same way: each negates `Lifo`,
`SafeAt`'s typing half, `HasTy` or `FloatDatum.Wf` at a configuration or
datum that fails only that definition, so a weakened definition makes the
statement false (the statement-vocabulary mutants of `MUTATION.md`). The checked programs are the
witnesses' (`Nonvacuous.dtor`, `Nonvacuous.panic`, `Nonvacuous.float`),
over the same declarations, with a few more for a `@copy` struct that declares a destructor.
Every program runs on `Float.exactOps`, a model of the float laws
(`Nonvacuous.exact_model`); the laws themselves are assumptions about the
model, not hypotheses about a program, so they have no counter-example here
(`Spec.sharpnessReasons`, `Spec.lean`).

That a statement refutes each hypothesis `Spec.sharpness` pairs it with is
checked by the kernel (RUE-2495): `RueCore/Sharp/Glue.lean` proves, from the
statement, the negation of the spine statement with that hypothesis removed,
and the lint computes that weakened statement itself (`Lint.dropHyp`) and
requires the glue theorem to state exactly its negation. So each statement
gives the other hypotheses in a form the spine statement applies to (`run`
as `eval` at `main()`, `WfProgram` beside `ProgramTyped`, `eval … =
r.withTrace []`), and its doc-comment names the spot. One thing the kernel
does not show is that the dropped hypothesis fails independently of the
spine. Every negated hypothesis (`¬ ProgramTyped`, `¬ WfProgram`, `¬ Typed`,
`¬ FrameMatches`, `¬ Steps …`) is proved through the spine theorem itself,
from the other hypotheses and the failed conclusion, except where the
doc-comment says it is shown directly. The content of each counter-example is
that the other hypotheses hold and the conclusion fails, and both are
established without the spine theorem.

A second limit: For `drop_order` 2–3, `eval_sound` 2–3, `run_sim` 1–2, `eval_complete` 2 and 4,
and `run_complete` 1 and 3, the dropped premise is the only thing tying its
bound value or trace to the program; once it is gone the weakened statement
claims something of *every* such value, so the counter-example shows only that
the conclusion is not a tautology, not that the premise does the work that
`ProgramTyped` or an `n < fuel` bound does.

`Spec.sharpness` (`Spec.lean`) lists each statement with the theorem that
proves it (`RueCore/Sharp.lean`, layer L2) and the spine hypotheses it refutes
(each pair checked by `RueCore/Sharp/Glue.lean`),
each as a spine theorem and a hypothesis number: the hypotheses of a statement
are its premises of `Prop` type, numbered from 1 in the order they occur,
premises inside the conclusion included (`Lint.hypotheses`). A statement's
doc-comment says which. `SPINE.md` prints the list under each spine statement
as its "Sharp" line; the kernel, the lint, Lean Comparator and the
fingerprints cover these statements as they cover the spine's.
-/

namespace RueCore.Spec.Sharp

/-- **An unchecked program that reads a moved-out value, run by `eval`**
(§7 sharpness, RUE-2485; the program is `Nonvacuous.stuck`'s). `let a = S0 { 1 };
@drop(a); a.x0` as the entry point: the checker rejects it and it is neither
`ProgramTyped` nor `WfProgram`, while its entry point exists and takes no
parameters, and its body is `pendingSafe`; `main()`, the call `run` makes, is
typed by `check` from the empty frame and store, which agree with the empty
context, and has a `Lead` (its empty argument list). `eval` refuses it with
`useAfterMove`, and at fuel `0` it answers `outOfFuel`. So each of these
conclusions fails once its program hypothesis is dropped: `soundness`
(`WfProgram`), `run_safe` (`WfProgram`), `no_violation`, `no_use_after_move`,
`checkProgram_sound` (`checkProgram P = true`), `eval_sound`,
`drop_exactly_once` and `rest_exactly_once` (`ProgramTyped`), and `no_masking`
(its second hypothesis, `eval m ≠ outOfFuel`, at `m = 0`). Where the spine theorem is stated over `eval`
(`no_masking`, `drop_exactly_once`, `rest_exactly_once`), the statement gives
`run P n` and `eval` at `main()` as the same term (`run`'s definition), and
`rest_exactly_once`'s hypothesis 8 as `eval … = r.withTrace []` with `r` the
refusal. The pairing is kernel-checked (`Sharp/Glue.lean`, RUE-2495). The negations `¬ ProgramTyped`
and `¬ WfProgram` are proved through the spine theorems themselves
(`no_use_after_move`, `soundness`), not by inverting the definitions. -/
def stuck_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.seq (.drop (.var 0)) (.use (.proj (.var 0) 0))) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ ¬ WfProgram P ∧
      (∃ fd, P.fns[0]? = some fd ∧ fd.params = []) ∧
      P.pendingSafe = true ∧ (Expr.call 0 []).pendingSafe = true ∧
      FrameMatches P.decls [] Frame.empty [] ∧ StoreCC P.decls [] ∧
      (∃ c Ω, check P (.int .w64 .signed) [] (.call 0 []) = some (c, Ω) ∧ c.fits (.int .w64 .signed) = true ∧
        Typed P (.int .w64 .signed) [] (.call 0 []) (.int .w64 .signed) Ω ∧
        ¬ EvalOk P.decls (.int .w64 .signed) (.int .w64 .signed) Ω.norm Ω.brk Frame.empty []
          (eval Float.exactOps 200 P [] Frame.empty (.call 0 []))) ∧
      Lead Float.exactOps P 200 [] Frame.empty [] [] [] (.call 0 []) ∧
      eval Float.exactOps 200 P [] Frame.empty (.call 0 []) = .stuck .useAfterMove ∧
      eval Float.exactOps 201 P [] Frame.empty (.call 0 []) = .stuck .useAfterMove ∧
      eval Float.exactOps 201 P [] Frame.empty (.call 0 []) =
        (EvalRes.stuck .useAfterMove).withTrace [] ∧
      (∀ n, run Float.exactOps P n = eval Float.exactOps n P [] Frame.empty (.call 0 [])) ∧
      run Float.exactOps P 200 = .stuck .useAfterMove ∧ run Float.exactOps P 0 = .outOfFuel ∧
      ¬ (run Float.exactOps P 200 = .outOfFuel ∨ (∃ k tr, run Float.exactOps P 200 = .panic k tr) ∨
        ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧ HasTy P.decls v (.int .w64 .signed))

/-- **The same program, run by §6's relation** (§7 sharpness, RUE-2485). `Step`
reaches a configuration stuck with `useAfterMove` from `Config.init`, and
`run` refuses at fuel `200` and exhausts fuel `0`. So once `ProgramTyped` is
dropped, `step_progress`, `step_preservation` and `step_type_safety` fail
(no horizon passes the stuck configuration, which is not a value or a
panic); once `step_never_stuck_of_run`'s hypothesis that `run` is never stuck
is dropped, its conclusion fails; and once `run_stuck_of_step_stuck`'s bound
`n < fuel` is dropped, no `n` makes `run` stuck at every fuel. -/
def stuck_step_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.seq (.drop (.var 0)) (.use (.proj (.var 0) 0))) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ¬ ProgramTyped P ∧
      (∃ C, Steps Float.exactOps P Config.init C ∧ C.Stuck Float.exactOps P .useAfterMove) ∧
      run Float.exactOps P 200 = .stuck .useAfterMove ∧ run Float.exactOps P 0 = .outOfFuel ∧
      ¬ (∀ C, Steps Float.exactOps P Config.init C → C.Terminal ∨ ∃ C', Step Float.exactOps P C C') ∧
      ¬ (∃ fd, P.fns[0]? = some fd ∧
        ∀ C, Steps Float.exactOps P Config.init C → C.SafeAt Float.exactOps P fd.ret) ∧
      ¬ (∃ fd, P.fns[0]? = some fd ∧ ∀ n,
        (∃ D, StepsN Float.exactOps P n Config.init D) ∨
        (∃ H v tr, Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
          HasTy P.decls v fd.ret) ∨
        (∃ κ tr, Steps Float.exactOps P Config.init (.panic κ tr))) ∧
      ¬ ∀ fuel, ∃ w', run Float.exactOps P fuel = .stuck w'

/-- **An ill-typed expression of a checked program** (§7 sharpness, RUE-2485).
Over the checked program of `Nonvacuous.dtor`, the expression `let a = S0 { 1
}; @drop(a); a.x0`, from the empty frame and store, is typed at no type and no
outcome, and `check` rejects it; everything else `soundness`,
`drop_exactly_once` and `rest_exactly_once` ask holds, the leading `S0 { 1 }`
included (`Lead`). Its evaluation is refused with `useAfterMove`, so none of
their conclusions holds of it: the typing hypothesis `Typed` is needed. It is
also `check_sound`'s first hypothesis dropped: `check` does not accept it, and
no type fits a derivation. `check_sound`'s hypothesis 2 (`c.fits T = true`) has
no `c` to hold of, since `check` answers `none`: the statement gives
`CTy.never`, which fits every type, and no `Ω` at all; `¬ Typed` is stated
for every type and outcome, so for any `c`, `Ω` a spine instance picks. And
`rest_exactly_once`'s hypothesis 8 is `eval … = r.withTrace []` with `r` the
refusal. The pairing is kernel-checked (`Sharp/Glue.lean`, RUE-2495); `¬ Typed`
is proved through `soundness`. -/
def typed_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ∀ e : Expr, e =
        .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.seq (.drop (.var 0)) (.use (.proj (.var 0) 0))) →
      ProgramTyped P ∧ WfProgram P ∧ P.pendingSafe = true ∧ e.pendingSafe = true ∧
      FrameMatches P.decls [] Frame.empty [] ∧ StoreCC P.decls [] ∧
      check P (.int .w64 .signed) [] e = none ∧ (∀ (T : Ty) (Ω : Out), ¬ Typed P (.int .w64 .signed) [] e T Ω) ∧
      Lead Float.exactOps P 200 [] Frame.empty [.dead] [.struct 0 0 [.int .w64 .signed 1]] [] e ∧
      eval Float.exactOps 200 P [] Frame.empty e = .stuck .useAfterMove ∧
      eval Float.exactOps 201 P [] Frame.empty e = .stuck .useAfterMove ∧
      eval Float.exactOps 201 P [] Frame.empty e = (EvalRes.stuck .useAfterMove).withTrace [] ∧
      CTy.never.fits (.int .w64 .signed) = true ∧
      ∀ (T : Ty) (Ω : Out), ¬ EvalOk P.decls T (.int .w64 .signed) Ω.norm Ω.brk Frame.empty []
        (eval Float.exactOps 200 P [] Frame.empty e)

/-- **A typed expression run in a frame that does not match its context**
(§7 sharpness, RUE-2485). `1; x`, typed by `check` in the context `x : i64` over
the checked program of `Nonvacuous.dtor`, is run from the empty frame and
store, which do not match that context (`FrameMatches` fails); everything else
`soundness`, `drop_exactly_once` and `rest_exactly_once` ask holds, a `Lead`
(the discarded `1`) included. `eval` refuses the read of `x` with `unbound`. The statement gives `ProgramTyped P` and `WfProgram P`
(`soundness` asks the second, `drop_exactly_once` the first), and
`rest_exactly_once`'s hypothesis 8 as `eval … = r.withTrace []` with `r` the
refusal. The pairing is kernel-checked (`Sharp/Glue.lean`, RUE-2495);
`¬ FrameMatches` is proved through `soundness`. -/
def frame_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ∀ e : Expr, e = .seq (.intLit .w64 .signed 1) (.use (.var 0)) →
      ProgramTyped P ∧ WfProgram P ∧ P.pendingSafe = true ∧ e.pendingSafe = true ∧ StoreCC P.decls [] ∧
      (∃ c Ω, check P (.int .w64 .signed) [{ ty := .int .w64 .signed, mu := false, st := .owned }] e = some (c, Ω) ∧
        c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [{ ty := .int .w64 .signed, mu := false, st := .owned }] e (.int .w64 .signed) Ω ∧
        ¬ EvalOk P.decls (.int .w64 .signed) (.int .w64 .signed) Ω.norm Ω.brk Frame.empty []
          (eval Float.exactOps 200 P [] Frame.empty e)) ∧
      ¬ FrameMatches P.decls [{ ty := .int .w64 .signed, mu := false, st := .owned }] Frame.empty [] ∧
      Lead Float.exactOps P 200 [] Frame.empty [] [.int .w64 .signed 1] [] e ∧
      eval Float.exactOps 200 P [] Frame.empty e = .stuck .unbound ∧
      eval Float.exactOps 201 P [] Frame.empty e = .stuck .unbound ∧
      eval Float.exactOps 201 P [] Frame.empty e = (EvalRes.stuck .unbound).withTrace []

/-- **A well-formed program with no entry point** (§7 sharpness, RUE-2485). The
program with the witnesses' declarations and no function is `WfProgram`, and
`P.fns[0]?` is `none`; `run` refuses the call of function `0` with `unbound`,
so for no entry point `fd` does `run_safe`'s conclusion hold. -/
def no_entry_stmt : Prop :=
  ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [] } →
      WfProgram P ∧ P.fns[0]? = none ∧ run Float.exactOps P 200 = .stuck .unbound ∧
      ∀ fd : FnDef, ¬ (run Float.exactOps P 200 = .outOfFuel ∨ (∃ k tr, run Float.exactOps P 200 = .panic k tr) ∨
        ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧ HasTy P.decls v fd.ret)

/-- **A well-formed program whose entry point takes a parameter** (§7 sharpness,
RUE-2485). `fn main(x: i64) -> i64 { x }` is `WfProgram`, but its entry point
has a parameter, so it is not `ProgramTyped`; `run` calls it with no
arguments, and `eval` refuses the call with `typeConfusion`. So `run_safe`
needs its hypothesis `fd.params = []`, and `no_violation` needs the entry
clause of `ProgramTyped`, not only `WfProgram`. `¬ ProgramTyped` is shown
directly, from that clause. -/
def entry_param_stmt : Prop :=
  ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [{ ty := .int .w64 .signed, mu := false }], ret := .int .w64 .signed, body := .use (.var 0) }] } →
      WfProgram P ∧ ¬ ProgramTyped P ∧
      (∃ fd, P.fns[0]? = some fd ∧ fd.params ≠ [] ∧
        ¬ (run Float.exactOps P 200 = .outOfFuel ∨ (∃ k tr, run Float.exactOps P 200 = .panic k tr) ∨
          ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧ HasTy P.decls v fd.ret)) ∧
      run Float.exactOps P 200 = .stuck .typeConfusion

/-- **The copy monitor fires** (R3 of `REDTEAM-LOG.md`; §7 sharpness, RUE-2485). An
unchecked program puts an owned value under a `Copy` one, the shape a copy
would duplicate an owner through: over `S0 = @copy struct { x0: i64 }` and
`S1`, affine with a destructor, `let p = S0 { x0: S1 { 1 } }; let q = p;
@drop(p.x0); @drop(q.x0); 0` (`Trace.lean`'s `dupProgram`). It is not
`ProgramTyped`, and `eval` refuses it with `ownedUnderCopy`, at the literal:
`no_violation`'s conclusion fails once `ProgramTyped` is dropped, and a machine
without the copy-closure monitor (`Contents.copyClosed` in `introVal`) makes
this statement false. -/
def copy_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.mkStruct 1 [.intLit .w64 .signed 1]])
        (.letIn false (.use (.var 0))
          (.seq (.drop (.proj (.var 1) 0))
            (.seq (.drop (.proj (.var 0) 0)) (.intLit .w64 .signed 0)))) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .copy, fields := [.int .w64 .signed], dtor := false, cls := .copy },
                { attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine }],
            enums := [] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ run Float.exactOps P 200 = .stuck .ownedUnderCopy

/-- **The leak monitor fires** (R3 of `REDTEAM-LOG.md`; §7 sharpness, RUE-2485).
`let x = S1 { 1 }; 0`, with `S1` declared `linear`, leaves a live linear
value at the scope's end. It is not `ProgramTyped`, and `eval` refuses it with
`linearLeak`: `no_linear_leak`'s conclusion fails once `ProgramTyped` is
dropped. §6's relation, which has no monitor, runs it to a value, which `run`
never returns at any fuel: `eval_complete` needs `ProgramTyped` too. A machine
whose leak monitor is off, or does not read a declared-`linear` struct's own
obligation (`Contents.residualLinear`), makes this statement false. -/
def leak_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 1 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ run Float.exactOps P 200 = .stuck .linearLeak ∧
      ∃ H φ v tr, Steps Float.exactOps P Config.init (.run H φ [] (.ret v) tr) ∧
        ¬ ∃ n, ∀ fuel, n < fuel → run Float.exactOps P fuel = .ok H v tr

/-- **The overwrite monitor fires** (R3 of `REDTEAM-LOG.md`; §7 sharpness,
RUE-2485). `let mut x = S1 { 1 }; x = S1 { 2 }; @drop(x); 0` overwrites a live
linear value. It is not `ProgramTyped`, and `eval` refuses the assignment with
`linearOverwrite`: `no_linear_overwrite`'s conclusion fails once
`ProgramTyped` is dropped, and a machine without the overwrite monitor makes
this statement false. -/
def overwrite_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn true (.mkStruct 1 [.intLit .w64 .signed 1])
        (.seq (.assign (.var 0) (.mkStruct 1 [.intLit .w64 .signed 2]))
          (.seq (.drop (.var 0)) (.intLit .w64 .signed 0))) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ run Float.exactOps P 200 = .stuck .linearOverwrite

/-- **The discard monitor fires** (R3 of `REDTEAM-LOG.md`; §7 sharpness, RUE-2485).
`S1 { 3 }; @panic("boom")` discards a linear value. It is not `ProgramTyped`,
and `eval` refuses the sequence with `linearDiscard`: `no_linear_discard`'s
conclusion fails once `ProgramTyped` is dropped. §6's relation drops the
value and reaches the panic, which `run` never answers: `eval_complete`'s
panic half needs `ProgramTyped` too. A machine without the discard monitor
makes this statement false. -/
def discard_stmt : Prop :=
  ∀ B : Expr, B =
      .seq (.mkStruct 1 [.intLit .w64 .signed 3]) (.panic "boom") →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ run Float.exactOps P 200 = .stuck .linearDiscard ∧
      ∃ κ tr, Steps Float.exactOps P Config.init (.panic κ tr) ∧
        ¬ ∃ n, ∀ fuel, n < fuel → run Float.exactOps P fuel = .panic κ tr

/-- **A loop that discards a linear value each turn** (§7 sharpness, RUE-2485).
`loop { S1 { 3 }; () }` is not `ProgramTyped`. `eval` refuses its first turn
with `linearDiscard`, while §6's relation, which has no monitor, turns forever:
it has runs of every length from `Config.init`, and every configuration they
reach steps. So `eval_diverges_iff` and `never_stuck_iff` fail once
`ProgramTyped` is dropped: one side of each holds and the other does not. A
machine without the discard monitor makes this statement false. -/
def discard_loop_stmt : Prop :=
  ∀ B : Expr, B =
      .loop (.seq (.mkStruct 1 [.intLit .w64 .signed 3]) .unitLit) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .unit, body := B }] } →
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ run Float.exactOps P 200 = .stuck .linearDiscard ∧
      (∀ n, ∃ D, StepsN Float.exactOps P n Config.init D) ∧
      (∀ C, Steps Float.exactOps P Config.init C → C.Terminal ∨ ∃ C', Step Float.exactOps P C C') ∧
      ¬ ((∀ fuel, run Float.exactOps P fuel = .outOfFuel) ↔ ∀ n, ∃ D, StepsN Float.exactOps P n Config.init D) ∧
      ¬ ((∀ fuel w, run Float.exactOps P fuel ≠ .stuck w) ↔
        ∀ C, Steps Float.exactOps P Config.init C → C.Terminal ∨ ∃ C', Step Float.exactOps P C C')

/-- **Fuel bounds, dropped** (§7 sharpness, RUE-2485). The checked program of
`Nonvacuous.dtor` exhausts fuel `0` and returns at fuel `200`, a value §6's
relation reaches. So `fuel_mono` fails without `n ≤ m` (`n = 200`, `m = 0`) and
without `eval n ≠ outOfFuel` (`n = 0`, `m = 200`); `no_masking` fails without
its first hypothesis (`eval n` is a value, not a refusal); and
`eval_complete`'s and `run_complete`'s value halves fail without `n < fuel`:
no `n` makes the value, or a refusal, the answer at every fuel. `run P fuel`
is `eval` at `main()` (`run`'s definition). `fuel_mono` and `no_masking` are
stated over `eval`; the statement gives `run P n` and `eval` at `main()` as
the same term, for every `n` (`run`'s definition). The pairing is
kernel-checked (`Sharp/Glue.lean`, RUE-2495). -/
def fuel_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ (∀ n, run Float.exactOps P n = eval Float.exactOps n P [] Frame.empty (.call 0 [])) ∧ run Float.exactOps P 0 = .outOfFuel ∧
      ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧
        Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        ¬ (200 ≤ 0) ∧ 0 ≤ 200 ∧ run Float.exactOps P 0 ≠ run Float.exactOps P 200 ∧
        (∀ w, run Float.exactOps P 200 ≠ .stuck w) ∧
        ¬ ∀ fuel, run Float.exactOps P fuel = .ok H v tr ∨ ∃ w, run Float.exactOps P fuel = .stuck w

/-- **Fuel bounds, dropped, at a panic** (§7 sharpness, RUE-2485). The checked
program of `Nonvacuous.panic` panics, and §6's relation reaches the panic, but
fuel `0` is exhausted: `eval_complete`'s and `run_complete`'s panic halves
fail without `n < fuel`. -/
def fuel_panic_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.seq (.dbg (.intLit .w64 .signed 5)) (.panic "boom")) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ run Float.exactOps P 0 = .outOfFuel ∧
      Steps Float.exactOps P Config.init (.panic .user [.dbg (.int .w64 .signed 5)]) ∧
      ¬ ∀ fuel, run Float.exactOps P fuel = .panic .user [.dbg (.int .w64 .signed 5)] ∨
        ∃ w, run Float.exactOps P fuel = .stuck w

/-- **A checked expression at a type its result does not fit** (§7 sharpness,
RUE-2485). `check` accepts the literal `1` at `i64` in the checked program of
`Nonvacuous.dtor`, and its result does not fit `bool`; no derivation types it
at `bool`. So `check_sound` needs `c.fits T = true`. -/
def not_fits_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ ∃ c Ω, check P (.int .w64 .signed) [] (.intLit .w64 .signed 1) = some (c, Ω) ∧
        c.fits .bool = false ∧ ¬ Typed P (.int .w64 .signed) [] (.intLit .w64 .signed 1) .bool Ω

/-- **An unchecked program that runs a destructor twice on one value**
(§7 sharpness, RUE-2485). Over a `@copy` struct `C` that declares a destructor
(which `DtorNotCopy`, and `WfDecls`, exclude) and an affine `W { x0: C }`,
`let c = C { 1 }; let a = W { c }; let b = W { c }; 0` copies `c` into two
`W`s, and dropping both runs `C`'s destructor on identity `0` twice. It is not
`ProgramTyped`, and `run` returns: `dtor_once` fails without `DtorNotCopy`, and
`no_double_free` without `ProgramTyped`. `¬ DtorNotCopy` is shown directly
(struct `0`); `¬ ProgramTyped` through `no_double_free`. (RUE-2400's cases, a dynamic read and
an array repeat of an affine value, no longer double-drop: `eval` refuses them
with `typeConfusion`.) The same run is reached by §6's relation (`run_sim`), so
`step_no_double_free` fails without `ProgramTyped` too. -/
def double_drop_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 1 [.use (.var 0)])
          (.letIn false (.mkStruct 1 [.use (.var 1)]) (.intLit .w64 .signed 0))) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .copy, fields := [.int .w64 .signed], dtor := true, cls := .copy },
                { attr := .none, fields := [.struct 0], dtor := false, cls := .affine },
                { attr := .linear, fields := [.struct 0, .struct 3], dtor := false, cls := .linear },
                { attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine }],
            enums := [] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ ¬ DtorNotCopy P.decls ∧
      ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧ (dtorIds tr).count 0 = 2 ∧
        ¬ (∀ a, (dtorIds (run Float.exactOps P 200).trace).count a ≤ 1)

/-- **An unchecked program whose trace runs a destructor outside a drop**
(§7 sharpness, RUE-2485). Over the same `@copy` struct `C` with a destructor, a
declared-`linear` `L { x0: C, x1: A }` and an affine `A` with a destructor,
`let l = L { C { 1 }, A { 2 } }; let s = l.x1; 0` destructures `l`: its
residue `C { 1 }` is `Copy`, so it is dropped with no marker, and its
destructor event opens the trace. It is not `ProgramTyped`, and §6's relation
runs it to a value whose trace is not in §6.11's block grammar: `drop_order`
fails without `ProgramTyped` (through `DtorNotCopy`), and so does
`drop_glue_order`, since a trace outside `Blocks` is outside `GlueBlocks`
(RUE-2487). -/
def bare_dtor_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 2 [.mkStruct 0 [.intLit .w64 .signed 1], .mkStruct 3 [.intLit .w64 .signed 2]])
        (.letIn false (.use (.proj (.var 0) 1)) (.intLit .w64 .signed 0)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .copy, fields := [.int .w64 .signed], dtor := true, cls := .copy },
                { attr := .none, fields := [.struct 0], dtor := false, cls := .affine },
                { attr := .linear, fields := [.struct 0, .struct 3], dtor := false, cls := .linear },
                { attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine }],
            enums := [] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = false ∧ ¬ ProgramTyped P ∧
      ∃ H φ v tr, Steps Float.exactOps P Config.init (.run H φ [] (.ret v) tr) ∧ ¬ Blocks P.decls tr

/-- **A checked program with a function that is not `pendingSafe`** (§7 sharpness,
RUE-2485; RUE-2316's carve-out). Beside an entry point returning `0`, `fn
g(s: S0) -> i64 { [s, return 7]; 0 }` is typed, but the array literal's first
element is pending when the second unwinds, so the program is not
`pendingSafe`. The call `g(s)`, from a frame holding `s : S0` at cell `0`,
returns `7` and ends `s`'s identity nowhere: it is in no cell, not in the
result and not in the trace. So `drop_exactly_once` and `rest_exactly_once`
fail without `P.pendingSafe` (`Exact` fails); everything else they ask holds. -/
def pending_program_stmt : Prop :=
  ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := .intLit .w64 .signed 0 },
          { params := [{ ty := .struct 0, mu := false }], ret := .int .w64 .signed,
            body := .seq (.mkArray (.struct 0) [.use (.var 0), .ret (.intLit .w64 .signed 7)])
              (.intLit .w64 .signed 0) }] } →
    ∀ e : Expr, e = .call 1 [.use (.var 0)] →
      ProgramTyped P ∧ P.pendingSafe = false ∧ e.pendingSafe = true ∧
      FrameMatches P.decls [{ ty := .struct 0, mu := false, st := .owned }] { env := [0], scope := [0] } [.full (.struct 0 0 [.int .w64 .signed 5])] ∧ StoreCC P.decls [.full (.struct 0 0 [.int .w64 .signed 5])] ∧
      (∃ c Ω, check P (.int .w64 .signed) [{ ty := .struct 0, mu := false, st := .owned }] e = some (c, Ω) ∧ c.fits (.int .w64 .signed) = true ∧
        Typed P (.int .w64 .signed) [{ ty := .struct 0, mu := false, st := .owned }] e (.int .w64 .signed) Ω) ∧
      Lead Float.exactOps P 200 [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } [.full .hole] [(.struct 0 0 [.int .w64 .signed 5])] [] e ∧
      eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e = (eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e).withTrace [] ∧
      ¬ Exact P.decls [.full (.struct 0 0 [.int .w64 .signed 5])] [] (eval Float.exactOps 200 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e) ∧
      ¬ Exact P.decls [.full .hole] (Contents.ownList P.decls (Contents.ofVals [(.struct 0 0 [.int .w64 .signed 5])]))
        (eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e)

/-- **An expression that is not `pendingSafe`** (§7 sharpness, RUE-2485; RUE-2316's
carve-out). In the checked program of `Nonvacuous.dtor`, `0; [s, return 7];
1` is typed in the context `s : S0`, but the array literal's first element is
pending when the second unwinds. From a frame holding `s` at cell `0`, the
`return` retires the frame and ends `s`'s identity nowhere. So
`drop_exactly_once` and `rest_exactly_once` fail without `e.pendingSafe`
(`Exact` fails); everything else they ask holds, a `Lead` (the discarded `0`)
included. -/
def pending_expr_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
    ∀ e : Expr, e = .seq (.intLit .w64 .signed 0)
        (.seq (.mkArray (.struct 0) [.use (.var 0), .ret (.intLit .w64 .signed 7)])
          (.intLit .w64 .signed 1)) →
      ProgramTyped P ∧ P.pendingSafe = true ∧ e.pendingSafe = false ∧
      FrameMatches P.decls [{ ty := .struct 0, mu := false, st := .owned }] { env := [0], scope := [0] } [.full (.struct 0 0 [.int .w64 .signed 5])] ∧ StoreCC P.decls [.full (.struct 0 0 [.int .w64 .signed 5])] ∧
      (∃ c Ω, check P (.int .w64 .signed) [{ ty := .struct 0, mu := false, st := .owned }] e = some (c, Ω) ∧ c.fits (.int .w64 .signed) = true ∧
        Typed P (.int .w64 .signed) [{ ty := .struct 0, mu := false, st := .owned }] e (.int .w64 .signed) Ω) ∧
      Lead Float.exactOps P 200 [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } [.full (.struct 0 0 [.int .w64 .signed 5])] [.int .w64 .signed 0] [] e ∧
      eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e = (eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e).withTrace [] ∧
      ¬ Exact P.decls [.full (.struct 0 0 [.int .w64 .signed 5])] [] (eval Float.exactOps 200 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e) ∧
      ¬ Exact P.decls [.full (.struct 0 0 [.int .w64 .signed 5])] (Contents.ownList P.decls (Contents.ofVals [.int .w64 .signed 0]))
        (eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e)

/-- **A store that is not copy-closed** (§7 sharpness, RUE-2485). The store's one
cell holds an `[i64; 1]` array (a `Copy` type) with an owned `S0` inside it,
outside the frame. `1; 2` is typed and run from the empty frame over it, which
agrees with the empty context, in the checked program of `Nonvacuous.dtor`;
everything else `drop_exactly_once` and `rest_exactly_once` ask holds. The
evaluation leaves the cell alone, and `Exact` asks the final store to be
copy-closed, which it is not: both fail without `StoreCC`. -/
def store_cc_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
    ∀ e : Expr, e = .seq (.intLit .w64 .signed 1) (.intLit .w64 .signed 2) →
      ProgramTyped P ∧ P.pendingSafe = true ∧ e.pendingSafe = true ∧
      FrameMatches P.decls [] Frame.empty [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] ∧ ¬ StoreCC P.decls [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] ∧
      (∃ c Ω, check P (.int .w64 .signed) [] e = some (c, Ω) ∧ c.fits (.int .w64 .signed) = true ∧
        Typed P (.int .w64 .signed) [] e (.int .w64 .signed) Ω) ∧
      Lead Float.exactOps P 200 [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] Frame.empty [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] [.int .w64 .signed 1] [] e ∧
      eval Float.exactOps 201 P [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] Frame.empty e = (eval Float.exactOps 201 P [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] Frame.empty e).withTrace [] ∧
      ¬ Exact P.decls [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] [] (eval Float.exactOps 200 P [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] Frame.empty e) ∧
      ¬ Exact P.decls [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] (Contents.ownList P.decls (Contents.ofVals [.int .w64 .signed 1]))
        (eval Float.exactOps 201 P [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] Frame.empty e)

/-- **A `Lead` that did not happen** (§7 sharpness, RUE-2485). For the body of
`Nonvacuous.dtor`, run from the empty frame, take the store `ℓ0 ↦ S0 { 1 }`
and the pending value `S0 { 1 }` (identity `0`) as if the leading operand had
produced them; it did not (`Lead` fails: it minted identity `0` into a reserved
slot). Everything else `rest_exactly_once` asks holds, and the evaluation ends
identity `0` once, not the twice those counts ask: `Exact` fails. -/
def no_lead_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ P.pendingSafe = true ∧ B.pendingSafe = true ∧
      FrameMatches P.decls [] Frame.empty [] ∧ StoreCC P.decls [] ∧
      (∃ c Ω, check P (.int .w64 .signed) [] B = some (c, Ω) ∧ c.fits (.int .w64 .signed) = true ∧
        Typed P (.int .w64 .signed) [] B (.int .w64 .signed) Ω) ∧
      ¬ Lead Float.exactOps P 200 [] Frame.empty [.full (.struct 0 0 [.int .w64 .signed 1])]
        [.struct 0 0 [.int .w64 .signed 1]] [] B ∧
      eval Float.exactOps 201 P [] Frame.empty B = (eval Float.exactOps 201 P [] Frame.empty B).withTrace [] ∧
      ¬ Exact P.decls [.full (.struct 0 0 [.int .w64 .signed 1])]
        (Contents.ownList P.decls (Contents.ofVals [.struct 0 0 [.int .w64 .signed 1]]))
        (eval Float.exactOps 201 P [] Frame.empty B)

/-- **A result that is not the evaluation's** (§7 sharpness, RUE-2485). For the body
of `Nonvacuous.dtor`, whose leading operand has a `Lead`, a refusal is not
what the evaluation at `fuel + 1` answers, and `rest_exactly_once`'s
conclusion, which starts with "never refused", fails for it: the hypothesis
`eval (fuel + 1) … = r.withTrace tr` is what ties `r` to the program. -/
def no_eval_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ P.pendingSafe = true ∧ B.pendingSafe = true ∧
      FrameMatches P.decls [] Frame.empty [] ∧ StoreCC P.decls [] ∧
      (∃ c Ω, check P (.int .w64 .signed) [] B = some (c, Ω) ∧ c.fits (.int .w64 .signed) = true ∧
        Typed P (.int .w64 .signed) [] B (.int .w64 .signed) Ω) ∧
      ∃ H₁ vs tr, Lead Float.exactOps P 200 [] Frame.empty H₁ vs tr B ∧
        ∀ w, eval Float.exactOps 201 P [] Frame.empty B ≠ (EvalRes.stuck w).withTrace tr

/-- **A value §6's relation does not reach** (§7 sharpness, RUE-2485). For the
checked program of `Nonvacuous.dtor`, the terminal configuration with the
value `8`, the empty store and a trace that opens with a destructor event is
not reached from `Config.init`, is not `run`'s answer at any fuel past any
bound, and its trace is not in §6.11's block grammar. So each statement
whose conclusion claims something of a reached or answered value fails once
the hypothesis naming that value is dropped: `eval_sound`'s and `run_sim`'s
`run … = .ok H v tr`, `eval_complete`'s and `run_complete`'s `Steps … (.ret
v)`, and `drop_order`'s and `drop_glue_order`'s (a trace outside `Blocks` is
outside `GlueBlocks`, RUE-2487). -/
def unreached_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧
      ¬ Steps Float.exactOps P Config.init (.run [] Frame.empty [] (.ret (.int .w64 .signed 8)) [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])]) ∧
      run Float.exactOps P 200 ≠ .ok [] (.int .w64 .signed 8) [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])] ∧
      ¬ Blocks P.decls [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])] ∧
      ¬ ∃ n, ∀ fuel, n < fuel → run Float.exactOps P fuel = .ok [] (.int .w64 .signed 8) [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])] ∨
        ∃ w, run Float.exactOps P fuel = .stuck w

/-- **A panic §6's relation does not reach** (§7 sharpness, RUE-2485). The same, for
the panic whose trace opens with a destructor event: not reached, not `run`'s
answer past any bound (the program returns), not in the block grammar. So
`eval_sound`'s and `run_sim`'s `run … = .panic k tr`, `eval_complete`'s and
`run_complete`'s `Steps … (.panic κ tr)`, and `drop_order`'s and
`drop_glue_order`'s are needed. -/
def unreached_panic_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧
      ¬ Steps Float.exactOps P Config.init (.panic .user [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])]) ∧
      run Float.exactOps P 200 ≠ .panic .user [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])] ∧
      ¬ Blocks P.decls [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])] ∧
      ¬ ∃ n, ∀ fuel, n < fuel → run Float.exactOps P fuel = .panic .user [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])] ∨
        ∃ w, run Float.exactOps P fuel = .stuck w

/-- **An unreachable configuration whose registration stack is out of order**
(§7 sharpness, RUE-2485). For the checked program of `Nonvacuous.dtor`, a
configuration whose frame registers cell `1` before cell `0` takes a step, but
`Config.init` does not reach it: `drop_order`'s last half fails without the
hypothesis that the configuration is reached. -/
def unordered_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧
      ¬ Steps Float.exactOps P Config.init
        (.run [] { env := [], scope := [1, 0] } [] (.eval (.intLit .w64 .signed 1)) []) ∧
      Step Float.exactOps P (.run [] { env := [], scope := [1, 0] } [] (.eval (.intLit .w64 .signed 1)) [])
        (.run [] { env := [], scope := [1, 0] } [] (.ret (.int .w64 .signed 1)) []) ∧
      ¬ ∃ evs, (Config.run [] { env := [], scope := [1, 0] } [] (.ret (.int .w64 .signed 1)) []).trace =
          (Config.run [] { env := [], scope := [1, 0] } [] (.eval (.intLit .w64 .signed 1)) []).trace ++ evs ∧
        NewestFirst (dropLocs evs) ∧
        Lifo (Config.run [] { env := [], scope := [1, 0] } [] (.eval (.intLit .w64 .signed 1)) []).stack
          (Config.run [] { env := [], scope := [1, 0] } [] (.ret (.int .w64 .signed 1)) []).stack (dropLocs evs) ∧
        (Config.run [] { env := [], scope := [1, 0] } [] (.eval (.intLit .w64 .signed 1)) []).stack.Pairwise (· < ·)

/-- **A pair that is not a step** (§7 sharpness, RUE-2485). For the checked program
of `Nonvacuous.dtor`, the value `run` returns is reached, with a trace that is
not empty, and the panic with an empty trace does not follow it by a step; its
trace does not extend the value's, so `drop_order`'s last half fails without
the hypothesis `Step … C C'`. -/
def not_a_step_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ ∃ H v tr, Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        tr ≠ [] ∧ ¬ Step Float.exactOps P (.run H Frame.empty [] (.ret v) tr) (.panic .user []) ∧
        ¬ ∃ evs, (Config.panic .user []).trace = (Config.run H Frame.empty [] (.ret v) tr).trace ++ evs ∧
          NewestFirst (dropLocs evs) ∧
          Lifo (Config.run H Frame.empty [] (.ret v) tr).stack (Config.panic .user []).stack
            (dropLocs evs) ∧
          (Config.run H Frame.empty [] (.ret v) tr).stack.Pairwise (· < ·)

/-- **The initial configuration, which steps** (§7 sharpness, RUE-2485). For the
checked program of `Nonvacuous.dtor`, `Config.init` steps to the argument
list of `main()`, and not to itself; it is not terminal and not stuck (with
`linearLeak`, a monitor's tag, not one of §6's stuck states); and `run` is
never stuck. So `Step.det` fails without either of its step hypotheses,
`Step.terminal` without `C.Terminal`, `step_stuck_isStuckState` without
`C.Stuck`, and `run_stuck_of_step_stuck` without `C.Stuck`. -/
def init_steps_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ Step Float.exactOps P Config.init (.run [] Frame.empty [] (.args (.call 0) [] []) []) ∧ ¬ Step Float.exactOps P Config.init Config.init ∧
      ((.run [] Frame.empty [] (.args (.call 0) [] []) []) : Config) ≠ Config.init ∧ ¬ Config.init.Terminal ∧
      ¬ Config.init.Stuck Float.exactOps P .linearLeak ∧ Violation.isStuckState .linearLeak = false ∧
      Steps Float.exactOps P Config.init Config.init ∧
      ¬ ∃ n, ∀ fuel, n < fuel → ∃ w', run Float.exactOps P fuel = .stuck w'

/-- **A stuck configuration that is not reached** (§7 sharpness, RUE-2485). For the
checked program of `Nonvacuous.dtor`, whose `run` is never stuck, a
configuration reading an unbound name is stuck and is not reached from
`Config.init`. So `step_progress`, `step_preservation`,
`step_never_stuck_of_run` and `run_stuck_of_step_stuck` fail without the
hypothesis that the configuration is reached, and so does `never_stuck_iff`:
its left side holds and its right side, over every configuration, does not. -/
def unreachable_stuck_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ ((.run [] Frame.empty [] (.eval (.use (.var 0))) []) : Config).Stuck Float.exactOps P .unbound ∧
      ¬ Steps Float.exactOps P Config.init (.run [] Frame.empty [] (.eval (.use (.var 0))) []) ∧
      (∀ fuel w, run Float.exactOps P fuel ≠ .stuck w) ∧
      ¬ (((.run [] Frame.empty [] (.eval (.use (.var 0))) []) : Config).Terminal ∨ ∃ C', Step Float.exactOps P (.run [] Frame.empty [] (.eval (.use (.var 0))) []) C') ∧
      (∀ T, ¬ ((.run [] Frame.empty [] (.eval (.use (.var 0))) []) : Config).SafeAt Float.exactOps P T) ∧
      ¬ (∀ C, C.Terminal ∨ ∃ C', Step Float.exactOps P C C') ∧
      ¬ ∃ n, ∀ fuel, n < fuel → ∃ w', run Float.exactOps P fuel = .stuck w'

/-- **A configuration that reads a retired cell, not reached** (§7 sharpness,
RUE-2496). For the checked program of `Nonvacuous.dtor`, a configuration whose
frame names a cell already retired (`†`) is stuck with `useAfterDrop`, and
`eval` from the same store and frame refuses the same way; the configuration
is not reached from `Config.init` (shown through `step_no_use_after_drop`
itself). So `step_no_use_after_drop` fails without the hypothesis that the
configuration is reached: the refusal is live from an open configuration, and
what keeps it away is the start, not the program's typing. -/
def retired_cell_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧
      eval Float.exactOps 1 P [.dead] { env := [0], scope := [] } (.use (.var 0)) = .stuck .useAfterDrop ∧
      ((.run [.dead] { env := [0], scope := [] } [] (.eval (.use (.var 0))) []) : Config).Stuck
        Float.exactOps P .useAfterDrop ∧
      ¬ Steps Float.exactOps P Config.init (.run [.dead] { env := [0], scope := [] } [] (.eval (.use (.var 0))) [])

/-- **A configuration whose trace destroys one value twice, not reached** (§7
sharpness, RUE-2477). For the checked program of `Nonvacuous.dtor`, the panic
whose trace runs `S0`'s destructor twice on identity `0` names that identity
twice among its destructor events, and `Config.init` does not reach it (shown
through `step_no_double_free` itself). So `step_no_double_free` fails without
the hypothesis that the configuration is reached: the bound is a property of
the runs of the program, not of every trace a configuration can carry. -/
def unreached_double_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧
      ¬ Steps Float.exactOps P Config.init
        (.panic .user [.dtor 0 (.struct 0 0 [.int .w64 .signed 1]), .dtor 0 (.struct 0 0 [.int .w64 .signed 1])]) ∧
      (dtorIds (Config.panic .user
        [.dtor 0 (.struct 0 0 [.int .w64 .signed 1]), .dtor 0 (.struct 0 0 [.int .w64 .signed 1])]).trace).count 0 = 2

/-- **A pop that drops a cell it did not cut, not reached** (§7 sharpness,
RUE-2500). For the checked program of `Nonvacuous.dtor`, a configuration whose
frame registers cells `0` and `1`, in location order, but whose pending
`endscope` names cell `0` rather than the newest cell `1` takes a step: (D-EndScope)
pops one cell off the record, which leaves `[0]`, and drops cell `0`, the cell
it kept. Its drop markers name one cell, so they are newest first, and its
stack is in location order; only `Lifo` fails: the step cut cell `1` and
dropped cell `0`. `Config.init` does not reach it, so `drop_order`'s last half
fails without the hypothesis that the configuration is reached, on the one
conjunct `unordered` leaves alone. -/
def uncut_drop_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧
      ¬ Steps Float.exactOps P Config.init
        (.run [.full (.struct 0 0 [.int .w64 .signed 1]), .full (.struct 0 1 [.int .w64 .signed 2])]
          { env := [1, 0], scope := [0, 1] } [.endscope [0]] (.ret (.int .w64 .signed 3)) []) ∧
      Step Float.exactOps P
        (.run [.full (.struct 0 0 [.int .w64 .signed 1]), .full (.struct 0 1 [.int .w64 .signed 2])]
          { env := [1, 0], scope := [0, 1] } [.endscope [0]] (.ret (.int .w64 .signed 3)) [])
        (.run [.dead, .full (.struct 0 1 [.int .w64 .signed 2])] { env := [0], scope := [0] } []
          (.ret (.int .w64 .signed 3))
          [.drop 0 (.struct 0 0 [.int .w64 .signed 1]), .dtor 0 (.struct 0 0 [.int .w64 .signed 1])]) ∧
      NewestFirst [0] ∧ [0, 1].Pairwise (· < ·) ∧ ¬ Lifo [0, 1] [0] [0] ∧
      ¬ ∃ evs,
        (Config.run [.dead, .full (.struct 0 1 [.int .w64 .signed 2])] { env := [0], scope := [0] } []
          (.ret (.int .w64 .signed 3))
          [.drop 0 (.struct 0 0 [.int .w64 .signed 1]), .dtor 0 (.struct 0 0 [.int .w64 .signed 1])]).trace =
          (Config.run [.full (.struct 0 0 [.int .w64 .signed 1]), .full (.struct 0 1 [.int .w64 .signed 2])]
            { env := [1, 0], scope := [0, 1] } [.endscope [0]] (.ret (.int .w64 .signed 3)) []).trace ++ evs ∧
        NewestFirst (dropLocs evs) ∧
        Lifo
          (Config.run [.full (.struct 0 0 [.int .w64 .signed 1]), .full (.struct 0 1 [.int .w64 .signed 2])]
            { env := [1, 0], scope := [0, 1] } [.endscope [0]] (.ret (.int .w64 .signed 3)) []).stack
          (Config.run [.dead, .full (.struct 0 1 [.int .w64 .signed 2])] { env := [0], scope := [0] } []
            (.ret (.int .w64 .signed 3))
            [.drop 0 (.struct 0 0 [.int .w64 .signed 1]), .dtor 0 (.struct 0 0 [.int .w64 .signed 1])]).stack
          (dropLocs evs) ∧
        (Config.run [.full (.struct 0 0 [.int .w64 .signed 1]), .full (.struct 0 1 [.int .w64 .signed 2])]
          { env := [1, 0], scope := [0, 1] } [.endscope [0]] (.ret (.int .w64 .signed 3)) []).stack.Pairwise (· < ·)

/-- **A halted configuration with a value of another type, not reached** (§7
sharpness, RUE-2500). For the checked program of `Nonvacuous.dtor`, whose entry
point returns `i64`, the configuration that has halted with `true` is terminal,
so nothing it reaches is stuck; but its value is not an `i64`, so it is not
`SafeAt` the entry type, and `Config.init` does not reach it. So
`step_preservation` fails without the hypothesis that the configuration is
reached, through `SafeAt`'s typing half, where `unreachable_stuck` fails it
through the progress half. -/
def ill_typed_halt_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ ((.run [] Frame.empty [] (.ret (.bool true)) []) : Config).Terminal ∧
      ¬ Steps Float.exactOps P Config.init (.run [] Frame.empty [] (.ret (.bool true)) []) ∧
      ¬ ((.run [] Frame.empty [] (.ret (.bool true)) []) : Config).SafeAt Float.exactOps P
        (.int .w64 .signed)

/-- **A halted configuration with an `i64` out of range, not reached** (§7
sharpness, RUE-2500; §6.1's `n_T`). For the same program, the configuration
that has halted with `2^63`, one past `i64`'s maximum, is terminal, but the
value is not well typed at `i64` (`HasTy` carries the bounds), so the
configuration is not `SafeAt` the entry type, and `Config.init` does not reach
it. So `step_preservation` fails without the hypothesis that the configuration
is reached, at a value of the right form whose only fault is its range. -/
def out_of_range_halt_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧
      ((.run [] Frame.empty [] (.ret (.int .w64 .signed 9223372036854775808)) []) : Config).Terminal ∧
      ¬ HasTy P.decls (.int .w64 .signed 9223372036854775808) (.int .w64 .signed) ∧
      ¬ Steps Float.exactOps P Config.init
        (.run [] Frame.empty [] (.ret (.int .w64 .signed 9223372036854775808)) []) ∧
      ¬ ((.run [] Frame.empty [] (.ret (.int .w64 .signed 9223372036854775808)) []) : Config).SafeAt
        Float.exactOps P (.int .w64 .signed)

/-- **Halted configurations with a datum outside `𝔽_f64`, not reached** (§7
sharpness, RUE-2500; §2's `𝔽_w`, §6.1's `f_T`). For the checked program of
`Nonvacuous.float`, whose entry point returns `f64` and whose run reaches the
datum `15 · 2^-1`, two data that are not in `𝔽_f64` are not `Wf`: `30 · 2^-2`,
the same number with an even significand (not canonical), and `1 · 2^-1075`,
half the least subnormal (below the floor `eMin`). The configurations that
have halted with them are terminal, not `SafeAt` the entry type, and not
reached from `Config.init`. So `step_preservation` fails without the
hypothesis that the configuration is reached, at a float value whose only
fault is its datum. -/
def float_halt_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false
        (.binop .add (.floatLit .w64 { sig := 15, negExp := true, e := 1 })
          (.floatLit .w64 { sig := 225, negExp := true, e := 2 }))
        (.binop .mul (.use (.var 0)) (.floatLit .w64 { sig := 2, negExp := false, e := 0 })) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .float .w64, body := B }] } →
      ProgramTyped P ∧
      (∃ H tr, Steps Float.exactOps P Config.init
        (.run H Frame.empty [] (.ret (.float .w64 (.num false 15 (-1)))) tr)) ∧
      ¬ (FloatDatum.num false 30 (-2)).Wf .w64 ∧ ¬ (FloatDatum.num false 1 (-1075)).Wf .w64 ∧
      ((.run [] Frame.empty [] (.ret (.float .w64 (.num false 30 (-2)))) []) : Config).Terminal ∧
      ((.run [] Frame.empty [] (.ret (.float .w64 (.num false 1 (-1075)))) []) : Config).Terminal ∧
      ¬ Steps Float.exactOps P Config.init
        (.run [] Frame.empty [] (.ret (.float .w64 (.num false 30 (-2)))) []) ∧
      ¬ Steps Float.exactOps P Config.init
        (.run [] Frame.empty [] (.ret (.float .w64 (.num false 1 (-1075)))) []) ∧
      ¬ ((.run [] Frame.empty [] (.ret (.float .w64 (.num false 30 (-2)))) []) : Config).SafeAt
        Float.exactOps P (.float .w64) ∧
      ¬ ((.run [] Frame.empty [] (.ret (.float .w64 (.num false 1 (-1075)))) []) : Config).SafeAt
        Float.exactOps P (.float .w64)

/-- **An owned value hidden under a `Copy` node, lost** (§7 sharpness,
RUE-2478; the ill-typed shape of `double_drop`, without the copies). With `S0`
a `@copy` struct whose field is an `i64`, `let p = S0 { S1 { 1 } }; 0` is
rejected by the checker (the field is given an `S1`), so it is not
`ProgramTyped` (shown through `whole_program_exactly_once` itself), and it is
`pendingSafe`. §6's relation, which has no copy-closure monitor, runs it: the
configuration after `S1`'s (D-Struct) holds `S1`'s identity `0`; (D-Struct)
wraps it in the `Copy` `S0`, which owns nothing, and `p`'s drop at scope exit
is a `Copy` cell's, which runs nothing; the run finishes with `0` and an empty
trace. Identity `0` is neither in the result nor ended: without
`ProgramTyped`, `whole_program_exactly_once` fails. -/
def copy_leak_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.mkStruct 1 [.intLit .w64 .signed 1]]) (.intLit .w64 .signed 0) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .copy, fields := [.int .w64 .signed], dtor := false, cls := .copy },
                { attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine }],
            enums := [] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ P.pendingSafe = true ∧
      ∃ C, Steps Float.exactOps P Config.init C ∧ 0 ∈ C.held P.decls ∧
        ∃ H v tr, Steps Float.exactOps P C (.run H Frame.empty [] (.ret v) tr) ∧
          (v.own P.decls).count 0 + (freedIds P.decls tr).count 0 = 0

/-- **A pending argument discarded by a `return`** (§7 sharpness, RUE-2478;
RUE-2316, the shape of `TraceExact.lean`'s `pendingSafe_needed` in `main`
itself). `fn main() -> i64 { f(S0 { 7 }, return 0) }` with `fn f(a: S0, b:
i64) -> i64 { @drop(a); b }` is accepted by the checker and is not
`pendingSafe`: the second argument returns. §6's relation reaches the
configuration holding the minted `S0` (identity `0`) pending in the call's
argument list; (D-Return) discards that list, and the run finishes with `0`
and an empty trace. Identity `0` is neither in the result nor ended: without
`pendingSafe`, `whole_program_exactly_once` fails on a checked program. A
`break` out of a pending form loses a value the same way (`loop { S2 { S0 { 1 },
break }; () }; 0`); `pendingSafe` rules out both. -/
def pending_leak_stmt : Prop :=
  ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed,
                  body := .call 1 [.mkStruct 0 [.intLit .w64 .signed 7], .ret (.intLit .w64 .signed 0)] },
                { params := [⟨.struct 0, false⟩, ⟨.int .w64 .signed, false⟩], ret := .int .w64 .signed,
                  body := .seq (.drop (.var 1)) (.use (.var 0)) }] } →
    checkProgram P = true ∧ ProgramTyped P ∧ P.pendingSafe = false ∧
    ∃ C, Steps Float.exactOps P Config.init C ∧ 0 ∈ C.held P.decls ∧
      ∃ H v tr, Steps Float.exactOps P C (.run H Frame.empty [] (.ret v) tr) ∧
        (v.own P.decls).count 0 + (freedIds P.decls tr).count 0 = 0

/-- **A configuration holding a value no run holds** (§7 sharpness, RUE-2478).
For the checked, `pendingSafe` program of `Nonvacuous.dtor`, the terminal
configuration whose one cell holds an `S0` with identity `5`, and whose
result `0` and trace are empty, holds identity `5`, reaches itself, and ends
it nowhere; `Config.init` does not reach it (shown through
`whole_program_exactly_once` itself). So the statement fails without the
hypothesis that the configuration is reached: it is about the values a run
holds, not about every configuration's. -/
def unreached_held_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ P.pendingSafe = true ∧
      ¬ Steps Float.exactOps P Config.init
        (.run [.full (.struct 0 5 [.int .w64 .signed 1])] Frame.empty [] (.ret (.int .w64 .signed 0)) []) ∧
      5 ∈ (Config.run [.full (.struct 0 5 [.int .w64 .signed 1])] Frame.empty []
        (.ret (.int .w64 .signed 0)) []).held P.decls ∧
      ((Val.int .w64 .signed 0).own P.decls).count 5 + (freedIds P.decls []).count 5 = 0

/-- **An identity the run never holds** (§7 sharpness, RUE-2478). The checked,
`pendingSafe` program of `Nonvacuous.dtor` finishes from `Config.init`, which
holds nothing; its trace ends identities `0` and `2` and nothing else, so
identity `1` — the index of `x`'s cell, which names a cell and no value — is
neither ended nor in the result. So the statement fails without the
hypothesis that the configuration holds the identity: it counts owned values,
not every index. -/
def unheld_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ P.pendingSafe = true ∧ 1 ∉ Config.init.held P.decls ∧
      ∃ H v tr, Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        (v.own P.decls).count 1 + (freedIds P.decls tr).count 1 = 0

/-- **A finished configuration the run does not reach** (§7 sharpness,
RUE-2478). The checked, `pendingSafe` program of `Nonvacuous.dtor` reaches a
configuration holding `x`'s `S0`, identity `0`; the terminal configuration
with an empty store, result `3` and an empty trace ends nothing, and that
configuration does not reach it (shown through `whole_program_exactly_once`
itself). So the statement fails without the hypothesis that the end is the
run's own. -/
def off_run_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3)) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      ProgramTyped P ∧ P.pendingSafe = true ∧
      ∃ C, Steps Float.exactOps P Config.init C ∧ 0 ∈ C.held P.decls ∧
        ¬ Steps Float.exactOps P C (.run [] Frame.empty [] (.ret (.int .w64 .signed 3)) []) ∧
        ((Val.int .w64 .signed 3).own P.decls).count 0 + (freedIds P.decls []).count 0 = 0

end RueCore.Spec.Sharp
