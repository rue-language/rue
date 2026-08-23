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
relates: ["ADR-0018", "RUE-1543", "crates/rue/src/timing.rs"]
---

# ADR-0067: Compiler performance measurement, epochs, and dashboard

## Status

Proposal. This ADR accompanies the benchmarking reset: the previous runner
(`bench.sh`), the `scripts/benchmark_*.py` package, the stored corpus and
annotation file, and the old dashboard have been removed, and this document
defines what replaces them. It is written against the post-reset tree.

**Amendment 1 (RUE-1543) is a proposal and is not accepted.** It asks two
questions this ADR's storage and versioning rules leave open once the store has
grown: which versioning axis owns a change to *how* a run object is written as
opposed to *what* it measured, and whether already-published records may be
re-encoded. Everything above and below it remains the text as written; nothing
in the amendment is implemented. Its companion, ADR-0071 Amendment 1, proposes
the smaller representation whose versioning this one rules on. See
[Amendment 1: versioning the record encoding, and compacting the store (RUE-1543)](#amendment-1-2026-08-16-versioning-the-record-encoding-and-compacting-the-store-rue-1543).

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

Every root or count transition between zero and nonzero records a timestamped
observation in thread-local collector state. Query workers publish their local
observations once at their bounded completion boundary; long-lived caller
threads publish before reporting. Finalization deterministically sorts and
reduces the observations. No span close or phase transition takes a shared
collector lock, so instrumentation does not serialize parallel compiler work.

Finalization uses two independent reducers. One computes the union of compiler
root intervals without inspecting phase transitions. The other partitions the
root-active timeline into phase bands. Comparing those independently derived
totals makes the exact accounting invariant a useful corruption check rather
than an identity of one state machine.

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
- the resolved transitive content hash of each workload's **own** source
  closure, excluding the standard library (see "The product boundary" below);
- the content hash of the Rust toolchain the compiler is built with;
- the per-workload sampling and batching policy;
- the environment policy: runner environment class and image label
  (e.g. `github-hosted`, `ubuntu-24.04`);
- the headline baseline: the first **complete, valid** run at a declared
  trunk revision, whose per-workload medians define ratio 1.0. An attempted
  or partial run is never a baseline;
- whether scheduled collection measures this epoch. The manifest is the only
  place this is declared, so the collector, the CI gate, and a maintainer
  resolving a stall cannot disagree about which epoch is being collected.

**A series may not stall silently.** Refusing an observation protects
comparability, but a refusal that nobody sees stops the series while every job
reports success — measurement that has quietly ceased is worse than no
measurement, because it is still believed. Two gates enforce this. A change
that moves a pinned input fails its own pull request, decided from that tree
alone. A series that has stopped advancing — more than five merged trunk
commits with no new plotted point on some platform — fails *every* pull
request until it is resolved, because a stall is a repository-wide condition
rather than a property of whichever change caused it. Neither has a bypass:
the remedy is declaring the next epoch, which needs no baseline and so is a
small manifest change, and a genuine emergency is served by bypassing branch
protection rather than by an escape hatch maintained for the purpose.

**The product boundary.** The Rue language, `std`, and the first-party
toolchain are a single product, and this suite measures that product. They are
therefore the *subject* of measurement, never pinned inputs: a `std` change
moves a series exactly as a change to compiler internals does. Each run records
the standard library's resolved hash, validation does not compare it against
the epoch, and the dashboard annotates the point where it changed. The
annotation is deliberately not marked advisory — unlike an environment change,
this is real movement in the thing being tracked.

What remains pinned is everything that is *not* the product: each workload's
own sources, the target and invocation, the environment policy, and the Rust
toolchain the compiler is built with. That last one is build environment rather
than product — changing it changes generated code while shipping nothing.

An epoch pinned the standard library outright until RUE-1256. Pinning makes a
`std` edit refuse every subsequent run, which stops the series rather than
describing it, and a pin reports only that a hash moved. Measuring `std` — a
workload that compiles it — reports what a change cost, which is both the
stronger signal and one that cannot halt collection.

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
fingerprints, and makes the affected comparisons advisory. Product changes —
compiler, `std`, first-party toolchain — are what the series exists to show,
and never invalidate anything.

**Versioning a workload rather than refusing its runs.** Refusal is the
forcing function, not the resolution. When a workload genuinely must change,
the answer is a new workload identity rather than an edit under the old name:
suffix the identifier (`caldera`, then `caldera-2`), declare it in the next
suite revision, and add the new one before removing the old so coverage stays
continuous. Both series render, the old one ending where the new one begins.
This follows `rustc-perf`, which versions its benchmarks the same way. The
trade is a bounded loss of continuity for measuring something still worth
measuring; what must never happen is a workload silently changing meaning
under a continuing headline.

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
  collection-health warning naming the failing workloads;
- an incomplete run also fails its own collection job, with its recorded
  failures in that job's log. The dashboard warning is where collection health
  is *read*; it is not what tells anyone to look. A refused run stores nothing
  and an incomplete one still publishes each workload that completed, but
  neither advances the headline series, and when no workload completes the two
  are indistinguishable from outside. Until RUE-1514 the incomplete case did
  that while every check reported success, leaving the repository-wide stall
  gate above to notice days later, against pull requests that did not cause
  it.

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
values, so hashing involves no floating-point formatting. That digest is taken
over the bytes a record was published as, and a reader takes a stored record's
name from the record rather than re-deriving it: record fields are additive, so
re-serializing a parsed record names it as today's schema would have written
it, which renames every record written before the newest field — including
whichever record an epoch's baseline pins. `index.json` only
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

Investigating a flagged movement happens outside the dashboard. Inclusive
spans and `--time-passes` locate compiler phases; questions about the output
binary itself — size composition, generated-code behavior — use the
symbolized build workflow in `docs/process/profiling.md` (RUE-1173), because
default internal-linker executables deliberately carry no symbol table.
Measured series keep the epoch's pinned invocation; symbolized builds are
investigation builds, never appended observations.

## Implementation Phases

- [x] **Phase 1: Measurement schema** - RUE-1184. Suite revisions, platform
      epochs, series identity, run-object schema, canonical serialization and
      SHA-256 content addressing, validation rules.
- [x] **Phase 2: Compiler phase accounting** - RUE-1185. The reference-counted
      state machine of §2 in the timing collector, published alongside
      existing spans; measurement-boundary tests including Rayon-parallel and
      same-phase-concurrent workloads; exact nanosecond invariant under test;
      `compiler_root_ns` / `process_elapsed_ns` distinction.
- [x] **Phase 3: Runner** - RUE-1186. `crates/rue-bench`, workload manifest,
      sampling and batching, partial-failure records, run-object emission.
- [x] **Phase 4: Noise calibration (unpublished)** - RUE-1187. Repeated runs
      on hosted runners per platform as workflow artifacts or explicitly
      marked non-series records; establishes sample counts, batching factors,
      the flagging multiplier and window, and the environment-fingerprint
      annotation policy. Nothing from this phase enters any series.
- [x] **Phase 5: Declare and collect** - RUE-1188. Suite revision 1 and the
      initial platform epochs with calibrated policies; `performance-data-v1`
      orphan branch; serialized collector workflow; first baselines.
- [x] **Phase 6: Dashboard** - RUE-1189. The page of §11, including
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

## Amendment 1 (2026-08-16): versioning the record encoding, and compacting the store (RUE-1543)

**Status: proposal. Not accepted, not implemented.** ADR-0071 Amendment 1
proposes a run-object encoding that is 4.0% of today's size. This amendment
rules on the three questions that proposal raises about *this* ADR: which
versioning axis owns the change, what that axis must become before it can carry
one, and what happens to the 1,481.2 MiB already published. All three need a
maintainer ruling, and the second is a prerequisite for the other two rather
than a preference among them.

(Figures throughout are the 2026-08-16 corpus the analysis was performed on.
Re-measured 2026-08-23 at rebase: 1,619 records, 3,470.7 MiB, growth
unchanged at ~284 MiB/day — the supporting note carries the details. The
trend the amendment addresses has continued exactly as projected.)

### Question 1: which axis owns a change to the record encoding?

§3 assigns "runner protocol semantics (what a sample is, how batching is
defined, what a run object contains)" to the **suite revision**. Read literally,
the ADR-0071 encoding is suite revision 5, and therefore new epochs on all three
platforms.

