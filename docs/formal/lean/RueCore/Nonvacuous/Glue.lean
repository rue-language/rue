module

public import RueCore.Spine

@[expose] public section

/-!
# RueCore.Nonvacuous.Glue — each witness applied to the theorems it witnesses (layer L2)

`Spec.witnesses` pairs each witness statement with the spine theorems whose
hypotheses it shows satisfiable. This module checks every pair in the kernel:
for each pair, one theorem, `Glue.<witness>.<theorem>`, that takes the witness's facts
(`RueCore.Spine.Nonvacuous.<w>`, with `M` from `Nonvacuous.exact_model` and
the frame from `Nonvacuous.empty_frame`) and **applies** each listed
`RueCore.Spine.<thm>` to them. The exact-model and empty-frame witnesses
have no program of their own: every program witness takes its `M` from
`Nonvacuous.exact_model`, and the evaluation statements their frame from
`Nonvacuous.empty_frame`, so their pairs are applied there. An application elaborates only if the witness
supplies that theorem's literal hypotheses, so a pair listed without them does
not compile; and the lint (`Lint.spineProblems`) fails on a listed pair
whose theorem is missing here or does not use both the witness's and the
spine theorem's `RueCore.Spine` constant. A spine theorem
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

/-- The `diverges_drop` witness's body (helper). -/
abbrev bodyDivergesDrop : Expr :=
  .loop (.letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) .unitLit)

/-- The `diverges_drop` witness's program (helper). -/
abbrev progDivergesDrop : Program :=
  { decls := decls, fns := [{ params := [], ret := .unit, body := bodyDivergesDrop }] }

/-- The `stuck` witness's body (helper). -/
abbrev bodyStuck : Expr :=
  .letIn false (.mkStruct 0 [.intLit .w64 .signed 1])
    (.seq (.drop (.var 0)) (.use (.proj (.var 0) 0)))

/-- The `stuck` witness's program (helper). -/
abbrev progStuck : Program :=
  { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := bodyStuck }] }

/-- The `whole_result` witness's program (helper). -/
abbrev progWholeResult : Program :=
  { decls := decls, fns := [{ params := [], ret := .struct 0, body := .mkStruct 0 [.intLit .w64 .signed 7] }] }

/-- `whole_drops` applied to `checkProgram_sound` (helper). -/
theorem whole_drops.checkProgram_sound : True := by
  obtain ⟨hc, -, -, -⟩ := Spine.Nonvacuous.whole_drops bodyDtor rfl progDtor rfl
  have := Spine.checkProgram_sound hc
  trivial

/-- `whole_drops` applied to `whole_program_exactly_once`, at each of the two
identities its reached configuration holds (helper). -/
theorem whole_drops.whole_program_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨-, hPT, hps, C, hC, h0, h2, H, v, tr, hT, -, -⟩ :=
    Spine.Nonvacuous.whole_drops bodyDtor rfl progDtor rfl
  rw [← hM] at hC hT
  have := Spine.whole_program_exactly_once M hPT hps hC h0 hT
  have := Spine.whole_program_exactly_once M hPT hps hC h2 hT
  trivial

/-- `whole_result` applied to `checkProgram_sound` (helper). -/
theorem whole_result.checkProgram_sound : True := by
  obtain ⟨hc, -, -, -⟩ := Spine.Nonvacuous.whole_result progWholeResult rfl
  have := Spine.checkProgram_sound hc
  trivial

/-- `whole_result` applied to `whole_program_exactly_once` (helper). -/
theorem whole_result.whole_program_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨-, hPT, hps, C, hC, h0, H, v, tr, hT, -, -⟩ :=
    Spine.Nonvacuous.whole_result progWholeResult rfl
  rw [← hM] at hC hT
  have := Spine.whole_program_exactly_once M hPT hps hC h0 hT
  trivial

/-- `exact_model` applied to `whole_program_exactly_once`, through the
`whole_drops` program (helper). -/
theorem exact_model.whole_program_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨-, hPT, hps, C, hC, h0, -, H, v, tr, hT, -, -⟩ :=
    Spine.Nonvacuous.whole_drops bodyDtor rfl progDtor rfl
  rw [← hM] at hC hT
  have := Spine.whole_program_exactly_once M hPT hps hC h0 hT
  trivial

/-- `dtor` applied to `soundness` (helper). -/
theorem dtor.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `dtor` applied to `run_safe` (helper). -/
theorem dtor.run_safe : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  trivial

/-- `dtor` applied to `no_violation` (helper). -/
theorem dtor.no_violation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_violation M hPT 200
  trivial

/-- `dtor` applied to `no_use_after_move` (helper). -/
theorem dtor.no_use_after_move : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_move M hPT 200
  trivial

/-- `dtor` applied to `no_use_after_drop` (helper). -/
theorem dtor.no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_drop M hPT 200
  trivial

/-- `dtor` applied to `run_no_use_after_drop` (helper). -/
theorem dtor.run_no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_no_use_after_drop M.toFloatOps P 200
  trivial

/-- `dtor` applied to `no_linear_leak` (helper). -/
theorem dtor.no_linear_leak : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_leak M hPT 200
  trivial

/-- `dtor` applied to `no_linear_overwrite` (helper). -/
theorem dtor.no_linear_overwrite : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_overwrite M hPT 200
  trivial

/-- `dtor` applied to `no_linear_discard` (helper). -/
theorem dtor.no_linear_discard : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_discard M hPT 200
  trivial

/-- `dtor` applied to `check_sound` (helper). -/
theorem dtor.check_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.check_sound _ hchk _ hfit
  trivial

