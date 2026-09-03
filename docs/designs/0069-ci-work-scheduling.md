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

Accepted. Phases 1-4 are implemented: determination on every lane (RUE-1130),
duplicate-target removal (RUE-1262), the duplication gate (RUE-1265), and
graph-owned platform scope (RUE-1266). Phase 6 landed with RUE-1267 and its
graph derivation was completed by RUE-1935/RUE-1936; Phase 5 remains unstarted.

**Amendment 1 (2026-08-14)** records the remote-execution evaluation this ADR's
open questions invite and declines RE as a scheduling lever: it is accepted as
recommended. Nothing changes in CI as a result — RE stays in the merge-group
canary, where it already was — so the amendment is a decision not to act, and is
implemented by construction. See
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
- Both native lanes were one step, and that step was **99.1%**
  `rue-compiler-test`, contradicting `docs/process/ci.md:46` ("Linux ARM64 and
  macOS ARM64 deliberately do not repeat the broad unit suite") while
  `scripts/validate-ci-gate.py:204` pins the repetition in place. RUE-1265
  resolved the contradiction in the only direction available without a coverage
  ruling. RUE-1266 now makes the original claim true: broad compiler tests stay
  in linux-premerge and the host-conditional subset has a focused native target.
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
(RUE-1157) and, where needed, the validated `rue_platform_native` platform
scope label. Lanes are generated
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
former and no longer pay for the latter. Host-conditional compiler tests move into their
own target, following the precedent already set in the same crate by
`rue-compiler-public-api-test`, `rue-compiler-differential-oracle-test`, and
`rue-compiler-payload-schema-test`.

### 2. Nothing executes twice

No unit of work executes more than once per platform per run without a declared
reason.

This is stated first among the behavioural rules because it is the only lever
that pays in every regime. The original measurements found three violations;
two remain represented in the current ledger, while RUE-1266 removed the
compiler cross-platform one:

- `scaling-matrix-test` re-runs 813 of `rue-compiler-test`'s tests, in the same
  lane, to add three;
- the focused compiler host-conditional target repeats on linux-arm64 and
  macOS, while `rue-compiler-test` is now owned only by linux-premerge;
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
| ~~`SELECTABLE_CORPUS`~~ | a new corpus job is ungated, so it always runs | **deleted** (RUE-1936): `corpus-targets` is derived from `_corpus_action` ∩ `rue_heavy_suite` ∩ (`rue_ci_dedicated_lane` ∪ `rue_cli_shard`); the `ci-contract` live tier check fails if the slow tier leaves it |
| `SELECTABLE_LANES` / `lane_targets` | a lane's job runs a target selection cannot see | **gated** (`gated_lane_errors`, `native_lane_ownership`, `clippy_lane_ownership`): every gate step names a lane the determinator emits, and the native and clippy proxies equal the live graph |
| the native lanes' platform unit list | new platform-sensitive tests are not enrolled | graph-owned `rue_platform_native` query (RUE-1266) |
| narrowed-lane scopes (`scope_targets`) | a narrowed lane silently widens beyond its unnarrowed work | by construction (RUE-1935): `narrow-scope` is only ever `scope ∩ impacted` over the consumer's live scope; the registry, the shell-text pins, and the content proofs that duplicated this were deleted |
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
      answers with "keep it and extend it to everything." Seven lanes are gated
      (`native` ×2, `release`, `valgrind`, `asan`, `compiler-reproducibility`,
      `rue-program-digests`),
      freeing **905–1034s of runner time** on each of four measured peripheral
      runs. `linux-premerge` and the native unit targets are narrowed to the
      impacted closure instead, so an unimpacted test binary is neither built
      nor run. The narrowed-lane contract now makes each narrowed scope an
      intersection with its declared unnarrowed scope; unavailable graph or
      closure queries fall open and report `DEGRADED`, never a verified subset.
      The determinator reports the head-graph denominator, live impacted
      closure, and selected/deselected lanes and corpora. Each registered
      consumer then reports its exact selected/unnarrowed target counts and
      unweighted saved share in the step summary (explicitly not runner-time
      savings); a planner candidate is not reported as verified. Dorian's recorded
      post-#2160 real-run measurements are: compiler-touching run
      `31348880780`, impacted closure **67**, build **4.5m**, premerge **7.9m**
      versus **12.1m** pre-narrow baseline and **41.5m** regressed; **31** local
      actions after versus **76** regressed, and zero root-level corpus actions
      after. Peripheral run `31348882324` had **0** impacted targets, no corpora
      or lanes, and a **1.7m** cache-served premerge; it does not measure the
      cold full-pattern cost. Phases 5 and 6 remain, and these measurements do
      not establish cold full-pattern cost or native-host wall-time savings.
      A structural negative consequence remains: CI/workflow changes are
      force-full, so a PR changing narrowing cannot exercise narrowing itself;
      correctness therefore relies on hermetic contract tests and subsequent
      real selective runs.
- [x] **Phase 3: Duplication gate** — RUE-1265.
      `scripts/validate-test-duplication.py` enumerates each lane from the live
      graph, collects identities with `--list`, and fails on any test scheduled
      more than once per platform per run without a declared allowance. It runs
      as a step in the premerge lane, where the binaries it lists are already
      built, and costs 0.29s wall. `docs/process/ci.md` now states the invariant
      — with the scope it actually checks, and a section naming where it cannot
      see. The compiler cross-platform allowance was removed by Phase 4.
- [x] **Phase 4: Platform scope as a target attribute** — RUE-1266.
      `rue_platform_native` is validated by the shared wrappers and selected
      from the live Buck graph. The workflow names no native unit target, and
      the focused compiler target removes broad cross-platform repetition while
      preserving host-conditional coverage.
- [ ] **Phase 5: Real input-keyed compile actions** — RUE-1164, milestone
      "Buck-native test actions"; RUE-1222 for the timing-refresh and
      undeclared-input follow-ups.
- [x] **Phase 6: Floor-aware packer with the indivisible-item alarm** —
      RUE-1267.
      `ci/cli-shard-planning.json` records the measured 407s native
      floor, 1,098s CLI maximum, and 25% count allowance. The deterministic
      planner derives four runners, refuses to name a count if an indivisible
      item exceeds the floor, and generates the required matrix from the live
      `rue_cli_shard` graph union. The weekly cache-free repetitions now feed a
      separate 20% observed lane-wall skew guard; the estimated self-check and
      `validate-cli-shard-coverage.py` are gone. The planning file's
      `phase_6_remeasurement` records merge-group run 33721329318 as the
      explicit 281s pre-change baseline (compiler reproducibility binds at
      225s), and passing PR run 33727605782 as the 358s post-change execution
      (all 11 matrix jobs present; premerge is the longest substantive job at
      209s). Event/cache mismatch and runner queueing make this execution and
      no-topology-delta evidence, not a causal speedup comparison.
      **Phase 6 follow-up (RUE-1935/RUE-1936).** The planner's
      `--corpus-targets` input was still the hand-maintained `SELECTABLE_CORPUS`
      array; it is now the graph query above, with the six oracle
      differentials given the `rue_ci_dedicated_lane` label they always
      deserved. The determinator itself shrank to BTD plus a thin wrapper:
      the count/content proof layer, the scope registry, the manifest tables,
      the `ci-corpus-decision` adapter, and the clippy select/materialize
      phases were deleted, because the merge queue always runs full and so a
      pull-request under-selection costs one queue ejection, never a merged
      regression. The validators that synced hand copies went with them
      (`narrowing_contract_errors`, the clippy adapter text pins, the
      performance-pin and Valgrind-installer text pins, the static Python
      floor scanner, the separate dotslash cache-key gate); what survives
      compares graph facts, workflow wiring, and the aggregate.

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

**Status: accepted 2026-08-14, as recommended.** This amendment answers the
question RUE-1505 asks — should ordinary CI builds run on BuildBuddy remote
execution rather than only consuming its cache — and answers *no*. Remote
execution stays exactly where it is, in the merge-group canary, so accepting
this changes no CI configuration: it is a decision not to act, and the reasons
are recorded here so the question is not reopened without new evidence.

Two findings outlived the RE question itself and are tracked separately.
BuildBuddy cache transfer already exceeds the free tier's published 100 GB by
roughly 8.5× on upload alone, independent of RE — the capacity conversation is
about the cache. And the two oracle-diff actions this amendment identifies as
78.9% of `Build all targets` were being built by premerge despite owning
dedicated lanes; RUE-1511 fixed that, which is the ~240s lever RE was proposed
as an alternative to.

Full measurements, populations, and caveats are in
`docs/notes/rue-1505-remote-execution-evaluation.md`, following the same
decision-here / evidence-in-notes split this ADR already uses for RUE-1250.

### Recommendation

**Keep remote execution scoped exactly where it is**: the merge-group canary in
`.github/workflows/ci.yml`, which also serves as ADR-0070's negative control 2.
Do not route `premerge`, the other linux-x64 build jobs, or any required lane
through `//platforms:remote_execution`.

RUE-1505 framed the choice as "more cores, less work, or more overlap", with RE
as the more-cores lever. The finding is that **the work that is cold is not the
work that is wide**, so more cores has almost nothing to compress — and that a
much larger "less work" lever is sitting unexercised in the same step.

### Evidence base

500 `CI` runs and ~1,100 job logs, 2026-08-10 to 2026-08-14. Required CI already
runs the controlled experiment on every merge: the `remote execution` canary and
the `compiler reproducibility` job build **the same 396–405 actions** of
`//crates/rue:rue`, cache-disabled, on the same commit and runner class, one
remote and one local. That pair ran 212 times in the window.

No BuildBuddy credential is reachable from a developer host — the key exists
only as a repository secret, and GitHub does not return secret values — so local
RE runs, the invocation API, and the account dashboard were all unavailable.
**No measurement of the full premerge closure under RE exists**; every claim
about premerge under RE below is inference from measured composition.

### 1. RE is modestly faster, much less predictable, and wins only on width

Over 210 paired cold builds: remote p50 **51.5s** (cv **50%**, max 245.3s)
against local p50 **74.2s** (cv **8%**, max 83.4s). Paired ratio p50 **1.4×**,
min **0.31×** — RE was three times *slower* in the worst pair. Netting the
toolchain `http_archive` fetch that only the local side performs (7.2s vs 0.8s)
gives the like-for-like figure: **≈1.3×**.

Input transfer is not the constraint (Up/Down p50 3.7/3.4 MiB per build). The
tail is shared-pool contention, and it is a recurring pattern rather than an
incident: the 15 slowest builds span four days and 18 distinct hours, and
**27 of 210 (12.9%) exceed 90s — slower than the slowest local build observed**.

Per-action attribution across all 424 runs shows where the 1.3× comes from:

| action | remote | local | local ÷ remote |
| --- | ---: | ---: | ---: |
| `rue-compiler` rustc rlib | 8.1s | 7.9s | **0.98** |
| `rue-compiler` rustc metadata | 4.9s | 3.9s | **0.79** |
| `winnow` rlib (41 queued locally) | 0.0s | 1.4s | 40.8 |
| toolchain `http_archive` | 0.7s | 6.3s | 9.0 |

**RE ties or loses on every critical single action and wins only where actions
queue behind the local executor.** Its advantage is width, not per-action speed.

### 2. Why that cannot pay: 78.9% of the cold build is two indivisible actions

Over 169 cold-compiler `pull_request` runs, `premerge`'s `Build all targets`
step ran p50 294s, ~91% cache-served, ~76 actions executed locally. Attributing
its wall time:

| target | mean | share |
| --- | ---: | ---: |
| `//crates/rue-oracle-diff:oracle-diff-test-action` | 138.2s | **50.6%** |
| `//crates/rue-oracle-diff:oracle-diff-spec-test-action` | 77.1s | **28.3%** |
| `//crates/rue-compiler:rue-compiler-test` | 19.4s | 7.1% |
| `//crates/rue-compiler:rue-compiler` | 13.1s | 4.8% |
| every third-party crate, together | 0.4s | **0.13%** |

**Two single actions are 78.9% of the cold premerge build.** They are one
`cached_corpus_suite` genrule each, so RE cannot subdivide them, and §1 says it
would not run them faster. The wide fan RE does accelerate — and which supplies
essentially all of the canary's 1.3× — is already fully cache-served on a real
PR and worth **0.13%** of the step. The canary's speedup is not transferable to
premerge, because the canary measures a cold graph premerge never has.

### 3. The lever this evaluation found instead, worth ~8× what RE offers

At the time of this evaluation, `crates/rue-oracle-diff/BUCK` declared both
suites `tier = "slow"` and `rue_heavy_suite`, and `ci.yml` gave each its own
dedicated lane, but **premerge built them anyway**. Its unnarrowed
`./buck2 build //crates/...` reached them, while its narrowed `build_scope()`
used a crate-prefix filter that also admitted them. RUE-1511 subsequently
removed the owned corpus-action closure from both build-scope branches, and
RUE-1292 made the narrowed result an exact intersection with that live
unnarrowed scope so a deleted or base-only label cannot reintroduce widening.

The comment directly above `build_scope()` explains that the filter exists
precisely because building a `cached_corpus_suite` action *runs its corpus*, and
that this once took premerge from ~12m to 32-42m. That fix removes root-level
(`//:`) corpus actions only. These two are crate-level and pass through both
branches.

**Worth ~240s at the median, against ≤30s for routing the same step through
RE.** Filed separately; named here because it is what RE was being proposed as
an alternative to.

### 4. Concurrency: the duplication RUE-1505 predicted is real

`premerge (linux-x64)` and `test (linux-x64-oracle-diff)` **both execute
`oracle-diff-test-action` cold, concurrently, at the identical execution
configuration**, on two runners. Over 60 cold-compiler `pull_request` runs the
median wall-clock overlap is **75s, present in 57 of 60 runs**, with both sides
going `local_execute` → `upload (action)` — racing, not one serving the other.

This is invisible in the obvious counter: the lane reports `Commands: 347
(cached: 342, local: 5)`, and one of those five is the 148s harness. **A 99% hit
rate and a two-and-a-half-minute duplicated execution look identical there.**
Counting actions rather than seconds is the error the accepted §5 exists to
forbid, and §2 above identifies the same action as the largest item in premerge.

Prior runs are not an explanation: every `head_sha` in the sample is unique, so
no earlier run warmed the changed crate. Siblings warm each other in real time
when their windows happen not to overlap; when they start together, both pay.

The aggregate picture for the *other* siblings does hold — on cold compiler PRs
`valgrind` executes 3 local actions, `release` 16, `native (linux-arm64)` 19,
and `compiler reproducibility` 396 by design. RE would relocate those, not
remove them.

### 5. Reliability: RE is not the flaky part, but the canary is not exposed

The canary succeeded in **210 of 212** merge-group runs (**0.94%** failure,
against 0.9% for `premerge` on `merge_group`). Both failures were the "Build
compiler remotely" step failing because `dotslash` could not fetch buck2 from
the GitHub releases CDN, before the RE endpoint was contacted. **Zero
BuildBuddy-attributable failures in 212 runs.**

That flatters the canary and misleads about adoption.
`//platforms:remote_execution` sets `allow_hybrid_fallbacks_on_failure = False`,
so BuildBuddy unavailability becomes merge-queue unavailability; the forgiving
platform tolerates action *failure*, not an unreachable endpoint; and **fork PRs
have no secret, hence no endpoint at all**. Today that degrades gracefully to a
cold local build. RE-by-default needs a second conditional path — the least
exercised and most depended upon code in required CI.

### 6. Cost: the free tier is already exceeded, on the cache alone

Published terms (buildbuddy.io/pricing, fetched 2026-08-14): the Personal free
plan, "For small teams and open source projects", gives **"100 GB of cache
transfer"** and **"Up to 80 cores for remote builds"**. Their FAQ: *"We don't
apply hard limits that prevent you from using more than your plan allows."*

**So what happens at a cap is nothing technical** — not throttling, not hard
failure, not a silent stop-caching. It is a commercial conversation. The
correctness-adjacent surprise RUE-1505 feared is not the exposure here.

The inference-free measurement: buck2's **upload** counter cannot be confused
with `http_archive` traffic, and across 16 complete runs it is 0.38 GiB mean per
`pull_request` run and 0.01 GiB per `merge_group` run. At the measured 70.6 and
52.0 runs/day, **upload alone is ≈28 GiB/day ≈ 850 GiB/month — 8.5× the
published allowance, with zero inference.** Including download (an upper bound,
since it conflates CAS with toolchain fetches) gives ~3.7 TB/month.

Unverified, and not to be presented otherwise: **the period the 100 GB covers is
not published**; the Team overage rate renders as a literal `$X / GB`; and
**current account usage against any cap could not be read** — no credential, no
dashboard. Two reconciliation caveats: the window is Monday–Friday, so a ×30
extrapolation states a weekday rate as a calendar rate (~2.7 TB on ~22 active
days, still 27×); and RUE-1505's "~50 runs/day" meant *`pull_request`* runs, so
the comparable measured figure is 70.6/day — a factor of 1.4, not 2.5.

The conclusion is therefore not "RE would push us over the free tier". It is
that **the repository already draws on BuildBuddy's goodwill an order of
magnitude beyond the published allowance, on the cache alone**, and nothing has
broken. RE's own transfer is negligible; what would rise is core-hours against
the 80-core figure, and §1 shows that pool already visibly contended at today's
essentially zero RE usage.

### Scope questions RUE-1505 asked

**Which jobs should default to RE?** None. Applying §1's 1.3× to the ~21% of the
step RE could touch saves ~14s at p50; ~30s is a generous ceiling. `premerge`
would trade that for a shared-pool tail with a 12.9% slow rate, and would import
a hard external dependency into the merge queue. Every other linux-x64 build job
executes 3–19 cold actions per run and has nothing to gain.

**Does `compiler reproducibility` stay cache-free and locally executed?** Yes,
and RE would actively weaken it. The proof works by making the two builds differ
— relocated root, `--num-threads 2`, different `TMPDIR`, `TZ`, `umask`, mtimes —
so that a byte-identical result indicts path, scheduling, and environment leaks.
A uniform remote container makes them *more* alike, so a pass would prove less.
`check-reproducible-compiler.sh` also hard-errors on a `.buckconfig.local`.

**Does RE change ADR-0070's hermeticity story?** Not materially, and the effect
is the opposite of the one anticipated. Negative control 2 needs exactly one
clean-root remote materialization, and it has one. Running everything remotely
would catch undeclared inputs in more places — a real if small gain — but would
convert each latent instance into a required-lane failure with no local
reproduction, the cost profile ADR-0070 chose a single canary to avoid. Broader
adoption strengthens the argument for *keeping* the canary, not for generalizing
it. `rue-program-digests` covers the complementary direction.

**Does this interact with RUE-1407?** No, and RUE-1505's reasoning is correct.
Keeping remote test-result caching off was a trust decision about *verdicts*; RE
changes where build *actions* execute. Rue additionally configures
`noop_test_toolchain` and `noop_remote_test_execution_toolchain`, so nothing
honours `supports_test_execution_caching` whatever the execution platform is.
Adopting or declining RE neither enables nor pressures that decision.

### What this contributes back to the accepted text

- **The accepted §5's indivisible-item alarm has a live, named instance**, and
  the accepted §2's "nothing executes twice" has a live violation. The two oracle-diff actions are 78.9% of
  the cold premerge build *and* run twice per run against their dedicated lanes,
  with a 75s median overlap. The duplication gate cannot see it, because the
  gate compares test identities while this is a build action racing a lane.
- **The accepted §5's remedies needed correcting for this case.** These suites
  were *already* re-tiered to `slow` with dedicated lanes; the defect was that
  premerge built them regardless. RUE-1511 fixed the corpus-action exclusion,
  and RUE-1292 now proves narrowed consumers are subsets of their registered
  live scopes. Splitting the suites remains a future optimization, not a
  correctness remedy and not part of Phase 2.
- **The "compiler build is 286–317s" open question is refined.** That figure was
  the whole `Build all targets` step. The compiler *binary* is ~74s cold and
  locally built; the entire rue-crate chain is ~21% of the step.
  Build-once-and-fan-out was measured against the wrong quantity.
- **RE is answered as a scheduling lever and can stop being an open option.**

### What would change this recommendation

- The two oracle-diff actions being split into many actions, or removed from
  premerge's scope. Either makes the cold closure genuinely wide, at which point
  more cores start to matter and this should be re-measured, not re-argued.
- An observed measurement of the full premerge closure under RE, which needs a
  temporary workflow and a maintainer willing to spend the runs.
- Dedicated rather than shared executors, which would remove §1's tail.

## References

- `docs/notes/rue-1505-remote-execution-evaluation.md` — Amendment 1's
  measurements, populations, and unverifiable items
- `docs/notes/rue-1250-shard-topology-analysis.md` — shard decision and run sample
- `docs/notes/rue-1250-premerge-critical-path.md` — premerge lane profile
- `docs/notes/rue-1250-ci-architecture.md` — native lane profile and design detail
- `docs/process/ci.md`, `docs/process/build-cache.md` — current contracts
- ADR-0015 (test suite optimization), ADR-0063 (parallel demand-driven
  incremental compilation), ADR-0067 (compiler performance measurement)
- RUE-1250, RUE-1262, RUE-1164, RUE-1130, RUE-1131, RUE-1222; RUE-1116, RUE-1118,
  RUE-1119, RUE-1157, RUE-1163 for the mechanisms this ADR unifies