**Recommendation: treat a change to the record encoding that provably preserves
every derived value as a `RUN_SCHEMA_VERSION` change, not a suite revision.**
Amend §3 to say so: a suite revision pins what was measured and what a sample
*means*; `schema_version` pins how a record is written down. The evidence that
the distinction is real here is that with the boundary-evidence keys stripped
from both sides, **0 of 1,188 published records differ** between the current
encoding and the proposed one. Identity, pins, environment, phase accounting,
`process_elapsed_ns`, `peak_memory_bytes`, `output_binary_bytes` and failure
records are byte-identical, so no median, dispersion, ratio, index, ratchet or
flag can move. The guarantee §3 exists to protect — that a series cannot change
meaning silently — is not the guarantee at stake.

The alternative, a suite revision, costs:

- an epoch turn on `x86_64-linux`, `aarch64-linux` and `aarch64-macos`;
- a headline-index gap on each platform until its baseline is pinned. Epoch 6
  went fourteen complete runs and thirty trunk commits with no index on any
  platform before RUE-1533 pinned it. RUE-1533's gate now holds that state to a
  deadline, but the gap is real and would recur;
- and, decisively, it makes Question 2 unanswerable: a re-encoded epoch-5
  record claims suite revision 3, which declares `protocol_version = 2`, so
  validation refuses it. Under the literal reading the legacy bytes cannot move
  at all without also redefining what epochs 5 and 6 mean.

