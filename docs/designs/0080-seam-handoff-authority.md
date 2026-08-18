---
id: 0080
title: "Seam handoffs: derived knowledge crosses scope boundaries with its authority attached"
status: proposal
tags: [architecture, compiler, performance, query-engine, principle]
feature-flag: null
created: 2026-08-18
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0073", "ADR-0074", "ADR-0075", "ADR-0077", "RUE-1576", "RUE-1584"]
---

# ADR-0080: Seam handoffs — derived knowledge crosses scope boundaries with its authority attached

## Status

Proposed, pending review. Generalizes what the RUE-1576 investigation
found and what its fixes (the collection-scope seam handoffs and their
observability) implemented. Those changes are merged; this ADR names the
principle they instantiate so the next phase boundary is designed against
it rather than rediscovered through a counter audit.

## Summary

When one compiler scope derives knowledge that a successor scope will
act on, the handoff across that boundary must carry the **authority**
that justifies the knowledge — the retained leases and pins under which
it was proved — together with the knowledge itself. A boundary that
hands over conclusions without authority forces the successor to
re-derive what its predecessor already established. Where a boundary
deliberately re-derives instead, it must say so at the seam and carry a
deterministic counter that makes the re-derivation cost visible. The
default is handoff; re-derivation is the documented exception, never
the silent one.

## Context

ADR-0073 split validation into two planes: certificates are the
**knowledge** plane (what has been proved), terminal leases and retained
pins are the **authority** plane (the right to rely on it without
re-checking). That split is what makes durable incremental validation
possible — and it creates a failure mode the planes' unity used to
prevent: knowledge can now cross a boundary while authority stays
behind.

A deterministic-work audit of a cold release build of the Lattice
reference workload (RUE-1576) found this shape repeatedly, at different
sizes:

- **Validation amplification (the motivating case).** Every one of the
  23,862 `proof_reacquisition_misses` on a cold one-worker Lattice
  build clustered at scope boundaries: `body_closure_collection` #1
  re-leasing the semantic cone that `declaration_graph_collection` #1
  had just certified, and the backend scopes re-leasing the CFG and
  codegen cones. Each miss was a certificate whose lease had not
  crossed the seam, and each triggered a demand cascade re-walking a
  certified cone solely to reacquire terminal leases — validation
  performing roughly 31× the deterministic work of the computation it
  was validating.
- **The double parse.** Wave-granular import discovery (ADR-0075)
  parses every module, then the canonical `compiler.parse-module`
  query parsed each of them again: the parse *results* informed
  discovery, but the parsed artifact itself never crossed into the
  query's custody. The fix (`stage_module_parses`) is a literal
  handoff: each staged parse is consumed at most once, on exact
  `SourceId` identity, "a work handoff rather than a second parse
  authority."
- **Smaller echoes.** Precompute passes re-scanning full-body RIR for
  facts an earlier phase had already localized carried the same shape
  at smaller cost: derived knowledge existed, the boundary dropped it,
  a successor re-derived it.

The seam-handoff fixes for the motivating case wired the existing
borrowed-fallback machinery across three phase boundaries (declaration
publication → body closure, optimized-CFG batch → codegen batch,
codegen batch → object projection and backend publication). Misses went
23,862 → 0, validation demands 23,872 → 10, validation node visits
242,296 → 48,035, with the output hash and all 121 non-validation
counters byte-identical. The follow-up made the handoff's failure mode
observable: a `publication.cone_retention_failures` counter carried
into the benchmark schema, a debug assertion, and a pipeline gate
pinning both that counter and `proof_reacquisition_misses` to zero at
one worker.

## Decision

The contract, for any boundary where a scope publishes derived
knowledge that a successor scope consumes:

1. **Hand authority with knowledge.** The publishing scope transfers
   (or shares) the retained authority under which the knowledge was
   proved — today: a `RetainedPinSet` published through a collection
   root and picked up by the successor's endorsement fallbacks. The
   successor validates by borrowing, not by re-leasing.
2. **Bound the authority's lifetime structurally.** A handed-off lease
   set lives until the publisher's next successful replacement
   (replace-on-next-success), mirroring the backend-root publication
   pattern. Authority never outlives the revision it was proved
   against; ADR-0073's durability falsifiers continue to hold
   unchanged.
3. **Degrade loudly, never silently.** Handoffs are best-effort — an
   unretained cone leaves behavior exactly as it was before the
   handoff existed — but the degradation must increment a
   deterministic counter (and assert in debug builds), so a weaker run
   can never masquerade as the stronger one. Best-effort is permanent
   policy, not a transitional state on the way to fail-closed: the
   counter and gate already make silent degradation impossible, and
   failing closed would convert a performance event into a compile
   failure without adding safety. Seams keep their counters for as
   long as they exist.
