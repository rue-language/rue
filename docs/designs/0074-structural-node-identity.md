---
id: 0074
title: "Structural node identity for dependency order and retained charge"
status: proposal
tags: [architecture, compiler, performance, query-engine]
feature-flag: null
created: 2026-08-17
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0063", "ADR-0071", "ADR-0073", "RUE-1381"]
---

# ADR-0074: Structural node identity for dependency order and retained charge

## Status

Proposed. Approved in principle by Steve Klabnik in session on 2026-08-16
("yes, let's do all of those"); this document records the contract so the
implementation has a falsifiable target. This is an internal query-engine
design with no language-semantics change. It changes the *representation*
of deterministic artifacts (published dependency order, retained-charge
values) exactly once, never their determinism.

## Summary

A query node's identity is today the pair of formatted display strings
`(family, key)` (`NodeIdentity` in `crates/rue-query/src/lib.rs:2228`):
ordering compares the text (`:2320`), hashing hashes the text (`:2331`),
the canonical published dependency order is family-then-key-text
(`into_observations`, `:9321`, per RUE-1381), and the retained-memory
charge is denominated in the text's byte length
(`retained_terminal_charge`, `:11196`, charging `key().len()` for the
published node and again for every observed dependency).

Because presentation is load-bearing for ordering and memory policy, every
memo-node incarnation must format its display identity eagerly: 32,912
materializations retaining ~8.0 MB of key text on a fresh Lattice build.
A full deferred-formatting prototype (2026-08-16, recorded in
`docs/notes/post-adr-0063-cold-compiler-architecture-audit.md`) removed
**zero** of them, because the ordinary path reads the text through exactly
these two contracts; carving presentation bytes out of the retained charge
alone regressed peak RSS by +4.9 MB to save 2.9 MB of text, because the
formatted length was carrying real retention back-pressure.

This ADR moves both contracts onto a structural identity so that display
text becomes genuinely presentation-only, then re-lands the (previously
falsified) lazy formatting on top:

1. Every `QueryKey` gains a deterministic, content-derived 128-bit
   **stable key hash**, computed from the typed key's fields with a fixed,
   keyed-with-a-compile-time-constant hasher — never from the formatted
   string, and never from addresses, allocation order, or scheduling.
2. `NodeIdentity` ordering, equality, and hashing become
   `(family, stable_hash)` with formatted text as a lazily-computed, cold
   tiebreaker that is only reachable on a 128-bit hash collision between
   distinct keys of the same family.
3. The canonical published dependency order (RUE-1381) is redefined as
   `(family, stable_hash, incarnation)`. The order remains fully
   deterministic across runs and worker counts; it is one-time different
   from today's text order.
4. `retained_terminal_charge` is re-denominated in structural units: the
   fixed sizes it already counts, plus the actual retained payload sizes,
   plus a fixed 16-byte identity charge per node/observation in place of
   `family().len() + key().len()`. The retained-byte budget is
   recalibrated once so the steady-state retained working set on the
   ADR-0071 reference workloads is unchanged (measured, not assumed —
   the falsified prototype proved the charge is real back-pressure).
5. Memo-node display identities become lazy: formatted on first
   diagnostic, cycle render, abort, or debug dump that needs a name.
   `display_identities.memo_node_materializations` then counts actual
   formatting events; its doc and `docs/process/compiler-scaling.md`
   already describe the current node-tracking semantics and are updated
   again to the new meaning.

## What this buys

- ~33k eager string formats and ~8 MB of retained text per fresh Lattice
  build disappear from the ordinary path (measured 24,079 of them are
  attributable to the ordering contract alone; the remainder to the
  retained charge).
- `NodeIdentity::cmp` and `Hash` become integer comparisons; `Task::pop`
  and every ordered dependency publication stop touching string bytes.
- Identity `Arc<str>` pairs shrink to a 16-byte inline hash plus a lazy
  slot, reducing per-node retained footprint and pointer chasing.

## Falsifiers

The implementation must land with tests that fail if any of these break:

- **Determinism**: two fresh builds of the same inputs publish identical
  dependency orders and identical retained-charge values, including under
  different worker counts (`-j1` vs `-j4`) and across process runs (the
  hash must not be seeded per-process).
- **Collision safety**: a forced hash collision between two distinct keys
  (test-only hasher override) still yields a total, deterministic order
  via the text tiebreaker, and distinct identities never compare equal.
- **Presentation identity**: diagnostic, cycle-render, and abort text is
  byte-identical to today's (the pinned cycle-render test from 2026-08-16
  must pass unchanged); `Debug` formatting still materializes
  (`session.rs` pins `format!("{:?}", dependency.node)`).
- **Emitted bytes**: compiled executables are byte-identical on the
  ADR-0071 reference workloads and the chain fixtures.
- **Retention neutrality**: median peak RSS on Lattice within ±2% of
  pre-change after budget recalibration.

## One-time rebaselines (expected, bounded)

- Deterministic `compiler_work` counters whose values encode the current
  text order (validation visit order, early-exit points) shift once.
  The falsifier is that they are *identical across runs* after the
  change, not that they match old values.
- Any golden artifact embedding the published observation order is
  regenerated once in the same change.

## Rejected alternatives

- **Ordinal identities assigned at first publication**: deterministic
  only single-worker; violates the worker-count falsifier.
- **`runtime_identity: Option<u64>`** (already on `NodeIdentityData`):
  incarnation-scoped and allocation-ordered; same violation.
- **Keeping text order, deferring formatting** (the 2026-08-16
  prototype): measured to remove nothing; recorded in the audit note.
- **64-bit hash**: collision probability at ~10^5–10^6 keys per family is
  no longer ignorable across the ecosystem's lifetime; 128-bit keeps the
  tiebreaker theoretical while the cold path guarantees correctness
  anyway.

## Implementation shape

`QueryKey` trait gains `fn stable_hash(&self, hasher: &mut StableHasher)`
alongside `stable_identity()`; the runtime computes the 128-bit digest at
node mint (cheap: hashes typed fields, no allocation), stores it inline in
`NodeIdentityData`, and keeps `key: OnceLock<Arc<str>>` for presentation.
`retained_terminal_charge` swaps the two `len()` terms per the contract
above; the budget constant is recalibrated in the same commit from
paired-run RSS measurements. Falsifier tests live next to the existing
ADR-0073 falsifiers in `rue-query`.
