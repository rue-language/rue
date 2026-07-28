---
id: 0067
title: "Compiler performance measurement, epochs, and dashboard"
status: proposal
tags: [tooling, ci, performance, website]
feature-flag: null
created: 2026-07-28
accepted:
implemented:
spec-sections: []
superseded-by:
supersedes: [0019, 0031]
relates: ["ADR-0018", "crates/rue/src/timing.rs"]
---

# ADR-0067: Compiler performance measurement, epochs, and dashboard

## Status

Proposal. This ADR accompanies the benchmarking reset: the previous runner
(`bench.sh`), the `scripts/benchmark_*.py` package, the stored corpus and
annotation file, and the old dashboard have been removed, and this document
defines what replaces them. It is written against the post-reset tree.

## Summary

Build a small compiler-performance system on four decisions: compiler-emitted,
mutually exclusive wall-clock phase accounting that sums exactly to
compiler-root time in the raw representation; a two-layer versioning model
(suite revisions and platform epochs) that makes configuration-invalid
observations unappendable rather than detected; raw-observation storage on a
fresh content-addressed Git branch; and a dashboard whose headline is a
normalized per-workload index, with absolute phase composition shown per
selected workload. The system's job is to show that compiler-build
performance is going wrong and where. History begins fresh: no legacy reader,
no compatibility path.

## Scope

Fresh compiler-build performance only: latency, peak memory, and output binary
size of complete compilations, with wall-clock phase attribution. A **fresh
build** is a newly launched compiler process with no retained compiler-session
state. Operating-system state (filesystem and page cache) is not controlled,
so measurements are not called "cold." Everything else is deferred (see Future
Work).

## Context

The removed system had three structural defects that motivated a reset rather
than a repair.

**The headline was composition-dependent.** It summed per-workload compile
times across an unevenly weighted corpus (the largest probe contributed ~29%
of the total, the smallest ~9%), so it moved whenever corpus membership or
content changed, for reasons unrelated to the compiler.

**Comparability was classified instead of guaranteed.** Because the headline
was fragile, the system decided after the fact whether two points could be
compared (`regime_changed`, `workload_composition_changed`, and four more
boundary reasons) and broke the chart at each boundary. The classification
logic, its metadata, its rendering, and the prose explaining it were permanent
costs, and the primary chart was frequently too fragmented to show a trend.

**Phase charts stacked quantities that are not additive.** The timing schema
(`crates/rue/src/timing.rs`) records span durations that are inclusive and may
overlap their parents; only aggregate duration and leaf invocation counts are
kept per span name; parent edges are test-only; and codegen subphases run on
Rayon workers, so spans overlap in wall time across threads. The root total is
already computed as a union of active intervals precisely because summing
spans double-counts. Truthful additive attribution must be produced by the
compiler, not reconstructed from span names downstream.

## Decision

### 1. Two timing models, kept distinct

The compiler publishes two kinds of phase measurement:

**Wall-clock phase accounting** — a small set of explicitly instrumented,
mutually exclusive top-level phases whose sum equals compiler-root elapsed
time. Only these appear in additive visualizations. The initial published set:

- source discovery and parsing
- program construction
- semantic analysis
- CFG and optimization
- backend (instruction selection, register allocation, emission)
- object generation
- linking

plus two structural buckets: `mixed_parallel` and `unattributed`.

**Inclusive tracing spans** — the existing span machinery, unchanged. Spans
nest and overlap; they appear in tables, per-span detail views, and
flame-style investigation displays, never in an additive stack.

Changing the published phase set is a timing-schema change and therefore a new
suite revision (§3).

### 2. Phase accounting state machine

The timing collector maintains a **reference count per published phase**:
entering a span that belongs to a phase increments that phase's count, and
exiting decrements it. Multiple concurrent spans of the same phase are
expected (Rayon workers), and a phase is active exactly while its count is
greater than zero; the set of active phase names is derived from the counts,
never tracked independently. Every interval of compiler-root wall time is
partitioned into exactly one bucket:

- exactly one phase has a positive count → that phase;
- more than one distinct phase has a positive count → `mixed_parallel`;
- root active with all phase counts zero → `unattributed`;
- root inactive → excluded from compiler-root phase totals.

Immediately before every root or count transition between zero and nonzero,
elapsed time since the previous transition is charged to the bucket determined
by the previous state. Timestamps are sampled under the same lock that orders
root transitions today, so intervals are well-ordered.

**Raw durations are integer nanoseconds.** Because the state machine
partitions a single timeline, the invariant holds exactly, not approximately:

