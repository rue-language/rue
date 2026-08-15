---
id: 0061
title: "Supported compiler facade and immutable artifact views"
status: accepted
tags: [architecture, compiler, tooling, api, incremental]
feature-flag: null
created: 2026-07-17
accepted: 2026-07-17
implemented: 2026-07-18
spec-sections: []
superseded-by:
relates: ["RUE-865", "RUE-866", "RUE-867", "RUE-868", "RUE-869", "RUE-1477", "RUE-1480", "RUE-439", "RUE-749", "ADR-0053", "ADR-0058"]
---

# ADR-0061: Supported compiler facade and immutable artifact views

## Status

Accepted under RUE-865 on 2026-07-17 and implemented by RUE-866 through RUE-869
on 2026-07-18. This is an API and ownership decision; it does not change Rue
language semantics.

Amended on 2026-08-12 by RUE-1477 and RUE-1480: the semantic and CFG view
portion of this decision is superseded. `CompilerSession::semantic`,
`SemanticView`, `FunctionView`, `CfgView`, `TypeView`, and their companion
views have been deleted. Compiler consumers and in-tree test/tooling consumers
project from the canonical rooted body/CFG query graph; no stable or test-only
whole-program semantic facade may assemble a peer artifact. The historical
inventory below records the original decision rather than the current API.

## Summary

`rue-compiler` has one supported, session-centered facade. It accepts owned
source and compile requests, exposes operations through `CompilerSession`, and
returns immutable artifact views. The facade does not reexport the types used
to implement parsing, interning, IR storage, durable reuse, query keys,
invalidation, code generation, or linking.

Human and machine presentation are an explicitly versioned tooling boundary.
Textual debug emits are explicitly unstable. The compiler may build both from
the same canonical session artifacts, but neither creates a peer frontend or
another phase path.

This decision classifies all 217 names reexported by `rue-compiler/src/lib.rs`
after RUE-777. Exact names are grouped below only when they have the same
disposition. A new root export requires an inventory change, an owner, a
stability class, and a consumer that cannot use an existing view.

## Context

ADR-0053 makes `CompilerSession` the sole orchestrator and query database
owner. ADR-0058 keeps compact live AIR separate from the canonical durable
artifact algebra. Those ownership decisions would be undermined if callers
could construct query records, mutate interners, retain request-local handles,
or install durable payloads through the public facade.

The current root is curated in shape but broad in substance. It reexports
request values next to storage records, parser and IR owners, raw side tables,
work counters, and free backend functions. Several are used only because the
CLI and compiler benchmarks historically depended on the umbrella crate.
Reexporting them turns a dependency convenience into an accidental compatibility
promise.

Current consumers establish real requirements:

| Consumer | Current need | Supported boundary |
| --- | --- | --- |
| `crates/rue` | project/import loading, compilation, diagnostics, and `--emit` presentation | stable requests/session/views; source-loading owner; unstable textual presentation |
| `crates/rue-compiler/tests` | public contract, differential reuse, payload schemas | public views for black-box tests; crate-private test support for engine invariants |
| `crates/rue-fuzz`, `crates/rue-oracle`, `crates/rue-oracle-diff` | semantic checking, executable generation, ICE classification, differential generation | stable session/results; direct owner dependencies only for models that intentionally inspect CFG/AIR |
| future profiling clients | work, retention, invalidation, and storage measurements | explicitly unstable metrics snapshots, never query records |
| future LSP | incremental compile/check, structured diagnostics, syntax/definition/navigation queries | stable session requests and immutable views with opaque stable IDs; no query, invalidation, or durable internals |
| future RUE-439 MCP | compile/check, structured diagnostics, error/spec metadata | versioned machine-readable tooling schema over stable views |
| future RUE-749 C ABI | token and diagnostic buffers | versioned owned FFI DTOs over the same syntax/diagnostic views; no Rust layout crosses the ABI |

The M8 parser and CLI splits are active while this ADR is written. RUE-860 is
changing parser modules, and RUE-856/RUE-861 are separating emit presentation
and source loading. This ADR therefore assigns semantic ownership categories,
not current file names. RUE-868 waits for RUE-860. Its result must fit the
post-split owners rather than preserve today's module layout.

## Decision

### 1. Five API classes

Every supported surface belongs to exactly one of these classes:

1. **Stable request inputs.** Owned values needed to describe source, target,
   preview, optimization, linking, or a host-supplied import observation. They
   contain no session-local handles. The compiler may validate them but does
   not perform filesystem discovery on their behalf.
