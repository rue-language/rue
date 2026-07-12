# Canonical query architecture completion audit

This audit covers authoritative trunk plus the local RUE-719 stack through
production in-process declaration reuse. It records observable APIs, structural
gates, and the remaining boundaries; it is not a claim that Rue has persistent
incremental compilation.

## Requirement evidence

| Requirement | Status and falsifiable evidence |
| --- | --- |
| Trustworthy phase/work baselines | Satisfied for the current frontend boundary. `compile_source_snapshot_with_options_and_work` returns `CanonicalPipelineWork`; `CanonicalFrontendSessionWork` separates calls, executions, and reuses. The compiler timing tree retains the non-nested `rir_declaration_index` leaf for both ordinary and durable paths. `rue-compiler-session-bench` hard-gates cold, no-op, edit, identity, failure/recovery, stable-ID, manifest, planning, and durable-reuse work at N=4 in ordinary tests and N=128 opt-in. |
| Pass/IR boundary simplification | Satisfied for snapshot batch and `CompilationUnit`: both use canonical parsed-module assembly, merge, `lower_canonical_rir`, and canonical semantic analysis. Compatibility projections are lazy and cannot trigger a second canonical lower. Declaration processing has explicit shell, resolution/import, and body-readiness phases. |
| Explicit module root | Satisfied. `SourceMetadata::new` requires a root `FileId`; `SourceRevision` and `ModuleResolutionInputs` retain a root `ModuleId` and reject an absent root. Root relocation and identity tests gate invalidation. |
| Stable source/module/definition identity | Satisfied at the source and named-declaration boundary. `SourceId` hashes bytes; `ModuleId` canonicalizes logical paths; `SourceRevision` combines an explicit root with sorted module/content pairs; `StableDefinitionKey` owns module/name/namespace/kind/owner identity. Relocation, file-ID, ordering, rename, and module-move tests gate the distinctions. Anonymous structural owners intentionally have no invented stable identity. |
| Immutable per-file/query inputs | Satisfied. `SourceSnapshot` owns `Arc<String>` text plus validated metadata. Semantic/codegen descriptors own source revision, resolution context, target, stable preview features, and (for codegen) optimization. Stable definition fingerprints are schema-versioned and partition declaration, signature, and authoritative body/initializer source. |
| Batch compatibility | Satisfied with deliberate diagnostic-order adapters. Snapshot batch uses canonical parse/merge/lower/analyze paths, and parity tests compare artifacts, warnings, and failures. Durable projection tests compare ordinary and imported AIR epochs through bodies and CFGs. |
| Reusable in-process invalidation | Satisfied for planning and supported declaration payloads, not bodies/CFGs. Production manifests can produce `Incremental` with deterministic fingerprint deltas and reverse closure and zero RIR scan; observed unsupported endpoints produce `Full` with exact blockers. Supported body edits install stable-keyed durable named-declaration payloads atomically into fresh shells and skip ordinary declaration resolution. Cold compilation seeds the baseline from its primary bind, so cache population performs no second bind. Ordinary non-generic free functions now also publish observational compiler-owned durable body candidates after exact owner/input joins and fresh-epoch validation, but no dedicated production body cache consumes a candidate or skips analysis. Merge/RIR and current body/CFG work remain revision-wide. |
| Tooling seams | Substantially satisfied. Backend-free `published`, `import_graph`, `merge`, `rir`, `semantic`, and `stable_definitions` queries exist. Durable diagnostic snapshots own exact attempted source and syntax/merge/semantic query identity. Binary-search module and stable-definition lookup support keyed access. Import validation remains a separate, honestly keyed artifact family. |
| No disk cache or full-LSP shortcut | Satisfied by scope. Session caches are process-owned values. No persistent cache format, watcher, protocol server, filesystem discovery in measured updates, backend, or link is introduced. |
| Small mergeable, tracked delivery | Satisfied locally by independently described and gated RUE-719 jj changes. This repository audit cannot prove external Linear state; change descriptions are its falsifiable tracking evidence. |