```
sum(phase_ns) + mixed_parallel_ns + unattributed_ns == compiler_root_ns
```

Milliseconds and percentages are derived at presentation time only.
Validation asserts the exact equality per sample. A sample that violates it
is invalidated: it is excluded from medians, dispersion, and all derived
publication, and is stored in the raw run object with a structured
invariant-failure record. It is evidence of an instrumentation bug, not a
discarded measurement.

Runs record both `compiler_root_ns` (the additive stack's total) and
`process_elapsed_ns` (externally measured). Their difference — process
startup, output publication, and other driver overhead — is real time the
user experiences and is reported, but it is outside the phase stack.

`mixed_parallel` and `unattributed` are published bands, not artifacts.
Sustained growth in `mixed_parallel` means phase boundaries no longer describe
the parallel structure and should be redrawn; growth in `unattributed` means
compiler time is moving somewhere instrumentation does not describe.
Fractional attribution of parallel work across phases is explicitly rejected:
if the partition is unsatisfying, fix the phase boundaries, do not invent
weights.

### 3. Suite revisions and platform epochs

Versioning has two layers, so logical facts are declared once and everything
that can vary by platform lives with the platform.

A **suite revision** pins the platform-independent logical contract:

- logical workload membership and source identity (which workloads exist and
  what program each one is);
- the published phase taxonomy and timing schema version;
- runner protocol semantics (what a sample is, how batching is defined, what
  a run object contains).

A **platform epoch** pins, per platform:

- the suite revision it implements;
- the target and the complete compiler invocation (optimization level, linker
  mode, feature flags, and any behavior-affecting arguments);
- the resolved transitive content hashes: each workload's full source
  closure, the standard library, and the toolchain as built for this target;
- the per-workload sampling and batching policy;
- the environment policy: runner environment class and image label
  (e.g. `github-hosted`, `ubuntu-24.04`);
- the headline baseline: the first **complete, valid** run at a declared
  trunk revision, whose per-workload medians define ratio 1.0. An attempted
  or partial run is never a baseline.

A change to what a workload *is* creates a new suite revision and therefore
new epochs on every participating platform. Raising only the macOS sample
count, or a macOS environment change, creates a new epoch only there.
Validation refuses to append a run whose pinned components do not match its
epoch; a maintainer creates the next suite revision or epoch deliberately.
The dashboard renders epochs as separate continuous stretches with labeled
boundaries — honest discontinuities, never manufactured continuity. Epoch
splicing via shared workloads is out of scope for version one.

**Environment fingerprints.** The epoch pins the environment *policy*, not
the exact machine. Every run records an environment fingerprint: runner
label, runner image version (as exposed by the hosted runner, e.g.
`ubuntu24/20250720.1.0`), CPU model, core count, memory, kernel and OS
version, and architecture. A fingerprint change within an epoch produces a
structural environment annotation on the dashboard; comparisons crossing it
are rendered as advisory rather than used to declare a regression. This is
the honest statement for hosted hardware: points in a series satisfy the
epoch's declared environment policy; exact environment changes are recorded,
not prevented. Choosing hosted runners buys this limitation; identity cannot
eliminate it. If measurement moves to controlled hardware, the policy can
tighten to exact-fingerprint pinning as a per-epoch choice.

### 4. Series identity

Within a platform epoch, a series is `(epoch, workload, metric)`. The suite
revision and epoch pin everything else that could invalidate comparison. The
compiler revision is a point within a series, never part of its identity. A
workload edit changes its source identity, fails validation, and therefore
requires a new suite revision — a workload can never silently reset its own
baseline while the headline continues.

The guarantee, stated precisely: observations that are invalid for a series'
suite revision or platform configuration cannot enter it; environment
variation within the epoch's policy remains possible, is recorded via
fingerprints, and makes the affected comparisons advisory.

### 5. Sampling and noise

The platform epoch pins a per-workload sampling policy: sample count, and for
very short workloads (the startup probe), a batching factor — K compiles
measured as one sample — so that timer resolution and per-process jitter do
not dominate. No sample count is fixed by this ADR; initial values come from
the unpublished calibration phase (Phase 4) and live in the epoch, where
changing them is a visible epoch event.

All raw samples are stored. Published statistics are the median and a robust
dispersion measure (MAD-based) per workload per run.

**Regression flagging rule.** The status line flags a workload only when the
difference between its current median and the trailing-window median exceeds
`k` times the pooled uncertainty of the two, where the pooled uncertainty
combines the current run's dispersion with the dispersion of the trailing
window's medians. The multiplier `k` and the window length are set during
calibration and pinned by the epoch. Points inside that bound render as
uncertain, not as movement. Paired base/candidate execution, effect sizes,
and formal significance testing are out of scope.

### 6. Headline index

The headline for a platform epoch at commit *C* is the geometric mean, over
the suite's workloads, of `median_ns(C) / baseline_median_ns`. It is a
dimensionless index: 1.00 at the epoch baseline, lower is faster. Equal
weighting in log space prevents corpus mass from weighting the aggregate, but
it attenuates single-workload signals: a 10% regression in one of *n*
workloads moves the index by roughly 10%/*n*. The full-size signal is the
per-workload small multiple; the index exists to answer "is anything moving,"
not "how much did it move." Peak memory and binary size receive the same
per-workload treatment with their own indexes.

The cohort and configuration behind the index cannot change silently; noise
and host drift within the environment policy still move individual
observations, which is what the dispersion machinery of §5 exists to absorb.

The index is not a wall-clock quantity and is never drawn as, or stacked
with, milliseconds. Platform indexes have independent baselines and are never
numerically compared with one another; cross-platform absolute measurements
remain available in the workload details.

### 7. Partial runs

A run is always stored, whatever happened: completed workloads contribute
valid samples, and failures — workload crashes, validation rejections,
phase-sum invariant violations — are recorded as structured evidence, not
discarded. Publication is tiered:

- a per-workload observation is published for every workload that completed
  validly;
- a headline point is published only when every suite workload completed
  validly;
- when the latest run is incomplete, the dashboard shows an explicit
  collection-health warning naming the failing workloads.

Fixed headline membership is preserved without throwing away evidence, and a
persistently broken workload is visible rather than an unexplained hole.

### 8. Storage: fresh orphan branch, raw data only

Raw observations live on a new orphan branch, `performance-data-v1`, with a
deliberately tiny contract:

```
index.json
runs/<content-hash>.json
```

A run object contains complete raw samples in integer nanoseconds (including
partial-run failure records), full identity (suite revision, epoch, platform,
commit, environment fingerprint, timestamps), and the phase accounting for
every sample. The filename hash is the SHA-256 digest of the canonical
serialized run object — canonical JSON with sorted keys and integer raw
values, so hashing involves no floating-point formatting. `index.json` only
points to run objects and their series. Everything derived — indexes,
dispersion, chart data, summaries — is rebuilt from raw records at site build
time and never stored on the data branch: no SVGs, no HTML, no derived JSON.
A single non-cancelling writer concurrency group serializes index updates;
run objects are immutable. Collection uses the repository write token — no
external service and no separately managed credential.

The legacy `perf` branch remains untouched as a historical artifact. No code
in the new system reads it.

### 9. Runner

One Rust binary (`crates/rue-bench`). It reads the manifest, builds the
compiler, runs the corpus under the epoch's sampling policy, captures
`process_elapsed_ns` and peak memory externally alongside the compiler's
phase JSON, validates the phase-sum equality and epoch pins, records failures
as structured evidence, and emits one immutable run object. The same binary
runs locally and in CI with the same run-object schema, so any recorded run
is locally rerunnable under the recorded protocol. (Rerunnable, not
reproducible: hosted-hardware timings do not repeat byte-for-byte.)

### 10. Corpus and annotations

The new corpus lives under `performance/`: `performance/manifest.toml`
declares suite revisions and workloads, and workload sources live in
`performance/workloads/` (referencing `examples/` programs where
appropriate). The initial corpus is small: a startup probe, one
representative medium multi-module program, `examples/caldera`,
`examples/meridian`, and a small number of independent scaling probes that
each name the question they answer. Growing the corpus is a new suite
revision — cheap, and never silent.

Authored annotations live in `performance/annotations.toml` (commit-,
metric-, workload-, and platform-scoped), reviewed through ordinary PRs.
Annotations explain; they never override identity or repair a comparison.
Derived environment annotations (§3) are computed from run fingerprints, not
authored.

### 11. Dashboard

In order down the page, per selected platform:

1. **Status line.** One sentence: current index, change versus the trailing
   week, any workload flagged under the §5 rule, and the collection-health
   warning when the latest run was incomplete.
2. **Headline chart.** The normalized index as a continuous line per epoch
   stretch, with authored annotations, environment annotations, and labeled
   epoch boundaries.
3. **Selected-workload phase chart.** Stacked area of absolute milliseconds
   by published phase (including `mixed_parallel` and `unattributed`) for one
   workload at a time; selection driven by the small multiples.
4. **Composition bar.** One horizontal stacked bar for the selected commit,
   following the cursor over the phase chart; shares its colors and serves as
   its legend.
5. **Per-workload small multiples.** One sparkline per workload with latest
   median, dispersion, and delta — the full-size regression signal.
6. **Field notes.** Authored and derived annotations in the visible window.
7. **Disclosures.** Inclusive-span detail, memory and binary-size indexes,
   driver overhead (`process_elapsed_ns` minus `compiler_root_ns`), raw
   records, and a raw-data download.

Tooltips are required wherever a point highlights: commit short hash and
subject, hovered band and value, total, delta versus the previous measured
commit, and commits-since-last-measurement when runs were skipped. Clicking
pins the tooltip and links to the commit. Tooltip content is
keyboard-reachable and exposed to assistive technology.

## Implementation Phases

- [ ] **Phase 1: Measurement schema** - RUE-NNN. Suite revisions, platform
      epochs, series identity, run-object schema, canonical serialization and
      SHA-256 content addressing, validation rules.
- [ ] **Phase 2: Compiler phase accounting** - RUE-NNN. The reference-counted
      state machine of §2 in the timing collector, published alongside
      existing spans; measurement-boundary tests including Rayon-parallel and
      same-phase-concurrent workloads; exact nanosecond invariant under test;
      `compiler_root_ns` / `process_elapsed_ns` distinction.
- [ ] **Phase 3: Runner** - RUE-NNN. `crates/rue-bench`, workload manifest,
      sampling and batching, partial-failure records, run-object emission.
- [ ] **Phase 4: Noise calibration (unpublished)** - RUE-NNN. Repeated runs
      on hosted runners per platform as workflow artifacts or explicitly
      marked non-series records; establishes sample counts, batching factors,
      the flagging multiplier and window, and the environment-fingerprint
      annotation policy. Nothing from this phase enters any series.
- [ ] **Phase 5: Declare and collect** - RUE-NNN. Suite revision 1 and the
      initial platform epochs with calibrated policies; `performance-data-v1`
      orphan branch; serialized collector workflow; first baselines.
- [ ] **Phase 6: Dashboard** - RUE-NNN. The page of §11, including
      collection-health presentation.

## Consequences

### Positive

- Additive phase charts are arithmetically truthful, produced by the compiler
  under an exact integer invariant that is tested per sample.
- The headline's cohort and configuration cannot change silently; workload
  and protocol changes are visible suite-revision or epoch events.
- Suite- or configuration-invalid observations cannot enter a series; there
  is no comparability classification to maintain, render, or explain.
- The headline chart is continuous within each epoch and can show a trend.
- Partial runs preserve evidence and make broken collection visible instead
  of leaving silent holes.
- Storage keeps the proven properties of content-addressed Git history
  (immutable, auditable, no external service) with a far smaller contract.
- One tested Rust runner replaces shell plus a Python package.

### Negative

- Every suite revision and epoch boundary is a visible headline
  discontinuity; deliberate changes now require declaring one. Friction by
  design, but friction.
- The headline is dimensionless and attenuates single-workload regressions by
  ~1/n; reading magnitude requires the per-workload views. "How long is a
  build" also requires the per-workload views.
- On hosted runners, environment fingerprints change within epochs and host
  drift moves observations; those comparisons are advisory, and the system
  records rather than prevents the drift.
- Phase instrumentation must be maintained as the compiler's parallel
  structure evolves, on pain of `mixed_parallel` growth.
- History restarts from zero; legacy measurements are unreachable from the
  dashboard.

## Open Questions

- Should `aarch64-macos` join the initial epochs, given hosted macOS runners
  show the most environment churn, or start Linux-only and add it when its
  noise profile is understood?
- Final membership of the initial corpus, and which scaling probes still name
  a question worth tracking.
- The flagging multiplier `k` and trailing-window length (settled empirically
  in Phase 4).
- The startup probe's batching factor (settled empirically in Phase 4).

## Future Work

Explicitly out of scope, each requiring its own ADR: interleaved
base/candidate measurement; per-PR performance reporting or gating; dedicated
measurement hardware (which would enable exact-fingerprint epochs);
generated-code performance; incremental and edit-scenario performance;
comparisons against C or Rust; formal external performance claims; epoch
splicing via shared workloads.

## References

- ADR-0019 (performance dashboard) and ADR-0031 (robust performance testing)
  — superseded; their system was removed by the benchmarking reset.
- ADR-0018 (tracing infrastructure) — the inclusive spans retained here.
- `crates/rue/src/timing.rs` — root active-interval union, inclusive span
  semantics, schema versioning.
- GitHub hosted-runner documentation and `actions/runner-images` — source of
  runner image version identity.
