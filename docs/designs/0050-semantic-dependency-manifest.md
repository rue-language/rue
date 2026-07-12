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

## Durable recursive type and value algebra

The query boundary uses the versioned `DurableType` and `DurableConstValue`
algebra in `rue-compiler`; schema version 1 is part of every future persistent
key. It contains only scalar data, `ModuleId`, `StableDefinitionKey`, owned
structural children, and source-order generic-parameter indices. It is
`Send + Sync + Eq + Ord + Hash`. It never contains `FileId`, `Span`, `Spur`,
`InstRef`, request-local `Type`, `StructId`, `EnumId`, or type-pool indices.

The current bound declaration inventory is: signed and unsigned integer widths,
bool, unit, never, comptime `type`, named structs/enums, arrays, const/mut raw
pointers, and modules. Constants additionally contain integers, booleans, type
values, function aliases, and unit. Function/method/destructor signatures use
the same type forms plus ordered parameter modes and comptime flags at the
declaration-record layer. Struct fields and enum payloads preserve source order.
Generic/comptime parameters preserve declaration order; concrete comptime
arguments use the value algebra. Tuples and first-class function types are
reserved algebra variants but are not presently emitted by AIR binding.

Named structs and enums are encoded as a single nominal stable-key edge, never
expanded. This makes self and mutual recursion finite without traversal-order
backreferences. Structural children are recursively encoded; encountering a
structural pool cycle is a typed failure. Ordered language constructs retain
source order. Sets and maps in declaration records must be sorted by stable key
before construction. No interner number, debug string, physical path, source
offset, or discovery order participates in equality or hashing.

Export is fail closed. Error types, unresolved modules, missing pool entries,
anonymous/local nominal types, unjoinable function aliases, and unknown future
type/value forms produce `DurableSemanticExportFailure`; they are never
stringified. Adding or changing a variant increments the schema version.

### Producer, projection, and installation seams

The one-way AIR export and stable compiler algebra are implemented. A compiler
projection adapter now performs the inverse stable-key/current-revision join:
it validates the exact `BoundDefinitionSet`, current declaration shells, and
durable record universe before producing AIR `SemanticDeclarationExport` DTOs.
It supplies current `FileId`s, spans, owners, names, kinds, and visibility while
leaving authoritative body handles and parameter metadata in the shells.
Records are projected in stable-key order; missing, duplicate, extra,
ambiguous, namespace/kind/owner/module/visibility, and unsupported cases are
typed failures. Work counters pin zero RIR traversal.

The result now feeds AIR's atomic installer from the canonical in-process
session for a deliberately narrow production subset. The session issues the
current stable universe directly from pre-resolution shells, compares
parser-authored declaration/signature fingerprints, projects retained durable
payloads, and then runs current-revision body analysis and CFG construction.
Function, method, associated-function, and destructor bodies are never cached.
Constants, module values, function aliases, generics, and anonymous owners
remain explicit clean fallbacks.

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
4. ~~Split declaration/signature and body fingerprints at parsed syntax boundaries.~~
5. Gate deterministic closure on recursion, mutual recursion, methods,
   destructors, constants, imports, generics, relocation/FileId/input order, and
   target/feature/root changes.
6. Only then retain typed AIR or CFG results; CFG keys additionally include
   optimization inputs and any global string/type remapping identity.

Acceptance requires one existing semantic execution to emit the complete
manifest with no second whole-RIR traversal and unchanged diagnostic bytes/order.

## Invalidation planning seam

`CanonicalFrontendSession::semantic_invalidation_plan` now memoizes an immutable
comparison of two manifests. It computes exact stable-key additions, removals,
and fingerprint changes, and contains a deterministic reverse-dependency closure
whose work counters explicitly pin zero RIR traversal. Root, canonical import,
target, and preview-feature changes are unconditional full invalidations;
physical relocation, FileId assignment, and input order are absent from its
definition comparison.

