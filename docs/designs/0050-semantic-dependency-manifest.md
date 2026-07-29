---
id: 0050
title: Stable semantic dependency manifests
status: superseded
tags: [compiler, incremental, tooling]
feature-flag: null
created: 2026-07-12
accepted: 2026-07-13
implemented: 2026-07-14
spec-sections: []
superseded-by: 0063
---

# Stable semantic dependency manifests

## Status

Superseded by [ADR-0063](0063-parallel-demand-driven-incremental-compilation.md)
on 2026-07-29. The stable-identity and fail-closed dependency principles remain
binding, but ADR-0063 replaces the whole-program manifest, invalidation planner,
and last-successful durable body/CFG caches with canonical per-key revisioned
queries. The design below records the retired architecture and is not a
description of the current `CompilerSession` API.

## Decision

`CompilerSession` represents reusable declaration, body, specialization, and CFG
inputs with compiler-owned stable identities and exact ordered dependency
records. Request-local `Spur`, `InstRef`, `FileId`, raw `Span`, `Type`, nominal
ID, AIR offset, string-pool index, specialization discovery order, and CFG-local
identity never serve as durable keys.

Consumers compute deterministic reverse edges and transitive invalidation
closure from sorted direct records. A missing owner, unresolved target,
incomplete observation, projection failure, or unsupported artifact invalidates
that candidate. It never silently drops an edge or partially installs an
artifact.

This model is an in-process, last-successful cache boundary. Persistent storage
and cross-process schema compatibility are not implied by this ADR.

## Stable definition and input model

The ordered definition universe uses `StableDefinitionKey`, consisting of
logical `ModuleId`, namespace, declaration kind, source name, and optional named
owner. Each named declaration has independently domain-separated declaration,
signature, and body/initializer fingerprints. Parser-authored boundaries, not
token or brace searches, define those partitions.

Semantic dependency manifests are keyed by:

- `SemanticInputDescriptor` (source revision, module-resolution inputs, target,
  and preview features);
- the canonical import graph;
- the ordered stable definition universe;
- direct declaration and per-body dependency records;
- explicit evidence-based blockers for unsupported successful surfaces.

Physical paths, FileId assignment, input order, offsets, interner values, and
discovery order do not participate in stable definition comparison. Canonical
module resolution remains an explicit input because relocation can change a
relative import's result. Optimization does not affect body identity; it is an
explicit CFG/codegen input. Linker choice is not a semantic input.

## Durable type, value, and specialization algebra

The versioned `DurableType` and `DurableConstValue` algebra contains scalar
data, stable module/definition keys, owned structural children, source-order
generic parameter indices, and closed builtin nominal tags. It is
`Send + Sync + Eq + Ord + Hash` and contains no request-local compiler or AIR
identity.

Named structs/enums encode one nominal stable-key edge rather than recursively
expanding their definitions. Structural arrays and pointers recursively encode
children. Source-ordered fields, variants, parameter modes, comptime flags, and
arguments retain source order; sets and maps are sorted before construction.

Export is fail closed. Error types, unresolved modules, anonymous/local nominal
types, ambiguous function aliases, malformed structural cycles, unknown builtin
tags, and unrecognized future variants produce typed failures rather than debug
strings or partial values.

Stable free-function specialization identity combines the generic base
`StableDefinitionKey` with canonical type and comptime-value arguments. The
session joins that identity to the current specialized machine symbol before an
imported caller becomes visible. Named methods with comptime parameters
currently produce one runtime body and therefore use the named method identity;
future method instantiation must add stable substitution identity before reuse
may cover it.

## Dependency capture

Dependencies are observed at the semantic operation that selects them, not by
a second RIR scan. Current successful manifests include:

- canonical module-import edges;
- declaration nominal-type edges for fields, enum payloads, signatures, const
  value types, method owners, and destructor owners;
- type-call heads, including stable free-function endpoints and tagged builtin
  inputs;
- named value-constant and module-binding dependencies;
- ordinary and specialized free-function calls;
- named method/associated-function calls;
- named destructor calls;
- implicit named-destructor obligations discovered during CFG elaboration;
- per-body owner, target/feature, warning policy, and exact dependency-input
  fingerprints.

AIR carries an opaque `(issuer, slot)` owner token only within one semantic
epoch. The compiler validates it against the exact `BoundDefinitionSet` and
translates it to stable identity before publishing a manifest or durable body.
Foreign, stale, duplicate, missing, or wrong-kind tokens reject publication.

