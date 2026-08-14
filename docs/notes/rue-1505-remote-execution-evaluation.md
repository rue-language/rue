# RUE-1505: remote execution evaluation — measurements

Evidence for ADR-0069 Amendment 1. The amendment records the decision; this file
records how each number was obtained, its population, and its caveats.

All figures come from GitHub Actions job logs and the Actions API for
`rue-language/rue`. Population: **500 `CI` runs**, 2026-08-10T13:39Z to
2026-08-14T15:32Z (4.08 days), and roughly 1,100 job logs. Nothing was measured
on a developer host, so local contention is not a confound for any figure here.

## What could not be measured, and why

There is no BuildBuddy credential reachable from a developer host. The key
exists only as the `BUILDBUDDY_API_KEY` repository secret, and GitHub does not
return secret values through its API. Consequently:

- local `--prefer-remote` runs were impossible;
- BuildBuddy's invocation UI and API — the natural source for an input-upload /
  queue / execute / output-download breakdown — were unreachable;
- **account usage against plan caps could not be read at all.** Every
  consumption figure below is buck2's *client-side* accounting, not BuildBuddy's
  billing.

Publishing a temporary workflow to measure the full premerge closure under RE
would have required pushing, which was out of scope. **No measurement of the
full premerge closure under RE exists**; every statement about premerge under RE
is inference from the closure's measured composition, and is marked as such.

## The controlled pair

Required CI already runs the experiment on every merge:

| lane | builds | executor | cache |
| --- | --- | --- | --- |
| `remote execution (linux-x64)` | `//crates/rue:rue` | `--prefer-remote`, `//platforms:remote_execution` | `--no-remote-cache` |
| `compiler reproducibility (linux-x64)` | `//crates/rue:rue`, twice | `--local-only` | `--no-remote-cache` |

Same commit, same run, same `ubuntu-latest` runner class, started within seconds
of each other, both genuinely cold. Verified: identical target-configuration
hash, and per-run action counts matching at 396 or 405 on both sides.

Two caveats belong with every ratio derived from this pair.

1. **The "local, full parallelism" number is inferred, not measured.**
   `scripts/rue-bin` swallows buck2's stderr, so no `BUILD SUCCEEDED` timestamp
   exists for the reference half. The available quantity is a step-gap that also
   contains the tool loop, `git archive | tar -x`, and `find … -exec touch`, all
   charged to the local side. The local figure is therefore an over-estimate,
   and every local-÷-remote ratio an over-estimate of RE's advantage.
2. **The two are not exec-configuration-identical.**
   `platforms/remote_cache.bzl` inserts `root//constraints:remote-execution`, and
   `toolchains/BUCK` selects linker `cc` on it. This is visible in the
   attribution below as `rue:rue rustc link` at 3.2s remote against 0.1s local.
   The difference is deliberate — ADR-0070's negative control depends on it —
   but it means the pair is "the same actions", not "the same commands".

## 1. Cold-build wall time

Buck2 daemon start to `BUILD SUCCEEDED`. The canary ran **212** times in the
window; 210 produced parseable timings.

| build | n | p25 | p50 | p75 | p90 | max | cv |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| remote | 210 | 45.6s | **51.5s** | 58.9s | 80.1s | **245.3s** | **50%** |
| local, full parallelism | 212 | 72.4s | **74.2s** | 76.8s | 79.6s | 83.4s | **8%** |
| local, `--num-threads 2` | 212 | 82.4s | 84.1s | 87.2s | 91.0s | 97.4s | 8% |

Paired per-run ratio (local ÷ remote): min **0.31×**, p25 1.26×, p50 **1.4×**,
p75 1.66×, p90 1.79×, max 2.0×.

**Toolchain discount.** The local build fetches the pinned rustc and Zig
`http_archive` payloads (183–236 MiB) to the runner; the remote build executes
those actions on the worker. Measured `http_archive` occupancy is 7.2s per local
run against 0.8s per remote run. Netting the 6.4s difference gives 67.8s against
51.5s — **≈1.3×**, which is the honest like-for-like figure. The download is a
real CI cost, but it is a runner-provisioning cost, not evidence about execution
parallelism.

The durable result is the variance asymmetry: **RE's coefficient of variation is
six times local's**, and its worst case is 4.8× its median against local's 1.1×.

## 2. Where the time goes

Per remote build (n=210): **Up p50 3.7 MiB, Down p50 3.4 MiB**, maxima 17 MiB
each. RE session establishment 1.3–1.9s; roughly 8s of loading and analysis
before the first action dispatches. Input transfer is not the constraint and
would not become one — the toolchain payload is already CAS-resident, so
`FindMissingBlobs` keeps steady-state upload to single-digit MiB.