/-- `dtor` applied to `checkProgram_sound` (helper). -/
theorem dtor.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.checkProgram_sound hc
  trivial

/-- `dtor` applied to `no_double_free` (helper). -/
theorem dtor.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_double_free M hPT 200
  trivial

/-- `dtor` applied to `drop_order` (helper). -/
theorem dtor.drop_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `dtor` applied to `drop_glue_order` (helper). -/
theorem dtor.drop_glue_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_glue_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `dtor` applied to `step_progress` (helper). -/
theorem dtor.step_progress : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_progress M hPT
  trivial

/-- `dtor` applied to `step_preservation` (helper). -/
theorem dtor.step_preservation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_preservation M hPT
  trivial

/-- `dtor` applied to `step_type_safety` (helper). -/
theorem dtor.step_type_safety : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_type_safety M hPT
  trivial

/-- `dtor` applied to `step_no_use_after_drop` (helper). -/
theorem dtor.step_no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_no_use_after_drop M.toFloatOps P hSteps
  trivial

/-- `dtor` applied to `eval_sound` (helper). -/
theorem dtor.eval_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  trivial

/-- `dtor` applied to `never_stuck_iff` (helper). -/
theorem dtor.never_stuck_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.never_stuck_iff M hPT
  trivial

/-- `dtor` applied to `eval_diverges_iff` (helper). -/
theorem dtor.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.eval_diverges_iff M hPT
  trivial

/-- `dtor` applied to `fuel_mono` (helper). -/
theorem dtor.fuel_mono : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  trivial

/-- `dtor` applied to `Step.terminal` (helper). -/
theorem dtor.Step.terminal : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  trivial

/-- `dtor` applied to `run_sim` (helper). -/
theorem dtor.run_sim : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  trivial

/-- `dtor` applied to `eval_complete` (helper). -/
theorem dtor.eval_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  trivial

/-- `dtor` applied to `run_complete` (helper). -/
theorem dtor.run_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- `dtor` applied to `step_no_double_free` (helper). -/
theorem dtor.step_no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_no_double_free M hPT hSteps
  trivial

/-- `dtor` applied to `freed_once` (helper). -/
theorem dtor.freed_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.freed_once M.toFloatOps P 200
  trivial

/-- `dtor` applied to `dtor_once` (helper). -/
theorem dtor.dtor_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.dtor_once M.toFloatOps hDNC 200
  trivial

/-- `dtor` applied to `drop_exactly_once` (helper). -/
theorem dtor.drop_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.drop_exactly_once M hPT hps (fuel := 200) hTy (Spine.Nonvacuous.empty_frame _).1 (Spine.Nonvacuous.empty_frame _).2 (by decide)
  trivial

/-- `dtor` applied to `rest_exactly_once` (helper). -/
theorem dtor.rest_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.rest_exactly_once M hPT hps hTy (Spine.Nonvacuous.empty_frame _).1 (Spine.Nonvacuous.empty_frame _).2 (by decide) hLead hEv
  trivial

/-- `dtor` applied to `Step.det` (helper). -/
theorem dtor.Step.det : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Step.det hStep hStep
  trivial

/-- `dtor` applied to `Config.trichotomy` (helper). -/
theorem dtor.Config.trichotomy : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Config.trichotomy M.toFloatOps P Config.init
  trivial

/-- `dtor` applied to `step_iff` (helper). -/
theorem dtor.step_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_iff.mp hStep
  trivial

/-- `dtor` applied to `step_never_stuck_of_run` (helper). -/
theorem dtor.step_never_stuck_of_run : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_never_stuck_of_run M.toFloatOps P hns
  trivial

/-- `linear` applied to `soundness` (helper). -/
theorem linear.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `linear` applied to `run_safe` (helper). -/
theorem linear.run_safe : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  trivial

/-- `linear` applied to `no_violation` (helper). -/
theorem linear.no_violation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_violation M hPT 200
  trivial

/-- `linear` applied to `no_use_after_move` (helper). -/
theorem linear.no_use_after_move : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_move M hPT 200
  trivial

/-- `linear` applied to `no_use_after_drop` (helper). -/
theorem linear.no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_drop M hPT 200
  trivial

/-- `linear` applied to `no_linear_leak` (helper). -/
theorem linear.no_linear_leak : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_leak M hPT 200
  trivial

/-- `linear` applied to `no_linear_overwrite` (helper). -/
theorem linear.no_linear_overwrite : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_overwrite M hPT 200
  trivial

/-- `linear` applied to `no_linear_discard` (helper). -/
theorem linear.no_linear_discard : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_discard M hPT 200
  trivial

/-- `linear` applied to `check_sound` (helper). -/
theorem linear.check_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.check_sound _ hchk _ hfit
  trivial

/-- `linear` applied to `checkProgram_sound` (helper). -/
theorem linear.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.checkProgram_sound hc
  trivial

/-- `linear` applied to `no_double_free` (helper). -/
theorem linear.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_double_free M hPT 200
  trivial

/-- `linear` applied to `drop_order` (helper). -/
theorem linear.drop_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `linear` applied to `drop_glue_order` (helper). -/
theorem linear.drop_glue_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_glue_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `linear` applied to `step_progress` (helper). -/
theorem linear.step_progress : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_progress M hPT
  trivial

/-- `linear` applied to `step_preservation` (helper). -/
theorem linear.step_preservation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_preservation M hPT
  trivial

/-- `linear` applied to `step_type_safety` (helper). -/
theorem linear.step_type_safety : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_type_safety M hPT
  trivial

/-- `linear` applied to `eval_sound` (helper). -/
theorem linear.eval_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  trivial

