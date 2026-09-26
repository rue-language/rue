module

public import RueCore.Spine

@[expose] public section

/-!
# RueCore.Nonvacuous.Glue — each witness applied to the theorems it witnesses (layer L2)

`Spec.witnesses` pairs each witness statement with the spine theorems whose
hypotheses it shows satisfiable. This module checks every pair in the kernel:
for each witness, one theorem that takes the witness's facts
(`RueCore.Spine.Nonvacuous.<w>`, with `M` from `Nonvacuous.exact_model` and
the frame from `Nonvacuous.empty_frame`) and **applies** each listed
`RueCore.Spine.<thm>` to them. The exact-model and empty-frame witnesses
have no program of their own: every program witness takes its `M` from
`Nonvacuous.exact_model`, and the evaluation statements their frame from
`Nonvacuous.empty_frame`, so their pairs are applied there. An application elaborates only if the witness
supplies that theorem's literal hypotheses, so a pair listed without them does
not compile; and the lint (`Lint.spineProblems`) fails on a listed pair no
theorem here applies, reading the constants each proof uses. A spine theorem
with no hypotheses (`freed_once`, `run_ne_returned`, `Config.trichotomy`,
`step_iff`, `Config.stuck_iff`) is applied at the witness's program or
configuration; `SPINE.md` marks it so. The programs are the witness
statements' own, abbreviated here (helper module, RUE-2469).
-/

namespace RueCore.Nonvacuous.Glue

/-- The witnesses' declarations: `S0`, affine with a destructor; `S1`,
`linear`; `E0 { K0(S0), K1 }` (helper). -/
abbrev decls : Decls :=
  { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] }

/-- The `dtor` witness's body (helper). -/
abbrev bodyDtor : Expr :=
  .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
    (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2]) (.intLit .w64 .signed 3))

/-- The `dtor` witness's program (helper). -/
abbrev progDtor : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyDtor }] }

/-- The `linear` witness's body (helper). -/
abbrev bodyLinear : Expr :=
  .letIn false (.mkStruct 1 [.intLit .w64 .signed 1])
    (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2])
      (.seq (.drop (.var 1)) (.intLit .w64 .signed 3)))

/-- The `linear` witness's program (helper). -/
abbrev progLinear : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyLinear }] }

/-- The `loop` witness's body (helper). -/
abbrev bodyLoop : Expr :=
  .letIn true (.intLit .w64 .signed 0)
    (.seq
      (.loop
        (.seq (.ite (.binop .ge (.use (.var 0)) (.intLit .w64 .signed 3)) .brk .unitLit)
          (.seq (.assign (.var 0) (.binop .add (.use (.var 0)) (.intLit .w64 .signed 1)))
            (.letIn false (.mkStruct 0 [.use (.var 0)]) .unitLit))))
      (.use (.var 0)))

/-- The `loop` witness's program (helper). -/
abbrev progLoop : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyLoop }] }

/-- The `array` witness's body (helper). -/
abbrev bodyArray : Expr :=
  .letIn false
    (.mkArray (.struct 0)
      [.mkStruct 0 [.intLit .w64 .signed 1], .mkStruct 0 [.intLit .w64 .signed 2]])
    (.indexRead (.var 0) [(.intLit .w64 .signed 1)] [[0]])

/-- The `array` witness's program (helper). -/
abbrev progArray : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyArray }] }

/-- The `enum_match` witness's body (helper). -/
abbrev bodyEnumMatch : Expr :=
  .letIn false (.mkEnum 0 0 [(.mkStruct 0 [.intLit .w64 .signed 1])])
    (.«match» (.use (.var 0)) [.use (.proj (.var 0) 0), (.intLit .w64 .signed 0)])

/-- The `enum_match` witness's program (helper). -/
abbrev progEnumMatch : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyEnumMatch }] }

