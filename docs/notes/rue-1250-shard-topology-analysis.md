# RUE-1250: CLI shard topology reassessment

Measured reassessment of the four-way CLI corpus sharding introduced by
RUE-1116 and cost-balanced by RUE-1158, against current cache and
affected-target behavior.

**Decision: keep four shards; fix the mechanism around them.** Four is the
correct count — see "Decision" below, which supersedes an earlier draft of this
note that recommended reducing to two. That draft measured the shards' slack
against `premerge`, which is 279–576s ahead of them; but premerge's lead is a
defect (`rue-1250-premerge-critical-path.md`), and the floor that actually
governs the CLI corpus is the unshardable native ARM64 lane at 341–407s. At two
shards the corpus exceeds that floor in 4 of 4 cold runs.

What does need to change is the mechanism: a balance guard that cannot fire, a
weights file that drifts undetected, and a count replicated by hand across 23
files — so the right answer has to be re-derived from scratch every time the
system moves, as this issue did.

The premerge defect is tracked separately as **RUE-1262**; it is the change that
actually moves required-CI latency, and it costs no extra runners.

## Method and sample

> Superseded in breadth by ADR-0069, which collects 120 runs, classifies 55 by
> change class, and times 21. Nothing measured here changed under the wider
> sample — `premerge` remained the critical path in 18 of 21 timed runs — but the
> determination conclusions belong to the ADR, which has the change-mix data this
> note lacks.

Nine `CI` runs from 2026-08-07/08 (four `pull_request`, five `merge_group`),
read from the Actions jobs API: per-job `created_at` / `started_at` /
`completed_at` and per-step durations. Shard-balance arithmetic is computed
directly from the checked-in `crates/rue-cli-tests/shard-weights.json` against
the LPT assignment in `crates/rue-cli-tests/src/sharding.rs`.

Runs separate cleanly into two populations by whether the corpus actions hit the
cache: five *warm* runs (CLI corpus step 4–5s) and four *executing* runs (corpus
step 169–266s).

Limits of this sample, stated up front:

- Raw run logs are not retrievable from this environment (the artifact host is
  blocked by network policy), so the **internal** composition of the premerge
  lane is derived structurally — from `test.sh`, `corpus.bzl`, the tier labels,
  and the Buck rule inventory — not measured per target. Every number attributed
  to a *job* or *step* below is measured; no number attributed to a target
  inside the premerge lane is.
- All sampled pull requests are same-repository branches, so they carried
  `BUILDBUDDY_API_KEY`. Genuinely cold fork PRs are not represented.
- Observed queue delay was 2–85s (median 3s). Queue pressure is not currently a
  constraint and is not a factor in the recommendation.

## Findings

### F1. The fan-out is aimed at a job that has never been the critical path

`premerge (linux-x64)` was the longest job in **9 of 9** runs. The gap between
it and the slowest CLI shard was 279–576s (median 444s).

| run | event | wall | premerge | slowest shard | slack |
| --- | --- | --- | --- | --- | --- |
| 31228627966 | PR (warm) | 478 | 467 | 18 | 449 |
| 31231601304 | MG (warm) | 478 | 463 | 19 | 444 |
| 31229045120 | MG (warm) | 462 | 450 | 20 | 430 |
| 31222559451 | MG (warm) | 483 | 456 | 21 | 435 |
| 31226836874 | MG (warm) | 350 | 312 | 22 | 290 |
| 31237682485 | MG (miss) | 571 | 548 | 269 | 279 |
| 31230794223 | PR (miss) | 820 | 802 | 281 | 521 |
| 31219956012 | PR (miss) | 848 | 839 | 290 | 549 |
| 31220565989 | PR (miss) | 895 | 873 | 297 | 576 |

Splitting the CLI corpus four ways has not shortened a required-CI run in this
sample. It cannot: the shards finish while premerge still has 5–10 minutes left.

### F2. The linux-x64 lane family is 2.6–6.7× off a perfect pack of its own runners

Required CI always spends exactly **eight** linux-x64 job slots on this work:
`premerge`, four CLI shards, `spec`, `oracle-diff`, `oracle-diff-spec`. Compare
the observed critical path against a perfect pack of the same eight runners:

