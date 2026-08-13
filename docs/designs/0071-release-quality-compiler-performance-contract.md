---
id: 0071
title: "Release-quality compiler performance contract"
status: accepted
tags: [architecture, compiler, performance, process]
feature-flag: null
created: 2026-08-12
accepted: 2026-08-12
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-1470", "RUE-1478", "ADR-0063", "ADR-0067", "ADR-0068"]
---

# ADR-0071: Release-quality compiler performance contract

## Status

Accepted by Steve Klabnik and Dorian Scheidt on 2026-08-12. This ADR sets a
product target and the rules for pursuing it. It does not replace the compiler
architecture in ADR-0063 or the measurement protocols in ADR-0067 and
ADR-0068. It turns their implemented mechanisms into an explicit performance
contract.

## Summary

Rue will optimize one canonical, release-quality source-to-native compiler
pipeline toward a 250 ms fresh-process build of the fixed Lattice reference
workload in a versioned GitHub Actions reference regime. A one-worker run is
the primary architecture target; an automatic-worker run at or below 150 ms is
a secondary stretch target in the same regime. The target pins Rue `-O3`; it
includes source discovery, parsing, semantic analysis, CFG construction and
optimization, backend work, object generation, linking, and output publication.
It cannot be met by weakening generated-code quality, introducing a separate
fast compiler, moving work outside the measured boundary, or assuming
precompiled package artifacts.

Wall time is the user-visible outcome. Retired instructions, allocations,
resident memory, phase accounting, and deterministic compiler-work counters
explain why it moved and protect algorithmic scaling when clock noise cannot.
The reference runner and compiler publish complementary, machine-checked
evidence that the complete boundary was actually measured; a sample that used
undeclared retained state, cached artifacts, a daemon, or a different codegen
policy is invalid rather than merely annotated.
Work proceeds as vertical, profile-driven milestones: first make the current
critical path and worker scaling explicit, then review repeated ownership across
semantic analysis and CFG construction, then drive the complete pipeline
through 500 ms, 350 ms, and 250 ms milestones without regressing correctness,
memory, output, or incremental locality.

## Accepted maintainer decisions

1. Rue `-O3`, from process spawn through successful native output and exit, is
   the indivisible product boundary.
2. The primary target is 250 ms with one worker. A 150 ms automatic-worker run
   is a stretch target rather than a prerequisite.
3. The 128 MiB envelope remains provisional through the first attribution
   milestone.
4. Runner, manifest, and compiler evidence jointly enforce the measured
   boundary. Structural repository tests additionally prohibit peer pipelines
   and undeclared ways to move work outside it.
5. Phase 1 measures the GitHub Actions reference regime, sets the first limit
   at the observed baseline, and publishes it. Accepted improvements ratchet
   that limit downward until it reaches 250 ms; after that, 250 ms is the
   permanent non-regression ceiling for the pinned regime.

## Context

Rue deliberately built incremental and parallel compiler architecture before
setting an absolute speed target. ADR-0063 established one revisioned query
graph through object projection, per-body semantic/CFG/codegen boundaries,
immutable canonical artifacts, and a fresh-link seam. ADR-0067 established
honest wall-clock phase accounting and versioned performance observations.
ADR-0068 established retained-session edit measurement. The post-ADR-0063
architecture audit confirms that these are the live paths; there is no peer
frontend or coarse shared semantic owner to remove.

That work has paid off, but relative improvement alone does not answer whether
the compiler is fast enough. On the maintained Lattice workload, the tracked
one-worker release-built compiler moved from 2,042.64 ms at RUE-1351's baseline
to a median 843.30 ms after RUE-1467, a 2.42x improvement. RUE-1468 then removed
a remaining quadratic warning-body selection path with a clock-neutral result.
The latest fixed-source profile used for this decision contains 781,873 source
bytes, 19,264 lines, 155,488 tokens, 161 modules, and 1,221 functions. Its
median peak RSS is 394.48 MiB.

That historical scaling invocation builds the compiler implementation with
release ThinLTO but passes no Rue optimization flag, so the compiled Lattice
program uses Rue's `-O0` default. It is valid evidence for the completed
compiler-work campaign, not the baseline for this ADR's release-quality target.
A six-pair local diagnostic on current trunk found `-O3` compiler-root time
1.17% above `-O0`, a 17.20% smaller executable, and 4.02% higher peak RSS. The
host was busy and the absolute times were not baseline-quality; Phase 1
therefore establishes the first external `-O3` target observation under the
pinned regime.