/-- The `early_return` witness's body (helper). -/
abbrev bodyEarlyReturn : Expr :=
  .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
    (.letIn false (.mkStruct 0 [.intLit .w64 .signed 2])
      (.seq (.ret (.intLit .w64 .signed 7)) (.intLit .w64 .signed 0)))

/-- The `early_return` witness's program (helper). -/
abbrev progEarlyReturn : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyEarlyReturn }] }

/-- The `panic` witness's body (helper). -/
abbrev bodyPanic : Expr :=
  .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
    (.seq (.dbg (.intLit .w64 .signed 5)) (.panic "boom"))

/-- The `panic` witness's program (helper). -/
abbrev progPanic : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyPanic }] }

/-- The `float` witness's body (helper). -/
abbrev bodyFloat : Expr :=
  .letIn false
    (.binop .add (.floatLit .w64 { sig := 15, negExp := true, e := 1 })
      (.floatLit .w64 { sig := 225, negExp := true, e := 2 }))
    (.binop .mul (.use (.var 0)) (.floatLit .w64 { sig := 2, negExp := false, e := 0 }))

/-- The `float` witness's program (helper). -/
abbrev progFloat : Program :=
  { decls := decls, fns := [{ params := [], ret := .float .w64, body := bodyFloat }] }

/-- The `stuck` witness's body (helper). -/
abbrev bodyStuck : Expr :=
  .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
    (.seq (.drop (.var 0)) (.use (.proj (.var 0) 0)))

/-- The `stuck` witness's program (helper). -/
abbrev progStuck : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyStuck }] }

/-- The `dtor` witness applied to each spine theorem `Spec.witnesses` lists for it (helper). -/
theorem dtor : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  have := Spine.no_violation M hPT 200
  have := Spine.no_use_after_move M hPT 200
  have := Spine.no_use_after_drop M hPT 200
  have := Spine.no_linear_leak M hPT 200
  have := Spine.no_linear_overwrite M hPT 200
  have := Spine.no_linear_discard M hPT 200
  have := Spine.check_sound _ hchk _ hfit
  have := Spine.checkProgram_sound hc
  have := Spine.no_double_free M hPT 200
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  have := Spine.step_progress M hPT
  have := Spine.step_preservation M hPT
  have := Spine.step_type_safety M hPT
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  have := Spine.never_stuck_iff M hPT
  have := Spine.eval_diverges_iff M hPT
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  have := Spine.freed_once M.toFloatOps P 200
  have := Spine.dtor_once M.toFloatOps hDNC 200
  have := Spine.drop_exactly_once M hPT hps (fuel := 200) hTy (Spine.Nonvacuous.empty_frame _).1 (Spine.Nonvacuous.empty_frame _).2 (by decide)
  have := Spine.rest_exactly_once M hPT hps hTy (Spine.Nonvacuous.empty_frame _).1 (Spine.Nonvacuous.empty_frame _).2 (by decide) hLead hEv
  have := Spine.Step.det hStep hStep
  have := Spine.Config.trichotomy M.toFloatOps P Config.init
  have := Spine.step_iff.mp hStep
  have := Spine.step_never_stuck_of_run M.toFloatOps P hns
  trivial

/-- The `linear` witness applied to each spine theorem `Spec.witnesses` lists for it (helper). -/
theorem linear : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  have := Spine.no_violation M hPT 200
  have := Spine.no_use_after_move M hPT 200
  have := Spine.no_use_after_drop M hPT 200
  have := Spine.no_linear_leak M hPT 200
  have := Spine.no_linear_overwrite M hPT 200
  have := Spine.no_linear_discard M hPT 200
  have := Spine.check_sound _ hchk _ hfit
  have := Spine.checkProgram_sound hc
  have := Spine.no_double_free M hPT 200
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  have := Spine.step_progress M hPT
  have := Spine.step_preservation M hPT
  have := Spine.step_type_safety M hPT
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  have := Spine.never_stuck_iff M hPT
  have := Spine.eval_diverges_iff M hPT
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- The `loop` witness applied to each spine theorem `Spec.witnesses` lists for it (helper). -/
theorem loop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  have := Spine.no_violation M hPT 200
  have := Spine.no_use_after_move M hPT 200
  have := Spine.no_use_after_drop M hPT 200
  have := Spine.no_linear_leak M hPT 200
  have := Spine.no_linear_overwrite M hPT 200
  have := Spine.no_linear_discard M hPT 200
  have := Spine.check_sound _ hchk _ hfit
  have := Spine.checkProgram_sound hc
  have := Spine.no_double_free M hPT 200
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  have := Spine.step_progress M hPT
  have := Spine.step_preservation M hPT
  have := Spine.step_type_safety M hPT
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  have := Spine.never_stuck_iff M hPT
  have := Spine.eval_diverges_iff M hPT
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  have := Spine.freed_once M.toFloatOps P 200
  trivial

