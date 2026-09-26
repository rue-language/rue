module

public import RueCore.Spec.Safety
public import RueCore.Spec.Checker
public import RueCore.Spec.Trace
public import RueCore.Spec.Step
public import RueCore.Spec.Adequacy
public import RueCore.Spec.Nonvacuous
public import RueCore.Spec.Sharp

@[expose] public section

/-!
# RueCore.Spec — the statements the mechanization claims (Spec layer)

The review surface (RUE-2460). Every theorem the mechanization's claim is made
of is stated here once, as a `def …_stmt : Prop` over the definitions of layers
L0 and L1 alone, with its English reading and the calculus paragraph it
realizes in its doc-comment. A reviewer reads these statements and the
definitions they unfold to (`TRUST.md`, "Trusted base") and nothing else:

* `Spec/Safety.lean` — §7's type safety and its memory-safety bullets, over
  the interpreter `eval`, with the fuel lemmas that make them one answer;
* `Spec/Checker.lean` — the executable checker decides the typing hypothesis;
* `Spec/Trace.lean` — no double free, exactly-once drops, and drop order;
* `Spec/Step.lean` — §6's relation's own properties, and §7 over it;
* `Spec/Adequacy.lean` — `eval` and §6's relation agree.

Beside them, `Spec/Nonvacuous.lean` holds the non-vacuity witnesses
(`witnesses` below, RUE-2469) and `Spec/Sharp.lean` the sharpness
counter-examples (`sharpness`, RUE-2485): statements about particular
programs that show each spine statement's hypotheses satisfiable, and each of
them needed.

`spine` below is the one list of them. It pairs each statement with the
theorem that proves it (in layer L2, stated in its own words), and it is read
by the tools: `Spine.lean` (layer L2) restates each theorem as
`RueCore.Spine.<name> : RueCore.Spec.<name>_stmt`, so the kernel checks every
proof against its statement; the trusted-base lint (`Lint.lean`) takes its
headline list from it and checks that each theorem's own statement is its
`_stmt`'s body, word for word; and Lean Comparator's challenge and
configuration (`comparator/`, README "The statement layer") and
`SPINE.md` are generated from it.

The list is the §7 claims the fragment can state and their linking
theorems; `SPINE.md` opens by naming the §7 bullets and lemmas with no
statement here (use-after-free, exclusivity, the loan and view lemmas). A lemma
`03-metatheory.md` cites as a step of a proof (the trace invariants behind
`no_double_free`, the drop-order lemmas, the float lemmas §7 owes) is not a
claim and is not here.
-/

namespace RueCore.Spec