| run | event | total lane-seconds | perfect pack | observed max | ratio |
| --- | --- | --- | --- | --- | --- |
| 31228627966 | PR (warm) | 560 | 70 | 467 | **6.67×** |
| 31231601304 | MG (warm) | 577 | 72 | 463 | **6.42×** |
| 31222559451 | MG (warm) | 577 | 72 | 456 | **6.32×** |
| 31229045120 | MG (warm) | 572 | 72 | 450 | **6.29×** |
| 31226836874 | MG (warm) | 426 | 53 | 312 | **5.86×** |
| 31230794223 | PR (miss) | 2058 | 257 | 802 | **3.12×** |
| 31219956012 | PR (miss) | 2300 | 288 | 839 | **2.92×** |
| 31220565989 | PR (miss) | 2487 | 311 | 873 | **2.81×** |
| 31237682485 | MG (miss) | 1668 | 208 | 548 | **2.63×** |

The runners are already provisioned. The work is already parallel-safe. The
packing is what is wrong — a single lane holds 27% (miss) to 82% (warm) of all
the work assigned to eight machines.

### F3. Event type does not predict cache warmth, so event-conditional topology would be wrong

The issue asks whether PR and merge-group topology should differ. The sample
says no: run 31228627966 is a **warm pull request** (65s of total shard time)
and 31237682485 is a **cold merge group** (989s). Warmth tracks what the diff
invalidated, not which event fired.

Conditioning on the event would deselect fan-out on exactly the cold PR runs
that need it and retain it on warm merge groups that do not. The existing
`affected-targets` determinator already conditions on the right thing.

### F4. The 25% skew budget is provably unreachable, while real skew reaches 24.9%

`CliShardPlan::validate_skew` rejects an estimated slowest shard more than 25%
above the mean. Run against the checked-in weights, the estimate is *perfect* at
every shard count:

```
 N    max_ms    mean_ms   estimated skew
 2   1101488   1101485        0.00%
 4    550749    550742        0.00%
 8    275375    275371        0.00%
```

This is structural, not luck. LPT guarantees `max ≤ mean + (1 − 1/N)·w_max`.
With `w_max` = 28.6s (`cli.examples_lattice::lattice_runs_cross_oracle_selftest`)
and N = 4, the worst *possible* estimated skew is 3.90%. Breaching 25% would
require a single case costing ≥ 183.6s — 6.4× the current slowest case. The
guard cannot fail on real data; it only fires in its own unit test's contrived
four-case fixture.

Meanwhile the **observed** skew across executing runs was 8.1%, 8.8%, 18.4%, and
**24.9%** — right at the budget the estimate claims is 0.00%.

The cause is a units mismatch. `estimated_load_ms` sums *serial* per-case cost,
but a shard's wall time is the makespan of those cases under the harness's own
internal parallelism (effective ≈ 2.2× on a 4-vCPU runner: 550.7s of planned
serial load completes in ~247s). Balancing serial sums does not balance parallel
makespans, and the guard validates the model rather than the outcome.

### F5. Weights drift with no detector

The case corpus declares 1722 case tables across 208 files; the weights file
carries 1714 entries, and 13 case files have no section in it at all. Unmeasured
cases silently take `default_ms` (1059). Refresh is a manual human procedure
(download per-platform JSONL artifacts, run `generate-cli-shard-weights.py`).
Because F4's guard is vacuous, nothing detects staleness — the estimate stays at
0.00% skew whatever the weights say.

### F6. The shard count is a constant replicated across 23 files

`CLI_TEST_SHARD_COUNT = 4` in `BUCK` is mirrored by four hand-written matrix
entries in `ci.yml` (five fields each), by `correctness-repetitions.yml`, and by
`docs/process/ci.md`. `scripts/validate-cli-shard-coverage.py` (118 lines) plus
`scripts/test-cli-shard-coverage.py` exist for the sole purpose of asserting
that two hand-written lists agree. Changing 4 → 2 or 4 → 6 is a multi-file edit
across BUCK, two workflows, a validator, that validator's tests, and docs.

That gate is real coverage insurance given a hand-written matrix — but the
hand-written matrix is the thing worth removing, and the gate goes with it.

### F7. Only one corpus can ever be sharded, and the rest serialize on one runner

