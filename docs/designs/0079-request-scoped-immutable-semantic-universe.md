---
id: 0079
title: "Request-scoped immutable semantic universe"
status: proposal
tags: [architecture, compiler, performance, query-engine, type-system]
feature-flag: null
created: 2026-08-18
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0063", "ADR-0071", "ADR-0074", "ADR-0076", "RUE-1548"]
---

# ADR-0079: Request-scoped immutable semantic universe

## Status

Proposed. Follows the maintainer decision of 2026-08-18 on RUE-1548 to
pursue this direction, the measured comparison in
`docs/notes/rue-1548-request-scoped-universe-evaluation.md`, and that
note's Phase 0 verdict: per-body pool-index assignment order does not
reach emitted output for the named-nominal family, so a shared
deterministic assignment can be gated on byte identity rather than
rebaselined. ADR-0076 is the direct precedent — it moved the body-private
symbol space to a revision-shared one under the same kind of contract —
and its implementation lessons (the dual-space amendment, the retention
bound correction) are folded into this ADR's falsifiers up front.

## Summary

After ADR-0076 and the bounded subsets that followed it, the largest
remaining cost in a cold build is no longer any single algorithm but the
per-body re-materialization of shared semantic facts. Measured on current
trunk (callgrind, release compiler, cold `-O3 -j1`): the CFG-side stack —
fact selection into per-body closures, body-local epoch construction, and
domain projection — is ~24 percent of a cold Caldera build and ~22.7
percent of Lattice, in three mutually disjoint subtrees; the residual
semantic identity cluster (mint-and-key work per body) is conservatively
~10 percent more. The share is scale-stable from 1.2K to 8.6K bodies and
falls to ~2 percent on a chain-shaped program with no shared closure, so
the honest prediction is a saving proportional to closure reuse.

This ADR creates **one request-scoped immutable semantic universe** —
shared stable type, layout, drop, and callable-signature identities,
alongside ADR-0076's already-shared symbol space — built once per
revision from durable facts and consumed directly by CFG construction.
Bodies own instruction-local data and small local indexes only. The
selection, epoch-construction, and projection stack is eliminated by
construction, not optimized: with CFG consuming shared typed identities,
there is nothing to select into, materialize, or project per body.

The universe is not a process-global mutable arena. It is immutable once
its revision's declaration fixed point completes, owned by the request or
published root, generation-branded, and retired exactly like ADR-0076's
symbol generations — fail-closed refusal, never a dangling handle.

## The contract

1. **Deterministic identity assignment.** Universe ids are assigned in a
   deterministic order derived from durable identity (stable definition
   keys and ADR-0074 structural digests), never from first-touch or
   worker-scheduling order. The Phase 0 verdict shows output survives
   assignment-order permutation for named nominals, which is what permits
   gating on byte identity; deterministic assignment is still required so
   ids are run-stable and `-j` variation cannot introduce novel orders
   the perturbation tests never exercised. Each migrated family repeats
   the Phase 0 perturbation test before its vertical lands: anonymous
   nominals, array/pointer interning (whose creation order today follows
   fixed-seed `HashMap` iteration in `pre_create_array_types`), and
   callable/parameter identities each get the same env-gated
   reversed-order probe with a liveness witness.
2. **Byte identity is a hard gate per vertical, with one stated
   exception.** Emitted executables and `source_metrics` are identical
   before and after every vertical, on the fixed Lattice workload,
   Caldera, and a chain fixture, at `-j1` and a parallel worker count.
   Unlike ADR-0076, the deterministic work counters cannot all survive:
   the `cfg_materialization`, `cfg_local_epoch`, and
   `cfg_retained_charge` families describe work this ADR exists to
   delete, and `semantic_provider` materialization counters shrink
   toward once-per-revision. The counter contract is therefore split:
   counters describing surviving work must be identical; counters
   describing eliminated work must fall exactly as the design predicts
   (to zero or to once-per-revision magnitudes), stated per vertical in
   its PR, with `rue-perf-schema` fields retained for stored-report
   compatibility as usual. There is no silent drift category.
3. **Invalidation scope.** A body edit does not touch the universe: the
   universe is a function of declaration-derived durable facts only, and
   the query graph depends on per-fact slices (durable keys), not on the
   universe object, so a declaration edit invalidates exactly the bodies
   that observe the changed facts — the same dependency edges that exist
   today — plus the universe rebuild itself. A body analyzed against a
   superseded universe generation fails the authority check and re-runs,
   exactly ADR-0076 §4-5: retirement on eviction rather than on mint,
   with a small window of live generations so concurrently pinned
   revisions cannot abandon each other's bodies.
