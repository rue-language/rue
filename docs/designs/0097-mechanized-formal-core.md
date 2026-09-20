---
id: 0097
title: "Mechanized formal core: a Lean 4 fourth view of the language, grown as a non-blocking experiment"
status: proposal
tags: [process, formal, testing, documentation]
feature-flag: null
created: 2026-09-20
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-1882", "RUE-2226", "RUE-207", "RUE-206", "RUE-50", "RUE-305", "RUE-2227", "RUE-2228", "RUE-2233", "RUE-2237", "RUE-2241", "RUE-2245", "RUE-2247", "RUE-2249", "RUE-2251", "RUE-2282", "RUE-2283", "RUE-2289", "ADR-0082", "ADR-0083"]
---

# ADR-0097: Mechanized formal core

## Status

Proposal, filed under RUE-1882. Acceptance ratifies the seven decisions in
§Decision; it does not by itself land any Lean code. The spike that motivates
this record lands separately under RUE-2226 once this ADR is accepted, and
the project that grows it is tracked in Linear as "Formal core mechanization"
(RUE-1882 is its first issue). This ADR reaches `implemented` when
the metatheory document the calculus plans (`03-metatheory.md`, next to
`docs/formal/01-core-calculus.md`) cites a kernel-checked theorem for every
§7 bullet (RUE-207).

## Summary

`docs/formal/01-core-calculus.md` §7 states seven memory-safety theorems and
promises a `03-metatheory.md` that proves them. A spike (2026-08-28; its note,
`lean-mechanization-spike.md`, lands under `docs/notes/` with RUE-2226)
mechanized a fragment of the
calculus in Lean 4 and proved the fragment's slice of §7 with zero `sorry`
and no dependencies beyond core Lean. This ADR adopts that artifact as a
**fourth view** of the language beside the prose, the core, and the compiler;
extends RUE-305's "disagreement is a defect, no precedence" rule to cover it;
fixes interpreter-style safety as the theorem form; adds a sixth step to the
formal-core extension rubric so a language change is never blocked on a
proof; and fixes the posture as **experimental and non-blocking**, with
nothing in CI until an independent review confirms a safety proof over the
full core dynamics. Two tenets govern the work: every artifact must be
explainable to a beginner and inspectable by an expert, and every phase
boundary gets an adversarial review by an agent from a different model
family than the implementers.

## Context

The formal core was written "in plain ASCII so it can be transcribed into a
proof assistant" (`docs/formal/README.md`, purpose 2). The spike tested that
claim and found it held: rules transcribed one-for-one, and every place the
encoding needed a decision, the calculus text had already made it with a
RUE citation. It also found the reason to keep going. The preservation
invariant could not be the naive "MovedOut means moved": the §5.5
conservative join makes static state an approximation, so the invariant
that works is asymmetric, and its "no live *linear* value behind `MovedOut`"
clause is exactly what makes the linear-leak and linear-overwrite refusals
unreachable. RUE-387, RUE-1591, RUE-1614, and RUE-1615 were all bugs in this
class of cross-rule invariant, each found by hand reconciliation between the
spec and the compiler. A mechanization checks that class by construction.

Three properties of the spike shape the decisions below.

- **Same shape as the oracle.** The spike proves safety over a total
  definitional interpreter rather than a small-step relation. The Lean
  artifact is therefore an interpreter that produces a result plus a drop
  trace, which is what `crates/rue-oracle` is. That makes a three-way
  differential loop (Lean model, oracle, compiler) a natural extension of
  RUE-50 rather than a new architecture. AWS Cedar's
  verification-guided development is the production precedent: a Lean
  model of the language, proofs of its key properties, and differential
  random testing between the model and the Rust implementation
  (<https://github.com/cedar-policy/cedar-spec>).
- **Cheap to build, not cheap to fetch.** Core Lean suffices; no Mathlib.
  A cold build is seconds. The toolchain is a per-platform, SHA-pinned
  release archive, the same thing `elan` downloads, and a fit for the
  repository's `toolchain_distribution` rule (`toolchains/distribution.bzl`,
  RUE-2003) once that rule learns zstd, since Lean ships `.tar.zst` and
  `.zip` only. Its shape is size, not file count: about 17,500 files but
  2.7 GB unpacked from a 570 MB archive, comparable to Zig by count and
  eight times its bytes, so the rule's CAS opt-out matters here for cache
  volume as much as for the RUE-2003 truncation.
- **Reviewable in an unusual way.** The kernel checks the proofs. What it
  cannot check is whether a theorem *statement* says what the calculus says,
  whether the fragment boundary is stated honestly, or whether a printer
  emits programs inside the core's image. Review effort therefore
  concentrates on statements, and that review must be independent of the
  authors.

