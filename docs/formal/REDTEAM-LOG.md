# Red-team log

Append-only; one entry per pass, newest last. The program, roles and entry
format are in [REDTEAM.md](REDTEAM.md). A finding listed here counted in
adjudication (reproduced against the repository or independently confirmed);
the findings dropped in adjudication are listed with the reason.

---

## 2026-09-25 — first full-claim pass (RUE-2463)

- **Trunk:** `f4ac09fc9`.
- **Kind:** full-claim, the program's first. Two targets were run, statements
  and docs. Definitions, bridge and trusted base were not attacked in this
  pass beyond what the statement agent read; their children (RUE-2465,
  RUE-2464, RUE-2457) carry them.
- **Models:** two fresh Claude Opus sessions, one per brief, run one after
  the other, neither shown our GUIDE, doc-comments, metatheory or issue
  threads. No cross-model auditor ran in this pass; RUE-2470 adds one.
  Adjudication: the coordinating Claude Opus session, against the
  repository at the trunk above.

### Packets

- **Statement agent.** `01-core-calculus.md` §§5–7 (lines 529–3587,
  unedited); the elaborated Lean statements of the 14 headline theorems
  (`check_sound`, `step_type_safety`, `step_progress`, `step_preservation`,
  `eval_sound`, `eval_complete`, `never_stuck_iff`, `eval_diverges_iff`,
  `Step.det`, `Config.trichotomy`, `no_double_free`, `drop_exactly_once`,
  `rest_exactly_once`, `drop_order`), taken from `lean/DIGEST.md`'s code
  blocks with every doc-comment stripped; and the definitions those
  statements use, closed transitively over the digest (245 entries, code
  blocks only).
- **Docs agent.** `docs/formal/README.md`, the first 120 lines of
  `docs/formal/lean/README.md`, and the same 14 statements, without the
  definitions.

**What the packets left out, and what it cost.** The 14 statements are not
the whole claim. Theorems that link them were not in the packet:
`step_iff` (`step` computes `Step`), `checkProgram_sound` (checker acceptance
gives `ProgramTyped`), `run_ne_returned` and `run_safe` (a run's outcome is
never `returned` or `broke`), the three linear theorems `no_linear_leak`,
`no_linear_overwrite` and `no_linear_discard`, and the kernel-checked examples
that each monitor fires. The closure extractor also missed five definitions
reached only through dot notation (`Decls.classOf`, `Decls.enumClassOf`,
`Decls.Names`, `Frame.popScope`, `FloatLit.exact`). Four of the statement
agent's findings are artefacts of that and are dropped below. REDTEAM.md now
requires the packet to carry every theorem §7 and the metatheory cite.

### Findings (counted)

Severities are the adjudicator's re-rating of the agents' own ratings, given
in the Source column; a finding's Issue is where it is filed as its own
issue, or commented onto an existing one.