2. **Stable session operations.** `CompilerSession` is the only incremental
   compiler owner. `compile_snapshot` is the sole one-shot adapter. Runtime
   configuration and update/result types are small opaque contracts rather
   than query-store records.
3. **Stable immutable artifact views.** Views expose source locations,
   diagnostics, dependency edges, syntax/token data, definitions, semantic
   facts, and final output through borrowed iterators and opaque stable IDs.
   A view cannot mutate its owner or reveal an owner-indexed storage type.
4. **Explicitly unstable presentation and metrics.** Debug text, compiler
   phase emits, timing, retention metrics, and work counters may change with
   compiler implementation. They live under an `unstable` namespace or an
   explicitly unstable tooling crate and are absent from the stable root.
5. **Internal implementation types.** Query keys and attempts, durable
   schemas, invalidation records, parser owners, raw IR, interners, backend
   state, and linker state stay in their owning crate or private compiler
   module.

“Stable” here means the reviewed Rust API compatibility policy below. It does
not make compiler output, optimization choices, or unfinished language
semantics permanently immutable.

### 2. The stable root

The final root is intentionally small. Exact spelling may improve during the
follow-ups, but the supported concepts are:

```text
requests:
  SourceSnapshot, SourceMetadata, SourceView, FileId
  CompileOptions, LinkerMode, OptLevel, PreviewFeature(s), Target, Arch
  accepted import-read/observation inputs and opaque source/module identities

operations:
  CompilerSession, CompilerSessionUpdate, compile_snapshot
  configure_thread_pool (process-wide, idempotent configuration)

views/results:
  CompileOutput
  CompileErrors, CompileWarning, FrontendDiagnosticSnapshot, DiagnosticStage
  ImportDiscoveryView, DependencyEnvelope, CanonicalImportGraphOutput
  TokenView, SyntaxView, SourceLocationView
  RirView, SemanticView, FunctionView, CfgView
  PresentationRequest/PresentationOutput only in the unstable tooling surface
```

`CompileOutput` owns final executable bytes and ordinary user-visible
diagnostics. `CompilerSession` returns owner-retaining immutable artifacts
(normally an `Arc` to a private owner) whose public methods yield the views.
Views borrow the owner or carry an `Arc`; they never outlive it.

The stable API may expose opaque, equality/hashable IDs such as `FileId`,
`ModuleId`, `SourceId`, and `SourceRevision`. Their numeric components and
allocation policy are not exposed. A location view combines an opaque source
identity with checked byte bounds; raw request-local `Span` is not the
cross-language or serialized contract.

### 3. View invariants

A stable artifact view must satisfy all of the following:

- immutable after publication and retainable across later session updates;
- tied to one immutable owner and unable to index another owner;
- iteration through checked slices/iterators, not public vectors plus indices;
- strings exposed as `&str` or owned strings, never `Spur` or an interner;
- types exposed through structural/type-description views or opaque IDs, never
  `TypeInternPool`, `FrozenTypeInternPool`, or epoch-local `Type`;
- syntax exposed through token/node views, never mutable parser arenas or raw
  `Ast` ownership;
- RIR/AIR/CFG exposed through read-only instruction/block views with checked
  references, never their storage vectors, payload ranges, or side tables;
- no query key, attempt, dependency stamp, invalidation cause, last-good
  pointer, retention record, durable envelope, or schema payload;
- no method that installs, imports, or constructs reusable compiler state.

Stable syntax and semantic views are projections of the canonical query
artifacts. Presentation mode can select ordering or formatting, but it cannot
select another parser, semantic analyzer, or frontend. A view implementation
may change when M8 moves files without changing its semantic contract.

### 4. Tooling, presentation, and crate ownership

The root compiler crate owns requests, the session, and stable views. The
following boundaries prevent the root from becoming an umbrella crate:

- Source loading and filesystem policy belong to the post-RUE-861 project/CLI
  loader. The compiler owns canonical import requests and consumes explicit
  observations; it never probes the filesystem.
- Lexer, parser, AIR, CFG, RIR, codegen, error, span, and target implementation
  crates remain directly usable by in-tree phase tests and specialized tools.
  Direct dependency is honest: those users accept that owner's compatibility
  policy. `rue-compiler` does not reexport the type merely to shorten imports.
