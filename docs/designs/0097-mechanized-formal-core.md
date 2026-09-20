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
relates: ["RUE-1882", "RUE-2226", "RUE-207", "RUE-206", "RUE-50", "RUE-305", "RUE-2227", "RUE-2228", "RUE-2237", "RUE-2241", "RUE-2245", "RUE-2247", "RUE-2249", "ADR-0082", "ADR-0083"]
---

# ADR-0097: Mechanized formal core

## Status

Proposal, filed under RUE-1882. Acceptance ratifies the seven decisions in
§Decision; it does not by itself land any Lean code. The spike that motivates
this record lands separately under RUE-2226 once this ADR is accepted, and
the project that grows it is tracked in Linear as "Formal core mechanization"
(RUE-1882 is its first issue). This ADR reaches `implemented` when
`docs/formal/03-metatheory.md` cites a kernel-checked theorem for every
§7 bullet of the core calculus (RUE-207).

## Summary

`docs/formal/01-core-calculus.md` §7 states seven memory-safety theorems and
promises a `03-metatheory.md` that proves them. A spike (2026-08-28,
`docs/notes/lean-mechanization-spike.md`) mechanized a fragment of the
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
  RUE-50 rather than a new architecture; AWS Cedar's
  verification-guided-development practice is the production precedent.
- **Cheap to build and run.** Core Lean suffices; no Mathlib. A cold build
  is seconds. The toolchain is user-local via `elan` and never enters the
  Buck graph or the remote cache (the RUE-2003 lesson about toolchains in
  the CAS applies).
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

1. **Adopt the artifact.** `docs/formal/lean/` is the mechanization home:
   Lean package `RueCore`, toolchain pinned by `lean-toolchain`, built with
   `lake build`, zero dependencies beyond core Lean. `elan` is a user-local
   install. Nothing touches the Buck graph, `scripts/rue`, or the remote
   cache. The spike lands as the seed under RUE-2226 after this ADR is
   accepted.

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
   deliverable the metatheory cites. A small-step relation is added only if
   a theorem needs one, with the standard step-function/relation equivalence
   lemma; `03-metatheory.md` records this choice and the reason (same
   corollaries, far less mechanization overhead, and shape-identity with the
   oracle).

4. **Rubric step 6.** The extension rubric in `docs/formal/README.md` gains a
   sixth step: *extend the mechanization, or file the gap as a tracked issue
   in the "Formal core mechanization" project.* The rubric's existing rule
   stands: a change that cannot be expressed by touching the rubric's steps
   is a framework change to escalate. A language change is never blocked on
   a proof, only tracked; drift between the four views is a red bridge run
   or a filed issue, never a silent divergence.

5. **Posture and promotion criteria.** The mechanization is experimental and
   non-blocking. Nothing runs in CI until (a) the safety theorem covers the
   full core dynamics for by-value programs (structs, paths, enums, calls,
   loops, drop order, no-double-free; RUE-2237 is the terminal issue) and
   (b) an independent review (decision 7, checkpoint C) confirms the proof
   is established. The first CI leg is then a scheduled tier in the pattern
   of `slow` (RUE-2241): `elan` bootstrap, `lake build`, `lean4checker`, an
   axioms assertion, and the bridge corpus, registered with the
   scheduled-workflow health check so a silently failing schedule turns
   required CI red (RUE-1507). Promotion to a merge-queue gate is a separate
   decision this ADR names but does not make; it needs a track record from
   the scheduled tier first.

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
   Each checkpoint is a Linear issue (RUE-2249 through RUE-2252) that blocks
   the next phase's start and closes when the review is delivered and its
   findings are filed, not when they are fixed. Rationale: RUE-305's
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
- Raw pointers and `unchecked` code stay outside the core. Buffers are
  modeled through the §6.13 container defining equations with the §6.13.5
  library obligations as explicit, assumed interfaces (§9 item 2); a
  semantic proof of the obligations themselves is a later, separate
  decision.
- Loans are second-class and never escape a call (§9 item 3). Phase D
  begins with the maintainer confirming this and §9 item 2.
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
- [ ] **Phase C: Grow the fragment** - RUE-2230 through RUE-2237, RUE-2251
      (checkpoint C is the "established proof" gate)
- [ ] **Phase D: Loans and the store** - RUE-2238, RUE-2239, RUE-2240,
      RUE-2252 (checkpoint D gates the metatheory)
- [ ] **Cross-cutting: Explainability** - RUE-2245, RUE-2246, RUE-2247
- [ ] **Phase E: Institutionalize** - RUE-207 (`03-metatheory.md` filled;
      this ADR becomes `implemented`), RUE-2241 (scheduled CI tier)

## Consequences

### Positive

- The §7 theorems stop being prose promises. Each becomes a named,
  kernel-checked statement with a traceability row, and the class of
  cross-rule invariant bug that produced RUE-387, RUE-1591, RUE-1614, and
  RUE-1615 is checked by construction as the fragment grows.
- Drift between the four views becomes a red bridge run rather than a
  latent divergence, and disagreement reports name the pair, so the
  RUE-305 rule is applied mechanically rather than by recollection.
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
  rather than blocking.
- A second toolchain in the repository, user-local and undeclared to Buck.
  Accepted because nothing depends on it until the scheduled tier, and the
  tier bootstraps it explicitly rather than caching it.
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

- `docs/notes/lean-mechanization-spike.md`: the spike's findings, per-§7
  assessment, and original milestone ladder (lands with RUE-2226).
- `docs/formal/README.md`: the three views and the extension rubric this
  ADR extends; `docs/formal/01-core-calculus.md` §7 and §9.
- RUE-305 (authority rule), RUE-50 (differential oracle), RUE-207
  (metatheory), RUE-1882 (this decision), Linear project "Formal core
  mechanization".
- ADR-0082 (a process ADR of the same shape), ADR-0083 (a phased project
  ADR whose review rounds were its ratification).
- AWS Cedar's verification-guided development (Lean model, Rust
  implementation, differential testing between them) as the external
  precedent for the bridge.
