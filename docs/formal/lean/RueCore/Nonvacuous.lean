module

public import RueCore.Float.Lemmas
public import RueCore.Checker
public import RueCore.Soundness
public import RueCore.Trace
public import RueCore.Adequacy

@[expose] public section

/-!
# RueCore.Nonvacuous — the witness statements, proved (layer L2)

Each theorem here proves the Spec statement of the same name in
`RueCore/Spec/Witnesses.lean` (`RueCore.Spec.Nonvacuous.<name>_stmt`), stated
word for word, and `Spine.lean` binds it to that statement. The programs are
written out in the statements, so this module defines none of its own; the
proofs run the checker and `eval` in the kernel (`rfl`, `decide`, and
`decide +kernel` for the float program, whose rounding reaches `2^1076`, past
the elaborator's evaluation threshold; never `native_decide`), and get the
rest from the spine's own theorems: `checkProgram_sound`, `check_sound`,
`eval_sound`, `no_violation` and `WfDecls.dtorNotCopy`, at
`Float.exactModel` (`Float/Lemmas.lean`).
-/

namespace RueCore.Nonvacuous

/-- Prefixing an empty trace changes nothing (helper). -/
theorem withTrace_nil (r : EvalRes) : r.withTrace [] = r := by
  cases r <;> rfl

/-- The float an `eval` result returns, with its width, if it returns one:
decidable, so the kernel can evaluate a float program's run where the
elaborator's `rfl` would stop at `2^1076` (helper). -/
def okFloat? : EvalRes → Option (FloatWidth × FloatDatum)
  | .ok _ (.float w f) _ => some (w, f)
  | _ => none

/-- A result `okFloat?` reads a float off is a returned float (helper). -/
theorem of_okFloat {r : EvalRes} {w : FloatWidth} {f : FloatDatum}
    (h : okFloat? r = some (w, f)) : ∃ H tr, r = .ok H (.float w f) tr := by
  match r, h with
  | .ok H (.float _ _) tr, h => cases h; exact ⟨H, tr, rfl⟩

/-- `loop { () }` exhausts every fuel, from every store and frame: each turn
spends one unit and the body never breaks (helper). -/
theorem loopUnit_eval (M : FloatOps) (P : Program) :
    ∀ n H φ, eval M n P H φ (.loop .unitLit) = .outOfFuel
  | 0, _, _ => rfl
  | 1, _, _ => rfl
  | n + 2, H, φ => by
      show (eval M (n + 1) P H φ (.loop .unitLit)).withTrace [] = _
      rw [loopUnit_eval M P (n + 1) H φ]; rfl

/-- The program whose entry point is `loop { () }` exhausts every fuel
(helper). -/
theorem loopUnit_run : ∀ fuel, run Float.exactOps
    { decls := { structs := [], enums := [] },
      fns := [{ params := [], ret := .unit, body := .loop .unitLit }] } fuel = .outOfFuel
  | 0 => rfl
  | n + 1 => by
      show EvalRes.withTrace [] (EvalRes.absorb (eval Float.exactOps n _ [] _ (.loop .unitLit)) _) = _
      rw [loopUnit_eval]; rfl


/-- `Spec.Nonvacuous.exact_model_stmt`, proved. -/
theorem exact_model :
    ∃ M : FloatModel, M.toFloatOps = Float.exactOps := ⟨Float.exactModel, rfl⟩

/-- `Spec.Nonvacuous.empty_frame_stmt`, proved. -/
theorem empty_frame :
    ∀ D : Decls, FrameMatches D [] Frame.empty [] ∧ StoreCC D [] :=
  fun _ => ⟨frameMatches_empty, fun _ _ hc => by simp at hc⟩

/-- `Spec.Nonvacuous.dtor_stmt`, proved. -/
theorem dtor :
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
        2 ≤ (freedIds P.decls tr).length ∧ 2 ≤ (dtorIds tr).length := by
  intro B hB P hPe
  subst hB
  have h1 : checkProgram P = true := by rw [hPe]; rfl
  have hP := checkProgram_sound h1
  subst hPe
  exact ⟨h1, hP, by rfl, ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    WfDecls.dtorNotCopy hP.wf.decls, ⟨_, step_iff.mpr rfl⟩, no_violation Float.exactModel hP,
    by decide, ⟨_, _, _, _, ⟨_, rfl, by rfl⟩, (withTrace_nil _).symm, by decide⟩,
    _, _, _, by rfl, (eval_sound Float.exactModel hP 200).2.1 _ _ _ (by rfl), by decide, by decide⟩

/-- `Spec.Nonvacuous.linear_stmt`, proved. -/
theorem linear :
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
        2 ≤ (freedIds P.decls tr).length := by
  intro B hB P hPe
  subst hB
  have h1 : checkProgram P = true := by rw [hPe]; rfl
  have hP := checkProgram_sound h1
  subst hPe
  exact ⟨h1, hP, by rfl, ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    _, _, _, by rfl, (eval_sound Float.exactModel hP 200).2.1 _ _ _ (by rfl), by decide⟩

/-- `Spec.Nonvacuous.loop_stmt`, proved. -/
theorem loop :
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
        3 ≤ (dtorIds tr).length := by
  intro B hB P hPe
  subst hB
  have h1 : checkProgram P = true := by rw [hPe]; rfl
  have hP := checkProgram_sound h1
  subst hPe
  exact ⟨h1, hP, by rfl, ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    _, _, _, by rfl, (eval_sound Float.exactModel hP 200).2.1 _ _ _ (by rfl), by decide⟩

/-- `Spec.Nonvacuous.array_stmt`, proved. -/
theorem array :
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
        2 ≤ (dtorIds tr).length := by
  intro B hB P hPe
  subst hB
  have h1 : checkProgram P = true := by rw [hPe]; rfl
  have hP := checkProgram_sound h1
  subst hPe
  exact ⟨h1, hP, by rfl, ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    _, _, _, by rfl, (eval_sound Float.exactModel hP 200).2.1 _ _ _ (by rfl), by decide⟩

/-- `Spec.Nonvacuous.enum_match_stmt`, proved. -/
theorem enum_match :
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
        2 ≤ (freedIds P.decls tr).length ∧ 1 ≤ (dtorIds tr).length := by
  intro B hB P hPe
  subst hB
  have h1 : checkProgram P = true := by rw [hPe]; rfl
  have hP := checkProgram_sound h1
  subst hPe
  exact ⟨h1, hP, by rfl, ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    _, _, _, by rfl, (eval_sound Float.exactModel hP 200).2.1 _ _ _ (by rfl), by decide, by decide⟩

/-- `Spec.Nonvacuous.early_return_stmt`, proved. -/
theorem early_return :
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
        v = .int .w64 .signed 7 ∧ 2 ≤ (dtorIds tr).length := by
  intro B hB P hPe
  subst hB
  have h1 : checkProgram P = true := by rw [hPe]; rfl
  have hP := checkProgram_sound h1
  subst hPe
  exact ⟨h1, hP, by rfl, ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    _, _, _, by rfl, (eval_sound Float.exactModel hP 200).2.1 _ _ _ (by rfl), rfl, by decide⟩

/-- `Spec.Nonvacuous.panic_stmt`, proved. -/
theorem panic :
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
        Steps Float.exactOps P Config.init (.panic .user [.dbg (.int .w64 .signed 5)]) := by
  intro B hB P hPe
  subst hB
  have h1 : checkProgram P = true := by rw [hPe]; rfl
  have hP := checkProgram_sound h1
  subst hPe
  exact ⟨h1, hP, by rfl, ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    by rfl, (eval_sound Float.exactModel hP 200).2.2 _ _ (by rfl)⟩

/-- `Spec.Nonvacuous.float_stmt`, proved. -/
theorem float :
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
        v = .float .w64 (.num false 15 (-1)) := by
  intro B hB P hPe
  subst hB
  have h1 : checkProgram P = true := by rw [hPe]; rfl
  have hP := checkProgram_sound h1
  have hf : okFloat? (run Float.exactOps P 200) = some (.w64, .num false 15 (-1)) := by
    rw [hPe]; decide +kernel
  obtain ⟨H, tr, hr⟩ := of_okFloat hf
  subst hPe
  exact ⟨h1, hP, by rfl, ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    H, _, tr, hr, (eval_sound Float.exactModel hP 200).2.1 _ _ _ hr, rfl⟩

/-- `Spec.Nonvacuous.diverges_stmt`, proved. -/
theorem diverges :
    ∀ P : Program, P =
      { decls := { structs := [], enums := [] },
        fns := [{ params := [], ret := .unit, body := .loop .unitLit }] } →
      checkProgram P = true ∧ ProgramTyped P ∧ ∀ fuel, run Float.exactOps P fuel = .outOfFuel := by
  intro P hPe
  have h1 : checkProgram P = true := by rw [hPe]; rfl
  subst hPe
  exact ⟨h1, checkProgram_sound h1, loopUnit_run⟩

/-- `Spec.Nonvacuous.stuck_stmt`, proved. -/
theorem stuck :
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
        ∃ C, Steps Float.exactOps P Config.init C ∧ C.Stuck Float.exactOps P .useAfterMove := by
  intro B hB P hPe
  subst hB
  subst hPe
  exact ⟨by rfl, by rfl, _, stepN_steps (n := 100), by rfl⟩

end RueCore.Nonvacuous
