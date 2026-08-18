---
id: 0075
title: "Wave-granular import discovery revisions and cumulative exhaustion witness"
status: implemented
tags: [architecture, compiler, incremental, performance, query-engine]
feature-flag: null
created: 2026-08-17
accepted: 2026-08-16
implemented: 2026-08-18
spec-sections: []
superseded-by:
relates: ["ADR-0063", "ADR-0071", "ADR-0073", "ADR-0074"]
---

# ADR-0075: Wave-granular import discovery revisions and cumulative exhaustion witness

## Status

Implemented. Approved by Steve Klabnik in session on 2026-08-16 and
proposed via PR #2464. The wave core (items 1-3) was implemented
2026-08-17 via PR #2472: one revision per discovery wave, batch stamp
verification, and the falsifier suite, with discovery revisions on a
depth-n chain collapsing from n to 1.

Item 4 — the cumulative exhaustion witness replacing the closing
whole-plan re-root — was deliberately held back at that point, because
the re-root measured at roughly 1.8% of a deep-chain compile and the
verified wave win did not need to wait for it. It landed separately via
PR #2500, which removes the closing dispatch: frontier roots on the
32-module chain fixture fall from 63 to 32, now an exact assertion rather
than a bound, and the witness-soundness falsifier below is in place.

No language-semantics change: the final published import plan, resolution
results, and emitted bytes are byte-identical. What changes is the
*granularity* of discovery's revision ledger — a deliberate coarsening of
the incremental story during discovery, spelled out below.

## Summary

Import discovery is semantically one-import-hop deep per round: each round
resolves the currently-known open imports, batch-publishes the newly
discovered sources as one immutable input revision, and only then may the
next layer's imports be resolved against it. A depth-n chain therefore
takes n rounds *by contract*. The 2026-08-16 work made each round's
marginal cost small — certificate misses O(1) (ADR-0073), frontier
dispatch delta-scoped, per-round plan reads delta-scoped — but the round
*count* itself remains O(depth), and every round still pays fixed
overheads (revision mint, ledger append, batch publication, validation
sweep, thread coordination). On the 1,024-module chain those fixed costs
dominate what remains of the multi-second discovery phase; no per-round
optimization can remove them because the contract mandates the rounds.

This ADR changes the unit of publication from the import *hop* to the
discovery *wave*:

1. Within a round, after resolving open imports yields newly discovered
   sources, the loop reads and parses those sources immediately and
   resolves *their* imports in the same round, iterating until the
   frontier yields no new sources. One wave = the transitive closure
   reachable from the round's starting open set.
2. The wave then batch-publishes **one** input revision containing every
   source it discovered, with the ledger recording every read the wave
   performed, in the same deterministic order the per-hop contract would
   have recorded them (module-path order within discovery order — the
   exact order is part of the implementation's falsifier, derived from
   the current per-round order, not invented).
3. A depth-n chain thus publishes O(1) discovery revisions instead of n.
   Rounds still exist (a wave can be followed by another if publication
   raises new demand); the loop structure, fail-closed frontier guard,
   and re-close path are unchanged.
4. The closing whole-plan re-root — one O(plan) dispatch that exists so
   `publish_trusted_toolchain_successor`'s witness can verify "the whole
   predecessor plan is exhausted" — is replaced by a **cumulative
   witness**: the union of dispatched roots provably covers the plan
   (maintained incrementally as a count/bitmask over plan segments, not
   recomputed), and the final wave's frontier returned no new demand.
   The witness remains fail-closed: if the cumulative record cannot
   prove coverage, the whole-plan re-root runs as today.

## What this buys

- The last O(depth) structural factor in discovery. With per-round costs
  already delta-scoped, wave-granularity turns a depth-n chain's
  discovery into O(n) total work with O(1) revision overhead — this is
  the difference between "deep dependency chains are a pathological
  shape" and "depth is free."
- Lattice (import depth ≈ 12) drops ~11 revision publications and their
  validation sweeps from every fresh build.
