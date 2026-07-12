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
| Reusable in-process invalidation | Partially satisfied below whole-query granularity. `CanonicalFrontendSession::update` reuses parsed module Arcs and preserves the last good revision after syntax failure. Canonical merge now retains immutable definition-surface shards keyed by module identity, FileId epoch, and ordered definition surface; body-only edits reuse all shards and a module rename rebuilds only its shard. A cached semantic invalidation planner now computes stable fingerprint deltas and deterministic reverse closure with zero RIR scanning, but production manifests explicitly fail closed to full invalidation while the dependency graph is incomplete. Merge/RIR/semantic queries still execute revision-wide. The benchmark makes both reuse and remaining work measurable. |
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
   explicitly fail closed and prevent body/CFG reuse when encountered.
   Completeness is now a
   production-derived, sorted blocker set rather than an opaque global boolean;
   the planner carries the exact union from both revisions. Supported
   production fixtures now reach `Incremental` for no-op and body-only deltas,
   with exact reverse closure, reusable-key counts, and zero additional RIR
   traversal. Anonymous drop owners and future unsupported heads still add
   surface-specific blockers and force `Full` when observed. Generic named
   methods now retain the authoritative reference
   sets from their single runtime body under the stable declaration caller key.
   A conservative resolved declaration-type slice now translates nominal
   signature/field/payload/constant/owner edges without rescanning RIR. A
   separate resolver-time observer records resolved user-defined type-call
   heads before generic composites are erased to `COMPTIME_TYPE`: nested
   `Option(Result(T))` retains both stable function endpoints, and qualified
   `lib.Box(T)` retains the exact defining module. These events are sorted and
   deduplicated and add zero RIR visits. Resolved nominal leaves in deferred
   generic arrays and pointers are retained before placeholder erasure, and
   type aliases retain their exact value-constant identity as well as the
   resolved nominal target. Declaration-type identity is therefore complete
   for successful named declarations. `Str(N)` is retained separately as a stable fixed-capacity-string builtin
   input whose preview identity is part of the semantic key; it does not invent
   a definition. No intrinsic returns a type today. Named-owner associated,
   anonymous, unnameable, and dynamic heads are not successful type syntax
   (dotted heads resolve only through modules), which tests pin as fail-closed.
   Named and builtin call-head completeness are therefore true for successful
   supported programs; successful programs without evidence-based blockers now
   have overall graph completeness.
   Named value-constant initializers now retain direct tagged dependencies on
   constants, comptime functions/type constructors, named types, and module
   bindings at the existing collector/evaluator seams. Recursive source context
   is preserved, cycles fail before publishing, and exact stable translation
   adds zero RIR visits. Module bindings remain import-topology inputs rather
   than value-constant sources. The value-constant slice is narrowly complete;
   dynamic/anonymous surfaces remain explicitly fail-closed when encountered.
   Stable definition input fingerprints are now schema-versioned and split at
   authoritative AST boundaries into identity/visibility, signature, and
   body-or-const-initializer components. Named struct signatures frame around
   method bodies, so a method body edit changes only that method payload rather
   than its owner type. Struct fields and enum variants remain exact
   signature-only inputs. Malformed joins fail closed, and hashes remain stable
   across relocation, FileId assignment, and input order. This improves delta
   classification and now drives production `Incremental` plans. Retaining and
   reusing semantic artifacts remains a separate later slice.
   Declaration binding now exposes an owned boundary after global namespace
   validation, builtin injection, and named struct/enum shell registration.
   The ordinary adapter crosses it immediately, with phase counters proving one
   setup, predeclaration, resolution, and body-readiness finalization. This is
   Callable/value shells are now predeclared deterministically at that boundary.
   Free functions, named methods/associated functions, constants (including
   function-valued aliases after evaluation), and named destructors carry
   logical-path identities, while spans, parameter/source order, generic
   context, and authoritative current-revision body/initializer handles remain
   separate from resolved payload. The ordinary payload-install/finalize adapter
   still resolves in historical order exactly once. Anonymous structural methods
   remain blocked on a durable structural-owner identity. A syntactic `const`
   cannot be classified as a value versus module binding until dependency-ordered
   initializer evaluation, so both share the pre-resolution value identity.
   Callers without logical symbol paths receive a request-local fallback that is
   deliberately not cross-relocation joinable. No cached payload is installed,
   and no declaration-resolution work may yet be skipped or reported as reused.
   The compiler now has a stable-key projection adapter between durable payloads
   and exact-current-revision declaration shells. Its join is total and
   bijective for supported named functions, methods, associated functions,
   structs, enums, and destructors; it fails atomically on universe, identity,
   shape, or provenance mismatch and reports zero RIR visits. A comparison-only
   seam runs ordinary and installed epochs through body analysis and CFG
   construction and compares durable exports, functions/CFGs, strings, warnings,
   and error diagnostics across relocation, input order, and multiple modules.
   Constants/module values/function aliases and anonymous owners remain typed
   fallbacks. Generic named-method installation also remains a prerequisite:
   its declaration-scoped type-parameter environment is not yet reconstructed
   by the installer, so production reuse must continue to fail closed there.

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
