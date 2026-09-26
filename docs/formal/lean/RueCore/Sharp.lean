module

public import RueCore.Float.Lemmas
public import RueCore.Checker
public import RueCore.Soundness
public import RueCore.Trace
public import RueCore.TraceOrder
public import RueCore.Adequacy
public import RueCore.Retire
public import RueCore.TracePrefix
public import RueCore.Nonvacuous

@[expose] public section

/-!
# RueCore.Sharp — the counter-examples, proved (layer L2)

Each theorem here proves the Spec statement of the same name in
`RueCore/Spec/Sharp.lean` (`RueCore.Spec.Sharp.<name>_stmt`), stated word for
word, and `Spine.lean` binds it to that statement (RUE-2485). The programs are
written out in the statements; the helpers below are general facts about
runs, and the program of `discard_loop`, whose run is infinite. The proofs run
the checker, `eval` and `step` in the kernel (`rfl`, `decide`; never
`native_decide`). A dropped hypothesis is shown false from the spine theorem
itself where no direct proof is short: for a program whose other hypotheses
hold and whose conclusion fails, the spine theorem leaves the dropped
hypothesis no way to hold (`¬ ProgramTyped P` from `no_use_after_move` and a
refusal, `¬ Typed` from `soundness` and a refusal, and so on).
-/

namespace RueCore.Sharp

/-- The model the statements run on is `Float.exactOps` (helper). -/
theorem exact_ops : Float.exactModel.toFloatOps = Float.exactOps := rfl

/-- Prefixing an empty trace changes nothing (helper). -/
theorem withTrace_nil (r : EvalRes) : r.withTrace [] = r := by
  cases r <;> rfl

/-- A stuck configuration takes no step (helper). -/
theorem noStep_of_stuck {M : FloatOps} {P : Program} {C : Config} {w : Violation}
    (h : C.Stuck M P w) : ∀ C', ¬ Step M P C C' :=
  (Config.stuck_iff.mpr ⟨w, h⟩).2

/-- A stuck configuration is not terminal (helper). -/
theorem not_terminal_of_stuck {M : FloatOps} {P : Program} {C : Config} {w : Violation}
    (h : C.Stuck M P w) : ¬ C.Terminal :=
  (Config.stuck_iff.mpr ⟨w, h⟩).1

/-- A terminal configuration takes no step (helper). -/
theorem noStep_of_terminal {M : FloatOps} {P : Program} {C : Config} (h : C.Terminal) :
    ∀ C', ¬ Step M P C C' := fun _ s => Step.terminal h s

/-- Of two configurations with no step, a run from `Config.init` reaches at
most one (`Step.det`) (helper). -/
theorem not_steps_of_final {M : FloatOps} {P : Program} {T X : Config}
    (hT : Steps M P Config.init T) (hTf : ∀ C', ¬ Step M P T C') (hXf : ∀ C', ¬ Step M P X C')
    (hne : T ≠ X) : ¬ Steps M P Config.init X :=
  fun hX => hne (Steps.final_unique hT hX hTf hXf)

/-- An answer other than `outOfFuel` is `run`'s at every larger fuel
(`fuel_mono`) (helper). -/
theorem run_from {M : FloatOps} {P : Program} {n : Nat} {r : EvalRes} (h : run M P n = r)
    (hr : r ≠ .outOfFuel) : ∀ fuel, n ≤ fuel → run M P fuel = r := by
  intro fuel hf
  have := fuel_mono M (P := P) (H := []) (φ := { env := [], scope := [] }) (e := .call 0 []) hf (by
    show run M P n ≠ .outOfFuel
    rw [h]; exact hr)
  exact this.trans h

/-- Once `run` answers `r`, no property `r` lacks holds of `run`'s answer at
every fuel past a bound (helper). -/
theorem not_eventually {M : FloatOps} {P : Program} {n : Nat} {r : EvalRes} (h : run M P n = r)
    (hr : r ≠ .outOfFuel) (Q : EvalRes → Prop) (hQ : ¬ Q r) :
    ¬ ∃ k, ∀ fuel, k < fuel → Q (run M P fuel) := by
  rintro ⟨k, hk⟩
  have := hk (k + n + 1) (by omega)
  rw [run_from h hr _ (by omega)] at this
  exact hQ this

/-- A refusal is none of `run_safe`'s outcomes, for any entry point (helper). -/
theorem stuck_not_safe {M : FloatOps} {P : Program} {n : Nat} {w : Violation}
    (h : run M P n = .stuck w) (fd : FnDef) :
    ¬ (run M P n = .outOfFuel ∨ (∃ k tr, run M P n = .panic k tr) ∨
      ∃ H v tr, run M P n = .ok H v tr ∧ HasTy P.decls v fd.ret) := by
  rw [h]; rintro (h | ⟨_, _, h⟩ | ⟨_, _, _, h, _⟩) <;> cases h

