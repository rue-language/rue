---
id: 0069
title: "CI work scheduling for a compiler monorepo"
status: accepted
tags: [process, ci, testing, build, performance]
feature-flag: null
created: 2026-08-09
accepted: 2026-08-09
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-1250", "RUE-1262", "RUE-1164", "RUE-1130", "RUE-1131", "RUE-1222", "RUE-1116", "RUE-1118", "RUE-1119", "RUE-1157", "RUE-1163"]
---

# ADR-0069: CI work scheduling for a compiler monorepo

## Status

Accepted. Phase 2 (lane-wide determination, RUE-1130) is implemented in the
same change that accepted this ADR; the remaining phases are unstarted.

## Summary

Rue's CI has three good mechanisms for not doing work — a change determinator
(RUE-1119), cacheable corpus actions (RUE-1118), and corpus sharding
(RUE-1116/RUE-1158) — each applied to a different hand-picked subset of the work
and composed in the wrong order. Measuring 120 runs — 55 classified by what they
changed, 21 timed to job and step level — shows the consequence: the critical
path is a single test function executed four times, and no topology change can
move it.

This ADR adopts a uniform model. Every unit of CI work is a Buck target carrying
its own tier and platform scope; no workflow file names a target; every lane is a
generated selection over the live graph.

It rests on two measurements that pull in opposite directions and are both true.
A compiler monorepo has a property the mechanisms were not designed for — **the
compiler is a universal dependency, so today three quarters of changes invalidate
everything downstream**, and neither selection nor input-keyed caching can help
them. Eliminating duplicated work therefore ranks above both, because it is the
only lever that pays in every regime. But the converse measurement is sharper:
**a two-file documentation change costs 444s against 465s for a compiler change —
a 4.5% difference — while roughly 80% of that run is provably unnecessary.**
Determination is not near its ceiling; it is not connected to the critical path
at all. Both facts must survive into the design, because the change mix that
makes the first one true today is deliberately being changed.

## Context

### What was measured

120 `CI` runs were collected (2026-08-06 to 2026-08-09). Of those, **55 were
classified by change class**, by resolving each run to its commit through the
merge-queue branch name and reading the changed-file list from local git; **21
have full job- and step-level timings**. `buck2` was also run directly against
the live graph for per-target measurement. Full evidence is in
`docs/notes/rue-1250-shard-topology-analysis.md`,
`rue-1250-premerge-critical-path.md`, and `rue-1250-ci-architecture.md`.

The finding that motivates this ADR is concentration, not imbalance:

- `premerge (linux-x64)` was the longest job in **18 of 21** timed runs (86%);
  `native (linux-arm64)` took over in the other three, all of them cheap runs.
  In the nine originally sampled it was 279–576s ahead of the slowest CLI shard.
- That lane runs 80 targets. Two are **81.4%** of it — and
  `//crates/rue-compiler:scaling-matrix-test` turns out to be a strict superset
  of `//crates/rue-compiler:rue-compiler-test`: 816 tests versus 813, all 813
  shared. The lane runs those 813 twice to gain three.