This is intentionally a safety seam, not yet retained semantic artifacts. A
production manifest carries an immutable sorted set of
`SemanticDependencyBlocker` records instead of a hard-coded global completeness
bit. Each record identifies the dependency surface, the precise missing
semantic identity, and a stable owner when one exists. Compatibility
`*_complete` accessors and whole-graph completeness are derived from that set.
The planner unions both revisions' blockers into its
`IncompleteDependencyGraph` reason, so endpoint loss fails closed and cannot be
hidden by a boolean toggle. Supported production programs now have an empty
blocker set and produce `Incremental` plans for exact no-ops and body-only
changes, including deterministic reverse-dependency closure and reusable-key
cardinality. That plan is evidence for a later retained-artifact query; it does
not itself reuse AIR or CFG results.

There are no unconditional production blockers. Individual programs can still
add evidence-based blockers, including anonymous drop owners and any future
unsupported dynamic or unnameable type-call heads. Such programs continue to
return `Full` with the exact blocker union and no reusable candidates. The
records are sorted/deduplicated, independent of FileId and physical location,
and require no second RIR scan.

## Definition fingerprint partition

Stable definition inputs now use schema v2 with independently domain-separated
declaration, signature, and body-or-initializer digests. Declaration hashes only
the stable key and visibility. Function, named method, associated-function, and
named-destructor signatures end at the parser-authored body start; const
signatures end at the parser-authored initializer start. Their exact payload
span is hashed separately. Struct signatures are framed source fragments that
exclude every parser-authored named-method body, so editing a method body does
not spuriously change its owner type; enum and body-free struct declarations are
exact signature-only inputs.

No token or brace search reconstructs these boundaries. Bound definition
records join semantic winners back to the canonical AST and reject missing,
foreign, reversed, overlapping, or out-of-range spans. Syntax and semantic
rejection still publish no manifest. The public precision enum includes a
conservative full-declaration fallback for future syntax whose authoritative
partition is unavailable, but every currently issued named definition kind has
an exact partition. Hashes exclude FileId, offsets, physical paths, and input
order; tests cover relocation, visibility, parameters/returns, fields, enum
variants, function/method/destructor bodies, and const initializers without an
extra parser, binder, or RIR traversal.

Implicit drop obligations are now observed where AIR is elaborated into CFG,
including scope/parameter/overwrite drops, recursive struct/enum/array glue,
and partial-drop destructor calls. Each analyzed body carries a neutral,
stable-capable owner (ordinary or specialized-base function, named method, or
named destructor); synthesized named struct/enum glue is owned by the named
type definition. The compiler joins those owners and named destructor targets
to exact-revision `StableDefinitionKey`s without rescanning RIR. Anonymous
owners or destructor targets explicitly make this surface incomplete. This is
separate from the current unconditional named-destructor analysis roots: roots
ensure code exists, while these edges record which definitions actually require
the destructor or its transitive glue.

### Free-function capture progress

The first request-local capture seam now records free-function references from
ordinary reachable free-function bodies at the existing worklist boundary. Its
neutral endpoints are the defining FileId epoch plus owned source name; methods,
constructors and intrinsics remain excluded by their separate channels. The
ordinary-caller output reports this surface complete; method and destructor
callers remain separate, explicitly incomplete surfaces.

Specialized free-function bodies now retain a neutral origin record containing
their mangled analyzed name, exact generic base FileId/source name, and
request-local type/value specialization argument words. This survives fixpoint
discovery without claiming that the argument encoding is a durable
cross-request key.

Specialized-body free-function references are now captured alongside that
origin at the existing specialization-analysis seam, including later fixpoint
rounds and recursion. `semantic_dependency_inputs` joins every ordinary and
specialized endpoint against the exact-revision `BoundDefinitionSet` by FileId,
owned source name, value namespace, and function kind. Missing, ambiguous, or
non-function endpoints fail closed. The resulting sorted stable edges use only
the generic base caller and callee `StableDefinitionKey`; specialization
argument words remain request-local evidence and distinct instances deduplicate
to one stable edge. Work counters pin zero additional RIR visits.

