# RUE-1250: the premerge critical path is one test function

Tracked as **RUE-1262**, split out of RUE-1250 because it is not a topology
change. This note is that issue's evidence.

Follow-up to `rue-1250-shard-topology-analysis.md`, which established that
`premerge (linux-x64)` — not the CLI shards — is the required-CI critical path
in 9 of 9 sampled runs, and left the lane's internal composition unmeasured.

This note measures it. The conclusion is that **no shard topology can improve
the premerge lane**, because a single test function is 81% of it, and that
function is compiled into two different targets that both run pre-merge.

## Method

`buck2` was run directly against the repository. The lane's membership is the
live Buck graph, not an inference:

```bash
buck2 uquery "attrfilter(labels, 'rue_test_tier_premerge', set(//... toolchains//...))"
# minus attrfilter(labels, 'rue_cli_shard', …) and 'rue_ci_dedicated_lane'
```

That is **80 targets**: 35 Rust unit tests and 45 root `sh_test` gates. Only
five are `cached_corpus_suite` action-backed suites; the other 75 re-execute on
every run, because OSS buck2 has no test-result cache (the premise of RUE-1118).

Each of the 80 was then timed individually with a warm build, and the whole lane
was timed as the single invocation CI actually issues.

**The measurement host has 4 cores — the same as `ubuntu-latest`** — so these
figures are directly comparable to CI rather than scaled. The host is faster
per-core (cold compiler build 78s here versus 286s on CI), so treat absolute
seconds as a lower bound and the *shares* as the result.

| measurement | value |
| --- | --- |
| serial sum of all 80 targets | 551.7s |
| lane as one parallel invocation (as CI runs it) | 268.0s |
| CI's warm premerge step, for reference | 440–463s |

## Finding 1: two targets are 81.4% of the lane

| target | seconds | share |
| --- | --- | --- |
| `//crates/rue-compiler:rue-compiler-test` | 225.6 | 40.9% |
| `//crates/rue-compiler:scaling-matrix-test` | 223.5 | 40.5% |
| `//crates/rue-oracle-diff:rue-oracle-diff-test` | 17.5 | 3.2% |
| `//:repository-quality-gates` | 12.2 | 2.2% |
| `//:tutorial-snippet-tests` | 11.8 | 2.1% |
| *all 45 root `sh_test` gates combined* | 56.9 | 10.3% |

The 45 validation and tool-test gates that dominate the lane's *target count*
are 10% of its cost. The 35 Rust unit tests are 89.7%, and two of them are
nearly all of that.

## Finding 2: the two big targets are the same test binary

`scaling-matrix-test` compiles the same sources as `rue-compiler-test` with
`--cfg=scaling_matrix`, which enables three `#[cfg(scaling_matrix)]` tests.
Listing both binaries:

```
rue-compiler-test:    813 tests
scaling-matrix-test:  816 tests
shared:               813
only in scaling-matrix-test:
  scaling_harness::scaling_matrix_fixed_bodies_growing_declarations
  scaling_harness::scaling_matrix_fixed_declarations_growing_bodies
  scaling_harness::scaling_matrix_identity_invariant
```

A strict superset. The premerge lane runs those 813 tests **twice** — 223.5s the
second time — to gain three tests.

The intent was already right: the BUCK comment says the target "remains a
separate target so `scaling_matrix` coverage is isolated from the default unit
target," and the isolation *is* achieved, by the cfg. What is missing is a
filter on execution. The stress sibling already has one —
`scaling-matrix-stress-test` passes `args = ["scaling_matrix", "--nocapture"]` —
and the premerge target passes nothing.

Filtering it to the three tests it uniquely owns, measured:

```
223.5s  ->  6.2s     (3 passed; 813 filtered out)
```

Note that `rust_test` does not accept an `args` attribute — `buck2` rejects it
outright — so this is a small structural choice, not a one-line edit. See
"Options" below.

## Finding 3: one test function is 81% of the remaining target