| # | Target | Severity | Source | Issue | Proposed issue / evidence |
|---|---|---|---|---|---|
| R1 | statement | medium | F2 (raw: HIGH) | RUE-2477 | `no_double_free` says nothing about a run that does not terminate: add a statement over every reachable configuration's trace. `EvalRes.trace` is `[]` for `outOfFuel` (its DIGEST entry: "nothing for a refusal or exhausted fuel"), and by `eval_diverges_iff` a diverging program is `outOfFuel` at every fuel, so both counts are 0 for it. §7's bullet ("every stored value's destructor runs at most once") has no termination proviso; `run_trace_once` and `eval_conserves` are also over `run`/`eval` results. A double drop inside a `loop` that never exits is outside every multiplicity statement. In FIELD.md's terms (§6, Alpern & Schneider), this multiplicity bound is a safety property; R1 is a missing finite-prefix form of it, as RUE-2477 frames it. |
| R2 | statement | medium | F7 (raw: medium) | RUE-2478 | Whole-program "every owned value ends exactly once" is a prose composition of `drop_exactly_once` and `rest_exactly_once`, not a theorem. `Exact` quantifies over `a < List.length H`; at the program level (`run`: `H = []`) the quantifier is empty. 03-metatheory's "Why two statements, not one over `run`" argues the composition in prose ("So every owned value a checked run holds ends exactly once …") and names two ends neither theorem sees (an end inside the minting evaluation; `main`'s result). No statement discharges the per-window hypotheses (`FrameMatches`, `StoreCC`) at every intermediate state. |
| R3 | statement | medium | F1 (raw: HIGH) | RUE-2469 (comment) | The linear-consumption guarantee lives only in `eval`'s monitors: bring the monitor-fires witnesses into the statement layer. `Step.assign`, `Step.seqDrop`, `Step.endScope` and the unwinds drop a linear value through the same `dropCell`/`dropContents` path as `@drop`, with the same `Event.drop`. The three linear theorems are `run … ≠ .stuck .linearX`, which hold for an `eval` with no monitors. They are non-vacuous only because of `Examples.lean`'s `run … = .stuck .linearLeak`/`.linearOverwrite` witnesses and `Corpus.lean:938`'s `.linearDiscard` one, which are examples, not part of the stated claim. For RUE-2469 (witnesses) and RUE-2465 (a mutant that removes a monitor). |
| R4 | statement | medium | F5 (raw: HIGH) | RUE-2423, RUE-2467 (comment) | `step_preservation` states `SafeAt init`: its `∀ C` adds nothing, and no configuration typing is preserved. `Config.SafeAt` quantifies over everything reachable from `C`, so it is closed under `Steps` by transitivity, and the theorem is equivalent to `SafeAt … Config.init`. 03-metatheory l.202–203 and l.211–214 already disclose this ("needs no typing hypothesis"; "holds by construction, and the content is the fundamental lemma `RueCore.init_safeAt`"), as does the semantic form generally (FIELD.md §2, the `SafeAt` docstring). What is not disclosed: no configuration-typing relation is stated at all (RUE-2423), and the theorem's name still reads as the syntactic lemma (RUE-2467, the spine alignment). |
| R5 | statement | low | F3 (raw: HIGH) | RUE-2467 (comment) | `never_stuck_iff` under `ProgramTyped` is the conjunction of two theorems, not an equivalence with content. Under `h`, its left side is `eval_sound`'s first conjunct and its right side is `step_progress`. Its own docstring says the content is the forward direction on every program (`step_never_stuck_of_run`), which is not the headline. The spine should cite that theorem (RUE-2467, the spine alignment). |
| R6 | trusted base / digest | medium | F14 (raw: medium) | RUE-2479 | DIGEST prints no body or defining equations for Bool-valued predicates that are hypotheses or rule premises (`Expr.pendingSafe`, `Expr.breaks`, `OwnSt.join`, `noDtorPrefix`, `linearResidue`, …). DIGEST's preamble promises a body "whenever it is a type or a predicate … or is short enough to read"; these entries (e.g. DIGEST `Expr.pendingSafe`, `Expr.breaks`, `OwnSt.join`) carry the signature only. `pendingSafe` is a hypothesis of two spine theorems and the red agent could not check it is satisfiable; `OwnSt.join` is where §5.5's "consumed on some paths" rejection lives. For RUE-2457's trusted-base list and the statement/proof split. |
| R7 | statement | low | F11 (raw: medium) | RUE-2467 (comment) | `drop_order`'s `Lifo` constrains nothing on a step that does not pop, and `Blocks` is claimed for finished traces only. `Lifo S S' ls := S <+: S' ∨ …`; the first disjunct holds on every non-popping step. `Sublist` lets a popped cell go without a drop (left to `Exact`). 03-metatheory says "every **finished** run's trace", so the second point is disclosed; the first is not stated as a limit (RUE-2467, the spine alignment). |
| R8 | docs | high | H2 (raw: HIGH) | RUE-2476 (comment) | `docs/formal/README.md` says the calculus "brings `ArrayBuf`/`StrBuf` buffers inside the proved perimeter". README Contents, lines 195–198. Nothing about buffers is mechanized: 03-metatheory's fragment paragraph ends "no … loans, or buffers", and TRUST.md lists the §6.13.5 library obligations as not yet present. "Proved" should be "specified". The README sentence describes `01-core-calculus.md`, so "proved perimeter" may mean the paper's own perimeter rather than the Lean's; the report and the RUE-2476 comment both flag this reading. |
| R9 | docs | medium | H1 (raw: HIGH), M1 (raw: medium), H4 (raw: HIGH, folded) | RUE-2476 (comment) | `docs/formal/README.md`'s "the core's soundness holds for any well-formed core program" and "its §7 theorems as kernel-checked Lean statements" state for the whole core what is proved for a fragment. Lines 17–18 and 73–75. Proved: a fragment (03-metatheory, "Fragment today"), with exclusivity (RUE-2238) and no-use-after-free (RUE-2240) not mechanized, and exactly-once under `pendingSafe`. `docs/formal/README.md` never says "fragment" in these sentences (`lean/README.md` does, line 3), and neither README mentions the `pendingSafe` restriction, the `FloatModel` interface, or that "safe" admits panics and divergence. |
| R10 | docs | low | M6 (raw: medium); second half: adjudicator, not a red finding | RUE-2476 (comment) | `docs/formal/README.md` calls `rue-oracle` "the formal dynamic semantics", and 03-metatheory "a skeleton today". Line 122: since ADR-0097 the proved dynamic semantics is the Lean `Step`/`eval`, and no statement relates it to `rue-oracle`. Line 204: 03-metatheory is filled in (every §7 bullet but exclusivity and no-use-after-free names its theorem); this second half is the adjudicator's own observation from re-reading the docs, not something either red agent reported, and it is correct only because the line is now stale. |