- Stable structured diagnostics and syntax views are compiler-owned. JSON and
  C ABI adapters encode owned DTOs from those views. Their schema versions are
  independent of Rust enum layout.
- Human debug emits and metrics live under `rue_compiler::unstable` or a
  dedicated unstable presentation/metrics crate. The namespace is not covered
  by stable compatibility and may expose owned debug records, but still cannot
  expose mutable compiler state or create a peer pipeline.
- Backend free functions are internal orchestration. An unstable presentation
  request asks the session for a named emit; it does not accept raw CFG, MIR,
  interners, or type pools from the caller.

RUE-439 therefore wraps compile/check and versioned diagnostic metadata; it
does not parse CLI prose. RUE-749 exports copied token/diagnostic buffers with
explicit ownership/free functions and schema versions; it does not expose Rust
`Token`, `Ast`, `Span`, or `ErrorKind` layout through C.

A future LSP retains one `CompilerSession` per workspace or compilation unit,
publishes immutable source snapshots as documents change, and requests
incremental compile/check results from that session. Diagnostics use the same
structured diagnostic views as batch compilation. Syntax, definition, and
navigation queries use immutable views plus opaque stable source, module, and
definition IDs; the LSP may cache those IDs only with the artifact revision
that authorized them. It cannot inspect query attempts, invalidation plans,
dependency stamps, durable payloads, or last-good storage to infer freshness.
Current versus explicitly requested last-good results remain session/view
provenance, not an LSP-owned cache policy.

### 5. Exhaustive disposition of the post-RUE-777 root

The following tables cover the 29 `pub use` statements and all 217 names in
the current root. “View-wrap” removes the named raw type from the stable root
after an equivalent supported view exists. “Move internal/direct owner” means
the compiler root stops reexporting it; an in-tree consumer may depend on the
real owner or on the unstable surface. “Remove” means no replacement is part
of the supported API.

#### Stable request inputs and operations

| Disposition | Exact current exports | Final class / reason |
| --- | --- | --- |
| Keep | `CompileOptions`, `LinkerMode`, `OptLevel`, `PreviewFeature`, `PreviewFeatures`, `Target`, `Arch` | Stable compile request values. Reexports are intentional because they occur in compiler requests. |
| Keep | `SourceMetadata`, `SourceSnapshot`, `SourceView`, `MAX_SOURCE_BYTES`, `FileId` | Stable owned source request and read-only file view. `FileId` stays opaque. |
| Keep | `ModuleId`, `ModuleRevision`, `SourceId`, `SourceIdVersion`, `SourceRevision` | Stable opaque identity values; hide representation and construction not required by request assembly. |
| Keep | `CompilerSession`, `CompilerSessionUpdate`, `compile_snapshot`, `configure_thread_pool` | Stable operations; no other one-shot compiler entry point is allowed. |
| Keep | `AcceptedImportSource`, `AcceptedReadManifestEntry`, `FileMetadataFingerprint`, `ImportDiscoveryContext`, `ImportObservation`, `ImportObservationLedger`, `ImportObservationStatus`, `PhysicalFileIdentity` | Stable host-to-session import observation inputs. Constructors validate owned data; identities remain opaque. |
| View-wrap | `ImportDiscoveryPlan`, `ImportDiscoveryRequest`, `ImportOccurrenceKey`, `ImportCandidateRole` | The host receives a read-only plan/request view and returns observations; it cannot construct query-owned request identity. |
| Move internal/direct owner | `DiscoverySourceAssembler` | Source aggregation belongs to the post-RUE-861 loader, not the compiler facade. |
| Remove | `IMPORT_DISCOVERY_POLICY_VERSION` | Query policy version participates internally in request keys; it is not a host compatibility knob. |

Post-decision note (RUE-1479): the six host-protocol observation records above
(`AcceptedImportSource`, `ImportDiscoveryPlan`, `ImportDiscoveryRequest`,
`ImportObservation`, `ImportObservationLedger`, `ImportObservationStatus`) were
later moved off the stable root and are re-exported only from
`rue_compiler::unstable`, next to the begin/frontier/publish/close protocol
that consumes them. The dependency-artifact records in the Keep rows are
unchanged.

#### Stable immutable artifacts and diagnostics