The honest cost of the recommendation: within one epoch, records written before
and after the cutover carry different amounts of re-checkable evidence, so a
reader auditing an epoch's admissibility retroactively gets different depth at
different points. That is a real weakening and it is the price of not turning
three epochs.

Naming `schema_version` as the axis is not sufficient on its own, because that
field is not currently a decoding axis at all. Question 1a settles what it has
to become.

### Question 1a: what must the reader contract become?

This amendment originally claimed the encoding could be adopted for new records
without compacting anything. **That claim was wrong**, and the correction is
Steve's on the pull request: `schema_version` today is a refusal marker, not a
compatibility axis.

`validate_run` (`crates/rue-perf-schema/src/validate.rs:401`) compares
`run.schema_version` against the single constant `RUN_SCHEMA_VERSION` and, on
any difference, returns `UnsupportedSchemaVersion` without evaluating anything
else. `lib.rs:139` states the intent outright — "Readers refuse versions they do
not implement rather than guessing: there is no compatibility path, by design" —
and `validate.rs:34` repeats it. So bumping the constant to 2 does not add a
version; it *replaces* the one version readers accept.

The consequence, if new records were written at v2 while the 1,188 v1 records
sat at the tip: `validate_run` rejects every one of them, `derive` routes all
1,188 to `rejected` and derives no platform at all, the dashboard empties, and —
worst of the three — `validate-performance-stall.py` sees an empty `platforms`
list, prints "no plotted points yet; nothing to stall", and **exits 0**. The
gate built to notice a stopped series reads a totally rejected corpus as the
honest first state of a suite that has not collected yet. A botched rollout is
therefore silent in exactly the way ADR-0067 §"A series may not stall silently"
forbids.

**Recommendation: amend the no-compatibility invariant, and make dual v1/v2
decoding and validation part of this change rather than a follow-up.** The
reader contract becomes:

1. **Readers implement every schema version that can still be in the store.**
   `RUN_SCHEMA_VERSION` stops being "the only version readers accept" and
   becomes "the version the *producer* writes"; refusal applies to versions
   ahead of the reader, not behind it. Both prose invariants
   (`lib.rs:139`, `validate.rs:34`) are amended to say so.
