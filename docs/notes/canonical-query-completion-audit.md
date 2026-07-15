# Compiler session architecture completion audit

This audit records the canonical compiler boundary after RUE-720.

## Supported boundary

| Requirement | Evidence |
| --- | --- |
| Owned source identity | `SourceSnapshot` owns validated source text and metadata with an explicit root and stable logical modules. Filesystem discovery remains a caller responsibility. |
| One semantic graph | `CompilerSession` owns parse, import, merge, RIR, semantic, diagnostics, invalidation, stable dependency, durable body, and durable CFG state. Returned query artifacts are immutable `Arc` values. |
| One batch adapter | `compile_snapshot` is the only public `compile_*` entry point. It uses the same session queries as long-lived consumers before backend emission and linking. |
| Explicit inputs | `CompileOptions` carries target, linker, optimization, and preview features. Semantic, CFG, and link descriptors own only their relevant subsets. |
| Diagnostics | Attempted and last-good diagnostics have bounded retention. Failed semantic requests retain value-only work while leaving successful artifact baselines intact. |
| Per-definition reuse | Supported declarations, bodies, stable free-function specializations, and CFGs cross versioned stable projection/import boundaries. Unsupported artifacts fail closed individually to the ordinary path. |
| Work accounting | Schema-11 N=4/N=128 workloads hard-gate exact body/CFG comparisons, imports, fallbacks, skipped analyses, avoided builds/optimization, recovery, and cold/fresh parity. |
| Public API control | `api_inventory` requires the supported snapshot/session/query surface, rejects retired peers, and permits exactly one public `compile_*` function. |

## Architectural invariants

Syntax-only AST presentation is not a semantic path. Backend presentation
accepts session-produced CFG artifacts. The CLI owns filesystem/project and
standard-library discovery plus user-facing rendering. `rue-oracle` owns its
evaluator state but obtains compiler semantics and CFGs through
`CompilerSession`.

Durable declaration, body, specialization, and CFG artifacts are projections
of the canonical query graph, not peer phase machines. They use stable logical
identity and atomically remap into fresh request-local domains. Raw interners,
FileIds, spans, type-pool indices, AIR references, and CFG-local identities do
not cross the durable boundary. Exact missing provenance selects ordinary
computation.

## Deliberately separate work

Persistent caches, filesystem watching, editor protocols, and stable
position/reference indexes remain separate projects and must extend the session
artifact graph rather than add another parser, lowerer, binder, or driver.
RUE-813 generalizes differential testing beyond RUE-720's bounded oracle;
RUE-901 adds realistic project/performance scenarios.

Physical relocation remains part of `ModuleResolutionInputs` because relative
imports may resolve differently when text is unchanged. Linker mode is excluded
from semantic identity. Optimization is excluded from body identity and is an
explicit CFG/codegen key.
