import Lean
import RueCore.Spec

/-!
# RueCore.Literature — the spine against the literature (RUE-2467)

One row per spine theorem (`RueCore.Spec.spine`): the standard theorem it
corresponds to, cited from the field map `../FIELD.md`; our form, in the
glossary's notation (`../GLOSSARY.md`); and the difference between the two in
one sentence, or "Identical up to notation." `lake exe ruecore-digest --spine`
prints the rows as the table "The spine against the literature" in
`SPINE.md`, and fails when `problems` finds a spine theorem with no row, a row
for a theorem the spine does not list, a theorem with two rows, or a row
with no FIELD.md citation.

**Why the table is here, in L3, and not in the Spec layer.** A row is
commentary on a statement, not part of what the mechanization claims. The
Spec layer is the claim: Comparator's challenge and `spine-fingerprints.txt`
are generated from it, and the lint allows a Spec module to declare nothing
but its statements and their lists. Kept in the tooling layer, a row can be
corrected, or a citation updated when `FIELD.md` changes, without touching
the Spec layer, the challenge or a fingerprint; and `problems` keeps it
complete against `Spec.spine`, as `Map.milestoneProblems` keeps the proof
map's list honest.

`FIELD.md` is the one source of citations: every row names a section of it
(`FIELD §` and its number) and a source that section's tables cite, with the theorem or
section number as `FIELD.md` records it. Where the field has no named
counterpart, the row says so and cites the section that records the
absence, or the nearest accepted term.
-/

namespace RueCore.Literature

/-- (helper) One row of the table: the spine theorem (as `Spec.spine`
names it), the literature's form with its `FIELD.md` citation, our form in
the glossary's notation, and the difference in one sentence. -/
structure Row where
  thm : Lean.Name
  lit : String
  cite : String
  ours : String
  diff : String