2. **Encoding shape dispatches on `schema_version`; what must be proven
   dispatches on the suite's `protocol_version`.** These axes now cross, and
   crossing them silently is the defect that would bite: `check_boundary_evidence`
   keys the `len == batch_size` rule off protocol v2, so applied to a v2-encoded
   record it must check `boundary_processes` against `batch_size` instead of
   `boundary_evidence`. A v2 record of a suite revision declaring
   `protocol_version = 2` is well-formed and must validate; the rule is the same
   guarantee read off a different field.
3. **v1 support may be dropped only after no v1 record can be reached by a
   consumer.** That is: after a compaction that removes the last v1 record from
   the tip, and never while the site build or the staleness gate can still read
   one. If Question 2 is declined, v1 support is permanent — which is a genuine,
   ongoing cost of declining, and belongs in that decision rather than in a
   later surprise.

The dual-reader is what makes the two decisions separable at all. Without it the
only alternative is an atomic cutover — the reader flip and the corpus
conversion landing at the same instant — which is not achievable across a
repository merge and a data-branch push, and whose failure window is the silent
one described above. Steve's review named both routes; this amendment takes the
first, and says plainly that the second is not implementable rather than merely
less attractive.

### Question 2: may published records be re-encoded?

**Recommendation: yes, once, as an ordinary append — and do not rewrite
history.**

The decisive measurement is that the store's cost is a *checkout* cost, not a
*storage* cost. The whole branch — 402 commits, 1,188 records — fetches in
**53.69 MiB** and expands to **1,482.9 MiB** on disk, a ~28× ratio, because a
16 MB macOS record is mostly repeated bytes. GitHub reports the entire
repository at 115.8 MiB.

So the two levers are not the same lever:

- Rewriting history can reclaim at most ~49 MiB of pack, and only after
  GitHub's own maintenance, which is not available on request. A force-push
  leaves unreachable objects in place, still fetchable by SHA, with the reported
  repository size unchanged. Measured: `git repack -adq --window=250
  --depth=100` on the branch as fetched produces **no improvement at all**.
