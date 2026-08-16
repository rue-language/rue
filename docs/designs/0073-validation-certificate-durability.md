---
id: 0073
title: "Validation-certificate durability across append-only revisions"
status: accepted
tags: [architecture, compiler, incremental, performance, query-engine]
feature-flag: null
created: 2026-08-16
accepted: 2026-08-16
implemented:
spec-sections: []
superseded-by:
relates: ["ADR-0051", "ADR-0063", "ADR-0067", "ADR-0071", "RUE-1112", "RUE-1473"]
---

# ADR-0073: Validation-certificate durability across append-only revisions

## Status

Accepted by Steve Klabnik on 2026-08-16 after external design review recorded
on PR #2449. This is an internal query-engine design with no language-semantics
change. It changes only the cost of red/green validation, never what
invalidates what; that boundary is a falsifier, not an aspiration.

## Summary

A query terminal's validation certificate is currently keyed to one exact
revision. Import discovery publishes one immutable input revision per
frontier round, so every round expires every certificate and re-walks the
accumulated green cone. Fresh-build validation therefore costs
O(import-depth × graph edges), and a depth-n import chain costs O(n²): the
deterministic counters record `certificate_misses` of exactly 64², 256², and
1024² on the corresponding chain fixtures, and the 1,024-module chain spends
roughly 12.5 s of a 14 s fresh build in discovery. The maintained Lattice
workload (import depth ≈ 12) pays the same multiplier as roughly 600k
validation probes against ~34k query claims on every fresh build measured
under ADR-0071.

Discovery rounds only ever add inputs. This ADR introduces a validation
epoch that survives publications proven observationally monotone, a
certificate rule that is explicitly directional, and mechanically enforced
publication classes that decide the epoch in release builds. Fresh-build
validation drops from O(depth × edges) to O(edges); a chain probe's
`certificate_misses` falls from n² to O(n). Ordinary edit, removal, and
retained-session invalidation behavior is unchanged.

## Context

### The mechanism today

Red/green validation walks a terminal's recorded dependency edges and, on
success, publishes a certificate in a single per-node slot. The reuse gate
accepts a certificate only when its revision equals the requesting task's
revision exactly. There is no durability tier, no watermark, and no notion
that one revision extends another.

The compiler cannot read the filesystem (ADR-0051), so a fresh build
discovers sources iteratively: parse what is present, compute the import
frontier, ask the host for the missing modules, publish a successor input
revision containing the prior inputs plus the newly accepted reads, and
repeat. Rounds equal import-graph depth; same-depth imports share one round
(RUE-1112 staging extends the pinned state rather than rebuilding it). A
bounded outer loop (toolchain acquisition, at most four rounds) republishes
for absent trusted modules and pays the same full re-validation per round.

### Measured behavior

Fresh-process, release ThinLTO, Rue `-O3 -j1`, x86-64 Linux:

| chain modules | fresh build | `validation.certificate_misses` |
| ---: | ---: | :--- |
| 64 | 131 ms | 4,096 = 64² |
| 256 | 1,036 ms | 65,536 = 256² |
| 1,024 | ~14.1 s | 1,048,576 = 1024² |

The compiled output is correct and byte-reproducible in every row; the cost
is pure re-validation of terminals that cannot have changed. A deep import
chain is an ordinary program shape, and ADR-0071's 250 ms Lattice target
leaves no room for a ~12× multiplier on a validation layer that, on a fresh
build, can never find anything stale.

### Why the certificates are semantically still valid

A discovery-round publication appends input leaves; it does not mutate or
remove any existing input. An already-validated terminal observed only
inputs that still hold their exact prior values, so its certificate remains
true. The engine merely has no vocabulary to say so.

The qualification that shapes this whole design: append-only at the input
level is not automatically meaning-preserving at the observation level,
because queries can observe absence. Import resolution records typed absent
candidate observations in the accepted-read ledger, and toolchain
acquisition exists precisely to satisfy a recorded absence. An appended
input that satisfies a previously recorded negative observation changes the
meaning of an existing terminal's observations and must expire certificates.

## Decision

### 1. The certificate rule is an explicit, directional invariant

A certificate minted at revision C may validate a request at revision T only
when all of the following hold:

