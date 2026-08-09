# A uniform CI architecture for Rue

Design conclusion of the RUE-1250 investigation. The two measurement notes
(`rue-1250-shard-topology-analysis.md`, `rue-1250-premerge-critical-path.md`)
establish what CI currently spends its time on; this one says what to build.

Rue already has all three mechanisms an ideal CI system needs, and each is good.
The defect is that each is applied to a different hand-picked subset of the work,
and the subsets do not overlap where it matters.

## The native lanes, profiled

Completing the picture. Both native lanes are one step, and that step is one
target:

| lane | median | dominant step | share |
| --- | --- | --- | --- |
| `native (linux-arm64)` | 351s (341–407) | "Run native backend, linker, runtime, ABI, allocator, and target tests" | **87.2%** |
| `native (macos-arm64)` | 230s (184–385) | the same step | **76.5%** |

That step runs eight targets. Measured locally, seven of them total **2.1s** and
`//crates/rue-compiler:rue-compiler-test` is **225.6s — 99.1% of the step**. And
that target is 94% one test function
(`failed_wide_batches_release_their_unpublished_child_cones_under_pressure`,
181.7s, RUE-1262).

So the same test function runs **four times per CI run**: twice in the premerge
lane (`rue-compiler-test` and its duplicate `scaling-matrix-test`), once on
linux-arm64, once on macOS. It is the dominant cost in all three lanes.

The "ARM64 floor" that an earlier draft of the topology note treated as an
immovable platform constraint is not a platform constraint. It is the same
defect, seen from a third angle.

### This contradicts the documented contract

`docs/process/ci.md:46` states:

> Linux ARM64 and macOS ARM64 deliberately do not repeat the broad unit suite or
> the specification corpus.

They do. `rue-compiler-test` is 813 tests — the broad compiler unit suite — and
`scripts/validate-ci-gate.py:204` pins it into the native lanes' required list.
The gate that exists to prevent coverage drift is what enforces the repetition.

What the native lanes actually want is the host-conditional coverage: roughly 27
`#[cfg(unix)]` / `#[cfg(target_os = "macos")]` / `#[cfg(target_arch = ...)]`
sites in the crate. They get all 813 tests, including a query-eviction pressure
test with no platform dependence whatsoever.

That is the general shape of the bug: **platform requirements are declared
per-target, but they are a property of individual tests.** A whole test binary is
too coarse a unit to express "this needs a real Mach-O linker."

## What the end state looks like

Fixing the pressure test once, at the source, fixes it in all four places.
Modelled against the nine sampled runs using each run's own step durations:

| | today | after |
| --- | --- | --- |
| pull_request (median critical path) | 820s | **361s** (−56%) |
| merge_group (median critical path) | 456s | **185s** (−59%) |
| `native (linux-arm64)` | 341–407s | **56–103s** |
| `native (macos-arm64)` | 184–385s | **41–130s** |
| whole-sample range | 341–873s | 170–393s |

The binding constraint stops being a single defect and becomes genuinely
distributed: `valgrind` (4 of 9 runs, ~170–185s), the cold compiler build inside
premerge (3 of 9, ~354–393s), `compiler reproducibility` (1), and one CLI shard
(1). That is what a healthy system looks like — no one thing to fix, and the next
improvement is a real trade rather than a bug.

## The diagnosis

| mechanism | eliminates | applied to | not applied to |
| --- | --- | --- | --- |
| determinator (BTD, RUE-1119) | work the diff cannot affect | the `platform-corpus` job, `pull_request` only | premerge, both native lanes, valgrind, asan, release, reproducibility |
| caching (`cached_corpus_suite`, RUE-1118) | work already done for this tree | 11 corpora | 75 of the 80 premerge targets, every native target |
| sharding (RUE-1116/1158) | wall time of work that must run | 1 corpus, fixed at 4 | everything else |

