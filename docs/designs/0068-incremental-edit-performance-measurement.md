---
id: 0068
title: "Incremental edit-scenario performance measurement"
status: accepted
tags: [tooling, compiler, incremental, performance]
feature-flag: null
created: 2026-08-07
accepted: 2026-08-07
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0063", "ADR-0067", "RUE-1033", "RUE-1086", "RUE-1096", "RUE-1200", "RUE-1242", "RUE-1243"]
---

# ADR-0068: Incremental edit-scenario performance measurement

## Status

Accepted. ADR-0063 implemented the retained query graph through per-function
object projection and a fresh link. ADR-0067 deliberately limits its historical
performance system to fresh compiler processes and lists incremental and edit-
scenario performance as future work requiring a separate ADR. This record
defines that separate regime; it does not change either predecessor's results or
terminology.

## Summary

Rue will measure interactive compiler performance as revision sequences through
one retained `CompilerSession`, using the canonical filesystem/import-discovery
host path and maintained Rue programs. Every scenario prepares an untimed
baseline, applies one declared successor edit, and records the latency and exact
structural work needed to reach three successful-compilation endpoints:
collected `CodegenUnit`s, retained object projections, and a freshly linked
executable. An expected erroneous successor instead records the time to its
canonical diagnostics.

Warm observations remain a separate lower-frequency report, never part of
ADR-0067's fresh-build headline or baseline history. Correctness is established
by comparing the warm result with a fresh session for the successor source.
Structural work is the architectural gate; host-specific latency decides where
to investigate, not whether compilation is correct.

## Context

ADR-0063's Phase 12 witness proves that a body-only edit can recompute one body,
CFG, and `CodegenUnit` while reusing an unaffected caller. On the recorded small
fixture, edit-to-`CodegenUnit` was 300 microseconds and edit-to-runnable with a
fresh link was 1,618 microseconds. Those values prove the boundary but do not
predict a maintained multi-module program.

The scheduled compiler-scaling report answers a different question. It launches
the compiler in a new process for Ruelex, Mosaic, Harbor, and Lattice, so it
includes no retained compiler session and cannot reveal warm locality. Mixing
the two regimes would make a graph look continuous while its observations have
different state, endpoints, and correctness controls.

The canonical production substrate already exists:

- `CompilerSession` owns the revisioned query graph and bounded retention;
- the driver source loader owns filesystem observation and import discovery;
- `reload_from_filesystem` re-observes the previous accepted-read closure under
  ADR-0063's Tier-B stat/hash rules and publishes a successor into the same
  session;
- per-function `CodegenUnit` and object-projection terminals survive compatible
  revisions; and
- `ProgramImagePlan` feeds the deliberately fresh linker.

Measurement must exercise those owners. A benchmark-only source loader, direct
snapshot mutation, peer phase coordinator, or separate codegen/link path would
measure a system users do not run and is prohibited.

## Decision

### 1. Two compiler-performance regimes

ADR-0067 retains exclusive ownership of **fresh-build performance**: one newly
launched compiler process with no retained compiler-session state. Its epochs,
headline index, raw-run objects, and alerting rules do not accept incremental
observations.

This ADR owns **retained-session edit performance**. One independent sample is:

1. create an isolated copy of the declared maintained-program fixture;
2. start one canonical filesystem-backed compiler host;
3. compile revision A to the selected baseline endpoint outside the timed
   interval;
4. apply exactly one manifest-declared transformation producing revision B;
5. time canonical re-observation, successor publication, and compilation to the
   selected endpoint;
6. collect structural work and retention gauges immediately at the endpoint;
7. outside the timed interval, compile revision B in a fresh session and compare
   diagnostics, warnings, and executable bytes when the scenario succeeds; and
8. discard the host and isolated fixture before the next independent sample.

Operating-system page-cache and scheduler state are uncontrolled. Reports call
these **retained-session** observations, not hot, cold, or clean builds.

### 2. Canonical endpoints

Every successful edit scenario records cumulative time from the start of
filesystem re-observation to each of these endpoints:

1. **codegen-ready** — the rooted reached set has produced its canonical
   `CodegenUnit` collection;
2. **objects-ready** — target object projections for the rooted units have been
   collected; and
3. **runnable-ready** — the internal linker has produced the executable bytes.

The intervals are cumulative and monotonic. Their differences may be presented
as derived bands, but raw observations store the three endpoint durations.

The error-introduction scenario records one alternate **diagnostics-ready**
endpoint: the retained host has completed the canonical failed query and owns
the deterministic diagnostics and warnings that the CLI would present. It does
not fabricate codegen-ready, objects-ready, or runnable-ready durations. An
unexpected failure in any other scenario records its stage and diagnostics and
is a correctness failure rather than a latency sample.

Output publication to the user's destination, filesystem-copy setup, fixture
mutation, fresh-oracle compilation, and executing the produced program are
outside all three intervals. A future watch-host report may add publication and
event-coalescing latency, but it must not silently redefine these compiler
endpoints.

### 3. Scenario matrix

The first suite defines these edit classes:

| Scenario | Required structural claim |
| --- | --- |
| no-op re-observation | no compiler artifact recomputes |
| unreachable body edit | reached semantic, CFG, codegen, and object terminals remain green |
| reached body-only edit | only the edited body cone and its function-local backend products recompute |
| callable-signature edit | exact semantic and ABI consumers invalidate |
| layout/ABI edit | exact layout, CFG, codegen, and image consumers invalidate |
| import-set edit | discovery and only the changed reachability cone recompute |
| reachability deletion | removed units leave the rooted image and unaffected units remain reusable |
| error introduction | valid revision A becomes invalid revision B; only the exact diagnostic cone recomputes and no successful downstream artifact publishes |

Each transformation is an exact, versioned fixture operation: logical file,
unique expected source fragment, replacement fragment, and expected edit class.
Applying it to zero or multiple locations is an invalid fixture, not a sample.
The isolated copy preserves the program's relative path layout and uses the
compiler's accepted-read manifest as the authoritative input closure.

A workload may also declare a versioned **baseline overlay** applied before
revision A and outside every timed interval. An overlay has the same exact
zero-or-one fragment checks as an edit and contributes to the incremental-
fixture revision. It may add only source material needed to express a scenario,
such as an initially unimported leaf or an underscore-prefixed private helper;
it may not change the compiler, generate a scaled program, or bypass ordinary
filesystem discovery. This keeps the workloads recognizably maintained Mosaic
and Lattice while making every edit class explicit and reversible.

The initial maintained-program suite is **Mosaic and Lattice**. Mosaic supplies a
faster multi-module development rung; Lattice is the largest maintained scaling
rung and the decision workload for linker investment. Ruelex and Harbor may be
added only by advancing the incremental-fixture revision and providing every
required transformation, not by inheriting fresh-scaling membership
automatically.

### 4. Raw observation contract

Every raw observation records at least:

- schema version and incremental-fixture revision;
- compiler commit, target, optimization level, query-worker count, and host
  fingerprint;
- workload identity and compiler-derived source shape;
- scenario and exact A/B transformation identity;
- integer-nanosecond diagnostics-ready, codegen-ready, objects-ready, and
  runnable-ready durations, present only for their declared outcome;
- exact computed, reused, joined, invalidated, canceled, and evicted work by
  available compiler phase;
- current and peak retained artifact charge, dependency/input observations, and
  configured budgets;
- diagnostics/warning identity and executable fingerprint;
- fresh-oracle comparison outcome; and
- for a divergence, both warm and fresh outcome identities plus the first
  differing diagnostic, warning, or executable fingerprint.

The compiler's deterministic counters are authoritative for locality. Process
RSS may be recorded as an advisory host observation, but it cannot replace the
session's retained-charge gauges or serve as an exact eviction assertion.

Unknown fields are rejected. Schema changes and fixture changes advance separate
explicit revisions so a workload edit cannot silently reset its own history.

### 5. Sampling and reporting