What the spike did not settle, and this ADR must: where the artifact lives
and under what authority; what theorem form the metatheory blesses; how the
work relates to CI and the merge queue; and how it stays honest as it grows
across many sessions and several agents.

## Decision

1. **Adopt the artifact, with Buck managing the toolchain.**
   `docs/formal/lean/` is the mechanization home: Lean package `RueCore`,
   zero dependencies beyond core Lean, built with `lake build`. The Lean
   toolchain is a Buck `toolchain_distribution` beside Rust and Zig, for
   all four supported platforms: a SHA-pinned archive fetched from its
   origin and extracted with the CAS upload opt-out, so the tree never
   enters the remote cache (the rule gains a zstd path to do it). The build
   is a Buck target (a genrule running `lake build` against that toolchain,
   emitting a build stamp and the axioms report via the toolchain's own
   `leanchecker`) that `scripts/rue` exposes. The `lean-toolchain` file stays as the pin lake
   and the editor extension read, and a validator asserts the Buck pin
   matches it so the two cannot drift; `elan` remains an optional
   developer convenience, never a CI dependency. Until decision 5's gate
   is met, the target is build-only: it carries no test tier, and
   `buck2 test` does not build non-test targets, so neither the nightly
   `//...` sweep nor any required lane requests it. The spike lands as the seed under RUE-2226 after this
   ADR is accepted.

2. **Authority: four views, no precedence.** RUE-305 ratified that the
   prose, the core, and the compiler are three views of one language, and
   that a genuine disagreement is a defect in one of them, fixed rather than
   resolved by precedence. The mechanization is the fourth view under the
   same rule. A Lean statement that disagrees with the calculus text is a
   defect in one of the two; the reconciler decides which, and the
   traceability rows follow. Where the mechanization forces the calculus
   text into a more explicit form (the §5.7 divergence machinery is the
   expected case), that rewrite is a normal spec change and a deliverable,
   not a workaround kept on the Lean side.

