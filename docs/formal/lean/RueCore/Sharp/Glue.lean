module

public import RueCore.Spine

@[expose] public section

/-!
# RueCore.Sharp.Glue — each sharpness pair checked in the kernel (layer L2)

`Spec.sharpness` pairs each counter-example statement with the spine
hypotheses it shows needed, each a spine theorem and a hypothesis number
(`Lint.hypotheses`). This module checks every pair in the kernel (RUE-2495):
for the counter-example `RueCore.Sharp.<x>` and hypothesis `i` of
`RueCore.<t>`, one theorem, `Glue.<x>.<t>_<i>`, whose statement is `¬ W`,
where `W` is `<t>`'s Spec statement with hypothesis `i` removed, and whose
proof takes the counter-example's facts (`RueCore.Spine.Sharp.<x>`) and
refutes `W` at its program. The lint (`Lint.sharpProblems`) computes `W`
itself, from the Spec statement and the number (`Lint.dropHyp`, the walk that
numbers the hypotheses), and fails when a listed pair's theorem is missing
here, does not use the counter-example's `RueCore.Spine` constant, or does
not state exactly `¬ W` (up to binder names); and when this module declares a
theorem that ties no listed pair. So a pair names a hypothesis whose removal
the counter-example refutes: a swapped or invented pair does not pass.

The statements are `Lint.dropHyp`'s output as Lean prints it: the spine
statement's binders and conclusion, with the dropped hypothesis's binder
left out, written with `∀ … →` where the Spec writes `(_ : …)`. Where the
dropped hypothesis is a premise inside a conjunction, an `↔` or an `∃` of the
conclusion, the whole statement is kept with that premise removed, and the
proof refutes the conjunct it was in. The programs are the counter-example
statements' own, abbreviated here where a proof names one (helper module).
-/

namespace RueCore.Sharp.Glue

/-- The witnesses' declarations: `S0`, affine with a destructor; `S1`,
`linear`; `E0 { K0(S0), K1 }` (helper). -/
abbrev decls : Decls :=
  { structs :=
      [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
        { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
    enums := [{ variants := [[.struct 0], []], cls := .affine }] }

/-- The checked body of `Nonvacuous.dtor` (helper). -/
abbrev bodyDtor : Expr :=
  .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
    (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3))

/-- The checked program of `Nonvacuous.dtor` (helper). -/
abbrev progDtor : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyDtor }] }

/-- The unchecked body of `Sharp.stuck`, which reads a dropped value (helper). -/
abbrev bodyStuck : Expr :=
  .letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) (.seq (.drop (.var 0)) (.use (.proj (.var 0) 0)))

/-- The program of `Sharp.stuck` (helper). -/
abbrev progStuck : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyStuck }] }


/-- `Sharp.stuck` refutes `soundness` without hypothesis 1 (helper). -/
theorem stuck.soundness_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat) {R : Ty} {Γ : Ctx} {Ω : Out} {e : RueCore.Expr}
        {T : Ty},
        Typed P R Γ e T Ω →
          ∀ {φ : Frame} {H : Store},
            FrameMatches P.decls Γ φ H →
              EvalOk P.decls T R Ω.norm Ω.brk φ H (eval M.toFloatOps fuel P H φ e) := by
  intro h
  obtain ⟨-, hnpt, hnwf, -, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht, hne⟩, hl, h200, -, h201, -, hr200, -, hnot⟩ :=
    Spine.Sharp.stuck _ rfl _ rfl
  exact hne (h Float.exactModel 200 ht hfm)

/-- `Sharp.stuck` refutes `run_safe` without hypothesis 1 (helper). -/
theorem stuck.run_safe_1 :
    ¬∀ (M : FloatModel) {P : Program} {fd : FnDef},
        P.fns[0]? = some fd →
          fd.params = [] →
            ∀ (fuel : Nat),
              run M.toFloatOps P fuel = EvalRes.outOfFuel ∨
                (∃ k tr, run M.toFloatOps P fuel = EvalRes.panic k tr) ∨
                  ∃ H v tr, run M.toFloatOps P fuel = EvalRes.ok H v tr ∧ HasTy P.decls v fd.ret := by
  intro h
  obtain ⟨-, hnpt, hnwf, -, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht, hne⟩, hl, h200, -, h201, -, hr200, -, hnot⟩ :=
    Spine.Sharp.stuck _ rfl _ rfl
  exact hnot (h Float.exactModel (fd := { params := [], ret := .int .w64 .signed, body := bodyStuck }) rfl rfl 200)

/-- `Sharp.stuck` refutes `no_violation` without hypothesis 1 (helper). -/
theorem stuck.no_violation_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat) (w : Violation),
        run M.toFloatOps P fuel ≠ EvalRes.stuck w := by
  intro h
  obtain ⟨-, hnpt, hnwf, -, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht, hne⟩, hl, h200, -, h201, -, hr200, -, hnot⟩ :=
    Spine.Sharp.stuck _ rfl _ rfl
  exact h Float.exactModel 200 _ hr200

/-- `Sharp.stuck` refutes `no_use_after_move` without hypothesis 1 (helper). -/
theorem stuck.no_use_after_move_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat),
        run M.toFloatOps P fuel ≠ EvalRes.stuck Violation.useAfterMove := by
  intro h
  obtain ⟨-, hnpt, hnwf, -, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht, hne⟩, hl, h200, -, h201, -, hr200, -, hnot⟩ :=
    Spine.Sharp.stuck _ rfl _ rfl
  exact h Float.exactModel 200 hr200

/-- `Sharp.stuck` refutes `no_masking` without hypothesis 2 (helper). -/
theorem stuck.no_masking_2 :
    ¬∀ (M : FloatOps) {P : Program} {H : Store} {φ : Frame} {e : RueCore.Expr} {n m : Nat}
        {w : Violation}, eval M n P H φ e = EvalRes.stuck w → eval M m P H φ e = EvalRes.stuck w := by
  intro h
  obtain ⟨-, hnpt, hnwf, -, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht, hne⟩, hl, h200, -, h201, -, hr200, -, hnot⟩ :=
    Spine.Sharp.stuck _ rfl _ rfl
  have h0 : eval Float.exactOps 0 progStuck [] Frame.empty (.call 0 []) = .outOfFuel := rfl
  have := h Float.exactOps (m := 0) h200
  rw [h0] at this
  cases this

/-- `Sharp.stuck` refutes `checkProgram_sound` without hypothesis 1 (helper). -/
theorem stuck.checkProgram_sound_1 :
    ¬∀ {P : Program}, ProgramTyped P := by
  intro h
  obtain ⟨-, hnpt, hnwf, -, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht, hne⟩, hl, h200, -, h201, -, hr200, -, hnot⟩ :=
    Spine.Sharp.stuck _ rfl _ rfl
  exact hnpt h

/-- `Sharp.stuck` refutes `drop_exactly_once` without hypothesis 1 (helper). -/
theorem stuck.drop_exactly_once_1 :
    ¬∀ (M : FloatModel) {P : Program},
        P.pendingSafe = true →
          ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
            {H : Store},
            Typed P R Γ e T Ω →
              FrameMatches P.decls Γ φ H →
                StoreCC P.decls H →
                  e.pendingSafe = true →
                    (∀ (w : Violation), eval M.toFloatOps fuel P H φ e ≠ EvalRes.stuck w) ∧
                      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) ∧
                        Tidy φ H (eval M.toFloatOps fuel P H φ e) := by
  intro h
  obtain ⟨-, hnpt, hnwf, -, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht, hne⟩, hl, h200, -, h201, -, hr200, -, hnot⟩ :=
    Spine.Sharp.stuck _ rfl _ rfl
  exact (h Float.exactModel hps (fuel := 200) ht hfm hcc heps).1 _ h200