Also reported and already tracked, so not proposed again: "verified checker"
and "verified interpreter" (README line 138, `lean/README.md` line 44), "any
pairwise disagreement is a defect" (`lean/README.md` line 48) and "surfaced
mechanically" (README line 21) are all in RUE-2476. The statement agent's
calculus-level "silent leak of a pending linear temporary on an early exit"
(its F12, inferred from §5.3 and §6.2 alone) is RUE-2316,
already documented as the carve-out `pendingSafe` exists for; the red agent
found it independently, and it is recorded here as a confirmation.

### Dropped in adjudication

| Red finding | Why dropped |
|---|---|
| `Config.trichotomy` holds for a stepper that always reports stuck | Packet artefact. `step_iff` (`Step M P C C' ↔ step M P C = .next C'`) ties `step` to `Step`; `Config.stuck_iff` states stuckness in `Step`'s terms. |
| `pendingSafe` might be false for every program | Packet artefact in part: its definition is syntactic (TraceExact.lean) and a `#guard` checks every accepted seed satisfies it. The residue, that DIGEST does not show the body, is R6. |
| `eval_sound` does not exclude `returned`/`broke` | Packet artefact: `run_ne_returned`, and `run_safe`'s case analysis, exclude both. |
| `check_sound` is not connected to `ProgramTyped` | Packet artefact: `checkProgram_sound`. Completeness is not claimed; the "verified checker" wording is RUE-2476. |
| `rest_exactly_once`'s `fuel`/`fuel + 1` coupling may be unsatisfiable | Does not hold: `eval (fuel + 1)` evaluates each lead at `fuel` (`Dynamics.lean`, e.g. the `seq`, `letIn` and `ret` arms). |
| DIGEST closure misses `Decls.classOf` and four others | Extractor artefact: all five have DIGEST entries. |
| Destructors are events, not nested runs | Disclosed: 03-metatheory's reduction-relation row and GUIDE §2 list it among `Step`'s departures from §6. |
| Exclusivity and no-use-after-free have no statement | Disclosed: 03-metatheory marks both *not yet mechanized* (RUE-2238). |
| `Entry.join` ignores the second entry's type; `Typed.loopDiv` does not require `Ωe.brk = []` | Not reproduced by reading; left to RUE-2465's mutants, which test exactly this kind of premise. |
| Docs: the bridge's corpus drops non-terminating cases; the generator's "typed by construction" has no theorem; "all 1,200 cases agree" is a snapshot | Process claims the README already hedges; no theorem is claimed. |