/-- `linear` applied to `never_stuck_iff` (helper). -/
theorem linear.never_stuck_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.never_stuck_iff M hPT
  trivial

/-- `linear` applied to `eval_diverges_iff` (helper). -/
theorem linear.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.eval_diverges_iff M hPT
  trivial

/-- `linear` applied to `fuel_mono` (helper). -/
theorem linear.fuel_mono : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  trivial

/-- `linear` applied to `Step.terminal` (helper). -/
theorem linear.Step.terminal : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  trivial

/-- `linear` applied to `run_sim` (helper). -/
theorem linear.run_sim : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  trivial

/-- `linear` applied to `eval_complete` (helper). -/
theorem linear.eval_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  trivial

/-- `linear` applied to `run_complete` (helper). -/
theorem linear.run_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.linear bodyLinear rfl progLinear rfl
  rw [← hM] at hrun hSteps
  let P := progLinear
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- `loop` applied to `soundness` (helper). -/
theorem loop.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `loop` applied to `run_safe` (helper). -/
theorem loop.run_safe : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  trivial

/-- `loop` applied to `no_violation` (helper). -/
theorem loop.no_violation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_violation M hPT 200
  trivial

/-- `loop` applied to `no_use_after_move` (helper). -/
theorem loop.no_use_after_move : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_move M hPT 200
  trivial

/-- `loop` applied to `no_use_after_drop` (helper). -/
theorem loop.no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_drop M hPT 200
  trivial

/-- `loop` applied to `no_linear_leak` (helper). -/
theorem loop.no_linear_leak : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_leak M hPT 200
  trivial

/-- `loop` applied to `no_linear_overwrite` (helper). -/
theorem loop.no_linear_overwrite : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_overwrite M hPT 200
  trivial

/-- `loop` applied to `no_linear_discard` (helper). -/
theorem loop.no_linear_discard : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_discard M hPT 200
  trivial

/-- `loop` applied to `check_sound` (helper). -/
theorem loop.check_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.check_sound _ hchk _ hfit
  trivial

/-- `loop` applied to `checkProgram_sound` (helper). -/
theorem loop.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.checkProgram_sound hc
  trivial

/-- `loop` applied to `no_double_free` (helper). -/
theorem loop.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_double_free M hPT 200
  trivial

/-- `loop` applied to `drop_order` (helper). -/
theorem loop.drop_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `loop` applied to `drop_glue_order` (helper). -/
theorem loop.drop_glue_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_glue_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `loop` applied to `step_progress` (helper). -/
theorem loop.step_progress : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_progress M hPT
  trivial

/-- `loop` applied to `step_preservation` (helper). -/
theorem loop.step_preservation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_preservation M hPT
  trivial

/-- `loop` applied to `step_type_safety` (helper). -/
theorem loop.step_type_safety : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_type_safety M hPT
  trivial

/-- `loop` applied to `eval_sound` (helper). -/
theorem loop.eval_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  trivial

/-- `loop` applied to `never_stuck_iff` (helper). -/
theorem loop.never_stuck_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.never_stuck_iff M hPT
  trivial

/-- `loop` applied to `eval_diverges_iff` (helper). -/
theorem loop.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.eval_diverges_iff M hPT
  trivial

/-- `loop` applied to `fuel_mono` (helper). -/
theorem loop.fuel_mono : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  trivial

/-- `loop` applied to `Step.terminal` (helper). -/
theorem loop.Step.terminal : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  trivial

/-- `loop` applied to `run_sim` (helper). -/
theorem loop.run_sim : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  trivial

/-- `loop` applied to `eval_complete` (helper). -/
theorem loop.eval_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  trivial

/-- `loop` applied to `run_complete` (helper). -/
theorem loop.run_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- `loop` applied to `freed_once` (helper). -/
theorem loop.freed_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.loop bodyLoop rfl progLoop rfl
  rw [← hM] at hrun hSteps
  let P := progLoop
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.freed_once M.toFloatOps P 200
  trivial

/-- `array` applied to `soundness` (helper). -/
theorem array.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `array` applied to `run_safe` (helper). -/
theorem array.run_safe : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  trivial

/-- `array` applied to `no_violation` (helper). -/
theorem array.no_violation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_violation M hPT 200
  trivial

/-- `array` applied to `no_use_after_move` (helper). -/
theorem array.no_use_after_move : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_move M hPT 200
  trivial

/-- `array` applied to `no_use_after_drop` (helper). -/
theorem array.no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_drop M hPT 200
  trivial

/-- `array` applied to `no_linear_leak` (helper). -/
theorem array.no_linear_leak : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_leak M hPT 200
  trivial

/-- `array` applied to `no_linear_overwrite` (helper). -/
theorem array.no_linear_overwrite : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_overwrite M hPT 200
  trivial

/-- `array` applied to `no_linear_discard` (helper). -/
theorem array.no_linear_discard : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_discard M hPT 200
  trivial

/-- `array` applied to `check_sound` (helper). -/
theorem array.check_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.check_sound _ hchk _ hfit
  trivial

/-- `array` applied to `checkProgram_sound` (helper). -/
theorem array.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.checkProgram_sound hc
  trivial

/-- `array` applied to `no_double_free` (helper). -/
theorem array.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_double_free M hPT 200
  trivial

/-- `array` applied to `drop_order` (helper). -/
theorem array.drop_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `array` applied to `drop_glue_order` (helper). -/
theorem array.drop_glue_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_glue_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `array` applied to `step_progress` (helper). -/
theorem array.step_progress : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_progress M hPT
  trivial

/-- `array` applied to `step_preservation` (helper). -/
theorem array.step_preservation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_preservation M hPT
  trivial

