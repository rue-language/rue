module

public import RueCore.Spec
public import RueCore.Soundness
public import RueCore.Checker
public import RueCore.Trace
public import RueCore.TraceExact
public import RueCore.TraceOrder
public import RueCore.Retire
public import RueCore.TracePrefix
public import RueCore.TraceWhole
public import RueCore.Adequacy
public import RueCore.Nonvacuous
public import RueCore.Sharp

@[expose] public section

/-!
# RueCore.Spine — each spine proof, checked against its statement (layer L2)

One theorem per entry of `RueCore.Spec.spine` (41), of
`RueCore.Spec.witnesses` (16, the non-vacuity witnesses; RUE-2469) and of
`RueCore.Spec.sharpness` (38, the sharpness counter-examples; RUE-2485), 95
in all: `RueCore.Spine.<name>` has
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
/-- `Spec.no_refusal_stmt`, by `RueCore.no_refusal` (helper). -/
theorem no_refusal : Spec.no_refusal_stmt := @RueCore.no_refusal
/-- `Spec.no_use_after_move_stmt`, by `RueCore.no_use_after_move` (helper). -/
theorem no_use_after_move : Spec.no_use_after_move_stmt := @RueCore.no_use_after_move
/-- `Spec.no_use_after_drop_stmt`, by `RueCore.no_use_after_drop` (helper). -/
theorem no_use_after_drop : Spec.no_use_after_drop_stmt := @RueCore.no_use_after_drop
/-- `Spec.run_no_use_after_drop_stmt`, by `RueCore.run_no_use_after_drop` (helper). -/
theorem run_no_use_after_drop : Spec.run_no_use_after_drop_stmt := @RueCore.run_no_use_after_drop
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
/-- `Spec.step_no_double_free_stmt`, by `RueCore.step_no_double_free` (helper). -/
theorem step_no_double_free : Spec.step_no_double_free_stmt := @RueCore.step_no_double_free
/-- `Spec.freed_once_stmt`, by `RueCore.freed_once` (helper). -/
theorem freed_once : Spec.freed_once_stmt := @RueCore.freed_once
/-- `Spec.dtor_once_stmt`, by `RueCore.dtor_once` (helper). -/
theorem dtor_once : Spec.dtor_once_stmt := @RueCore.dtor_once
/-- `Spec.drop_exactly_once_stmt`, by `RueCore.drop_exactly_once` (helper). -/
theorem drop_exactly_once : Spec.drop_exactly_once_stmt := @RueCore.drop_exactly_once
/-- `Spec.rest_exactly_once_stmt`, by `RueCore.rest_exactly_once` (helper). -/
theorem rest_exactly_once : Spec.rest_exactly_once_stmt := @RueCore.rest_exactly_once
/-- `Spec.whole_program_exactly_once_stmt`, by `RueCore.whole_program_exactly_once` (helper). -/
theorem whole_program_exactly_once : Spec.whole_program_exactly_once_stmt :=
  @RueCore.whole_program_exactly_once
/-- `Spec.drop_order_stmt`, by `RueCore.drop_order` (helper). -/
theorem drop_order : Spec.drop_order_stmt := @RueCore.drop_order
/-- `Spec.drop_glue_order_stmt`, by `RueCore.drop_glue_order` (helper). -/
theorem drop_glue_order : Spec.drop_glue_order_stmt := @RueCore.drop_glue_order
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
/-- `Spec.step_safeAt_stmt`, by `RueCore.step_safeAt` (helper). -/
theorem step_safeAt : Spec.step_safeAt_stmt := @RueCore.step_safeAt
/-- `Spec.step_type_safety_stmt`, by `RueCore.step_type_safety` (helper). -/
theorem step_type_safety : Spec.step_type_safety_stmt := @RueCore.step_type_safety
/-- `Spec.step_no_use_after_drop_stmt`, by `RueCore.step_no_use_after_drop` (helper). -/
theorem step_no_use_after_drop : Spec.step_no_use_after_drop_stmt :=
  @RueCore.step_no_use_after_drop
