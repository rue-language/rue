# RUE-1250: CLI shard topology reassessment

Measured reassessment of the four-way CLI corpus sharding introduced by
RUE-1116 and cost-balanced by RUE-1158, against current cache and
affected-target behavior.

**Decision: reduce and generalize.** Keep parallel corpus execution — cold runs
still need it — but stop expressing it as a hand-maintained shard count on one
harness. The current topology spends its fan-out on the one corpus that caching
already made cheap, while the actual critical path runs unsharded on a single
runner.

## Method and sample

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

**Reduce** the CLI shard count and **generalize** the mechanism. Do not remove
sharding: on the four executing runs, a single unsharded CLI corpus projects to
900–1100s (measured shard sums), which would exceed premerge on the merge-group
miss run (548s) and become the new critical path. Parallel corpus execution is
load-bearing; the CLI-specific, hand-maintained expression of it is not.

Note that "how many CLI shards" has no fixed answer, which is itself the
argument. Two lanes (≈495s) sit under today's 548s merge-group critical path by
54s. Fix F7 and premerge drops to roughly 300s — at which point two CLI lanes
are the critical path and three are correct. A constant cannot track that; a
derivation can.

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

The plan comes from `scripts/plan-ci-lanes.py`: read the live premerge test
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

### 6. Concrete first move, runner-neutral

Reduce CLI shards 4 → 2 and give the two recovered slots to premerge halves.
Same eight linux-x64 runners, materially better packing, and it exercises the
generalized rule on the lane that actually needs it.

## Expected effect, with the Amdahl caveat

The F2 ratios are an **upper bound**, not a forecast. Three floors apply:

- per-job overhead of 13–25s (setup, checkout, dotslash, post) per lane;
- the largest indivisible test target, which this sample cannot measure;
- the compiler build, which every premerge lane pays and which does not shard.

That last one dominates cold runs. On run 31230794223 the premerge job was 286s
of build plus 505s of tests; a four-way split of the *tests* gives
286 + 126 ≈ 412s, not 200s. On warm runs the build is 6s and the split is nearly
linear. Fork PRs without the remote cache pay the full build in every lane
concurrently — parallel, not serial, but real.

Realistic critical path after the first move (F2 sample, premerge halved and CLI
at two lanes):

| scenario | today | projected |
| --- | --- | --- |
| warm merge group | 450–463s | ~240–260s |
| miss merge group | 548s | ~350–400s |
| cold pull request | 802–873s | ~550–600s |

Runner cost is unchanged by construction. For reference, CLI shards are
currently 19.2% of runner-seconds and 10.8% of billed job-minutes across the
sample; on warm runs four shard jobs bill four minutes to verify ~20s of cached
stamps.

## What this note does not establish

- The per-target breakdown inside the premerge lane. The split in step 6 is
  justified by the lane's total and by the structural argument in F7, but the
  actual cut points need one instrumented run (`buck2 test` with per-target
  timings, or the existing `ci-timed` job summaries read from a real run).
- Behavior of genuinely cold fork PRs, which carry no BuildBuddy key.
- Whether the largest indivisible premerge target sets a floor above the
  projections above. This is the main risk to step 6 and should be measured
  before K is tuned.