Each mechanism is sound. The composition is inverted. Sharding is permanent and
outermost; caching covers a middle slice; determination is innermost and
narrowest. It should be the exact reverse, because each stage is strictly cheaper
than the next and shrinks the next one's input:

```
determinate  ->  don't schedule what the diff cannot affect      (free)
     cache   ->  don't execute what this tree already executed   (near-free)
     shard   ->  spread only what genuinely must execute         (costs runners)
```

### The structural root cause

There are three different kinds of "unit of work" in this CI system:

1. **Buck actions** — cacheable, determinable, remotely executable.
2. **Buck test targets** — visible to the determinator, but not cacheable as
   configured, so they re-execute every run.
3. **CI steps** — hand-written target lists in YAML, opaque to all three
   mechanisms.

Every defect this investigation found lives in category 3 or 2:

- the native lanes' eight-target shell invocation (3) — the determinator cannot
  see it, caching does not apply, sharding is impossible. A 181.7s test hid there
  on two platforms;
- the 75 uncached premerge targets (2) — 440–463s of cache-immune work per run;
- `scaling-matrix-test` duplicating 813 of `rue-compiler-test`'s tests (2) —
  invisible because nothing compares test *contents*, only target lists.

## The architecture

### 1. One kind of unit, and CI never names one

Every piece of premerge work is a Buck target carrying its tier and its platform
requirement as attributes. **No workflow file contains a target label.** Every
lane is a generated selection over the live graph.

The repository already does this well for corpus cases — `only_on` plus
`RUE_PLATFORM_CASE_SELECTION=native` makes platform-scoped cases self-enrolling,
and `docs/process/ci.md` correctly calls that out as the reason new cases do not
need a workflow edit. Extend exactly that idea to unit targets, and the native
lanes become `attrfilter(labels, 'rue_platform_native', …)` instead of a list
that `validate-ci-gate.py` has to pin.

This also fixes the granularity mismatch. If platform need is per-test, the unit
must be per-test: split the host-conditional compiler tests into their own target
(the repository's own precedent — `rue-compiler-public-api-test`,
`rue-compiler-differential-oracle-test`, and `rue-compiler-payload-schema-test`
are already separate targets in the same crate), and let the native lanes select
that instead of the whole crate suite.

### 2. Determinate everything, not just the corpora

`affected-targets` already produces a fail-open decision, is already pinned by
`scripts/test-affected-targets.sh`, and already publishes JSON the matrix
consumes. Today only `platform-corpus` consults it; every other lane runs
unconditionally.

Extend it to every lane, and let it emit the lane plan rather than a
selection flag — `strategy.matrix: ${{ fromJSON(needs.affected-targets.outputs.lanes) }}`.
The coverage gate then becomes stronger *and* simpler than
`validate-cli-shard-coverage.py`: **the union of planned lanes equals the tier
selection from the live Buck graph**, fail-closed on any target in the graph and
in no lane. `merge_group` keeps forcing the full authoritative run.

### 3. Make test results cacheable, uniformly

`rust_test` provides `RunInfo` (verified with `buck2 audit providers`), so
`cached_corpus_suite`'s existing `_corpus_action` rule accepts a unit-test target
as its `harness` with no new machinery. The input contract is *tighter* than any
corpus already converted: the test binary's digest covers the entire Rust source
closure, because Buck built it.

Two things make this safe rather than hopeful:

- **Remote execution is the undeclared-input detector.** RUE-1222's item 3 worries
  that an undeclared action input becomes a silent false pass. RE materializes
  only declared inputs, so an undeclared read fails with file-not-found instead.
  RUE-320 is Done (2026-07-18, "full remote execution now default-capable"), and
  the stale instructions in `AGENTS.md:182` and the `./buck2` wrapper should be
  reconciled with `docs/process/build-cache.md`, which already describes it as
  supported.
