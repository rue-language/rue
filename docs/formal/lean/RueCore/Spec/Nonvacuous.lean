module

public import RueCore.Float
public import RueCore.Checker.Defs
public import RueCore.Soundness.Defs
public import RueCore.Trace.Defs
public import RueCore.Adequacy.Defs

@[expose] public section

/-!
# RueCore.Spec.Nonvacuous — the spine's hypotheses hold of real programs (Spec layer)

A kernel-checked theorem can still be empty: a checker that accepts nothing
is trivially sound, and a statement over every `M : FloatModel` holds
vacuously if no model satisfies the laws (RUE-2469). Each statement here says
that some spine statements' hypotheses **hold together, of a non-trivial
program**, with the program written out and its non-triviality in the
statement itself: the checker accepts it, it is `ProgramTyped`, its body is
typed, its run returns (or panics, or diverges, or is stuck) and is reached by
§6's relation from `Config.init`, and its trace drops several values, runs
several destructors, or returns a particular value.

The programs cover the construct classes of the fragment: destructors,
declared-linear values, loops, arrays, enums with `match`, early `return`,
`@panic` and floats, plus a program that diverges and an unchecked one that
gets stuck. Each is a small copy of a corpus case (`Corpus.lean`), named in its
doc-comment, over one declaration environment: `S0`, an affine struct with a
destructor; `S1`, a `linear` struct; and `E0 { K0(S0), K1 }`. Every program is
run on `Float.exactOps`, which `exact_model` shows is the operations of a
`FloatModel`, so the statements quantified over `M : FloatModel` apply to it.

`Spec.witnesses` (`Spec.lean`) lists each statement with the theorem that
proves it (`RueCore/Nonvacuous.lean`, layer L2) and the spine statements
whose hypotheses it instantiates; `SPINE.md` prints the list under each spine
statement as its "non-vacuous" line, and the kernel, the lint, Lean Comparator
and the fingerprints cover these statements as they cover the spine's.

A witness shows a hypothesis is satisfiable, not that it is needed: that a
statement fails once a hypothesis is dropped (its sharpness) is RUE-2485.
-/

namespace RueCore.Spec.Nonvacuous


/-- **The float laws have a model: `Float.exactOps`** (§7's "totality of the
float operations"; RUE-2469). Some `FloatModel` has the executable instance
`Float.exactOps` as its operations, so every law of `FloatModel` holds of the
model the corpus runs on, and the laws are jointly satisfiable: the 19 spine
statements that quantify over `M : FloatModel` are not vacuous in `M`. -/
def exact_model_stmt : Prop :=
  ∃ M : FloatModel, M.toFloatOps = Float.exactOps

/-- **The initial frame agrees with the empty context** (§6.12's initial
configuration): at every declaration environment, the empty frame over the
empty store matches the empty context (`FrameMatches`) and its store is
copy-closed (`StoreCC`). With a program's typed body this is the frame and
store the evaluation statements (`soundness`, `drop_exactly_once`,
`rest_exactly_once`) are applied at by the witnesses below. -/
def empty_frame_stmt : Prop :=
  ∀ D : Decls, FrameMatches D [] Frame.empty [] ∧ StoreCC D []

/-- **An open term in a live frame** (§6.1, §7): the evaluation statements apply
beyond the empty frame. Over the witnesses' declarations, `@drop(s); 1` is
typed by `check` in the context `s : S0`, owned, and the frame `{ ρ := [ℓ0],
σ := [ℓ0] }` over the store `ℓ0 ↦ S0 { 5 }` agrees with that context
(`FrameMatches`) and is copy-closed (`StoreCC`), for a checked, `pendingSafe`
program. Its evaluation runs the destructor of the value it started with, and
its leading operand has a `Lead`, so `soundness`, `drop_exactly_once` and
`rest_exactly_once` apply to a term with a free variable and a store that is
not empty. -/
def open_frame_stmt : Prop :=
  ∀ D : Decls, D =
      { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] } →
    ∀ e : Expr, e = .seq (.drop (.var 0)) (.intLit .w64 .signed 1) →
    ∀ P : Program, P =
      { decls := D, fns := [{ params := [], ret := .int .w64 .signed, body := .intLit .w64 .signed 0 }] } →
      ProgramTyped P ∧ P.pendingSafe = true ∧ e.pendingSafe = true ∧
      FrameMatches D [{ ty := .struct 0, mu := false, st := .owned }]
        { env := [0], scope := [0] } [.full (.struct 0 0 [.int .w64 .signed 5])] ∧
      StoreCC D [.full (.struct 0 0 [.int .w64 .signed 5])] ∧
      (∃ c Ω, check P (.int .w64 .signed) [{ ty := .struct 0, mu := false, st := .owned }] e =
          some (c, Ω) ∧ c.fits (.int .w64 .signed) = true ∧
        Typed P (.int .w64 .signed) [{ ty := .struct 0, mu := false, st := .owned }] e
          (.int .w64 .signed) Ω) ∧
      1 ≤ (dtorIds (eval Float.exactOps 200 P [.full (.struct 0 0 [.int .w64 .signed 5])]
        { env := [0], scope := [0] } e).trace).length ∧
      ∃ H₁ vs tr, ∃ r : EvalRes,
        Lead Float.exactOps P 200 [.full (.struct 0 0 [.int .w64 .signed 5])]
          { env := [0], scope := [0] } H₁ vs tr e ∧
        eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])]
          { env := [0], scope := [0] } e = r.withTrace tr