The latest controlled one-worker `-O0` phase composition is approximately:

| Exclusive phase | Median | Share of compiler-root time |
| --- | ---: | ---: |
| Source discovery and parsing | 59.67 ms | 7.1% |
| Semantic analysis | 365.67 ms | 43.4% |
| CFG and optimization | 258.62 ms | 30.7% |
| Backend | 100.54 ms | 11.9% |
| Object generation and linking | 9.57 ms | 1.1% |
| Unattributed | 47.16 ms | 5.6% |

Component medians do not sum exactly to the median total because they are
derived independently from the same 16 samples. The conclusion is nonetheless
clear: linking is not the current fresh-process bottleneck. Semantic analysis and
CFG construction account for roughly three quarters of the serial profile.

The recent optimization sequence also demonstrates why the contract needs more
than wall time. It removed large exact quantities of allocations, repeated
retention passes, identity formatting, graph traversals, and quadratic work
while several individual wall-clock comparisons remained neutral within host
noise. These are real architectural improvements, but Amdahl's law means that
continuing to polish diffuse bookkeeping cannot by itself produce the required
multi-fold end-to-end improvement. The next step must combine precise counters
with critical-path and ownership evidence.

### Terminology

This ADR uses **fresh-process build**, not **cold build**, for its measured
contract. Every sample launches a new compiler process with no retained Rue
session or persistent query cache. The operating-system filesystem and page
caches are not reset, matching ADR-0067's terminology. A machine-cold or
cache-flushed experiment may be useful diagnostically, but it is not stable
enough to define the product target.

**Release-quality** means the single production pipeline with Rue `-O3`,
emitting the same native program Rue would distribute. It does not mean only
compiling the Rust implementation with release settings, and it does not permit
a lower-quality Rue codegen mode to satisfy this target.

## Decision

### 1. Optimize one complete product boundary

The performance boundary is an externally observed invocation of the canonical
Rue compiler, beginning at process launch and ending only after the requested
native executable has been written and the compiler exits successfully.
Compiler-emitted phase accounting remains the authority for attribution inside
that invocation. The runner records launch and teardown outside compiler-root
time so those costs cannot disappear into an unmeasured seam.

The boundary includes:

- compiler process startup and argument/configuration validation;
- source discovery, loading, lexing, parsing, and program construction;
- semantic analysis and diagnostic/warning production;
- CFG construction and all requested optimizations;
- target lowering, register allocation, scheduling, and machine-code emission;
- object generation, linking, and output publication; and
- acquisition of source-defined standard/toolchain inputs needed by a clean
  compiler process under the current distribution model.

Moving work to a wrapper, daemon, build script, package installer, or previous
invocation does not improve this metric. Such systems may later create valuable
additional product modes, but their results must be reported separately.

### 2. Make the boundary machine-checkable

ADR-0067's schema gains an exhaustive build-boundary identity. The first
variant is `fresh_source_to_native_v1`; future retained-session, package-cache,
precompiled-standard, or daemon-backed measurements require different variants.
Unknown variants and unknown provenance fields are rejected. They cannot enter
the reference series as advisory samples.

Each valid observation combines three independent authorities:

**The runner proves process facts.** It starts its monotonic clock before
spawning the canonical `rue` CLI, creates a fresh per-sample state and output
directory, supplies no daemon endpoint or retained-session handle, waits for
process exit, verifies the native output, and stops the clock afterward. It
records compiler binary identity and separates pre-spawn fixture preparation
from measured work. It never calls lexer, semantic, CFG, codegen, or linker
libraries directly.

**The target manifest proves intended policy.** It pins the compiler target,
release ThinLTO implementation profile, complete Rue invocation including
`-O3`, target architecture, link policy, worker setting, source/toolchain input
identity, output kind, allowed artifact provenance, and allowed
compiler-embedded asset classes. The one-worker and automatic-worker targets
are separate declared rows. A change to any field creates a reviewed target
epoch; the runner does not infer compatibility.