**The tail is a recurring pattern, not an incident.** The 15 slowest remote
builds span 2026-08-10 through 2026-08-13 across **18 distinct hours**, and
**27 of 210 (12.9%) exceed 90s — slower than the slowest local build observed.**
The distribution concentrates daily around ~17:00–23:00Z. Network figures on
slow builds are unremarkable (1.4–11 MiB), and tracing the slowest shows no
stall: every dependency stage is uniformly stretched, with a single
`rue-perf-schema` rlib occupying 21s. Individual remote actions run slower when
the shared pool is busy.

## 3. What RE actually accelerates

Per-action attribution across all 212 canary and 212 reproducibility runs,
seconds of occupancy per run:

| action | remote | local | local ÷ remote |
| --- | ---: | ---: | ---: |
| `rue-compiler` rustc rlib | 8.1 | 7.9 | **0.98** |
| `rue-compiler` rustc metadata | 4.9 | 3.9 | **0.79** |
| `rue:rue` rustc link | 3.2 | 0.1 | **0.03** |
| `winnow` rlib (41 queued locally) | 0.0 | 1.4 | 40.8 |
| toolchain `http_archive` | 0.7 | 6.3 | 9.0 |

**RE ties or loses on every critical single action and wins only where actions
queue behind the local executor.** Its advantage is width, not per-action speed.
This is the direct refutation of "a BuildBuddy worker might run the 126s
indivisible corpus action faster than the runner does": on the evidence, it
would not.

## 4. Composition of the cold premerge build

Over the **169** cold-compiler `pull_request` runs in the window, `Build all
targets` ran p25 275s / p50 294s / p90 322s, issuing 849–879 commands of which
~91% were cache-served and only ~76 executed locally. Attributing wall time to
the target buck2 reported waiting on:

| target | mean | share |
| --- | ---: | ---: |
| `//crates/rue-oracle-diff:oracle-diff-test-action` | 138.2s | **50.6%** |
| `//crates/rue-oracle-diff:oracle-diff-spec-test-action` | 77.1s | **28.3%** |
| `//crates/rue-compiler:rue-compiler-test` | 19.4s | 7.1% |
| `//crates/rue-compiler:rue-compiler` | 13.1s | 4.8% |
| `//crates/rue-air:rue-air-test` | 12.5s | 4.6% |
| every third-party crate, together | 0.4s | **0.13%** |

**Two single actions are 78.9% of the cold premerge build.** They are one action
each — `cached_corpus_suite` genrules (RUE-1118) running a whole harness — so
remote execution cannot subdivide them, and §3 shows it would not run them
faster. The part of the graph RE demonstrably accelerates is 0.13% of the step.

## 5. Concurrent duplication of the oracle-diff corpora

`premerge (linux-x64)` and `test (linux-x64-oracle-diff)` **both execute
`oracle-diff-test-action` cold, concurrently, at the identical execution
configuration** `prelude//platforms:default#5c1b01ec01a662a2`, on two runners.

Over 60 cold-compiler `pull_request` runs: premerge's window on the action is
155s median, the dedicated lane's is 148s median, and the **median wall-clock
overlap is 75s, present in 57 of 60 runs**. The logs show both sides going
`local_execute` → `upload (action)` — they are racing, not one serving the
other.

Two corrections to the natural reading of this:

- **Counting actions hides it.** The lane reports `Commands: 347 (cached: 342,
  local: 5)`; one of those five is the 148s harness. A 99% hit rate and a
  two-and-a-half-minute duplicated execution look identical in that counter.
  This is the error ADR-0069 §5 exists to forbid.
- **Prior runs are not the explanation.** Every `head_sha` in the 600-run set is
  unique, so no earlier run warmed the changed crate. Sibling jobs warm each
  other in real time when their windows happen not to overlap; when they start
  together, both pay.

The aggregate picture for other siblings still holds — on cold compiler PRs
`valgrind` executes 3 local actions, `release` 16, `native (linux-arm64)` 19,
and `compiler reproducibility` 396 by design. But "concurrent duplication does
not happen here" is false, and the largest item in premerge is where it happens.

## 6. The premerge / dedicated-lane scope defect

`crates/rue-oracle-diff/BUCK` declares both suites `tier = "slow"` with
`labels = ["rue_not_quick", "rue_heavy_suite"]`, and `.github/workflows/ci.yml`
gives each its own lane. **Premerge builds them anyway**, on both branches of
its build step:

- unnarrowed, `./buck2 build //crates/...` reaches them directly;
- narrowed, `scripts/affected-targets`' `build_scope()` filters the impacted
  list with `grep '^//crates/'`, and these targets live at
  `//crates/rue-oracle-diff:…`, so they pass the filter.

The comment directly above `build_scope()` explains that the filter exists
because building a `cached_corpus_suite` action *runs its corpus*, and that this
took premerge from ~12m to 32-42m. That fix removes root-level (`//:`) corpus
actions only. These two are crate-level and sail through both branches.