| Disposition | Exact current exports | Final class / reason |
| --- | --- | --- |
| Keep | `CompileOutput` | Stable final one-shot result, narrowed so its fields use stable diagnostic views/owned output. Current `source_stats` and `work` fields move to the unstable metrics result. |
| Keep/narrow | `FrontendDiagnosticSnapshot`, `DiagnosticStage`, `CompileErrors`, `CompileWarning`, `MultiErrorResult` | Stable diagnostic snapshots and aggregate result types remain available. Granular diagnostic records are named through their direct `rue-error` owner rather than reexported by the compiler facade. |
| Move direct owner | `CompileError`, `Diagnostic`, `ErrorCode`, `ErrorKind`, `WarningKind`, `Suggestion`, `Applicability`, `CompileResult`, `Span` | Raw diagnostic and span records remain available from `rue-error` or `rue-span` for direct users; checked compiler artifact locations use `SourceLocationView`. |
| Keep | `VERSION` | Compiler version. Machine schemas carry their own independent versions. |
| Keep/narrow | `ImportDiscoveryView`, `ImportDiscoveryStatus`, `DependencyEnvelope`, `DependencyEnvelopeStatus`, `DependencyTopology`, `DependencyTopologyRecord`, `DependencyResolutionOutcome` | Stable owner-retaining discovery status and dependency graph/status outputs used by the source loader, `--emit deps`, and machine tooling. |
| Move internal | `DependencyAcceptedRead`, `DependencyContext`, `DependencyObservation`, `DependencyObservationOutcome`, `DependencyRequest` | Closure evidence and query request records are engine/source-loader implementation. |
| View-wrap | `CanonicalImportGraphOutput`, `CanonicalImportGraph`, `CanonicalImportCycle`, `CanonicalImportGraphProblem`, `CanonicalImportGraphValidation`, `CanonicalImportRecord`, `CanonicalImportResolution`, `ImportDirective`, `ImportDirectives` | One immutable dependency/import graph view. Validation and resolution records stay behind the owner. |
| Move internal | `ResolvedCodegenRevision`, `ResolvedLinkRevision`, `ResolvedProgramRevision` | Query publication/revision joins are not artifact facts for callers. |
| View-wrap | `ParsedProgram`, `ParsedAstPresentation`, `ParsedInvalidImport`, `InvalidImportShape` | Stable syntax/token/import views, independent of parser file layout. |
| Move internal/direct owner | `parse_source_snapshot_for_ast_presentation` | Presentation moves to the explicitly unstable session/tooling owner after M8, not a second free parse entry point. |
| Move internal | `ParseInvalidationSummary`, `ParsedAstPresentationWork`, `ParsedModulesWork` | Query invalidation and work records. |
| Move internal | `CanonicalMergedAst`, `CanonicalMergedProgram` | Canonical merge is a session implementation boundary. Supported syntax tooling reads the parsed `SyntaxView`; no consumer needs a separately public merged-AST owner. |
| View-wrap | `CanonicalRirOutput`, `CanonicalSemanticOutput`, `FunctionWithCfg` | Private owners publish `RirView`, `SemanticView`, `FunctionView`, and `CfgView`; no raw phase storage leaks. |
| Move internal/direct owner | `SourceStats` | The explicitly unstable metrics owner replaces this root export; metrics are not a language/tooling compatibility contract. |
| Move internal | `CanonicalMergeWork`, `CanonicalRirWork`, `CanonicalSemanticFailurePhase`, `CanonicalSemanticFailureWork`, `CanonicalSemanticWork`, `PipelineWork` | Work/failure-path bookkeeping is exposed only through the unstable metrics view where needed. |
| Move internal | `DefinitionId`, `DefinitionKind`, `DefinitionNameKey`, `DefinitionNamespace`, `DefinitionOccurrenceId`, `DefinitionRecord`, `DefinitionSnapshot`, `ModuleDefinition`, `DefinitionShard`, `DefinitionShardWork` | No current external consumer uses the raw definition owner. A future navigation consumer must request an owner-retaining definition view rather than reopening these parser records. |
| Move internal | `BoundDefinitionId`, `BoundDefinitionRecord`, `BoundDefinitionSet`, `StableDefinitionKey`, `StableDefinitionKind`, `StableDefinitionNamespace`, `StableNamedTypeKey`, `SnapshotBoundDefinitionId`, `BoundDefinitionWork` | Definition binding, issuer-scoped authorization, and reuse bookkeeping remain session-owned. A future stable definition identity is introduced only with its immutable view. |

#### Query, invalidation, and durable implementation