Timing `rue-compiler-test` by module isolates it to `pipeline_tests` (226.0s of
the target's 225.6s), and timing that module's 74 tests individually isolates it
further:

| test | seconds |
| --- | --- |
| `pipeline_tests::tests::failed_wide_batches_release_their_unpublished_child_cones_under_pressure` | **181.7** |
| `published_backend_root_keeps_wide_no_edit_and_single_edit_builds_exactly_warm` | 5.7 |
| `retained_object_projection_is_local_bounded_and_released_with_the_backend_root` | 5.5 |
| *remaining 71 tests* | 7.6 |

The runner-up is 32× smaller. Skipping this one test takes the whole binary from
225.6s to **12.7s** for the other 809 tests.

The test is not accidentally slow. It compiles `wide_reached_program(33, N)`
eighteen times under codegen-failure injection, deliberately, to drive real
query-family eviction pressure:

```rust
fail(&mut session, &failed_source);
let evictions_before_pressure = session.query_evictions_for_test();
for value in 101..117 {
    fail(&mut session, &wide_reached_program(CHAIN_FUNCTIONS, value));
}
assert!(
    session.query_evictions_for_test() > evictions_before_pressure,
    "failed child cones must face real query-family eviction pressure"
);
```

It is genuine coverage doing genuine work. It is also compiled into both
binaries above, so the premerge lane pays it **twice**: roughly 363s of the
lane's 551.7s serial cost, or 66%, is this one function.

## Finding 4: sharding cannot move this

Partitioning the 813 tests four ways by name hash and running each partition:

| partition | tests | seconds |
| --- | --- | --- |
| 0 | 202 | 3.4 |
| 1 | 184 | 6.8 |
| 2 | 225 | 7.2 |
| 3 | 202 | **227.0** |

Count-balanced sharding does nothing. Cost-balanced sharding — the RUE-1158
approach — cannot do better either: one item weighs 181.7s and the entire rest
of the binary weighs ~44s, so every assignment puts that item alone on the
critical path.

This generalizes to the whole lane. LPT's makespan is bounded below by the
largest indivisible item, so with a 225.6s target the lane cannot go below
225.6s on any number of runners. The measured parallel wall is already 268s —
within 19% of that floor. **Target-level fan-out has at most 16% left to give,
and it costs a runner per lane to collect it.**

## Options, in order of effect

| scenario | serial lane cost | largest item | 4-core LPT bound |
| --- | --- | --- | --- |
| today | 551.7s | 225.6s | 225.6s (measured wall 268s) |
| A — de-duplicate `scaling-matrix-test` only | 334.4s | 225.6s | **225.6s** |
| B — A, plus the pressure test leaves premerge | 121.5s | 17.5s | **30.4s** |

Read the middle row carefully: **de-duplication alone does not move the critical
path.** It frees 217s of CPU and a shard's worth of runner cost, but the surviving
copy of the pressure test still sets the floor. Only dealing with that one test
changes the lane's wall time — and then the lane becomes genuinely flat, with a
new largest item of 17.5s and no target above 20s.

### A. De-duplicate the scaling-matrix target

Because `rust_test` takes no `args`, either:

1. wrap it the way the stress tier already does — keep the `rust_test` as the
   binary and add an `rue_sh_test(test = ":scaling-matrix-test", args =
   ["scaling_matrix"])` as the premerge canary, moving the binary target itself
   out of the premerge tier so `//...` does not run all 816; or
2. move the three `scaling_harness` matrix tests into their own `tests/`
   integration target, so the binary contains only them and the cfg trick is no
   longer needed.

Option 2 is cleaner and removes a cfg-gated-superset pattern that is easy to
regress into. Option 1 is smaller and matches existing precedent. Either is a
maintainer call about where RUE-1086's coverage lives; both are behavior-neutral
for what actually gets tested, since all 813 shared tests already run in
`rue-compiler-test` in the same lane.

### B. Give the pressure test the treatment this repository already uses twice

A 181.7s eviction-pressure test in the pre-merge unit suite is the same shape as
two problems this repository has already solved:

- the scaling matrix keeps a bounded 100/1k premerge canary and puts the real
  10k ladder in `stress`;
- the large examples keep reduced `-canary` roots in premerge and the real
  programs in `slow`.

The same split applies: keep a bounded premerge canary that proves eviction
happens at all (a smaller `CHAIN_FUNCTIONS`, or 3–4 pressure iterations instead
of 16), and move the full 18-compile ladder to `slow`, where the oracle
differentials already live and where a regression surfaces on the same day.

This is a coverage decision, not a cleanup, and it belongs to whoever owns
RUE-1083/RUE-1086. What the measurement establishes is only its price: this one
test is currently two-thirds of the pre-merge unit suite's cost, and the merge
queue serializes on it.

#### Update (2026-08-11): the price collapsed, and the ruling went the other way

The figures above stand as measured, but they no longer describe trunk. Same
host, same default configuration, `rue-compiler-test`:

| | at `137e3f60` (2026-08-09) | at `18def29` (2026-08-11) |
| -- | --: | --: |
| the pressure test alone | 146.09s | 1.66s |
| whole binary, parallel | 154.53s | 19.93s |
| whole binary, serial | 196.78s | 60.24s |

The collapse is not from this note's scope A or B, and it is not specific to
this test. It is general compiler perf work landing: a sibling test that
compiles the same 34-function chain across several revisions but has **no**
eviction-pressure loop moved 10.22s → 0.60s over the same interval, while
sibling tests that only compile one-function programs did not move
(1.31s → 1.07s, 0.29s → 0.30s). The win is on repeated revisions of a wide
program in one long-lived session, which is why this test — 19 wide revisions —
shows it most extremely. It came from the trunk work between those two commits,
which includes several query-runtime changes (RUE-1247 and RUE-1343 through
RUE-1349); it has not been bisected to a single commit.

Note that a whole-suite subtraction hides this: the rest of `rue-compiler-test`
went 50.69s → 58.58s across the same interval, because nearly all of those ~810
tests compile toy programs where there is nothing to win.

The premise of the split proposed above — that this test is two-thirds of the
pre-merge unit suite — is gone, and with it the case for a `slow` variant: the
big variant would run identical assertions on an identical program for no
additional coverage, which is what separates it from the scaling-matrix and
large-example precedents cited above.

The ruling recorded in RUE-1262 and in the test itself is therefore to reduce
the loop in place, from 16 iterations to a constant derived from the family
retention bound, and to state in the test what the iterations buy. The eviction
this test needs is driven by a per-family retained-*count* watermark
(`BODY_QUERY_MEMO_RETENTION = 8`) against 34 terminals per compile, so it is a
property of the graph rather than of the host, and one compile already clears
it four times over. That also bounds the blast radius if the per-compile
constant ever regresses again: 5 compiles rather than 19.

##### The lane is no longer governed by one item

`scripts/rue premerge`, warm, on the same 4-core host: 113.9s wall, 199.7s
serial across 144 targets. (That denominator is not this note's 80 — the script
also runs the per-crate fmt and clippy gates.) The head of the distribution:

| target | seconds | share of serial |
| -- | --: | --: |
| `//crates/rue-oracle-diff:rue-oracle-diff-test` | 37.4 | 18.7% |
| `//crates/rue-compiler:rue-compiler-test` | 25.5 | 12.8% |
| `//crates/rue-compiler:scaling-matrix-test` | 18.4 | 9.2% |
| `//:tutorial-snippet-tests` | 15.7 | 7.9% |
| `//crates/rue-fuzz:rue-fuzz-test` | 12.7 | 6.4% |

The four-core LPT bound is now `max(37.4, 199.7/4) = 49.9s` — set by aggregate
work, not by the largest indivisible item, which is the end state this note's
"Projection" predicted. The gap between that bound and the 113.9s measured wall
is scheduling and build serialization, so any further premerge work is a packer
question (RUE-1267), not a single-test question.

This is warm and local. It says nothing about the cold compiler build inside
the CI premerge job, which the ADR-0069 sample put at 354–393s in 3 of 9 runs;
on CI that, not any test, is the likely binding constraint now.

## What this means for RUE-1250

The sharding question is answered in the negative for premerge. Do not add lanes
to it. After A and B the lane is ~30s of well-distributed work and the case for
any premerge fan-out disappears entirely.

It also settles the CLI shard count, by removing the reason to move it. An
earlier draft of the first note proposed reducing CLI shards 4 → 2 and spending
the recovered runners on premerge halves. Both halves of that trade are wrong:
premerge's parallel wall is already within 19% of its indivisible floor, so the
runners would buy ~16%; and once premerge stops masking it, the floor that
governs the CLI corpus is the native ARM64 lane at 341–407s, which two shards
(450–550s) breach in 4 of 4 cold runs. The first note now records **keep four**.

Its other conclusions stand: the balance guard should measure outcomes rather
than the model, the weights should not drift undetected, and the count should be
derived rather than replicated by hand. But the ordering changes — the two fixes
above are worth more than every topology change in that note combined, and they
cost no extra runners.

## Adjacent findings

- **RUE-320 is Done** (2026-07-18, "full remote execution now default-capable"),
  but `AGENTS.md:182` still instructs agents not to use `--prefer-remote` "while
  RUE-320 remains open", and the `./buck2` wrapper's comment still cites it as
  open when forcing `--prefer-local`. `docs/process/build-cache.md` already
  describes remote execution as supported. The two stale references should be
  reconciled with the third.
- **Buck has a native test-caching contract that this repository does not use.**
  `ExternalRunnerTestInfo` carries `supports_test_execution_caching`, and the
  executor protocol carries `disable_test_execution_caching`. Rue configures
  `noop_test_toolchain` and `noop_remote_test_execution_toolchain`, so nothing
  honors them — `corpus.bzl`'s "OSS buck2 ships no test-result cache" is true
  *as configured*. Before generalizing RUE-1118's stamp pattern to the other 75
  targets, it is worth establishing whether a real remote test-execution
  toolchain makes the native path work; that would be one attribute instead of
  75 conversions.
- `rust_test` provides `RunInfo` (confirmed with `buck2 audit providers`), so if
  the stamp pattern is generalized after all, `cached_corpus_suite`'s existing
  `_corpus_action` rule accepts a unit-test target as its `harness` with no new
  machinery, and with a tighter input contract than any corpus — the test
  binary's digest already covers the whole Rust source closure.

## Limits

- Single host, single sample per target; no repetitions, so small targets carry
  ordinary noise. The two findings that matter are 20–30× above that floor.
- Per-target figures were measured serially, each target having all four cores.
  The parallel wall (268s) was measured separately and is the number to compare
  against CI.
- Scenario B's 12.7s figure comes from `--skip` on the existing binary, not from
  an actual re-tiering, which would also change what the binary compiles.