Worth **~240s at the median** — against a best case of ≤30s for routing the same
step through RE.

## 7. Reliability

The canary succeeded in **210 of 212** merge-group runs — **0.94% failure**,
against 0.9% for `premerge` on `merge_group` and 6.8% on `pull_request`.

Both failures were the same thing and neither involved BuildBuddy: the "Build
compiler remotely" step failed because `dotslash` could not download buck2 from
the GitHub releases CDN, before the RE endpoint was contacted. **Zero
BuildBuddy-attributable failures in 212 runs.**

That is a favourable result for the canary and a misleading one for adoption,
because the canary is not exposed to what a required lane would be:

- `//platforms:remote_execution` sets `allow_hybrid_fallbacks_on_failure =
  False`; a failed remote action is never retried locally. Correct for a canary
  whose purpose is to refuse to hide a worker regression; an outage amplifier on
  a required lane.
- The forgiving `//platforms:remote_cache` platform allows fallback *on action
  failure*, which is not the same as tolerating an unreachable or slow endpoint.
- **Fork PRs have no secret, hence no `.buckconfig.local`, hence no RE endpoint
  or credential.** Today `scripts/provision-build-cache` treats an empty key as
  "skip" and the lane builds cold and locally. An RE-by-default premerge needs a
  second, conditional execution path — the least exercised and most depended
  upon code in required CI.

## 8. Cost against the free tier

Sources, fetched 2026-08-14 from BuildBuddy's published pricing page. The
Personal (free) plan, "For small teams and open source projects", states
**"100 GB of cache transfer"**, **"Up to 80 cores for remote builds"**, and
community support. Team states "Up to 800 cores" and "$X / GB of cache transfer
over 100 GB" — rendered with a literal `$X`. Their FAQ states: *"We don't apply
hard limits that prevent you from using more than your plan allows. If you have
a big temporary burst of usage, feel free"*, with sustained overage prompting an
upgrade conversation.

**What happens when a cap is hit is therefore nothing technical** — not
throttling, not hard failure, not a silent stop-caching. It is a commercial
conversation.

Not verifiable, and not to be presented otherwise:

- the period the 100 GB is measured over is **not published**;
- the Team per-GB overage rate is **not published**;
- **current account usage against any cap could not be read** — no credential,
  no dashboard, no API.

### The inference-free figure

Measured across 16 complete runs, buck2's client-side network per run:

| event | mean up | max up | mean down |
| --- | ---: | ---: | ---: |
| `pull_request` | **0.38 GiB** | 0.60 GiB | 3.66 GiB |
| `merge_group` | 0.01 GiB | — | 1.20 GiB |

**Upload cannot be confused with `http_archive` traffic — it is unambiguously
CAS.** At the measured rate of 70.6 `pull_request` and 52.0 `merge_group` runs
per day, upload alone is **≈28 GiB/day ≈ 850 GiB/month: 8.5× the published
100 GB allowance, with zero inference.**

Including download — which conflates CAS with the 183–236 MiB toolchain fetches
per buck2-running job, and is therefore an upper bound — gives ~3.7 TB/month, or
37×.

### Two reconciliation caveats

- **The window is Monday to Friday with no weekend**, so a ×30 extrapolation
  presents a weekday rate as a calendar rate. On ~22 active days the
  download-inclusive figure is ~2.7 TB/month — still 27×.
- **The run-rate comparison to RUE-1505 is apples-to-oranges if taken as
  122.6 vs ~50.** The issue said ~50 *`pull_request`* runs/day; the comparable
  measured figure is **70.6 PR runs/day, a factor of 1.4**, not 2.5. The 122.6
  figure is all CI runs of both events.

### What this means

The conclusion is not "RE would push us over the free tier". It is that **the
repository is already drawing on BuildBuddy's goodwill an order of magnitude
beyond the published allowance, on the cache alone, before any RE change** — and
nothing has broken, consistent with the vendor's stated no-hard-limits policy.

RE's own transfer is negligible (~7 MiB/build) and would barely move that
number. What would rise is core-hours against the 80-core figure, and §2 shows
the shared pool already visibly contended at today's essentially zero RE usage.

## Reproducing this

Every figure derives from two API surfaces and no privileged access:

```bash
gh api /repos/rue-language/rue/actions/workflows/ci.yml/runs   # run population
gh api /repos/rue-language/rue/actions/runs/<id>/jobs          # job + step times
gh api /repos/rue-language/rue/actions/jobs/<id>/logs --allow-escape-sequences
```

Job logs carry per-line timestamps, and `scripts/ci-timed` tees buck2's own
output, so `Commands: N (cached: C, remote: R, local: L)`, `Network: Up/Down`,
and the `Waiting on <target>` heartbeats are all recoverable per run. Occupancy
attribution charges each heartbeat interval to the target named at its end.