**The compiler proves executed work.** Its benchmark envelope reports the
boundary variant, canonical pipeline identity, session/root-request count,
resolved worker count, configuration identity, accepted input classes,
embedded-asset manifest, persistent artifact/cache hits, and the successful
completion of source, semantic, CFG/optimization, backend, object, link, and
output stages. For `fresh_source_to_native_v1`, retained-session inputs, daemon
handoffs, precompiled program/package/standard-artifact hits, and persistent
compiler cache hits must all be zero. Existing source-read envelopes and
explicit link inputs are the basis of the provenance account; any new external
input or compiler-embedded artifact path must join that account before it can be
used by this boundary.

The checker accepts a sample only when runner evidence, manifest policy, and
compiler evidence agree. It also records external process elapsed time,
compiler-root time, and their difference, so startup and teardown cannot be
made invisible by moving the compiler's root span.

Repository-policy tests pin the scheduled and documented target commands to the
real release compiler and this boundary variant. A compiler change which adds a
persistent cache, precompiled artifact, daemon path, alternate frontend, or
alternate linker must add explicit provenance and must not change the existing
variant's accepted set. This is an exhaustive enum/schema change, not a naming
convention.

Structural repository tests also reject any peer frontend, semantic, CFG, or
codegen path, including test-only compatibility aggregates that can bypass the
canonical rooted query graph. A future alternate product mode must have its own
explicit boundary variant and architecture decision; it cannot enter by adding
a facade over a second computation path.

Performance-only changes preserve diagnostics, warning identities, emitted
bytes, and executable fingerprints. A change that intentionally alters machine
code is reviewed and measured as a codegen-quality change first; it cannot claim
the same-boundary speedup merely by doing less optimization while leaving the
manifest's optimization identity unchanged.

### 3. Set an aggressive reference target

The primary target is:

> The versioned Lattice reference workload builds from a fresh compiler process
> in **250 ms or less with exactly one compiler worker** in the versioned
> `x86_64-linux` GitHub-hosted Actions collection epoch using `ubuntu-24.04`,
> the release ThinLTO compiler, and the canonical Rue pipeline with `-O3`.

The secondary stretch target is 150 ms or less with the production automatic
worker setting in the same regime. One worker is primary because it exposes
algorithmic work and makes the target portable across different core counts;
parallelism should shorten the critical path, not hide repeated work. The
automatic-worker target preserves the user-facing latency outcome.

The reference is a frozen workload revision, not whatever source happens to be
called `Lattice` later. Its source shape, compiler invocation, target, standard
inputs, compiler build profile, Actions runner label and image, and collection
policy are versioned. Each observation records the available runner
fingerprint. A material provider hardware or image change starts a reviewed
comparison epoch, re-establishes its baseline and ratchet, and preserves the
previous target record. It does not silently splice unlike measurements into
one series. Local maintainer-host measurements remain useful diagnostics, but
they do not decide whether an absolute milestone has been reached.

The target is intentionally at least 3.4x faster than the 843.30 ms internal
`-O0` reference even before external process overhead is included. Phase 1 will
record the exact `-O3` gap. The goal is aggressive enough to force architectural
scrutiny while remaining source-to-optimized-native rather than relying on
cached artifacts or a reduced-quality backend.

### 4. Treat memory as a first-class, initially provisional envelope

The 250 ms design target carries a provisional **128 MiB peak RSS** envelope on
the same reference workload and regime. Until resident allocator behavior is
reconciled with Rue's exact retained-artifact charges, this is a planning target
rather than a hard acceptance gate. The first vertical milestone has an
intermediate 256 MiB envelope.

A change may land with neutral clock time when it materially improves code
ownership, deterministic work, retired instructions, allocations, or requested
bytes, but it may not introduce a reproducible peak-memory or latency regression
without a separately reviewed policy decision. Improvements cannot trade
unbounded retained state for speed.

### 5. Use a hierarchy of evidence

No single measurement answers every performance question:

1. **External elapsed time** is the product outcome and decides whether a
   milestone has been reached.
2. **Compiler-root phase accounting** locates the exclusive wall-clock budget
   and must remain exhaustive.
3. **Retired instructions and cycles** adjudicate small CPU changes more
   reliably than wall time alone.
4. **Allocation calls, requested bytes, peak RSS, and exact retained charges**
   distinguish temporary churn from persistent state and allocator high-water
   effects.
5. **Deterministic compiler-work counters** are the authority for algorithmic
   amplification, duplicated work, invalidation, and query-runtime behavior.
6. **Output, diagnostics, warning identities, and executable fingerprints**
   remain exact correctness gates.