/-- `Sharp.stuck` refutes `rest_exactly_once` without hypothesis 1 (helper). -/
theorem stuck.rest_exactly_once_1 :
    ¬∀ (M : FloatModel) {P : Program},
        P.pendingSafe = true →
          ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
            {H : Store},
            Typed P R Γ e T Ω →
              FrameMatches P.decls Γ φ H →
                StoreCC P.decls H →
                  e.pendingSafe = true →
                    ∀ {H₁ : Store} {vs : List Val} {tr : List Event},
                      Lead M.toFloatOps P fuel H φ H₁ vs tr e →
                        ∀ {r : EvalRes},
                          eval M.toFloatOps (fuel + 1) P H φ e = EvalRes.withTrace tr r →
                            (∀ (w : Violation), r ≠ EvalRes.stuck w) ∧
                              Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧
                                Settled φ H₁ r := by
  intro h
  obtain ⟨-, hnpt, hnwf, -, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht, hne⟩, hl, h200, -, h201, -, hr200, -, hnot⟩ :=
    Spine.Sharp.stuck _ rfl _ rfl
  exact (h Float.exactModel hps ht hfm hcc heps hl h201).1 _ rfl

/-- `Sharp.stuck` refutes `eval_sound` without hypothesis 1 (helper). -/
theorem stuck.eval_sound_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat),
        (∀ (w : Violation), run M.toFloatOps P fuel ≠ EvalRes.stuck w) ∧
          (∀ (H : Store) (v : Val) (tr : List Event),
              run M.toFloatOps P fuel = EvalRes.ok H v tr →
                Steps M.toFloatOps P Config.init (Config.run H Frame.empty [] (Focus.ret v) tr)) ∧
            ∀ (k : PanicKind) (tr : List Event),
              run M.toFloatOps P fuel = EvalRes.panic k tr →
                Steps M.toFloatOps P Config.init (Config.panic k tr) := by
  intro h
  obtain ⟨-, hnpt, hnwf, -, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht, hne⟩, hl, h200, -, h201, -, hr200, -, hnot⟩ :=
    Spine.Sharp.stuck _ rfl _ rfl
  exact (h Float.exactModel 200).1 _ hr200

/-- `Sharp.stuck_step` refutes `step_progress` without hypothesis 1 (helper). -/
theorem stuck_step.step_progress_1 :
    ¬∀ (M : FloatModel) {P : Program} (C : RueCore.Config),
        Steps M.toFloatOps P Config.init C → C.Terminal ∨ ∃ C', Step M.toFloatOps P C C' := by
  intro h
  obtain ⟨-, ⟨C, hC, hst⟩, -, -, hprog, hpres, hts, hn⟩ := Spine.Sharp.stuck_step _ rfl _ rfl
  exact hprog (h Float.exactModel)

/-- `Sharp.stuck_step` refutes `step_preservation` without hypothesis 1 (helper). -/
theorem stuck_step.step_preservation_1 :
    ¬∀ (M : FloatModel) {P : Program},
        ∃ fd,
          P.fns[0]? = some fd ∧
            ∀ (C : RueCore.Config),
              Steps M.toFloatOps P Config.init C → Config.SafeAt M.toFloatOps P fd.ret C := by
  intro h
  obtain ⟨-, ⟨C, hC, hst⟩, -, -, hprog, hpres, hts, hn⟩ := Spine.Sharp.stuck_step _ rfl _ rfl
  exact hpres (h Float.exactModel)

/-- `Sharp.stuck_step` refutes `step_type_safety` without hypothesis 1 (helper). -/
theorem stuck_step.step_type_safety_1 :
    ¬∀ (M : FloatModel) {P : Program},
        ∃ fd,
          P.fns[0]? = some fd ∧
            ∀ (n : Nat),
              (∃ D, StepsN M.toFloatOps P n Config.init D) ∨
                (∃ H v tr,
                    Steps M.toFloatOps P Config.init (Config.run H Frame.empty [] (Focus.ret v) tr) ∧
                      HasTy P.decls v fd.ret) ∨
                  ∃ κ tr, Steps M.toFloatOps P Config.init (Config.panic κ tr) := by
  intro h
  obtain ⟨-, ⟨C, hC, hst⟩, -, -, hprog, hpres, hts, hn⟩ := Spine.Sharp.stuck_step _ rfl _ rfl
  exact hts (h Float.exactModel)

/-- `Sharp.stuck_step` refutes `step_never_stuck_of_run` without hypothesis 1 (helper). -/
theorem stuck_step.step_never_stuck_of_run_1 :
    ¬∀ (M : FloatOps) (P : Program) (C : RueCore.Config),
        Steps M P Config.init C → C.Terminal ∨ ∃ C', Step M P C C' := by
  intro h
  obtain ⟨-, ⟨C, hC, hst⟩, -, -, hprog, hpres, hts, hn⟩ := Spine.Sharp.stuck_step _ rfl _ rfl
  exact hprog (h Float.exactOps _)

/-- `Sharp.stuck_step` refutes `run_stuck_of_step_stuck` without hypothesis 3 (helper). -/
theorem stuck_step.run_stuck_of_step_stuck_3 :
    ¬∀ (M : FloatOps) (P : Program) {C : RueCore.Config} {w : Violation},
        Steps M P Config.init C →
          Config.Stuck M P C w → ∃ _n : Nat, ∀ (fuel : Nat), ∃ w', run M P fuel = EvalRes.stuck w' := by
  intro h
  obtain ⟨-, ⟨C, hC, hst⟩, -, -, hprog, hpres, hts, hn⟩ := Spine.Sharp.stuck_step _ rfl _ rfl
  obtain ⟨_, hk⟩ := h _ _ hC hst
  exact hn hk

/-- `Sharp.typed` refutes `soundness` without hypothesis 2 (helper). -/
theorem typed.soundness_2 :
    ¬∀ (M : FloatModel) {P : Program},
        WfProgram P →
          ∀ (fuel : Nat) {R : Ty} {Γ : Ctx} {Ω : Out} {e : RueCore.Expr} {T : Ty} {φ : Frame}
            {H : Store},
            FrameMatches P.decls Γ φ H →
              EvalOk P.decls T R Ω.norm Ω.brk φ H (eval M.toFloatOps fuel P H φ e) := by
  intro h
  obtain ⟨hPT, hwf, hps, heps, hfm, hcc, -, hnt, hl, h200, -, h201, hfit, hne⟩ :=
    Spine.Sharp.typed _ rfl _ rfl _ rfl
  exact hne (.int .w64 .signed) ⟨none, []⟩
    (h Float.exactModel hwf 200 (R := .int .w64 .signed) (T := .int .w64 .signed) (Ω := ⟨none, []⟩) hfm)

/-- `Sharp.typed` refutes `check_sound` without hypothesis 1 (helper). -/
theorem typed.check_sound_1 :
    ¬∀ {P : Program} {R : Ty} (e : RueCore.Expr) {Γ : Ctx} {c : CTy} {Ω : Out} (T : Ty),
        c.fits T = true → Typed P R Γ e T Ω := by
  intro h
  obtain ⟨hPT, hwf, hps, heps, hfm, hcc, -, hnt, hl, h200, -, h201, hfit, hne⟩ :=
    Spine.Sharp.typed _ rfl _ rfl _ rfl
  exact hnt (.int .w64 .signed) ⟨none, []⟩
    (h (R := .int .w64 .signed) (Γ := []) (c := .never) (Ω := ⟨none, []⟩) _ _ hfit)

/-- `Sharp.typed` refutes `drop_exactly_once` without hypothesis 3 (helper). -/
theorem typed.drop_exactly_once_3 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {_R : Ty} {Γ : Ctx} {e : RueCore.Expr} {_T : Ty} {_Ω : Out} {φ : Frame}
              {H : Store},
              FrameMatches P.decls Γ φ H →
                StoreCC P.decls H →
                  e.pendingSafe = true →
                    (∀ (w : Violation), eval M.toFloatOps fuel P H φ e ≠ EvalRes.stuck w) ∧
                      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) ∧
                        Tidy φ H (eval M.toFloatOps fuel P H φ e) := by
  intro h
  obtain ⟨hPT, hwf, hps, heps, hfm, hcc, -, hnt, hl, h200, -, h201, hfit, hne⟩ :=
    Spine.Sharp.typed _ rfl _ rfl _ rfl
  exact (h Float.exactModel hPT hps (fuel := 200) (_R := .int .w64 .signed) (_T := .int .w64 .signed)
    (_Ω := ⟨none, []⟩) hfm hcc heps).1 _ h200