/-- `array` applied to `step_type_safety` (helper). -/
theorem array.step_type_safety : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_type_safety M hPT
  trivial

/-- `array` applied to `eval_sound` (helper). -/
theorem array.eval_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  trivial

/-- `array` applied to `never_stuck_iff` (helper). -/
theorem array.never_stuck_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.never_stuck_iff M hPT
  trivial

/-- `array` applied to `eval_diverges_iff` (helper). -/
theorem array.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.eval_diverges_iff M hPT
  trivial

/-- `array` applied to `fuel_mono` (helper). -/
theorem array.fuel_mono : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  trivial

/-- `array` applied to `Step.terminal` (helper). -/
theorem array.Step.terminal : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  trivial

/-- `array` applied to `run_sim` (helper). -/
theorem array.run_sim : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  trivial

/-- `array` applied to `eval_complete` (helper). -/
theorem array.eval_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  trivial

/-- `array` applied to `run_complete` (helper). -/
theorem array.run_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.array bodyArray rfl progArray rfl
  rw [← hM] at hrun hSteps
  let P := progArray
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- `enum_match` applied to `soundness` (helper). -/
theorem enum_match.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `enum_match` applied to `run_safe` (helper). -/
theorem enum_match.run_safe : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  trivial

/-- `enum_match` applied to `no_violation` (helper). -/
theorem enum_match.no_violation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_violation M hPT 200
  trivial

/-- `enum_match` applied to `no_use_after_move` (helper). -/
theorem enum_match.no_use_after_move : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_move M hPT 200
  trivial

/-- `enum_match` applied to `no_use_after_drop` (helper). -/
theorem enum_match.no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_drop M hPT 200
  trivial

/-- `enum_match` applied to `no_linear_leak` (helper). -/
theorem enum_match.no_linear_leak : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_leak M hPT 200
  trivial

/-- `enum_match` applied to `no_linear_overwrite` (helper). -/
theorem enum_match.no_linear_overwrite : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_overwrite M hPT 200
  trivial

/-- `enum_match` applied to `no_linear_discard` (helper). -/
theorem enum_match.no_linear_discard : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_discard M hPT 200
  trivial

/-- `enum_match` applied to `check_sound` (helper). -/
theorem enum_match.check_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.check_sound _ hchk _ hfit
  trivial

/-- `enum_match` applied to `checkProgram_sound` (helper). -/
theorem enum_match.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.checkProgram_sound hc
  trivial

/-- `enum_match` applied to `no_double_free` (helper). -/
theorem enum_match.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_double_free M hPT 200
  trivial

/-- `enum_match` applied to `drop_order` (helper). -/
theorem enum_match.drop_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `enum_match` applied to `drop_glue_order` (helper). -/
theorem enum_match.drop_glue_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_glue_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `enum_match` applied to `step_progress` (helper). -/
theorem enum_match.step_progress : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_progress M hPT
  trivial

/-- `enum_match` applied to `step_preservation` (helper). -/
theorem enum_match.step_preservation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_preservation M hPT
  trivial

/-- `enum_match` applied to `step_type_safety` (helper). -/
theorem enum_match.step_type_safety : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_type_safety M hPT
  trivial

/-- `enum_match` applied to `eval_sound` (helper). -/
theorem enum_match.eval_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  trivial

/-- `enum_match` applied to `never_stuck_iff` (helper). -/
theorem enum_match.never_stuck_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.never_stuck_iff M hPT
  trivial

/-- `enum_match` applied to `eval_diverges_iff` (helper). -/
theorem enum_match.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.eval_diverges_iff M hPT
  trivial

/-- `enum_match` applied to `fuel_mono` (helper). -/
theorem enum_match.fuel_mono : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  trivial

/-- `enum_match` applied to `Step.terminal` (helper). -/
theorem enum_match.Step.terminal : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  trivial

/-- `enum_match` applied to `run_sim` (helper). -/
theorem enum_match.run_sim : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  trivial

/-- `enum_match` applied to `eval_complete` (helper). -/
theorem enum_match.eval_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  trivial

/-- `enum_match` applied to `run_complete` (helper). -/
theorem enum_match.run_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.enum_match bodyEnumMatch rfl progEnumMatch rfl
  rw [← hM] at hrun hSteps
  let P := progEnumMatch
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- `early_return` applied to `soundness` (helper). -/
theorem early_return.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `early_return` applied to `run_safe` (helper). -/
theorem early_return.run_safe : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  trivial

/-- `early_return` applied to `no_violation` (helper). -/
theorem early_return.no_violation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_violation M hPT 200
  trivial

/-- `early_return` applied to `no_use_after_move` (helper). -/
theorem early_return.no_use_after_move : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_move M hPT 200
  trivial

/-- `early_return` applied to `no_use_after_drop` (helper). -/
theorem early_return.no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_drop M hPT 200
  trivial

/-- `early_return` applied to `no_linear_leak` (helper). -/
theorem early_return.no_linear_leak : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_leak M hPT 200
  trivial

/-- `early_return` applied to `no_linear_overwrite` (helper). -/
theorem early_return.no_linear_overwrite : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_overwrite M hPT 200
  trivial

/-- `early_return` applied to `no_linear_discard` (helper). -/
theorem early_return.no_linear_discard : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_discard M hPT 200
  trivial

/-- `early_return` applied to `check_sound` (helper). -/
theorem early_return.check_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.check_sound _ hchk _ hfit
  trivial

/-- `early_return` applied to `checkProgram_sound` (helper). -/
theorem early_return.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.checkProgram_sound hc
  trivial

/-- `early_return` applied to `no_double_free` (helper). -/
theorem early_return.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_double_free M hPT 200
  trivial