/-- The `array` witness applied to each spine theorem `Spec.witnesses` lists for it (helper). -/
theorem array : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  have := Spine.no_violation M hPT 200
  have := Spine.no_use_after_move M hPT 200
  have := Spine.no_use_after_drop M hPT 200
  have := Spine.no_linear_leak M hPT 200
  have := Spine.no_linear_overwrite M hPT 200
  have := Spine.no_linear_discard M hPT 200
  have := Spine.check_sound _ hchk _ hfit
  have := Spine.checkProgram_sound hc
  have := Spine.no_double_free M hPT 200
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  have := Spine.step_progress M hPT
  have := Spine.step_preservation M hPT
  have := Spine.step_type_safety M hPT
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  have := Spine.never_stuck_iff M hPT
  have := Spine.eval_diverges_iff M hPT
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- The `enum_match` witness applied to each spine theorem `Spec.witnesses` lists for it (helper). -/
theorem enum_match : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  have := Spine.no_violation M hPT 200
  have := Spine.no_use_after_move M hPT 200
  have := Spine.no_use_after_drop M hPT 200
  have := Spine.no_linear_leak M hPT 200
  have := Spine.no_linear_overwrite M hPT 200
  have := Spine.no_linear_discard M hPT 200
  have := Spine.check_sound _ hchk _ hfit
  have := Spine.checkProgram_sound hc
  have := Spine.no_double_free M hPT 200
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  have := Spine.step_progress M hPT
  have := Spine.step_preservation M hPT
  have := Spine.step_type_safety M hPT
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  have := Spine.never_stuck_iff M hPT
  have := Spine.eval_diverges_iff M hPT
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- The `early_return` witness applied to each spine theorem `Spec.witnesses` lists for it (helper). -/
theorem early_return : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  have := Spine.no_violation M hPT 200
  have := Spine.no_use_after_move M hPT 200
  have := Spine.no_use_after_drop M hPT 200
  have := Spine.no_linear_leak M hPT 200
  have := Spine.no_linear_overwrite M hPT 200
  have := Spine.no_linear_discard M hPT 200
  have := Spine.check_sound _ hchk _ hfit
  have := Spine.checkProgram_sound hc
  have := Spine.no_double_free M hPT 200
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  have := Spine.step_progress M hPT
  have := Spine.step_preservation M hPT
  have := Spine.step_type_safety M hPT
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  have := Spine.never_stuck_iff M hPT
  have := Spine.eval_diverges_iff M hPT
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  have := Spine.run_ne_returned M.toFloatOps (P := P) (fuel := 200)
  trivial