/-- `Spec.eval_big_to_small_stmt`, by `RueCore.eval_big_to_small` (helper). -/
theorem eval_big_to_small : Spec.eval_big_to_small_stmt := @RueCore.eval_big_to_small
/-- `Spec.run_sim_stmt`, by `RueCore.run_sim` (helper). -/
theorem run_sim : Spec.run_sim_stmt := @RueCore.run_sim
/-- `Spec.eval_small_to_big_stmt`, by `RueCore.eval_small_to_big` (helper). -/
theorem eval_small_to_big : Spec.eval_small_to_big_stmt := @RueCore.eval_small_to_big
/-- `Spec.run_small_to_big_stmt`, by `RueCore.run_small_to_big` (helper). -/
theorem run_small_to_big : Spec.run_small_to_big_stmt := @RueCore.run_small_to_big
/-- `Spec.never_refused_iff_stmt`, by `RueCore.never_refused_iff` (helper). -/
theorem never_refused_iff : Spec.never_refused_iff_stmt := @RueCore.never_refused_iff
/-- `Spec.step_never_stuck_of_run_stmt`, by `RueCore.step_never_stuck_of_run` (helper). -/
theorem step_never_stuck_of_run : Spec.step_never_stuck_of_run_stmt := @RueCore.step_never_stuck_of_run
/-- `Spec.run_refused_of_step_stuck_stmt`, by `RueCore.run_refused_of_step_stuck` (helper). -/
theorem run_refused_of_step_stuck : Spec.run_refused_of_step_stuck_stmt := @RueCore.run_refused_of_step_stuck
/-- `Spec.eval_diverges_iff_stmt`, by `RueCore.eval_diverges_iff` (helper). -/
theorem eval_diverges_iff : Spec.eval_diverges_iff_stmt := @RueCore.eval_diverges_iff

/-! ## The non-vacuity witnesses (`Spec.witnesses`) -/

/-- `Spec.Nonvacuous.exact_model_stmt`, by `RueCore.Nonvacuous.exact_model` (helper). -/
theorem Nonvacuous.exact_model : Spec.Nonvacuous.exact_model_stmt := @RueCore.Nonvacuous.exact_model
/-- `Spec.Nonvacuous.empty_activation_stmt`, by `RueCore.Nonvacuous.empty_activation` (helper). -/
theorem Nonvacuous.empty_activation : Spec.Nonvacuous.empty_activation_stmt := @RueCore.Nonvacuous.empty_activation
/-- `Spec.Nonvacuous.open_activation_stmt`, by `RueCore.Nonvacuous.open_activation` (helper). -/
theorem Nonvacuous.open_activation : Spec.Nonvacuous.open_activation_stmt := @RueCore.Nonvacuous.open_activation
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
/-- `Spec.Nonvacuous.diverges_drop_stmt`, by `RueCore.Nonvacuous.diverges_drop` (helper). -/
theorem Nonvacuous.diverges_drop : Spec.Nonvacuous.diverges_drop_stmt :=
  @RueCore.Nonvacuous.diverges_drop
/-- `Spec.Nonvacuous.whole_drops_stmt`, by `RueCore.Nonvacuous.whole_drops` (helper). -/
theorem Nonvacuous.whole_drops : Spec.Nonvacuous.whole_drops_stmt :=
  @RueCore.Nonvacuous.whole_drops
/-- `Spec.Nonvacuous.whole_result_stmt`, by `RueCore.Nonvacuous.whole_result` (helper). -/
theorem Nonvacuous.whole_result : Spec.Nonvacuous.whole_result_stmt :=
  @RueCore.Nonvacuous.whole_result
/-- `Spec.Nonvacuous.refused_stuck_stmt`, by `RueCore.Nonvacuous.refused_stuck` (helper). -/
theorem Nonvacuous.refused_stuck : Spec.Nonvacuous.refused_stuck_stmt := @RueCore.Nonvacuous.refused_stuck

/-! ## The sharpness counter-examples (`Spec.sharpness`) -/

