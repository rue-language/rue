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
relates: ["RUE-1250", "RUE-1262", "RUE-1265", "RUE-1164", "RUE-1130", "RUE-1131", "RUE-1222", "RUE-1116", "RUE-1118", "RUE-1119", "RUE-1157", "RUE-1163", "RUE-1505"]
---

# ADR-0069: CI work scheduling for a compiler monorepo

## Status

Accepted. Phase 2 (determination on every lane, RUE-1130) is implemented in
the same change that accepted this ADR. Phase 1 (RUE-1262) and Phase 3
(the duplication gate, RUE-1265) have since landed; Phases 4-6 are unstarted.

**Amendment 1 (RUE-1505) is a proposal and is not accepted.** It records the
remote-execution evaluation this ADR's open questions invite, and recommends
declining RE as a scheduling lever. Everything above and below it remains the
accepted text; nothing in the amendment is implemented. See
[Amendment 1: remote execution is not the "more cores" lever (RUE-1505)](#amendment-1-2026-08-14-remote-execution-is-not-the-more-cores-lever-rue-1505).

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
  `scripts/validate-ci-gate.py:204` pins the repetition in place. RUE-1265
  resolved the contradiction in the only direction available without a coverage
  ruling: the contract now describes what runs, and names Phase 4 as the work
  that makes the original claim true again.
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

**Landed as `scripts/validate-test-duplication.py` (RUE-1265).** Against the
graph on 2026-08-14 the three violations stand as: the scaling-matrix superset
is **gone** — RUE-1262 rebuilt that target as an `sh_test` selecting three
`#[ignore]`d rows out of the one binary, and the gate confirms the two targets
are now disjoint; the other two are **declared allowances with written
reasons**, marked provisional because both need a ruling this gate is not
positioned to make. Measured cost with the binaries already built: 0.29s wall
for 73 `--list` invocations.

Two things the implementation established that this section did not anticipate.
**`--list` is not a universal protocol**, and probing for it is not free: two
corpus harnesses are shell scripts that ignore their arguments and run their
whole suite, so a gate that asks them to list executes two premerge suites a
second time — the failure this very section names, caused by its own enforcement.
Whether a target can be listed is therefore settled from the graph before
anything runs. And **the largest overlap in the repository is one this gate
cannot score**: the oracle differentials re-execute the entire CLI and
specification corpora against the reference interpreter, several thousand cases,
in the same run on the same platform. That is defensible as a differential and
is far larger than the `release-smoke` case §2 does name; it is recorded as
provisional in `NOT_LISTABLE` rather than quietly omitted.

### 3. Determinate every lane, and say which regime the run is in

Extend `affected-targets` from gating one job to planning every lane, and have it
emit the lane plan the matrix consumes
(`strategy.matrix: ${{ fromJSON(...) }}`). `merge_group` keeps forcing the
authoritative full run. The existing fail-open contract and
`scripts/test-affected-targets.sh` pinning are retained unchanged.

This is the decision RUE-1130 asks for, and the answer is *keep and extend to
everything*, not *remove*.

Gating decides **whether** a lane runs; narrowing decides **what** it runs. Both
are the same determinator output applied at different granularity, and a lane
takes whichever fits it. A lane with a fixed target list is gated, then narrowed
to the impacted subset of that list. `linux-premerge` is never gated — skipping
the broad-discovery lane on a representative subset is the RUE-924 failure mode
— but it *is* narrowed, running `impacted ∩ tier` in place of `//... ∩ tier`.
Membership still comes from the live graph, so a target added since any list was
written is still discovered; it simply is not built or run when the diff cannot
reach it.

Narrowing matters even where a lane is not the critical path. Building a test
binary is most of what a unit target costs, and the premerge lane spends
286–317s building every crate whenever a compiler crate changes. An unimpacted
crate cannot have been broken by the diff, so that build is waste — and waste
that grows with the repository rather than with the change, which is the whole
reason to fix it structurally now rather than when it hurts. The measured saving on a peripheral run is ~80% of its wall time, and
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

## The standard this is held to

CI is a platform, and the test of a platform is what it demands of the people
using it. The standard is therefore not "CI is fast" but: **the repository can
grow — new crates, new corpora, an LSP, a test runner, code mods, a much larger
`std/` — without anyone editing CI to keep it correct or fast.** Every hand-
maintained list is a future outage or a future slowdown with a delay fuse,
because it is correct on the day it is written and nobody is reminded when it
stops being.

Rue is some way from that, and the honest way to close the distance is to
enumerate what still needs a human. Anything on this ledger is either derived
from the graph later, or gated so that drift fails closed rather than silently:

| Hand-maintained today | What goes wrong when the repo grows | Status |
| --- | --- | --- |
| `CLI_TEST_SHARD_COUNT` + the `platform-corpus` matrix | count and matrix drift | gated (`//:cli-shard-coverage-validation`); derive in phase 6 |
| `SELECTABLE_CORPUS` | a new corpus job is ungated, so it always runs | fails open (runs); not yet gated |
| `SELECTABLE_LANES` / `lane_targets` | a lane's job runs a target selection cannot see | **gated** (`lane_target_drift`) |
| the native lanes' eight-target list, in the job and in the contract | new platform-sensitive tests are not enrolled | partly gated; phase 4 removes the list |
| `RUE_AFFECTED_NARROW_LIMIT` (600) | a threshold nobody revisits | unmeasured; should follow measurement |
| the platform responsibility matrix | a responsibility silently moves | gated (`validate-ci-gate.py`) |
| `shard-weights.json` refresh | weights go stale, shards skew | manual (RUE-1222); guard is vacuous (§6) |
| the duplication gate's `ALLOWANCES` ledger | a duplication outlives its reason | **self-gating** (RUE-1265): an entry matching nothing fails |

The pattern the ledger is meant to enforce: a list that must exist is paired
with a gate that fails closed, and a list that need not exist is deleted in
favour of a query against the live graph. Phases 1 and 4 delete rows; phases 3
and 6 gate the rest. **A phase that adds a row without a gate has not met the
standard**, which is the test this ADR's own implementation failed once already:
RUE-1130 introduced `lane_targets` as a second copy of the native lane's target
list before that row was gated.

## Implementation Phases

Ordered by measured value, with the packer deliberately last: until the earlier
phases land it would be optimizing a distribution that is about to change
completely.

- [x] **Phase 1: Eliminate the duplicated critical path** — RUE-1262.
      56–59% of the critical path, no new machinery, needs one coverage ruling.
- [x] **Phase 2: Determination on every lane, by gating or by narrowing** —
      RUE-1130, whose "make it discriminate or remove it" question this ADR
      answers with "keep it and extend it to everything." Six lanes are gated
      (`native` ×2, `release`, `valgrind`, `asan`, `compiler-reproducibility`),
      freeing **905–1034s of runner time** on each of four measured peripheral
      runs. `linux-premerge` and the native unit targets are narrowed to the
      impacted closure instead, so an unimpacted test binary is neither built
      nor run. Reporting the saved share per run is still to do, and is what
      turns the change-mix question from an argument into a measurement.
- [x] **Phase 3: Duplication gate** — RUE-1265.
      `scripts/validate-test-duplication.py` enumerates each lane from the live
      graph, collects identities with `--list`, and fails on any test scheduled
      more than once per platform per run without a declared allowance. It runs
      as a step in the premerge lane, where the binaries it lists are already
      built, and costs 0.29s wall. `docs/process/ci.md` now states the invariant
      — with the scope it actually checks, and a section naming where it cannot
      see — and no longer claims the native lanes skip the broad unit suite;
      that repetition is a declared, provisional allowance until Phase 4
      removes it.
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
  defensible coverage that nobody appears to have decided. RUE-1265's gate now
  measures it — 25 differential-opt cases the CLI shards also run — and carries
  it as a provisional allowance rather than settling it.

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

## Amendment 1 (2026-08-14): remote execution is not the "more cores" lever (RUE-1505)

**Status: proposal. Not accepted, not implemented.** This amendment answers the
question RUE-1505 asks — should ordinary CI builds run on BuildBuddy remote
execution rather than only consuming its cache — and recommends *no*. It needs a
maintainer ruling before it means anything.

### Recommendation

**Keep remote execution scoped exactly where it is**: the merge-group canary in
`.github/workflows/ci.yml`, which also serves as ADR-0070's negative control 2.
Do not route `premerge`, the other linux-x64 build jobs, or any required lane
through `//platforms:remote_execution`. The measurements below say RE is
modestly faster on the graph the canary builds, materially less predictable, and
— decisively — aimed at the wrong 1% of the problem.

RUE-1505 framed the choice as "more cores, less work, or more overlap", with RE
as the more-cores lever. The finding is that **the work that is cold is not the
work that is wide**, so more cores has almost nothing to compress.

### How this was measured, and what could not be

Every number here comes from GitHub Actions job logs and the Actions API for
`rue-language/rue`, over 500 `CI` runs spanning 2026-08-10T13:39Z to
2026-08-14T15:32Z (4.08 days). Nothing was measured on a local host, so local
contention from concurrent work is not a confound for any figure reported.

The evaluation deliberately did not run the experiments as RUE-1505 drafted
them, because it could not: there is no BuildBuddy credential on this machine.
The key exists only as the `BUILDBUDDY_API_KEY` repository secret, and GitHub
does not return secret values through its API, so local `--prefer-remote` runs,
the BuildBuddy invocation UI, and the account dashboard were all unreachable.
Publishing a temporary workflow to measure the full premerge closure under RE
would have required pushing, which was out of scope.

What replaced them is better than a laptop experiment would have been, because
required CI already runs the controlled pair on every merge:

| lane | what it builds | executor | cache |
| --- | --- | --- | --- |
| `remote execution (linux-x64)` | `//crates/rue:rue` | `--prefer-remote`, `//platforms:remote_execution` | `--no-remote-cache` |
| `compiler reproducibility (linux-x64)` | `//crates/rue:rue`, twice | `--local-only` | `--no-remote-cache` |

Same commit, same run, same `ubuntu-latest` runner class, started within
seconds of each other, both genuinely cold, and — confirmed from the logs —
**the same 396–405 actions**. That is the paired cold-build experiment, already
running 137 times in the sample.

The gap that remains, and it is the important one: **no measurement exists of
the full premerge closure under RE.** Every claim below about premerge under RE
is inference from the closure's measured composition, not observation, and is
marked as such.

### 1. Cold-build speedup: real, modest, and much less predictable

Distribution over the paired runs, buck2 daemon start to `BUILD SUCCEEDED`:

| build | n | p25 | p50 | p75 | p90 | max | sd | cv |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| remote | 60 | 44.9s | **48.7s** | 55.9s | 76.3s | 170.7s | 25.3 | **44%** |
| local, full parallelism | 105 | 72.5s | **74.4s** | 76.9s | 79.7s | 83.4s | 5.8 | **8%** |
| local, `--num-threads 2` | 105 | 82.4s | 84.1s | 87.2s | 91.0s | 97.4s | 6.5 | 8% |

Paired per-run ratio (local ÷ remote, n=60): min **0.5×**, p25 1.3×, p50
**1.5×**, p75 1.7×, p90 1.8×, max 2.0×. RE wins the median and *loses* the tail:
in the worst paired run it was twice as slow as the local build it replaces.

One confound, stated rather than smoothed: the local build downloads the pinned
rustc and Zig `http_archive` payloads to the runner (183–236 MiB; buck2 is still
waiting on those actions 7.3s into the build at p50, 21.4s at p90). The remote
build never does, because those actions execute on the worker. Net of that, the
median advantage is nearer 1.4× than 1.5×. It is a real CI cost either way, but
it is a runner-provisioning cost, not evidence about execution parallelism.

The variance asymmetry is the durable result: **RE's coefficient of variation is
five times local's**, and its worst case is 3.5× its median while local's is
1.1×.

### 2. Where the time goes: not upload, not download — contention

Per remote build, buck2's own network accounting (n=60): **Up p50 3.7 MiB, Down
p50 3.4 MiB**, maxima 17 MiB each. RE session establishment is 1.3–1.9s; roughly
8s passes in loading and analysis before the first action dispatches.

So the "a 2× on execution eaten by upload overhead" failure mode does not occur
here, and the reason is structural: the toolchain payload is already resident in
the CAS, so `FindMissingBlobs` keeps steady-state upload to single-digit MiB.
Input transfer is not the constraint and would not become one.

The tail is contention on the shared free-tier pool. The five slowest remote
builds in the sample (99.4s, 106.5s, 128.0s, 152.8s, 170.7s) all fall inside
2026-08-13 18:08–22:00Z, and their network figures are unremarkable (1.4–11 MiB).
Tracing the slowest one shows no stall: every dependency stage is uniformly
stretched, with a single `rue-perf-schema` rlib occupying 21s. Individual remote
actions simply run slower when the pool is busy. That matters more than it would
for a wide graph, because the graph this repository builds is latency-bound.

### 3. Why RE cannot pay: 72% of the cold build is two indivisible actions

This is the finding that decides the question. Over the 20 cold-compiler
`pull_request` runs in the sample, `premerge`'s `Build all targets` step ran
p25 275s / p50 294s / p90 322s, issuing 849–879 commands of which ~91% were
cache-served and only **76 executed locally**. Attributing the step's wall time
to the target buck2 reported waiting on:

| target | mean | share |
| --- | ---: | ---: |
| `//crates/rue-oracle-diff:oracle-diff-test-action` | 126.4s | **44.5%** |
| `//crates/rue-oracle-diff:oracle-diff-spec-test-action` | 78.4s | **27.6%** |
| `//crates/rue-compiler:rue-compiler-test` | 19.4s | 6.8% |
| `//crates/rue-compiler:rue-compiler` | 13.1s | 4.6% |
| `//crates/rue-air:rue-air-test` | 12.5s | 4.4% |
| `//crates/rue-air:rue-air` | 6.9s | 2.4% |
| `//crates/rue-air:rue-air-fuzz-support` | 5.7s | 2.0% |
| every third-party crate, together | ~2s | ~1% |

**Two single actions are 72.1% of the cold premerge build.** They are one action
each — `cached_corpus_suite` genrules (RUE-1118) that run a whole harness — so
remote execution cannot subdivide them. It can only help if a BuildBuddy worker
runs one action faster than the runner does, and §2's contention evidence says
that is at best unreliable and at worst inverted.

Meanwhile the part of the graph that RE demonstrably *does* accelerate — the
wide fan of third-party rlibs, which is most of the canary's 405 actions and
most of its 1.5× — is already 100% cache-served on a real PR and worth about 1%
of the step. **The canary's speedup is not transferable to premerge**, because
the canary measures a cold graph that premerge never has.

This is exactly the situation §5 of this ADR describes and demands be named
rather than expressed as a lane count. The two oracle-diff actions are the
largest indivisible items in required CI, and §5's remedies apply to them:
splitting them, or re-tiering them. Caching, the third remedy, is already done —
it is why the merge queue sees this step at 23s.

### 4. Concurrency: the feared duplication is not happening

RUE-1505 predicted that simultaneously started jobs cannot warm each other's
cache, so any split would make each half pay the same cold build. Measured over
the 23 cold-compiler `pull_request` runs, per run:

| job | commands | cache hit | locally executed | job wall |
| --- | ---: | ---: | ---: | ---: |
| `premerge (linux-x64)` | 879 | 91% | 76 | 530s |
| `native (linux-arm64)` | 462 | 96% | 19 | 117s |
| `release (linux-x64)` | 455 | 97% | 16 | 170s |
| `valgrind (linux-x64)` | 396 | 99% | 3 | 111s |
| `compiler reproducibility (linux-x64)` | 396 | **0%** | **396** | 171s |
| `asan (linux-x64)` | 0 | — | 0 | 8s |

510 locally executed actions per run in total, 18.5 runner-minutes; a peripheral
change costs 304 and 9.7 runner-minutes. The premise is false in practice:
simultaneous start is not a problem because **what is cold is small and mostly
job-specific**, and the bulk of every job's graph was populated by trunk's
merge-group runs rather than by its siblings. A second concurrent job pays 3–19
actions today, not a second cold build.

The one genuine duplicate cold compiler build per run is `compiler
reproducibility`, and it is deliberate. RE would not change any of this: it
would relocate the same small number of cold actions, not remove them.

### 5. Reliability: RE is not the flaky part

The canary succeeded in 135 of 137 merge-group runs (**1.5% failure**), against
1.5% for `premerge` on merge_group and 6.8% on `pull_request`. Both failures
were the same thing, and neither involved BuildBuddy: `dotslash` could not
download buck2 from the GitHub releases CDN, before the RE endpoint was
contacted. **Zero BuildBuddy-attributable failures in 137 runs.**

That is a favourable result for the canary and a misleading one for adoption,
because the canary is not exposed to the failure modes a required lane would be:

- `//platforms:remote_execution` sets `allow_hybrid_fallbacks_on_failure =
  False`. A failed remote action is never retried locally. That is correct for a
  canary whose entire purpose is to refuse to hide a worker regression, and it
  is an outage amplifier on a required lane: BuildBuddy unavailability becomes
  merge-queue unavailability.
- The forgiving `//platforms:remote_cache` platform allows fallback *on action
  failure*, which is not the same as tolerating an unreachable or slow endpoint.
- **Fork PRs have no secret, hence no `.buckconfig.local`, hence no RE endpoint
  or credential at all.** Today `scripts/provision-build-cache` treats an empty
  key as "skip" and the lane builds cold and locally — the graceful degradation
  RUE-1505 requires any replacement to match. An RE-by-default premerge needs a
  second, conditional execution path, and that path would be the least exercised
  and most depended upon code in required CI.

### 6. Cost and the free tier: the honest answer is that this is already unfunded

Sources, fetched 2026-08-14 from BuildBuddy's published pricing page. The
Personal (free) plan, "For small teams and open source projects", states
**"100 GB of cache transfer"**, **"Up to 80 cores for remote builds"**, and
community support. Team states "Up to 800 cores" and "$X / GB of cache transfer
over 100 GB". Their FAQ states: *"We don't apply hard limits that prevent you
from using more than your plan allows. If you have a big temporary burst of
usage, feel free"*, with sustained overage prompting an upgrade conversation.

**That answers "what happens when a cap is hit": nothing technical.** Not
throttling, not hard failure, not a silent stop-caching. It is a commercial
conversation. The correctness-adjacent surprise RUE-1505 was most worried about
is not the exposure here.

Explicitly unverified, and it must not be presented otherwise:

- **The period the 100 GB is measured over is not published.** Per month is the
  natural reading; it is not stated.
- **The Team per-GB overage rate is not published** — the page renders it as
  "$X / GB". Any paid step-up needs a quote before it can be costed.
- **Current account usage against any cap could not be read.** No credential, no
  dashboard, no API. Every usage figure below is buck2's *client-side*
  accounting, not BuildBuddy's billing.

Measured volume: 500 CI runs in 4.08 days = **122.6 runs/day** (70.6
`pull_request`, 52.0 `merge_group`) — higher than the ~50/day RUE-1505 assumed.
Client-side network per complete run, summing every buck2 invocation in every
job (n=8 of each): `merge_group` mean 1.6 GiB, `pull_request` mean 2.6 GiB, with
cold-compiler PR runs near 3.8 GiB. Upload is unambiguously CAS traffic and is
small (3–135 MiB/run); download conflates CAS with the toolchain `http_archive`
fetches, so it is an upper bound.

Even on a deliberately conservative reading — call it 1 GiB of genuine CAS
transfer per run — that is ~120 GiB/day, **~3.7 TB/month against a published
allowance of 100 GB**. The cache alone is one to two orders of magnitude over
the free tier *today*, before any RE change, and nothing has broken. That is
entirely consistent with the vendor's stated no-hard-limits policy.

The conclusion is therefore not "RE would push us over the free tier". It is
sharper and less comfortable: **the repository is already relying on BuildBuddy's
goodwill well beyond the published allowance, and has been for some time.** RE's
own transfer is negligible (~7 MiB/build) and would barely move that number;
what would rise is core-hours against the 80-core figure — and §2 already shows
the shared pool visibly contended at today's essentially zero RE usage.

So the free-tier finding cuts against adoption without being the primary reason
for it. Two things follow that are worth a maintainer's attention independent of
this decision:

1. Someone with dashboard access should check real consumption against real
   limits. This evaluation could not, and the gap between 100 GB and ~3.7 TB is
   too large to leave as an inference from client-side counters.
2. If Rue wants a durable claim on this capacity, the conversation to have with
   BuildBuddy is about the *cache* it already depends on, not about RE.

Costed option, presented and not decided: Team tier buys "Up to 800 cores" and
metered transfer at an unpublished rate. It is the only listed step-up short of
Enterprise. Nothing in this evaluation argues it is worth buying for latency.

### Scope questions RUE-1505 asked

**Which jobs should default to RE?** None. `premerge` gains an inferred ~80–100s
at p50 on cold compiler PRs at best — applying §1's measured 1.5× to the ~28% of
the step RE could touch gives far less, and the honest projection is under 30s —
while importing a shared-pool tail and a hard external dependency. Every other
linux-x64 build job executes 3–19 cold actions per run and has nothing to gain.

**Does `compiler reproducibility` stay cache-free and locally executed?** Yes,
unchanged, and RE would actively weaken it. The proof works by making the two
builds differ — relocated root, `--num-threads 2`, different `TMPDIR`, `TZ`,
`umask`, and mtimes — so that a byte-identical result indicts path, scheduling
and environment leaks. A uniform remote container makes the two builds *more*
alike, so a pass would prove less. `check-reproducible-compiler.sh` also
hard-errors on a `.buckconfig.local` for precisely this reason. No change.

**Does RE change ADR-0070's hermeticity story?** Not materially, and the effect
is the opposite of the one RUE-1505 anticipated. Negative control 2 needs
exactly one clean-root remote materialization to prove that undeclared inputs
are simply unavailable, and it has one. Running everything remotely would catch
undeclared inputs in more places — a real if small gain — but it would convert
each latent instance into a required-lane failure with no local reproduction,
which is the cost profile ADR-0070 chose a single canary to avoid. Broader
adoption strengthens the *argument for keeping the canary*, not the case for
generalizing it. `rue-program-digests` already covers the complementary
direction.

**Does this interact with RUE-1407?** No, and the reasoning RUE-1505 offers is
correct. Keeping remote test-result caching off was a trust decision about
*verdicts*; RE changes where build *actions* execute. The two do not touch. Rue
additionally configures `noop_test_toolchain` and
`noop_remote_test_execution_toolchain`, so nothing honours
`supports_test_execution_caching` whatever the execution platform is. Adopting
or declining RE neither enables nor pressures that decision.

### What this contributes back to the accepted text

- **§5's indivisible-item alarm has a live, named instance.**
  `oracle-diff-test-action` (126.4s) and `oracle-diff-spec-test-action` (78.4s)
  are 72.1% of the cold premerge build. §5 says the planner must name such an
  item and refuse to express it as a lane count; this is that item, and the
  remaining remedy is to split or re-tier it. That is the work RE was proposed
  as an alternative to, and it is still the work.
- **The "compiler build is 286–317s" open question is refined.** That figure was
  the whole `Build all targets` step. The compiler *binary* is ~74s cold and
  locally built; the entire rue-crate chain is ~20% of the step. Build-once-and-
  fan-out was measured against the wrong quantity.
- **RE is answered as a scheduling lever and can stop being an open option.**
  The cache is doing its job (91% hit at p50 on cold compiler PRs); the residue
  is two harnesses, and no executor change reaches them.

### What would change this recommendation

- The two oracle-diff corpus actions being split into many actions. That would
  make the cold premerge closure genuinely wide, at which point more cores start
  to matter and this should be re-measured rather than re-argued.
- A measurement of the full premerge closure under RE, which requires a
  temporary workflow and a maintainer willing to spend the runs. It would
  replace this amendment's one inferred number with an observed one.
- Dedicated rather than shared executors, which would remove §2's tail.

## References

- `docs/notes/rue-1250-shard-topology-analysis.md` — shard decision and run sample
- `docs/notes/rue-1250-premerge-critical-path.md` — premerge lane profile
- `docs/notes/rue-1250-ci-architecture.md` — native lane profile and design detail
- `docs/process/ci.md`, `docs/process/build-cache.md` — current contracts
- ADR-0015 (test suite optimization), ADR-0063 (parallel demand-driven
  incremental compilation), ADR-0067 (compiler performance measurement)
- RUE-1250, RUE-1262, RUE-1164, RUE-1130, RUE-1131, RUE-1222; RUE-1116, RUE-1118,
  RUE-1119, RUE-1157, RUE-1163 for the mechanisms this ADR unifies