### Not pursued

Neither counted above nor dropped as a packet artefact or a disclosed point;
each raw finding is accounted for here.

| Raw finding | Where it goes, or why not pursued |
|---|---|
| **L4** (statement, LOW): no `FloatModel` instance is exhibited. All 14 statements quantify over `M : FloatModel`, so they are vacuous if it is uninhabited. | Filed as a priority item on RUE-2469, alongside the non-vacuity witnesses it already owns: exhibit an instance, or state inhabitation as an assumption. |
| **L6** (statement, LOW): `Tidy`'s `Local` lets evaluation turn any pre-existing cell outside `φ.env` dead, including a caller's live local; the harm is prevented only indirectly, by `step_progress`. | Not independently reproduced as an exploitable gap; a candidate mutant for RUE-2465. |
| **F13** bullets 2–4 (statement, MEDIUM): copy/move decided from the runtime value against a declared plan recomputed from contents (§6.3 says the dynamic rules "never … recompute"); aggregate identities minted into the same namespace as binding locations; no `H0` of string literals, no by-ref parameter paths, no buffers (§6.13). Bullet 1 (destructors as events, not nested runs) is dropped above as disclosed. | The copy/move-vs-plan point is equivalent on well-typed programs (see L5) and a candidate mutant for RUE-2465. The shared namespace is disclosed as harmless in the same finding. `H0`/by-ref/buffers are unmechanized scope, alongside R8's buffers gap and the exclusivity/no-use-after-free bullets already disclosed as not yet mechanized (RUE-2238). |
| **Docs M7** (MEDIUM): "a property proven once about the core holds for all ten" needs elaboration correctness, which is unspecified (`02-elaboration.md` is planned). | The same overclaim shape as R9 (whole-core vs. fragment); not separately counted. Route with R9's citation, RUE-2476. |
| **Docs M8** (MEDIUM): "kernel-checked" checks the proofs; it does not check that `Typed`/`Step`/`ProgramTyped` faithfully transcribe §5/§6. Faithfulness rests on review and the INDEX cross-reference. | Not a claim about what is proved so much as what "kernel-checked" can be read to promise; route to RUE-2476. |
| **Docs H4** in full (HIGH): the exactly-once guarantee is expression-level and needs `FrameMatches`/`StoreCC` at every intermediate state, not only `pendingSafe`; no statement instantiates it at `Config.init`. | R9 cites only the `pendingSafe` half. The rest is R2's and R7's territory (RUE-2478, RUE-2467); not separately filed. |
| **L1, L3, L5, L8–L11** (statement, LOW): `@drop` of a moved place is a no-op on a hole, not stuck (L1); `WfProgram.fns` requires unreachable functions well typed too, stronger than §7's scope (L3); the dynamic copy/move-vs-declared-plan equivalence (L5, see F13); `step_type_safety` is derivable from `step_progress` + `step_preservation` (L8); the residue-before-leaf drop order for a declared-linear projection is unstated (L9, see F14); `Typed.indexRead`'s `fullyOwned` is stronger than (Use-Untrackable-Dynamic-Copy), rejecting some §5-admitted programs (L10); no (Call-Bottom): a call to a divergent function types as continuing (L11). | Harmless or conservative on inspection. No action. |
| **Docs Lo1, Lo2, Lo6–Lo8** (LOW): `rue-oracle`'s fuel-bounded outcomes not distinguished from divergence (Lo1); the fixed-fuel corpus outcome is honest for accepted programs but unverified for rejected ones, overlapping H3 (Lo2); "everything hard … lives here and only here" is a design claim outside what predicates cover (Lo6); "precise, mechanizable" / "in time, mechanically proven" are hedged intent (Lo7); "the complete small-step dynamic semantics" is about the paper, not the Lean fragment (Lo8). | Process or hedge claims the statements cannot speak to and the docs do not present as theorems. No action. |