The initial report is lower-frequency and advisory. It publishes raw JSON plus a
derived Markdown report as a workflow artifact; it does not append to
ADR-0067's content-addressed history or dashboard.

The initial suite declares two query-worker modes for every workload/scenario
row: exactly one worker (`-j1`) and the production automatic setting (`-j0`).
The raw observation records both the declared mode and the exact worker count
to which automatic mode resolved. Neither lane may substitute for the other.

Each workload/scenario/worker row uses at least five independent sessions.
Scenario order rotates deterministically between samples so one scenario does
not always receive the same thermal position. The report retains every raw
observation and shows median and median absolute deviation for each endpoint.

Infrastructure invalidity and compiler divergence have opposite publication
rules:

- fewer than the declared sample count, an unexpected source shape, or a
  malformed edit makes the run infrastructure-invalid and publishes no partial
  latency report; but
- a warm/fresh mismatch serializes the divergent observation and comparison
  details, publishes them as a failing workflow artifact, and exits nonzero. A
  tracking issue must be opened or linked before the failure is considered
  triaged.

No wall-time threshold gates ordinary CI in the first implementation. Focused
tests gate structural work and warm/fresh equality; an oracle mismatch is a
correctness failure, not a wall-time threshold. The scheduled report exposes
latency, memory, and scaling evidence without treating hosted-runner noise as a
compiler correctness failure.

### 6. Long-edit retention row

In addition to independent edit samples, the suite has one untimed or separately
timed bounded-retention sequence on a representative multi-module fixture. It
alternates a finite set of valid edits, errors, fixes, reachability additions,
and deletions through at least 1,000 revisions. Before the sequence starts, the
runner prepares one fresh oracle for every distinct fixture state; every warm
success compares with the corresponding precomputed oracle without paying a
fresh compile on each revision. The row asserts:

- every successful warm result matches a fresh session;
- canceled and failed revisions never publish a successful artifact;
- retained bytes and dependency observations respect the existing soft-budget
  and protected-overflow contract; and
- after protection releases, retained gauges return within their configured
  bounds rather than growing with revision count.

This row proves service viability. Its aggregate duration is not mixed with
single-edit latency rows.

### 7. Linker decision gate

RUE-1096 remains a design issue until maintained Lattice measurements exist.
Advance it only when one of these is true:

- the median objects-ready-to-runnable interval is both at least 20% of
  Lattice's median re-observation-to-runnable interval and at least 10
  milliseconds for a reached body-only edit;
- the same interval exceeds 20 milliseconds on the reference host; or
- across maintained program-size rungs, fresh-link work grows materially with
  whole-program size even though the edit's pre-link invalidation cone stays
  bounded, and it prevents an agreed interactive latency objective despite the
  earlier query endpoints remaining within that objective.

Crossing the gate authorizes the linker ADR, not an implementation. The ADR must
still decide symbol/data identity, placement, resizing, reverse relocation,
determinism, compaction, fallback, atomic publication, target runtime inputs,
and signing. Failing the gate records a measured deferral and shifts effort to
the dominant warm interval.

The versioned incremental manifest declares the reference-host identity and its
required hardware, operating-system, target, and automatic-worker fingerprint.
Only a matching host's automatic-worker Lattice row may decide the numerical
gate; other hosts and the `-j1` lane remain valid diagnostic evidence.

The numerical gate is a project-prioritization rule tied to that manifest's
reference host and fixture revision, not a language guarantee or permanent user
latency promise.

### 8. Ownership and consumers

The canonical driver/source-loader layer owns physical files and edit-host
re-observation. It will expose one reusable `FilesystemCompilerHost` from a
small driver library shared by the one-shot CLI, retained-session runner, and
watch mode. The host owns the existing import-discovery result and retained
`CompilerSession`; its operations open a root, re-observe the accepted-read
closure, acquire reached toolchain modules, and drive the canonical endpoint
queries. The binary CLI remains a thin options, diagnostics, and publication
consumer. `rue-compiler` remains free of host filesystem I/O.