1. C and T belong to the same certificate-compatible lineage;
2. C is not newer than T;
3. no publication in the half-open interval (C, T] changed the meaning of
   any observation available to the certified terminal — represented as
   epoch(C) = epoch(T); and
4. the certificate belongs to the exact node incarnation and terminal stamp
   being consulted.

Certificates store both lineage information and the epoch. The single
per-node certificate slot is retained: a newer publication's validation may
overwrite an older certificate, which can cost an old pinned revision an
avoidable re-walk. That direction is safe. Accepting a newer certificate
backward — a request at an old pin reusing a certificate minted after inputs
that pin does not contain — is the direction the raw-revision equality check
prevents today, and clauses 1–2 must keep preventing it.

### 2. A narrowly named predicate, not an existing one by resemblance

The gate is implemented by a new predicate, `extends_for_certificate(C, T)`.
The existing revision-compatibility relation used for namespace binding is
related but answers a different question; the ADR that accepts this design
must record a proof that whatever fields `extends_for_certificate` consults
(revision ordering, compatibility token, epoch) give it these properties:

- reflexive and transitive over the publication history;
- monotone: if `extends_for_certificate(C, T)` and T′ is an epoch-preserving
  successor of T, then `extends_for_certificate(C, T′)`;
- false across any epoch bump.

The runtime does not make publication history globally linear, and this ADR
does not pretend it does. `publish_revision` accepts independent full input
views, and `publish_revision_overlay` admits more than one newer child of
the same retained parent: it checks parent availability, compatibility
equality, and `child.id > parent.id`, but not unique descent. Sibling
revisions are therefore representable at the runtime boundary, and numeric
ordering alone is not a proof of ancestry — `C.id <= T.id` can mistake a
sibling for an ancestor.

The property this design requires and enforces is narrower:
**certificate-eligible same-epoch history is linear.** Each
`(compatibility namespace, epoch)` pair has one extension head — the newest
revision of its chain. A publication is accepted as epoch-preserving only
when it is a constrained-class extension (section 3) whose parent is the
current head; the head then advances to the child. Every other publication —
an independent full view, an overlay whose parent is not the head, or a
second child of an already-extended parent — receives a fresh or bumped
epoch. This is enforced at the runtime publication boundary, in release
builds, alongside the class checks of section 3.

Within one epoch the history is then a single extension chain by
construction, so `extends_for_certificate` reduces to epoch equality plus
directional revision comparison inside one compatibility namespace, and its
transitivity follows from the head-extension rule rather than from any
global assumption. Clause 3 of the invariant also does real work in the
sibling case: two children of one parent never share an epoch, so a
certificate minted under one can never validate the other. If a future
design wants certificates to cross a genuinely branching structure, it must
replace this reduction with a real lineage relation first.

### 3. Publication classes are mechanically enforced in release builds

The epoch is decided by the publication's class, and the class is not a
caller-supplied claim. Failure here can produce a wrong executable, so a
debug assertion is a second-line falsifier, never the enforcement.

| class | epoch | enforcement |
| :--- | :--- | :--- |
| discovery extension | preserved | a constrained publication API that consumes the proof already produced by the existing extension/mutation validation: a successor that does not extend the pinned snapshot and manifest byte-for-byte is rejected today, and only a publication carrying that proof result may claim this class |
| toolchain acquisition | bumped | its own publication API; satisfying a recorded absence is this path's purpose, and the loop is bounded at four rounds |
| edit / removal / reload | bumped | the ordinary staged-update path, unchanged |
| unknown or future class | bumped | the default; a new publication kind is safe until someone proves it monotone and gives it a constrained API |

For the discovery-extension class, the negative-observation argument is:
within one build, a confirmed-absent candidate is stable, because a
filesystem change that contradicts pinned state trips the existing
mutation-rejection path rather than flowing in silently. The class API must
additionally verify appended keys against an index of recorded negative
observations — O(appended inputs), not O(ledger) — and conservatively bump
the epoch (or reject the publication) if any appended input satisfies a
recorded absence. Reusing the existing extension-validation proof is the
primary mechanism; the appended-key check is the belt over it; the debug
assertion that re-derives the class from the ledger is the last line.

Confirmed-absent and not-yet-requested are distinct states with different
monotonicity and must remain distinguishable in the ledger. Only a
host-confirmed absence participates in the bump rule; a candidate that was
never probed constrains nothing.