/-- (helper) The rows, in `Spec.spine`'s order. -/
def rows : List Row := [
  -- type safety over `eval`
  { thm := `RueCore.soundness
    lit := "Soundness via a definitional interpreter: `⊢ e : T ∧ eval n e = r ≠ Timeout ⇒ r = Val v ∧ v : T`"
    cite := "FIELD §3: Amin & Rompf Lemma 3; Owens et al. §5"
    ours := "`WfProgram P`, `Typed P R Γ e T Ω` and a frame and store agreeing with `Γ` ⇒ `EvalOk` of `eval` at every fuel"
    diff := "The same theorem for an open expression, from any frame and store that agree with its context (`FrameMatches`), with an unwinding `return` or `break`, a defined panic and exhausted fuel among the allowed outcomes, and with `.stuck` excluded, which covers `eval`'s monitors as well as stuck states." },
  { thm := `RueCore.run_safe
    lit := "Syntactic soundness: `⊢ e : τ ⇒ e⇑ ∨ (e ↦* v ∧ ⊢ v : τ)`"
    cite := "FIELD §2: Wright & Felleisen Thm 4.12 (preprint numbering)"
    ours := "`WfProgram P` and a parameterless entry point ⇒ at every fuel, `run` is out of fuel, a panic, or a value of the entry point's type"
    diff := "Wright & Felleisen's three-way form, read per fuel bound over the interpreter: divergence is exhausting this fuel rather than `⇑`, and a defined panic, PFPL's checked error, is an allowed outcome." },
  { thm := `RueCore.no_violation
    lit := "Weak soundness: a well-typed program never yields `wrong` (\"well-typed programs do not go wrong\")"
    cite := "FIELD §2: Wright & Felleisen §§1–2; Milner 1978"
    ours := "`ProgramTyped P` ⇒ `run` is never `.stuck w`, at any fuel and for any `w`"
    diff := "`.stuck w` is wider than `wrong`: it includes the refusals of `eval`'s four monitors, which are not stuck states of §6, so the statement also says that no monitor fires (R3 of `REDTEAM-LOG.md`)." },
  { thm := `RueCore.no_use_after_move
    lit := "No named theorem; reading a moved-from (deinitialized) place is a use of uninitialized memory, one of the memory access errors of memory safety"
    cite := "FIELD §5: Rust Reference, Expressions and Glossary (moved from); Hicks 2014 (memory safety)"
    ours := "`ProgramTyped P` ⇒ `run` never refuses with `useAfterMove`"
    diff := "It is `no_violation` at one tag, so it rules out a read of a `⊘` cell only as far as `eval` checks every read and labels it so." },
  { thm := `RueCore.no_use_after_drop
    lit := "No use after free: the program never \"reuses or references memory after it has been freed\""
    cite := "FIELD §5: CWE-416"
    ours := "`ProgramTyped P` ⇒ `run` never refuses with `useAfterDrop`"
    diff := "The freed object is a dropped binding's retired (`†`) cell rather than heap memory, the error is ruled out as far as `eval` labels an access to it, and the typing hypothesis is redundant (`run_no_use_after_drop`)." },
  { thm := `RueCore.run_no_use_after_drop
    lit := "No use after free (CWE-416), as above"
    cite := "FIELD §5: CWE-416"
    ours := "every program ⇒ `run` never refuses with `useAfterDrop`, at any fuel and any float operations"
    diff := "No typing hypothesis: the property is structural, since a cell is minted fresh and nothing names it once its scope has retired it." },
  { thm := `RueCore.no_linear_leak
    lit := "Linearity: a linear assumption is used exactly once, so there is no weakening; failing the \"at least once\" half is a leak"
    cite := "FIELD §4: Walker §1.1; FIELD §5: CWE-401"
    ours := "`ProgramTyped P` ⇒ `run` never refuses with `linearLeak`"
    diff := "Walker's rule is a property of the typing context; ours is its dynamic image, that no scope exit or unwind meets a live linear value, as `eval`'s monitor for it watches." },
  { thm := `RueCore.no_linear_overwrite
    lit := "Linearity, no weakening (Walker), for an assignment"
    cite := "FIELD §4: Walker §1.1"
    ours := "`ProgramTyped P` ⇒ `run` never refuses with `linearOverwrite`"
    diff := "The dynamic image of no weakening at an assignment, which would discard the live linear value it overwrites, as `eval`'s monitor for it watches." },
  { thm := `RueCore.no_linear_discard
    lit := "Linearity, no weakening (Walker), for a sequence"
    cite := "FIELD §4: Walker §1.1"
    ours := "`ProgramTyped P` ⇒ `run` never refuses with `linearDiscard`"
    diff := "The dynamic image of no weakening at a sequence, which would discard a linear value, as `eval`'s monitor for it watches." },
  { thm := `RueCore.fuel_mono
    lit := "The clock lemma: not timed out at clock `c` ⇒ the same result at every `c + k`"
    cite := "FIELD §3: Owens et al. §3.4 (unnamed); Software Foundations `ceval_step_more`"
    ours := "`n ≤ m` and `eval n … ≠ outOfFuel` ⇒ `eval m … = eval n …`"
    diff := "Identical up to notation." },
  { thm := `RueCore.no_masking
    lit := "No named counterpart; a consequence of the clock lemma"
    cite := "FIELD §3: the `no_masking` row (none)"
    ours := "`eval n … = .stuck w` and `eval m … ≠ outOfFuel` ⇒ `eval m … = .stuck w`"
    diff := "A corollary of `fuel_mono` in either order of `n` and `m`, stated because the ∀-fuel theorems need it and the literature does not state it separately." },
  { thm := `RueCore.run_ne_returned
    lit := "No counterpart: the literature's interpreter result is a timeout, an error or a value"
    cite := "FIELD §3: the result type `Timeout ∣ Done (Error ∣ Val v)` (Owens et al.; Siek 2013)"
    ours := "`run` never answers an unwinding `return`, on every program"
    diff := "Our result type also has unwinding outcomes (`returned`, `broke`) that the literature's has no counterpart of, and this says an unwinding `return` never escapes `run`, since the entry point's call catches it ((D-Return-Main) §6.9)." },
  -- the checker
  { thm := `RueCore.check_sound
    lit := "Algorithmic soundness: `Γ₁ ⊢ t : T; Γ₂ ∧ L(Γ₂) = ∅ ⇒ Γ₁ ⊢ t : T`"
    cite := "FIELD §4: Walker 1.2.9"
    ours := "`check P R Γ e = some (c, Ω)` ⇒ `Typed P R Γ e T Ω` at every `T` that `c` fits"
    diff := "Walker's direction and shape, with the outgoing state `Ω` kept in the declarative judgment rather than required empty, and soundness only: completeness is not stated, and does not hold (a bound on the loop-head iteration can reject a typed program)." },
  { thm := `RueCore.checkProgram_sound
    lit := "Algorithmic soundness (Walker 1.2.9), for a whole program"
    cite := "FIELD §4: Walker 1.2.9"
    ours := "`checkProgram P = true` ⇒ `ProgramTyped P`"
    diff := "Soundness only, lifted to a program; a typed program the checker rejects is possible." },
  -- the trace
  { thm := `RueCore.no_double_free
    lit := "No double free: no program run \"calls free() twice on the same memory address\", an at-most-once safety property of the trace"
    cite := "FIELD §5: CWE-415; FIELD §6: Alpern & Schneider §2"
    ours := "`ProgramTyped P` ⇒ at every fuel, `run` is not stuck and its trace frees each identity, and runs a destructor on each, at most once"
    diff := "It counts drop and destructor events per value identity rather than calls of `free()` per address, and holds of finished runs only (an `outOfFuel` result has an empty trace), so the safety-property form is `step_no_double_free`." },
  { thm := `RueCore.step_no_double_free
    lit := "A safety property: every violation has a finite prefix no continuation repairs"
    cite := "FIELD §6: Alpern & Schneider §2; FIELD §5: CWE-415"
    ours := "`ProgramTyped P` and `init →* C` ⇒ `C`'s trace frees each identity, and runs a destructor on each, at most once"
    diff := "The at-most-once bound stated on every finite prefix of every run, as a property of each reachable configuration rather than of Alpern & Schneider's infinite sequences, which for this property is the same content." },
  { thm := `RueCore.freed_once
    lit := "No double free (CWE-415), on every program"
    cite := "FIELD §5: CWE-415"
    ours := "every program ⇒ a finished run's trace frees each identity at most once"
    diff := "No typing hypothesis and no destructor count, per value identity, over finished runs." },
  { thm := `RueCore.dtor_once
    lit := "No double free (CWE-415), for destructor runs"
    cite := "FIELD §5: CWE-415; Rust Reference, Destructors"
    ours := "`DtorNotCopy` ⇒ a finished run's trace runs a destructor on each identity at most once"
    diff := "Its only hypothesis is that a destructor-bearing struct is not `Copy`, and it counts destructor runs per identity, over finished runs." },
  { thm := `RueCore.drop_exactly_once
    lit := "Exactly once = at most once ∧ at least once; linear use is exactly one use; a memory leak is the failure of \"at least once\""
    cite := "FIELD §6: Confluent (delivery), Walker (linear use); FIELD §5: CWE-401"
    ours := "a typed, `pendingSafe` expression of a checked program, from a frame and store agreeing with its context ⇒ its evaluation is not stuck, ends every identity exactly as often as held (`Exact`), and retires what it allocated (`Tidy`)"
    diff := "Per evaluation of one expression from a matching frame and store, not per run from `Config.init`, under `pendingSafe` (RUE-2316) and with nothing about a panic (RUE-2478)." },
  { thm := `RueCore.rest_exactly_once
    lit := "Exactly once (as above), for values minted during an evaluation"
    cite := "FIELD §6: Confluent (delivery), Walker (linear use)"
    ours := "the hypotheses of `drop_exactly_once`, and a form's leading operands evaluated (`Lead`) ⇒ the rest of the form ends them and the store's identities exactly once (`Exact`) and retires what it allocated (`Settled`)"
    diff := "The induction form behind `drop_exactly_once`, listed as a linking statement because it covers the values a form mints mid-evaluation; the literature has no separate counterpart." },
  { thm := `RueCore.drop_order
    lit := "Drop order: variables are dropped in reverse order of declaration, temporaries in reverse order of creation"
    cite := "FIELD §5: Rust Reference, Destructors; FIELD §6: trace property over finished traces (no accepted name)"
    ours := "`ProgramTyped P` ⇒ a finished run's trace is in §6.11's block grammar (`Blocks`), and each step from a reachable configuration drops newest first (`NewestFirst`, `Lifo`) from a location-ordered stack"
    diff := "Newest first by location rather than reverse declaration order, where `Lifo` constrains only a step that pops a scope (it holds of every step that keeps its stack), and `Blocks` holds of finished traces only (R7 of `REDTEAM-LOG.md`)." },
  { thm := `RueCore.drop_glue_order
    lit := "Drop glue: `Drop::drop` if implemented, then each field's drop glue; struct fields in declaration order, array elements first to last"
    cite := "FIELD §5: rustc-dev-guide, Drop elaboration; Rust Reference, Destructors"
    ours := "`ProgramTyped P` ⇒ a finished run's trace is in §6.11's block grammar with each drop's events given by §6.11's rules (`GlueBlocks`, `DropGlue`)"
    diff := "The Rust order for structs and arrays, with an enum dropping its active payload only, stated over finished traces only." },
  -- §6's relation, and §7 over it
  { thm := `RueCore.Step.det
    lit := "Determinacy: `e ↦ e′ ∧ e ↦ e″ ⇒ e′ =α e″`"
    cite := "FIELD §1: PFPL Lemma 5.3"
    ours := "`C → C₁` and `C → C₂` ⇒ `C₁ = C₂`"
    diff := "Identical up to notation, with syntactic equality for `=α` because bindings are de Bruijn indices." },
  { thm := `RueCore.Step.terminal
    lit := "Finality of values: `¬(e val ∧ e ↦ e′)`; a terminal transition system's final configurations take no step"
    cite := "FIELD §1: PFPL Lemma 5.2; Plotkin 1981/2004 §1.2, Def. 2"
    ours := "`C` terminal (`✓` or `↯κ`) ⇒ no `C → C′`"
    diff := "Identical up to notation, with a trap `↯κ` final too, as PFPL's checked error is." },
  { thm := `RueCore.Config.trichotomy
    lit := "No named theorem: by the definition of stuck, a state is final, steps, or is stuck"
    cite := "FIELD §1: Plotkin 1981/2004 §3.1, Def. 11; PFPL ch. 6"
    ours := "every `C` steps, is terminal, or is stuck on a named `Violation`"
    diff := "Classically immediate from the definition of stuck, it has content here because `Config.Stuck` is the step function's verdict, so it says that verdict is exhaustive and names the violation." },
  { thm := `RueCore.step_iff
    lit := "No named counterpart: an executable step function agrees with the transition relation"
    cite := "FIELD §1: small-step transition relation (Plotkin 1981/2004 §1.2)"
    ours := "`C → C′` ⇔ `step C = .next C′`"
    diff := "Correctness of the executable `step` for the relation `Step`, both ways, which the literature, defining only the relation, does not need." },
  { thm := `RueCore.Config.stuck_iff
    lit := "Stuck: not a value (not final) and no step applies"
    cite := "FIELD §1: Plotkin 1981/2004 §3.1, Def. 11; PFPL ch. 6"
    ours := "(`C` not terminal and no `C → C′`) ⇔ `C` is stuck on some `Violation`"
    diff := "The accepted definition, proved equal to ours (the step function's verdict), with terminal in the role of value, traps included." },
  { thm := `RueCore.step_stuck_isStuckState
    lit := "No counterpart"
    cite := "FIELD §2: the `Violation` row (four of its constructors are monitors, not stuck states of `Step`)"
    ours := "`C` stuck on `w` ⇒ `w` is one of §6's own four stuck states"
    diff := "Says that `Step`'s stuck states are §6's own and never one of `eval`'s monitors, which the literature, with no monitors, has no need of." },
  { thm := `RueCore.step_progress
    lit := "Progress: `⊢ e : τ ⇒ e val ∨ ∃e′. e ↦ e′`; along runs, `safe(e)`: every state reachable from `e` is progressive"
    cite := "FIELD §2: PFPL Thm 6.4; Timany et al. §2.4"
    ours := "`ProgramTyped P` and `init →* C` ⇒ `C` terminal or `C → C′` for some `C′`"
    diff := "Not the one-step lemma over typed configurations, since no configuration typing is defined (RUE-2423), but its consequence along every run from `Config.init`, Timany's `safe(init)` with traps final." },
  { thm := `RueCore.step_preservation
    lit := "Preservation (subject reduction): `Γ ⊢ e : τ ∧ e ↦ e′ ⇒ Γ ⊢ e′ : τ`"
    cite := "FIELD §2: PFPL Thm 6.2; Wright & Felleisen Lemma 4.3"
    ours := "`ProgramTyped P` and `init →* C` ⇒ `C.SafeAt` the entry point's return type"
    diff := "Substantive: `SafeAt` is a semantic invariant (Timany's `safe` plus typed halting values), closed under `→*` by definition, so the statement is `SafeAt` at `Config.init`, the conclusion of Timany's Cor. 2.3, and no syntactic `⊢ C : T` is preserved (R4 of `REDTEAM-LOG.md`; RUE-2423)." },
  { thm := `RueCore.step_type_safety
    lit := "Type safety = preservation ∧ progress; in Wright & Felleisen's form, `⊢ e : τ ⇒ e⇑ ∨ (e ↦* v ∧ ⊢ v : τ)`"
    cite := "FIELD §2: PFPL Thm 6.1; Wright & Felleisen Thm 4.12"
    ours := "`ProgramTyped P` ⇒ for every `n`, `init →ⁿ D` for some `D`, or `init →* ✓v` with `v` of the entry point's type, or `init →* ↯κ`"
    diff := "Wright & Felleisen's three-way form per horizon `n`, with a trap as an allowed outcome (PFPL's checked error) and divergence read as having run `n` steps, rather than a conjunction of progress and preservation." },
  { thm := `RueCore.step_no_use_after_drop
    lit := "No use after free (CWE-416), a safety property of runs"
    cite := "FIELD §5: CWE-416; FIELD §6: Alpern & Schneider §2"
    ours := "every program and `init →* C` ⇒ `C` is not stuck on a retired (`†`) cell"
    diff := "Over §6's relation on every program, checked or not, with a dropped binding's cell in the role of freed memory." },
  -- semantic equivalence of `eval` and `Step`
  { thm := `RueCore.eval_sound
    lit := "Semantic equivalence, interpreter to small-step: `eval n e = r ⇒ ∃e′. e →* e′ ∧ r ∼ e′`; big-step to small-step, `a ⇒ v ⇒ a →* v`"
    cite := "FIELD §3: Amin & Rompf Thm 2; Leroy & Grall Thm 9"
    ours := "`ProgramTyped P` ⇒ at every fuel, `run` is not stuck, a value it answers is `init →* ✓v` and a panic is `init →* ↯κ`, with the same store and trace"
    diff := "The interpreter-to-small-step direction, restricted to checked programs and bundled with \"`run` is never stuck\"; the unconditional direction is `run_sim`." },
  { thm := `RueCore.run_sim
    lit := "Semantic equivalence, interpreter to small-step (Amin & Rompf Thm 2), unconditionally"
    cite := "FIELD §3: Amin & Rompf Thm 2; Leroy & Grall Thm 9"
    ours := "every program ⇒ at every fuel, a value `run` answers is `init →* ✓v` and a panic is `init →* ↯κ`"
    diff := "The literature's unconditional direction for values and panics, with a refusal or exhausted fuel left outside it." },
  { thm := `RueCore.eval_complete
    lit := "Semantic equivalence, small-step to interpreter: `e →* e′ ⇒ ∃n. eval n e ∼ e′`; small-step to big-step, `a →* v ∧ v value ⇒ a ⇒ v`"
    cite := "FIELD §3: Amin & Rompf Thm 2; Leroy & Grall Thm 9; Owens et al. §3.4"
    ours := "`ProgramTyped P` and `init →* ✓v` (or `↯κ`) ⇒ `run` answers that value (or panic), with the same store and trace, at every fuel past some `n`"
    diff := "Restricted to checked programs, and at every fuel past `n` rather than at some `n`, which folds in the clock lemma (`fuel_mono`)." },
  { thm := `RueCore.run_complete
    lit := "Semantic equivalence, small-step to interpreter (Amin & Rompf Thm 2), unconditionally"
    cite := "FIELD §3: Amin & Rompf Thm 2"
    ours := "every program and `init →* ✓v` (or `↯κ`) ⇒ past some fuel, `run` answers that value (or panic) or refuses"
    diff := "On every program only up to a refusal, since `eval`'s monitors are stricter than §6, which makes it much weaker than the literature's unconditional direction." },
  { thm := `RueCore.never_stuck_iff
    lit := "No named counterpart: the interpreter's weak soundness against the small-step `safe(init)`"
    cite := "FIELD §2: Wright & Felleisen §§1–2 (weak soundness); Timany et al. §2.4 (`safe`)"
    ours := "`ProgramTyped P` ⇒ (`run` never stuck at any fuel ⇔ every `C` with `init →* C` is terminal or steps)"
    diff := "Under `ProgramTyped` both sides hold outright (`no_violation`, `step_progress`), so the `⇔` adds nothing; its content is the forward direction on every program, `step_never_stuck_of_run` (R5 of `REDTEAM-LOG.md`)." },
  { thm := `RueCore.step_never_stuck_of_run
    lit := "No named counterpart: `safe(init)` transferred from the interpreter"
    cite := "FIELD §2: Timany et al. §2.4 (`safe`); FIELD §3: Amin & Rompf Thm 2"
    ours := "every program, `run` never stuck at any fuel ⇒ every `C` with `init →* C` is terminal or steps"
    diff := "Carries the interpreter's never-stuck to `Step`'s `safe(init)` on every program, a use of the equivalence the literature does not state separately." },
  { thm := `RueCore.run_stuck_of_step_stuck
    lit := "No named counterpart: the stuck case of the equivalence, where `∼` relates an error result to a stuck term"
    cite := "FIELD §3: Amin & Rompf Thm 2"
    ours := "`init →* C` and `C` stuck ⇒ past some fuel, `run` refuses, perhaps with another `Violation`"
    diff := "The error half of the equivalence, stated separately because `run` and `Step` may name the same failure by different violations." },
  { thm := `RueCore.eval_diverges_iff
    lit := "Big-step/small-step equivalence for diverging runs: `a ⇒∞ ⇔ a →∞`; clock-based divergence, timing out at every clock"
    cite := "FIELD §3: Leroy & Grall Thm 11; Owens et al. §3.4"
    ours := "`ProgramTyped P` ⇒ (`run` out of fuel at every fuel ⇔ `init →ⁿ D` for every `n` and some `D`)"
    diff := "Restricted to checked programs, with Owens's clocked divergence on the interpreter side and runs of every length instead of a coinductive `→∞` on the small-step side, which agree because `Step` is deterministic." }
]