The codegen-ready endpoint requires one narrow unstable compiler projection
that drives rooted `CodegenUnit` collection and returns only owned measurement
identity/count data. The existing unstable pre-link projection remains the
objects-ready endpoint, and the supported executable query remains the
runnable-ready endpoint. This does not expose installable artifacts or let a
consumer construct query keys.

`CompilerSession` owns query state and artifacts. The internal linker owns
executable production. The measurement runner owns only isolated fixture
preparation, declared transformations, timing boundaries, validation, and
report serialization. It depends on the driver library rather than importing
binary sources or recreating source loading.

The first user-facing consumer is expected to be an in-process `rue --watch`
host over the same driver/session boundary. A daemon, LSP integration,
persistent artifact codec, and stateful incremental linker remain separate
projects. None may introduce a peer compilation path.

## Implementation phases

- [x] **Phase 1: Contract and schema.** Add the versioned incremental manifest,
  raw report types, strict validation, and deterministic report derivation. —
  RUE-1245
- [x] **Phase 2: Canonical retained-session host.** Factor the existing
  filesystem-backed reload/session seam into the shared driver library, add the
  narrow unstable codegen-ready projection, and keep the CLI on that same
  owner. — RUE-1246
- [x] **Phase 3: Retained-session runner.** Drive the shared host through the
  diagnostics and three successful-compilation endpoints and serialize
  validated raw observations without duplicating source loading or compiler
  orchestration. — RUE-1251
- [ ] **Phase 4: Maintained edit fixtures.** Add exact Mosaic and Lattice
  transformations, structural expectations, and warm/fresh oracle coverage. —
  follow-up issue
- [ ] **Phase 5: Scheduled report.** Publish lower-frequency JSON and Markdown
  workflow artifacts with no dashboard/headline integration. — follow-up issue
- [ ] **Phase 6: Product host.** Add and measure `rue --watch` on the same seam,
  including cancellation, error/fix, import-set, and atomic-publication tests. —
  follow-up issue
- [ ] **Phase 7: Linker gate.** Record the Lattice decision result on RUE-1096
  and either advance its ADR or explicitly defer it. — RUE-1096

## Consequences

### Positive

- Rue gains maintained-program evidence for the incremental architecture users
  will actually exercise.
- Structural locality, latency, and retained memory remain distinct claims with
  appropriate authorities.
- Watch mode and any future service reuse the canonical compiler and filesystem
  contracts instead of growing a benchmark-specific path.
- Incremental-linker complexity requires measured justification.

### Negative

- Independent retained-session samples repeatedly pay an untimed large-program
  baseline, making the scheduled suite expensive.
- Exact source transformations create maintained fixture work whenever example
  programs change.
- Five samples expose trends rather than laboratory-grade microbenchmark
  certainty; very small endpoints still need focused local witnesses.
- Warm measurement adds a second report schema that must remain explicitly
  separate from the fresh-build system.

### Neutral

- This ADR changes no Rue language semantics or generated-program behavior.
- It does not promise persistent cross-process reuse.
- It does not choose an incremental-linker architecture.
- It does not make hosted-runner latency a merge gate.

## Rejected alternatives

### Add warm rows to ADR-0067's existing history

Rejected. Fresh processes and retained revision sequences have different state,
sampling, correctness, and versioning contracts. A shared history would make
incomparable observations appear continuous.

### Measure synthetic snapshots only

Rejected as the primary regime. Synthetic fixtures remain useful structural
tests, but they do not exercise canonical filesystem re-observation, real import
topology, or maintained-program linker scaling.

### Measure a benchmark-only compiler facade

Rejected. It would create a peer source-loading or phase-orchestration path and
could report wins absent from the product compiler.

### Implement persistent caching before watch mode

Rejected. In-process retention can first prove identity, invalidation,
cancellation, correctness, and memory policy without adding a serialization
trust boundary.

### Implement the incremental linker before measurement

Rejected. The current fresh-link interval is tiny on the Phase 12 fixture, while
stateful linking carries substantial correctness and platform complexity.
Maintained Lattice evidence is the required investment gate.
