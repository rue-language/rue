module

public import RueCore.Spec
public import RueCore.Soundness
public import RueCore.Checker
public import RueCore.Trace
public import RueCore.TraceExact
public import RueCore.TraceOrder
public import RueCore.Adequacy
public import RueCore.Nonvacuous

@[expose] public section

/-!
# RueCore.Spine — each spine proof, checked against its statement (layer L2)

One theorem per entry of `RueCore.Spec.spine`: `RueCore.Spine.<name>` has
exactly the type `RueCore.Spec.<name>_stmt` and is proved by the theorem
`RueCore.<name>` of the proof layer. The kernel accepts each only if that
theorem's statement is definitionally the Spec statement, so a statement
changed in the proof layer and not in `Spec` fails the build here. The
trusted-base lint checks the stronger fact that the two are the same term
(up to binder names), and that this module declares nothing else.

This module is Lean Comparator's *solution* (README, "The statement layer"):
the challenge, `comparator/Challenge.lean`, writes each `_stmt` out in full
and declares the same names with the same types and `sorry` for proofs, and Comparator certifies that each of these
theorems has the challenge's statement, is accepted by the kernel, and uses no
axiom but `propext` and `Quot.sound`.
-/

namespace RueCore.Spine

/-- `Spec.soundness_stmt`, by `RueCore.soundness` (helper). -/
theorem soundness : Spec.soundness_stmt := @RueCore.soundness
/-- `Spec.run_safe_stmt`, by `RueCore.run_safe` (helper). -/
theorem run_safe : Spec.run_safe_stmt := @RueCore.run_safe
/-- `Spec.no_violation_stmt`, by `RueCore.no_violation` (helper). -/
theorem no_violation : Spec.no_violation_stmt := @RueCore.no_violation
/-- `Spec.no_use_after_move_stmt`, by `RueCore.no_use_after_move` (helper). -/
theorem no_use_after_move : Spec.no_use_after_move_stmt := @RueCore.no_use_after_move
/-- `Spec.no_use_after_drop_stmt`, by `RueCore.no_use_after_drop` (helper). -/
theorem no_use_after_drop : Spec.no_use_after_drop_stmt := @RueCore.no_use_after_drop
/-- `Spec.no_linear_leak_stmt`, by `RueCore.no_linear_leak` (helper). -/
theorem no_linear_leak : Spec.no_linear_leak_stmt := @RueCore.no_linear_leak
/-- `Spec.no_linear_overwrite_stmt`, by `RueCore.no_linear_overwrite` (helper). -/
theorem no_linear_overwrite : Spec.no_linear_overwrite_stmt := @RueCore.no_linear_overwrite
/-- `Spec.no_linear_discard_stmt`, by `RueCore.no_linear_discard` (helper). -/
theorem no_linear_discard : Spec.no_linear_discard_stmt := @RueCore.no_linear_discard
/-- `Spec.fuel_mono_stmt`, by `RueCore.fuel_mono` (helper). -/
theorem fuel_mono : Spec.fuel_mono_stmt := @RueCore.fuel_mono
/-- `Spec.no_masking_stmt`, by `RueCore.no_masking` (helper). -/
theorem no_masking : Spec.no_masking_stmt := @RueCore.no_masking
/-- `Spec.run_ne_returned_stmt`, by `RueCore.run_ne_returned` (helper). -/
theorem run_ne_returned : Spec.run_ne_returned_stmt := @RueCore.run_ne_returned
/-- `Spec.check_sound_stmt`, by `RueCore.check_sound` (helper). -/
theorem check_sound : Spec.check_sound_stmt := @RueCore.check_sound
/-- `Spec.checkProgram_sound_stmt`, by `RueCore.checkProgram_sound` (helper). -/
theorem checkProgram_sound : Spec.checkProgram_sound_stmt := @RueCore.checkProgram_sound
/-- `Spec.no_double_free_stmt`, by `RueCore.no_double_free` (helper). -/
theorem no_double_free : Spec.no_double_free_stmt := @RueCore.no_double_free
/-- `Spec.freed_once_stmt`, by `RueCore.freed_once` (helper). -/
theorem freed_once : Spec.freed_once_stmt := @RueCore.freed_once
/-- `Spec.dtor_once_stmt`, by `RueCore.dtor_once` (helper). -/
theorem dtor_once : Spec.dtor_once_stmt := @RueCore.dtor_once
/-- `Spec.drop_exactly_once_stmt`, by `RueCore.drop_exactly_once` (helper). -/
theorem drop_exactly_once : Spec.drop_exactly_once_stmt := @RueCore.drop_exactly_once
/-- `Spec.rest_exactly_once_stmt`, by `RueCore.rest_exactly_once` (helper). -/
theorem rest_exactly_once : Spec.rest_exactly_once_stmt := @RueCore.rest_exactly_once
/-- `Spec.drop_order_stmt`, by `RueCore.drop_order` (helper). -/
theorem drop_order : Spec.drop_order_stmt := @RueCore.drop_order
/-- `Spec.Step.det_stmt`, by `RueCore.Step.det` (helper). -/
theorem Step.det : Spec.Step.det_stmt := @RueCore.Step.det
/-- `Spec.Step.terminal_stmt`, by `RueCore.Step.terminal` (helper). -/
theorem Step.terminal : Spec.Step.terminal_stmt := @RueCore.Step.terminal
/-- `Spec.Config.trichotomy_stmt`, by `RueCore.Config.trichotomy` (helper). -/
theorem Config.trichotomy : Spec.Config.trichotomy_stmt := @RueCore.Config.trichotomy
/-- `Spec.step_iff_stmt`, by `RueCore.step_iff` (helper). -/
theorem step_iff : Spec.step_iff_stmt := @RueCore.step_iff
/-- `Spec.Config.stuck_iff_stmt`, by `RueCore.Config.stuck_iff` (helper). -/
theorem Config.stuck_iff : Spec.Config.stuck_iff_stmt := @RueCore.Config.stuck_iff
/-- `Spec.step_stuck_isStuckState_stmt`, by `RueCore.step_stuck_isStuckState` (helper). -/
theorem step_stuck_isStuckState : Spec.step_stuck_isStuckState_stmt := @RueCore.step_stuck_isStuckState
/-- `Spec.step_progress_stmt`, by `RueCore.step_progress` (helper). -/
theorem step_progress : Spec.step_progress_stmt := @RueCore.step_progress
/-- `Spec.step_preservation_stmt`, by `RueCore.step_preservation` (helper). -/
theorem step_preservation : Spec.step_preservation_stmt := @RueCore.step_preservation
/-- `Spec.step_type_safety_stmt`, by `RueCore.step_type_safety` (helper). -/
theorem step_type_safety : Spec.step_type_safety_stmt := @RueCore.step_type_safety
/-- `Spec.eval_sound_stmt`, by `RueCore.eval_sound` (helper). -/
theorem eval_sound : Spec.eval_sound_stmt := @RueCore.eval_sound
/-- `Spec.run_sim_stmt`, by `RueCore.run_sim` (helper). -/
theorem run_sim : Spec.run_sim_stmt := @RueCore.run_sim
/-- `Spec.eval_complete_stmt`, by `RueCore.eval_complete` (helper). -/
theorem eval_complete : Spec.eval_complete_stmt := @RueCore.eval_complete
/-- `Spec.run_complete_stmt`, by `RueCore.run_complete` (helper). -/
theorem run_complete : Spec.run_complete_stmt := @RueCore.run_complete
/-- `Spec.never_stuck_iff_stmt`, by `RueCore.never_stuck_iff` (helper). -/
theorem never_stuck_iff : Spec.never_stuck_iff_stmt := @RueCore.never_stuck_iff
/-- `Spec.step_never_stuck_of_run_stmt`, by `RueCore.step_never_stuck_of_run` (helper). -/
theorem step_never_stuck_of_run : Spec.step_never_stuck_of_run_stmt := @RueCore.step_never_stuck_of_run
/-- `Spec.run_stuck_of_step_stuck_stmt`, by `RueCore.run_stuck_of_step_stuck` (helper). -/
theorem run_stuck_of_step_stuck : Spec.run_stuck_of_step_stuck_stmt := @RueCore.run_stuck_of_step_stuck
/-- `Spec.eval_diverges_iff_stmt`, by `RueCore.eval_diverges_iff` (helper). -/
theorem eval_diverges_iff : Spec.eval_diverges_iff_stmt := @RueCore.eval_diverges_iff