/-- `Sharp.typed` refutes `rest_exactly_once` without hypothesis 3 (helper). -/
theorem typed.rest_exactly_once_3 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {_R : Ty} {Γ : Ctx} {e : RueCore.Expr} {_T : Ty} {_Ω : Out} {φ : Frame}
              {H : Store},
              FrameMatches P.decls Γ φ H →
                StoreCC P.decls H →
                  e.pendingSafe = true →
                    ∀ {H₁ : Store} {vs : List Val} {tr : List Event},
                      Lead M.toFloatOps P fuel H φ H₁ vs tr e →
                        ∀ {r : EvalRes},
                          eval M.toFloatOps (fuel + 1) P H φ e = EvalRes.withTrace tr r →
                            (∀ (w : Violation), r ≠ EvalRes.stuck w) ∧
                              Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧
                                Settled φ H₁ r := by
  intro h
  obtain ⟨hPT, hwf, hps, heps, hfm, hcc, -, hnt, hl, h200, -, h201, hfit, hne⟩ :=
    Spine.Sharp.typed _ rfl _ rfl _ rfl
  exact (h Float.exactModel hPT hps (_R := .int .w64 .signed) (_T := .int .w64 .signed)
    (_Ω := ⟨none, []⟩) hfm hcc heps hl h201).1 _ rfl

/-- `Sharp.frame` refutes `soundness` without hypothesis 3 (helper). -/
theorem frame.soundness_3 :
    ¬∀ (M : FloatModel) {P : Program},
        WfProgram P →
          ∀ (fuel : Nat) {R : Ty} {Γ : Ctx} {Ω : Out} {e : RueCore.Expr} {T : Ty},
            Typed P R Γ e T Ω →
              ∀ {φ : Frame} {H : Store},
                EvalOk P.decls T R Ω.norm Ω.brk φ H (eval M.toFloatOps fuel P H φ e) := by
  intro h
  obtain ⟨hPT, hwf, hps, heps, hcc, ⟨c, Ω, -, -, ht, hne⟩, -, hl, h200, -, h201⟩ :=
    Spine.Sharp.frame _ rfl _ rfl _ rfl
  exact hne (h Float.exactModel hwf 200 ht (φ := Frame.empty) (H := []))

/-- `Sharp.frame` refutes `drop_exactly_once` without hypothesis 4 (helper). -/
theorem frame.drop_exactly_once_4 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
              {H : Store},
              Typed P R Γ e T Ω →
                StoreCC P.decls H →
                  e.pendingSafe = true →
                    (∀ (w : Violation), eval M.toFloatOps fuel P H φ e ≠ EvalRes.stuck w) ∧
                      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) ∧
                        Tidy φ H (eval M.toFloatOps fuel P H φ e) := by
  intro h
  obtain ⟨hPT, hwf, hps, heps, hcc, ⟨c, Ω, -, -, ht, hne⟩, -, hl, h200, -, h201⟩ :=
    Spine.Sharp.frame _ rfl _ rfl _ rfl
  exact (h Float.exactModel hPT hps (fuel := 200) (φ := Frame.empty) ht hcc heps).1 _ h200

/-- `Sharp.frame` refutes `rest_exactly_once` without hypothesis 4 (helper). -/
theorem frame.rest_exactly_once_4 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
              {H : Store},
              Typed P R Γ e T Ω →
                StoreCC P.decls H →
                  e.pendingSafe = true →
                    ∀ {H₁ : Store} {vs : List Val} {tr : List Event},
                      Lead M.toFloatOps P fuel H φ H₁ vs tr e →
                        ∀ {r : EvalRes},
                          eval M.toFloatOps (fuel + 1) P H φ e = EvalRes.withTrace tr r →
                            (∀ (w : Violation), r ≠ EvalRes.stuck w) ∧
                              Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧
                                Settled φ H₁ r := by
  intro h
  obtain ⟨hPT, hwf, hps, heps, hcc, ⟨c, Ω, -, -, ht, hne⟩, -, hl, h200, -, h201⟩ :=
    Spine.Sharp.frame _ rfl _ rfl _ rfl
  exact (h Float.exactModel hPT hps (φ := Frame.empty) ht hcc heps hl h201).1 _ rfl

/-- `Sharp.no_entry` refutes `run_safe` without hypothesis 2 (helper). -/
theorem no_entry.run_safe_2 :
    ¬∀ (M : FloatModel) {P : Program} {fd : FnDef},
        WfProgram P →
          fd.params = [] →
            ∀ (fuel : Nat),
              run M.toFloatOps P fuel = EvalRes.outOfFuel ∨
                (∃ k tr, run M.toFloatOps P fuel = EvalRes.panic k tr) ∨
                  ∃ H v tr, run M.toFloatOps P fuel = EvalRes.ok H v tr ∧ HasTy P.decls v fd.ret := by
  intro h
  obtain ⟨hwf, -, -, hn⟩ := Spine.Sharp.no_entry _ rfl
  exact hn { params := [], ret := .unit, body := .unitLit }
    (h Float.exactModel hwf (fd := { params := [], ret := .unit, body := .unitLit }) rfl 200)

/-- `Sharp.entry_param` refutes `run_safe` without hypothesis 3 (helper). -/
theorem entry_param.run_safe_3 :
    ¬∀ (M : FloatModel) {P : Program} {fd : FnDef},
        WfProgram P →
          P.fns[0]? = some fd →
            ∀ (fuel : Nat),
              run M.toFloatOps P fuel = EvalRes.outOfFuel ∨
                (∃ k tr, run M.toFloatOps P fuel = EvalRes.panic k tr) ∨
                  ∃ H v tr, run M.toFloatOps P fuel = EvalRes.ok H v tr ∧ HasTy P.decls v fd.ret := by
  intro h
  obtain ⟨hwf, -, ⟨fd, hfd, -, hn⟩, hr⟩ := Spine.Sharp.entry_param _ rfl
  exact hn (h Float.exactModel hwf hfd 200)

/-- `Sharp.entry_param` refutes `no_violation` without hypothesis 1 (helper). -/
theorem entry_param.no_violation_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat) (w : Violation),
        run M.toFloatOps P fuel ≠ EvalRes.stuck w := by
  intro h
  obtain ⟨hwf, -, ⟨fd, hfd, -, hn⟩, hr⟩ := Spine.Sharp.entry_param _ rfl
  exact h Float.exactModel 200 _ hr

/-- `Sharp.copy` refutes `no_violation` without hypothesis 1 (helper). -/
theorem copy.no_violation_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat) (w : Violation),
        run M.toFloatOps P fuel ≠ EvalRes.stuck w := by
  intro h
  obtain ⟨-, -, hr⟩ := Spine.Sharp.copy _ rfl _ rfl
  exact h Float.exactModel 200 _ hr

/-- `Sharp.leak` refutes `no_linear_leak` without hypothesis 1 (helper). -/
theorem leak.no_linear_leak_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat),
        run M.toFloatOps P fuel ≠ EvalRes.stuck Violation.linearLeak := by
  intro h
  obtain ⟨-, -, hr, H, φ, v, tr, hs, hn⟩ := Spine.Sharp.leak _ rfl _ rfl
  exact h Float.exactModel 200 hr

/-- `Sharp.leak` refutes `eval_complete` without hypothesis 1 (helper). -/
theorem leak.eval_complete_1 :
    ¬∀ (M : FloatModel) {P : Program},
        (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
            Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
              ∃ n, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.ok H v tr) ∧
          ∀ (κ : PanicKind) (tr : List Event),
            Steps M.toFloatOps P Config.init (Config.panic κ tr) →
              ∃ n, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.panic κ tr := by
  intro h
  obtain ⟨-, -, hr, H, φ, v, tr, hs, hn⟩ := Spine.Sharp.leak _ rfl _ rfl
  exact hn ((h Float.exactModel).1 H φ v tr hs)