## Production invalidation and reuse contract

The semantic dependency manifest owns stable input fingerprints, the ordered
definition universe, import topology, and direct stable edges for supported
free calls, named methods, named destructors and implicit drop glue, declaration
types/type constructors, and named constant dependencies. Completeness is a
production-derived sorted blocker set. Supported programs therefore reach
`SemanticInvalidationScope::Incremental`; anonymous drop owners or future
unrepresentable endpoints force `Full` rather than fabricating an identity.

The session retains successful durable declaration exports only after the whole
ordinary request succeeds. A later request may reuse them only when root,
target, preview features, stable definition universe, and declaration/signature
fingerprints match. Projection validates the entire current shell universe and
installation is atomic. Supported records are named structs, enums, free
functions, non-generic named methods/associated functions, and named
destructors. A successful 128-module reachable-root-body-edit test requires 128 records
compared, reused, and installed; zero ordinary declaration resolution; exact
fresh-batch functions/CFGs, strings, warnings, and body-analysis work; and zero
cache-population binds. Separate failure/recovery and unsupported-surface tests
gate diagnostic parity and cache-poisoning behavior.

Constants, module values, function aliases, generic named methods, and anonymous
structural owners currently take the ordinary path. Direct end-to-end session
tests require generic named-method and anonymous-structural revisions to execute
ordinary resolution, install zero durable records, publish exact fresh-session
artifacts/diagnostics, and leave no partial baseline that can poison recovery.
These fallbacks are supported correctness behavior, not incremental reuse.

## Ranked remaining gaps

1. **Body and CFG invalidation remains revision-wide.** Declaration resolution
   is reusable, but canonical merge/RIR, reachable body analysis, specialization,
   and CFG construction still run for a successful edited revision. Further
   reuse requires stable body/specialization outputs without request-local
   `InstRef`, `Spur`, type-pool, or CFG identities; it must not be inferred from
   the declaration speedup. The [RUE-720 body/CFG incrementality audit](body-analysis-cfg-incrementality-audit.md)
   maps the live ownership and identity boundaries and identifies the complete
   structural work ledger as the first gated implementation slice. RUE-720 now
   also emits ordered stable input records for supported ordinary analyzed
   bodies, including exact owner/dependency fingerprints and local fail-closed
   blockers, but retains no AIR or CFG artifacts yet.

2. **Resolution-only changes conservatively invalidate downstream artifacts.**
   The import graph is independently memoized, but merged/RIR provenance owns
   exact current parsed/source inputs. A future relocation-independent boundary
   must prove relative-import and diagnostic provenance rather than dropping
   physical resolution inputs from semantic keys.

3. **Unsupported durable declaration surfaces remain explicit.** Constants,
   module values, aliases, generic named-method environments, and anonymous
   structural owners need stable representations before they can join the
   durable cache. Until then they must continue to resolve ordinarily with zero
   partial installation.

4. **Tooling consumers remain later work.** Import validation and compiler
   diagnostics need an aggregation view that preserves both keys. Position to
   enclosing/reference definition and reverse-reference indexes do not yet
   exist. A future LSP must consume these session artifacts and must not create a
   parallel parser, lowerer, or binder.

5. **Compatibility API debt remains.** Public raw-AST and exact-RIR entry points
   permit manual phase assembly. They should be classified and deprecated only
   after known consumers migrate to the canonical module-rooted APIs.

6. **Persistent caching remains deliberately later.** Any disk representation
   must version identity, fingerprint schema, target, and feature inputs and must
   preserve the same fail-closed projection rules. In-process values are not a
   disk format.

## Cache-key intent

Physical relocation is part of `ModuleResolutionInputs` because relative
imports can resolve differently even when text and logical IDs are unchanged.
Linker mode is excluded from semantic identity; optimization is included in the
codegen descriptor but excluded from stable-definition identity. The remaining
precision work is to prove narrower artifact provenance, not to weaken these
keys.
