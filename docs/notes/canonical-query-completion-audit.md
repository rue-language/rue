# Canonical query architecture completion audit

This audit covers authoritative trunk plus the local RUE-719 stack through the
in-process benchmark. It records observable APIs and tests, not a claim that the
incremental-compilation goal is complete.

## Requirement evidence

| Requirement | Status and falsifiable evidence |
| --- | --- |
| Trustworthy phase/work baselines | Satisfied for the current frontend boundary. `compile_source_snapshot_with_options_and_work` returns `CanonicalPipelineWork`; `CanonicalFrontendSessionWork` separates calls, executions, and reuses. `rue-compiler-session-bench` hard-gates cold, no-op, edit, identity, failure/recovery, and stable-ID work at N=4 in ordinary tests and N=128 opt-in. A regression is any extra lexer/parser, merge, RIR, semantic bind/body, or manifest execution rejected by those gates. |
| Pass/IR boundary simplification | Satisfied for snapshot batch and `CompilationUnit`: both call canonical parsed-module assembly, canonical merge, `lower_canonical_rir`, and `analyze_canonical_program`. `CompilationUnit` retains compatibility projections, but its tests require projections to be lazy and not trigger a second canonical lower. |
| Explicit module root | Satisfied. `SourceMetadata::new` requires a root `FileId`; `SourceRevision` and `ModuleResolutionInputs` retain a root `ModuleId` and reject an absent root. `frontend_session::root_relocation_file_id_and_logical_changes_invalidate_correctly` gates root-only invalidation. |
| Stable source/module/definition identity | Satisfied at the source boundary. `SourceId` hashes bytes; `ModuleId` canonicalizes logical paths; `SourceRevision` combines explicit root and sorted module/content pairs; `StableDefinitionKey` is module/name/namespace/kind/owner based. Relocation, file-ID, ordering, rename, and module-move tests cover the intended distinctions. |
| Immutable per-file/query inputs | Satisfied. `SourceSnapshot` owns `Arc<String>` text plus validated metadata. `SemanticInputDescriptor` owns source revision, module-resolution inputs, target, and stable preview features; `CodegenInputDescriptor` adds optimization level. |
| Batch compatibility | Satisfied with deliberate diagnostic-order adapters. `compile_source_snapshot_with_options_impl` uses `parse_source_snapshot_modules_for_batch` and `merge_parsed_modules_for_batch`; batch/borrowed compatibility tests compare artifacts and diagnostics. |
| Reusable in-process invalidation | Partially satisfied below whole-query granularity. `CanonicalFrontendSession::update` reuses parsed module Arcs and preserves the last good revision after syntax failure. Canonical merge now retains immutable definition-surface shards keyed by module identity, FileId epoch, and ordered definition surface; body-only edits reuse all shards and a module rename rebuilds only its shard. Merge/RIR/semantic queries still execute revision-wide. The benchmark makes both reuse and remaining work measurable. |
| Tooling seams | Substantially satisfied. Backend-free `published`, `import_graph`, `merge`, `rir`, `semantic`, and `stable_definitions` queries exist. Immutable `FrontendDiagnosticSnapshot` artifacts retain exact attempted source metadata/text identity plus syntax, merge, or semantic query identity, errors, and successful warnings; failed syntax does not replace last-good compilation artifacts. Import resolution is lazy, memoized, keyed by owned source/resolution/std-dir inputs, and retains deterministic validation separately. Binary-search module and stable-definition lookup support keyed artifact access. |
| No disk cache or full-LSP shortcut | Satisfied by scope. Session caches are process-owned values only; the benchmark performs no measured filesystem discovery and no backend/link. No persistent cache format, watcher, protocol server, or LSP is introduced. |
| Small mergeable, tracked delivery | Satisfied operationally: the stack is split into described RUE-719 jj changes and independently gated slices. This audit cannot prove external Linear state; commit descriptions are the locally falsifiable tracking evidence. |

## Ranked remaining gaps

1. **Must finish: resolution-only downstream invalidation remains broad.** The
   direct `CanonicalFrontendSession::import_graph` query now has session tests for
   physical relocation, root and std-dir variants, pointer reuse, failed syntax,
   empty graphs, and zero lower/bind work. A physical-only change performs zero
   lex/parse and recomputes import resolution exactly once. It still invalidates
   merge/RIR/semantic artifacts because canonical merged and RIR provenance owns
   exact rebound parsed-module/source-snapshot inputs. Acceptance: either prove
   and implement a relocation-independent merged/RIR artifact boundary, or retain
   current invalidation and make the first dependency-keyed semantic slice reuse
   only values whose provenance excludes physical resolution.