/-- `Sharp.overwrite` refutes `no_linear_overwrite` without hypothesis 1 (helper). -/
theorem overwrite.no_linear_overwrite_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat),
        run M.toFloatOps P fuel ≠ EvalRes.stuck Violation.linearOverwrite := by
  intro h
  obtain ⟨-, -, hr⟩ := Spine.Sharp.overwrite _ rfl _ rfl
  exact h Float.exactModel 200 hr

/-- `Sharp.discard` refutes `no_linear_discard` without hypothesis 1 (helper). -/
theorem discard.no_linear_discard_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat),
        run M.toFloatOps P fuel ≠ EvalRes.stuck Violation.linearDiscard := by
  intro h
  obtain ⟨-, -, hr, κ, tr, hs, hn⟩ := Spine.Sharp.discard _ rfl _ rfl
  exact h Float.exactModel 200 hr

/-- `Sharp.discard` refutes `eval_complete` without hypothesis 1 (helper). -/
theorem discard.eval_complete_1 :
    ¬∀ (M : FloatModel) {P : Program},
        (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
            Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
              ∃ n, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.ok H v tr) ∧
          ∀ (κ : PanicKind) (tr : List Event),
            Steps M.toFloatOps P Config.init (Config.panic κ tr) →
              ∃ n, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.panic κ tr := by
  intro h
  obtain ⟨-, -, hr, κ, tr, hs, hn⟩ := Spine.Sharp.discard _ rfl _ rfl
  exact hn ((h Float.exactModel).2 κ tr hs)

/-- `Sharp.discard_loop` refutes `no_linear_discard` without hypothesis 1 (helper). -/
theorem discard_loop.no_linear_discard_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat),
        run M.toFloatOps P fuel ≠ EvalRes.stuck Violation.linearDiscard := by
  intro h
  obtain ⟨-, -, hr, -, -, hdiv, hns⟩ := Spine.Sharp.discard_loop _ rfl _ rfl
  exact h Float.exactModel 200 hr

/-- `Sharp.discard_loop` refutes `never_stuck_iff` without hypothesis 1 (helper). -/
theorem discard_loop.never_stuck_iff_1 :
    ¬∀ (M : FloatModel) {P : Program},
        (∀ (fuel : Nat) (w : Violation), run M.toFloatOps P fuel ≠ EvalRes.stuck w) ↔
          ∀ (C : RueCore.Config),
            Steps M.toFloatOps P Config.init C → C.Terminal ∨ ∃ C', Step M.toFloatOps P C C' := by
  intro h
  obtain ⟨-, -, hr, -, -, hdiv, hns⟩ := Spine.Sharp.discard_loop _ rfl _ rfl
  exact hns (h Float.exactModel)

/-- `Sharp.discard_loop` refutes `eval_diverges_iff` without hypothesis 1 (helper). -/
theorem discard_loop.eval_diverges_iff_1 :
    ¬∀ (M : FloatModel) {P : Program},
        (∀ (fuel : Nat), run M.toFloatOps P fuel = EvalRes.outOfFuel) ↔
          ∀ (n : Nat), ∃ D, StepsN M.toFloatOps P n Config.init D := by
  intro h
  obtain ⟨-, -, hr, -, -, hdiv, hns⟩ := Spine.Sharp.discard_loop _ rfl _ rfl
  exact hdiv (h Float.exactModel)

/-- `Sharp.fuel` refutes `fuel_mono` without hypothesis 1 (helper). -/
theorem fuel.fuel_mono_1 :
    ¬∀ (M : FloatOps) {P : Program} {H : Store} {φ : Frame} {e : RueCore.Expr} {n m : Nat},
        eval M n P H φ e ≠ EvalRes.outOfFuel → eval M m P H φ e = eval M n P H φ e := by
  intro h
  obtain ⟨hPT, hrun, -, H, v, tr, hr, hs, -, -, hne, hns, hn⟩ := Spine.Sharp.fuel _ rfl _ rfl
  have k := h Float.exactOps (P := progDtor) (H := []) (φ := Frame.empty) (e := .call 0 []) (n := 200) (m := 0)
    (by rw [← hrun, hr]; intro h; cases h)
  rw [← hrun, ← hrun] at k
  exact hne k

/-- `Sharp.fuel` refutes `fuel_mono` without hypothesis 2 (helper). -/
theorem fuel.fuel_mono_2 :
    ¬∀ (M : FloatOps) {P : Program} {H : Store} {φ : Frame} {e : RueCore.Expr} {n m : Nat},
        n ≤ m → eval M m P H φ e = eval M n P H φ e := by
  intro h
  obtain ⟨hPT, hrun, -, H, v, tr, hr, hs, -, -, hne, hns, hn⟩ := Spine.Sharp.fuel _ rfl _ rfl
  have k := h Float.exactOps (P := progDtor) (H := []) (φ := Frame.empty) (e := .call 0 []) (Nat.zero_le 200)
  rw [← hrun, ← hrun] at k
  exact hne k.symm

/-- `Sharp.fuel` refutes `no_masking` without hypothesis 1 (helper). -/
theorem fuel.no_masking_1 :
    ¬∀ (M : FloatOps) {P : Program} {H : Store} {φ : Frame} {e : RueCore.Expr} {_n : Nat} {m : Nat}
        {w : Violation}, eval M m P H φ e ≠ EvalRes.outOfFuel → eval M m P H φ e = EvalRes.stuck w := by
  intro h
  obtain ⟨hPT, hrun, -, H, v, tr, hr, hs, -, -, hne, hns, hn⟩ := Spine.Sharp.fuel _ rfl _ rfl
  have k := h Float.exactOps (P := progDtor) (H := []) (φ := Frame.empty) (e := .call 0 []) (_n := 0) (m := 200)
    (w := .unbound) (by rw [← hrun, hr]; intro h; cases h)
  rw [← hrun] at k
  exact hns _ k

/-- `Sharp.fuel` refutes `eval_complete` without hypothesis 3 (helper). -/
theorem fuel.eval_complete_3 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
                ∃ _n : Nat, ∀ (fuel : Nat), run M.toFloatOps P fuel = EvalRes.ok H v tr) ∧
            ∀ (κ : PanicKind) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.panic κ tr) →
                ∃ n : Nat, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.panic κ tr := by
  intro h
  obtain ⟨hPT, hrun, -, H, v, tr, hr, hs, -, -, hne, hns, hn⟩ := Spine.Sharp.fuel _ rfl _ rfl
  obtain ⟨_, hk⟩ := (h Float.exactModel hPT).1 _ _ _ _ hs
  exact hn fun f => .inl (hk f)

/-- `Sharp.fuel` refutes `run_complete` without hypothesis 2 (helper). -/
theorem fuel.run_complete_2 :
    ¬∀ (M : FloatOps) (P : Program),
        (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
            Steps M P Config.init (Config.run H φ [] (Focus.ret v) tr) →
              ∃ _n : Nat,
                ∀ (fuel : Nat),
                  run M P fuel = EvalRes.ok H v tr ∨ ∃ w, run M P fuel = EvalRes.stuck w) ∧
          ∀ (κ : PanicKind) (tr : List Event),
            Steps M P Config.init (Config.panic κ tr) →
              ∃ n : Nat,
                ∀ (fuel : Nat),
                  n < fuel → run M P fuel = EvalRes.panic κ tr ∨ ∃ w, run M P fuel = EvalRes.stuck w := by
  intro h
  obtain ⟨hPT, hrun, -, H, v, tr, hr, hs, -, -, hne, hns, hn⟩ := Spine.Sharp.fuel _ rfl _ rfl
  obtain ⟨_, hk⟩ := (h Float.exactOps _).1 _ _ _ _ hs
  exact hn hk