### 4. Capture-by-key limits the proof surface

Queries that depend on "the set of modules" take the sorted module list in
their query key (declaration-semantics projection, whole-program parse
composition). Appending a module mints a different key and therefore a
different node; there is no retained terminal to wrongly certify. The
accepting ADR must include an audit confirming that no retained terminal
observes the input universe implicitly — that negative and enumerative
observations are exhaustively represented by the typed ledger (import
candidate probing and toolchain demand are the two known sites). This audit
is what confines the section 3 argument to a small, checkable surface.

### 5. Behavior that must not change

- Exact invalidation: a real edit or removal expires exactly what it expires
  today. This design may only remove re-validation of unchanged cones.
- Retained-session (watch-mode) edit behavior, ready-frontier scheduling,
  and body-level parallelism are untouched.
- No new lock on validation, claim, or publication hot paths; no
  process-global mutable state.
- Emitted bytes, diagnostics, warning identities, and executable
  fingerprints are exact across the change.

## Acceptance evidence

Deterministic counters and paired same-output runs, per ADR-0067/0071
practice:

- On the module-chain probes, `validation.certificate_misses` falls from n²
  to O(n), and the two committed fixture sizes (64, 256) gain a premerge
  ratio gate so a superlinear regression fails structurally rather than
  waiting for a wall-clock symptom.
- Lattice fresh-build validation probes fall by approximately the import
  depth multiplier; executable hashes are byte-identical.
- Toolchain-acquisition rounds still expire certificates (bounded four-round
  cost is expected and correct).

Gate-targeted tests, both directions of the single slot and the sibling
case:

1. Mint a certificate in a newer append-only revision, then demand the same
   terminal at an older pinned revision: the newer certificate is rejected
   and the pin re-validates.
2. Validate at the old pin so its certificate overwrites the slot, then
   demand the newer revision: the older certificate is accepted forward.
3. Publish two overlay children of one retained parent, mint a certificate
   under the first child, then demand the same terminal under the second:
   the certificate is rejected because the siblings do not share an epoch,
   and the head-extension rule assigns epoch-preservation to at most one of
   them.

Class and shadowing tests:

4. A discovery append that satisfies a recorded absent observation (a
   shadowing candidate) bumps the epoch or is rejected — proven by a test
   that would carry a stale resolution if the rule were wrong.
5. Toolchain acquisition bumps; parked-then-satisfied semantic attempts
   re-validate.
6. Deletion and delete-then-re-add across watch-mode reloads invalidate
   exactly as today.
7. A publication of unknown class bumps by default.
8. Any query enumerating available inputs (if the section 4 audit finds one)
   gets an explicit regression test or is re-keyed.

## Consequences

### Positive

- Fresh builds validate each edge once: O(edges), independent of import
  depth; the deep-chain shape stops being quadratic.
- The multiplier disappears from ADR-0071's measured boundary without
  weakening any invalidation edge.
- Publication classes give future input kinds a safe default (bump) and an
  explicit, reviewable path to monotonicity.

### Negative

- Certificates carry more state, and the gate is a three-clause predicate
  rather than an equality; the proof obligations in sections 2–4 are real
  review work.
- The single-slot overwrite can force avoidable re-walks for old pins
  demanded after newer publications. Accepted: it is the safe direction.

### Neutral

- ADR-0063's query architecture and ADR-0051's import authority are
  unchanged; this ADR consumes their existing proofs rather than adding
  peers.

## Open questions

1. Should the epoch be one global watermark or per input class (source
   leaves, configuration, toolchain) — full durability tiers? The global
   watermark is sufficient for the measured problem; tiers are a compatible
   later refinement.
2. Does the section 4 audit find any implicit universe observation, and if
   so, is re-keying it acceptable?
3. Where exactly does the existing extension-validation proof surface, and
   can the discovery publication API consume it without copying state?

## References

- [ADR-0051: Canonical import resolution authority](0051-canonical-import-resolution-authority.md)
- [ADR-0063: Parallel demand-driven incremental compilation](0063-parallel-demand-driven-incremental-compilation.md)
- [ADR-0067: Compiler performance measurement](0067-compiler-performance-measurement.md)
- [ADR-0071: Release-quality compiler performance contract](0071-release-quality-compiler-performance-contract.md)