/-- `Spec.Sharp.refused_stmt`, by `RueCore.Sharp.refused` (helper). -/
theorem Sharp.refused : Spec.Sharp.refused_stmt := @RueCore.Sharp.refused
/-- `Spec.Sharp.stuck_step_stmt`, by `RueCore.Sharp.stuck_step` (helper). -/
theorem Sharp.stuck_step : Spec.Sharp.stuck_step_stmt := @RueCore.Sharp.stuck_step
/-- `Spec.Sharp.typed_stmt`, by `RueCore.Sharp.typed` (helper). -/
theorem Sharp.typed : Spec.Sharp.typed_stmt := @RueCore.Sharp.typed
/-- `Spec.Sharp.activation_stmt`, by `RueCore.Sharp.activation` (helper). -/
theorem Sharp.activation : Spec.Sharp.activation_stmt := @RueCore.Sharp.activation
/-- `Spec.Sharp.no_entry_stmt`, by `RueCore.Sharp.no_entry` (helper). -/
theorem Sharp.no_entry : Spec.Sharp.no_entry_stmt := @RueCore.Sharp.no_entry
/-- `Spec.Sharp.entry_param_stmt`, by `RueCore.Sharp.entry_param` (helper). -/
theorem Sharp.entry_param : Spec.Sharp.entry_param_stmt := @RueCore.Sharp.entry_param
/-- `Spec.Sharp.copy_stmt`, by `RueCore.Sharp.copy` (helper). -/
theorem Sharp.copy : Spec.Sharp.copy_stmt := @RueCore.Sharp.copy
/-- `Spec.Sharp.leak_stmt`, by `RueCore.Sharp.leak` (helper). -/
theorem Sharp.leak : Spec.Sharp.leak_stmt := @RueCore.Sharp.leak
/-- `Spec.Sharp.overwrite_stmt`, by `RueCore.Sharp.overwrite` (helper). -/
theorem Sharp.overwrite : Spec.Sharp.overwrite_stmt := @RueCore.Sharp.overwrite
/-- `Spec.Sharp.discard_stmt`, by `RueCore.Sharp.discard` (helper). -/
theorem Sharp.discard : Spec.Sharp.discard_stmt := @RueCore.Sharp.discard
/-- `Spec.Sharp.discard_loop_stmt`, by `RueCore.Sharp.discard_loop` (helper). -/
theorem Sharp.discard_loop : Spec.Sharp.discard_loop_stmt := @RueCore.Sharp.discard_loop
/-- `Spec.Sharp.fuel_stmt`, by `RueCore.Sharp.fuel` (helper). -/
theorem Sharp.fuel : Spec.Sharp.fuel_stmt := @RueCore.Sharp.fuel
/-- `Spec.Sharp.fuel_panic_stmt`, by `RueCore.Sharp.fuel_panic` (helper). -/
theorem Sharp.fuel_panic : Spec.Sharp.fuel_panic_stmt := @RueCore.Sharp.fuel_panic
/-- `Spec.Sharp.not_fits_stmt`, by `RueCore.Sharp.not_fits` (helper). -/
theorem Sharp.not_fits : Spec.Sharp.not_fits_stmt := @RueCore.Sharp.not_fits
/-- `Spec.Sharp.double_drop_stmt`, by `RueCore.Sharp.double_drop` (helper). -/
theorem Sharp.double_drop : Spec.Sharp.double_drop_stmt := @RueCore.Sharp.double_drop
/-- `Spec.Sharp.bare_dtor_stmt`, by `RueCore.Sharp.bare_dtor` (helper). -/
theorem Sharp.bare_dtor : Spec.Sharp.bare_dtor_stmt := @RueCore.Sharp.bare_dtor
/-- `Spec.Sharp.pending_program_stmt`, by `RueCore.Sharp.pending_program` (helper). -/
theorem Sharp.pending_program : Spec.Sharp.pending_program_stmt := @RueCore.Sharp.pending_program
/-- `Spec.Sharp.pending_expr_stmt`, by `RueCore.Sharp.pending_expr` (helper). -/
theorem Sharp.pending_expr : Spec.Sharp.pending_expr_stmt := @RueCore.Sharp.pending_expr
/-- `Spec.Sharp.store_cc_stmt`, by `RueCore.Sharp.store_cc` (helper). -/
theorem Sharp.store_cc : Spec.Sharp.store_cc_stmt := @RueCore.Sharp.store_cc
/-- `Spec.Sharp.no_lead_stmt`, by `RueCore.Sharp.no_lead` (helper). -/
theorem Sharp.no_lead : Spec.Sharp.no_lead_stmt := @RueCore.Sharp.no_lead
/-- `Spec.Sharp.no_eval_stmt`, by `RueCore.Sharp.no_eval` (helper). -/
theorem Sharp.no_eval : Spec.Sharp.no_eval_stmt := @RueCore.Sharp.no_eval
/-- `Spec.Sharp.unreached_stmt`, by `RueCore.Sharp.unreached` (helper). -/
theorem Sharp.unreached : Spec.Sharp.unreached_stmt := @RueCore.Sharp.unreached
/-- `Spec.Sharp.unreached_panic_stmt`, by `RueCore.Sharp.unreached_panic` (helper). -/
theorem Sharp.unreached_panic : Spec.Sharp.unreached_panic_stmt := @RueCore.Sharp.unreached_panic
/-- `Spec.Sharp.unordered_stmt`, by `RueCore.Sharp.unordered` (helper). -/
theorem Sharp.unordered : Spec.Sharp.unordered_stmt := @RueCore.Sharp.unordered
/-- `Spec.Sharp.not_a_step_stmt`, by `RueCore.Sharp.not_a_step` (helper). -/
theorem Sharp.not_a_step : Spec.Sharp.not_a_step_stmt := @RueCore.Sharp.not_a_step
/-- `Spec.Sharp.init_steps_stmt`, by `RueCore.Sharp.init_steps` (helper). -/
theorem Sharp.init_steps : Spec.Sharp.init_steps_stmt := @RueCore.Sharp.init_steps
/-- `Spec.Sharp.unreachable_stuck_stmt`, by `RueCore.Sharp.unreachable_stuck` (helper). -/
theorem Sharp.unreachable_stuck : Spec.Sharp.unreachable_stuck_stmt := @RueCore.Sharp.unreachable_stuck
/-- `Spec.Sharp.tombstoned_cell_stmt`, by `RueCore.Sharp.tombstoned_cell` (helper). -/
theorem Sharp.tombstoned_cell : Spec.Sharp.tombstoned_cell_stmt := @RueCore.Sharp.tombstoned_cell
/-- `Spec.Sharp.unreached_double_stmt`, by `RueCore.Sharp.unreached_double` (helper). -/
theorem Sharp.unreached_double : Spec.Sharp.unreached_double_stmt :=
  @RueCore.Sharp.unreached_double