/-- `early_return` applied to `drop_order` (helper). -/
theorem early_return.drop_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `early_return` applied to `drop_glue_order` (helper). -/
theorem early_return.drop_glue_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_glue_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `early_return` applied to `step_progress` (helper). -/
theorem early_return.step_progress : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_progress M hPT
  trivial

/-- `early_return` applied to `step_preservation` (helper). -/
theorem early_return.step_preservation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_preservation M hPT
  trivial

/-- `early_return` applied to `step_type_safety` (helper). -/
theorem early_return.step_type_safety : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_type_safety M hPT
  trivial

/-- `early_return` applied to `eval_sound` (helper). -/
theorem early_return.eval_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  trivial

/-- `early_return` applied to `never_stuck_iff` (helper). -/
theorem early_return.never_stuck_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.never_stuck_iff M hPT
  trivial

/-- `early_return` applied to `eval_diverges_iff` (helper). -/
theorem early_return.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.eval_diverges_iff M hPT
  trivial

/-- `early_return` applied to `fuel_mono` (helper). -/
theorem early_return.fuel_mono : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  trivial

/-- `early_return` applied to `Step.terminal` (helper). -/
theorem early_return.Step.terminal : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  trivial

/-- `early_return` applied to `run_sim` (helper). -/
theorem early_return.run_sim : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  trivial

/-- `early_return` applied to `eval_complete` (helper). -/
theorem early_return.eval_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  trivial

/-- `early_return` applied to `run_complete` (helper). -/
theorem early_return.run_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- `early_return` applied to `run_ne_returned` (helper). -/
theorem early_return.run_ne_returned : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.early_return bodyEarlyReturn rfl progEarlyReturn rfl
  rw [← hM] at hrun hSteps
  let P := progEarlyReturn
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_ne_returned M.toFloatOps (P := P) (fuel := 200)
  trivial

/-- `float` applied to `soundness` (helper). -/
theorem float.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `float` applied to `run_safe` (helper). -/
theorem float.run_safe : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  trivial

/-- `float` applied to `no_violation` (helper). -/
theorem float.no_violation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_violation M hPT 200
  trivial

/-- `float` applied to `no_use_after_move` (helper). -/
theorem float.no_use_after_move : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_move M hPT 200
  trivial

/-- `float` applied to `no_use_after_drop` (helper). -/
theorem float.no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_drop M hPT 200
  trivial

/-- `float` applied to `no_linear_leak` (helper). -/
theorem float.no_linear_leak : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_leak M hPT 200
  trivial

/-- `float` applied to `no_linear_overwrite` (helper). -/
theorem float.no_linear_overwrite : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_overwrite M hPT 200
  trivial

/-- `float` applied to `no_linear_discard` (helper). -/
theorem float.no_linear_discard : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_discard M hPT 200
  trivial

/-- `float` applied to `check_sound` (helper). -/
theorem float.check_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.check_sound _ hchk _ hfit
  trivial

/-- `float` applied to `checkProgram_sound` (helper). -/
theorem float.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.checkProgram_sound hc
  trivial

/-- `float` applied to `no_double_free` (helper). -/
theorem float.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_double_free M hPT 200
  trivial

/-- `float` applied to `drop_order` (helper). -/
theorem float.drop_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `float` applied to `drop_glue_order` (helper). -/
theorem float.drop_glue_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_glue_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `float` applied to `step_progress` (helper). -/
theorem float.step_progress : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_progress M hPT
  trivial

/-- `float` applied to `step_preservation` (helper). -/
theorem float.step_preservation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_preservation M hPT
  trivial

/-- `float` applied to `step_type_safety` (helper). -/
theorem float.step_type_safety : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_type_safety M hPT
  trivial

/-- `float` applied to `eval_sound` (helper). -/
theorem float.eval_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  trivial

/-- `float` applied to `never_stuck_iff` (helper). -/
theorem float.never_stuck_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.never_stuck_iff M hPT
  trivial

/-- `float` applied to `eval_diverges_iff` (helper). -/
theorem float.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.eval_diverges_iff M hPT
  trivial

/-- `float` applied to `fuel_mono` (helper). -/
theorem float.fuel_mono : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  trivial

/-- `float` applied to `Step.terminal` (helper). -/
theorem float.Step.terminal : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.run H Frame.empty [] (.ret v) tr) from trivial)
  trivial

/-- `float` applied to `run_sim` (helper). -/
theorem float.run_sim : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_sim M.toFloatOps P 200).1 _ _ _ hrun
  trivial

/-- `float` applied to `eval_complete` (helper). -/
theorem float.eval_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  trivial

/-- `float` applied to `run_complete` (helper). -/
theorem float.run_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.float bodyFloat rfl progFloat rfl
  rw [← hM] at hrun hSteps
  let P := progFloat
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_complete M.toFloatOps P).1 _ _ _ _ hSteps
  trivial

/-- `panic` applied to `soundness` (helper). -/
theorem panic.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `panic` applied to `run_safe` (helper). -/
theorem panic.run_safe : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  trivial

/-- `panic` applied to `no_violation` (helper). -/
theorem panic.no_violation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_violation M hPT 200
  trivial

/-- `panic` applied to `no_use_after_move` (helper). -/
theorem panic.no_use_after_move : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_move M hPT 200
  trivial

/-- `panic` applied to `no_use_after_drop` (helper). -/
theorem panic.no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_drop M hPT 200
  trivial

/-- `panic` applied to `no_linear_leak` (helper). -/
theorem panic.no_linear_leak : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_leak M hPT 200
  trivial

