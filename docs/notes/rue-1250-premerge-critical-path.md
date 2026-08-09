# RUE-1250: the premerge critical path is one test function

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