/-- (helper) What is wrong with the table, against `Spec.spine`: a spine
theorem with no row, a theorem with two rows, a row for a theorem the spine
does not list, and a row whose citation names no `FIELD.md` section or whose
difference is not one sentence ending in a full stop. `ruecore-digest
--spine` prints each and exits non-zero. -/
def problems : List String := Id.run do
  let spineThms := Spec.spine.map (·.1)
  let mut out : List String := []
  for t in spineThms do
    match (rows.filter (·.thm == t)).length with
    | 0 => out := out ++ [s!"{t}: a spine theorem with no row in Literature.rows"]
    | 1 => pure ()
    | n => out := out ++ [s!"{t}: {n} rows in Literature.rows"]
  for r in rows do
    if !spineThms.contains r.thm then
      out := out ++ [s!"{r.thm}: a row in Literature.rows for a theorem Spec.spine does not list"]
    if !(r.cite.startsWith "FIELD §") then
      out := out ++ [s!"{r.thm}: its citation does not start with a FIELD.md section (`FIELD §n`)"]
    if r.lit.isEmpty || r.ours.isEmpty then
      out := out ++ [s!"{r.thm}: an empty literature or our-form cell"]
    if !(r.diff.endsWith ".") then
      out := out ++ [s!"{r.thm}: its difference is not a sentence ending in a full stop"]
    if (r.lit ++ r.cite ++ r.ours ++ r.diff).any (· == '|') then
      out := out ++ [s!"{r.thm}: a cell contains `|`, which breaks the Markdown table"]
  return out

end RueCore.Literature