/-- `panic` applied to `no_linear_overwrite` (helper). -/
theorem panic.no_linear_overwrite : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_overwrite M hPT 200
  trivial

/-- `panic` applied to `no_linear_discard` (helper). -/
theorem panic.no_linear_discard : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_discard M hPT 200
  trivial

/-- `panic` applied to `check_sound` (helper). -/
theorem panic.check_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.check_sound _ hchk _ hfit
  trivial

/-- `panic` applied to `checkProgram_sound` (helper). -/
theorem panic.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.checkProgram_sound hc
  trivial

/-- `panic` applied to `no_double_free` (helper). -/
theorem panic.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_double_free M hPT 200
  trivial

/-- `panic` applied to `drop_order` (helper). -/
theorem panic.drop_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_order M hPT).2.1 _ _ hSteps
  trivial

/-- `panic` applied to `drop_glue_order` (helper). -/
theorem panic.drop_glue_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_glue_order M hPT).2 _ _ hSteps
  trivial

/-- `panic` applied to `step_progress` (helper). -/
theorem panic.step_progress : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_progress M hPT
  trivial

/-- `panic` applied to `step_preservation` (helper). -/
theorem panic.step_preservation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_preservation M hPT
  trivial

/-- `panic` applied to `step_type_safety` (helper). -/
theorem panic.step_type_safety : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_type_safety M hPT
  trivial

/-- `panic` applied to `eval_sound` (helper). -/
theorem panic.eval_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_sound M hPT 200).2.2 _ _ hrun
  trivial

/-- `panic` applied to `never_stuck_iff` (helper). -/
theorem panic.never_stuck_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.never_stuck_iff M hPT
  trivial

/-- `panic` applied to `eval_diverges_iff` (helper). -/
theorem panic.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.eval_diverges_iff M hPT
  trivial

/-- `panic` applied to `fuel_mono` (helper). -/
theorem panic.fuel_mono : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.fuel_mono M.toFloatOps (Nat.le_succ 200) hne
  trivial

/-- `panic` applied to `Step.terminal` (helper). -/
theorem panic.Step.terminal : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.Step.terminal (M := M.toFloatOps) (P := P) (C' := Config.init) (show Config.Terminal (.panic .user [.dbg (.int .w64 .signed 5)]) from trivial)
  trivial

/-- `panic` applied to `run_sim` (helper). -/
theorem panic.run_sim : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_sim M.toFloatOps P 200).2 _ _ hrun
  trivial

/-- `panic` applied to `eval_complete` (helper). -/
theorem panic.eval_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_complete M hPT).2 _ _ hSteps
  trivial

/-- `panic` applied to `run_complete` (helper). -/
theorem panic.run_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hrun, hSteps⟩ :=
    Spine.Nonvacuous.panic bodyPanic rfl progPanic rfl
  rw [← hM] at hrun hSteps
  let P := progPanic
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.run_complete M.toFloatOps P).2 _ _ hSteps
  trivial

/-- `exact_model` applied to `soundness`, through the `dtor` program (helper). -/
theorem exact_model.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `exact_model` applied to `run_safe`, through the `dtor` program (helper). -/
theorem exact_model.run_safe : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.run_safe M hPT.wf (fd := _) rfl rfl 200
  trivial

/-- `exact_model` applied to `no_violation`, through the `dtor` program (helper). -/
theorem exact_model.no_violation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_violation M hPT 200
  trivial

/-- `exact_model` applied to `no_use_after_move`, through the `dtor` program (helper). -/
theorem exact_model.no_use_after_move : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_move M hPT 200
  trivial

/-- `exact_model` applied to `no_use_after_drop`, through the `dtor` program (helper). -/
theorem exact_model.no_use_after_drop : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_use_after_drop M hPT 200
  trivial

/-- `exact_model` applied to `no_linear_leak`, through the `dtor` program (helper). -/
theorem exact_model.no_linear_leak : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_leak M hPT 200
  trivial

/-- `exact_model` applied to `no_linear_overwrite`, through the `dtor` program (helper). -/
theorem exact_model.no_linear_overwrite : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_overwrite M hPT 200
  trivial

/-- `exact_model` applied to `no_linear_discard`, through the `dtor` program (helper). -/
theorem exact_model.no_linear_discard : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_linear_discard M hPT 200
  trivial

/-- `exact_model` applied to `no_double_free`, through the `dtor` program (helper). -/
theorem exact_model.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.no_double_free M hPT 200
  trivial

/-- `exact_model` applied to `step_no_double_free`, through the `dtor` program (helper). -/
theorem exact_model.step_no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_no_double_free M hPT hSteps
  trivial

/-- `exact_model` applied to `drop_exactly_once`, through the `dtor` program (helper). -/
theorem exact_model.drop_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.drop_exactly_once M hPT hps (fuel := 200) hTy (Spine.Nonvacuous.empty_frame _).1 (Spine.Nonvacuous.empty_frame _).2 (by decide)
  trivial

/-- `exact_model` applied to `rest_exactly_once`, through the `dtor` program (helper). -/
theorem exact_model.rest_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.rest_exactly_once M hPT hps hTy (Spine.Nonvacuous.empty_frame _).1 (Spine.Nonvacuous.empty_frame _).2 (by decide) hLead hEv
  trivial

/-- `exact_model` applied to `drop_order`, through the `dtor` program (helper). -/
theorem exact_model.drop_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `exact_model` applied to `drop_glue_order`, through the `dtor` program (helper). -/
theorem exact_model.drop_glue_order : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.drop_glue_order M hPT).1 _ _ _ _ hSteps
  trivial

/-- `exact_model` applied to `step_progress`, through the `dtor` program (helper). -/
theorem exact_model.step_progress : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_progress M hPT
  trivial