/-- `Spec.Sharp.uncut_drop_stmt`, by `RueCore.Sharp.uncut_drop` (helper). -/
theorem Sharp.uncut_drop : Spec.Sharp.uncut_drop_stmt := @RueCore.Sharp.uncut_drop
/-- `Spec.Sharp.ill_typed_halt_stmt`, by `RueCore.Sharp.ill_typed_halt` (helper). -/
theorem Sharp.ill_typed_halt : Spec.Sharp.ill_typed_halt_stmt := @RueCore.Sharp.ill_typed_halt
/-- `Spec.Sharp.out_of_range_halt_stmt`, by `RueCore.Sharp.out_of_range_halt` (helper). -/
theorem Sharp.out_of_range_halt : Spec.Sharp.out_of_range_halt_stmt := @RueCore.Sharp.out_of_range_halt
/-- `Spec.Sharp.float_halt_stmt`, by `RueCore.Sharp.float_halt` (helper). -/
theorem Sharp.float_halt : Spec.Sharp.float_halt_stmt := @RueCore.Sharp.float_halt
/-- `Spec.Sharp.copy_leak_stmt`, by `RueCore.Sharp.copy_leak` (helper). -/
theorem Sharp.copy_leak : Spec.Sharp.copy_leak_stmt := @RueCore.Sharp.copy_leak
/-- `Spec.Sharp.pending_leak_stmt`, by `RueCore.Sharp.pending_leak` (helper). -/
theorem Sharp.pending_leak : Spec.Sharp.pending_leak_stmt := @RueCore.Sharp.pending_leak
/-- `Spec.Sharp.unreached_held_stmt`, by `RueCore.Sharp.unreached_held` (helper). -/
theorem Sharp.unreached_held : Spec.Sharp.unreached_held_stmt := @RueCore.Sharp.unreached_held
/-- `Spec.Sharp.unheld_stmt`, by `RueCore.Sharp.unheld` (helper). -/
theorem Sharp.unheld : Spec.Sharp.unheld_stmt := @RueCore.Sharp.unheld
/-- `Spec.Sharp.off_run_stmt`, by `RueCore.Sharp.off_run` (helper). -/
theorem Sharp.off_run : Spec.Sharp.off_run_stmt := @RueCore.Sharp.off_run


end RueCore.Spine
