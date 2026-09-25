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
  statements use, closed transitively over the digest (259 entries, code
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

Each has a proposed issue title; the coordinator files them.

| # | Target | Severity | Proposed issue | Evidence |
|---|---|---|---|---|
| R1 | statement | medium | `no_double_free` says nothing about a run that does not terminate: add a statement over every reachable configuration's trace | `EvalRes.trace` is `[]` for `outOfFuel` (its DIGEST entry: "nothing for a refusal or exhausted fuel"), and by `eval_diverges_iff` a diverging program is `outOfFuel` at every fuel, so both counts are 0 for it. §7's bullet ("every stored value's destructor runs at most once") has no termination proviso; `run_trace_once` and `eval_conserves` are also over `run`/`eval` results. A double drop inside a `loop` that never exits is outside every multiplicity statement. |
| R2 | statement | medium | Whole-program "every owned value ends exactly once" is a prose composition of `drop_exactly_once` and `rest_exactly_once`, not a theorem | `Exact` quantifies over `a < List.length H`; at the program level (`run`: `H = []`) the quantifier is empty. 03-metatheory's "Why two statements, not one over `run`" argues the composition in prose ("So every owned value a checked run holds ends exactly once …") and names two ends neither theorem sees (an end inside the minting evaluation; `main`'s result). No statement discharges the per-window hypotheses (`FrameMatches`, `StoreCC`) at every intermediate state. |
| R3 | statement | medium | The linear-consumption guarantee lives only in `eval`'s monitors: bring the monitor-fires witnesses into the statement layer | `Step.assign`, `Step.seqDrop`, `Step.endScope` and the unwinds drop a linear value through the same `dropCell`/`dropContents` path as `@drop`, with the same `Event.drop`. The three linear theorems are `run … ≠ .stuck .linearX`, which hold for an `eval` with no monitors. They are non-vacuous only because of `Examples.lean`'s `run … = .stuck .linearLeak`/`.linearOverwrite` witnesses and `Corpus.lean:938`'s `.linearDiscard` one, which are examples, not part of the stated claim. For RUE-2469 (witnesses) and RUE-2465 (a mutant that removes a monitor). |
| R4 | statement | medium | `step_preservation` states `SafeAt init`: its `∀ C` adds nothing, and no configuration typing is preserved | `Config.SafeAt` quantifies over everything reachable from `C`, so it is closed under `Steps` by transitivity, and the theorem is equivalent to `SafeAt … Config.init`. The semantic form is disclosed (FIELD.md §2, the `SafeAt` docstring, 03-metatheory), but the redundancy of the quantifier is not, and the name still reads as the syntactic lemma. For RUE-2470 / the spine alignment. |
| R5 | statement | low | `never_stuck_iff` under `ProgramTyped` is the conjunction of two theorems, not an equivalence with content | Under `h`, its left side is `eval_sound`'s first conjunct and its right side is `step_progress`. Its own docstring says the content is the forward direction on every program (`step_never_stuck_of_run`), which is not the headline. The spine should cite that theorem. |
| R6 | trusted base / digest | medium | DIGEST prints no body or defining equations for Bool-valued predicates that are hypotheses or rule premises (`Expr.pendingSafe`, `Expr.breaks`, `OwnSt.join`, `noDtorPrefix`, `linearResidue`, …) | DIGEST's preamble promises a body "whenever it is a type or a predicate … or is short enough to read"; these entries (e.g. DIGEST `Expr.pendingSafe`, `Expr.breaks`, `OwnSt.join`) carry the signature only. `pendingSafe` is a hypothesis of two spine theorems and the red agent could not check it is satisfiable; `OwnSt.join` is where §5.5's "consumed on some paths" rejection lives. For RUE-2457's trusted-base list and the statement/proof split. |
| R7 | statement | low | `drop_order`'s `Lifo` constrains nothing on a step that does not pop, and `Blocks` is claimed for finished traces only | `Lifo S S' ls := S <+: S' ∨ …`; the first disjunct holds on every non-popping step. `Sublist` lets a popped cell go without a drop (left to `Exact`). 03-metatheory says "every **finished** run's trace", so the second point is disclosed; the first is not. |
| R8 | docs | high | `docs/formal/README.md` says the calculus "brings `ArrayBuf`/`StrBuf` buffers inside the proved perimeter" | README Contents, lines 195–198. Nothing about buffers is mechanized: 03-metatheory's fragment paragraph ends "no … loans, or buffers", and TRUST.md lists the §6.13.5 library obligations as not yet present. "Proved" should be "specified". |
| R9 | docs | medium | `docs/formal/README.md`'s "the core's soundness holds for any well-formed core program" and "its §7 theorems as kernel-checked Lean statements" state for the whole core what is proved for a fragment | Lines 17–18 and 73–75. Proved: a fragment (03-metatheory, "Fragment today"), with exclusivity (RUE-2238) and no-use-after-free (RUE-2240) not mechanized, and exactly-once under `pendingSafe`. `docs/formal/README.md` never says "fragment" in these sentences (`lean/README.md` does, line 3), and neither README mentions the `pendingSafe` restriction, the `FloatModel` interface, or that "safe" admits panics and divergence. |
| R10 | docs | low | `docs/formal/README.md` calls `rue-oracle` "the formal dynamic semantics", and 03-metatheory "a skeleton today" | Line 122: since ADR-0097 the proved dynamic semantics is the Lean `Step`/`eval`, and no statement relates it to `rue-oracle`. Line 204: 03-metatheory is filled in (every §7 bullet but exclusivity and no-use-after-free names its theorem). |

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
| `Config.trichotomy` holds for a stepper that always reports stuck | Packet artefact. `step_iff` (`Step M P C C' ↔ step M P C = .next C'`) and `step_stuck_isStuckState` tie `step` to `Step`; `Config.stuck_iff` states stuckness in `Step`'s terms. |
| `pendingSafe` might be false for every program | Packet artefact in part: its definition is syntactic (TraceExact.lean) and a `#guard` checks every accepted seed satisfies it. The residue, that DIGEST does not show the body, is R6. |
| `eval_sound` does not exclude `returned`/`broke` | Packet artefact: `run_ne_returned`, and `run_safe`'s case analysis, exclude both. |
| `check_sound` is not connected to `ProgramTyped` | Packet artefact: `checkProgram_sound`. Completeness is not claimed; the "verified checker" wording is RUE-2476. |
| `rest_exactly_once`'s `fuel`/`fuel + 1` coupling may be unsatisfiable | Does not hold: `eval (fuel + 1)` evaluates each lead at `fuel` (`Dynamics.lean`, e.g. the `seq`, `letIn` and `ret` arms). |
| DIGEST closure misses `Decls.classOf` and four others | Extractor artefact: all five have DIGEST entries. |
| Destructors are events, not nested runs | Disclosed: 03-metatheory's reduction-relation row and GUIDE §2 list it among `Step`'s departures from §6. |
| Exclusivity and no-use-after-free have no statement | Disclosed: 03-metatheory marks both *not yet mechanized* (RUE-2238). |
| `Entry.join` ignores the second entry's type; `Typed.loopDiv` does not require `Ωe.brk = []` | Not reproduced by reading; left to RUE-2465's mutants, which test exactly this kind of premise. |
| Docs: the bridge's corpus drops non-terminating cases; the generator's "typed by construction" has no theorem; "all 1,200 cases agree" is a snapshot | Process claims the README already hedges; no theorem is claimed. |

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