/-- `exact_model` applied to `step_preservation`, through the `dtor` program (helper). -/
theorem exact_model.step_preservation : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_preservation M hPT
  trivial

/-- `exact_model` applied to `step_type_safety`, through the `dtor` program (helper). -/
theorem exact_model.step_type_safety : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.step_type_safety M hPT
  trivial

/-- `exact_model` applied to `eval_sound`, through the `dtor` program (helper). -/
theorem exact_model.eval_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_sound M hPT 200).2.1 _ _ _ hrun
  trivial

/-- `exact_model` applied to `eval_complete`, through the `dtor` program (helper). -/
theorem exact_model.eval_complete : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := (Spine.eval_complete M hPT).1 _ _ _ _ hSteps
  trivial

/-- `exact_model` applied to `never_stuck_iff`, through the `dtor` program (helper). -/
theorem exact_model.never_stuck_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.never_stuck_iff M hPT
  trivial

/-- `exact_model` applied to `eval_diverges_iff`, through the `dtor` program (helper). -/
theorem exact_model.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.eval_diverges_iff M hPT
  trivial

/-- `empty_frame` applied to `soundness`, through the `dtor` program (helper). -/
theorem empty_frame.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.soundness M hPT.wf 200 hTy (Spine.Nonvacuous.empty_frame _).1
  trivial

/-- `empty_frame` applied to `drop_exactly_once`, through the `dtor` program (helper). -/
theorem empty_frame.drop_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.drop_exactly_once M hPT hps (fuel := 200) hTy (Spine.Nonvacuous.empty_frame _).1 (Spine.Nonvacuous.empty_frame _).2 (by decide)
  trivial

/-- `empty_frame` applied to `rest_exactly_once`, through the `dtor` program (helper). -/
theorem empty_frame.rest_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hps, ⟨c, Ω, hchk, hfit, hTy⟩, hDNC, ⟨C, hStep⟩, hns, -, ⟨H₁, vs, tr₁, r, hLead, hEv, -⟩,
    H, v, tr, hrun, hSteps, -⟩ :=
    Spine.Nonvacuous.dtor bodyDtor rfl progDtor rfl
  rw [← hM] at hStep hns hLead hEv hrun hSteps
  let P := progDtor
  have hne : run M.toFloatOps P 200 ≠ .outOfFuel := by rw [hrun]; intro h; cases h
  have := Spine.rest_exactly_once M hPT hps hTy (Spine.Nonvacuous.empty_frame _).1 (Spine.Nonvacuous.empty_frame _).2 (by decide) hLead hEv
  trivial

/-- `open_frame` applied to `soundness` (helper). -/
theorem open_frame.soundness : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hPT, hps, heps, hFM, hCC, ⟨c, Ω, hchk, hfit, hTy⟩, -, H₁, vs, tr, r, hLead, hEv⟩ :=
    Spine.Nonvacuous.open_frame decls rfl (.seq (.drop (.var 0)) (.intLit .w64 .signed 1)) rfl
      { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := .intLit .w64 .signed 0 }] } rfl
  rw [← hM] at hLead hEv
  have := Spine.soundness M hPT.wf 200 hTy hFM
  trivial

/-- `open_frame` applied to `check_sound` (helper). -/
theorem open_frame.check_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hPT, hps, heps, hFM, hCC, ⟨c, Ω, hchk, hfit, hTy⟩, -, H₁, vs, tr, r, hLead, hEv⟩ :=
    Spine.Nonvacuous.open_frame decls rfl (.seq (.drop (.var 0)) (.intLit .w64 .signed 1)) rfl
      { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := .intLit .w64 .signed 0 }] } rfl
  rw [← hM] at hLead hEv
  have := Spine.check_sound _ hchk _ hfit
  trivial

/-- `open_frame` applied to `drop_exactly_once` (helper). -/
theorem open_frame.drop_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hPT, hps, heps, hFM, hCC, ⟨c, Ω, hchk, hfit, hTy⟩, -, H₁, vs, tr, r, hLead, hEv⟩ :=
    Spine.Nonvacuous.open_frame decls rfl (.seq (.drop (.var 0)) (.intLit .w64 .signed 1)) rfl
      { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := .intLit .w64 .signed 0 }] } rfl
  rw [← hM] at hLead hEv
  have := Spine.drop_exactly_once M hPT hps (fuel := 200) hTy hFM hCC heps
  trivial

/-- `open_frame` applied to `rest_exactly_once` (helper). -/
theorem open_frame.rest_exactly_once : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hPT, hps, heps, hFM, hCC, ⟨c, Ω, hchk, hfit, hTy⟩, -, H₁, vs, tr, r, hLead, hEv⟩ :=
    Spine.Nonvacuous.open_frame decls rfl (.seq (.drop (.var 0)) (.intLit .w64 .signed 1)) rfl
      { decls := decls, fns := [{ params := [], ret := .int .w64 .signed, body := .intLit .w64 .signed 0 }] } rfl
  rw [← hM] at hLead hEv
  have := Spine.rest_exactly_once M hPT hps hTy hFM hCC heps hLead hEv
  trivial

/-- `diverges` applied to `checkProgram_sound` (helper). -/
theorem diverges.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hoof⟩ := Spine.Nonvacuous.diverges
    { decls := { structs := [], enums := [] },
      fns := [{ params := [], ret := .unit, body := .loop .unitLit }] } rfl
  rw [← hM] at hoof
  have := Spine.checkProgram_sound hc
  trivial