Clock-neutral work reductions are acceptable architectural wins. A clock
regression is not. Phase 1 calibrates the distribution of repeated samples in
the pinned GitHub Actions epoch and sets an initial non-regression limit at the
measured baseline. Each accepted improvement ratchets that limit downward; the
limit is never relaxed automatically. Once the limit reaches 250 ms, 250 ms is
the permanent ceiling for that pinned regime. Calibrated dispersion handles
ordinary hosted-runner noise, deterministic work counters guard structural
claims, and a material runner-fingerprint change starts a reviewed epoch rather
than weakening the existing gate.

### 6. Use phase budgets as planning tools, not local scorecards

The initial 250 ms planning envelope is:

| Budget | Time |
| --- | ---: |
| Source discovery, loading, lexing, and parsing | 25 ms |
| Semantic analysis | 80 ms |
| CFG construction and optimization | 55 ms |
| Backend | 50 ms |
| Object generation, linking, and output publication | 20 ms |
| Process/orchestration/unattributed allowance | 20 ms |
| **Total** | **250 ms** |

These are allocation guides, not six independent acceptance criteria. A faster
phase may fund a slower one as long as the complete boundary, memory envelope,
correctness gates, and scaling requirements hold. The budgets are revised from
new profiles at milestone boundaries rather than used to justify optimizing a
phase that has ceased to dominate.

### 7. Measure scaling and critical path before another broad refactor

The next measurement pass records the fixed Lattice workload at 1, 2, 4, 8,
and automatic workers. It must explain, rather than merely display, the scaling
curve by adding or deriving:

- worker utilization and ready-but-not-running time;
- the longest producer/dependency chain;
- semantic and CFG work distributions per body;
- repeated construction or traversal of canonical facts;
- time inside compiler work versus toolchain acquisition and process overhead;
- unattributed time; and
- allocator, memory-bandwidth, and shared-lock contention evidence where the
  profile supports it.

Instrumentation must be bounded and aggregated off hot shared paths. A new
counter must identify a decision or falsify a proposed cause; counters are not
added merely because an event exists.

### 8. Review semantic-to-CFG ownership using current source

The first architectural review after measurement examines every material
artifact from parsed declarations through optimized CFG and records:

- its canonical owner and lifetime;
- how many times it is derived, copied, hashed, indexed, and traversed;
- whether equal values allocate independently;
- whether it is request-wide, module-local, body-local, or target-local; and
- which source edit invalidates it.

The expected direction is shared immutable canonical substrate feeding compact
per-body epochs, not a whole-program mutable semantic arena. Any proposed shared
state must remove measured repeated work, preserve independent body execution,
retain fine invalidation, and avoid a new global lock. A neutral prototype is
acceptable when it makes duplicate ownership unrepresentable or creates a
cleaner boundary for later optimization. A reproducible time or memory
regression is not.

ADR-0063 remains authoritative unless this review produces evidence for a
different decision. Retained `LoweredMir`, a coarser semantic query, or a new
shared owner requires its own explicit decision if it changes query granularity
or invalidation.

### 9. Advance through vertical milestones

Performance work follows complete-pipeline milestones rather than finishing
one compiler phase in isolation:

- **Baseline and initial gate:** freeze and publish the Lattice workload,
  annotate the 250 ms target on the public performance dashboard, pin the
  `x86_64-linux` `ubuntu-24.04` GitHub Actions epoch, measure its distribution,
  and set the first ratcheting limit at the observed baseline.
- **500 ms / 256 MiB:** remove the largest semantic/CFG repeated ownership and
  critical-path costs exposed by the new measurements.
- **350 ms:** reprofile the shifted bottleneck and address the new dominant
  phase without assuming the previous diagnosis still applies.
- **250 ms / provisional 128 MiB:** close the remaining holistic gap across
  frontend, backend, orchestration, output, and resident memory.
- **150 ms automatic-worker stretch:** improve exposed parallelism and shorten
  the dependency chain without increasing one-worker work.

Each milestone is measured across the maintained Ruelex, Mosaic, Harbor, and
Lattice scale curve. Lattice is the absolute target, not permission to regress
smaller programs, startup, warm edits, or asymptotic growth.

### 10. Make performance part of feature admission

A compiler or language feature proposal that can materially affect compilation
must state:

- the input dimension that drives its cost;
- expected time and space complexity;
- the canonical query/artifact owner;
- invalidation granularity;
- whether it introduces a whole-program scan or a peer computation path;
- an adversarial source shape that exercises the bound;
- deterministic counters or tests that prove the intended work; and
- expected cold, warm-edit, memory, and codegen-quality effects.

Features do not need to predict exact milliseconds before implementation. They
do need an architecture whose common and adversarial work can be measured and
bounded. An attractive language feature is not accepted with an accidental
quadratic compiler algorithm or whole-program invalidation as its unexamined
default.

### 11. Keep future artifact boundaries explicit without assuming them

Precompiled standard artifacts, package artifacts, persistent compiler hosts,
and incremental linking can eventually make repeated developer builds much
faster. They are not assumptions of the 250 ms target because Rue has not yet
decided package distribution, artifact compatibility, installation, or cache
invalidation policy.

Current compiler artifacts should nevertheless remain immutable, versionable,
target/configuration explicit, and owned at boundaries that can later be
serialized. This preserves options without charging an undeclared cache hit to
the fresh-process result.

## Implementation Phases

- [x] **Phase 1: Pin the reference regime, publish the frozen Lattice workload
  and target dashboard, establish the baseline ratchet, and produce the
  critical-path scaling report** — RUE-1475 and RUE-1478.
- [x] **Phase 2: Publish the semantic-to-CFG ownership and repetition audit** —
  RUE-1474.
- [ ] **Phase 3: Reach the 500 ms vertical milestone** — RUE-1473.
- [ ] **Phase 4: Reprofile and reach the 350 ms vertical milestone** — RUE-1471.
- [ ] **Phase 5: Reach the 250 ms target and evaluate the 150 ms parallel
  stretch** — RUE-1472.

The milestone issues are parents for bounded, profile-selected changes. The ADR
does not pre-authorize a large refactor merely because a later milestone exists.

## Consequences

### Positive

- Rue has a concrete definition of “fast compiler” tied to the complete product
  rather than an indefinitely improving relative index.
- The target rewards both algorithmic efficiency and useful parallelism without
  allowing cores to hide repeated serial work.
- Existing query, phase, allocation, and work-counter infrastructure becomes a
  decision system rather than a collection of interesting numbers.
- Language design incorporates compile-time complexity while Rue is still
  young enough to change representations and ownership cleanly.
- The deliberately difficult 250 ms target forces architectural scrutiny while
  Rue's representations and ownership boundaries remain inexpensive to change.
- A ratcheting non-regression gate makes compiler performance an enforceable
  product priority rather than an advisory aspiration.
- Future package and incremental systems inherit explicit artifact boundaries
  without becoming prerequisites for acceptable first-build performance.

### Negative

- GitHub-hosted Actions provides a shared reference regime, not a portable claim
  about every developer machine, and provider changes require explicit epochs.
- Critical-path and resident-memory attribution add measurement complexity and
  must be kept out of hot shared paths.

### Neutral

- ADR-0063's query architecture remains in force. This ADR may cause a later
  architecture decision, but current evidence does not justify one now.
- ADR-0067's dashboard and ADR-0068's warm-edit suite continue to answer their
  existing relative and incremental questions.
- A neutral clock result remains publishable when it is an exact work,
  complexity, ownership, or maintainability win with no other regression.

## Open Questions

1. Should the 128 MiB figure become a hard target now, or remain provisional
   until the first milestone reconciles allocator RSS with exact retained
   charges?

## Future Work

- Package and library distribution, including precompiled standard artifacts
  and compatibility/versioning policy.
- Stateful incremental linking, which requires a separate ADR and joint design.
- Persistent compiler-host productization and its security/lifecycle boundary.
- A separately measured reduced-codegen or debug mode, if Rue ever chooses to
  offer one; this ADR neither requires nor forbids that future product.
- Formal cross-language compile-speed claims on controlled comparable inputs.

## References

- [ADR-0063: Parallel demand-driven incremental compilation](0063-parallel-demand-driven-incremental-compilation.md)
- [ADR-0067: Compiler performance measurement, epochs, and dashboard](0067-compiler-performance-measurement.md)
- [ADR-0068: Incremental edit-scenario performance measurement](0068-incremental-edit-performance-measurement.md)
- [Post-ADR-0063 cold compiler architecture audit](../notes/post-adr-0063-cold-compiler-architecture-audit.md)
- [Compiler scaling reports](../process/compiler-scaling.md)