- The closing re-root's O(plan) dispatch disappears in the common case.

## What this costs: invalidation granularity during discovery

Today, a source file that changes *between rounds* is caught at the next
round's revision publication (stamp mismatch on the recorded read), and
invalidation re-runs from that round. Under waves:

- **Mid-wave mutation.** A source read early in a wave that changes
  before the wave publishes is caught at wave publication — every read
  the wave performed is stamp-verified at publish, batch, fail-closed.
  The re-run unit is the wave, not the hop. This is strictly coarser
  re-execution on a strictly narrower race window (discovery of a
  typical project is tens of milliseconds), and it can never publish a
  revision that mixes stale and fresh stamps — the falsifier below.
- **Interruption/resume.** A discovery interrupted mid-wave resumes from
  the last published revision, re-running the whole wave. Today it
  resumes from the last hop. Deep-chain cold builds trade at most one
  wave of redone work (bounded by the plan) for n−1 avoided
  publications; incremental builds are unaffected because their
  discovery starts from a full previous plan and typically completes in
  a single trivial wave.
- **Downstream consumers of revision identity** (ADR-0073 epochs,
  certificates) see fewer, larger revisions. ADR-0073's monotonicity
  proof obligations are per-publication and carry over unchanged — a
  wave publication is still strictly additive; it is *more* additive at
  once.

## Falsifiers

- **Plan identity**: final published plan, resolution diagnostics, and
  emitted executables byte-identical on the ADR-0071 reference workloads,
  chain fixtures (n = 64/256/1024), and the answered-but-still-open
  fixture from the 2026-08-16 rooting change.
- **Stamp atomicity**: a test mutating a source mid-wave (test hook
  between wave read and wave publish) must observe a fail-closed wave
  re-run and a published revision whose recorded reads all verify; no
  mixed-stamp revision can ever be observed.
- **Revision count**: chain fixtures publish O(1) discovery revisions
  (premerge-gated exact count per fixture, alongside the existing
  `certificate_misses` and `import_frontier_roots_requested` gates).
- **Witness soundness**: a test that forges an uncovered plan segment
  (test-only) must see the cumulative witness refuse and the whole-plan
  re-root fire — the fallback stays reachable and correct.
- **Determinism**: ledger read order and revision contents identical
  across runs and worker counts.

## One-time rebaselines (expected, bounded)

Deterministic counters that count per-revision events (revisions
published, validation sweeps, ledger appends, per-round dispatch families)
shift once to their wave-granular values; the chain premerge gates are
re-pinned to the new exact counts in the same change. Counters for parse,
semantic, CFG, codegen, and link work must be byte-identical — waves
reorder *when* discovery work happens, never *what* work happens.

## Rejected alternatives

- **Speculative parallel resolution across future rounds**: reintroduces
  scheduling-dependent work sets; violates the determinism falsifier.
- **Removing rounds entirely** (single implicit wave with streaming
  publication): erases the fail-closed batch verification point that
  makes mid-discovery mutation detection deterministic; the wave keeps
  the barrier, there is just one barrier per closure instead of per hop.
- **Relaxing the witness without the cumulative record** (trust the
  frontier's empty answer alone): weaker than today's contract; the
  cumulative coverage record keeps the witness's meaning intact.

## Implementation shape

The round loop in `crates/rue/src/source_loader.rs` gains an inner
wave-extension loop between "resolve open imports" and "publish": drain
the frontier's newly-demanded sources through read + parse + resolve,
accumulating reads and discovered sources, publishing once at fixpoint.
The plan-membership guard, `round_roots`, and re-close path are reused
as-is (a wave is a round whose root set grows during execution). The
cumulative coverage witness is a per-plan-segment counter maintained by
the same code that maintains the delta segments today. Falsifier fixtures
extend the existing 32-module driver test and chain premerge gates.
Sequencing: implementation starts after the in-flight delta-scoping
change lands (same files); ADR-0074 is independent (different crate).