4. **Pin the exactness regime.** Where deterministic-work counters are
   exact (one worker, per ADR-0067's contract), a gate pins the seam
   counters to their designed values — today
   `publication.cone_retention_failures == 0` and
   `proof_reacquisition_misses == 0` on a full compile.
5. **Documented re-derivation is the only alternative.** A boundary
   that intentionally re-derives (because retention would cost more
   than re-derivation, or the authority cannot be safely shared) says
   so in a comment at the seam and carries a counter making the cost
   visible in `compiler_work`. Undocumented, uncounted re-derivation
   discovered at a boundary is a defect, not a style choice.

Boundary taxonomy this contract applies across:

- **Phase seams** — collection scope to successor collection scope
  within one rooted compile. Implemented; the motivating case above.
- **Scheduling seams** — parent task to spawned batch child, sibling
  to sibling. Contract fixed by ADR-0077 (endorsement inheritance for
  batch children). Its parent-to-child direction is implemented:
  `BatchValidationAuthority::seed_from_task` copies the spawning
  task's proved identities into the batch's shared authority at
  construction (RUE-1583), cutting automatic-worker misses by 32–90%
  across the reference workloads. The open instance is the intra-batch
  first-touch race — concurrently started siblings validating
  overlapping cones before any has published (RUE-1584); closing it
  means mid-item lease-plus-endorsement publication, which touches the
  atomicity contract `publish_child` maintains.
- **Revision seams** — one append-only revision to the next. Already
  governed by ADR-0073's durability semantics; this ADR adds nothing
  there beyond principle 2's deference to it.

This ADR fixes the contract, not the representation. Collection roots
and borrowed fallbacks are today's mechanism; a future mechanism
satisfies this ADR if it preserves the five principles and the
falsifiers below.

## Falsifiers

- **Zero-miss exactness gate**: a one-worker compile of a maintained
  multi-module workload reports `proof_reacquisition_misses == 0` and
  `publication.cone_retention_failures == 0`. (Standing test:
  `publication_seams_leave_no_lease_reacquisition_cascades`.)
- **Bit-stable semantics**: seam handoffs change no outputs — output
  hash and all non-validation deterministic counters byte-identical
  before and after any handoff change, on the ADR-0071 reference
  workloads.
- **No silent degradation**: forcing a cone-retention failure in a
  test must be visible in the counter and fatal under debug
  assertions; a run with a degraded seam must be distinguishable from
  a healthy one by counters alone.
- **Schema additivity**: seam-health counters are additive schema
  fields — older `compiler_work` reports decode with zero defaults.
- **Authority lifetime**: no handed-off lease is honorable after its
  publisher's replacement or its revision's supersession (shared with
  ADR-0073 and ADR-0077 falsifiers).

## Implementation Phases

- [x] **Phase 1: Phase-seam handoffs** — RUE-1576 (declaration →
  body-closure, CFG → codegen, codegen → backend scopes; merged)
- [x] **Phase 2: Seam-health observability and gate** — RUE-1576
  follow-up (counter through `OneShotMetrics` into the benchmark
  schema, debug assertion, one-worker zero-miss pipeline gate; merged)
- [x] **Phase 3a: Parent-proof seeding at batch creation** — RUE-1583
  (`BatchValidationAuthority::seed_from_task`; merged)
- [x] **Phase 3b: Intra-batch first-touch race** — RUE-1584 (per-proof
  publication into the shared batch authority, authority-held leases as
  sibling-visible proof, and parent lease seeding across waves; merged).
  Automatic-worker reacquisition misses fell 97.7% on Lattice, 95.5% on
  Meridian, 89% on Caldera, with byte-identical executables at every
  worker count. The residue is simultaneous first probes inside one
  scheduling quantum, which only exact-identity inheritance
  (ADR-0077 direction 1) can collapse further.
- [ ] **Phase 4: Boundary audit** — sweep remaining publication
  boundaries for undocumented re-derivation; each either gains a
  handoff or a seam comment plus counter

## Consequences

### Positive

- New phase boundaries get designed against a named contract instead
  of being found later as counter anomalies; "where does the authority
  cross?" becomes a review question.
- Silent re-derivation converts into either a cheap borrow or a loud,
  counted, documented cost — the audit that produced this ADR becomes
  continuous rather than episodic.
- The mechanism reuses existing machinery (endorsement fallbacks,
  retained-pin sets, publication roots); no new proof semantics.

### Negative

- Handoff wiring couples lease lifetimes across scopes: a publisher's
  retained set now lives until its next success, which held ~1 MiB of
  additional peak RSS on Lattice. Larger workloads inherit that
  retention proportionally.
- Every new boundary carries a small obligation (handoff or
  comment-plus-counter), which is friction where re-derivation is
  genuinely trivial.

### Neutral

- Best-effort handoffs mean correctness never depends on a seam: a
  failed handoff is a performance event, not a semantics event. The
  counters exist precisely because that makes failures otherwise
  invisible.

## Future Work

- Surfacing seam-health counters as an ADR-0067 dashboard series once
  more than one seam counter exists.

## References

- ADR-0073 — validation-certificate durability (the knowledge/authority
  split this contract governs)
- ADR-0075 — wave-granular import discovery (the parse-stage handoff)
- ADR-0077 — endorsement inheritance for batch children (the
  scheduling-seam contract)
- RUE-1576 — the motivating investigation; RUE-1584 — the open
  scheduling-seam instance
