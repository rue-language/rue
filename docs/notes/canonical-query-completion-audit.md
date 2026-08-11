# Compiler session architecture completion audit

> [!WARNING]
> Historical implementation audit. This note describes the compiler before
> ADR-0063's revisioned query-runtime cutover. See the
> [post-ADR-0063 cold compiler architecture audit](post-adr-0063-cold-compiler-architecture-audit.md)
> for the current source map; [ADR-0063](../designs/0063-parallel-demand-driven-incremental-compilation.md)
> remains the architectural authority.

This audit records the canonical compiler boundary after RUE-720 and the
completion boundary for RUE-627.

## RUE-627 completion decision

RUE-627 is complete at its stated **query-ready and performance-characterized**
boundary. The compiler now has one owned source model, one canonical session,
immutable query artifacts, explicit dependency/invalidation inputs, measured
module and per-definition edit reuse, and a frozen semantic-to-backend metadata
boundary. It does not yet claim to be a long-lived incremental compilation
service.

The implementation evidence is:

| RUE-627 work area | Current evidence |
| --- | --- |
| Delivered measurement | The retired fresh-process and session harnesses characterized phase time, peak RSS, source/work accounting, and bounded cold-versus-reused edit sequences. Focused clone and lock decisions were measured in RUE-659 and RUE-660. |
| Explicit identity and module graph | `SourceSnapshot`, `SourceId`, `ModuleId`, `SourceRevision`, and `ModuleResolutionInputs` carry validated root, source, logical-module, relocation, and import identity independently of load order. |
| Per-module syntax and indexes | `ParsedModule` retains its AST, symbol provenance, definition candidates, and revision. Canonical merge/lowering translates those artifacts into one request-local semantic universe. |
| Query and invalidation seams | `CompilerSession` owns parse, import, merge, RIR, semantic, body, CFG, diagnostic, dependency-manifest, and invalidation-plan state. Public query results are immutable `Arc` artifacts; missing durable provenance fails closed to ordinary computation. |
| Proven semantic/backend edit reuse | The schema-11 completion workload checks no-op, unrelated-body, reachable-body, specialization, error, recovery, N=4, and N=128 scenarios. It compares reused work with cold work and executable/diagnostic artifacts. Parsed syntax is reusable between unchanged modules, while edits still reparse their containing module. |
| Frozen backend input | Semantic finalization consumes the mutable `TypeInternPool` only after specialization and destructor/type discovery, producing `FrozenTypeInternPool`. CFG, optimization, and native backends accept the frozen read-only API. |
| Shared compiler implementation | Batch compilation is the thin `compile_snapshot` adapter over `CompilerSession`; CLI presentation and `rue-oracle` also consume session artifacts rather than owning peer frontends. |

The maintainer-approved completion amendment on RUE-627 records three explicit
scope dispositions that must not be inferred as present features:

- Cooperative cancellation, stale-result suppression/isolation, revisioned
  long-lived transactions, bounded service memory, and the diagnostics/hover
  LSP prototype move to RUE-648. Closing RUE-627 unblocks that service; it does
  not satisfy RUE-648's acceptance criteria.
- Parse reuse is module-granular. Finer-than-module reuse needed to ensure that
  a one-function edit never repeats unrelated parsing moves to RUE-648's
  demand-driven reuse and invalidation work.
- A generic allocation/clone-volume and lock-contention telemetry baseline is
  waived for RUE-627 closure. RUE-659 and RUE-660 supply focused evidence for
  the concrete clone/lock decisions; RUE-901 owns broader realistic scenario
  measurement.

The remaining architecture work is intentionally transitional and tracked:
RUE-812 owns a typed query-state design including cancellation semantics;
RUE-813 broadens the bounded cold-versus-reused oracle to generated edit
sequences; RUE-818 replaces imperative clearing with dependency-derived
invalidation; and RUE-901 adds realistic project benchmark scenarios. None of
those projects should introduce a second compiler graph.

## Supported boundary

| Requirement | Evidence |
| --- | --- |
| Owned source identity | `SourceSnapshot` owns validated source text and metadata with an explicit root and stable logical modules. Filesystem discovery remains a caller responsibility. |
| One semantic graph | `CompilerSession` owns parse, import, merge, RIR, semantic, diagnostics, invalidation, stable dependency, durable body, and durable CFG state. Returned query artifacts are immutable `Arc` values. |
| One batch adapter | `compile_snapshot` is the only public `compile_*` entry point. It uses the same session queries as long-lived consumers before backend emission and linking. |
| Explicit inputs | `CompileOptions` carries target, linker, optimization, and preview features. Semantic, CFG, and link descriptors own only their relevant subsets. |
| Diagnostics | Attempted and last-good diagnostics have bounded retention. Failed semantic requests retain value-only work while leaving successful artifact baselines intact. |
| Per-definition reuse | Supported declarations, bodies, stable free-function specializations, and CFGs cross versioned stable projection/import boundaries. Unsupported artifacts fail closed individually to the ordinary path. |
| Frozen backend metadata | `FrozenTypeInternPool` exposes typed immutable reads after the last legal semantic mutation. CFG construction, optimization, and both native backends use it without pool locks or routine whole-definition clones. |
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

Persistent caches, filesystem watching, editor protocols, cancellation, and
stable position/reference indexes remain separate projects and must extend the
session artifact graph rather than add another parser, lowerer, binder, or
driver. RUE-648 owns the service and LSP boundary; RUE-813 generalizes
differential testing beyond RUE-720's bounded oracle; RUE-901 adds realistic
project/performance scenarios.

Physical relocation remains part of `ModuleResolutionInputs` because relative
imports may resolve differently when text is unchanged. Linker mode is excluded
from semantic identity. Optimization is excluded from body identity and is an
explicit CFG/codegen key.