- Changing the tip tree reclaims 1,428.6 MiB of checkout, and needs no rewrite.
  (1,421.9 MiB once ADR-0071 Amendment 1's split digest is included.)

The recommended operation is therefore a single commit on `performance-data-v1`
that adds 1,188 re-encoded records under their own new content addresses,
removes the originals from the tip, and rewrites `index.json`. No history is
rewritten, no address is ever reused for different bytes, and every original
record stays reachable at `<pre-compaction-commit>:runs/<address>.json`. Tag
that commit so the full evidence has a name a reader can quote. Measured
result: the tip falls from 1,482.9 MiB to 54.3 MiB and parses in 0.34s instead
of 18.48s.

Every re-encoded-tip figure in this amendment was serialized with **one digest
per process**. ADR-0071 Amendment 1 now splits that digest in two so the
encoding survives a parallel boundary epoch, which adds a measured 6.7 MiB of
digests across the branch: the tip becomes **61.0 MiB** rather than 54.3, a 24×
checkout reduction rather than 27×. Parse time is unaffected at this scale. The
pack and fetch consequences are bounded by that same 6.7 MiB before compression
and were not separately measured; the implementing change owes both
re-measurements. The tables below are left at their measured values and labelled
accordingly rather than restated with derived ones.

**All 1,188 addresses move.** `schema_version` is an ordinary field of
`RunObject` (`run.rs:457`) with no `skip_serializing_if`, so it is part of the
canonical form `content_address` digests, and Question 1a requires every
re-encoded record to declare `schema_version = 2`. A record whose only change is
`1` → `2` therefore gets a new name. The 877 records carrying no boundary
evidence — epoch 2's 868, epoch 4's 3, and six epoch-5 records — stay
byte-identical *below* the version field, which is why the equivalence result
holds, but they move too. The required repository changes are exactly:

| Site | Change |
| --- | --- |
| `performance/manifest.toml` | re-pin all 9 `[epoch.baseline] run` values (epochs 2, 5 and 6, three platforms each) and the epoch-5 `reference_run`. |
| `docs/notes/adr-0071-phase-1-…md`, `adr-0071-phase-2-…md` | four prose citations of epoch-5 record addresses |
| everything else | nothing. `website/static/performance-data.json` is generated and untracked; no test pins a record address; no committed derived data exists. |

Two failure modes are worth naming because they differ sharply:

- Getting the `reference_run` out of step with its baseline fails **loudly**:
  `manifest.rs` rejects the manifest at parse.
- Getting a baseline address wrong fails **silently for a retired epoch**.
  `derive` resolves the baseline by address among that epoch's own records and,
  on a miss, publishes no index and no workload ratios while still plotting
  every per-workload series. `validate-performance-stall.py`'s `unindexed()`
  gate reports exactly that — but iterates `newest_epochs()`, so epochs 2 and 5
  are unguarded, and six of the nine pins that must move belong to those two
  retired epochs.

  An earlier draft of this amendment made "extend `unindexed()` to every epoch
  declaring a baseline" the condition of acceptance. **That is not
  implementable as stated**, and the correction is Steve's on the pull request:
  the gate never sees a retired epoch's records. `rue-bench staleness-inputs`
  selects the epoch holding each platform's newest point and nothing else
  (RUE-1542), so those records are never materialized into the data root
  `derive` reads. Editing the rule cannot make it inspect data that was not
  checked out.

  Restoring them is the wrong repair. Selecting every epoch that declares a
  baseline means reading epochs 2, 5 and 6 — on 2026-08-18, 1,437 of 1,440
  records against the 321 the gate reads now, which is the cost RUE-1542 was
  merged to remove.

  The check that catches this failure needs no derived data at all. Every
  `[epoch.baseline] run` must name a record of its own epoch and platform, and
  `index.json` already carries the platform, epoch and address of every record
  — and is already checked out, because the selection above reads it. So the
  condition of acceptance is **a manifest-against-index baseline resolution
  check covering every epoch, live or retired**, which this branch implements
  as `rue-bench check-baselines` and runs in the staleness job ahead of
  `derive`. It reports all nine of today's baselines resolving, and exits 3
  naming the epoch when one does not.

### What immutability and content addressing are actually protecting

§8 argues a stored record can be verified against its own name without trusting
whoever wrote it. In the actual write path there is no untrusted writer: the
only writer is the `publish` job in `performance-collect.yml` running with the
repository's own token, the branch is not protected, and anyone who can make
that job write a record can equally make it write one with a correct address.
The properties in use are narrower and worth stating so a decision to spend one
of them is deliberate:

1. **Idempotent republication.** A re-run of a collection workflow produces
   byte-identical records and the publisher skips them by name. Without this,
   re-runs would double-count points.
2. **Accident refusal.** Differing bytes under an existing name are refused
   rather than clobbered.
3. **Protection against a silent schema rename.** Record fields are additive,
   so re-serializing a parsed record yields bytes — and a name — it never had,
   which would unname whichever record a baseline pins, invisibly. This is what
   `Stored` exists for, and it is a guarantee against our own future
   carelessness rather than against an attacker.
4. **A governance property.** A published measurement cannot be quietly edited
   later to make a chart look better.

A reviewed one-time re-encode spends (4) once, in the open, and touches none of
(1)–(3): a re-encoded record is a *new* record with its own correct address, and
the original keeps its name and its bytes in the branch's history. That is the
whole argument for allowing it, and the reason the recommendation is "once, as
an append" rather than "immutability was a mistake".

One premise should not be assumed: the repository is public with 42 forks, and a
default clone fetches every branch, so "nobody else has this data" is not
verifiable. This does not affect the append-based recommendation, which breaks
no clone; it is a reason not to choose the force-push variant.

### Alternatives considered

Measured. "Loses" is what becomes unavailable to a reader holding only the tip.

"Checkout after" is the working-tree cost — what RUE-1543 is about and what
every consumer pays. It is not the fetch cost, and for the recommended option
the fetch moves the *wrong* way. Each state below was measured as a standalone
single commit (5.0 MiB of pack for the re-encoded S4 tip), but the append lands
that commit on top of the existing 402, so the branch's fetch grows from
53.69 MiB to roughly 59 MiB and stays there — under 66 MiB once the split
digest's 6.7 MiB is added, taking that half as uncompressed. That is the one
figure in this amendment where the storage side gets worse. At that size it
remains an easy trade for a 24× checkout reduction, but since the whole argument
rests on "the store's cost is a checkout cost, not a storage cost", it belongs
in the text rather than inferred from the supporting note's tables.

Checkout figures are as measured, with one digest per process; add 6.7 MiB to
each re-encoded row for the split digest.

| Option | Checkout after | Loses |
| --- | ---: | --- |
| do nothing | 1,482.9 MiB, +289/day | nothing; the trend continues |
| new encoding for new records only | 1,482.9 MiB, +11/day | nothing |
| **re-encode at the tip, no history rewrite** | **54.3 MiB** | per-process `critical_path` from the tip; still in history |
| re-encode retired epochs only | 456.0 MiB | same, epochs 2–5 only; 1,119 addresses move |
| delete retired-epoch records | 420.0 MiB | epochs 2/4/5 vanish from the dashboard |
| summarize a retired epoch to one record | ~420 MiB | per-commit resolution; needs a new record kind and a dashboard path |
| archive the pre-compaction tip as a tag | unchanged | nothing; costs zero bytes, composes with the above |
| force-push a fresh orphan, same records | 1,482.9 MiB | history, for ~0 reclaimed |
| force-push a fresh orphan, re-encoded | 54.3 MiB | all history including the full evidence, permanently |
| git-level repacking, no content change | unchanged | nothing — and measured to reclaim nothing |

A reader in six months re-derives any chart from the recommended option
unchanged, because every value a chart is drawn from is byte-identical.
Recovering a specific run's full per-process evidence means checking out the
archived tag. Under either force-push variant, that recovery is impossible once
GitHub's maintenance runs.

### Relationship to the other amendment

An earlier draft of this section claimed the two amendments were independent and
could land "in either order". They are not, and cannot. Corrected:

**The dual-version reader of Question 1a is a prerequisite for both, and must
land first.** Until readers implement v1 and v2 together, a v2 record and a v1
record cannot coexist in the store, so neither the encoding nor the compaction
can be adopted without the other arriving in the same instant.

With that prerequisite in place, the two are independent *in outcome* and
*ordered* in execution. The permitted sequences are:

1. dual-version reader, then producer writes v2, then compaction — the legacy
   records convert whenever it is convenient, or never;
2. dual-version reader, then compaction, then producer writes v2 — the store is
   uniformly v2 sooner, and records written between the two steps are v1 and
   convert in a second sweep.

Either order works because both encodings are readable throughout. No order
works without the reader.

If Question 1 is answered literally — suite revision 5 — they are **coupled**
regardless: compaction is impossible without redefining epochs 5 and 6, because
a re-encoded epoch-5 record claims a suite revision declaring
`protocol_version = 2`, and the ADR-0071 Amendment 1 recommendation additionally
costs a headline gap on three platforms.

Accepting ADR-0071 Amendment 1 and declining Question 2 is coherent: it stops
the growth and leaves the 1,481.2 MiB in place, which after RUE-1542 already
sits outside the staleness gate's read path and burdens only the website build.
The cost of that combination is now explicit: v1 decoding is retained
permanently, because a v1 record remains reachable at the tip forever.

Declining Question 1a is not coherent with accepting anything else here. It is
the one part of this proposal that is a prerequisite rather than a preference.

## References

- ADR-0019 (performance dashboard) and ADR-0031 (robust performance testing)
  — superseded; their system was removed by the benchmarking reset.
- ADR-0018 (tracing infrastructure) — the inclusive spans retained here.
- `crates/rue/src/timing.rs` — root active-interval union, inclusive span
  semantics, schema versioning.
- GitHub hosted-runner documentation and `actions/runner-images` — source of
  runner image version identity.
- [Boundary evidence and the size of performance-data-v1](../notes/performance-boundary-evidence-size.md)
  — Amendment 1's measurements, breakage inventory, and option space.