/-- **A checked program that drops two values with destructors** (§6.11, §7; construct
class: destructors; the corpus case `affine_scope_drop`, with two bindings). The program `let a = S0 { 1 }; let b = S0 { 2 }; 3`, over an affine
`S0` that declares a destructor, is accepted, is `ProgramTyped` and
`pendingSafe`, and its body is typed by `check`. Its run returns, reached by
`Step` from `Config.init`, and its trace frees two identities and runs two
destructors. It also carries the other hypotheses of the trace statements:
the declarations keep destructor-bearing structs off `Copy` (`DtorNotCopy`),
the initial configuration steps, no fuel makes the run stuck, the body's own
evaluation drops two values, and the body's leading operand mints an owned
identity (`Lead`), so `rest_exactly_once` applies to a value minted
mid-evaluation. -/
def dtor_stmt : Prop :=
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
      checkProgram P = true ∧ ProgramTyped P ∧ P.pendingSafe = true ∧
      (∃ c Ω, check P (.int .w64 .signed) [] B = some (c, Ω) ∧
        c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [] B (.int .w64 .signed) Ω) ∧
      DtorNotCopy P.decls ∧ (∃ C, Step Float.exactOps P Config.init C) ∧
      (∀ fuel w, run Float.exactOps P fuel ≠ .stuck w) ∧
      2 ≤ (freedIds P.decls (eval Float.exactOps 200 P [] Frame.empty B).trace).length ∧
      (∃ H₁ vs tr, ∃ r : EvalRes, Lead Float.exactOps P 200 [] Frame.empty H₁ vs tr B ∧
        eval Float.exactOps 201 P [] Frame.empty B = r.withTrace tr ∧
        Contents.ownList P.decls (Contents.ofVals vs) ≠ []) ∧
      ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧
        Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        2 ≤ (freedIds P.decls tr).length ∧ 2 ≤ (dtorIds tr).length