Sharding is a property of the CLI harness (`RUE_CLI_TEST_SHARD`, `CliShardPlan`,
`shard-weights.json`, the `rue_cli_shard` label), not of the CI scheduler. The
repository defines eleven `cached_corpus_suite` corpora. Two have dedicated
lanes (`spec-tests`, `cli-tests` via its shards) and two more run as their own
jobs (`oracle-diff-test`, `oracle-diff-spec-test`).

The remaining premerge-tier corpora — `ui-tests`, `reproducible-programs`
(timeout 1800), `frontend-diff-test` (900), `oracle-diff-generated-smoke` (600),
`release-smoke` — have no lane. Every one declares `weight_percentage = 100` in
`corpus.bzl`, meaning each demands the whole machine, so buck2 correctly runs
them **one at a time inside the single premerge job**, alongside roughly 130
unit and `sh_test` targets that OSS buck2 re-executes on every run because it
has no test-result cache.

This is why warm premerge is still 450–467s while every cached corpus costs 4s:
that lane's cost is cache-immune test execution plus serialized corpora, and it
is the one lane with no fan-out mechanism available to it.

It is also the scaling answer the issue asks for. Each new independent aspect of
the toolchain adds another `weight_percentage = 100` corpus to that serial
queue, extending the critical path by its full wall time, with no way to
parallelize it short of building a second bespoke shard system next to the CLI
one.

## Decision

**Keep four.** Reject *reduce*, *remove*, and *conditionalize*.

The count must be judged against the slowest thing CI cannot shard away, not
against whatever happens to be slowest today. That is the native ARM64 lane: it
runs on every event, it is one runner by construction, and it measured 341–407s
(median 351s) across all nine runs. Against that floor, on the four runs where
the CLI corpus actually executed:

| shards | corpus wall | exceeds the ARM64 floor |
| --- | --- | --- |
| 4 (today) | 269–297s | **0 of 4** |
| 2 | 450–550s | **4 of 4** |
| 1 | 900–1100s | 4 of 4 |

Reducing to two would make the CLI corpus the critical path on every cold run
the moment premerge is fixed. Removing sharding does so immediately.

A derivation confirms the count rather than contradicting it.
`ceil(total ÷ floor)` gives 3 on all four runs (327–366s per shard), but the
measured shard skew is 8–25% (F4), and 366s × 1.25 = 458s breaches the 407s
floor. With a skew allowance, the derivation lands on **4** — which is where the
constant already is. Nobody had written down why, which is how this issue came
to ask the question from scratch.

Do not differ topology by event (F3): warmth tracks what the diff invalidated,
not which event fired, so an event-conditional rule would drop fan-out on
exactly the cold pull requests that need it.

What remains wrong is the mechanism, not the number. The three defects below
(F4, F5, F6) are worth fixing on their own terms, and none of them changes the
count.

## Proposed topology

### 1. Move sharding from the harness to the corpus rule

Lift `CliShardPlan` out of `crates/rue-cli-tests` into `rue-test-runner`, beside
the `ShardSelector` it already uses, and add `shardable = True` to
`cached_corpus_suite`. Any corpus then declares its own slices with one
attribute and a per-corpus weights file, reusing the existing `case_timings`
output for measurement. `spec-tests`, `ui-tests`, `frontend-diff-test`, and the
oracle differentials become shardable without new machinery.

### 2. Generate the matrix instead of writing it

`affected-targets` already runs before `platform-corpus` and already publishes
JSON that the corpus jobs consume. Extend it to emit the lane plan, and have
`platform-corpus` take `strategy.matrix: ${{ fromJSON(needs.affected-targets.outputs.lanes) }}`.

The plan comes from a new `plan-ci-lanes.py` script: read the live premerge test
graph from `//test_tiers.bxl`, read measured per-target wall times from a
checked-in timings file, LPT-pack into lanes, emit the matrix.

This deletes the hand-written matrix, and with it the drift that
`validate-cli-shard-coverage.py` exists to catch. Its replacement is a stronger
and simpler assertion: **the union of planned lanes equals the premerge
selection from the Buck graph**, fail-closed on any target that appears in the
graph and in no lane. Unknown or newly added targets fail toward running, as
`scripts/affected-targets` already does.

