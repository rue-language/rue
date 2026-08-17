---
id: 0076
title: "Shared revision symbol space for body analysis"
status: proposal
tags: [architecture, compiler, performance, query-engine, type-system]
feature-flag: null
created: 2026-08-17
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0063", "ADR-0071", "ADR-0074", "RUE-1236"]
---

# ADR-0076: Shared revision symbol space for body analysis

## Status

Proposed, pending review. Follows the measurement note
`docs/notes/per-body-identity-closure-materialization.md`: of the four
candidate fixes for per-body identity-closure materialization, sharing the
symbol interner has roughly five times the headroom of sharing the type
pool, so it is proposed first. This is an ADR-0063 boundary question — it
moves a body-private index space into a revision-shared one — which is why
it needs a stated contract rather than just a byte-identity gate.

## Summary

Every body's semantic analysis today creates a fresh symbol interner
(`ThreadedRodeo`) alongside its fresh `TypeInternPool`. On a cold Lattice
build the program's 103-type nominal closure is re-materialized 20,325
times across 1,263 bodies, and the measured cost is concentrated in the
leaves of that re-materialization: symbol interning (`try_get_or_intern`,
213.7M instructions, 2.95% of the build) and the formatting that feeds it,
with the pool machinery itself at only 0.71%. Interning the same 103
closure names once per body is pure redundancy: the strings are identical,
only the `Spur` handles differ.

This ADR shares one append-only symbol interner across all bodies of a
revision:

1. **Index space and scope.** The shared space is the *equality/identity*
   symbol space (string → handle), scoped to one revision generation of
   the semantic engine. Type pools remain body-private exactly as today —
   this ADR deliberately does not share pool indices, which are sort keys
   in measured places and whose sharing (candidate A) reaches only 0.71%.

   *Amended after the Phase 1 audit*: the shared space cannot simply
   replace the body interner behind `body_symbol_interner()`. Packed RIR
   asserts that each body's symbol handles equal its **dense, body-local
   ordinals** (`materialize_candidate_rir_internal`,
   `canonical_lower.rs:386`) and encodes symbols as those ordinals — the
   ordinal is the encoding, not an ordering that can be converted to
   text. A body therefore keeps a private dense encoding space alongside
   the shared equality space, wired through the existing remap machinery
   (`PackedValidatedRir::remap_symbol`, `AstGen::normalize_symbol`).
   Artifact-space assignment: packed RIR and the AIR comptime-argument
   encoder speak the body-dense ordinal space; analysis-state equality,
   membership, and cross-body identity speak the shared space. The
   interning saving is realized in the shared space (strings are interned
   once per revision); the dense space holds only the body's own compact
   remap, not re-interned strings.
2. **Handle discipline (the core contract).** `Spur` values under a
   shared concurrent interner are assigned in first-intern order, which
   varies with worker scheduling. Therefore: `Spur` is a process-local,
   equality-only handle. It must never be ordered by, never feed a
   deterministic counter, never be hashed into published state, and never
   reach an emitted artifact. Anything needing an order orders by symbol
   text or by a stable identity (ADR-0074's structural hashes exist for
   exactly this). The implementation must enforce this mechanically — a
   wrapper handle type that does not expose `Ord`/`PartialOrd`, with the
   pre-change audit finding and converting any existing order-bearing use
   before the sharing lands.
3. **Byte identity is a hard gate, not a rebaseline.** Because handles
   are equality-only, emitted output and all deterministic counters remain
   byte-identical, including across `-j1`/`-j4`. If the implementation
   cannot achieve that, the ADR's premise failed and the change must not
   land — there is no counter-rebaseline escape hatch here, unlike
   ADR-0074/0075 where the artifact itself was redefined.
4. **Invalidation.** The shared interner is append-only within its
   revision generation and dropped whole when the generation is
   superseded. Bodies never observe removal or mutation of an entry. A
   body analyzed against a superseded generation fails the authority
   check below and re-runs — fail-closed, never silently reused.