/-- `Sharp.fuel_panic` refutes `eval_complete` without hypothesis 5 (helper). -/
theorem fuel_panic.eval_complete_5 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
                ∃ n : Nat, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.ok H v tr) ∧
            ∀ (κ : PanicKind) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.panic κ tr) →
                ∃ _n : Nat, ∀ (fuel : Nat), run M.toFloatOps P fuel = EvalRes.panic κ tr := by
  intro h
  obtain ⟨hPT, -, hs, hn⟩ := Spine.Sharp.fuel_panic _ rfl _ rfl
  obtain ⟨_, hk⟩ := (h Float.exactModel hPT).2 _ _ hs
  exact hn fun f => .inl (hk f)

/-- `Sharp.fuel_panic` refutes `run_complete` without hypothesis 4 (helper). -/
theorem fuel_panic.run_complete_4 :
    ¬∀ (M : FloatOps) (P : Program),
        (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
            Steps M P Config.init (Config.run H φ [] (Focus.ret v) tr) →
              ∃ n : Nat,
                ∀ (fuel : Nat),
                  n < fuel → run M P fuel = EvalRes.ok H v tr ∨ ∃ w, run M P fuel = EvalRes.stuck w) ∧
          ∀ (κ : PanicKind) (tr : List Event),
            Steps M P Config.init (Config.panic κ tr) →
              ∃ _n : Nat,
                ∀ (fuel : Nat), run M P fuel = EvalRes.panic κ tr ∨ ∃ w, run M P fuel = EvalRes.stuck w := by
  intro h
  obtain ⟨hPT, -, hs, hn⟩ := Spine.Sharp.fuel_panic _ rfl _ rfl
  obtain ⟨_, hk⟩ := (h Float.exactOps _).2 _ _ hs
  exact hn hk

/-- `Sharp.not_fits` refutes `check_sound` without hypothesis 2 (helper). -/
theorem not_fits.check_sound_2 :
    ¬∀ {P : Program} {R : Ty} (e : RueCore.Expr) {Γ : Ctx} {c : CTy} {Ω : Out},
        RueCore.check P R Γ e = some (c, Ω) → ∀ (T : Ty), Typed P R Γ e T Ω := by
  intro h
  obtain ⟨-, c, Ω, hc, -, hn⟩ := Spine.Sharp.not_fits _ rfl _ rfl
  exact hn (h _ hc .bool)

/-- `Sharp.double_drop` refutes `no_double_free` without hypothesis 1 (helper). -/
theorem double_drop.no_double_free_1 :
    ¬∀ (M : FloatModel) {P : Program} (fuel : Nat),
        (∀ (w : Violation), run M.toFloatOps P fuel ≠ EvalRes.stuck w) ∧
          (∀ (a : Nat), List.count a (freedIds P.decls (run M.toFloatOps P fuel).trace) ≤ 1) ∧
            ∀ (a : Nat), List.count a (dtorIds (run M.toFloatOps P fuel).trace) ≤ 1 := by
  intro h
  obtain ⟨-, -, -, H, v, tr, -, -, hn⟩ := Spine.Sharp.double_drop _ rfl _ rfl
  exact hn (h Float.exactModel 200).2.2

/-- `Sharp.double_drop` refutes `step_no_double_free` without hypothesis 1 (helper). -/
theorem double_drop.step_no_double_free_1 :
    ¬∀ (M : FloatModel) {P : Program} {C : RueCore.Config},
        Steps M.toFloatOps P Config.init C →
          (∀ (a : Nat), List.count a (freedIds P.decls C.trace) ≤ 1) ∧
            ∀ (a : Nat), List.count a (dtorIds C.trace) ≤ 1 := by
  intro h
  obtain ⟨-, -, -, H, v, tr, hr, hc, -⟩ := Spine.Sharp.double_drop _ rfl _ rfl
  have := (h Float.exactModel ((Spine.run_sim Float.exactOps _ 200).1 _ _ _ hr)).2 0
  simp only [Config.trace] at this
  omega

/-- `Sharp.double_drop` refutes `dtor_once` without hypothesis 1 (helper). -/
theorem double_drop.dtor_once_1 :
    ¬∀ (M : FloatOps) {P : Program} (fuel a : Nat), List.count a (dtorIds (run M P fuel).trace) ≤ 1 := by
  intro h
  obtain ⟨-, -, -, H, v, tr, -, -, hn⟩ := Spine.Sharp.double_drop _ rfl _ rfl
  exact hn (h Float.exactOps 200)

/-- `Sharp.bare_dtor` refutes `drop_order` without hypothesis 1 (helper). -/
theorem bare_dtor.drop_order_1 :
    ¬∀ (M : FloatModel) {P : Program},
        (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
            Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) → Blocks P.decls tr) ∧
          (∀ (κ : PanicKind) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.panic κ tr) → Blocks P.decls tr) ∧
            ∀ (C C' : RueCore.Config),
              Steps M.toFloatOps P Config.init C →
                Step M.toFloatOps P C C' →
                  ∃ evs,
                    C'.trace = C.trace ++ evs ∧
                      NewestFirst (dropLocs evs) ∧
                        Lifo C.stack C'.stack (dropLocs evs) ∧
                          List.Pairwise (fun x1 x2 => x1 < x2) C.stack := by
  intro h
  obtain ⟨-, -, H, φ, v, tr, hs, hn⟩ := Spine.Sharp.bare_dtor _ rfl _ rfl
  exact hn ((h Float.exactModel).1 _ _ _ _ hs)

/-- `Sharp.bare_dtor` refutes `drop_glue_order` without hypothesis 1: a trace
outside `Blocks` is outside `GlueBlocks` (helper). -/
theorem bare_dtor.drop_glue_order_1 :
    ¬∀ (M : FloatModel) {P : Program},
        (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
            Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
              GlueBlocks P.decls tr) ∧
          ∀ (κ : PanicKind) (tr : List Event),
            Steps M.toFloatOps P Config.init (Config.panic κ tr) → GlueBlocks P.decls tr := by
  intro h
  obtain ⟨-, -, H, φ, v, tr, hs, hn⟩ := Spine.Sharp.bare_dtor _ rfl _ rfl
  exact hn ((h Float.exactModel).1 _ _ _ _ hs).toBlocks

/-- `Sharp.pending_program` refutes `drop_exactly_once` without hypothesis 2 (helper). -/
theorem pending_program.drop_exactly_once_2 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
            {H : Store},
            Typed P R Γ e T Ω →
              FrameMatches P.decls Γ φ H →
                StoreCC P.decls H →
                  e.pendingSafe = true →
                    (∀ (w : Violation), eval M.toFloatOps fuel P H φ e ≠ EvalRes.stuck w) ∧
                      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) ∧
                        Tidy φ H (eval M.toFloatOps fuel P H φ e) := by
  intro h
  obtain ⟨hPT, -, heps, hfm, hcc, ⟨c, Ω, -, -, ht⟩, hl, heq, hn200, hn201⟩ :=
    Spine.Sharp.pending_program _ rfl _ rfl
  exact hn200 (h Float.exactModel hPT (fuel := 200) ht hfm hcc heps).2.1

/-- `Sharp.pending_program` refutes `rest_exactly_once` without hypothesis 2 (helper). -/
theorem pending_program.rest_exactly_once_2 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
            {H : Store},
            Typed P R Γ e T Ω →
              FrameMatches P.decls Γ φ H →
                StoreCC P.decls H →
                  e.pendingSafe = true →
                    ∀ {H₁ : Store} {vs : List Val} {tr : List Event},
                      Lead M.toFloatOps P fuel H φ H₁ vs tr e →
                        ∀ {r : EvalRes},
                          eval M.toFloatOps (fuel + 1) P H φ e = EvalRes.withTrace tr r →
                            (∀ (w : Violation), r ≠ EvalRes.stuck w) ∧
                              Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧
                                Settled φ H₁ r := by
  intro h
  obtain ⟨hPT, -, heps, hfm, hcc, ⟨c, Ω, -, -, ht⟩, hl, heq, hn200, hn201⟩ :=
    Spine.Sharp.pending_program _ rfl _ rfl
  exact hn201 (h Float.exactModel hPT ht hfm hcc heps hl heq).2.1