/-- The spine: each statement of the Spec layer with the theorem that proves
it, in the order `SPINE.md` prints them (§7's claims; helper for the tools).
The statement names are resolved when this module compiles; the theorem
names cannot be, since the proofs are in a higher layer, so the lint resolves
them. -/
def spine : List (Lean.Name × Lean.Name) := [
  -- type safety over `eval`
  (`RueCore.soundness, ``soundness_stmt),
  (`RueCore.run_safe, ``run_safe_stmt),
  (`RueCore.no_violation, ``no_violation_stmt),
  (`RueCore.no_use_after_move, ``no_use_after_move_stmt),
  (`RueCore.no_use_after_drop, ``no_use_after_drop_stmt),
  (`RueCore.no_linear_leak, ``no_linear_leak_stmt),
  (`RueCore.no_linear_overwrite, ``no_linear_overwrite_stmt),
  (`RueCore.no_linear_discard, ``no_linear_discard_stmt),
  (`RueCore.fuel_mono, ``fuel_mono_stmt),
  (`RueCore.no_masking, ``no_masking_stmt),
  (`RueCore.run_ne_returned, ``run_ne_returned_stmt),
  -- the checker
  (`RueCore.check_sound, ``check_sound_stmt),
  (`RueCore.checkProgram_sound, ``checkProgram_sound_stmt),
  -- the trace
  (`RueCore.no_double_free, ``no_double_free_stmt),
  (`RueCore.freed_once, ``freed_once_stmt),
  (`RueCore.dtor_once, ``dtor_once_stmt),
  (`RueCore.drop_exactly_once, ``drop_exactly_once_stmt),
  (`RueCore.rest_exactly_once, ``rest_exactly_once_stmt),
  (`RueCore.drop_order, ``drop_order_stmt),
  -- §6's relation, and §7 over it
  (`RueCore.Step.det, ``Step.det_stmt),
  (`RueCore.Step.terminal, ``Step.terminal_stmt),
  (`RueCore.Config.trichotomy, ``Config.trichotomy_stmt),
  (`RueCore.step_iff, ``step_iff_stmt),
  (`RueCore.Config.stuck_iff, ``Config.stuck_iff_stmt),
  (`RueCore.step_stuck_isStuckState, ``step_stuck_isStuckState_stmt),
  (`RueCore.step_progress, ``step_progress_stmt),
  (`RueCore.step_preservation, ``step_preservation_stmt),
  (`RueCore.step_type_safety, ``step_type_safety_stmt),
  -- adequacy
  (`RueCore.eval_sound, ``eval_sound_stmt),
  (`RueCore.run_sim, ``run_sim_stmt),
  (`RueCore.eval_complete, ``eval_complete_stmt),
  (`RueCore.run_complete, ``run_complete_stmt),
  (`RueCore.never_stuck_iff, ``never_stuck_iff_stmt),
  (`RueCore.step_never_stuck_of_run, ``step_never_stuck_of_run_stmt),
  (`RueCore.run_stuck_of_step_stuck, ``run_stuck_of_step_stuck_stmt),
  (`RueCore.eval_diverges_iff, ``eval_diverges_iff_stmt)
]

/-- The non-vacuity witnesses (RUE-2469): each statement of
`Spec/Nonvacuous.lean` with the theorem that proves it, and the spine theorems
whose hypotheses it shows satisfiable by a non-trivial program, in `spine`'s
order. The tools read it beside `spine`: `Spine.lean` binds each proof to its
statement, the lint holds each to the same checks as a spine entry and fails
on a spine theorem no witness names, and Comparator's challenge, the
fingerprints and `SPINE.md` include every witness statement (helper). -/
def witnesses : List (Lean.Name × Lean.Name × List Lean.Name) := [
  (`RueCore.Nonvacuous.exact_model, ``Nonvacuous.exact_model_stmt, [
      `RueCore.soundness,
      `RueCore.run_safe,
      `RueCore.no_violation,
      `RueCore.no_use_after_move,
      `RueCore.no_use_after_drop,
      `RueCore.no_linear_leak,
      `RueCore.no_linear_overwrite,
      `RueCore.no_linear_discard,
      `RueCore.no_double_free,
      `RueCore.drop_exactly_once,
      `RueCore.rest_exactly_once,
      `RueCore.drop_order,
      `RueCore.step_progress,
      `RueCore.step_preservation,
      `RueCore.step_type_safety,
      `RueCore.eval_sound,
      `RueCore.eval_complete,
      `RueCore.never_stuck_iff,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.empty_frame, ``Nonvacuous.empty_frame_stmt, [
      `RueCore.soundness,
      `RueCore.drop_exactly_once,
      `RueCore.rest_exactly_once]),
  (`RueCore.Nonvacuous.open_frame, ``Nonvacuous.open_frame_stmt, [
      `RueCore.soundness,
      `RueCore.check_sound,
      `RueCore.drop_exactly_once,
      `RueCore.rest_exactly_once]),
  (`RueCore.Nonvacuous.dtor, ``Nonvacuous.dtor_stmt, [
      `RueCore.soundness,
      `RueCore.run_safe,
      `RueCore.no_violation,
      `RueCore.no_use_after_move,
      `RueCore.no_use_after_drop,
      `RueCore.no_linear_leak,
      `RueCore.no_linear_overwrite,
      `RueCore.no_linear_discard,
      `RueCore.fuel_mono,
      `RueCore.check_sound,
      `RueCore.checkProgram_sound,
      `RueCore.no_double_free,
      `RueCore.freed_once,
      `RueCore.dtor_once,
      `RueCore.drop_exactly_once,
      `RueCore.rest_exactly_once,
      `RueCore.drop_order,
      `RueCore.Step.det,
      `RueCore.Step.terminal,
      `RueCore.Config.trichotomy,
      `RueCore.step_iff,
      `RueCore.step_progress,
      `RueCore.step_preservation,
      `RueCore.step_type_safety,
      `RueCore.eval_sound,
      `RueCore.run_sim,
      `RueCore.eval_complete,
      `RueCore.run_complete,
      `RueCore.never_stuck_iff,
      `RueCore.step_never_stuck_of_run,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.linear, ``Nonvacuous.linear_stmt, [
      `RueCore.soundness,
      `RueCore.run_safe,
      `RueCore.no_violation,
      `RueCore.no_use_after_move,
      `RueCore.no_use_after_drop,
      `RueCore.no_linear_leak,
      `RueCore.no_linear_overwrite,
      `RueCore.no_linear_discard,
      `RueCore.fuel_mono,
      `RueCore.check_sound,
      `RueCore.checkProgram_sound,
      `RueCore.no_double_free,
      `RueCore.drop_order,
      `RueCore.Step.terminal,
      `RueCore.step_progress,
      `RueCore.step_preservation,
      `RueCore.step_type_safety,
      `RueCore.eval_sound,
      `RueCore.run_sim,
      `RueCore.eval_complete,
      `RueCore.run_complete,
      `RueCore.never_stuck_iff,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.loop, ``Nonvacuous.loop_stmt, [
      `RueCore.soundness,
      `RueCore.run_safe,
      `RueCore.no_violation,
      `RueCore.no_use_after_move,
      `RueCore.no_use_after_drop,
      `RueCore.no_linear_leak,
      `RueCore.no_linear_overwrite,
      `RueCore.no_linear_discard,
      `RueCore.fuel_mono,
      `RueCore.check_sound,
      `RueCore.checkProgram_sound,
      `RueCore.no_double_free,
      `RueCore.freed_once,
      `RueCore.drop_order,
      `RueCore.Step.terminal,
      `RueCore.step_progress,
      `RueCore.step_preservation,
      `RueCore.step_type_safety,
      `RueCore.eval_sound,
      `RueCore.run_sim,
      `RueCore.eval_complete,
      `RueCore.run_complete,
      `RueCore.never_stuck_iff,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.array, ``Nonvacuous.array_stmt, [
      `RueCore.soundness,
      `RueCore.run_safe,
      `RueCore.no_violation,
      `RueCore.no_use_after_move,
      `RueCore.no_use_after_drop,
      `RueCore.no_linear_leak,
      `RueCore.no_linear_overwrite,
      `RueCore.no_linear_discard,
      `RueCore.fuel_mono,
      `RueCore.check_sound,
      `RueCore.checkProgram_sound,
      `RueCore.no_double_free,
      `RueCore.drop_order,
      `RueCore.Step.terminal,
      `RueCore.step_progress,
      `RueCore.step_preservation,
      `RueCore.step_type_safety,
      `RueCore.eval_sound,
      `RueCore.run_sim,
      `RueCore.eval_complete,
      `RueCore.run_complete,
      `RueCore.never_stuck_iff,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.enum_match, ``Nonvacuous.enum_match_stmt, [
      `RueCore.soundness,
      `RueCore.run_safe,
      `RueCore.no_violation,
      `RueCore.no_use_after_move,
      `RueCore.no_use_after_drop,
      `RueCore.no_linear_leak,
      `RueCore.no_linear_overwrite,
      `RueCore.no_linear_discard,
      `RueCore.fuel_mono,
      `RueCore.check_sound,
      `RueCore.checkProgram_sound,
      `RueCore.no_double_free,
      `RueCore.drop_order,
      `RueCore.Step.terminal,
      `RueCore.step_progress,
      `RueCore.step_preservation,
      `RueCore.step_type_safety,
      `RueCore.eval_sound,
      `RueCore.run_sim,
      `RueCore.eval_complete,
      `RueCore.run_complete,
      `RueCore.never_stuck_iff,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.early_return, ``Nonvacuous.early_return_stmt, [
      `RueCore.soundness,
      `RueCore.run_safe,
      `RueCore.no_violation,
      `RueCore.no_use_after_move,
      `RueCore.no_use_after_drop,
      `RueCore.no_linear_leak,
      `RueCore.no_linear_overwrite,
      `RueCore.no_linear_discard,
      `RueCore.fuel_mono,
      `RueCore.run_ne_returned,
      `RueCore.check_sound,
      `RueCore.checkProgram_sound,
      `RueCore.no_double_free,
      `RueCore.drop_order,
      `RueCore.Step.terminal,
      `RueCore.step_progress,
      `RueCore.step_preservation,
      `RueCore.step_type_safety,
      `RueCore.eval_sound,
      `RueCore.run_sim,
      `RueCore.eval_complete,
      `RueCore.run_complete,
      `RueCore.never_stuck_iff,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.panic, ``Nonvacuous.panic_stmt, [
      `RueCore.soundness,
      `RueCore.run_safe,
      `RueCore.no_violation,
      `RueCore.no_use_after_move,
      `RueCore.no_use_after_drop,
      `RueCore.no_linear_leak,
      `RueCore.no_linear_overwrite,
      `RueCore.no_linear_discard,
      `RueCore.fuel_mono,
      `RueCore.check_sound,
      `RueCore.checkProgram_sound,
      `RueCore.no_double_free,
      `RueCore.drop_order,
      `RueCore.Step.terminal,
      `RueCore.step_progress,
      `RueCore.step_preservation,
      `RueCore.step_type_safety,
      `RueCore.eval_sound,
      `RueCore.run_sim,
      `RueCore.eval_complete,
      `RueCore.run_complete,
      `RueCore.never_stuck_iff,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.float, ``Nonvacuous.float_stmt, [
      `RueCore.soundness,
      `RueCore.run_safe,
      `RueCore.no_violation,
      `RueCore.no_use_after_move,
      `RueCore.no_use_after_drop,
      `RueCore.no_linear_leak,
      `RueCore.no_linear_overwrite,
      `RueCore.no_linear_discard,
      `RueCore.fuel_mono,
      `RueCore.check_sound,
      `RueCore.checkProgram_sound,
      `RueCore.no_double_free,
      `RueCore.drop_order,
      `RueCore.Step.terminal,
      `RueCore.step_progress,
      `RueCore.step_preservation,
      `RueCore.step_type_safety,
      `RueCore.eval_sound,
      `RueCore.run_sim,
      `RueCore.eval_complete,
      `RueCore.run_complete,
      `RueCore.never_stuck_iff,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.diverges, ``Nonvacuous.diverges_stmt, [
      `RueCore.checkProgram_sound,
      `RueCore.eval_diverges_iff]),
  (`RueCore.Nonvacuous.stuck, ``Nonvacuous.stuck_stmt, [
      `RueCore.fuel_mono,
      `RueCore.no_masking,
      `RueCore.Config.trichotomy,
      `RueCore.Config.stuck_iff,
      `RueCore.step_stuck_isStuckState,
      `RueCore.run_stuck_of_step_stuck])
]

/-- The sharpness counter-examples (RUE-2485): each statement of
`Spec/Sharp.lean` with the theorem that proves it, and the spine hypotheses it
shows needed, each a spine theorem and a hypothesis number. A statement's
hypotheses are its premises of `Prop` type, numbered from 1 in the order they
occur, premises inside a conjunction, an `↔` or an `∃` of the conclusion
included (`Lint.hypotheses`, the traversal `SPINE.md`'s "no hypotheses" reads).
For each listed pair the statement writes out a program (or a configuration)
of which that hypothesis fails, every other hypothesis of the theorem holds,
and the conclusion fails. The tools read it beside `spine` and `witnesses`:
`Spine.lean` binds each proof to its statement, `Sharp/Glue.lean` proves for
each pair the negation of the spine statement with that hypothesis removed
(RUE-2495), the lint holds each to a spine entry's checks, requires each
pair's glue theorem to state exactly that negation (`Lint.dropHyp`) and fails
on a hypothesis that neither this list nor `sharpnessReasons` covers, and Comparator's challenge, the fingerprints and
`SPINE.md` (each theorem's "Sharp" line) include every statement (helper). -/
def sharpness : List (Lean.Name × Lean.Name × List (Lean.Name × Nat)) := [
  (`RueCore.Sharp.stuck, ``Sharp.stuck_stmt, [
      (`RueCore.soundness, 1),
      (`RueCore.run_safe, 1),
      (`RueCore.no_violation, 1),
      (`RueCore.no_use_after_move, 1),
      (`RueCore.no_masking, 2),
      (`RueCore.checkProgram_sound, 1),
      (`RueCore.drop_exactly_once, 1),
      (`RueCore.rest_exactly_once, 1),
      (`RueCore.eval_sound, 1)]),
  (`RueCore.Sharp.stuck_step, ``Sharp.stuck_step_stmt, [
      (`RueCore.step_progress, 1),
      (`RueCore.step_preservation, 1),
      (`RueCore.step_type_safety, 1),
      (`RueCore.step_never_stuck_of_run, 1),
      (`RueCore.run_stuck_of_step_stuck, 3)]),
  (`RueCore.Sharp.typed, ``Sharp.typed_stmt, [
      (`RueCore.soundness, 2),
      (`RueCore.check_sound, 1),
      (`RueCore.drop_exactly_once, 3),
      (`RueCore.rest_exactly_once, 3)]),
  (`RueCore.Sharp.frame, ``Sharp.frame_stmt, [
      (`RueCore.soundness, 3),
      (`RueCore.drop_exactly_once, 4),
      (`RueCore.rest_exactly_once, 4)]),
  (`RueCore.Sharp.no_entry, ``Sharp.no_entry_stmt, [
      (`RueCore.run_safe, 2)]),
  (`RueCore.Sharp.entry_param, ``Sharp.entry_param_stmt, [
      (`RueCore.run_safe, 3),
      (`RueCore.no_violation, 1)]),
  (`RueCore.Sharp.copy, ``Sharp.copy_stmt, [
      (`RueCore.no_violation, 1)]),
  (`RueCore.Sharp.leak, ``Sharp.leak_stmt, [
      (`RueCore.no_linear_leak, 1),
      (`RueCore.eval_complete, 1)]),
  (`RueCore.Sharp.overwrite, ``Sharp.overwrite_stmt, [
      (`RueCore.no_linear_overwrite, 1)]),
  (`RueCore.Sharp.discard, ``Sharp.discard_stmt, [
      (`RueCore.no_linear_discard, 1),
      (`RueCore.eval_complete, 1)]),
  (`RueCore.Sharp.discard_loop, ``Sharp.discard_loop_stmt, [
      (`RueCore.no_linear_discard, 1),
      (`RueCore.never_stuck_iff, 1),
      (`RueCore.eval_diverges_iff, 1)]),
  (`RueCore.Sharp.fuel, ``Sharp.fuel_stmt, [
      (`RueCore.fuel_mono, 1),
      (`RueCore.fuel_mono, 2),
      (`RueCore.no_masking, 1),
      (`RueCore.eval_complete, 3),
      (`RueCore.run_complete, 2)]),
  (`RueCore.Sharp.fuel_panic, ``Sharp.fuel_panic_stmt, [
      (`RueCore.eval_complete, 5),
      (`RueCore.run_complete, 4)]),
  (`RueCore.Sharp.not_fits, ``Sharp.not_fits_stmt, [
      (`RueCore.check_sound, 2)]),
  (`RueCore.Sharp.double_drop, ``Sharp.double_drop_stmt, [
      (`RueCore.no_double_free, 1),
      (`RueCore.dtor_once, 1)]),
  (`RueCore.Sharp.bare_dtor, ``Sharp.bare_dtor_stmt, [
      (`RueCore.drop_order, 1)]),
  (`RueCore.Sharp.pending_program, ``Sharp.pending_program_stmt, [
      (`RueCore.drop_exactly_once, 2),
      (`RueCore.rest_exactly_once, 2)]),
  (`RueCore.Sharp.pending_expr, ``Sharp.pending_expr_stmt, [
      (`RueCore.drop_exactly_once, 6),
      (`RueCore.rest_exactly_once, 6)]),
  (`RueCore.Sharp.store_cc, ``Sharp.store_cc_stmt, [
      (`RueCore.drop_exactly_once, 5),
      (`RueCore.rest_exactly_once, 5)]),
  (`RueCore.Sharp.no_lead, ``Sharp.no_lead_stmt, [
      (`RueCore.rest_exactly_once, 7)]),
  (`RueCore.Sharp.no_eval, ``Sharp.no_eval_stmt, [
      (`RueCore.rest_exactly_once, 8)]),
  (`RueCore.Sharp.unreached, ``Sharp.unreached_stmt, [
      (`RueCore.drop_order, 2),
      (`RueCore.eval_sound, 2),
      (`RueCore.run_sim, 1),
      (`RueCore.eval_complete, 2),
      (`RueCore.run_complete, 1)]),
  (`RueCore.Sharp.unreached_panic, ``Sharp.unreached_panic_stmt, [
      (`RueCore.drop_order, 3),
      (`RueCore.eval_sound, 3),
      (`RueCore.run_sim, 2),
      (`RueCore.eval_complete, 4),
      (`RueCore.run_complete, 3)]),
  (`RueCore.Sharp.unordered, ``Sharp.unordered_stmt, [
      (`RueCore.drop_order, 4)]),
  (`RueCore.Sharp.not_a_step, ``Sharp.not_a_step_stmt, [
      (`RueCore.drop_order, 5)]),
  (`RueCore.Sharp.init_steps, ``Sharp.init_steps_stmt, [
      (`RueCore.Step.det, 1),
      (`RueCore.Step.det, 2),
      (`RueCore.Step.terminal, 1),
      (`RueCore.step_stuck_isStuckState, 1),
      (`RueCore.run_stuck_of_step_stuck, 2)]),
  (`RueCore.Sharp.unreachable_stuck, ``Sharp.unreachable_stuck_stmt, [
      (`RueCore.step_progress, 2),
      (`RueCore.step_preservation, 2),
      (`RueCore.never_stuck_iff, 2),
      (`RueCore.step_never_stuck_of_run, 2),
      (`RueCore.run_stuck_of_step_stuck, 1)])

]

/-- The spine hypotheses with no counter-example, each with the reason
(RUE-2485), in the form of `sharpness`'s pairs; the lint fails on a
hypothesis that neither list covers (helper).

One reason covers every statement over `M : FloatModel`, and is recorded here
once: the laws of `FloatModel` are not a hypothesis about a program but
assumptions about the float model every statement is instantiated at, and the
counter-examples all run on `Float.exactOps`, a model of them
(`Nonvacuous.exact_model`). A statement that failed at a model breaking a law
would say something about that model, not about the program's hypotheses; so
the laws have no counter-example, and `M` is not numbered among the
hypotheses (it is not a `Prop`). -/
def sharpnessReasons : List (Lean.Name × Nat × String) := [
  (`RueCore.no_use_after_drop, 1,
    "No counter-example has been found. By reading `Dynamics.lean`, `.dead` enters the store \
    only as an identity slot no binding names, or when a cell is retired as its binding leaves \
    the environment; and a fuzz of 78,000 programs, checked and unchecked, reached \
    `useAfterDrop` through neither `run` nor `step`. So the hypothesis appears redundant; the \
    theorem over every program is RUE-2496.")
]

end RueCore.Spec