5. **`require_rir_authority`.** Its pointer-equality assertion (a body's
   analysis-state interner must be the body RIR's interner) survives with
   the revision interner as the shared referent: both sides hold the same
   shared handle, and the assertion becomes pointer identity against the
   revision's interner. A body RIR carrying a superseded generation's
   interner fails the check, which is the invalidation path in (4) doing
   its job. *Amended after the Phase 1 audit*: the ownership chain
   through `body_identity.rs`/`provider_body_host.rs` is `Rc<…>` today,
   so Phase 2 includes converting that chain to `Arc` (or an equivalent
   shared-ownership handle) before the revision-scoped referent can
   exist; this conversion is in Phase 2's budget, byte-identity gated
   like everything else. The authority contract additionally covers the
   dense space: a body's packed RIR must carry the dense remap minted by
   that body's own analysis, and the dense-ordinal assertion at
   `canonical_lower.rs:386` keeps holding unchanged.

## What this buys

Up to ~213M instructions of redundant interning plus the formatting that
feeds it — roughly **2.9–3.3% of a cold Lattice build**, concentrated in
the semantic phase that is 45% of Lattice's wall time. chain-shaped
programs (closure ≈ 5 types) see approximately nothing, which is the
expected shape: this is a closure-size-scaled cost.

## Falsifiers

- **Byte identity**: executables and all `compiler_work` counters
  identical before/after on Lattice, Mosaic, and chain fixtures, at
  `-j1` and `-j4`, across repeated runs. This is the premise, not a
  nice-to-have; failure kills the change.
- **Handle discipline**: the wrapper handle exposes no ordering; a test
  (or compile-time assertion) proves `Spur`-ordered iteration cannot
  reach publication. The pre-change audit's findings (every current
  order-bearing symbol-handle use and its replacement) are listed in the
  implementation PR.
- **Authority**: a test hands a body an interner from a superseded
  generation and asserts `require_rir_authority` refuses it and the body
  re-runs against the current generation.
- **Concurrency**: `-j4` stress run with deterministic-output comparison
  across ≥3 runs (scheduling-varied `Spur` assignment must be invisible).
- **Retention**: peak RSS on Lattice within ±2% (one interner holding
  the union of body symbols replaces 1,263 holding overlapping subsets —
  expected to drop, measured not assumed).
- **Dense-space integrity** (added with the dual-space amendment): the
  packed-RIR dense-ordinal assertion (`canonical_lower.rs:386`) holds
  unchanged under sharing, and a test proves a body's packed RIR round-
  trips through its dense remap to the same bytes as today.

## Rejected alternatives (measured in the note)

- **A. Shared frozen closure pool with copy-on-write overlays**: 0.71%
  headroom; pool indices are sort keys (`ConstraintGenerator::expr_types`
  ordering), so it breaks byte-identity by construction.
- **B. Cross-body memo of minted closures keyed by identity epoch**:
  refuted — the cached values are pool indices and `Spur`s in another
  body's index spaces; a memo cannot transplant them.
- **C. Per-module pre-mint**: strictly dominated by A on Lattice's shape
  (61 named nominals span all 1,263 bodies).
- **Caching formatted names without sharing the interner**: removes the
  formatting but not the insertion, which is the expensive half.
- **Lazy endpoint installation**: changes work counters by construction;
  a contract change with unestablished benefit, deferred until this ADR
  settles the symbol-space question.

## Implementation shape

Phase 1 (audit, own commit): find every ordered or published use of a
symbol handle; convert each to text or stable-identity ordering; prove
byte identity unchanged. Phase 2: introduce the equality-only wrapper
handle and the revision-scoped shared interner behind the existing
`body_symbol_interner()` accessor; thread the `Arc` through analysis
state and body RIR; update `require_rir_authority`. Phase 3: falsifier
tests and the retention measurement. Each phase lands with the full
byte-identity gate; Phase 1 is independently valuable (it removes latent
scheduling-sensitivity) even if Phase 2 stalls.