4. **`require_rir_authority` and the dense seam.** ADR-0076's dual-space
   amendment is assumed from the start rather than rediscovered: packed
   RIR's dense-ordinal encoding and any consumer that requires dense,
   body-local numbering keep a body-local remap (`Vec` from local ordinal
   to universe id), while equality, membership, and cross-body identity
   speak universe ids. The pointer-identity authority check binds a
   body's artifacts to the universe generation they were built against,
   and its liveness, exactly as the symbol space does today.
5. **Memory honesty.** ADR-0076's retention lesson is adopted as the
   expectation, not discovered again: body-local epochs are transient and
   never all resident, while the universe is resident for the whole
   revision. Peak RSS is therefore *not* expected to fall from sharing
   alone, despite the 12.5MB of duplicated CFG interners and 258K
   duplicated type-pool entries the status quo carries; the falsifier is
   "peak RSS does not rise materially" (±2 percent on the reference
   workloads), and any real reduction must come from the deleted
   transient churn, measured rather than assumed. Sparse universe ids
   indexed into dense per-body tables are a layout question the vertical
   PRs must measure (table sizing by universe cardinality), never a byte
   question, because exports are durable-keyed.

## Migration: verticals in descending measured order

Each vertical lands separately behind the full gate in §2, is
independently valuable, and stops the migration cleanly if its evidence
fails — the ADR-0076 phase discipline.

1. **Fact selection (~11 percent).** `select_materialization_facts`
   consumes the universe directly instead of copying durable facts into
   per-body `LocalMaterializationFacts` closures. The RUE-1547 interner
   and its counters retire here (stated counter change).
2. **Epoch construction (~7.4 percent).** `SemanticImportEpoch::new_local`
   and `materialize_local_body_with_types` are replaced by binding the
   body's instruction-local data to universe identities; the body-local
   type pool becomes the dense remap of §4.
3. **Domain projections (~5.4 percent).** `CfgDomainProjection` reads
   universe identities instead of projecting body-local domains for
   codegen.
4. **Identity cluster (~10 percent).** The provider mint-and-key work —
   parameter arenas, key clones, per-body pool writes — becomes
   once-per-revision universe construction. The durable-fact reads that
   feed it are RUE-1580's separate concern and are not claimed here.

## Falsifiers

- **Byte identity** per §2, per vertical, `-j1` and parallel, repeated
  runs; failure stops the vertical, and there is no rebaseline escape
  for emitted bytes.
- **Counter partition** per §2: an unexplained movement in a surviving
  counter kills the vertical exactly as a byte difference does.
- **Per-family perturbation** per §1 before each family shares:
  byte-identical output under reversed assignment order with a witnessed
  live knob.
- **Invalidation exactness** per §3: a test edits one declaration and
  asserts the invalidated body set equals today's; a body-only edit
  rebuilds no universe.
- **Authority** per §4: a body holding a superseded universe generation
  is refused and re-runs; the packed-RIR dense-ordinal assertion holds
  unchanged.
- **Retention** per §5: peak RSS within ±2 percent on the reference
  workloads, measured over interleaved pairs.
- **Warm edits**: ADR-0068 retained-session edit measurements unchanged
  for body edits; declaration edits pay one universe rebuild whose cost
  is measured and stated in the vertical that introduces it.

## Rejected alternatives

- **Shared base plus body-local overlays** (RUE-1548's middle option):
  reaches only the ~10 percent identity cluster while paying the same
  contract costs — shared index space, authority plumbing, dense-seam
  handling — because CFG still selects, materializes, and projects
  per body. Measured in the evaluation note; strictly dominated.
- **Continuing bounded subsets alone**: remains available and compatible
  (RUE-1587 took 3.75 percent this way after the evaluation), but the
  remaining stack is structural — the subsets shave leaves off work the
  universe deletes whole, and no enumerated subset reaches the ~24
  percent CFG-side stack.
- **Process-global or cross-revision mutable arena**: rejected in
  RUE-1548's own framing; it would trade exact invalidation and
  request-independence — the ADR-0063 properties — for sharing that the
  request-scoped form already achieves.

## Implementation shape

Phase 1 (audit, own commit): the type-identity analog of ADR-0076's
Phase 1 — inventory every ordered, value-bearing, or published use of
pool indices (`Type::as_u32`, dense table sizing, sort keys), convert or
annotate each, and land the per-family perturbation harness. Phase 2:
universe construction from durable facts with deterministic assignment,
authority plumbing, and the dense remap. Phases 3-6: the four verticals
above, in order, each with its stated counter partition and the full
falsifier battery. Every phase is independently shippable; the first two
are independently valuable (the audit removes latent order sensitivity;
the universe with zero consumers is dead code weight, so Phase 2 lands
fused with the first vertical).