| Disposition | Exact current exports | Final class / reason |
| --- | --- | --- |
| Move internal/direct owner | `CompilerSessionWork`, `FrontendQueryWork`, `FrontendRetentionMetrics`, `DifferentialOracleFault` | An explicitly unstable metrics/test-support owner provides owned snapshots for benchmarks and differential tests. No record grants access to query state. |
| Remove | `FRONTEND_DIAGNOSTIC_RETENTION_LIMIT`, `FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT` | Retention policy is implementation detail; metrics may report observed capacity. |
| Move internal | `DefinitionQueryRecord`, `SemanticQueryRecord`, `ImportDiagnosticInputDescriptor`, `ImportGraphInputDescriptor`, `ImportDiscoveryRevisionArtifact`, `ImportDiscoveryRevisionStatus` | Query keys, attempts, and revision publications. |
| Remove | `SemanticDependencyBlocker`, `SemanticDependencyIncompleteReason`, `SemanticDependencyInputManifest`, `SemanticDependencyManifestWork`, `SemanticDependencySurface` | ADR-0063 supersedes ADR-0050's whole-program dependency-manifest projection; canonical per-key queries own dependency evidence directly. |
| Remove | `SemanticFullInvalidationReason`, `SemanticInvalidationPlan`, `SemanticInvalidationScope`, `SemanticInvalidationWork` | ADR-0063 replaces the peer invalidation planner with revisioned per-key validation. |
| Move internal | `StableBodyDependencyInputRecord`, `StableBuiltinTypeCallHeadInput`, `StableDeclarationTypeCallHeadDependency`, `StableDeclarationTypeDependency`, `StableDefinitionFingerprint`, `StableDefinitionFingerprintPrecision`, `StableDefinitionInputFingerprint`, `StableFreeFunctionDependency`, `StableModuleImportDependency`, `StableNamedConstDependency`, `StableNamedConstDependencyTarget`, `StableNamedDestructorDependency`, `StableNamedMethodDependency`, `StableNamedMethodDependencyTarget` | Cache authorization, fingerprints, and durable dependency edges. |
| Move internal | `CodegenInputDescriptor`, `LinkInputDescriptor`, `SemanticInputDescriptor`, `ModuleResolutionInput`, `ModuleResolutionInputs`, `SourceStore`, `StableLinkerInput`, `StableOptLevel`, `StablePreviewFeatures` | Typed query keys and internal source storage. Public request values remain the authority at the boundary. |
| Move internal | `DURABLE_ORDINARY_BODY_SCHEMA_VERSION`, `DURABLE_SPECIALIZED_BODY_SCHEMA_VERSION`, `DurableAirInst`, `DurableAirInstData`, `DurableAirRef`, `DurableBodyAnchor`, `DurableBodyConversionFailure`, `DurableBodyProjectionFailure`, `DurableBodyWork`, `DurableCallArg`, `DurableMatchArm`, `DurableOrdinaryBody`, `DurableOrdinaryBodyPayload`, `DurablePattern`, `DurablePlace`, `DurablePlaceRef`, `DurableProjection`, `DurableSpecializedBody`, `DurableSpecializedBodyPayload`, `convert_semantic_specialized_body_exports` | Body cache schemas, validation, projection, and conversion stay session-owned. Operational compatibility is reported as unstable status, never constructible payloads. |
| Move internal | `DURABLE_SEMANTIC_SCHEMA_VERSION`, `DurableConstValue`, `DurableDeclarationPayload`, `DurableDeclarationSemantic`, `DurableParameterMode`, `DurableSemanticExportFailure`, `DurableSemanticImportEpoch`, `DurableSemanticParameter`, `DurableSemanticProjectionFailure`, `DurableSemanticProjectionWork`, `DurableSemanticSchemaVersion`, `DurableType` | Declaration/type cache schemas and projection internals. |

#### Presentation, phase, and owner-crate implementation