### Attacked and survived

Read the same way by the red agent and by us, with no gap either found:

- `Step.det`: determinism, unconditional; the red agent checked the rules'
  premises are pairwise exclusive and fresh identities deterministic.
- `step_progress`: progress for every configuration reachable from a checked
  program. It excludes use-after-move and use-after-drop redexes (a hole fails
  `Contents.toVal`, a dead cell fails `rootCell`), operand confusion (no rule
  for `OpRes.confused`) and an uncovered `match` tag.
- `step_type_safety`: equal to §7's type-safety bullet on the fragment
  (slightly stronger: the halting frame is `Frame.empty`).
- `eval_complete` and `eval_diverges_iff`: the two directions of the
  big-step/small-step equivalence (FIELD.md §3) for terminating and diverging
  runs; `eval_diverges_iff`'s backward direction has content no other
  statement has.
- `eval_sound`'s second and third conjuncts: every `ok`/`panic` result is
  `Step`-reachable with the same store, value and trace.
- `check_sound`, read as expression-level soundness, with (Sub-Never) folded
  into `CTy.fits` correctly.
- `no_double_free` on runs that finish or trap: identities are never reused
  and holes carry no identity.
- `drop_order`'s `Blocks` conjuncts: within a drop, the destructor first,
  fields in declaration order, elements ascending, an enum's active payload
  only (§6.11).
- `drop_exactly_once`'s first conjunct: local never-stuck from any matching
  frame, stronger than whole-program never-stuck, modulo `pendingSafe` (R6).
- The typing rules checked premise by premise against §5 (the use, drop,
  assign, index-write, seq, let, return, loop and match rules, and `WfFn`
  against (Fn)), as far as the signature-only helpers allowed (R6).
- Docs: type safety for closed checked programs under any float model; the
  interpreter/step-relation agreement in both directions for checked programs;
  the CI and fuel disclaimers in `lean/README.md`.