/-- `diverges` applied to `eval_diverges_iff` (helper). -/
theorem diverges.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hoof⟩ := Spine.Nonvacuous.diverges
    { decls := { structs := [], enums := [] },
      fns := [{ params := [], ret := .unit, body := .loop .unitLit }] } rfl
  rw [← hM] at hoof
  have := (Spine.eval_diverges_iff M hPT).mp hoof
  trivial

/-- `diverges_drop` applied to `checkProgram_sound` (helper). -/
theorem diverges_drop.checkProgram_sound : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hoof, C, hSteps, -, -⟩ :=
    Spine.Nonvacuous.diverges_drop bodyDivergesDrop rfl progDivergesDrop rfl
  rw [← hM] at hoof hSteps
  have := Spine.checkProgram_sound hc
  trivial

/-- `diverges_drop` applied to `no_double_free` (helper). -/
theorem diverges_drop.no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hoof, C, hSteps, -, -⟩ :=
    Spine.Nonvacuous.diverges_drop bodyDivergesDrop rfl progDivergesDrop rfl
  rw [← hM] at hoof hSteps
  have := Spine.no_double_free M hPT 200
  trivial

/-- `diverges_drop` applied to `step_no_double_free` (helper). -/
theorem diverges_drop.step_no_double_free : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hoof, C, hSteps, -, -⟩ :=
    Spine.Nonvacuous.diverges_drop bodyDivergesDrop rfl progDivergesDrop rfl
  rw [← hM] at hoof hSteps
  have := Spine.step_no_double_free M hPT hSteps
  trivial

/-- `diverges_drop` applied to `eval_diverges_iff` (helper). -/
theorem diverges_drop.eval_diverges_iff : True := by
  obtain ⟨M, hM⟩ := Spine.Nonvacuous.exact_model
  obtain ⟨hc, hPT, hoof, C, hSteps, -, -⟩ :=
    Spine.Nonvacuous.diverges_drop bodyDivergesDrop rfl progDivergesDrop rfl
  rw [← hM] at hoof hSteps
  have := (Spine.eval_diverges_iff M hPT).mp hoof
  trivial

/-- `stuck` applied to `fuel_mono` (helper). -/
theorem stuck.fuel_mono : True := by
  obtain ⟨-, hr, C, hS, hSt⟩ := Spine.Nonvacuous.stuck bodyStuck rfl progStuck rfl
  have h300 : run Float.exactOps progStuck 300 = .stuck .useAfterMove := by rfl
  have hne : run Float.exactOps progStuck 200 ≠ .outOfFuel := by rw [hr]; intro h; cases h
  have := Spine.fuel_mono Float.exactOps (Nat.le_succ 200) hne
  trivial

/-- `stuck` applied to `no_masking` (helper). -/
theorem stuck.no_masking : True := by
  obtain ⟨-, hr, C, hS, hSt⟩ := Spine.Nonvacuous.stuck bodyStuck rfl progStuck rfl
  have h300 : run Float.exactOps progStuck 300 = .stuck .useAfterMove := by rfl
  have hne : run Float.exactOps progStuck 200 ≠ .outOfFuel := by rw [hr]; intro h; cases h
  have := Spine.no_masking Float.exactOps hr (m := 300) (show run Float.exactOps progStuck 300 ≠ .outOfFuel by rw [h300]; intro h; cases h)
  trivial

/-- `stuck` applied to `Config.trichotomy` (helper). -/
theorem stuck.Config.trichotomy : True := by
  obtain ⟨-, hr, C, hS, hSt⟩ := Spine.Nonvacuous.stuck bodyStuck rfl progStuck rfl
  have h300 : run Float.exactOps progStuck 300 = .stuck .useAfterMove := by rfl
  have hne : run Float.exactOps progStuck 200 ≠ .outOfFuel := by rw [hr]; intro h; cases h
  have := Spine.Config.trichotomy Float.exactOps progStuck C
  trivial

/-- `stuck` applied to `Config.stuck_iff` (helper). -/
theorem stuck.Config.stuck_iff : True := by
  obtain ⟨-, hr, C, hS, hSt⟩ := Spine.Nonvacuous.stuck bodyStuck rfl progStuck rfl
  have h300 : run Float.exactOps progStuck 300 = .stuck .useAfterMove := by rfl
  have hne : run Float.exactOps progStuck 200 ≠ .outOfFuel := by rw [hr]; intro h; cases h
  have := Spine.Config.stuck_iff.mpr ⟨_, hSt⟩
  trivial

/-- `stuck` applied to `step_stuck_isStuckState` (helper). -/
theorem stuck.step_stuck_isStuckState : True := by
  obtain ⟨-, hr, C, hS, hSt⟩ := Spine.Nonvacuous.stuck bodyStuck rfl progStuck rfl
  have h300 : run Float.exactOps progStuck 300 = .stuck .useAfterMove := by rfl
  have hne : run Float.exactOps progStuck 200 ≠ .outOfFuel := by rw [hr]; intro h; cases h
  have := Spine.step_stuck_isStuckState hSt
  trivial

/-- `stuck` applied to `run_stuck_of_step_stuck` (helper). -/
theorem stuck.run_stuck_of_step_stuck : True := by
  obtain ⟨-, hr, C, hS, hSt⟩ := Spine.Nonvacuous.stuck bodyStuck rfl progStuck rfl
  have h300 : run Float.exactOps progStuck 300 = .stuck .useAfterMove := by rfl
  have hne : run Float.exactOps progStuck 200 ≠ .outOfFuel := by rw [hr]; intro h; cases h
  have := Spine.run_stuck_of_step_stuck Float.exactOps progStuck hS hSt
  trivial

end RueCore.Nonvacuous.Glue
