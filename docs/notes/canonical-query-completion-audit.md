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
| Reusable in-process invalidation | Satisfied at whole-query granularity, not fine-grained semantic granularity. `CanonicalFrontendSession::update` reuses parsed module Arcs, preserves the last good revision after syntax failure, and invalidates merge/RIR/semantic/definition caches when the published program changes. The benchmark makes this behavior measurable. |
| Tooling seams | Partially satisfied. Backend-free `published`, `import_graph`, `merge`, `rir`, `semantic`, and `stable_definitions` queries exist; semantic output exposes warnings. Import resolution is lazy, memoized, keyed by owned source/resolution/std-dir inputs, and consumes canonical parsed import sites without RIR. Binary-search `ParsedProgram::module(ModuleId)` and `BoundDefinitionSet::definition_by_key(StableDefinitionKey)` support keyed artifact access. Diagnostics remain returned errors rather than a retained query result. |
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

2. **Must finish: semantic invalidation is still revision-wide.** One leaf edit
   reparses one module but clears and reruns merge, RIR, binding, and reachable
   body analysis. This is explicitly visible in the benchmark's
   `leaf_body_edit` counters. Acceptance: introduce dependency-keyed reuse at one
   post-parse boundary with a counter proving unaffected work is retained; do
   not hide whole-pass inefficiency behind only an outer cache.

3. **Must finish: frontend diagnostics are not durable query artifacts.** Syntax,
   merge, and semantic failures return `CompileErrors`; only successful semantic
   output retains warnings. A tooling client must own errors and correlate them
   with the attempted snapshot, while the session keeps the prior published
   revision. Acceptance: expose an immutable attempted-revision diagnostic result
   (including warnings on success) whose provenance can be checked without
   backend work and whose failed query is safely memoized.

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
