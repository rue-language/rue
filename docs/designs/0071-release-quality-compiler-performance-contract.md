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
relates: ["RUE-1470", "RUE-1478", "RUE-1543", "ADR-0063", "ADR-0067", "ADR-0068"]
---

# ADR-0071: Release-quality compiler performance contract

## Status

Accepted by Steve Klabnik and Dorian Scheidt on 2026-08-12. This ADR sets a
product target and the rules for pursuing it. It does not replace the compiler
architecture in ADR-0063 or the measurement protocols in ADR-0067 and
ADR-0068. It turns their implemented mechanisms into an explicit performance
contract.

**Amendment 1 (RUE-1543) is a proposal and is not accepted.** It measures what
Decision 2's build-boundary evidence costs in the published store, finds that
most of it is duplicate or unread, and recommends a smaller representation that
keeps every guarantee a reader can re-derive. Everything above and below it
remains the accepted text; nothing in the amendment is implemented. Its
companion, ADR-0067 Amendment 1, rules on how such a change is versioned and
what happens to the records already published. See
[Amendment 1: boundary evidence costs more than it proves (RUE-1543)](#amendment-1-2026-08-16-boundary-evidence-costs-more-than-it-proves-rue-1543).

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

## Amendment 1 (2026-08-16): boundary evidence costs more than it proves (RUE-1543)

**Status: proposal. Not accepted, not implemented.** This amendment answers the
question RUE-1543 asks — does a run object need to carry the complete
build-boundary evidence of every measured process — and recommends *no*. It
needs a maintainer ruling before it means anything. ADR-0067 Amendment 1 is its
companion and rules on the versioning and on the records already published;
this one rules only on what the evidence must contain.

(Figures throughout are the 2026-08-16 corpus the analysis was performed on.
Re-measured 2026-08-23 at rebase: 1,619 records, 3,470.7 MiB, growth
unchanged at ~284 MiB/day — the supporting note carries the details.)

**This amendment cannot be adopted on its own.** Changing what a record contains
changes how it is written down, and readers today accept exactly one encoding:
`validate_run` refuses any `schema_version` but the current constant, with no
compatibility path. So ADR-0067 Amendment 1's Question 1a — dual v1/v2 decoding
and validation, and the amendment of that invariant — is a prerequisite for
anything here reaching the store. Accepting this amendment while declining that
one would produce a producer whose records every reader rejects.

### Recommendation

**Retain one complete boundary witness per workload observation, and a pair of
SHA-256 digests per process, instead of one complete evidence entry per
process.**
Concretely, a run object gains a run-level `boundary` block for the parts that
are invariant across the whole run, each workload observation gains a
workload-level `boundary` block for the parts invariant across its processes,
and each sample carries **two digests per process**: one over that process's
`{runner, compiler}` and one over its `compiler_work`.

**The digest is split because the two cross-process guarantees it has to carry
are not equally conditional.** `check_boundary_evidence` requires every
process's `runner.output_sha256` to agree for every protocol-2 record
(`validate.rs:569-580`), but requires `compiler_work` to agree only
`if policy.worker_setting == WorkerSetting::One` (`validate.rs:588-600`), and
says why: "Parallel rows deliberately include schedule-dependent joins, reuses,
and validation paths; those are distribution evidence, not output identity."

Every boundary epoch in `performance/manifest.toml` is `worker_setting = "one"`
today, which is exactly why the measurement finds one distinct `compiler_work`
per workload. A single combined digest would promote that observed fact to a
structural assumption, and this ADR already commits to breaking it: Decision 2
names the 150 ms automatic-worker target and Decision 7 requires the report
across `WorkerSetting::REFERENCE_MATRIX`. On a `two`/`four`/`eight`/`automatic`
boundary epoch, `compiler_work` varies across processes by design — so no
workload-level witness could hold it, every combined digest would differ from
the witness and from every other process, and a reader would lose the ability
to re-derive the guarantee that is *not* gated. Output identity would become
unverifiable, because a digest mismatch would no longer distinguish "a
different binary" from "a different schedule".

Split, the invariant half stays shared and re-derivable under any worker
setting, and the schedule-dependent half stays per-process and comparable.

#### What each digest is taken over

A digest a reader cannot recompute is a producer assertion, which is the
objection this amendment raises against encoding A. So the preimage has to be a
function of the stored record alone. Four rules settle it, and the implementing
change owes a test for each.

**1. The preimage is the complete original per-process value, not the residue
left after hoisting.** Hoisting is a *lossless partition*: every field of a
process's `runner` and `compiler` lands in exactly one of the run-level block
or the workload-level block. A reader reassembles the whole `{runner,
compiler}` pair from those two blocks and digests that, so the digest commits
to the evidence as measured rather than to whatever survived the encoding.

This is what makes the rule robust to the partition itself being adjusted: the
digest does not depend on *where* a field was hoisted to, only on the partition
being complete and disjoint. The implementing change must assert that
round-trip — reassemble, compare against the original evidence, require
equality — because a field landing in neither block, or in both, is otherwise
invisible.

One boundary of the partition is pinned explicitly: "output identity" is four
fields, not two — `runner.output_sha256`,
`runner.output_size_bytes`, `compiler.emitted_output_sha256` and
`compiler.emitted_output_size_bytes`. All four are workload-level, and
`accepted_inputs` joins them there.

**2. Canonicalization is `canonical_json`, unchanged.** Object keys sorted by
Unicode scalar sequence, no insignificant whitespace, floating point an error
rather than a rounding decision (`canonical.rs:51`). This is deliberately the
same function `content_address` already uses: a second canonicalization path
would eventually disagree with the first, and the digests would then certify
something other than what naming enforces.

**3. There are no omission or default rules, and none may be added.**
`RunnerBoundaryEvidence` and `CompilerBoundaryEvidence` are both
`deny_unknown_fields` with no `skip_serializing_if` and no `serde(default)` on
any field, so every field is always present and the preimage cannot depend on
what a writer chose to emit. Adding `skip_serializing_if` to either type later
would silently change the preimage for records already published — the same
class of accident `Stored` exists to prevent — so this amendment forbids it for
these two types by name.

**4. Each digest is domain-separated, and `schema_version` does not
participate.** The digest is `SHA-256(tag || canonical_json(value))` with a
fixed ASCII tag ending in a newline:

| Digest | Tag |
| --- | --- |
| `boundary_processes` (over `{runner, compiler}`) | `rue.boundary.identity.1\n` |
| `boundary_work_processes` (over v3 `compiler_work`) | `rue.boundary.work.2\n` |

Schema-v2 records remain readable with their historical `rue.boundary.work.1\n`
domain and preimage: v2 omitted the candidate-plan and canonical-RIR groups.
Readers reconstruct that exact historical shape before validating its digest;
they do not default fields and reserialize the current work type. New v3
compiler work exposes the query-native candidate construction and materialization
taxonomy, while semantic inference/precompute counters remain governed legacy
evidence and timing distributions never project candidate structural work. The
v3 public `CompilerWork` shape names those candidate groups explicitly and
retains only the unrelated semantic-analysis evidence; the retired body-lowering
and index subgroup is accepted only by the private v2 decoder and is never
rendered or emitted by v3.

The scaling-report wire revision is 25. Revision 25 adds query-worker physical
thread-birth and coordinator-residual construction evidence; it does not change
the query work taxonomy described here. Canonical-RIR
presentation reports `requests_computed` only: the current presentation query
does not reuse a published result, so no synthetic reuse counter is exposed.
The candidate-plan fields report query-terminal computed/reused counts and
successful output quantities independently of timing distributions.

Run-object schema numbers are globally unique across the two encodings: schema
1 is the historical full-evidence shape, schema 2 the historical stored shape,
schema 3 the current stored shape, and schema 4 the current full-evidence
shape. Schema 1 is validation-only; its retired work taxonomy is not losslessly
representable in schema 3 and `encode_stored_v3` refuses it.

| `schema_version` | Encoding shape | Current policy |
| ---: | --- | --- |
| 1 | historical full evidence | validate only; no lossless migration |
| 2 | historical stored witness and work.1 digests | decode and validate |
| 3 | current stored witness and work.2 digests | producer output |
| 4 | current full evidence and query-native work | producer's in-memory form |

Without the tag, the two digests are computed over different types but by the
same construction, and a record's `content_address` is a third — domain
separation is what keeps a value from ever being read as the wrong kind. The
trailing `.1` versions the *digest scheme*, which is why `schema_version` is
deliberately not inside the preimage: the process evidence is a property of the
process, not of the record encoding that carries it, and folding the record's
version into it would change every digest on a re-encode that changed nothing
about what was measured. If the preimage ever changes, the tag increments and
the two are distinguishable by construction.

A worked, byte-exact vector — the canonical JSON of one `runner` value, its
domain tag, and the resulting digest, checkable with `sha256sum` — is in the
supporting note.

Keep one `critical_path` per workload observation (encoding **S4** in the
supporting note). The strictly smaller variant that drops it entirely (**S1**)
is the true floor, and either is defensible; S4 is recommended because it costs
0.6 percentage points and preserves the only per-commit critical-path record
the project has outside a scaling rerun.

**Which process supplies it is part of the encoding, not left to the encoder.**
`critical_path` is the one member measured to vary across processes, so "one per
workload observation" is ambiguous until the choice is pinned: two conforming
encoders could otherwise publish different per-commit critical paths for the
same observation, and nothing downstream could tell.

The rule is **the first process of the first sample** — `samples[0]`,
`boundary_evidence[0]` — and the record states its provenance rather than
relying on the convention:

```
workloads[i].boundary.critical_path
workloads[i].boundary.critical_path_source = { sample_index: 0, process_index: 0 }
```

Three things make that the right choice rather than an arbitrary one. It is
already the project's convention: every existing consumer takes
`boundary_evidence.first()`, in eight places in `scaling.rs`. The ordering it
selects on is deterministic — `measure_sample` runs a batch serially and pushes
each process's evidence in spawn order (`measure.rs:139-164`), so index 0 names
the same process on every platform and every rerun. And carrying
`critical_path_source` explicitly means a reader can tell *what* it is holding;
without it the field is a number with no stated provenance, which is how a
representative sample gets misread as a witness.

A median over processes was considered and rejected: `CompilerCriticalPathEvidence`
is a struct of histograms and counters rather than a scalar, so a median needs a
per-field rule and a tie-break, and it would produce a value no process actually
observed. The same selection rule and the same `_source` field carry the
representative `compiler_work` a parallel epoch retains, so the encoding has one
convention for "a sample, not a witness" rather than two.

Measured against all 1,188 published records. The measured encodings carry one
digest per process; the split adds a second, whose cost is derived beneath the
table.

| | Branch total | Epoch-6 record, x86-64 | Epoch-6 record, macOS | Growth/day |
| --- | ---: | ---: | ---: | ---: |
| today | 1,481.2 MiB | 1,639.5 KiB | 15,933.7 KiB | 288.6 MiB |
| S4, one digest (measured) | 52.6 MiB (3.6%) | 203.8 KiB | 350.2 KiB | 11.4 MiB |
| **S4, split digest (derived)** | **59.3 MiB (4.0%)** | **210.2 KiB** | **412.1 KiB** | **12.5 MiB** |
| S1, one digest (measured) | 44.9 MiB (3.0%) | 134.1 KiB | 280.1 KiB | 8.2 MiB |
| S1, split digest (derived) | 51.6 MiB (3.5%) | 140.5 KiB | 342.0 KiB | 9.3 MiB |

The split costs one extra 64-character digest per process, and the supporting
note measures that quantity directly rather than estimating it: S1 stores one
digest per process, S3 replaces it with a process count, and the difference is
**6.7 MiB** across the branch's 105,489 process entries — 66.6 bytes each,
which is a 64-hex string plus its quotes and separator. The second digest costs
the same 6.7 MiB and 1.1 MiB/day. Per-record figures scale with that epoch's
process count: 99 for an epoch-6 x86-64 record, 951 for macOS, which is why the
macOS record moves most.

The recommendation is unchanged by this: 4.0% instead of 3.6% is still a 25×
reduction, and it buys an encoding that keeps working on the parallel boundary
epoch Decision 7 already requires. The derived rows are arithmetic on a measured
per-digest cost rather than a fresh serialization; re-measuring them belongs to
the implementing change, with the derive-level A/B.

### Why this is not a loss of precision

Decision 2 requires runner, manifest and compiler to agree before a sample is
admissible. The proposal keeps that intact, because the entries being removed
are not independent observations.

**They are byte-identical.** Across every workload of every record examined,
the number of distinct values among a workload's evidence entries is exactly
one for `runner`, one for `compiler` including `accepted_inputs`, and one for
`compiler_work`. Only `critical_path` varies. A macOS `startup` observation
stores 792 identical copies of a 5.4 KiB structure. This is by construction:
`check_boundary_evidence` *requires* the output digest and, at one worker,
`compiler_work` to be equal, and the epoch fixes the configuration and the
source closure.

**The cross-process guarantees survive exactly.** A reader re-derives "all N
processes produced the same output" and "all N reported identical deterministic
work" by comparing the N stored digests and checking they equal the digest of
the stored witness — the same conclusion, from 64 hex characters per process
instead of 5.4 KiB.

**The N copies were never N authorities.** They are one runner process
reporting what it observed N times, and that runner already refuses to emit a
sample whose processes disagree.

**Nothing that consumes the branch reads the rest.** The complete consumer
inventory of stored evidence is one function, `check_boundary_evidence`. The
heavy checks that genuinely need a full `critical_path` — the 26-row
`REQUIRED_SEMANTIC_EVIDENCE` table, the attribution partitions, the RUE-1510
signature-parse deletion proof — run in the producing process before the sample
is assembled, and are unaffected. `verify_input_provenance` re-hashes every
accepted input against the filesystem, also before storage. The scaling report
that Decision 7 calls for reads `critical_path`, but it measures fresh
processes and writes a workflow artifact; it never opens the data branch.

**What is actually dropped**, stated plainly: per-process `critical_path`
histograms for processes 2..N, and the ability to re-check each of those
histograms' internal consistency. That check compares a histogram's bucket sum
against the `count` beside it in the same object — it can catch a corrupt
serializer, and nothing about the compiler. Under S4 it survives for one
process per workload; under S1 it does not survive at all.

### Alternatives considered

Measured over the same 1,188 records:

| Alternative | Branch total | Why not |
| --- | ---: | --- |
| drop `log2_buckets` (39% of an entry, ~88% zeros) | 1,033.7 MiB (69.8%) | the obvious waste is not the problem; leaves ~196 MiB/day |
| sparse `log2_buckets` | 1,129.2 MiB (76.2%) | worse than deleting them |
| `accepted_inputs` → digest | 1,215.9 MiB (82.1%) | large only for `lattice`; 18% of the branch |
| keep process 0 of each sample | 584.3 MiB (39.4%) | halves it, and the half kept is still 60× too large |
| whole array → one digest | 24.9 MiB (1.7%) | cheapest and weakest: a digest of discarded bytes turns every re-derivable guarantee into a producer assertion |
| S2 — S1 with `accepted_inputs` digested | 34.2 MiB (2.3%) | `accepted_inputs` is the only record of what the compiler actually read, including `std`, and is not re-derivable from the epoch's workload pin; 3.4 MiB/day is worth it |

Retaining the complete evidence as a **workflow artifact**, with its digest in
the record, composes with any of these and is recommended alongside: it keeps
full-depth auditing for the artifact retention window at zero cost to the
store, and it is the only arrangement in which a stored digest of dropped
evidence means anything.

### Consequences

- Decision 2's checker does the same work over one witness per workload instead
  of one entry per process: 1,860 `validate_against` calls across the branch
  instead of 105,489.
- Decision 5's evidence hierarchy is unchanged. Levels 1, 2, 4 and 6 live in
  fields this amendment does not touch; level 5's deterministic counters are
  retained once per workload; level 3 was never in the run object.
- Decision 7's critical-path and scaling questions are answered by the scaling
  report, which is unaffected.
- A future boundary variant — retained-session, package-cache, daemon-backed —
  inherits the same shape and the same per-process digest rule. So does a
  parallel boundary epoch: the split is what makes the shape survive
  `worker_setting` other than `one`, since only the `compiler_work` digest is
  permitted to vary across processes there, and the `{runner, compiler}` digest
  must still equal the workload witness for every process.
- The producer keeps building the full evidence in memory and keeps validating
  it in full. Only what reaches storage changes.

### What could not be verified

The re-encoded corpus cannot be validated end-to-end by the current reader:
`RunObject` uses `deny_unknown_fields`, and a protocol-2 suite requires
`boundary_evidence.len() == batch_size`, so every re-encoded record is refused
by construction until the schema change exists. The equivalence claim was
therefore checked structurally rather than through `rue-bench derive`: with the
evidence keys stripped from both sides, 0 of 1,188 records differ, so every
value a chart is drawn from is byte-identical. A derive-level A/B, in the style
of RUE-1542's byte-identical report comparison, is the acceptance evidence for
the implementing change rather than for this proposal.

## References

- [ADR-0063: Parallel demand-driven incremental compilation](0063-parallel-demand-driven-incremental-compilation.md)
- [ADR-0067: Compiler performance measurement, epochs, and dashboard](0067-compiler-performance-measurement.md)
- [ADR-0068: Incremental edit-scenario performance measurement](0068-incremental-edit-performance-measurement.md)
- [Post-ADR-0063 cold compiler architecture audit](../notes/post-adr-0063-cold-compiler-architecture-audit.md)
- [Compiler scaling reports](../process/compiler-scaling.md)
- [Boundary evidence and the size of performance-data-v1](../notes/performance-boundary-evidence-size.md)
  — Amendment 1's measurements, consumer inventory, and encoding prototypes.