/-- `Sharp.pending_expr` refutes `drop_exactly_once` without hypothesis 6 (helper). -/
theorem pending_expr.drop_exactly_once_6 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
              {H : Store},
              Typed P R Γ e T Ω →
                FrameMatches P.decls Γ φ H →
                  StoreCC P.decls H →
                    (∀ (w : Violation), eval M.toFloatOps fuel P H φ e ≠ EvalRes.stuck w) ∧
                      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) ∧
                        Tidy φ H (eval M.toFloatOps fuel P H φ e) := by
  intro h
  obtain ⟨hPT, hps, -, hfm, hcc, ⟨c, Ω, -, -, ht⟩, hl, heq, hn200, hn201⟩ :=
    Spine.Sharp.pending_expr _ rfl _ rfl _ rfl
  exact hn200 (h Float.exactModel hPT hps (fuel := 200) ht hfm hcc).2.1

/-- `Sharp.pending_expr` refutes `rest_exactly_once` without hypothesis 6 (helper). -/
theorem pending_expr.rest_exactly_once_6 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
              {H : Store},
              Typed P R Γ e T Ω →
                FrameMatches P.decls Γ φ H →
                  StoreCC P.decls H →
                    ∀ {H₁ : Store} {vs : List Val} {tr : List Event},
                      Lead M.toFloatOps P fuel H φ H₁ vs tr e →
                        ∀ {r : EvalRes},
                          eval M.toFloatOps (fuel + 1) P H φ e = EvalRes.withTrace tr r →
                            (∀ (w : Violation), r ≠ EvalRes.stuck w) ∧
                              Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧
                                Settled φ H₁ r := by
  intro h
  obtain ⟨hPT, hps, -, hfm, hcc, ⟨c, Ω, -, -, ht⟩, hl, heq, hn200, hn201⟩ :=
    Spine.Sharp.pending_expr _ rfl _ rfl _ rfl
  exact hn201 (h Float.exactModel hPT hps ht hfm hcc hl heq).2.1

/-- `Sharp.store_cc` refutes `drop_exactly_once` without hypothesis 5 (helper). -/
theorem store_cc.drop_exactly_once_5 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
              {H : Store},
              Typed P R Γ e T Ω →
                FrameMatches P.decls Γ φ H →
                  e.pendingSafe = true →
                    (∀ (w : Violation), eval M.toFloatOps fuel P H φ e ≠ EvalRes.stuck w) ∧
                      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) ∧
                        Tidy φ H (eval M.toFloatOps fuel P H φ e) := by
  intro h
  obtain ⟨hPT, hps, heps, hfm, -, ⟨c, Ω, -, -, ht⟩, hl, heq, hn200, hn201⟩ :=
    Spine.Sharp.store_cc _ rfl _ rfl _ rfl
  exact hn200 (h Float.exactModel hPT hps (fuel := 200) ht hfm heps).2.1

/-- `Sharp.store_cc` refutes `rest_exactly_once` without hypothesis 5 (helper). -/
theorem store_cc.rest_exactly_once_5 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
              {H : Store},
              Typed P R Γ e T Ω →
                FrameMatches P.decls Γ φ H →
                  e.pendingSafe = true →
                    ∀ {H₁ : Store} {vs : List Val} {tr : List Event},
                      Lead M.toFloatOps P fuel H φ H₁ vs tr e →
                        ∀ {r : EvalRes},
                          eval M.toFloatOps (fuel + 1) P H φ e = EvalRes.withTrace tr r →
                            (∀ (w : Violation), r ≠ EvalRes.stuck w) ∧
                              Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧
                                Settled φ H₁ r := by
  intro h
  obtain ⟨hPT, hps, heps, hfm, -, ⟨c, Ω, -, -, ht⟩, hl, heq, hn200, hn201⟩ :=
    Spine.Sharp.store_cc _ rfl _ rfl _ rfl
  exact hn201 (h Float.exactModel hPT hps ht hfm heps hl heq).2.1

/-- `Sharp.no_lead` refutes `rest_exactly_once` without hypothesis 7 (helper). -/
theorem no_lead.rest_exactly_once_7 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
              {H : Store},
              Typed P R Γ e T Ω →
                FrameMatches P.decls Γ φ H →
                  StoreCC P.decls H →
                    e.pendingSafe = true →
                      ∀ {H₁ : Store} {vs : List Val} {tr : List Event} {r : EvalRes},
                        eval M.toFloatOps (fuel + 1) P H φ e = EvalRes.withTrace tr r →
                          (∀ (w : Violation), r ≠ EvalRes.stuck w) ∧
                            Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧
                              Settled φ H₁ r := by
  intro h
  obtain ⟨hPT, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht⟩, -, heq, hn⟩ := Spine.Sharp.no_lead _ rfl _ rfl
  exact hn (h Float.exactModel hPT hps (fuel := 200) ht hfm hcc heps heq).2.1

/-- `Sharp.no_eval` refutes `rest_exactly_once` without hypothesis 8 (helper). -/
theorem no_eval.rest_exactly_once_8 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          P.pendingSafe = true →
            ∀ {fuel : Nat} {R : Ty} {Γ : Ctx} {e : RueCore.Expr} {T : Ty} {Ω : Out} {φ : Frame}
              {H : Store},
              Typed P R Γ e T Ω →
                FrameMatches P.decls Γ φ H →
                  StoreCC P.decls H →
                    e.pendingSafe = true →
                      ∀ {H₁ : Store} {vs : List Val} {tr : List Event},
                        Lead M.toFloatOps P fuel H φ H₁ vs tr e →
                          ∀ {r : EvalRes},
                            (∀ (w : Violation), r ≠ EvalRes.stuck w) ∧
                              Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧
                                Settled φ H₁ r := by
  intro h
  obtain ⟨hPT, hps, heps, hfm, hcc, ⟨c, Ω, -, -, ht⟩, H₁, vs, tr, hl, -⟩ := Spine.Sharp.no_eval _ rfl _ rfl
  exact (h Float.exactModel hPT hps ht hfm hcc heps hl (r := .stuck .unbound)).1 _ rfl

/-- `Sharp.unreached` refutes `drop_order` without hypothesis 2 (helper). -/
theorem unreached.drop_order_2 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (_H : Store) (_φ : Frame) (_v : Val) (tr : List Event), Blocks P.decls tr) ∧
            (∀ (κ : PanicKind) (tr : List Event),
                Steps M.toFloatOps P Config.init (Config.panic κ tr) → Blocks P.decls tr) ∧
              ∀ (C C' : RueCore.Config),
                Steps M.toFloatOps P Config.init C →
                  Step M.toFloatOps P C C' →
                    ∃ evs,
                      C'.trace = C.trace ++ evs ∧
                        NewestFirst (dropLocs evs) ∧
                          Lifo C.stack C'.stack (dropLocs evs) ∧
                            List.Pairwise (fun x1 x2 => x1 < x2) C.stack := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached _ rfl _ rfl
  exact hnb ((h Float.exactModel hPT).1 [] Frame.empty (.int .w64 .signed 8) _)

/-- `Sharp.unreached` refutes `drop_glue_order` without hypothesis 2 (helper). -/
theorem unreached.drop_glue_order_2 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (_H : Store) (_φ : Frame) (_v : Val) (tr : List Event), GlueBlocks P.decls tr) ∧
            ∀ (κ : PanicKind) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.panic κ tr) → GlueBlocks P.decls tr := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached _ rfl _ rfl
  exact hnb ((h Float.exactModel hPT).1 [] Frame.empty (.int .w64 .signed 8) _).toBlocks

/-- `Sharp.unreached` refutes `eval_sound` without hypothesis 2 (helper). -/
theorem unreached.eval_sound_2 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          ∀ (fuel : Nat),
            (∀ (w : Violation), run M.toFloatOps P fuel ≠ EvalRes.stuck w) ∧
              (∀ (H : Store) (v : Val) (tr : List Event),
                  Steps M.toFloatOps P Config.init (Config.run H Frame.empty [] (Focus.ret v) tr)) ∧
                ∀ (k : PanicKind) (tr : List Event),
                  run M.toFloatOps P fuel = EvalRes.panic k tr →
                    Steps M.toFloatOps P Config.init (Config.panic k tr) := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached _ rfl _ rfl
  exact hns ((h Float.exactModel hPT 200).2.1 [] (.int .w64 .signed 8) _)