/-- `Exact` fails at a value whose counts miss one identity the evaluation
started with (helper). -/
theorem not_exact_ok {D : Decls} {H H' : Store} {v : Val} {tr : List Event} {Y : List Nat}
    (a : Nat) (ha : a < H.length)
    (hne : (storeOwn D H').count a + (v.own D).count a + (freedIds D tr).count a ≠
      (storeOwn D H).count a + Y.count a) : ¬ Exact D H Y (.ok H' v tr) :=
  fun h => hne (h.2.2.2 a ha)

/-- The same at an unwinding `return` (helper). -/
theorem not_exact_returned {D : Decls} {H H' : Store} {v : Val} {tr : List Event} {Y : List Nat}
    (a : Nat) (ha : a < H.length)
    (hne : (storeOwn D H').count a + (v.own D).count a + (freedIds D tr).count a ≠
      (storeOwn D H).count a + Y.count a) : ¬ Exact D H Y (.returned H' v tr) :=
  fun h => hne (h.2.2.2 a ha)

/-- `Exact` fails at a value whose final store is not copy-closed (helper). -/
theorem not_exact_cc {D : Decls} {H H' : Store} {v : Val} {tr : List Event} {Y : List Nat}
    (h : ¬ StoreCC D H') : ¬ Exact D H Y (.ok H' v tr) :=
  fun he => h he.2.1

/-- A one-cell store is copy-closed when its cell is (helper). -/
theorem storeCC_one {D : Decls} {c : Contents} (h : c.copyClosed D = true) : StoreCC D [.full c] := by
  intro ℓ c' h'
  match ℓ, h' with
  | 0, h' => cases h'; exact h

/-- In a run with a longer one beside it from the same start, the shorter
one's end steps (`Step.det`) (helper). -/
theorem StepsN.steps_of_longer {M : FloatOps} {P : Program} :
    ∀ {k : Nat} {C₀ C D : Config}, StepsN M P k C₀ C → StepsN M P (k + 1) C₀ D →
      ∃ C', Step M P C C'
  | 0, _, _, _, h, h' => by
      cases h
      cases h' with
      | step s _ => exact ⟨_, s⟩
  | k + 1, _, _, _, h, h' => by
      cases h with
      | step s rest =>
        cases h' with
        | step s' rest' =>
          rw [Step.det s s'] at rest
          exact StepsN.steps_of_longer rest rest'

/-- A machine with runs of every length never reaches a configuration that
does not step (helper). -/
theorem steps_of_forever {M : FloatOps} {P : Program}
    (h : ∀ n, ∃ D, StepsN M P n Config.init D) :
    ∀ C, Steps M P Config.init C → C.Terminal ∨ ∃ C', Step M P C C' := by
  intro C hC
  obtain ⟨k, hk⟩ := Steps.toN hC
  obtain ⟨D, hD⟩ := h (k + 1)
  exact .inr (StepsN.steps_of_longer hk hD)

/-- `discard_loop`'s loop body, `S1 { 3 }; ()` (helper). -/
abbrev loopBody : Expr := .seq (.mkStruct 1 [.intLit .w64 .signed 3]) .unitLit

/-- `discard_loop`'s program (helper). -/
abbrev loopProg : Program :=
  { decls :=
      { structs :=
          [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
            { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
        enums := [{ variants := [[.struct 0], []], cls := .affine }] },
    fns := [{ params := [], ret := .unit, body := .loop loopBody }] }

/-- `discard_loop`'s configuration at the head of a turn, over any store and
trace (helper). -/
def loopTurn (H : Store) (tr : List Event) : Config :=
  .run H Frame.empty [.loop loopBody Frame.empty, .call Frame.empty] (.eval loopBody) tr

/-- One turn is nine steps of §6's relation, which has no monitor: the
literal is minted into a reserved slot, discarded, and the loop starts again
(helper). -/
theorem loopTurn_step (M : FloatOps) (H : Store) (tr : List Event) :
    StepsN M loopProg 9 (loopTurn H tr)
      (loopTurn (H ++ [.dead]) ((tr ++ [.dropTemp (.struct 1 H.length [.int .w64 .signed 3])]) ++ [])) := by
  repeat (refine .step (step_iff.mpr rfl) ?_)
  exact .refl _

/-- So the loop has runs of every multiple of nine steps from any turn
(helper). -/
theorem loopTurn_forever (M : FloatOps) :
    ∀ m H tr, ∃ D, StepsN M loopProg (9 * m) (loopTurn H tr) D
  | 0, _, _ => ⟨_, .refl _⟩
  | m + 1, H, tr => by
      obtain ⟨D, hD⟩ := loopTurn_forever M m _ _
      exact ⟨D, by rw [show 9 * (m + 1) = 9 + 9 * m by omega]; exact (loopTurn_step M H tr).trans hD⟩

/-- And runs of every length from `Config.init` (helper). -/
theorem loop_forever (M : FloatOps) : ∀ n, ∃ D, StepsN M loopProg n Config.init D := by
  intro n
  have h3 : StepsN M loopProg 3 Config.init (loopTurn [] []) := by
    repeat (refine .step (step_iff.mpr rfl) ?_)
    exact .refl _
  obtain ⟨D, hD⟩ := loopTurn_forever M n [] []
  exact (h3.trans hD).prefix (by omega)

/-- `Spec.Sharp.stuck_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
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
        ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧ HasTy P.decls v (.int .w64 .signed)) := by
  intro B hB P hP
  have hr : eval Float.exactOps 200 P [] Frame.empty (.call 0 []) = .stuck .useAfterMove := by
    subst hB hP; rfl
  have hrun : run Float.exactOps P 200 = .stuck .useAfterMove := hr
  obtain ⟨c, Ω, hc, hf, ht⟩ : ∃ c Ω, check P (.int .w64 .signed) [] (.call 0 []) = some (c, Ω) ∧
      c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [] (.call 0 []) (.int .w64 .signed) Ω := by
    subst hB hP; exact ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩
  refine ⟨by subst hB hP; rfl, fun h => no_use_after_move Float.exactModel h 200 hrun, fun hw => ?_,
    ⟨_, by subst hB hP; rfl, rfl⟩, by subst hB hP; rfl, rfl, frameMatches_empty,
    fun _ _ h => by simp at h, ⟨c, Ω, hc, hf, ht, by rw [hr]; exact id⟩, rfl, hr,
    by subst hB hP; rfl, by subst hB hP; rfl, fun _ => rfl, hrun, by subst hP; rfl, stuck_not_safe hrun ⟨[], .int .w64 .signed, .unitLit⟩⟩
  have := soundness Float.exactModel hw 200 ht frameMatches_empty
  rw [exact_ops, hr] at this
  exact this

/-- `Spec.Sharp.stuck_step_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem stuck_step :
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
      ¬ ∀ fuel, ∃ w', run Float.exactOps P fuel = .stuck w' := by
  intro B hB P hP
  have hrun : run Float.exactOps P 200 = .stuck .useAfterMove := by subst hB hP; rfl
  have h0 : run Float.exactOps P 0 = .outOfFuel := by subst hP; rfl
  obtain ⟨C, hC, hst⟩ : ∃ C, Steps Float.exactOps P Config.init C ∧ C.Stuck Float.exactOps P .useAfterMove := by
    subst hB hP; exact ⟨_, stepN_steps (n := 100), by rfl⟩
  have hnoC := noStep_of_stuck hst
  have hnt := not_terminal_of_stuck hst
  refine ⟨fun h => no_use_after_move Float.exactModel h 200 hrun, ⟨C, hC, hst⟩, hrun, h0, fun h => ?_, ?_, ?_,
    fun h => ?_⟩
  · rcases h C hC with ht | ⟨_, s⟩
    · exact hnt ht
    · exact hnoC _ s
  · rintro ⟨fd, _, hs⟩
    rcases (hs Config.init (.refl _)).1 C hC with ht | ⟨_, s⟩
    · exact hnt ht
    · exact hnoC _ s
  · rintro ⟨fd, _, hs⟩
    obtain ⟨k, hk⟩ := Steps.toN hC
    rcases hs (k + 1) with ⟨D, hD⟩ | ⟨H, v, tr, hR, _⟩ | ⟨κ, tr, hR⟩
    · exact absurd (StepsN.bound hk hnoC hD) (by omega)
    · have := Steps.final_unique hC hR hnoC (noStep_of_terminal trivial)
      subst this
      exact hnt trivial
    · have := Steps.final_unique hC hR hnoC (noStep_of_terminal trivial)
      subst this
      exact hnt trivial
  · obtain ⟨w, hw⟩ := h 0
    rw [h0] at hw
    cases hw

/-- `Spec.Sharp.typed_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem typed :
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
        (eval Float.exactOps 200 P [] Frame.empty e) := by
  intro B hB P hP e he
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hr : eval Float.exactOps 200 P [] Frame.empty e = .stuck .useAfterMove := by subst he hB hP; rfl
  have hnt : ∀ (T : Ty) (Ω : Out), ¬ Typed P (.int .w64 .signed) [] e T Ω := fun T Ω ht => by
    have := soundness Float.exactModel hPT.wf 200 ht frameMatches_empty
    rw [exact_ops, hr] at this
    exact this
  exact ⟨hPT, hPT.wf, by subst hB hP; rfl, by subst he; rfl, frameMatches_empty,
    fun _ _ h => by simp at h, by subst he hB hP; rfl, hnt, by subst he hB hP; exact ⟨_, rfl, by rfl⟩,
    hr, by subst he hB hP; rfl, by subst he hB hP; rfl, rfl, fun T Ω => by rw [hr]; exact id⟩

/-- `Spec.Sharp.frame_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem frame :
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
      eval Float.exactOps 201 P [] Frame.empty e = (EvalRes.stuck .unbound).withTrace [] := by
  intro B hB P hP e he
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hr : eval Float.exactOps 200 P [] Frame.empty e = .stuck .unbound := by subst he hB hP; rfl
  obtain ⟨c, Ω, hc, hf, ht⟩ : ∃ c Ω, check P (.int .w64 .signed) [{ ty := .int .w64 .signed, mu := false, st := .owned }] e = some (c, Ω) ∧
      c.fits (.int .w64 .signed) = true ∧ Typed P (.int .w64 .signed) [{ ty := .int .w64 .signed, mu := false, st := .owned }] e (.int .w64 .signed) Ω := by
    subst he hB hP; exact ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩
  refine ⟨hPT, hPT.wf, by subst hB hP; rfl, by subst he; rfl, fun _ _ h => by simp at h,
    ⟨c, Ω, hc, hf, ht, by rw [hr]; exact id⟩, fun hfm => ?_, by subst he; exact ⟨_, rfl, by rfl⟩,
    hr, by subst he hB hP; rfl, by subst he hB hP; rfl⟩
  have := soundness Float.exactModel hPT.wf 200 ht hfm
  rw [exact_ops, hr] at this
  exact this

/-- `Spec.Sharp.no_entry_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem no_entry :
  ∀ P : Program, P =
      { decls :=
          { structs :=
              [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine },
                { attr := .linear, fields := [.int .w64 .signed], dtor := false, cls := .linear }],
            enums := [{ variants := [[.struct 0], []], cls := .affine }] },
        fns := [] } →
      WfProgram P ∧ P.fns[0]? = none ∧ run Float.exactOps P 200 = .stuck .unbound ∧
      ∀ fd : FnDef, ¬ (run Float.exactOps P 200 = .outOfFuel ∨ (∃ k tr, run Float.exactOps P 200 = .panic k tr) ∨
        ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧ HasTy P.decls v fd.ret) := by
  intro P hP
  have hr : run Float.exactOps P 200 = .stuck .unbound := by subst hP; rfl
  refine ⟨⟨by subst hP; exact checkDecls_sound (by rfl), fun fd h => by subst hP; simp at h⟩,
    by subst hP; rfl, hr, stuck_not_safe hr⟩

/-- `Spec.Sharp.entry_param_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem entry_param :
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
      run Float.exactOps P 200 = .stuck .typeConfusion := by
  intro P hP
  have hwf : WfProgram P := by
    subst hP
    refine ⟨checkDecls_sound (by rfl), fun fd h => ?_⟩
    simp only [List.mem_singleton] at h
    subst h
    exact checkFn_sound (by rfl)
  have hr : run Float.exactOps P 200 = .stuck .typeConfusion := by subst hP; rfl
  have hnpt : ¬ ProgramTyped P := fun h => by
    obtain ⟨fd, hfd, hp⟩ := h.entry
    subst hP
    simp only [List.getElem?_cons_zero, Option.some.injEq] at hfd
    subst hfd
    simp at hp
  exact ⟨hwf, hnpt,
    ⟨{ params := [{ ty := .int .w64 .signed, mu := false }], ret := .int .w64 .signed, body := .use (.var 0) }, by subst hP; rfl, by simp, stuck_not_safe hr _⟩, hr⟩

/-- `Spec.Sharp.copy_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem copy :
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
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ run Float.exactOps P 200 = .stuck .ownedUnderCopy := by
  intro B hB P hP
  have hr : run Float.exactOps P 200 = .stuck .ownedUnderCopy := by subst hB hP; rfl
  exact ⟨by subst hB hP; rfl, fun h => no_violation Float.exactModel h 200 _ hr, hr⟩

/-- `Spec.Sharp.leak_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem leak :
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
        ¬ ∃ n, ∀ fuel, n < fuel → run Float.exactOps P fuel = .ok H v tr := by
  intro B hB P hP
  have hr : run Float.exactOps P 200 = .stuck .linearLeak := by subst hB hP; rfl
  obtain ⟨H, v, tr, hs⟩ : ∃ H v tr, Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) := by
    subst hB hP; exact ⟨_, _, _, stepN_steps (n := 100)⟩
  exact ⟨by subst hB hP; rfl, fun h => no_linear_leak Float.exactModel h 200 hr, hr, H, Frame.empty, v, tr, hs,
    not_eventually hr (by simp) (· = .ok H v tr) (by simp)⟩

/-- `Spec.Sharp.overwrite_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem overwrite :
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
      checkProgram P = false ∧ ¬ ProgramTyped P ∧ run Float.exactOps P 200 = .stuck .linearOverwrite := by
  intro B hB P hP
  have hr : run Float.exactOps P 200 = .stuck .linearOverwrite := by subst hB hP; rfl
  exact ⟨by subst hB hP; rfl, fun h => no_linear_overwrite Float.exactModel h 200 hr, hr⟩

/-- `Spec.Sharp.discard_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem discard :
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
        ¬ ∃ n, ∀ fuel, n < fuel → run Float.exactOps P fuel = .panic κ tr := by
  intro B hB P hP
  have hr : run Float.exactOps P 200 = .stuck .linearDiscard := by subst hB hP; rfl
  obtain ⟨κ, tr, hs⟩ : ∃ κ tr, Steps Float.exactOps P Config.init (.panic κ tr) := by
    subst hB hP; exact ⟨_, _, stepN_steps (n := 100)⟩
  exact ⟨by subst hB hP; rfl, fun h => no_linear_discard Float.exactModel h 200 hr, hr, κ, tr, hs,
    not_eventually hr (by simp) (· = .panic κ tr) (by simp)⟩

/-- `Spec.Sharp.discard_loop_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem discard_loop :
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
        ∀ C, Steps Float.exactOps P Config.init C → C.Terminal ∨ ∃ C', Step Float.exactOps P C C') := by
  intro B hB P hP
  have hr : run Float.exactOps P 200 = .stuck .linearDiscard := by subst hB hP; rfl
  have hf : ∀ n, ∃ D, StepsN Float.exactOps P n Config.init D := by subst hB hP; exact loop_forever _
  have hs := steps_of_forever hf
  refine ⟨by subst hB hP; rfl, fun h => no_linear_discard Float.exactModel h 200 hr, hr, hf, hs, fun h => ?_,
    fun h => (h.mpr hs) 200 _ hr⟩
  have := (h.mpr hf) 200
  rw [hr] at this
  cases this

/-- `Spec.Sharp.fuel_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem fuel :
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
        ¬ ∀ fuel, run Float.exactOps P fuel = .ok H v tr ∨ ∃ w, run Float.exactOps P fuel = .stuck w := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have h0 : run Float.exactOps P 0 = .outOfFuel := by subst hP; rfl
  obtain ⟨H, v, tr, hr⟩ : ∃ H v tr, run Float.exactOps P 200 = .ok H v tr := by
    subst hB hP; exact ⟨_, _, _, by rfl⟩
  refine ⟨hPT, fun _ => rfl, h0, H, v, tr, hr, (eval_sound Float.exactModel hPT 200).2.1 _ _ _ hr, by omega, by omega,
    by rw [h0, hr]; simp, fun w => by rw [hr]; simp, fun h => ?_⟩
  rcases h 0 with h' | ⟨w, h'⟩ <;> rw [h0] at h' <;> cases h'

/-- `Spec.Sharp.fuel_panic_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem fuel_panic :
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
        ∃ w, run Float.exactOps P fuel = .stuck w := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have h0 : run Float.exactOps P 0 = .outOfFuel := by subst hP; rfl
  have hr : run Float.exactOps P 200 = .panic .user [.dbg (.int .w64 .signed 5)] := by subst hB hP; rfl
  refine ⟨hPT, h0, (eval_sound Float.exactModel hPT 200).2.2 _ _ hr, fun h => ?_⟩
  rcases h 0 with h' | ⟨w, h'⟩ <;> rw [h0] at h' <;> cases h'

/-- `Spec.Sharp.not_fits_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem not_fits :
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
        c.fits .bool = false ∧ ¬ Typed P (.int .w64 .signed) [] (.intLit .w64 .signed 1) .bool Ω := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  obtain ⟨c, Ω, hc, hf⟩ : ∃ c Ω, check P (.int .w64 .signed) [] (.intLit .w64 .signed 1) = some (c, Ω) ∧
      c.fits .bool = false := by
    subst hB hP; exact ⟨_, _, by rfl, by rfl⟩
  refine ⟨hPT, c, Ω, hc, hf, fun ht => ?_⟩
  have := soundness Float.exactModel hPT.wf 200 ht frameMatches_empty
  have hr : eval Float.exactOps 200 P [] Frame.empty (.intLit .w64 .signed 1) =
      .ok [] (.int .w64 .signed 1) [] := by subst hP; rfl
  rw [exact_ops, hr] at this
  revert this
  cases Ω.norm with
  | none => exact id
  | some Γ' => exact fun h => by cases h.1

/-- `Spec.Sharp.double_drop_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem double_drop :
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
        ¬ (∀ a, (dtorIds (run Float.exactOps P 200).trace).count a ≤ 1) := by
  intro B hB P hP
  obtain ⟨H, v, tr, hr, hc⟩ : ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧ (dtorIds tr).count 0 = 2 := by
    subst hB hP; exact ⟨_, _, _, by rfl, by decide⟩
  have hnd : ¬ ∀ a, (dtorIds (run Float.exactOps P 200).trace).count a ≤ 1 := fun h => by
    have := h 0
    rw [hr] at this
    simp only [EvalRes.trace] at this
    omega
  exact ⟨by subst hB hP; rfl, fun hPT => hnd (no_double_free Float.exactModel hPT 200).2.2,
    fun hdt => hdt 0 _ (by subst hB hP; rfl) (by subst hB hP; rfl) (by subst hB hP; rfl),
    H, v, tr, hr, hc, hnd⟩

/-- `Spec.Sharp.bare_dtor_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem bare_dtor :
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
      ∃ H φ v tr, Steps Float.exactOps P Config.init (.run H φ [] (.ret v) tr) ∧ ¬ Blocks P.decls tr := by
  intro B hB P hP
  obtain ⟨H, v, tr, hs, hb⟩ : ∃ H v tr,
      Steps Float.exactOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧ ¬ Blocks P.decls tr := by
    subst hB hP; exact ⟨_, _, _, stepN_steps (n := 100), Blocks.not_dtor⟩
  exact ⟨by subst hB hP; rfl, fun hPT => hb ((drop_order Float.exactModel hPT).1 _ _ _ _ hs),
    H, Frame.empty, v, tr, hs, hb⟩

/-- `Spec.Sharp.pending_program_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem pending_program :
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
        (eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e) := by
  intro P hP e he
  have hPT : ProgramTyped P := checkProgram_sound (by subst hP; rfl)
  have hr : eval Float.exactOps 200 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e = .ok [.full .hole, .dead] (.int .w64 .signed 7) [] := by
    subst he hP; rfl
  have hr' : eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e = .ok [.full .hole, .dead] (.int .w64 .signed 7) [] := by
    subst he hP; rfl
  refine ⟨hPT, by subst hP; rfl, by subst he; rfl, by subst hP; exact ⟨.cons rfl ⟨_, rfl, .owned (.struct rfl (.cons (.int (by decide)) .nil)) rfl⟩ (by simp) .nil, rfl⟩,
    by subst hP; exact storeCC_one rfl,
    by subst he hP; exact ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    by subst he hP; rfl, (withTrace_nil _).symm, ?_, ?_⟩
  · rw [hr]; exact not_exact_ok 0 (by decide) (by subst hP; decide)
  · rw [hr']; exact not_exact_ok 0 (by decide) (by subst hP; decide)

/-- `Spec.Sharp.pending_expr_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem pending_expr :
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
        (eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e) := by
  intro B hB P hP e he
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hr : eval Float.exactOps 200 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e = .returned [.dead] (.int .w64 .signed 7) [] := by
    subst he hB hP; rfl
  have hr' : eval Float.exactOps 201 P [.full (.struct 0 0 [.int .w64 .signed 5])] { env := [0], scope := [0] } e = .returned [.dead] (.int .w64 .signed 7) [] := by
    subst he hB hP; rfl
  refine ⟨hPT, by subst hB hP; rfl, by subst he; rfl, by subst hP; exact ⟨.cons rfl ⟨_, rfl, .owned (.struct rfl (.cons (.int (by decide)) .nil)) rfl⟩ (by simp) .nil, rfl⟩,
    by subst hP; exact storeCC_one rfl,
    by subst he hB hP; exact ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    by subst he; exact ⟨_, rfl, by rfl⟩, (withTrace_nil _).symm, ?_, ?_⟩
  · rw [hr]; exact not_exact_returned 0 (by decide) (by subst hB hP; decide)
  · rw [hr']; exact not_exact_returned 0 (by decide) (by subst hB hP; decide)

/-- `Spec.Sharp.store_cc_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem store_cc :
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
        (eval Float.exactOps 201 P [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] Frame.empty e) := by
  intro B hB P hP e he
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hcc : ¬ StoreCC P.decls [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] := fun h => by
    have := h 0 _ rfl
    subst hB hP
    revert this
    decide
  obtain ⟨tr, hr⟩ : ∃ tr, eval Float.exactOps 200 P [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] Frame.empty e = .ok [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] (.int .w64 .signed 2) tr := by
    subst he hB hP; exact ⟨_, by rfl⟩
  obtain ⟨tr', hr'⟩ : ∃ tr, eval Float.exactOps 201 P [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] Frame.empty e = .ok [.full (.array (.int .w64 .signed) 0 [.struct 0 1 [.int .w64 .signed 1]])] (.int .w64 .signed 2) tr := by
    subst he hB hP; exact ⟨_, by rfl⟩
  refine ⟨hPT, by subst hB hP; rfl, by subst he; rfl, ⟨.nil, rfl⟩, hcc,
    by subst he hB hP; exact ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    by subst he; exact ⟨_, rfl, by rfl⟩, (withTrace_nil _).symm, ?_, ?_⟩
  · rw [hr]; exact not_exact_cc hcc
  · rw [hr']; exact not_exact_cc hcc

/-- `Spec.Sharp.no_lead_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem no_lead :
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
        (eval Float.exactOps 201 P [] Frame.empty B) := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  obtain ⟨H', v, tr, hr, hne⟩ : ∃ H' v tr, eval Float.exactOps 201 P [] Frame.empty B = .ok H' v tr ∧
      (storeOwn P.decls H').count 0 + (v.own P.decls).count 0 + (freedIds P.decls tr).count 0 ≠
        (storeOwn P.decls [.full (.struct 0 0 [.int .w64 .signed 1])]).count 0 +
          (Contents.ownList P.decls (Contents.ofVals [.struct 0 0 [.int .w64 .signed 1]])).count 0 := by
    subst hB hP; exact ⟨_, _, _, by rfl, by decide⟩
  refine ⟨hPT, by subst hB hP; rfl, by subst hB; rfl, frameMatches_empty, fun _ _ h => by simp at h,
    by subst hB hP; exact ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩, ?_,
    (withTrace_nil _).symm, by rw [hr]; exact not_exact_ok 0 (by decide) hne⟩
  subst hB hP
  rintro ⟨v, _, h⟩
  cases h

/-- `Spec.Sharp.no_eval_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem no_eval :
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
        ∀ w, eval Float.exactOps 201 P [] Frame.empty B ≠ (EvalRes.stuck w).withTrace tr := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  obtain ⟨H', v, tr, hr⟩ : ∃ H' v tr, eval Float.exactOps 201 P [] Frame.empty B = .ok H' v tr := by
    subst hB hP; exact ⟨_, _, _, by rfl⟩
  refine ⟨hPT, by subst hB hP; rfl, by subst hB; rfl, frameMatches_empty, fun _ _ h => by simp at h,
    by subst hB hP; exact ⟨_, _, by rfl, by rfl, check_sound _ (by rfl) _ (by rfl)⟩,
    [.dead], [.struct 0 0 [.int .w64 .signed 1]], [], by subst hB hP; exact ⟨_, rfl, by rfl⟩,
    fun w h => ?_⟩
  rw [hr] at h
  cases h

/-- `Spec.Sharp.unreached_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem unreached :
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
        ∃ w, run Float.exactOps P fuel = .stuck w := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  obtain ⟨H, v, tr, hr, hv⟩ : ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧ v = .int .w64 .signed 3 := by
    subst hB hP; exact ⟨_, _, _, by rfl, rfl⟩
  subst hv
  have hs := (eval_sound Float.exactModel hPT 200).2.1 _ _ _ hr
  refine ⟨hPT, not_steps_of_final hs (noStep_of_terminal trivial) (noStep_of_terminal trivial)
    (by simp), by rw [hr]; simp, Blocks.not_dtor,
    not_eventually hr (by simp) (fun r => r = .ok [] (.int .w64 .signed 8) [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])] ∨ ∃ w, r = .stuck w)
      (by simp)⟩

/-- `Spec.Sharp.unreached_panic_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem unreached_panic :
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
        ∃ w, run Float.exactOps P fuel = .stuck w := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  obtain ⟨H, v, tr, hr⟩ : ∃ H v tr, run Float.exactOps P 200 = .ok H v tr := by
    subst hB hP; exact ⟨_, _, _, by rfl⟩
  have hs := (eval_sound Float.exactModel hPT 200).2.1 _ _ _ hr
  refine ⟨hPT, not_steps_of_final hs (noStep_of_terminal trivial)
    (noStep_of_terminal (C := .panic .user [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])]) trivial)
    (by simp), by rw [hr]; simp, Blocks.not_dtor,
    not_eventually hr (by simp) (fun r => r = .panic .user [.dtor 0 (.struct 0 0 [.int .w64 .signed 1])] ∨ ∃ w, r = .stuck w)
      (by simp)⟩

/-- `Spec.Sharp.unordered_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem unordered :
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
        (Config.run [] { env := [], scope := [1, 0] } [] (.eval (.intLit .w64 .signed 1)) []).stack.Pairwise (· < ·) := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hstep : Step Float.exactOps P (.run [] { env := [], scope := [1, 0] } [] (.eval (.intLit .w64 .signed 1)) [])
      (.run [] { env := [], scope := [1, 0] } [] (.ret (.int .w64 .signed 1)) []) :=
    step_iff.mpr (by subst hB hP; rfl)
  have hnp : ¬ (Config.run [] { env := [], scope := [1, 0] } [] (.eval (.intLit .w64 .signed 1)) []).stack.Pairwise
      (· < ·) := by
    simp [Config.stack, Stk]
  refine ⟨hPT, fun hs => ?_, hstep, ?_⟩
  · obtain ⟨_, _, _, _, hp⟩ := (drop_order Float.exactModel hPT).2.2 _ _ hs hstep
    exact hnp hp
  · rintro ⟨_, _, _, _, hp⟩
    exact hnp hp

/-- `Spec.Sharp.not_a_step_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem not_a_step :
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
          (Config.run H Frame.empty [] (.ret v) tr).stack.Pairwise (· < ·) := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  obtain ⟨H, v, tr, hr, htr⟩ : ∃ H v tr, run Float.exactOps P 200 = .ok H v tr ∧ tr ≠ [] := by
    subst hB hP; exact ⟨_, _, _, by rfl, by simp⟩
  refine ⟨hPT, H, v, tr, (eval_sound Float.exactModel hPT 200).2.1 _ _ _ hr, htr,
    noStep_of_terminal (C := .run H Frame.empty [] (.ret v) tr) trivial _, ?_⟩
  rintro ⟨evs, h, _⟩
  simp only [Config.trace] at h
  exact htr (List.append_eq_nil_iff.mp h.symm).1

/-- `Spec.Sharp.init_steps_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem init_steps :
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
      ¬ ∃ n, ∀ fuel, n < fuel → ∃ w', run Float.exactOps P fuel = .stuck w' := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hs : Step Float.exactOps P Config.init (.run [] Frame.empty [] (.args (.call 0) [] []) []) := step_iff.mpr (by subst hB hP; rfl)
  obtain ⟨H, v, tr, hr⟩ : ∃ H v tr, run Float.exactOps P 200 = .ok H v tr := by
    subst hB hP; exact ⟨_, _, _, by rfl⟩
  refine ⟨hPT, hs, fun h => ?_, by simp [Config.init], fun h => h, fun h => ?_, rfl, .refl _,
    not_eventually hr (by simp) (fun r => ∃ w', r = .stuck w') (by simp)⟩
  · have := Step.det hs h
    simp [Config.init] at this
  · have := (step_iff.mp hs).symm.trans h
    cases this

/-- `Spec.Sharp.unreachable_stuck_stmt`, proved: a §7 hypothesis needed (RUE-2485). -/
theorem unreachable_stuck :
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
      ¬ ∃ n, ∀ fuel, n < fuel → ∃ w', run Float.exactOps P fuel = .stuck w' := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hst : ((.run [] Frame.empty [] (.eval (.use (.var 0))) []) : Config).Stuck Float.exactOps P .unbound := by subst hB hP; rfl
  have hnoC := noStep_of_stuck hst
  have hnt := not_terminal_of_stuck hst
  obtain ⟨H, v, tr, hr⟩ : ∃ H v tr, run Float.exactOps P 200 = .ok H v tr := by
    subst hB hP; exact ⟨_, _, _, by rfl⟩
  refine ⟨hPT, hst, fun hs => ?_, no_violation Float.exactModel hPT, ?_, fun T h => ?_, fun h => ?_,
    not_eventually hr (by simp) (fun r => ∃ w', r = .stuck w') (by simp)⟩
  · rcases step_progress Float.exactModel hPT _ hs with h | ⟨_, s⟩
    · exact hnt h
    · exact hnoC _ s
  · rintro (h | ⟨_, s⟩)
    · exact hnt h
    · exact hnoC _ s
  · rcases h.1 _ (.refl _) with h | ⟨_, s⟩
    · exact hnt h
    · exact hnoC _ s
  · rcases h (.run [] Frame.empty [] (.eval (.use (.var 0))) []) with h | ⟨_, s⟩
    · exact hnt h
    · exact hnoC _ s

/-- `Spec.Sharp.retired_cell_stmt`, proved: a §7 hypothesis needed (RUE-2496). -/
theorem retired_cell :
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
      ¬ Steps Float.exactOps P Config.init (.run [.dead] { env := [0], scope := [] } [] (.eval (.use (.var 0))) []) := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  refine ⟨hPT, by subst hB hP; rfl, by subst hB hP; rfl, fun hs => ?_⟩
  exact step_no_use_after_drop Float.exactOps P hs (by subst hB hP; rfl)

/-- `Spec.Sharp.unreached_double_stmt`, proved: a §7 hypothesis needed (RUE-2477). -/
theorem unreached_double :
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
        [.dtor 0 (.struct 0 0 [.int .w64 .signed 1]), .dtor 0 (.struct 0 0 [.int .w64 .signed 1])]).trace).count 0 = 2 := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  refine ⟨hPT, fun hs => ?_, by decide⟩
  have := (step_no_double_free Float.exactModel hPT hs).2 0
  revert this
  decide

/-- `Spec.Sharp.uncut_drop_stmt`, proved: a §7 hypothesis needed (RUE-2500). -/
theorem uncut_drop :
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
          { env := [1, 0], scope := [0, 1] } [.endscope [0]] (.ret (.int .w64 .signed 3)) []).stack.Pairwise (· < ·) := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hstep : Step Float.exactOps P
      (.run [.full (.struct 0 0 [.int .w64 .signed 1]), .full (.struct 0 1 [.int .w64 .signed 2])]
        { env := [1, 0], scope := [0, 1] } [.endscope [0]] (.ret (.int .w64 .signed 3)) [])
      (.run [.dead, .full (.struct 0 1 [.int .w64 .signed 2])] { env := [0], scope := [0] } []
        (.ret (.int .w64 .signed 3))
        [.drop 0 (.struct 0 0 [.int .w64 .signed 1]), .dtor 0 (.struct 0 0 [.int .w64 .signed 1])]) :=
    step_iff.mpr (by subst hB hP; rfl)
  have hnl : ¬ Lifo [0, 1] [0] [0] := by
    rintro (h | ⟨-, h⟩)
    · exact absurd h.length_le (by decide)
    · exact absurd (h.subset (List.mem_singleton_self 0)) (by decide)
  have hno : ¬ ∃ evs,
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
        { env := [1, 0], scope := [0, 1] } [.endscope [0]] (.ret (.int .w64 .signed 3)) []).stack.Pairwise
        (· < ·) := by
    rintro ⟨evs, h, -, hl, -⟩
    simp only [Config.trace, List.nil_append] at h
    subst h
    exact hnl hl
  refine ⟨hPT, fun hs => ?_, hstep, .inl ⟨0, fun _ hx => List.mem_singleton.mp hx⟩, by decide, hnl, hno⟩
  exact hno ((drop_order Float.exactModel hPT).2.2 _ _ hs hstep)

/-- `Spec.Sharp.ill_typed_halt_stmt`, proved: a §7 hypothesis needed (RUE-2500). -/
theorem ill_typed_halt :
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
        (.int .w64 .signed) := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hns : ¬ ((.run [] Frame.empty [] (.ret (.bool true)) []) : Config).SafeAt Float.exactOps P
      (.int .w64 .signed) := fun h => nomatch h.2 _ _ _ _ (.refl _)
  refine ⟨hPT, trivial, fun hs => ?_, hns⟩
  obtain ⟨fd, hfd, hsafe⟩ := step_preservation Float.exactModel hPT
  subst hB hP
  simp only [List.getElem?_cons_zero, Option.some.injEq] at hfd
  subst hfd
  exact hns (hsafe _ hs)

/-- `Spec.Sharp.out_of_range_halt_stmt`, proved: a §7 hypothesis needed (RUE-2500). -/
theorem out_of_range_halt :
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
        Float.exactOps P (.int .w64 .signed) := by
  intro B hB P hP
  have hPT : ProgramTyped P := checkProgram_sound (by subst hB hP; rfl)
  have hty : ¬ HasTy P.decls (.int .w64 .signed 9223372036854775808) (.int .w64 .signed) := by
    intro h
    cases h with
    | int hb => revert hb; decide
  have hns : ¬ ((.run [] Frame.empty [] (.ret (.int .w64 .signed 9223372036854775808)) []) : Config).SafeAt
      Float.exactOps P (.int .w64 .signed) := fun h => hty (h.2 _ _ _ _ (.refl _))
  refine ⟨hPT, trivial, hty, fun hs => ?_, hns⟩
  obtain ⟨fd, hfd, hsafe⟩ := step_preservation Float.exactModel hPT
  subst hB hP
  simp only [List.getElem?_cons_zero, Option.some.injEq] at hfd
  subst hfd
  exact hns (hsafe _ hs)

/-- `Spec.Sharp.float_halt_stmt`, proved: a §7 hypothesis needed (RUE-2500). -/
theorem float_halt :
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
        Float.exactOps P (.float .w64) := by
  intro B hB P hP
  obtain ⟨-, hPT, -, -, H, v, tr, -, hsv, hv⟩ := Nonvacuous.float B hB P hP
  subst hv
  have hw1 : ¬ (FloatDatum.num false 30 (-2)).Wf .w64 := by decide
  have hw2 : ¬ (FloatDatum.num false 1 (-1075)).Wf .w64 := by decide
  have hns1 : ¬ ((.run [] Frame.empty [] (.ret (.float .w64 (.num false 30 (-2)))) []) : Config).SafeAt
      Float.exactOps P (.float .w64) := fun h => by
    cases h.2 _ _ _ _ (.refl _) with
    | float hf => exact hw1 hf
  have hns2 : ¬ ((.run [] Frame.empty [] (.ret (.float .w64 (.num false 1 (-1075)))) []) : Config).SafeAt
      Float.exactOps P (.float .w64) := fun h => by
    cases h.2 _ _ _ _ (.refl _) with
    | float hf => exact hw2 hf
  obtain ⟨fd, hfd, hsafe⟩ := step_preservation Float.exactModel hPT
  have hret : fd.ret = .float .w64 := by
    subst hB hP
    simp only [List.getElem?_cons_zero, Option.some.injEq] at hfd
    subst hfd; rfl
  rw [hret] at hsafe
  exact ⟨hPT, ⟨H, tr, hsv⟩, hw1, hw2, trivial, trivial, fun hs => hns1 (hsafe _ hs),
    fun hs => hns2 (hsafe _ hs), hns1, hns2⟩

end RueCore.Sharp