/-- **A checked program with a declared-linear value** (§5.6, §7; construct class:
declared-linear values; the corpus case `linear_explicit_drop`'s shape). `let x
= S1 { 1 }; let y = S0 { 2 }; @drop(x); 3`, with `S1` declared `linear`, is
accepted and typed; its run returns, reached by `Step`, and its trace frees
both values: the linear one at its `@drop`, the affine one at scope exit. -/
def linear_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 1 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2])
          (.seq (.drop (.var 1)) (.intLit .w64 .signed 3))) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = true ∧ ProgramTyped P ∧ P.pendingSafe = true ∧
      (∃ c Ω, check P (.int .w64 .signed) [] B = some (c, Ω) ∧
        c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [] B (.int .w64 .signed) Ω) ∧
      ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧
        Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        2 ≤ (freedIds P.decls tr).length

/-- **A checked program with a loop that turns three times** (§5.7, §6.10; construct class:
loops; the corpus case `loop_counted`'s shape). A counted loop over a `mut`
counter, breaking once it reaches `3`, whose body binds an affine `S0` each
turn, is accepted and typed; its run returns, reached by `Step`, and its trace
runs three destructors, one per turn. -/
def loop_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn true (.intLit .w64 .signed 0)
        (.seq
          (.loop
            (.seq (.ite (.binop .ge (.use (.var 0)) (.intLit .w64 .signed 3)) .brk .unitLit)
              (.seq (.assign (.var 0) (.binop .add (.use (.var 0)) (.intLit .w64 .signed 1)))
                (.letIn false (.mkStruct 0 [.use (.var 0)]) .unitLit))))
          (.use (.var 0))) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = true ∧ ProgramTyped P ∧ P.pendingSafe = true ∧
      (∃ c Ω, check P (.int .w64 .signed) [] B = some (c, Ω) ∧
        c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [] B (.int .w64 .signed) Ω) ∧
      ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧
        Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        3 ≤ (dtorIds tr).length

/-- **A checked program with an array** (§6.5, §6.11; construct class: arrays; the corpus
cases `array_drop_order` and `array_dyn_read_below`). `let a = [S0 { 1 }, S0 {
2 }]; a[1].x0`, a dynamic-index read of a `Copy` leaf below an array of
destructor-bearing elements, is accepted and typed; its run returns, reached
by `Step`, and its trace runs both elements' destructors. -/
def array_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false
        (.mkArray (.struct 0)
          [.mkStruct 0 [.intLit .w64 .signed 1], .mkStruct 0 [.intLit .w64 .signed 2]])
        (.indexRead (.var 0) [(.intLit .w64 .signed 1)] [[0]]) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = true ∧ ProgramTyped P ∧ P.pendingSafe = true ∧
      (∃ c Ω, check P (.int .w64 .signed) [] B = some (c, Ω) ∧
        c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [] B (.int .w64 .signed) Ω) ∧
      ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧
        Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        2 ≤ (dtorIds tr).length

/-- **A checked program with an enum and a `match`** (§5.5, §6.6; construct class: enums with
`match`; the corpus case `enum_match_affine`). `let e = E0::K0(S0 { 1 }); match
e { K0(s) => s.x0, K1 => 0 }` is accepted and typed; its run returns, reached
by `Step`, and its trace frees two identities (the scrutinee's shell,
consumed by the match, and the payload, dropped at the arm's end) and runs
the payload's destructor. -/
def enum_match_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkEnum 0 0 [(.mkStruct 0 [.intLit .w64 .signed 1])])
        (.«match» (.use (.var 0)) [.use (.proj (.var 0) 0), (.intLit .w64 .signed 0)]) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = true ∧ ProgramTyped P ∧ P.pendingSafe = true ∧
      (∃ c Ω, check P (.int .w64 .signed) [] B = some (c, Ω) ∧
        c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [] B (.int .w64 .signed) Ω) ∧
      ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧
        Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        2 ≤ (freedIds P.decls tr).length ∧ 1 ≤ (dtorIds tr).length

/-- **A checked program with an early `return`** (§6.9; construct class: early
`return`; the corpus case `return_past_affine`). `let a = S0 { 1 }; let b = S0
{ 2 }; return 7; 0` is accepted and typed; its run returns `7` as an ordinary
value (the call boundary absorbs the unwind, which is what `run_ne_returned`
says), reached by `Step`, and the unwind runs both destructors. -/
def early_return_stmt : Prop :=
  ∀ B : Expr, B =
      .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2])
          (.seq (.ret (.intLit .w64 .signed 7)) (.intLit .w64 .signed 0))) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .int .w64 .signed, body := B }] } →
      checkProgram P = true ∧ ProgramTyped P ∧ P.pendingSafe = true ∧
      (∃ c Ω, check P (.int .w64 .signed) [] B = some (c, Ω) ∧
        c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [] B (.int .w64 .signed) Ω) ∧
      ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧
        Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        v = .int .w64 .signed 7 ∧ 2 ≤ (dtorIds tr).length

/-- **A checked program that panics** (construct class: `@panic`;
`Examples.panicPastAffine` with a `@dbg` line before the trap, beside the
corpus case `panic_after_drop`). `let a = S0 { 1 }; @dbg(5); @panic("boom")` is
accepted and typed; its run is the user panic with the `@dbg` line in its
trace and no drop (§5.7 exempts the panic edge), and §6's relation reaches
the same panic from `Config.init`. -/
def panic_stmt : Prop :=
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
      checkProgram P = true ∧ ProgramTyped P ∧ P.pendingSafe = true ∧
      (∃ c Ω, check P (.int .w64 .signed) [] B = some (c, Ω) ∧
        c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [] B (.int .w64 .signed) Ω) ∧
      run Float.exactOps P 200 = .panic .user [.dbg (.int .w64 .signed 5)] ∧
        Steps Float.exactOps P Config.init (.panic .user [.dbg (.int .w64 .signed 5)])

/-- **A checked program that computes with floats** (§6.4; construct class: floats;
the corpus case `float_arith`). `let x = 1.5 + 2.25; x * 2.0` at `f64` is
accepted and typed; run on `Float.exactOps` it returns `7.5`, the datum `15 ·
2^-1`, reached by `Step`. -/
def float_stmt : Prop :=
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
      checkProgram P = true ∧ ProgramTyped P ∧ P.pendingSafe = true ∧
      (∃ c Ω, check P (.float .w64) [] B = some (c, Ω) ∧
        c.fits (.float .w64) = true ∧ Typed P (.float .w64) [] B (.float .w64) Ω) ∧
      ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧
        Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        v = .float .w64 (.num false 15 (-1))

/-- **A checked program that diverges** (§6.10; the loop with no `break`). `loop
{ () }` as the entry point returning `()` is accepted, and its run exhausts
every fuel, so both sides of `eval_diverges_iff` hold of it, as neither does
of the witnesses above, which return. -/
def diverges_stmt : Prop :=
  ∀ P : Program, P =
      { decls := { structs := [], enums := [] },
        fns := [{ params := [], ret := .unit, body := .loop .unitLit }] } →
      checkProgram P = true ∧ ProgramTyped P ∧ ∀ fuel, run Float.exactOps P fuel = .outOfFuel

/-- **A checked program that diverges and drops a value on every turn** (§6.10,
§6.7; RUE-2477). `loop { let s = S0 { 1 }; () }` as the entry point returning
`()` is accepted and typed, and its run exhausts every fuel, so `run`'s answer
carries no trace and `no_double_free` says nothing about it. Yet §6's relation
reaches, from `Config.init`, a configuration two turns in whose trace has run
`S0`'s destructor on two distinct identities (`0` and `2`, one value minted per
turn) and freed both. So `step_no_double_free`'s hypotheses hold of a diverging
run whose trace is not empty: its bound is not vacuous where `no_double_free`'s
is. -/
def diverges_drop_stmt : Prop :=
  ∀ B : Expr, B = .loop (.letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) .unitLit) →
    ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [{ params := [], ret := .unit, body := B }] } →
      checkProgram P = true ∧ ProgramTyped P ∧ (∀ fuel, run Float.exactOps P fuel = .outOfFuel) ∧
      ∃ C, Steps Float.exactOps P Config.init C ∧ dtorIds C.trace = [0, 2] ∧
        2 ≤ (freedIds P.decls C.trace).length

/-- **An unchecked program that gets stuck** (§6.3's read of a `⊘`; the corpus
case `use_after_move` reads its moved binding the same way). `let a = S0 { 1
}; @drop(a); a.x0` is rejected by the checker; run
unchecked, `eval` refuses it with `useAfterMove`, and §6's relation reaches a
configuration stuck with the same violation from `Config.init`. So the
statements whose hypothesis is a stuck run or a stuck configuration are not
vacuous either. -/
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
      checkProgram P = false ∧ run Float.exactOps P 200 = .stuck .useAfterMove ∧
        ∃ C, Steps Float.exactOps P Config.init C ∧ C.Stuck Float.exactOps P .useAfterMove

end RueCore.Spec.Nonvacuous