3. **Theorem shape: interpreter-style safety.** The primary §7 statement
   form is: for a well-typed program, the definitional interpreter returns a
   well-typed value, a defined panic, or (once fuel exists) `outOfFuel`, and
   never a named violation. Progress and preservation are one statement,
   with "Σ faithfully tracks the store's initialization" (§7's phrase) as the
   preservation invariant. Named corollaries per §7 bullet are the
   deliverable the metatheory cites. §6 is written small-step, so `eval` is
   a second presentation of the same dynamics, and a theorem about `eval`
   is a theorem about §6 only once the two are proved to agree. That
   adequacy (a mechanized §6 reduction relation, `eval` sound and complete
   modulo fuel with respect to it) is therefore a Phase C deliverable
   (RUE-2289) that checkpoint C requires, not a later promise; §6's heading
   and §7's first bullet are reworded as a normal spec change (RUE-2226) so
   the views agree. A refusal is a positive result in this form, so every
   `Violation` and panic kind gets a kernel-checked witness per slice, and
   fuel, when it lands, brings monotonicity and no-masking lemmas
   (RUE-2233) so `outOfFuel` can never stand in for a violation.
   `03-metatheory.md` records the choice and the reason (same corollaries,
   far less overhead, shape-identity with the oracle).

4. **Rubric step 6.** The extension rubric in `docs/formal/README.md` gains a
   sixth step: *extend the mechanization, or file the gap as a tracked issue
   in the "Formal core mechanization" project.* The rubric's existing rule
   stands: a change that cannot be expressed by touching the rubric's steps
   is a framework change to escalate. A language change is never blocked on
   a proof, only tracked; drift between the four views is a red bridge run
   or a filed issue, never a silent divergence.

5. **Posture and promotion criteria.** The mechanization is experimental and
   non-blocking. Nothing runs in CI until (a) the safety theorem covers the
   full core dynamics for by-value programs (every Phase C slice: structs,
   paths, enums, arrays, declared-linear destructure, the leaf and float
   inventory, calls, loops, drop order, no-double-free), (b) that theorem
   is about §6 and not only about `eval`: the fuel lemmas (RUE-2233) and
   the adequacy lemma (RUE-2289) are proved for the same fragment, and (c)
   an independent review (decision 7, checkpoint C, RUE-2251) confirms the
   proof is established. Promotion then certifies exactly that: §6's
   dynamics are safe on the by-value fragment, with loans and buffers still
   outside the claim. The mechanism is the test-tier system, read
   correctly: `premerge` and `slow` are both pre-merge selectors (`slow` is
   the oracle-diff corpus lane), `stress` runs in the nightly release sweep,
   that sweep runs `//...` and so reaches every tier not excluded by label,
   and `scripts/rue test` runs premerge plus slow. Before the gate there is
   no test target, only the build target of decision 1. At the gate,
   RUE-2241 wraps that target in a test (`lake build`, `leanchecker`, the
   axioms assertion, the bridge corpus) and picks its tier: `stress`, riding
   the nightly sweep, or a new experimental tier with its own scheduled lane
   and `--exclude` labels in the sweep and in `test.sh`. The
   scheduled-workflow health check blocks a merge only for a schedule that
   has never succeeded (RUE-1507), so a new lane's first two runs are the
   exposure and an ordinary red run is a warning either way. That choice is
   RUE-2241's, made with the measured fetch cost in hand. Promotion to a
   merge-queue gate is a separate decision this ADR names but does not
   make.

6. **Explainability is a tenet, not a follow-up.** Formal work only its
   author can read is not verified in any useful sense, and the bus-factor
   risk is otherwise unmitigated. The project therefore builds, alongside
   the proofs, a reader's guide for a Rue contributor with no Lean
   (RUE-2245), a generated rule↔Lean↔paragraph cross-reference index that
   errors on uncited declarations so it cannot rot (RUE-2245), a derivation
   and trace visualizer (RUE-2246), and an expert validation surface: a
   generated statement digest with no proofs, a trust report (`#print
   axioms`, checker output, `sorry` count, declared assumptions), and a
   thirty-minute validation procedure (RUE-2247). Every slice of new
   mechanization ships a worked example, cited doc-comments, and readable
   theorem statements as deliverables. The test of these artifacts is
   decision 7: reviewers read them first, and a question a reviewer has to
   ask the authors is a defect in the artifacts.

7. **Cross-model-family adversarial review at checkpoints.** At each phase
   boundary of the project, an agent from a different model family than the
   implementers reviews statement fidelity against the calculus, printer
   faithfulness against the surface language, and scope claims against the
   code, with a maintainer as the human of record. The implementers are
   Claude models, so the reviewers are Steve's Codex agents (`agent:codex`).
   Each checkpoint is a Linear issue (RUE-2249 through RUE-2252) that gates
   the work its brief covers, as the Phases list records, and closes when
   the review is delivered and its findings are filed, not when they are
   fixed. Rationale: RUE-305's
   discipline needs an independent reader for the fourth view, and
   same-family review shares blind spots.

### The bridge, in one paragraph

`rue-oracle` interprets the compiler's CFG built from Rue source, so it
cannot consume core syntax; an elaborator from Rue to core is RUE-206 and
expensive. The bridge therefore runs the cheap direction: a printer from
core syntax to Rue source on the Lean side, a JSON corpus carrying each
program, its checker verdict, and its expected observation (value or panic
kind, plus the drop trace made observable through printing destructors), and
a `rue-oracle-diff` mode on the Rust side that compares the compiler's
accept/reject against the Lean checker and the oracle's and native binary's
observations against the Lean interpreter (RUE-2227, RUE-2228). Every
pairwise disagreement is reported by which pair disagrees, never by which
side is wrong. This is built at fragment scope immediately after the spike
lands, so every later extension ships with three-way corpus cases from the
start. A random core-program generator (RUE-2229) follows.

### Scope and non-goals

- The runtime core only. Comptime evaluation and monomorphization remain an
  elaboration layer outside the theorems (RUE-206; §9 item 1 of the
  calculus).
- Raw pointers and `unchecked` code stay outside the core, buffers are
  modeled through the §6.13 container equations with the §6.13.5 library
  obligations as explicit assumed interfaces, and loans are second-class
  and never escape a call. These are the calculus's §9 items 2 and 3,
  assumed here and ratified by a maintainer under RUE-2283 before Phase D
  starts; item 5 (sign-only NaN) is assumed the same way by the leaf
  inventory. A semantic proof of the obligations is a later decision.
- No Mathlib, no `native_decide`, no `Classical.choice`. The trust report
  asserts the axiom set is `propext` and `Quot.sound` only, plus declared
  obligation interfaces once they exist.
- The milestone-by-milestone plan is not in this ADR. It lives in Linear
  and would go stale here.

## Implementation Phases

Tracked as milestones of the "Formal core mechanization" Linear project.

- [ ] **Phase A: Land** - RUE-1882 (this ADR), RUE-2226 (spike lands),
      RUE-2249 (review checkpoint A gates Phase C)
- [ ] **Phase B: Bridge at fragment scope** - RUE-2227, RUE-2228, RUE-2229,
      RUE-2250 (checkpoint B gates CI)
- [ ] **Phase C: Grow the fragment** - RUE-2230 through RUE-2237, RUE-2282,
      RUE-2289 (adequacy), RUE-2251 (checkpoint C is the "established
      proof" gate)
- [ ] **Phase D: Loans and the store** - RUE-2283 (the §9 rulings),
      RUE-2238, RUE-2239, RUE-2240, RUE-2252 (checkpoint D gates the
      metatheory)
- [ ] **Cross-cutting: Explainability** - RUE-2245, RUE-2246, RUE-2247
- [ ] **Phase E: Institutionalize** - RUE-207 (`03-metatheory.md` filled;
      this ADR becomes `implemented`), RUE-2241 (CI test target and tier)

## Consequences

### Positive

- The §7 theorems stop being prose promises. Each becomes a named,
  kernel-checked statement with a traceability row, and the class of
  cross-rule invariant bug that produced RUE-387, RUE-1591, RUE-1614, and
  RUE-1615 is checked by construction as the fragment grows.
- Drift between the four views becomes a red bridge run rather than a
  latent divergence, in CI once the tier exists and at every checkpoint
  before that, and disagreement reports name the pair, so the RUE-305 rule
  is applied mechanically rather than by recollection.
- The theorem form matches the oracle's shape, so the Lean model doubles as
  a second executable semantics and the existing RUE-50 harness gains a
  third participant without a new architecture.
- The explainability artifacts give maintainers, contributors, and outside
  readers a way to check what is claimed without trusting the authors, and
  give future language changes a rubric step instead of a research task.

### Negative (accepted costs)

- Every language change that touches the core now carries a proof
  obligation. Mitigations: proofs stay boring (structural inductions over
  syntax-directed rules, the spike's pattern); extension is checker-first,
  so `check` grows cheaply and `check_sound` follows mechanically; rubric
  step 6 makes "file the gap" an honest exit, so the obligation is tracked
  rather than blocking. The Phase C slices are independent in meaning, not
  in files: all extend one inductive and one induction, so they serialize
  on the package and the second to land re-proves.
- A third hermetic toolchain distribution in the Buck graph, the largest
  by bytes. Because its extracted tree stays out of the CAS, every lane
  with a fresh `buck-out` whose graph reaches the Lean target fetches about
  570 MB and extracts 2.7 GB, more than twice RUE-2003's measured cost for
  all current distributions combined. Accepted because only lanes that
  request the target pay: before the gate none do, and after it only the
  lane RUE-2241 picks does. The pin validator and the rule's zstd path are
  the price.
- Reviewer capacity. Four checkpoints, each a real review by a maintainer's
  agent fleet. Accepted because the alternative, same-family self-review of
  theorem statements, is the failure mode this ADR exists to avoid.
- The §5.7 loop corner and path-keyed Σ may force calculus rewrites. Treated
  as deliverables under decision 2; the cost is spec-change review, which
  the project already pays.
- Buffers may want separation logic. If the obligation-interface approach
  proves unsatisfying, the project stops at the interface and files the
  design question; the obligations stay explicit assumptions either way,
  which is already §6.13.5's framing. No Iris dependency is taken on
  speculatively.

## Open Questions

- **Merge-queue gate.** Named in decision 5, deliberately not decided. The
  question returns when the scheduled tier has a track record and the
  proof-maintenance cost is measured rather than estimated.
- **Which tier at the gate.** `stress` rides the nightly `//...` sweep,
  which has no owner and is often red, so a Lean failure there is easy to
  miss; a dedicated experimental tier costs a tier-vocabulary change, a
  scheduled lane, and exclusion labels, and meets the health check only in
  its first two runs. RUE-2241 decides with the measured cost in hand.
- **§9 items 2, 3, and 5.** Assumed by the mechanization, ratified under
  RUE-2283; this ADR's `implemented` status is contingent on them.
- **Obligation proofs.** Whether to prove the §6.13.5 library obligations
  semantically, and with what logic, is deferred to Phase D's end.
- **Reviewer rotation.** Decision 7 names Codex because that is the
  available different family today. If the implementer fleet changes, the
  rule is the family difference, not the vendor.

## Future Work

- `02-elaboration.md` (RUE-206): the surface-to-core layer. The bridge
  printer is the inverse direction and is not a substitute.
- Promotion of the scheduled tier to a merge-queue gate.
- Semantic proofs of the §6.13.5 obligations.

## References

- `lean-mechanization-spike.md` under `docs/notes/` (lands with RUE-2226):
  the spike's findings, per-§7 assessment, and original milestone ladder.
- `docs/formal/README.md`: the three views and the extension rubric this
  ADR extends; `docs/formal/01-core-calculus.md` §7 and §9.
- RUE-305 (authority rule), RUE-50 (differential oracle), RUE-207
  (metatheory), RUE-1882 (this decision), Linear project "Formal core
  mechanization".
- ADR-0082 (a process ADR of the same shape), ADR-0083 (a phased project
  ADR whose review rounds were its ratification).
- AWS Cedar's verification-guided development as the external precedent
  for the bridge: the Lean formalization and differential-testing harness
  at <https://github.com/cedar-policy/cedar-spec>, and the paper "Cedar: A
  New Language for Expressive, Fast, Safe, and Analyzable Authorization"
  (Cutler et al., OOPSLA 2024; extended version at
  <https://arxiv.org/abs/2403.04651>).