Anonymous/dynamic owners and any future successful form without an authoritative
stable endpoint emit a sorted `SemanticDependencyBlocker`. The invalidation
planner unions blockers from both revisions and selects full invalidation for
the affected graph. Successful supported programs have no unconditional global
blocker.

## Invalidation planning

`CompilerSession::semantic_invalidation_plan` memoizes comparison of two stable
manifests. It computes exact additions, removals, declaration/signature/body
changes, and deterministic reverse dependency closure. Root, canonical-import,
target, and preview-feature changes select conservative full invalidation.
Planning performs no RIR query or instruction traversal.

The plan selects candidates; it is not itself proof that a retained artifact
can import. Each declaration, body, specialization, and CFG projection still
validates its complete current inputs atomically.

## Fresh-epoch declaration import

The compiler predeclares current shells in stable-key order and projects
retained declaration payloads onto exact current identities. AIR's installer
completes structs, enums, free functions, named methods/associated functions,
and named destructors while retaining current bodies, spans, parameter names,
and source order.

Projection is read-only. Installation validates the complete identity set,
visibility, nominal shape, callable arity, parameter modes/comptime flags,
receiver, and unchecked state before publication. A failed installation
consumes the candidate epoch and falls back through a wholly fresh ordinary
binder, preventing observation of partial state.

Cold compilation exports the declaration baseline from its primary ordinary
binder. It does not run a population binder or second RIR/body pass. A new
baseline is published only after the whole semantic/CFG request succeeds.

## Per-definition body import

Supported ordinary free functions, named methods, associated functions, named
destructors, and stable free-function specializations export compiler-owned
durable bodies. Each record contains:

- stable owner/specialization identity and exact input fingerprints;
- canonical durable types and comptime values;
- owned symbols and strings;
- record-local instruction and place references;
- source-relative anchors and ABI metadata;
- exact dependency and completeness provenance.

Projection resolves all current stable endpoints and anchors before mutation.
Atomic import remaps the complete body into the fresh AIR epoch. Imported bodies
remain part of the canonical reachability/specialization fixed point and return
the same dependency observations as ordinary analysis. A rejected candidate is
scheduled through the ordinary worklist; no partial AIR survives.

Warning-producing bodies, anonymous structural owners, unresolved generic
calls, untranslatable comptime/function values, and incomplete dependency
surfaces currently compile through ordinary analysis and make no reuse claim.

## Per-function CFG import

CFG artifacts are keyed by exact stable body provenance, optimization level,
target, and the transitive nominal layouts consumed by CFG operations. The
durable projection owns current-independent representations of types,
struct/enum identities, callable/intrinsic symbols, strings, relative spans,
warnings, and implicit destructor targets.

Import clones the retained CFG and atomically remaps every external domain.
Block, value, and instruction references stay local to that clone. The remapped
artifact is exhaustively validated before publication. Pointer pointees are
layout dependencies only for operations that consume pointee layout; nominal
field/payload closure remains transitive.

Target/optimization mismatch, missing or ambiguous symbols/layouts, malformed
schema, unsupported drop-glue/synthetic provenance, unreproducible
warning/destructor data, or any remap failure rebuilds only that CFG. Body
analysis and CFG reuse are independent: a conservatively reanalyzed body may
still yield an exactly reusable CFG.

## Atomic publication and evidence

The session retains last-successful declaration, body, and CFG candidates.
Syntax, declaration, body, specialization, CFG, diagnostic, or output failure
does not replace them. Work counters record every comparison, conversion,
projection, import, remap, reuse, fallback, rejection, atomic discard, skipped
body analysis, avoided CFG build, and avoided optimization. Attempts increment
before fallible work; parallel CFG counters are value-owned and reduced in
deterministic function order.

The schema-11 session benchmark provides the completion evidence: supported
N=128 exact-noop, unrelated-edit, changed-reachable-body, reverse-closure,
specialization, O0/O1 CFG, failure, and recovery scenarios have exact structural
assertions. Reused scenarios are compared with fresh sessions for semantic/CFG
artifacts, warnings, diagnostics, stable identities, dependency records,
manifests, and byte-identical executables. Cold compilation is asserted to
populate all caches from its one canonical analysis.

## Consequences and remaining work

Rue has one canonical semantic/CFG computation path with stable, atomic
per-definition reuse consumers. Unsupported artifacts fail closed without
weakening compilation or introducing a peer frontend.

Persistent serialization, filesystem watchers, editor protocols, and stable
position/reference indexes remain separate work. RUE-813 tracks a general
reusable differential oracle beyond the bounded completion workload. RUE-901
tracks realistic multi-module cold/reused projects and performance baselines.