The narrow `free_function_caller_dependencies_complete` flag covers ordinary
and specialized free-function callers to free-function callees. The overall
semantic graph remains incomplete until method/destructor callers, declaration
and type/constant/drop dependencies are captured, so these edges still do not
authorize AIR or CFG reuse.

Named methods on stable named structs now have a second neutral capture seam.
The caller is `(FileId, owned owner name, owned method name)` and each target is
tagged as a free function or another named method. Compiler translation joins
methods by exact revision, FileId, owner, method namespace and
method/associated-function kind; anonymous owners and invalid endpoints never
silently enter the stable graph. Stable named-method edges are sorted and
deduplicated without another RIR traversal.

`non_generic_named_method_dependencies_complete` covers reachable named methods
without comptime parameters calling free functions (including generic free
function bases) or other named methods. The same authoritative reference sets
now retain edges for named methods with comptime parameters. Anonymous methods,
destructors, constructors and intrinsics remain outside this surface and keep
the overall semantic graph incomplete.

Audit of comptime-parameter named methods found no specialization mechanism:
receiver resolution records only `(StructId, method)`, coerces every explicit
argument as a runtime argument, emits one direct `Call` symbol, and the lazy
driver analyzes the declaration once. `MethodInfo` has no specialization key or
substitution/origin fields. Tests pin two distinct comptime arguments producing
one analyzed method body and no specialization origins. The declaration is
therefore the exact stable caller identity, and
`generic_named_method_dependencies_complete` is production-derived from the
same successful named-method traversal. Future method instantiation support
must add substitution identity before this flag may remain true; rejected or
endpoint-incomplete traversals still publish no manifest.

Named destructor bodies now use the same existing reference sets to capture
tagged free-function and named-method targets. Their caller handle is the exact
named owner declaration FileId/name and compiler translation joins the
Destructor namespace/kind and owner in the exact `BoundDefinitionSet`.
`named_destructor_dependencies_complete` covers named destructors; anonymous
destructors remain a separate incomplete surface.

Resolved declaration tables now publish a conservative nominal-type edge slice
for struct fields, enum payloads, ordinary function signatures, named
method/associated-function signatures and owners, constant value types, and
named destructor owners. Arrays and pointers are traversed to their nominal
leaf; builtins and anonymous/structural types do not invent definition keys.
Compiler translation joins both endpoints against the exact bound-definition
revision and retains the field/payload/signature/declared-type/owner edge kind,
with zero additional RIR traversal.

Declaration-type capture is complete for successful named declarations.
Resolver-time observation retains nominal leaves before deferred generic
signatures become `COMPTIME_TYPE`, recursively including arrays and pointers;
the resolved declaration-table sweep remains a parity backstop. Type aliases
retain both the exact value-constant declaration endpoint and the resolved
nominal target. All endpoints join the same bound-definition revision, and no
RIR rescan or source-text inference is involved. Type-call heads remain a
separate, independently fail-closed surface.

The pre-substitution observer now classifies every type-call head accepted by
the current resolver. User-defined free and module-qualified free comptime
functions retain exact declaration endpoints. The preview-gated `Str(N)` head
is a `FixedCapacityString` language-builtin input, not a fabricated source
definition; its target and preview-feature identity are already part of the
semantic input descriptor. No intrinsic currently returns a type. Dotted heads
are exclusively module paths: `Owner.Make()` where `Owner` is a named type is
rejected, so associated type constructors, dynamic heads, and unnameable heads
are not successful language forms and emit no partial dependency edge.
Both type-call-head completeness surfaces are true for successful programs:
named heads retain their exact stable function endpoint, while `Str(N)` retains
its tagged builtin input. Unsupported syntax is rejected before a manifest is
published; if a successful dynamic/unnameable form is introduced later, its
producer must emit an evidence-based blocker rather than silently broadening
this claim. Supported programs can therefore have whole-graph completeness.