2. **Must finish: RIR and semantic invalidation is still revision-wide.** One
   leaf edit reparses one module and now reuses every unchanged definition-surface
   shard, but still reruns merge duplicate scanning, RIR, binding, and reachable
   body analysis. This is explicitly visible in the benchmark's
   `leaf_body_edit` counters. Per-module RIR fragments are not yet sound because
   `InstRef` ordering, semantic `Spur` handles, and cross-module references belong
   to one request-global universe. Acceptance: introduce fragment-local handles
   plus deterministic global remapping, or choose a dependency-keyed declaration
   or body boundary whose outputs contain no request-global handles.
   ADR-0050 audits the missing const/type/call/method/destructor/generic capture
   surfaces. The first tooling-only `semantic_dependency_inputs` slice supplies
   a stable ordered destination universe, explicit semantic inputs, and complete
   resolved/missing/ambiguous module-import edges. Definition-level edges and
   body/CFG reuse remain intentionally absent.
   Ordinary and specialized free-function callers now retain direct free-call
   events at their existing analysis boundaries. The dependency-input query
   validates specialized origins against the exact source revision and
   translates endpoints to sorted, deduplicated `StableDefinitionKey` edges.
   Relocation/FileId/input order, recursion, later specialization rounds, sibling
   names, and rename sensitivity are gated with zero extra RIR visits; benchmark
   output exposes ordinary events, specialization origins, and specialized
   events. This narrow free-function caller surface is complete, while method
   non-generic named-method callers now also translate exact named owner/method
   identities to tagged free-function or named-method stable targets. Generic
   named methods are proven to have no specialization path today: comptime
   arguments still produce one runtime method body, so their generic provenance
   is explicitly unsupported. Named destructor callers now translate exact
   owner identities and tagged free/method targets. Anonymous methods and
   destructors plus declaration/type/const/drop surfaces keep the overall graph
   explicitly incomplete and prevent body/CFG reuse.
   A conservative resolved declaration-type slice now translates nominal
   signature/field/payload/constant/owner edges without rescanning RIR. A
   separate resolver-time observer records resolved user-defined type-call
   heads before generic composites are erased to `COMPTIME_TYPE`: nested
   `Option(Result(T))` retains both stable function endpoints, and qualified
   `lib.Box(T)` retains the exact defining module. These events are sorted and
   deduplicated and add zero RIR visits. This surface remains explicitly
   `Str(N)` is retained separately as a stable fixed-capacity-string builtin
   input whose preview identity is part of the semantic key; it does not invent
   a definition. No intrinsic returns a type today. Named-owner associated,
   anonymous, unnameable, and dynamic heads are not successful type syntax
   (dotted heads resolve only through modules), which tests pin as fail-closed.
   A narrow supported-head completeness bit is therefore true, while broader
   declaration-type and overall graph completeness remain false.

3. **Later tooling integration: import validation and compiler diagnostics remain
   distinct artifact families.** Syntax, merge, and semantic diagnostics are now
   durable attempted-source artifacts with memoized error/warning identity.
   Import resolution retains deterministic missing/ambiguous findings on its
   separately keyed `CanonicalImportGraphOutput`; these are not compiler errors
   until a language query consumes them. Acceptance for a unified IDE report is
   an explicit aggregation view that preserves both keys and does not relabel an
   import finding as a syntax/semantic error or trigger RIR/backend work.

4. **Later tooling consumer: indexed source-position lookup.** Stable module and
   definition-key lookup now avoid consumer scans, but there is no session query
   from `(ModuleId, byte offset)` to enclosing/reference definition, nor a reverse
   reference index. Acceptance belongs with IDE/navigation design, built over
   canonical parsed and definition artifacts rather than a parallel AST pipeline.

5. **Compatibility debt: public raw-AST and exact-RIR entry points remain.** The
   production snapshot batch and `CompilationUnit` paths are canonical. Public
   `parse_all_files*`, `merge_symbols`, `parse_source_snapshot_module*`,
   `lower_canonical_rir`, `analyze_canonical_program`, and
   `bind_canonical_definitions` permit callers to assemble work manually; tests
   intentionally exercise exact-RIR provenance and legacy equivalence. Acceptance:
   classify and document these as low-level compatibility APIs, then deprecate
   redundant raw-AST routes only after known consumers migrate.

6. **Later infrastructure: persistent cache and full LSP.** Neither is required to
   validate the in-process architecture. Any future disk format must version all
   identity and target/feature inputs; any LSP must consume session artifacts and
   must not introduce a second parser, lowerer, or binder.

## Cache-key intent

Physical relocation is intentionally part of `ModuleResolutionInputs` because
relative imports can resolve differently even when module text and logical IDs
are unchanged. Linker mode is intentionally excluded from semantic identity;
optimization level is included in `CodegenInputDescriptor` but excluded from
stable-definition identity. Tests in `canonical_semantic` and `frontend_session`
gate these distinctions. The remaining work is to make import-only invalidation
more precise, not to remove resolution inputs from the semantic key.