- Both native lanes are one step, and that step is **99.1%**
  `rue-compiler-test`, contradicting `docs/process/ci.md:46` ("Linux ARM64 and
  macOS ARM64 deliberately do not repeat the broad unit suite") while
  `scripts/validate-ci-gate.py:204` pins the repetition in place.
- Inside that target, **one test function is 181.7s** against a 5.7s runner-up.
  It is compiled into two targets and runs on three platforms: **four executions
  per CI run**, together 56–59% of the critical path (RUE-1262).

Sharding cannot address this. Partitioning those 813 tests four ways gives
3.4s / 6.8s / 7.2s / **227.0s**, and cost-balancing cannot do better when one
item weighs 181.7s against ~44s of everything else. LPT's makespan is bounded
below by the largest indivisible item, and the lane's measured parallel wall
(268s) is already within 19% of that bound.

### The change mix, and why it is the wrong thing to design against

The determinator and the action cache both key on change. In a compiler
monorepo that looks like a poor bet, because the compiler is an input to nearly
every downstream action: a change to any compiler crate changes the compiler
binary's digest, which invalidates every Rue-program compile action, hence every
corpus. Classifying 55 runs by what they touched:

| area touched | runs | share |
| --- | --- | --- |
| compiler internals | 41 | 74.5% |
| docs / website | 29 | 52.7% |
| CI / build inputs | 18 | 32.7% |
| test cases | 15 | 27.3% |
| tooling crates | 12 | 21.8% |
| performance manifests | 8 | 14.5% |
| **`std/`** | **0** | **0%** |

Collapsing that to what a determinator can act on: **74.5% touch compiler
internals** and cannot be narrowed; **18.2%** touch a graph-global CI or build
input and correctly force a full run; **7.3%** are peripheral and addressable
today. RUE-1130 records the corresponding symptom — zero deselections across
9 runs × 18 corpus jobs.

**It would be a mistake to read that as the mechanism's ceiling.** Two facts say
otherwise.

First, determination is not failing on the runs it *can* address; it is not
reaching them. Comparing timed runs by class:

| change class | runs | median wall |
| --- | --- | --- |
| touches compiler internals | 9 | 465s |
| peripheral (docs, tooling, performance) | 4 | **444s** |

A two-file documentation change costs 4.5% less than a compiler change. It still
builds the compiler, still runs 813 compiler unit tests on linux-x64, and still
runs those same 813 again on linux-arm64 and again on macOS. The correct run for
that change is the doc-consuming targets — `tutorial-snippet-tests` (11.8s),
`adr-registry-validation` (0.7s), `spec-traceability` — plus the lint and
aggregate gates: on the order of 80s including runner overhead, against 444s
actually paid. **Roughly 80% of a peripheral run is provably unnecessary, and the
determinator computes the right answer already; it is simply wired to 7 of ~14
jobs, and not to the one on the critical path in 86% of runs.**

Second, the 74.5% figure describes the repository's *current* shape, not a
property of the design. Rue is at a compiler-building stage, and the planned work
is explicitly elsewhere: an LSP, a test runner, code mods, and a substantial
standard-library expansion — components that carry their own tests and mostly do
not touch compiler internals. The `std/` row above is the tell: **zero of 55
sampled changes touched `std/` at all.** That entire class of work, along with
the other three components, is ahead of this measurement rather than behind it.
A design that treated 7.3% as the ceiling would be tuned for a repository shape
that is deliberately being replaced.

So the honest statement is narrower than "determination has a low ceiling in a
compiler monorepo." It is: *determination's addressable share is a function of
the change mix; the mix is currently compiler-heavy; the mechanism is
nevertheless leaving its entire current share on the table, and the share is
designed to grow.* Keep it, wire it to everything, and make it report what it
saved so the question is answered by data next time rather than by argument.

Classifying the same runs by cache state gives the complementary picture:

| regime | runs | compiler build | corpus |
| --- | --- | --- | --- |
| warm | 5 of 9 | ≤22s | cached |
| corpus-cold | 1 of 9 | 19s | 900–1100s |
| compiler-cold | 3 of 9 | 286–317s | 900–1100s |

Two consequences follow, and they are the reason this ADR exists rather than a
narrower one:

1. **Sharding is load-bearing and is not made obsolete by caching.** RUE-1250
   asked whether caching had retired the CLI shards. It has not: in the four runs
   where the corpus executed, a single unsharded corpus projects to 900–1100s,
   against an unshardable native ARM64 lane at 341–407s. Four shards stay under
   that floor in 4 of 4 runs; two breach it in 4 of 4. **Keep four.**
2. **Not doing duplicated work outranks both other mechanisms**, because it is
   the only one that pays in every regime — while determination pays hugely in a
   regime that is currently small and growing, and caching pays in a regime that
   is currently large and shrinking as the compiler stabilizes.

### Why the current composition is inverted

| mechanism | eliminates | applied to | not applied to |
| --- | --- | --- | --- |
| determinator (RUE-1119) | work the diff cannot affect | `platform-corpus`, `pull_request` only | premerge, both native lanes, valgrind, asan, release, reproducibility |
| corpus action cache (RUE-1118) | work already done for this tree | 11 corpora | 75 of 80 premerge targets, every native target |
| sharding (RUE-1116/1158) | wall time of work that must run | 1 corpus, fixed at 4 | everything else |

Sharding is permanent and outermost, caching covers a middle slice, and
determination is innermost and narrowest — the reverse of their cost order.

The structural cause is that "unit of work" is not one thing. There are three:
Buck actions (all mechanisms apply), Buck test targets (only determination
applies, since OSS buck2 as configured has no test-result cache), and
**hand-written target lists inside YAML steps** (nothing applies). Every defect
found lives in the last two categories. The native lanes' eight-target shell
invocation is the clearest case: invisible to the determinator, uncacheable,
unshardable — and where a 181.7s test hid on two platforms for weeks.

## Decision

### 1. One unit of work, and no workflow file names a target

Every piece of pre-merge work is a Buck target carrying its execution tier
(RUE-1157) *and* its platform scope as attributes. Lanes are generated
selections over the live graph. `.github/workflows/*.yml` contains no target
label.

The repository already proves this works for corpus cases: `only_on` plus
`RUE_PLATFORM_CASE_SELECTION=native` makes platform-scoped cases self-enrolling,
and `docs/process/ci.md` correctly cites that as the reason new cases need no
workflow edit. This decision extends the same idea to unit targets, so the native
lanes become a label selection rather than a list `validate-ci-gate.py` must pin.

It also fixes a granularity error the measurement exposed: **platform scope is
declared per target but is a property of individual tests.** `rue-compiler-test`
contains ~27 host-conditional sites and 813 tests; the native lanes want the
former and pay for the latter. Host-conditional compiler tests move into their
own target, following the precedent already set in the same crate by
`rue-compiler-public-api-test`, `rue-compiler-differential-oracle-test`, and
`rue-compiler-payload-schema-test`.

### 2. Nothing executes twice

No unit of work executes more than once per platform per run without a declared
reason.

This is stated first among the behavioural rules because it is the only lever
that pays in every regime. Three violations exist today and **no current gate can
see any of them**, because every gate compares *target lists* while these are
overlaps in *test contents*:

- `scaling-matrix-test` re-runs 813 of `rue-compiler-test`'s tests, in the same
  lane, to add three;
- `rue-compiler-test` runs on linux-x64, linux-arm64, and macOS, against a
  documented contract that says the native lanes do not repeat the broad unit
  suite;
- `release-smoke` runs in the premerge lane under the debug platform and again in
  the `release` job under the release platform — defensible, but undecided.

A repository gate collects each lane's test identities (`--list` on the test
binaries — cheap, exact, and how the superset above was found) and fails on any
test scheduled twice without an explicit allowance. That gate is what converts
"we fixed this instance" into "this class cannot recur."

