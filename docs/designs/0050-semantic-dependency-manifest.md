---
id: 0050
title: Stable semantic dependency manifests
status: proposal
tags: [compiler, incremental, tooling]
feature-flag: null
created: 2026-07-12
accepted:
implemented:
spec-sections: []
superseded-by:
---

# Stable semantic dependency manifests

## Goal

Permit sound declaration and body invalidation without retaining request-local
`Spur`, `InstRef`, `FileId`, `StructId`, specialization order, or CFG identity.
This proposal does not authorize CFG reuse until every dependency surface below
is captured and closure tests prove it.

## Stable model

Each manifest is keyed by `SemanticInputDescriptor`, canonical import graph, and
an ordered definition universe of `StableDefinitionKey`. A future record has:

- `owner: StableDefinitionKey`;
- separate declaration/signature and body fingerprints;
- ordered direct `declaration_dependencies` and `body_dependencies`;
- explicit non-definition dependencies: root, canonical imports, target and
  preview features; optimization belongs only to CFG/codegen reuse;
- specialization descriptors made from stable callee key plus canonical type and
  comptime-value arguments, never discovery order.

Consumers compute deterministic reverse edges and transitive closure from sorted
direct edges. Missing owners or unresolved targets fail closed by invalidating
the requesting record, never by silently dropping an edge.

## Current capture audit

The existing compiler does not yet expose a complete sound graph:

- declaration binding resolves types, constants, functions, methods, associated
  functions, module bindings and destructors, but retains results under
  request-local IDs;
- constant initialization recursively discovers const dependencies inside
  `declarations.rs`; those edges are not returned;
- reachable body analysis returns `HashSet<Spur>` and
  `HashSet<(StructId, Spur)>` from function/method/destructor analysis, sorts them
  only for worklist scheduling, then discards the owner-to-target edges;
- type syntax and struct/enum payload resolution can reference constructors,
  constants and types without appearing as ordinary call edges;
- implicit drop glue and anonymous destructors add roots after type/comptime
  evaluation;
- generic specialization creates concrete callees from type/comptime values and
  may discover more bodies; specialization identity is not durable;
- import resolution is already a stable separate query and must be included as
  an explicit input rather than rediscovered from RIR;
- CFG construction consumes typed AIR and optimization level. Reusing it before
  stable AIR/type/string identity exists would be unsound.

Adding a second RIR scan would still miss semantic choices made during binding
and specialization, so extraction must occur at the points above.

## First implemented slice

`CanonicalFrontendSession::semantic_dependency_inputs` is tooling-only and
shares `import_graph` plus `stable_definitions`. It publishes an immutable,
ordered stable definition universe with semantic and import inputs. It also
contains the first complete edge surface: ordered module import dependencies
from importer to resolved target, with missing and ambiguous outcomes retained
as fail-closed records. Work records visited definition/import records and
asserts zero extra RIR visits. This is the destination index required to
translate future definition-level edges; it makes no body or CFG reuse claim.

## Required next slices

1. During declaration binding, translate winning bindings to stable keys and
   capture signature/type/const/module/destructor direct edges.
2. Thread an owner stable key through body analysis and translate each returned
   free-function/method/destructor reference before the worklist sets are
   discarded.
3. Define durable canonical type/comptime specialization arguments and record
   generic-origin edges.
4. Split declaration/signature and body fingerprints at parsed syntax boundaries.
5. Gate deterministic closure on recursion, mutual recursion, methods,
   destructors, constants, imports, generics, relocation/FileId/input order, and
   target/feature/root changes.
6. Only then retain typed AIR or CFG results; CFG keys additionally include
   optimization inputs and any global string/type remapping identity.

Acceptance requires one existing semantic execution to emit the complete
manifest with no second whole-RIR traversal and unchanged diagnostic bytes/order.

### Free-function capture progress

The first request-local capture seam now records free-function references from
ordinary reachable free-function bodies at the existing worklist boundary. Its
neutral endpoints are the defining FileId epoch plus owned source name; methods,
constructors and intrinsics remain excluded by their separate channels. The
output explicitly reports incomplete because generic-specialized, method and
destructor callers have separate driver branches not yet carrying a stable
owner. These events are not translated to `StableDefinitionKey` and must not
drive reuse until every branch is captured and unique translation fails closed.

Specialized free-function bodies now retain a neutral origin record containing
their mangled analyzed name, exact generic base FileId/source name, and
request-local type/value specialization argument words. This survives fixpoint
discovery without claiming that the argument encoding is a durable
cross-request key. Compiler-layer stable-key translation remains prerequisite.