/-- `Sharp.unreached` refutes `run_sim` without hypothesis 1 (helper). -/
theorem unreached.run_sim_1 :
    ¬∀ (M : FloatOps) (P : Program) (fuel : Nat),
        (∀ (H : Store) (v : Val) (tr : List Event),
            Steps M P Config.init (Config.run H Frame.empty [] (Focus.ret v) tr)) ∧
          ∀ (k : PanicKind) (tr : List Event),
            run M P fuel = EvalRes.panic k tr → Steps M P Config.init (Config.panic k tr) := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached _ rfl _ rfl
  exact hns ((h Float.exactOps _ 200).1 [] (.int .w64 .signed 8) _)

/-- `Sharp.unreached` refutes `eval_complete` without hypothesis 2 (helper). -/
theorem unreached.eval_complete_2 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (H : Store) (_φ : Frame) (v : Val) (tr : List Event),
              ∃ n, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.ok H v tr) ∧
            ∀ (κ : PanicKind) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.panic κ tr) →
                ∃ n, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.panic κ tr := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached _ rfl _ rfl
  obtain ⟨n, hk⟩ := (h Float.exactModel hPT).1 [] Frame.empty (.int .w64 .signed 8)
    [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])]
  exact hn ⟨n, fun f hf => .inl (hk f hf)⟩

/-- `Sharp.unreached` refutes `run_complete` without hypothesis 1 (helper). -/
theorem unreached.run_complete_1 :
    ¬∀ (M : FloatOps) (P : Program),
        (∀ (H : Store) (_φ : Frame) (v : Val) (tr : List Event),
            ∃ n,
              ∀ (fuel : Nat),
                n < fuel → run M P fuel = EvalRes.ok H v tr ∨ ∃ w, run M P fuel = EvalRes.stuck w) ∧
          ∀ (κ : PanicKind) (tr : List Event),
            Steps M P Config.init (Config.panic κ tr) →
              ∃ n,
                ∀ (fuel : Nat),
                  n < fuel → run M P fuel = EvalRes.panic κ tr ∨ ∃ w, run M P fuel = EvalRes.stuck w := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached _ rfl _ rfl
  exact hn ((h Float.exactOps _).1 [] Frame.empty (.int .w64 .signed 8) _)

/-- `Sharp.unreached_panic` refutes `drop_order` without hypothesis 3 (helper). -/
theorem unreached_panic.drop_order_3 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
                Blocks P.decls tr) ∧
            (∀ (_κ : PanicKind) (tr : List Event), Blocks P.decls tr) ∧
              ∀ (C C' : RueCore.Config),
                Steps M.toFloatOps P Config.init C →
                  Step M.toFloatOps P C C' →
                    ∃ evs,
                      C'.trace = C.trace ++ evs ∧
                        NewestFirst (dropLocs evs) ∧
                          Lifo C.stack C'.stack (dropLocs evs) ∧
                            List.Pairwise (fun x1 x2 => x1 < x2) C.stack := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached_panic _ rfl _ rfl
  exact hnb ((h Float.exactModel hPT).2.1 .user _)

/-- `Sharp.unreached_panic` refutes `drop_glue_order` without hypothesis 3 (helper). -/
theorem unreached_panic.drop_glue_order_3 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
                GlueBlocks P.decls tr) ∧
            ∀ (_κ : PanicKind) (tr : List Event), GlueBlocks P.decls tr := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached_panic _ rfl _ rfl
  exact hnb ((h Float.exactModel hPT).2 .user _).toBlocks

/-- `Sharp.unreached_panic` refutes `eval_sound` without hypothesis 3 (helper). -/
theorem unreached_panic.eval_sound_3 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          ∀ (fuel : Nat),
            (∀ (w : Violation), run M.toFloatOps P fuel ≠ EvalRes.stuck w) ∧
              (∀ (H : Store) (v : Val) (tr : List Event),
                  run M.toFloatOps P fuel = EvalRes.ok H v tr →
                    Steps M.toFloatOps P Config.init (Config.run H Frame.empty [] (Focus.ret v) tr)) ∧
                ∀ (k : PanicKind) (tr : List Event),
                  Steps M.toFloatOps P Config.init (Config.panic k tr) := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached_panic _ rfl _ rfl
  exact hns ((h Float.exactModel hPT 200).2.2 .user _)

/-- `Sharp.unreached_panic` refutes `run_sim` without hypothesis 2 (helper). -/
theorem unreached_panic.run_sim_2 :
    ¬∀ (M : FloatOps) (P : Program) (fuel : Nat),
        (∀ (H : Store) (v : Val) (tr : List Event),
            run M P fuel = EvalRes.ok H v tr →
              Steps M P Config.init (Config.run H Frame.empty [] (Focus.ret v) tr)) ∧
          ∀ (k : PanicKind) (tr : List Event), Steps M P Config.init (Config.panic k tr) := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached_panic _ rfl _ rfl
  exact hns ((h Float.exactOps _ 200).2 .user _)

/-- `Sharp.unreached_panic` refutes `eval_complete` without hypothesis 4 (helper). -/
theorem unreached_panic.eval_complete_4 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
                ∃ n, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.ok H v tr) ∧
            ∀ (κ : PanicKind) (tr : List Event),
              ∃ n, ∀ (fuel : Nat), n < fuel → run M.toFloatOps P fuel = EvalRes.panic κ tr := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached_panic _ rfl _ rfl
  obtain ⟨n, hk⟩ := (h Float.exactModel hPT).2 .user [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])]
  exact hn ⟨n, fun f hf => .inl (hk f hf)⟩

/-- `Sharp.unreached_panic` refutes `run_complete` without hypothesis 3 (helper). -/
theorem unreached_panic.run_complete_3 :
    ¬∀ (M : FloatOps) (P : Program),
        (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
            Steps M P Config.init (Config.run H φ [] (Focus.ret v) tr) →
              ∃ n,
                ∀ (fuel : Nat),
                  n < fuel → run M P fuel = EvalRes.ok H v tr ∨ ∃ w, run M P fuel = EvalRes.stuck w) ∧
          ∀ (κ : PanicKind) (tr : List Event),
            ∃ n,
              ∀ (fuel : Nat),
                n < fuel → run M P fuel = EvalRes.panic κ tr ∨ ∃ w, run M P fuel = EvalRes.stuck w := by
  intro h
  obtain ⟨hPT, hns, -, hnb, hn⟩ := Spine.Sharp.unreached_panic _ rfl _ rfl
  exact hn ((h Float.exactOps _).2 .user _)

