---
id: 0077
title: "Endorsement inheritance for batch children"
status: accepted
tags: [architecture, compiler, performance, query-engine, parallelization]
feature-flag: null
created: 2026-08-18
accepted: 2026-08-18
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0063", "ADR-0071", "ADR-0073", "RUE-1577"]
---

# ADR-0077: Endorsement inheritance for batch children

## Status

Accepted 2026-08-18. Direction 1 was implemented while this record was
under review: PR #2517 (RUE-1583) seeds each batch's shared authority
with the spawning task's proved identities at construction
(`BatchValidationAuthority::seed_from_task`), one point on the
representation spectrum §"Implementation shape" leaves open, argued on
exactly the structured-lifetime grounds §1 states. The remaining gap is
the intra-batch first-touch race (RUE-1584), which lives inside §2's
sibling-to-sibling direction.

Follows the measurement note
`docs/notes/compiler-worker-scaling.md` (RUE-1577): the first
characterization of worker scaling found that adding workers makes
semantic analysis slower — 0.462× paired at `-j4` on Lattice, dragging
the whole compile to 0.986× against `-j1` — and located the cause in one
deliberate line. This ADR states the contract under which that line may
change. It is the query runtime's proof contract, which is why it needs
an ADR rather than an optimization commit: the question is when one
task's proved endorsement may justify another task skipping validation.

## Summary

At concurrency 1, `query_registered_adaptive_batch` evaluates every item
inline on the requesting task, so one `ValidationEndorsementScope`'s
`identities` set warms across the whole batch: item 400 finds what items
1–399 proved. Above one worker, `Task::batch_child` deliberately hands
each child `identities: AHashSet::new()` — an empty set — so every child
re-proves certificate cones from cold. Measured: validation traversals
8.40×, `proof_reacquisition_misses` 8.62× at `-j2` (flat to `-j4` — a
mode switch, not contention), +21.5% retired instructions on Lattice,
with the one-body-wide chain256 as the control proving the cost is paid
for configuring concurrency, not using it. `claims` is bit-identical at
every worker count: nothing is recomputed; everything is re-*proved*.

This ADR permits endorsement identities to flow to and between batch
children under a structured-lifetime argument, in two directions:

1. **Parent → child at spawn.** A batch child starts from a snapshot of
   (or shared read access to) the parent scope's proved `identities`
   instead of an empty set. Soundness argument: batch children are
   structured descendants — the parent's scope strictly outlives the
   batch join, and the leases under which the parent proved those
   identities remain held by the parent for the batch's whole duration.
   The child already inherits the parent's coarse retained-pin
   `fallbacks` and the published `batch_validation_authority` on exactly
   this argument; the exact identities are a refinement of the same
   inheritance, not a new trust edge.
2. **Sibling → sibling via published terminals.** A proof completed by
   one child during the batch propagates to siblings only through the
   existing published-authority mechanism (publish-on-terminal into the
   shared `batch_validation_authority`), never by direct mutable sharing
   of another task's live scope. Published authority is already trusted
   cross-task; this direction adds no new trust edge either, only more
   traffic through an existing one. Publication granularity within this
   direction is the implementation's choice: publishing per completed
   item, or per individually proved endorsement, before the proving
   child itself completes is still publish-on-terminal and adds no new
   trust edge — provided the atomicity invariant holds (an identity
   becomes visible in the same write transaction that already retains
   the lease or fallback backing it) and no task ever observes
   another's incomplete validation state.

What this ADR does NOT permit: honoring an identity no task in the
batch's structured lineage proved; endorsements outliving the batch join
or the revision they were proved against; and any sharing that requires
one task to observe another's *incomplete* validation state. The empty
set was guaranteeing these by construction; the implementation must
guarantee them by argument and falsifier instead.

## What this buys

Measured projection from the note's band model: bringing semantic
analysis merely up to the paired scaling CFG already achieves (1.85×)
is worth about **1.7× on a Lattice `-j4` compile**; the current 2.83×
Amdahl ceiling at `-j4` is unreachable without this. Chain-shaped
programs stop paying the 1.7–2.1× semantic penalty for concurrency they
cannot even use.

## Falsifiers

- **Byte identity across worker counts** (the standing hard gate):
  executables byte-identical at `-j1`/`-j2`/`-j4` on the ADR-0071
  reference workloads and chain fixtures; `claims` bit-identical at
  every worker count, unchanged from today's values.
- **`-j1` untouched**: the single-worker path never takes the new code;
  all `-j1` counters byte-identical to pre-change.
- **No forged endorsements**: a test-only harness in which a child is
  offered an identity that no task in its lineage proved must see
  validation fall through to the full check, not a `TaskLocal` hit.
- **Lifetime bounds**: an endorsement must not be honorable after the
  batch join completes or after the revision it was proved against is
  superseded — tests construct both and assert refusal.
- **The measured regression closes**: on the chain256 control,
  validation traversals at `-j2` return to within ~1.1× of the `-j1`
  values (today 2.09×); on Lattice the `-j2` traversal multiplier drops
  from 8.40× to near parity. Parallel-row validation counters get a
  one-time rebaseline (they are already permitted to vary run-to-run;
  the `-j1` row remains the structural probe).
- **Soundness regression suite**: the existing validation falsifiers
  (ADR-0073's) pass unchanged — durability semantics are orthogonal to
  who holds the proof.

## Rejected alternatives (from the measurement note)

- **Collapsing one-item batch windows** (the coordinator's 514
  single-key child spawns on chain256): real but small, needs a counter
  rebaseline, worth ~nothing on Lattice/Mosaic; may ride along only if
  the implementation gets it for free.
- **Changing the auto `-j` default**: a maintainer policy call that
  must not be decided from a 4-core host; explicitly out of scope here.
- **Doing nothing**: leaves `-jN` strictly dominated by `-j1` on every
  measured shape, which makes the parallel machinery a cost with no
  return on today's programs.

## Implementation shape

The spectrum (snapshot-at-spawn, copy-on-read, publish-on-terminal) is
acknowledged in the note; this ADR fixes the trust edges (§1 and §2
above) and leaves the representation choice to the implementation,
which must state its choice and argue it against the falsifiers.
Relevant sites, line numbers as of trunk `32c0e83b`:
`query_registered_adaptive_batch` (`crates/rue-query/src/lib.rs:8425`),
`Task::batch_child` (`:10370`),
`validation_endorsement_authority_at_raw` (`:10610`), and the
`proof_reacquisition_miss` branch (`:5925`). The 8/16-core CI
re-measurement flagged by the note is acceptance evidence for the
implementation, not for this proposal.