<details>
<summary>Brief: statement red agent (verbatim; <code>&lt;packet&gt;</code> is the pass's scratch directory)</summary>

```text
You are a red-team auditor for a machine-checked formalization. Your job is to attack what is CLAIMED, not how it is proved: assume every proof checks. Find where a statement says less than it appears to, says something other than what the calculus says, or could be satisfied vacuously.

You may read exactly three files and nothing else (no other files, no repository browsing, no web, no git):
- <packet>/calculus-s5-7.md — §§5–7 of a core calculus (static semantics, dynamic semantics, and §7's soundness claims).
- <packet>/statements.md — the Lean 4 statements of 14 headline theorems, as elaborated by the checker.
- <packet>/definitions.md — every definition those statements use, transitively (inductive types with their constructors, predicates with their bodies; a long function such as eval/check/step is given as a signature only).

You have no other explanation of these statements, deliberately. Do not guess what the authors intended; read what is written.

For EACH of the 14 theorems, give:
1. Your own English reading of the statement, precise enough that someone could check it against the Lean.
2. The calculus paragraph(s) it corresponds to (cite the §7 bullet and the §5/§6 rule or section).
3. Verdict: weaker / equal / stronger than the calculus claim, and state the gap exactly.
4. Suspicious hypotheses: any hypothesis that is stronger than it needs to be, excludes interesting programs, or is not obviously satisfiable in the cases that matter.
5. Vacuity: any way the statement could hold trivially — an unsatisfiable hypothesis, a disjunct that always holds, a predicate that is defined too weakly (check the definition bodies!), a quantifier over a set that might be empty, a conclusion that holds for any evaluator/step relation of the given type.

Then, across all 14: any definition in definitions.md whose body looks under-constrained or unfaithful to the calculus (e.g. a typing or stepping rule missing a premise the calculus states), and any pair of theorems whose combination is claimed or implied but not actually entailed.

Rate each finding: HIGH (the statement does not say what a reader of §7 would take it to say), MEDIUM (a real gap a careful reader would notice), LOW (wording/presentation). For each, give the exact Lean text and calculus text you are relying on.

Also list, explicitly, the statements you attacked and could NOT break — where your reading matches §7's claim with no gap you can find. Those matter as much as the findings.

Do not edit any file. Write your full report to <packet>/red-statement-report.md and return a short summary.
```

</details>

<details>
<summary>Brief: docs red agent (verbatim; <code>&lt;packet&gt;</code> as above)</summary>

```text
You are a red-team auditor for a machine-checked formalization. Your job is to find claims in its documentation that the proved statements do not support. Assume every proof checks; the question is only whether the docs say more than the theorems do.

You may read exactly three files and nothing else (no other files, no repository browsing, no web, no git):
- <packet>/docs-formal-README.md — the formal semantics' top-level README.
- <packet>/docs-lean-README-1-120.md — the first 120 lines of the mechanization's README.
- <packet>/statements.md — the Lean 4 statements of the 14 headline theorems, as elaborated by the checker (namespace RueCore; definitions are not included — where a claim depends on what a named predicate means, say so and say what the predicate would have to mean for the claim to hold).

List EVERY claim the two docs make about what is proved, verified, guaranteed, covered, or established — including implicit ones (e.g. "the compiler is correct", "every program", "the calculus is sound", a scope word such as "all" or "every", a claim about CI or tooling that enforces something). For each claim give:
- the exact quoted sentence and file/line;
- the statement(s) that would have to support it;
- verdict: SUPPORTED / PARTLY (say what is missing) / UNSUPPORTED (no statement says this) / NOT-A-THEOREM-CLAIM (a claim about process, tooling or intent that the statements cannot speak to — flag if it reads as a guarantee);
- severity for anything not SUPPORTED: HIGH (a reader would believe something false about what is proved), MEDIUM, LOW.

Pay particular attention to: scope (fragment vs whole language vs compiler), the relation claimed between the formal model and the real compiler, words like "sound", "safe", "complete", "adequate", "never", "exactly once", and anything about fuel, panics, or assumptions (such as a float model) that the docs omit.

Also list the claims you checked and found fully SUPPORTED.

Do not edit any file. Write your full report to <packet>/red-docs-report.md and return a short summary.
```

</details>

---

## 2026-09-26 — non-vacuity pass (RUE-2469, part 1)

- **Trunk:** `1f227dbd3`.
- **Kind:** witness pass, not a red-agent pass: no fresh session attacked
  anything. It answers the vacuity findings of the first pass (L4, and the
  non-vacuity half of R3) by construction; sharpness, including R3's
  monitor-fires witnesses, is RUE-2485.
- **What was built.**
  - **L4 closed.** `Float.exactModel` (`lean/RueCore/Float/Lemmas.lean`)
    proves every `FloatModel` law of `Float.exactOps`, so the laws have a
    model. No law turned out false of `exactOps`.
  - **Every spine theorem is witnessed.** Twelve Spec statements
    (`lean/RueCore/Spec/Witnesses.lean`, listed in `Spec.witnesses`) show the
    hypotheses of all 36 satisfiable by non-trivial programs, written out:
    one per construct class (destructors, linear values, loops, arrays, enums
    with `match`, early `return`, `@panic`, floats), a divergent one, an
    unchecked stuck one, the model and the empty frame. The lint now fails on
    a spine theorem no witness names, and Comparator and the fingerprints
    cover the witnesses.
  - **Checker profile.** `ruecore-corpus --profile`: 140 of 174 seed cases
    accepted, 34 rejected, 13 of those running to a value (conservative
    rejections); 115 of 200 generated programs (seed 7) accepted. No accepted
    program is refused. `errorClasses_rejected` checks in the kernel one
    rejected corpus case per error class.
- **Findings.** None against the claim: every hypothesis was satisfiable by
  the programs tried. One tooling finding: the core library's
  `Nat.lt_of_mul_lt_mul_right`, `Nat.pow_lt_pow_right`,
  `Nat.pow_le_pow_iff_right` and `Nat.sqrt_le` reach `Classical.choice` on
  this toolchain (4.33.1), so `Float/Lemmas.lean` reproves them.