/-- The `float` witness applied to each spine theorem `Spec.witnesses` lists for it (helper). -/
theorem float : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  have := Spine.no_violation M hPT 200
  have := Spine.no_use_after_move M hPT 200
  have := Spine.no_use_after_drop M hPT 200
  have := Spine.no_linear_leak M hPT 200
  have := Spine.no_linear_overwrite M hPT 200
  have := Spine.no_linear_discard M hPT 200
  have := Spine.check_sound _ hchk _ hfit
  have := Spine.checkProgram_sound hc
  have := Spine.no_double_free M hPT 200
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  have := Spine.step_progress M hPT
  have := Spine.step_preservation M hPT
  have := Spine.step_type_safety M hPT
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  have := Spine.never_stuck_iff M hPT
  have := Spine.eval_diverges_iff M hPT
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- The `panic` witness applied to each spine theorem `Spec.witnesses` lists for it (helper). -/
theorem panic : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  have := Spine.no_violation M hPT 200
  have := Spine.no_use_after_move M hPT 200
  have := Spine.no_use_after_drop M hPT 200
  have := Spine.no_linear_leak M hPT 200
  have := Spine.no_linear_overwrite M hPT 200
  have := Spine.no_linear_discard M hPT 200
  have := Spine.check_sound _ hchk _ hfit
  have := Spine.checkProgram_sound hc
  have := Spine.no_double_free M hPT 200
  have := (Spine.drop_order M hPT).2.1 _ _ hSteps
  have := Spine.step_progress M hPT
  have := Spine.step_preservation M hPT
  have := Spine.step_type_safety M hPT
  have := (Spine.eval_sound M hPT 200).2.2 _ _ hrun
  have := Spine.never_stuck_iff M hPT
  have := Spine.eval_diverges_iff M hPT
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.panic .user [.dbg (.int .w64 .signed 5)]) from trivial)
  have := (Spine.run_sim M.toFloatOps P 200).2 _ _ hrun
  have := (Spine.eval_complete M hPT).2 _ _ hSteps
  have := (Spine.run_complete M.toFloatOps P).2 _ _ hSteps
  trivial

/-- The `open_frame` witness applied to each spine theorem it lists (helper). -/
theorem open_frame : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hPT, hps, heps, hFM, hCC, ⟨c, Ω, hchk, hfit, hTy⟩, -, H₁, vs, tr, r, hLead, hEv⟩ :=
    Spine.Nonvacuous.open_frame decls rfl (.seq (.drop (.var 0)) (.intLit .w64 .signed 1)) rfl
      { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := .intLit .w64 .signed 0 }] } rfl
  rw [← hM] at hLead hEv
  have := Spine.soundness M hPT.wf 200 hTy hFM
  have := Spine.check_sound _ hchk _ hfit
  have := Spine.drop_exactly_once M hPT hps (fuel := 200) hTy hFM hCC heps
  have := Spine.rest_exactly_once M hPT hps hTy hFM hCC heps hLead hEv
  trivial

/-- The `diverges` witness applied to each spine theorem it lists (helper). -/
theorem diverges : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hoof⟩ := Spine.Nonvacuous.diverges
    { decls := { structs := [], enums := [] },
      fns := [{ params := [], ret := .unit, body := .loop .unitLit }] } rfl
  rw [← hM] at hoof
  have := Spine.checkProgram_sound hc
  have := (Spine.eval_diverges_iff M hPT).mp hoof
  trivial

/-- The `stuck` witness applied to each spine theorem it lists (helper). -/
theorem stuck : True := by
  obtain ⟨-, hr, C, hS, hSt⟩ := Spine.Nonvacuous.stuck bodyStuck rfl progStuck rfl
  have h300 : run Float.exactOps progStuck 300 = .stuck .useAfterMove := by rfl
  have hne : run Float.exactOps progStuck 200 ≠ .outOfFuel := by rw [hr]; intro h; cases h
  have := Spine.fuel_mono Float.exactOps (Nat.le_succ 200) hne
  have := Spine.no_masking Float.exactOps hr (m := 300) (show run Float.exactOps progStuck 300 ≠ .outOfFuel by rw [h300]; intro h; cases h)
  have := Spine.Config.trichotomy Float.exactOps progStuck C
  have := Spine.Config.stuck_iff.mpr ⟨_, hSt⟩
  have := Spine.step_stuck_isStuckState hSt
  have := Spine.run_stuck_of_step_stuck Float.exactOps progStuck hS hSt
  trivial


end RueCore.Nonvacuous.Glue