### 3. Derive the lane count from measurement and a platform floor

    K = clamp(ceil(total_measured_lane_seconds / lane_budget), 1, K_max)

with `lane_budget` set to the cross-platform floor — the native lanes CI cannot
go below. In this sample `native (linux-arm64)` ran 341–389s and
`native (macos-arm64)` 184–322s, so a budget near 390s is the honest stopping
point: driving linux lanes below the ARM64 lane buys nothing. K then grows on
its own as corpora are added and shrinks when caching improves.

### 4. Balance on observed wall time; guard on observed skew

Feed the packer measured **lane wall times**, not summed serial case costs
(F4's units bug). Replace `validate_skew` with a post-run check that compares
observed lane durations against the plan and reports skew in the job summary,
failing the weights-refresh path — not the contributor's PR — when observed
skew exceeds budget. Keep LPT; validate it against reality rather than against
itself.

### 5. Condition on selection, not on event

Keep one topology for `pull_request` and `merge_group` (F3). The determinator
already deselects unaffected corpora, and a deselected lane already costs only
spin-up.

### 6. Record the derivation next to the constant

`CLI_TEST_SHARD_COUNT = 4` should carry the reason it is 4 — the ARM64 floor and
the skew allowance above — so the next reassessment starts from the rule instead
of rediscovering it. Until step 3 computes the count, the comment is the
mechanism.

## What not to do, and why

The obvious-looking move is to spend CLI shard slack on the premerge lane: same
eight runners, better packing. Do not. The slack is not real — it exists only
because premerge is 279–576s ahead, and that lead is a defect rather than a
budget (`rue-1250-premerge-critical-path.md`). Two measurements close that door:

- **Premerge does not want more lanes.** Its parallel wall is 268s against a
  225.6s largest-indivisible-target floor, so target-level fan-out has at most
  16% to give. Two source-level fixes take the same lane to a ~30s bound with no
  extra runners at all.
- **The CLI corpus cannot give lanes up.** At two shards it breaches the ARM64
  floor in 4 of 4 cold runs.

Both halves of the trade fail independently. The runner budget is already in the
right shape; what is mispriced is the work inside one lane.

The Amdahl caveat that applied to the old proposal is worth keeping for whoever
revisits lane counts later: per-job overhead is 13–25s per lane, and the
compiler build does not shard — on run 31230794223 premerge was 286s of build
plus 505s of tests, so a four-way split of the *tests* would have given
286 + 126 ≈ 412s, not 200s. Fork PRs without the remote cache pay that build in
every lane concurrently.

## Expected effect

Keeping the count means required-CI latency is unchanged by this decision. The
critical path moves when the premerge defects are fixed, and then it lands on the
native ARM64 lane:

| scenario | today | after the premerge fixes |
| --- | --- | --- |
| warm merge group | 450–463s (premerge) | ~351s (native ARM64) |
| cold merge group | 548s (premerge) | ~349s (native ARM64) |
| cold pull request | 802–873s (premerge) | ~390–407s (native ARM64) |

At that point every linux-x64 lane, CLI shards included, sits below the floor,
and the next honest CI question is about the native lanes rather than about
sharding.

For cost reference: CLI shards are 19.2% of runner-seconds and 10.8% of billed
job-minutes across the sample. On warm runs four shard jobs bill four minutes to
verify ~20s of cached stamps — the price of the cold-run insurance the table in
"Decision" prices, and cheap against a wrong answer there.

## End state, and what the packer's objective should be

Modelling all three changes together — derived shard count, `scaling-matrix-test`
de-duplicated, and the pressure test reduced to its measured floor — against the
nine sampled runs, using each run's *own* build-step and suite-step durations:

| event | critical path today | after | driver after |
| --- | --- | --- | --- |
| pull_request (median) | 820s | **388s** | native (linux-arm64) |
| merge_group (median) | 456s | **342s** | native (linux-arm64) |
| whole-sample range | 341–873s | **341–407s** | native in **9 of 9** |

Two things matter more than the headline reduction. First, **no linux-x64 lane is
the bottleneck in any run any more** — including the cold-build pull requests,
where premerge lands at 354–393s, just under the ARM64 lane. Second, the spread
collapses from 2.6× to 1.19×, so the merge queue becomes predictable rather than
merely faster.

That changes what a packer should optimize. Once every linux lane sits under a
floor set elsewhere, **minimizing makespan is the wrong objective** — work
already finishes before the floor, so the remaining gain is zero. The right
objective is to *minimize runners subject to the slowest lane staying under the
floor*: bin-packing under a capacity constraint, not makespan across fixed bins.
The two produce different answers, and only the second can conclude "use fewer".

### Three regimes, measured

Solving `build + work/K + overhead ≤ floor` per run, with the ARM64 lane as the
floor and each run's measured build cost:

| regime | runs | poolable linux-x64 work | lanes needed | lanes today |
| --- | --- | --- | --- | --- |
| build warm, corpus warm | 5 of 9 | 42–66s | **1** | 8 |
| build warm, corpus cold | 1 of 9 | 1073s | **4** | 8 |
| build cold | 3 of 9 | 1208–1570s | 14–21 (unreachable) | 8 |

Read the third row as a boundary rather than a target. A cold build costs
286–317s of a 387–407s floor, leaving 75–88s of per-lane budget, so each
additional runner buys almost nothing and no lane count reaches the floor. **When
the per-lane fixed cost approaches the floor, the answer is to attack the fixed
cost, not to add lanes** — a packer that only knows how to add runners would ask
for 21 of them here and still miss.

Note also what "build cold" correlates with: all three were compiler-crate
changes, which invalidate most of the graph. That is the common case for this
repository's work, not an anomaly.

(The lane-count model assumes every lane pays the premerge build in full, which
overstates the fixed cost for corpus-only lanes. Treat the regimes as directional
and the ordering as the result.)

### The property that makes it robust

Cost concentration, not imbalance, is what defeats a packer — and it is invisible
to one that reasons only about totals. A packer given the pre-fix premerge lane
would have happily requested eight runners for a workload where one item was
225.6s, and reported success while changing nothing.

So the load-bearing requirement is an alarm, not an algorithm: **when the largest
indivisible item exceeds the floor, the planner must fail loudly, name the item,
and refuse to express the problem as a lane count.** Its remedies are the three
this repository already has — split the item (the CLI corpus's intra-target
sharding), cache it (RUE-1118's action-stamp pattern), or re-tier it (the canary
plus `slow`/`stress` split). Choosing among them is a human call; noticing is
not, and that alarm is what would have surfaced RUE-1262 the day it landed.

Two smaller properties follow from the same idea. The planner should **name its
binding constraint on every run** — the reason this issue had to ask about CLI
shards at all is that nothing in CI said what the critical path was. And the
balance guard should **compare observed lane walls to plan**, since stale weights
surface as skew and skew is the signal to refresh.

The pieces already exist: `affected-targets` runs first and emits JSON the matrix
consumes, `ci-timed` records per-command wall time and cache-hit rates, and
`cache-probe.yml` tracks cache health on a schedule. What is missing is the
arithmetic between them.

### What this promotes to next

Finishing this work does not end the question, it moves it. Two constraints
inherit the critical path, and neither is in RUE-1250's scope:

- **The native ARM64 lane, 341–407s, in 9 of 9 runs.** It is one runner by
  construction and this note never measured what it spends that time on.
- **The compiler build, 286–317s whenever a compiler crate changes.** It does not
  shard, so it caps what any linux topology can achieve.

Both are worth measuring the way the premerge lane was measured, before anyone
proposes a topology for them.

## What this note does not establish

- **Reliability.** The acceptance criteria ask for it and this note does not
  supply it. One `merge_group` run in the sample failed (31225370145); it was
  not investigated, and no flake rate was computed for the shards. The weekly
  `correctness-repetitions.yml` workflow already owns that signal and is the
  right place to read it from, not a nine-run sample.
- Behavior of genuinely cold fork PRs, which carry no BuildBuddy key — every
  sampled pull request was a same-repository branch.
- Whether the ARM64 floor is itself reducible. It is treated here as fixed
  because it is one runner by construction, but nothing in this note measures
  what it spends its 341–407s on. If it were reduced, the shard-count derivation
  would need re-running against the new floor — which is the argument for
  computing the count rather than pinning it.
