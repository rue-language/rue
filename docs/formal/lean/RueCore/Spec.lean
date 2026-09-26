module

public import RueCore.Spec.Safety
public import RueCore.Spec.Checker
public import RueCore.Spec.Trace
public import RueCore.Spec.Step
public import RueCore.Spec.Adequacy

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

end RueCore.Spec