- **Check the native path first.** buck2 carries
  `supports_test_execution_caching` on `ExternalRunnerTestInfo` and
  `disable_test_execution_caching` in the executor protocol. Rue configures
  `noop_test_toolchain` and `noop_remote_test_execution_toolchain`, so nothing
  honors them — `corpus.bzl`'s "OSS buck2 ships no test-result cache" is true *as
  configured*, not in general. If a real remote test-execution toolchain makes
  the native path work, it is one attribute instead of 75 conversions.

### 4. Shard last, to a measured floor, with an alarm

The objective is **minimize runners subject to the slowest lane staying under the
floor** — bin-packing under a capacity constraint, not makespan across fixed
bins. Only that objective can conclude "use fewer." The floor is measured (the
slowest lane that cannot be split), never chosen.

The load-bearing part is not the algorithm. **When the largest indivisible item
exceeds the floor, the planner must fail loudly, name the item, and refuse to
express the problem as a lane count.** A packer reasoning about totals would have
requested eight runners for the pre-fix premerge lane, where one item was 225.6s,
and reported success having changed nothing.

Its three remedies are all ones this repository already has:

| symptom | remedy | precedent |
| --- | --- | --- |
| one item dominates a lane | split it | CLI intra-target shards (RUE-1116) |
| the item re-executes every run | cache it | `cached_corpus_suite` (RUE-1118) |
| the item is heavier than pre-merge warrants | re-tier it | scaling matrix 100/1k vs 10k; large-example canary vs slow |

Choosing among them is a human call. Noticing is not — and that alarm would have
surfaced RUE-1262 on the day it landed, in three lanes at once.

### 5. The invariant nobody currently checks: nothing executes twice

This is the finding that generalizes furthest. Three duplications, none
detectable by any existing gate:

- `scaling-matrix-test` re-runs 813 of `rue-compiler-test`'s tests, in the same
  lane, to add three;
- `rue-compiler-test`'s 813 tests run on linux-x64, linux-arm64, and macOS, while
  the documented contract says the native lanes do not repeat the broad unit
  suite;
- `release-smoke` runs in the premerge lane under the debug platform and again in
  the `release` job under the release platform — arguably intentional, but nobody
  decided it.

Every CI gate here compares **target lists**. None compares **test contents**, so
a target that is a strict superset of another is invisible. The fix is a gate
that collects each lane's test identities (`--list` on the test binaries, which
is how the `scaling-matrix-test` superset was found — cheap and exact) and fails
when a test executes more than once per platform per run without a declared
reason.

That gate is what converts "we fixed this instance" into "this class cannot
recur," which is the difference between a faster CI system and a robust one.

## Ordering

1. **RUE-1262** — the pressure test and the `scaling-matrix-test` duplicate. It
   is 56–59% of the critical path, its scope is three lanes rather than one, and
   it needs no new machinery.
2. **The duplication gate** (§5). Cheap, and it is what stops the class from
   recurring. Doing it second means it lands while the evidence is fresh.
3. **Native-lane selection by attribute** (§1). Deletes the pinned target list
   and fixes the per-test/per-target granularity mismatch.
4. **Determinate every lane and generate the matrix** (§2). Largest workflow
   change; safest once §1 has removed the hand-written lists it would otherwise
   have to reproduce.
5. **Uniform test caching** (§3), after establishing whether buck2's native path
   works.
6. **Floor-aware packing with the indivisible-item alarm** (§4). Last, because
   until 1–5 land the packer would be optimizing a distribution that is about to
   change completely.

## Limits

- The end-state model scales measured CI step durations by locally measured
  target ratios. It assumes the native lanes' 99.1% concentration holds on ARM64
  and macOS as it does on x86-64; that is an inference from target composition,
  not a measurement on those hosts.
- The native lanes were profiled from step timings, not per-target. The step runs
  eight targets and the local composition is unambiguous, but a per-target
  measurement on real ARM64/macOS runners would confirm it directly.
- Nine runs over two days. Reliability and flake rate remain unmeasured; the
  weekly `correctness-repetitions.yml` workflow is the right source.