| Disposition | Exact current exports | Final class / reason |
| --- | --- | --- |
| Move internal/direct owner | `ColorChoice`, `DiagnosticFormatter`, `JsonDiagnostic`, `JsonDiagnosticFormatter`, `JsonSpan`, `JsonSuggestion`, `MultiFileFormatter`, `MultiFileJsonFormatter`, `SourceInfo` | Presentation/encoding adapters move to the explicitly unstable presentation owner. Machine schemas gain explicit versions; formatting is not root-stable. |
| Move internal/direct owner | `Mir`, `generate_emitted_asm`, `generate_liveness_info`, `generate_lowering_info`, `generate_mir`, `generate_regalloc_info`, `LoweringDebugInfo`, `RegAllocDebugInfo`, `StackFrameInfo`, `generate_stack_frame_info` | Backend state stays internal; an explicitly unstable session-owned emit request returns owned presentation output. Callers never drive backend phases with raw artifacts. |
| View-wrap | `FrozenTypeInternPool` | Semantic type descriptions are read through `TypeView`; the pool type leaves the root. |
| Move internal/direct owner | `TypeInternPool` | Mutable AIR interner; never part of an immutable view. |
| Move internal/direct owner | `Lexer`, `Token`, `TokenKind`, `Ast` | The compiler exposes `TokenView`/`SyntaxView`; lexer/parser specialists depend directly on their owner crates. RUE-868 performs this after RUE-860. |
| Move internal/direct owner | `Rir` | The compiler exposes `RirView`; RIR specialists depend directly on `rue-rir`. |
| Move internal/direct owner | `ThreadedRodeo`, `SemanticSymbol`, `SemanticSymbolUniverse`, `SemanticTranslationWork` | Mutable/request-local interning, symbol translation, and work accounting. Views expose resolved strings. |

The `CompilerSession` export also makes its public inherent methods reachable,
so narrowing only the `pub use` list is insufficient. The current session
operations have these dispositions:

| Disposition | Current `CompilerSession` operations | Final contract |
| --- | --- | --- |
| Keep | `new`, `update`, `executable` | Stable session lifecycle and final-output query. |
| Keep | `import_discovery_plan`, `stage_import_discovery`, `close_import_discovery`, `import_graph` | Host-driven import closure remains canonical session orchestration; plans, requests, status, and graphs use immutable views. |
| View-wrap | `published`, `committed_import_graph`, `rir`, `semantic` | Stable artifact queries return private owners exposing syntax/dependency/RIR/semantic views. |
| View-wrap | `latest_diagnostics`, `latest_successful_diagnostics`, `last_good_semantic_diagnostics`, `most_recent_diagnostics_for`, `diagnostics_for`, `import_diagnostics` | Return stable diagnostic views with explicit current/last-good provenance. |
| Move internal/direct owner | `update_for_presentation`, `work` | Presentation selection and metrics move to explicit unstable requests/snapshots. |
| Move internal/direct owner | `oracle_executable`, `inject_stale_query_for_oracle` | Crate-private test support preserves differential validation without making fault injection or oracle knobs a supported compiler API. |
| Move internal / remove | `discovery_attempt`, `last_good_discovery`, `committed_import_discovery`, `stable_definitions`, `merge`, `executable_in_compile_scope`; retired `semantic_dependency_inputs`, `semantic_invalidation_plan` | Query attempts, definition binding, canonical merge, and scoped orchestration stay session-owned. ADR-0063 removes the peer whole-program dependency/invalidation operations instead of exposing adapters for them. |

`CompilerSessionUpdate` likewise keeps only success/diagnostic/view access needed
to publish a source request. Raw parse work and invalidation accessors move to
unstable metrics or become private. Public methods on every type classified
“move internal” cease to be reachable through the stable facade with their
owner; they are not individually compatibility commitments.

### 6. Compatibility and version policy

The follow-ups are an intentional pre-1.0 narrowing, but removals are still
staged rather than silently broken:

1. Each implementation PR first adds the replacement view or unstable owner,
   migrates all in-tree consumers, and then removes the raw root export in the
   same PR. No deprecated adapter may create a parallel computation path.
2. Stable Rust root additions and removals are reviewed as major API changes.
   Until Rue declares a 1.0 compatibility window, milestone release notes and
   the semantic API inventory are the compatibility record. After 1.0, removal
   or semantic weakening requires a major version; additive view methods may be
   minor when they preserve all invariants.
3. `rue_compiler::unstable` and explicitly unstable tooling crates provide no
   SemVer compatibility. Their serialized output still carries a schema name
   and version when consumed across a process boundary.
4. Machine-readable diagnostic/token/dependency schemas use explicit major and
   minor versions. A major changes names, variants, meaning, ownership, or
   required fields. A minor is additive with documented defaults. Unknown
   major versions are rejected, not guessed.
5. C ABI entry points use a separately versioned symbol family and copied owned
   buffers. Rust structs/enums, discriminants, pointers into artifacts, and
   session-local IDs are never the ABI.