/-! ## The non-vacuity witnesses (`Spec.witnesses`) -/

/-- `Spec.Nonvacuous.exact_model_stmt`, by `RueCore.Nonvacuous.exact_model` (helper). -/
theorem Nonvacuous.exact_model : Spec.Nonvacuous.exact_model_stmt := @RueCore.Nonvacuous.exact_model
/-- `Spec.Nonvacuous.empty_frame_stmt`, by `RueCore.Nonvacuous.empty_frame` (helper). -/
theorem Nonvacuous.empty_frame : Spec.Nonvacuous.empty_frame_stmt := @RueCore.Nonvacuous.empty_frame
/-- `Spec.Nonvacuous.dtor_stmt`, by `RueCore.Nonvacuous.dtor` (helper). -/
theorem Nonvacuous.dtor : Spec.Nonvacuous.dtor_stmt := @RueCore.Nonvacuous.dtor
/-- `Spec.Nonvacuous.linear_stmt`, by `RueCore.Nonvacuous.linear` (helper). -/
theorem Nonvacuous.linear : Spec.Nonvacuous.linear_stmt := @RueCore.Nonvacuous.linear
/-- `Spec.Nonvacuous.loop_stmt`, by `RueCore.Nonvacuous.loop` (helper). -/
theorem Nonvacuous.loop : Spec.Nonvacuous.loop_stmt := @RueCore.Nonvacuous.loop
/-- `Spec.Nonvacuous.array_stmt`, by `RueCore.Nonvacuous.array` (helper). -/
theorem Nonvacuous.array : Spec.Nonvacuous.array_stmt := @RueCore.Nonvacuous.array
/-- `Spec.Nonvacuous.enum_match_stmt`, by `RueCore.Nonvacuous.enum_match` (helper). -/
theorem Nonvacuous.enum_match : Spec.Nonvacuous.enum_match_stmt := @RueCore.Nonvacuous.enum_match
/-- `Spec.Nonvacuous.early_return_stmt`, by `RueCore.Nonvacuous.early_return` (helper). -/
theorem Nonvacuous.early_return : Spec.Nonvacuous.early_return_stmt := @RueCore.Nonvacuous.early_return
/-- `Spec.Nonvacuous.panic_stmt`, by `RueCore.Nonvacuous.panic` (helper). -/
theorem Nonvacuous.panic : Spec.Nonvacuous.panic_stmt := @RueCore.Nonvacuous.panic
/-- `Spec.Nonvacuous.float_stmt`, by `RueCore.Nonvacuous.float` (helper). -/
theorem Nonvacuous.float : Spec.Nonvacuous.float_stmt := @RueCore.Nonvacuous.float
/-- `Spec.Nonvacuous.diverges_stmt`, by `RueCore.Nonvacuous.diverges` (helper). -/
theorem Nonvacuous.diverges : Spec.Nonvacuous.diverges_stmt := @RueCore.Nonvacuous.diverges
/-- `Spec.Nonvacuous.stuck_stmt`, by `RueCore.Nonvacuous.stuck` (helper). -/
theorem Nonvacuous.stuck : Spec.Nonvacuous.stuck_stmt := @RueCore.Nonvacuous.stuck

end RueCore.Spine
