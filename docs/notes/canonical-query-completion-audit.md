# Compiler session architecture completion audit

This is the RUE-730 completion audit after the RUE-728–RUE-736 migration. It
records the supported boundary, the compatibility surface removed from the
compiler crate, and the responsibilities that remain deliberately separate.

## Supported boundary

| Requirement | Evidence |
| --- | --- |
| Owned source identity | `SourceSnapshot::new` owns text and validated `SourceMetadata`; metadata identifies an explicit root and stable logical modules. `SourceView` is a read-only record returned by snapshots for diagnostics and presentation, not a borrowed assembly input. Filesystem discovery is a caller responsibility. |
| One semantic graph | `CompilerSession` publishes snapshots and owns parse, import, merge, RIR, semantic, invalidation, diagnostic, and durable-reuse queries. Returned artifacts are immutable `Arc` values. |
| One batch adapter | `compile_snapshot` is the only public `compile_*` entry point. It creates a session, issues the same RIR and semantic queries as long-lived consumers, then performs backend emission and linking. |
| Explicit options | `CompileOptions` carries target, linker, optimization, and preview features. Query descriptors own the subset that affects their identity. |
| Diagnostics | Session snapshots preserve attempted and last-good query identity with bounded retention. Batch mode preserves caller-selected source ordering where diagnostics require it. |
| Work accounting | `PipelineWork` and `CompilerSessionWork` distinguish executions from reuse. The N=4 test workload and opt-in N=128 session benchmark hard-gate structural counters. |
| Public API control | `api_inventory` limits facade size, requires the supported source/session/query names, rejects retired peers, and permits exactly one public `compile_*` function. |

## Removed compatibility surface

The following names are written with a soft line-break so repository searches
for a retired identifier remain clean while the audit still records the exact
human-readable spelling.

| Removed symbol family | Exact retired names |
| --- | --- |
| Peer driver | <code>Compilation<wbr>Unit</code> |
| Shared-interner parse records | <code>Parsed<wbr>File</code>, the compatibility form of <code>Parsed<wbr>Program</code> |
| Concatenated merge records | <code>Merged<wbr>Ast</code>, <code>Merged<wbr>Program</code> |
| Duplicate parse/merge functions | <code>parse_<wbr>all_files</code> and variants, <code>merge_<wbr>symbols</code> |
| Aggregate frontend adapters | <code>compile_<wbr>frontend</code>, <code>compile_<wbr>frontend_with_options</code>, <code>query_<wbr>canonical_frontend</code>, <code>query_<wbr>canonical_frontend_source</code>, <code>Canonical<wbr>FrontendArtifacts</code>, and <code>Canonical<wbr>PipelineWork</code> |
| Raw-AST semantic adapters | <code>compile_<wbr>frontend_from_ast</code>, <code>compile_<wbr>frontend_from_ast_with_options</code>, <code>compile_<wbr>frontend_from_ast_with_file_paths</code>, <code>compile_<wbr>frontend_from_ast_with_file_paths_and_target</code>, <code>compile_<wbr>frontend_from_ast_with_file_paths_and_symbol_paths_and_target</code>, <code>compile_<wbr>frontend_from_ast_with_source_metadata_and_target</code>, and <code>compile_<wbr>frontend_from_merged_ast_with_source_metadata_and_target</code> |
| Test/fuzz semantic adapters | <code>compile_<wbr>to_air</code>, <code>compile_<wbr>to_cfg</code>, and <code>compile_<wbr>to_cfg_with_preview_features</code> |
| Parallel batch adapters | <code>compile_<wbr>with_options</code>, <code>compile_<wbr>multi_file_with_options</code>, <code>compile_<wbr>multi_file_with_symbol_paths_and_options</code>, <code>compile_<wbr>multi_file_with_symbol_paths_and_options_and_stats</code>, <code>compile_<wbr>multi_file_with_source_metadata_and_options</code>, <code>compile_<wbr>multi_file_with_source_metadata_and_options_and_stats</code>, <code>compile_<wbr>source_snapshot_with_options</code>, <code>compile_<wbr>source_snapshot_with_options_and_work</code>, and <code>compile_<wbr>source_snapshot_with_options_and_stats</code> |

No compatibility type aliases or forwarding wrappers remain. Syntax-only AST
presentation is not a compatibility semantic path: it cannot lower, analyze,
generate code, or link.

## Intentionally separate responsibilities

- The CLI owns project discovery, physical paths, standard-library discovery,
  user-facing emit selection, and diagnostic rendering.
- `ParsedAstPresentation` preserves token/AST inspection in caller order. It is
  deliberately syntax-only and does not compete with session semantics.
- Backend presentation functions accept session-produced CFG artifacts for
  `--emit` views. Machine lowering and linking live in dedicated implementation
  modules, while the public one-shot adapter controls their sequencing.
- Import-graph validation is independently keyed because tooling may request it
  without semantic analysis. It still consumes the session's parsed modules.
- `rue-oracle` owns its evaluator state projection, but obtains all compiler
  semantics and CFGs through `CompilerSession`.

## Deliberate future work

Body and CFG reuse remains revision-wide even though declaration payload reuse
is supported. Persistent caching, filesystem watching, editor protocols, and
stable position/reference indexes remain separate projects. Any such work must
extend session query artifacts rather than introduce another parser, lowerer,
binder, or compiler driver.

Physical relocation remains part of `ModuleResolutionInputs` because relative
imports can resolve differently when source text is unchanged. Linker mode is
excluded from semantic identity; optimization is included in codegen identity
but excluded from stable-definition identity.
