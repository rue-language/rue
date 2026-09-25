# Red-team program for the formal core

The loop's adversarial review attacks a *change*: one lane, one diff, with
the implementer's plan in hand. This program attacks the *claim as a whole*:
that the Lean statements say what the calculus (§7 of
[01-core-calculus.md](01-core-calculus.md)) says, that the definitions they
use are faithful to it, that the bridge would notice a broken compiler, that
no document says more than the theorems, and that the trusted base is what
[lean/TRUST.md](lean/TRUST.md) reports. It complements the per-lane review
and does not replace it. Results go in [REDTEAM-LOG.md](REDTEAM-LOG.md).
Terms follow [FIELD.md](FIELD.md) where it has them ("trusted base" below
glosses FIELD.md's *trusted computing base*, §7); the rest — red agent,
full-claim/targeted pass, packet, adjudicator, non-vacuity witness, mutation
analysis, sensitivity drill, statement layer, spine — are this program's own
process vocabulary, mostly from the issue text, and are a candidate for
RUE-2461's glossary rather than FIELD.md's theorem-level terms.

## Targets

| Target | Attack | Evidence a pass yields | Carried by |
|---|---|---|---|
| **Statements** (the headline theorems in [lean/DIGEST.md](lean/DIGEST.md)) | Read each statement cold and compare it with §7: a hypothesis that is unsatisfiable or excludes the interesting programs, a conclusion a trivial evaluator or step relation also meets, a disjunct that always holds, a statement weaker than its English reading | A reading per theorem, a verdict (weaker / equal / stronger, with the gap), and, for each spine theorem, a checked **non-vacuity witness** (a program meeting every hypothesis on which the conclusion is non-trivial) and a **sharpness counter-example** (a program just outside a hypothesis, showing the hypothesis is not slack) | RUE-2470 (fresh-context statement audits), RUE-2469 (non-vacuity witnesses and sharpness counter-examples) |
| **Definitions** (`Typed`, `Step`, `eval`, `HasTy`, the trace predicates) | Fidelity to §5/§6 rule by rule; **mutation**: drop a premise or weaken a rule and see whether any theorem or corpus case notices | The mutants that survive, each either a missing test or an under-constrained rule | RUE-2465 (mutation testing) |
| **Bridge** (the differential testing of the compiler against the Lean model, [lean/README.md](lean/README.md)) | Would it catch a broken compiler? Re-introduce historical compiler bugs, one at a time, and run the bridge | Per drill: caught or not, and by which case | RUE-2464 (sensitivity drills) |
| **Docs** ([README.md](README.md), [03-metatheory.md](03-metatheory.md), `lean/README.md`, `lean/GUIDE.md`, the plain-language account, Linear project updates) | List every claim about what is proved, established or guaranteed and match it to a statement | Each claim marked supported, partly supported, unsupported, or a process claim; an issue per overclaim | RUE-2476 (overclaims) |
| **Trusted base** (FIELD.md's *trusted computing base*, §7; axioms, `set_option` escapes, the digest and trust generators, the statement/proof split's Comparator configuration) | Try to get a hole past the checks: a new axiom, `sorry` behind a macro, `native_decide`, a claim file that differs from what was proved | Whether the lint and the trust report catch each attempt | RUE-2457 (trusted-base lint), RUE-2460 (the Comparator configuration) |

A full-claim pass covers all five targets at the depth the current children
allow. Until a child lands, its row is attacked by reading, with the
reading's limits stated in the log.

## Roles

- **Red agent.** A fresh context, never the lane's implementer or reviewer.
  It receives **only** the artifact under attack and the calculus text, never
  `GUIDE.md`, the doc-comments, the metatheory's explanations or our issue
  threads, so our framing cannot steer its reading. Statements are given as
  the digest's elaborated Lean with the doc-comments stripped, plus the
  transitive closure of the definitions they use. The brief names what to
  produce (reading, correspondence, verdict, suspicious hypotheses, vacuity)
  and asks for **what it could not break** as well as what it broke.
  The packet must be the whole claim: every theorem §7's bullets and
  [03-metatheory.md](03-metatheory.md) cite (including the linking theorems
  such as `checkProgram_sound`, `step_iff` and `run_safe`), and a closure
  that includes definitions reached through dot notation. A packet that
  leaves one out produces findings that are artefacts of the packet; the
  first pass lost several findings to this (see the log). Today the packet
  is built by hand, with an ad hoc extraction script kept in the pass's
  scratch directory (`scratch/rue-<issue>/`, uncommitted); RUE-2460's Spec
  layer, once it lands, gives a canonical statement/definition extraction to
  build it from instead. The docs agent's packet follows the same
  completeness rule: the top-level docs in full (the formal-semantics README
  and the mechanization's README) plus the same theorem block, without the
  definitions closure.
- **Cross-model auditor.** A model from a different family (Codex today),
  given the same brief and packet independently, without the red agent's
  report. Invoke it as an ordinary lane addressed to that model, never shown
  the red agent's report or its own prior runs. The review checkpoints
  RUE-2251 and RUE-2252 are passes of this kind.
- **Adjudicator.** The coordinator. A finding **counts** once it is
  reproduced against the repository (a probe, an `#eval`, a witness, a quoted
  line) or once a second independent session reaches it on its own. A finding
  that neither reproduces nor recurs is dropped, and the log says why. When
  the red agent and the cross-model auditor disagree, or a finding turns on
  what the calculus *should* say, it goes to Dorian or Steve. Counted
  findings are filed as issues in Linear by the loop coordinator, under
  RUE-2454 (or one of its children, when one already covers the target), in
  the project's Assurance milestone; the red agents never write to Linear or
  the tree.

## Cadence

- **One full-claim pass per milestone**, and one before each review
  checkpoint (RUE-2251, RUE-2252), against the trunk the checkpoint reviews.
- **A targeted pass** whenever a spine statement, a trusted-base definition
  (anything a spine statement mentions, transitively) or the bridge changes:
  the statement agent for the first two, a drill for the third. The loop
  schedules it as an ordinary lane after the change merges.
- Every pass occupies one lane under the loop's two-lane cap, whatever number
  of red-agent, auditor and adjudicator subagent turns it takes within that
  lane — subagents are not lanes, so a full-claim pass with both a red agent
  and a cross-model auditor is still one lane.

## The log

[REDTEAM-LOG.md](REDTEAM-LOG.md) is append-only, newest entry last. Each
entry records:

1. date, trunk SHA, and the kind of pass (full-claim or targeted, and why);
2. targets and the exact packet each agent received;
3. the briefs, verbatim (in a collapsed section);
4. model or models, and which role each played;
5. findings, each with its evidence and its issue (or proposed title, until
   filed), and the findings dropped in adjudication with the reason;
6. **what was attacked and survived**: statements the red agent read the same
   way we do, and attacks that found nothing. Unbroken attempts are the
   evidence a pass produces, so they are recorded as carefully as findings.

## Human experts

Reach out once there is something concrete to show, and only on Steve's
decision. What to show: the statement layer (the Lean statements and the
definitions they use, in isolation from the proofs, once the statement/proof
split lands; `SPINE.md`, once it exists, or the digest until then), the
calculus §§5–7, the trust report, and this log, including its open findings.
Where: the Lean community Zulip
for the mechanization, and programming-languages researchers Steve knows for
the calculus and the choice of theorem forms. The question to ask them is the
red agent's: do these statements say what §7 says, and could any of them hold
vacuously.