### 3. Determinate every lane, and say which regime the run is in

Extend `affected-targets` from gating one job to planning every lane, and have it
emit the lane plan the matrix consumes
(`strategy.matrix: ${{ fromJSON(...) }}`). `merge_group` keeps forcing the
authoritative full run. The existing fail-open contract and
`scripts/test-affected-targets.sh` pinning are retained unchanged.

This is the decision RUE-1130 asks for, and the answer is *keep and extend*, not
*remove*. The measured saving on a peripheral run is ~80% of its wall time, and
none of it is currently reachable, because the determinator gates 7 of ~14 jobs
and `premerge` — the critical path in 86% of runs — is not one of them. Extending
the gate is a smaller change than the rest of this ADR and does not depend on it:
the existing `steps.sel.outputs.run` pattern already used by `platform-corpus`
generalizes to the other lanes directly, and BTD already computes the closure
these lanes would consult. Docs and specification sources are ordinary in-graph
inputs (`tutorial-snippet-tests`, `adr-registry-validation`, `spec-traceability`
declare them), so a documentation change narrows to exactly those targets without
any new force-full rule.

The same applies to the native lanes. A documentation change currently runs 813
compiler unit tests on linux-arm64 and again on macOS; nothing about that change
can reach them.

The coverage gate then becomes stronger *and* simpler than
`validate-cli-shard-coverage.py`: **the union of planned lanes equals the tier
selection from the live Buck graph**, failing closed on any target that is in the
graph and in no lane. That subsumes the hand-written-matrix drift problem instead
of policing it.

Crucially, the planner **names the regime and the binding constraint on every
run**. RUE-1130 had to discover the zero-deselection result empirically, and
RUE-1250 asked about CLI shards while premerge was the actual critical path —
both because nothing in CI reports what it is bound by. A determinator that
reports "compiler-cold: selection saved nothing, the critical path is X" is worth
more than one that silently selects everything.

### 4. Cache with real input keys, not success stamps

RUE-1118's `cached_corpus_suite` proved the mechanism and cut merge-group runs
from ~12.5 to ~5 minutes. It is not the destination. RUE-1164 already states the
constraint this ADR adopts: **do not use an aggregate success-stamp action as the
destination — every source, import, compiler flag, target architecture, runtime
input, and expected output must contribute to the action key.** The stamp is
evidence, not design authority.

Two things make extending caching safe rather than hopeful:

- **Remote execution is the undeclared-input detector.** RUE-1222's item 3 warns
  that an undeclared action input becomes a silent false pass rather than an
  untracked re-run. RE materializes only declared inputs, so an undeclared read
  fails with file-not-found instead. RUE-320 landed on 2026-07-18 ("full remote
  execution now default-capable"); `AGENTS.md:182` and the `./buck2` wrapper
  still describe it as open and should be reconciled with
  `docs/process/build-cache.md`, which already describes it as supported.
- **Check buck2's native path before hand-rolling more.** buck2 carries
  `supports_test_execution_caching` on `ExternalRunnerTestInfo` and
  `disable_test_execution_caching` in the executor protocol. Rue configures
  `noop_test_toolchain` and `noop_remote_test_execution_toolchain`, so nothing
  honors them — `corpus.bzl`'s "OSS buck2 ships no test-result cache" is true *as
  configured*, not in general.

### 5. Pack to a measured floor, and alarm on indivisible items

The packer's objective is **minimize runners subject to the slowest lane staying
under the floor** — bin-packing under a capacity constraint, not makespan across
fixed bins. Only that objective can ever conclude "use fewer." The floor is
measured (the slowest lane that cannot be split), never chosen.

The load-bearing part is not the algorithm. **When the largest indivisible item
exceeds the floor, the planner fails loudly, names the item, and refuses to
express the problem as a lane count.** A packer reasoning about totals would have
requested eight runners for the pre-fix premerge lane — where one item was
225.6s — and reported success having changed nothing.

Its three remedies are all ones this repository already has:

| symptom | remedy | precedent |
| --- | --- | --- |
| one item dominates a lane | split it | CLI intra-target shards (RUE-1116) |
| the item re-executes every run | cache it | cacheable actions (RUE-1118, RUE-1164) |
| the item is heavier than pre-merge warrants | re-tier it | scaling matrix 100/1k vs 10k; large-example canary vs slow |

Choosing among them is a human call. Noticing is not.

The shard count follows from the same rule and, applied today, confirms the
existing constant rather than contradicting it: `ceil(total ÷ floor)` gives 3,
but measured shard skew is 8–25% and 366s × 1.25 breaches the 407s floor, so with
a skew allowance it lands on **4**. The count stays; what changes is that the
reason is written down and recomputed.

### 6. Balance guards measure outcomes

`CliShardPlan::validate_skew` rejects an estimated slowest shard >25% above the
mean. LPT over the checked-in weights produces **0.00%** estimated skew at every
count from 1 to 12, and the LPT bound proves the worst possible estimate at N=4
is 3.90% — the guard cannot fail on real data. Observed skew reached **24.9%**.

Guards compare observed lane wall time against plan, and stale weights surface as
skew. A guard that validates the model against itself is not a guard.

## Implementation Phases

Ordered by measured value, with the packer deliberately last: until the earlier
phases land it would be optimizing a distribution that is about to change
completely.

- [ ] **Phase 1: Eliminate the duplicated critical path** — RUE-1262.
      56–59% of the critical path, no new machinery, needs one coverage ruling.
- [x] **Phase 2: Extend determination to every gateable lane** — RUE-1130,
      whose "make it discriminate or remove it" question this ADR answers with
      "keep it and extend it." Six lanes now consult the determinator
      (`native` ×2, `release`, `valgrind`, `asan`, `compiler-reproducibility`),
      freeing **905–1034s of runner time** on each of four measured peripheral
      runs. It moves the critical path in only one of those four: `premerge`
      still dominates the rest, which is the measured argument for doing Phase 1
      next rather than more gating. `linux-premerge` stays ungated on purpose —
      see "Decision §3". Reporting the saved share per run is still to do.
- [ ] **Phase 3: Duplication gate** — new. Cheap, and it is what stops the class
      from recurring; lands while the evidence is fresh.
- [ ] **Phase 4: Platform scope as a target attribute** — new (RUE-1262 scope C).
      Removes the `validate-ci-gate.py:204` pin and the per-test/per-target
      granularity error. Phase 2 already stops peripheral changes from reaching
      the native lanes; this makes compiler changes stop over-selecting them too.
- [ ] **Phase 5: Real input-keyed compile actions** — RUE-1164, milestone
      "Buck-native test actions"; RUE-1222 for the timing-refresh and
      undeclared-input follow-ups.
- [ ] **Phase 6: Floor-aware packer with the indivisible-item alarm** — new.
      Subsumes `validate-cli-shard-coverage.py`.

Adjacent, not sequenced: RUE-1131 (avoid compiler builds in stubbed jobs) is the
same fixed-cost problem this ADR names in the compiler-cold regime.

## Consequences

### Positive

- Measured 56–59% critical-path reduction from Phase 1 alone: `pull_request`
  median 820s → 361s, `merge_group` 456s → 185s, with the whole-sample range
  narrowing from 341–873s to 170–393s. The merge queue becomes predictable, not
  merely faster.
- Phase 2 takes a peripheral change from a measured 444s to roughly 80s, and its
  share of runs grows with every component added outside the compiler. The two
  phases are complementary rather than competing: Phase 1 shrinks the work every
  change must do, Phase 2 stops changes doing work they cannot affect.
- The binding constraint stops being one defect and becomes distributed —
  `valgrind` in 4 of 9 runs, the cold compiler build in 3, reproducibility in 1,
  one CLI shard in 1. The next improvement becomes a real trade rather than a bug.
- New work inherits all three mechanisms by construction. A new toolchain aspect
  becomes a tier-labelled target and is determinated, cached, and packed without
  a workflow edit — the scaling property RUE-1250 asked for.
- Two hand-maintained gates disappear: the shard-count/matrix agreement check and
  the pinned native target list, each replaced by a graph-derived assertion.

### Negative

- A generated matrix is harder to read than YAML. Mitigation: the plan is written
  to the job summary, as `affected-targets` already does for corpus selection.
- The planner becomes a new failure mode on the critical path. It must be
  fail-open by construction, as `scripts/affected-targets` already is, and pinned
  by its own tests.
- The duplication gate costs a `--list` invocation per test binary per run.
  Bounded and small, but it is new required work.
- "Regime" is a new concept for contributors to learn. It earns its place only if
  CI reports it plainly.
- Phase 5 widens the class of false passes an undeclared input can cause, from
  corpora to unit tests. This is why RE-based hermeticity validation gates it
  rather than following it.

## Open Questions

- Does buck2's native `supports_test_execution_caching` work under a real remote
  test-execution toolchain? If so it obsoletes most of Phase 5's per-target
  conversion work, and should be established before that phase starts.
- `compiler reproducibility` measured 164–183s in every regime — it is cache-free
  by design (RUE-617/RUE-1019) and becomes a binding constraint once Phase 1
  lands. Is its cost reducible without weakening the proof?
- The compiler build is 286–317s whenever a compiler crate changes, and every
  lane pays it. Build-once-and-fan-out serializes it ahead of the lanes and
  measures worse; is there a third option (RUE-1131 is adjacent)?
- Should `release-smoke`'s debug/release double execution be kept? It is
  defensible coverage that nobody appears to have decided.

## Future Work

- **Finer-grained compiler invalidation.** The blind spot in "Context" exists
  because CI keys on the compiler *binary*. Rue's own query engine (ADR-0063)
  maintains fine-grained fingerprints internally while CI invalidates at
  whole-binary granularity externally. Exposing query-level fingerprints to the
  build system would make invalidation proportional to the change rather than
  total. This is speculative and out of scope here, but it is the only idea in
  sight that would raise the determinator's ceiling in the regime that matters.
- Reliability. This ADR classifies 55 runs and times 21, but computes no flake
  rate; `correctness-repetitions.yml` is the right source, and shard reliability
  remains unquantified.
- The change-mix measurement is four days of one repository at one stage. It is
  the right input for Phase 2's priority and the wrong input for a permanent
  constant, which is the general argument this ADR makes about derived rather
  than pinned numbers. The determinator should report its own saved share so the
  mix is observed continuously rather than sampled once.
- Per-target measurement on real ARM64 and macOS runners. The native-lane
  conclusions here scale CI step timings by locally measured target ratios; the
  99.1% concentration is inferred from target composition, not measured on those
  hosts.

## References

- `docs/notes/rue-1250-shard-topology-analysis.md` — shard decision and run sample
- `docs/notes/rue-1250-premerge-critical-path.md` — premerge lane profile
- `docs/notes/rue-1250-ci-architecture.md` — native lane profile and design detail
- `docs/process/ci.md`, `docs/process/build-cache.md` — current contracts
- ADR-0015 (test suite optimization), ADR-0063 (parallel demand-driven
  incremental compilation), ADR-0067 (compiler performance measurement)
- RUE-1250, RUE-1262, RUE-1164, RUE-1130, RUE-1131, RUE-1222; RUE-1116, RUE-1118,
  RUE-1119, RUE-1157, RUE-1163 for the mechanisms this ADR unifies