6. Durable schema versions remain compiler-owned. Operational tools may observe
   `compatible`, `rejected-version`, or recomputation counters through unstable
   metrics, but cannot negotiate versions or provide unchecked records.

### 7. Implementation sequence

- **RUE-866:** introduce stable status/diagnostic/dependency views and an
  unstable metrics snapshot; migrate benchmarks and the differential oracle;
  remove query records, invalidation plans, manifests, fingerprints, retention
  constants, work records, and input descriptors from the root.
- **RUE-867:** make all durable body/semantic payloads, versions, conversion,
  projection, and failure types private to session-owned cache operations.
  Preserve only unstable operational compatibility status when useful.
- **RUE-868:** wait for RUE-860 before changing parser-facing exports. Before
  starting, recheck the source boundary against current `trunk`. If any planned
  removal or consumer migration touches CLI presentation or source loading,
  also wait for the relevant RUE-856 then RUE-861 owner split to merge; defer
  that removal explicitly if the split is not yet authoritative. Then add
  syntax/token/RIR/semantic/type views, migrate CLI/fuzz/oracle users, and
  remove lexer, parser AST, RIR, interner, raw symbol, raw pool, and backend
  presentation exports from their post-M8 owners.
- **RUE-869:** replaced the line-count guard with the mechanically checked
  `supported_api_inventory.rs` inventory of every root export and public
  session signature. Each line records owner, class, stability, approved
  consumer, symbol, and canonical signature; CI rejects unreviewed additions,
  aliases, globs, and forbidden implementation categories. The maintainer
  workflow for extending the facade is documented in
  `docs/process/compiler-facade.md`.

RUE-866 and RUE-867 may proceed after this ADR on their existing prerequisites.
RUE-868 explicitly waits for RUE-860 and performs the source-boundary recheck
above; presentation/loading-sensitive work additionally waits for the relevant
RUE-856 -> RUE-861 owner split or is explicitly deferred. RUE-869 runs last,
after RUE-866, RUE-867, RUE-868, and RUE-777.

## Consequences

### Positive

- Tooling retains structured access without making query-engine storage a
  public construction API.
- The CLI, MCP, C ABI, benchmarks, and compiler tests share one canonical
  session path while selecting stability appropriate to their use.
- Parser, IR, and backend reorganizations can change physical layout without
  forcing public API compatibility shims.
- Durable reuse stays fail-closed and session-owned.
- A public addition produces a small semantic inventory diff rather than
  merely fitting under a line limit.

### Negative

- In-tree consumers must add direct owner-crate dependencies or migrate to
  views; the umbrella import path becomes less convenient.
- Stable views require deliberate projection APIs and may allocate owned DTOs
  at JSON/FFI boundaries.
- Debug output and metrics consumers accept explicit instability.
- The migration temporarily carries raw owners and replacement views inside
  the compiler, although only one is supported at the root.

## Rejected alternatives

### Keep all types public but document some as internal

Rust visibility is the enforceable boundary. Public construction and matching
remain compatibility surface even if prose discourages them.

### Put implementation types behind a cargo feature

Rue builds with Buck2, and a feature would still publish the same unsafe
ownership boundary to whichever consumer enables it. Explicit unstable APIs or
direct owner dependencies state the contract more honestly.

### Create a separate tooling frontend

Rejected. Syntax, semantic, diagnostic, and presentation views are queries of
the same `CompilerSession` artifacts. A second parser/semantic phase machine
would recreate the architecture this project removed.

### Make canonical durable schemas the public artifact API

Rejected. Durable envelopes encode cache authorization, compatibility, and
stable joins, not a user-facing semantic model. Letting callers construct them
would bypass session validation and freeze storage representation.

### Expose raw interners and compact IR read-only

Rejected. Read-only access still leaks owner-relative indices and side-table
coupling, permits cross-artifact handle confusion, and makes storage layout a
compatibility promise. Checked views are the correct borrowing boundary.

## References

- [ADR-0053: Typed CompilerSession query state](0053-typed-compiler-query-state.md)
- [ADR-0058: Canonical semantic artifact algebra](0058-canonical-semantic-artifact-algebra.md)
- [ADR-0056: Typed IR payload schemas](0056-typed-ir-payload-schemas.md)
- [ADR-0051: CanonicalImportGraph as the sole import-resolution authority](0051-canonical-import-resolution-authority.md)