/-- `Sharp.unordered` refutes `drop_order` without hypothesis 4 (helper). -/
theorem unordered.drop_order_4 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
                Blocks P.decls tr) ∧
            (∀ (κ : PanicKind) (tr : List Event),
                Steps M.toFloatOps P Config.init (Config.panic κ tr) → Blocks P.decls tr) ∧
              ∀ (C C' : RueCore.Config),
                Step M.toFloatOps P C C' →
                  ∃ evs,
                    C'.trace = C.trace ++ evs ∧
                      NewestFirst (dropLocs evs) ∧
                        Lifo C.stack C'.stack (dropLocs evs) ∧
                          List.Pairwise (fun x1 x2 => x1 < x2) C.stack := by
  intro h
  obtain ⟨hPT, -, hs, hn⟩ := Spine.Sharp.unordered _ rfl _ rfl
  exact hn ((h Float.exactModel hPT).2.2 _ _ hs)

/-- `Sharp.not_a_step` refutes `drop_order` without hypothesis 5 (helper). -/
theorem not_a_step.drop_order_5 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          (∀ (H : Store) (φ : Frame) (v : Val) (tr : List Event),
              Steps M.toFloatOps P Config.init (Config.run H φ [] (Focus.ret v) tr) →
                Blocks P.decls tr) ∧
            (∀ (κ : PanicKind) (tr : List Event),
                Steps M.toFloatOps P Config.init (Config.panic κ tr) → Blocks P.decls tr) ∧
              ∀ (C C' : RueCore.Config),
                Steps M.toFloatOps P Config.init C →
                  ∃ evs,
                    C'.trace = C.trace ++ evs ∧
                      NewestFirst (dropLocs evs) ∧
                        Lifo C.stack C'.stack (dropLocs evs) ∧
                          List.Pairwise (fun x1 x2 => x1 < x2) C.stack := by
  intro h
  obtain ⟨hPT, H, v, tr, hs, -, -, hn⟩ := Spine.Sharp.not_a_step _ rfl _ rfl
  exact hn ((h Float.exactModel hPT).2.2 _ (.panic .user []) hs)

/-- `Sharp.init_steps` refutes `Step.det` without hypothesis 1 (helper). -/
theorem init_steps.Step.det_1 :
    ¬∀ {M : FloatOps} {P : Program} {C C₁ C₂ : RueCore.Config}, Step M P C C₂ → C₁ = C₂ := by
  intro h
  obtain ⟨-, hs, -, hne, -, -, hf, hinit, hn⟩ := Spine.Sharp.init_steps _ rfl _ rfl
  exact hne (h (C₁ := Config.init) hs).symm

/-- `Sharp.init_steps` refutes `Step.det` without hypothesis 2 (helper). -/
theorem init_steps.Step.det_2 :
    ¬∀ {M : FloatOps} {P : Program} {C C₁ C₂ : RueCore.Config}, Step M P C C₁ → C₁ = C₂ := by
  intro h
  obtain ⟨-, hs, -, hne, -, -, hf, hinit, hn⟩ := Spine.Sharp.init_steps _ rfl _ rfl
  exact hne (h (C₂ := Config.init) hs)

/-- `Sharp.init_steps` refutes `Step.terminal` without hypothesis 1 (helper). -/
theorem init_steps.Step.terminal_1 :
    ¬∀ {M : FloatOps} {P : Program} {C C' : RueCore.Config}, ¬Step M P C C' := by
  intro h
  obtain ⟨-, hs, -, hne, -, -, hf, hinit, hn⟩ := Spine.Sharp.init_steps _ rfl _ rfl
  exact h hs

/-- `Sharp.init_steps` refutes `step_stuck_isStuckState` without hypothesis 1 (helper). -/
theorem init_steps.step_stuck_isStuckState_1 :
    ¬∀ {_M : FloatOps} {_P : Program} {_C : RueCore.Config} {w : Violation}, w.isStuckState = true := by
  intro h
  obtain ⟨-, hs, -, hne, -, -, hf, hinit, hn⟩ := Spine.Sharp.init_steps _ rfl _ rfl
  have := @h Float.exactOps progDtor Config.init .linearLeak
  rw [hf] at this
  cases this

/-- `Sharp.init_steps` refutes `run_stuck_of_step_stuck` without hypothesis 2 (helper). -/
theorem init_steps.run_stuck_of_step_stuck_2 :
    ¬∀ (M : FloatOps) (P : Program) {C : RueCore.Config} {_w : Violation},
        Steps M P Config.init C → ∃ n, ∀ (fuel : Nat), n < fuel → ∃ w', run M P fuel = EvalRes.stuck w' := by
  intro h
  obtain ⟨-, hs, -, hne, -, -, hf, hinit, hn⟩ := Spine.Sharp.init_steps _ rfl _ rfl
  exact hn (h _ _ (_w := .unbound) hinit)

/-- `Sharp.unreachable_stuck` refutes `step_progress` without hypothesis 2 (helper). -/
theorem unreachable_stuck.step_progress_2 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P → ∀ (C : RueCore.Config), C.Terminal ∨ ∃ C', Step M.toFloatOps P C C' := by
  intro h
  obtain ⟨hPT, hst, -, hnr, -, hns, hall, hn⟩ := Spine.Sharp.unreachable_stuck _ rfl _ rfl
  exact hall (h Float.exactModel hPT)

/-- `Sharp.unreachable_stuck` refutes `step_preservation` without hypothesis 2 (helper). -/
theorem unreachable_stuck.step_preservation_2 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          ∃ fd, P.fns[0]? = some fd ∧ ∀ (C : RueCore.Config), Config.SafeAt M.toFloatOps P fd.ret C := by
  intro h
  obtain ⟨hPT, hst, -, hnr, -, hns, hall, hn⟩ := Spine.Sharp.unreachable_stuck _ rfl _ rfl
  obtain ⟨_, -, hs⟩ := h Float.exactModel hPT
  exact hns _ (hs _)

/-- `Sharp.unreachable_stuck` refutes `never_stuck_iff` without hypothesis 2 (helper). -/
theorem unreachable_stuck.never_stuck_iff_2 :
    ¬∀ (M : FloatModel) {P : Program},
        ProgramTyped P →
          ((∀ (fuel : Nat) (w : Violation), run M.toFloatOps P fuel ≠ EvalRes.stuck w) ↔
            ∀ (C : RueCore.Config), C.Terminal ∨ ∃ C', Step M.toFloatOps P C C') := by
  intro h
  obtain ⟨hPT, hst, -, hnr, -, hns, hall, hn⟩ := Spine.Sharp.unreachable_stuck _ rfl _ rfl
  exact hall ((h Float.exactModel hPT).mp hnr)

/-- `Sharp.unreachable_stuck` refutes `step_never_stuck_of_run` without hypothesis 2 (helper). -/
theorem unreachable_stuck.step_never_stuck_of_run_2 :
    ¬∀ (M : FloatOps) (P : Program),
        (∀ (fuel : Nat) (w : Violation), run M P fuel ≠ EvalRes.stuck w) →
          ∀ (C : RueCore.Config), C.Terminal ∨ ∃ C', Step M P C C' := by
  intro h
  obtain ⟨hPT, hst, -, hnr, -, hns, hall, hn⟩ := Spine.Sharp.unreachable_stuck _ rfl _ rfl
  exact hall (h Float.exactOps _ hnr)

/-- `Sharp.unreachable_stuck` refutes `run_stuck_of_step_stuck` without hypothesis 1 (helper). -/
theorem unreachable_stuck.run_stuck_of_step_stuck_1 :
    ¬∀ (M : FloatOps) (P : Program) {C : RueCore.Config} {w : Violation},
        Config.Stuck M P C w → ∃ n, ∀ (fuel : Nat), n < fuel → ∃ w', run M P fuel = EvalRes.stuck w' := by
  intro h
  obtain ⟨hPT, hst, -, hnr, -, hns, hall, hn⟩ := Spine.Sharp.unreachable_stuck _ rfl _ rfl
  exact hn (h _ _ hst)

/-- `Sharp.retired_cell` refutes `step_no_use_after_drop` without hypothesis 1 (helper). -/
theorem retired_cell.step_no_use_after_drop_1 :
    ¬∀ (M : FloatOps) (P : Program) {C : RueCore.Config}, ¬C.Stuck M P Violation.useAfterDrop := by
  intro h
  obtain ⟨-, -, hst, -⟩ := Spine.Sharp.retired_cell _ rfl _ rfl
  exact h _ _ hst

/-- `Sharp.unreached_double` refutes `step_no_double_free` without hypothesis 2 (helper). -/
theorem unreached_double.step_no_double_free_2 :
    ¬∀ (_M : FloatModel) {P : Program}, ProgramTyped P → ∀ {C : RueCore.Config},
        (∀ (a : Nat), List.count a (freedIds P.decls C.trace) ≤ 1) ∧
          ∀ (a : Nat), List.count a (dtorIds C.trace) ≤ 1 := by
  intro h
  obtain ⟨hPT, -, hc⟩ := Spine.Sharp.unreached_double _ rfl _ rfl
  have := (h Float.exactModel hPT (C := .panic .user
    [.dtor 0 (.struct 0 0 [.int .w64 .signed 1]), .dtor 0 (.struct 0 0 [.int .w64 .signed 1])])).2 0
  omega

end RueCore.Sharp.Glue