Named value-constant initializers record direct dependencies while the existing
dependency-ordered constant collector and comptime evaluator resolve them.
Targets are tagged as value constants, free comptime functions/type
constructors, named struct/enum types, or module bindings. Recursive collection
temporarily changes and then restores the exact `(FileId, name)` source, so
chains and diamonds retain direct edges rather than a transitive approximation.
Compiler translation joins every source and target against the exact bound
definition revision; module-binding targets are real binding identities whose
resolved import topology remains in the existing module-import edge channel.
Module bindings are not value-constant sources and are filtered from this
surface. Cyclic or otherwise rejected initializers publish no partial manifest.
`named_value_const_dependencies_complete=true` covers successful named value
constants; anonymous/dynamic values and the overall semantic graph remain
explicitly incomplete. Capture is sorted/deduplicated and adds no RIR scan.

## Fresh-epoch semantic import boundary

Durable declaration values can now be remapped into a fresh AIR-owned epoch
without retaining an exporting request's `Type`, `StructId`, `EnumId`,
`ModuleId`, or `Spur`. AIR exposes a neutral, generic stable-key import DTO and
`SemanticImportEpoch`; the compiler alone translates `StableDefinitionKey` and
logical `ModuleId` values into it. Nominal shells are predeclared in stable-key
order and completed in a second phase, allowing recursive pointer graphs.
Primitive, nominal, array, pointer and module types plus scalar/type/function
constant values round-trip through the epoch's stable join.

AIR now also has a comparison-only declaration-install boundary. Given a
durable payload already projected onto the exact current-revision identities,
fresh predeclared shells can be completed for structs, enums, free functions,
named methods and associated functions, and named destructors. The installer
retains current RIR bodies, spans, parameter names and source order; it imports
only resolved signature/aggregate payloads. It validates the complete identity
set and visibility before mutation, then validates nominal field/variant shape
and callable arity, modes, comptime flags, receiver and unchecked state. A
typed failure consumes the candidate binder, ensuring that fallback creates a
fresh binder and runs ordinary resolution rather than observing partial state.
Installation and installed-payload counters distinguish this path from ordinary
declaration resolution, and installation performs no additional RIR scan.

The canonical session now retains a successful stable-keyed durable baseline
across source updates. Exact no-op/relocation and body-only edits of the
supported universe install that baseline into current shells; a 128-module
workload pins 128 records installed and zero ordinary declaration-resolution
invocations while exact fresh-batch functions, strings, and canonically ordered
warnings remain equal. Signature/declaration/root/target/feature changes fail
closed before installation. Module types and constants (including function
aliases) still fail closed because module/value classification is not
authoritative until dependency-ordered initializer evaluation. AIR's reserved
tuple/function forms also remain unsupported.

Cold population exports stable-keyed declaration payloads from the primary
ordinary binder before body analysis consumes it. The exporter uses the
already captured declaration shells, so it neither materializes the optional
binding manifest nor traverses RIR. The session publishes the semantic result
and its durable baseline only after the whole ordinary request succeeds;
failed requests leave the last-good baseline untouched. The legacy
`durable_cache_population_bindings` work counter is hard-gated at zero to make
an accidental second bind visible. Cold and successful reuse requests each
construct exactly one semantic epoch, one declaration index, and one shell
predeclaration epoch. Cold requests report one population export; reuse reports
none. Projection failure is read-only and resolves the same unmutated shells.
Only an installation failure, which consumes candidate shells to preserve
atomicity, may report a second semantic/index epoch and one fallback epoch.
These values, together with the exact plan/comparison/install/reuse counters,
are serialized under `semantic_work.declaration_reuse` by the session benchmark
and are treated as a checked schema rather than inferred timing data.

The split-binding seam now predeclares free-callable, named-method/associated,
named-const, and named-destructor identities in logical-path order. It retains
exact-revision bodies/initializers, spans, parameter names/modes/comptime flags,
source order, and generic context in private pending records, apart from any
resolved payload. The ordinary adapter crosses the explicit install/finalize
boundary using current resolution and preserves historical diagnostic and
constant-evaluation order. Anonymous structural methods remain deferred because
they lack a durable structural-owner identity. Module bindings and value
constants also cannot be distinguished before dependency-ordered initializer
evaluation, though their value-namespace identity is already fixed. No cached
AIR bodies, CFGs, and RIR fragments are never imported by this boundary.
