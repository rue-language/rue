# ADR-0071 horizontal and vertical compiler ownership re-audit

Status: current-source audit, 2026-08-16. This note supersedes the Phase 2
semantic-to-CFG ranking and the post-ADR-0063 implementation snapshot. It
reviews the whole maintained source-to-native path after the RUE-1510 frontend
integrity work and the accepted ADR-0071 ownership changes. Source and tests are
authoritative; completed issues and the superseded notes remain historical
evidence.

## Result

Rue does not need a major pipeline redesign. Its broad structure is conventional:
module-local parsing, candidate-local lowering, stable semantic facts, per-body
analysis, per-function CFG and optimization, target-specific MIR, register
allocation, object projection, and a fresh link. The query graph has one
production owner for each of those computations. Presentation requests consume
the same artifacts instead of selecting peer frontends, semantic engines, CFG
builders, or backends.

The re-audit found three real appendices and one suspected retention appendix:

1. CFG preparation imported a stable semantic body into a fresh local AIR/type/
   symbol epoch, then immediately walks the new AIR beside the stable body to
   reconstruct `CfgDomainProjection`. Translation and reverse-map construction
   were two traversals of the same boundary. This patch removes the parallel
   body walk: the issuing AIR and its admitted identity mappings now own the
   projection.
2. `CodegenUnit` normalized machine code into sections, atoms, relocations, and
   symbols, but object generation reconstructed an owned legacy
   `FunctionBackendProduct` from those fields. Ordinary codegen also retained
   flattened and atomized rodata simultaneously and eagerly formatted backend
   presentation text. This appendix is now complete: the terminal owns one
   link-ready representation, while object and presentation consumers borrow
   thin views.
3. The optimized `CfgRecord` retains the unoptimized record's AIR and other
   presentation domains even though ordinary codegen consumes CFG/type/codegen
   domains. Attribution showed that these values are Arc-shared with the
   dependency cone rather than copied, and retained-edit tests reclaimed the
   superseded generation. Removing the field would save only an Arc and weaken
   the exact dependency boundary, so this is not an appendix to remove.
4. Adaptive layout/drop query batches nested under the already-parallel
   optimized-CFG batch usually had no worker reservation available, but still
   built structured child tasks, queues, wait edges, and validation authorities
   for every exact request. The adaptive API now reserves once and executes in
   the current task when that reservation is saturated. It preserves every
   exact query edge while removing scheduling work that cannot create
   parallelism.

The first two ownership items and the adaptive scheduling fix are completed
below without coarsening the query graph, phase ownership, or invalidation. The
paired AIR clock and memory result was neutral, so it is an ownership and
maintenance improvement rather than a claimed speedup. The CodegenUnit cleanup
is a measured retention improvement. Optimized-CFG attribution rejected the
suspected representation change, while the nested-batch experiment produced a
material wall-time and queueing improvement.

## Current vertical map

| Stage | Canonical owner | What crosses the boundary | Assessment |
| --- | --- | --- | --- |
| source discovery and import staging | the driver plus `CompilerSession` continuation and exact frontier publication | immutable source/module revisions | Iterative discovery is intentional because the host owns filesystem reads. RUE-1516 makes successor work exact-frontier rather than accumulated-frontier work. |
| lexing and parsing | `compiler.parse-module` | Arc-shared module syntax, resolver, candidate locators, definition index, and import sites | Conventional module-local query. No second normal parse path. |
| candidate RIR | `compiler.declaration-body-plan-artifacts` | one packed, candidate-local, file-independent RIR envelope | One candidate AstGen owner. Normal compilation demands reached candidates; whole-RIR presentation composes the same candidates. |
| declaration semantics | `compiler.semantic-nucleus` and exact type/layout/ABI/drop-glue queries | stable definition/type/function keys and immutable fact payloads | Shared payload work is now effective; provider materialization is 97.2% shared on Lattice. |
| body semantics | `compiler.body-transaction` | immutable stable semantic body plus exact dependency observations | Per-body scheduling is conventional and independently invalidated. No normal source reparse or alternate semantic engine. |
| local AIR and CFG | `compiler.cfg` | a body-local AIR/type/symbol epoch, `CfgDomainProjection`, validated CFG, warnings, and codegen domains | One owner. AIR is the sole live body representation, and the projection derives from AIR plus its admitted stable identities. |
| optimized CFG | `compiler.optimized-cfg` | cloned-and-optimized CFG plus Arc-shared local domains | The clone is an immutable-query tradeoff, not a peer optimizer. Attribution confirms that AIR/domain storage is shared with the exact dependency cone. |
| target backend | `compiler.codegen-unit` | one per-function target-specific lowering, MIR, liveness, allocation, scheduling, emission, and requested artifacts | One target-selected owner. Shared planning modules make x86-64/AArch64 policy explicit without pretending their MIRs are identical. |
| object generation | `compiler.object-projection` | serialized object bytes | Canonical query boundary; it projects borrowed `CodegenUnit` sections, ordered atoms, and relocations into transient linker-builder inputs. |
| linking | `ProgramImagePlan` and the fresh linker | ordered object bytes and export thunks | Deliberately fresh under ADR-0063. The final owned-byte adapter is not a second codegen path. |

## Current horizontal map

The same review across phases found the following owners:

- **Identity:** stable module, definition, type, and function-instance keys cross
  query boundaries. Dense RIR/AIR/CFG/MIR indices remain local to their owning
  artifact. No native parser `Spur` or local instruction reference is used as a
  stable identity.
- **Source provenance:** packed candidate RIR stores declaration-relative span
  information. Current `FileId`, path, and absolute coordinates come from the
  current source basis. This keeps semantic equality independent of file
  relocation while diagnostics remain current.
- **Symbols:** candidate-local dense spellings are packed with RIR; body-local
  AIR and backend domains own request-local interners. The boundaries are
  explicit, though local semantic import remains more construction than the CFG
  builder itself.
- **Type facts:** stable type queries own cross-body facts. `compiler.cfg`
  materializes only the selected local closure into dense AIR types, then
  requests exact layout and drop-glue prerequisites through stable keys.
- **Presentation:** AST/RIR/AIR/CFG/backend presentation consumes canonical
  artifacts. It does not choose a different compiler. Backend artifact text is
  retained only when explicitly requested; ordinary `CodegenUnit` values carry
  typed artifact fields rather than an eager formatted presentation string.
- **Cancellation and failure:** query cancellation remains distinct from typed
  parse, semantic, RIR, CFG, codegen, and resource failures. Long candidate
  packing/materialization and body work have bounded checkpoints.
- **Retention:** query families retain immutable values with explicit charges
  and bounded history. Publication-time interner rescans are gone. Optimized
  CFG roots Arc-share their AIR/domain values with the exact dependency cone;
  they do not retain a second physical body representation.

## Current measurement

The directional local reference is three fresh release-ThinLTO, Rue `-O3`,
one-worker x86-64 Linux compilations of Lattice on 2026-08-16. It is not a
replacement for the ADR-0071 reference machine, but deterministic work was
identical across the three runs.

| Signal | Median or exact work |
| --- | ---: |
| complete compiler root | 651.99 ms |
| peak process memory | 364,658,688 bytes |
| source loading | 55.45 ms |
| provider semantic analysis | 141.33 ms |
| local semantic materialization | 41.70 ms |
| CFG domain projection | 9.66 ms |
| layout/drop prerequisite phase | 20.27 ms |
| actual CFG builder | 5.43 ms |
| CFG publication | 26.07 ms |
| CFG optimization | 14.28 ms |
| codegen units | 90.92 ms |
| object serialization | 2.76 ms |
| linker | 6.25 ms |
| body transactions / CFG epochs | 1,263 / 1,280 |
| provider payloads shared / owned | 10,582 / 306 |
| local AIR instructions / type entries | 52,567 / 32,513 |
| layout requests / drop-glue requests | 25,141 / 25,141 |
| query claims / dependency observations | 33,897 / 283,413 |

The important ratio is not merely semantic versus backend time. Creating and
publishing each local semantic/CFG domain costs substantially more than the
`CfgBuilder` traversal itself. At the start of this re-audit,
`CfgDomainProjection::from_local_body` accounted for about 9.7 ms after the
approximately 41.7 ms import had already translated the same instructions,
types, symbols, and spans. Source review established duplicated boundary
ownership, although the completed paired experiment below showed that its
lockstep validation was not the counter's dominant clock cost. By contrast,
the recorded object-serialization value is only a pre-cleanup baseline. It
does not establish a post-cleanup performance result; the coordinator must
measure the current ownership boundary before reporting one.

## Comparison with established query compilers

The comparison does not justify collapsing Rue to one IR or one global arena.

- rustc query values are expected to be immutable and cheaply cloneable, using
  interning or `Rc`/`Arc` for non-trivial results. Rue's immutable query
  terminals and stable-key boundaries follow that model.
- rustc groups MIR passes at useful intermediate query boundaries and codegen
  consumes `optimized_mir`. Multiple IR states and a distinct optimized body
  are normal; the design question is whether an intermediate must remain
  retained, not whether it may exist.
- rustc sometimes "steals" an intermediate MIR body to avoid cloning it. Rue's
  overlapping pinned revisions and independently demandable presentation make
  that exact ownership trick a poor direct fit, but it is useful evidence that
  avoidable whole-body copies should be questioned.
- Salsa treats the tracked function as the unit of reuse, stores interned
  identities in the database, and permits heavy memo values to be evicted while
  retaining dependency information. Rue implements the corresponding concepts
  with stable keys, explicit terminal leases, and family retention policies.

What is unusual in Rue is therefore not parse→RIR→AIR→CFG→MIR. The local AIR
boundary now follows the conventional shape: AIR is the sole live body
representation, and CFG derives its relocation domain from AIR without
consulting a parallel semantic body. The CodegenUnit object-projection
appendix is also closed. Optimized-CFG retention is ordinary Arc sharing, and
the measured query fanout problem was nested scheduling overhead rather than a
second semantic computation.

Primary references:

- [rustc queries](https://rustc-dev-guide.rust-lang.org/query.html)
- [rustc MIR queries and passes](https://rustc-dev-guide.rust-lang.org/mir/passes.html)
- [Salsa IR identities](https://salsa-rs.github.io/salsa/tutorial/ir.html)
- [Salsa memo retention and eviction](https://salsa-rs.github.io/salsa/tuning.html)

## Present, historical, and intentionally transitional work

### Present

- large exact layout/drop-glue request fanout remains part of CFG invalidation,
  but saturated nested requests no longer build child-task machinery that
  cannot run in parallel.

### Historical and already removed

- body-local copying of most declaration provider payloads;
- one declaration/type scan per CFG selection;
- publication-time local-interner retained-charge scans;
- module-wide normal AstGen and per-body source reconstruction/reparse;
- the runtime raw-body lexical scan for fallible intrinsic demand;
- normalized candidate RIR plus separately retained symbol/basis/index copies;
- peer semantic/CFG/codegen paths selected by presentation mode;
- the string-spelling semantic type-syntax resolver in production. Current
  production call sites use the structured resolver; inventory and consistency
  tests retain the retired name only as a prohibition.

### Intentional

- driver-owned iterative import discovery across filesystem reads;
- candidate-local RIR and body-local AIR/CFG dense identities;
- separate unoptimized and optimized CFG query values;
- separate x86-64 and AArch64 MIRs with shared policy/planning helpers;
- fresh object linking at the final product boundary.

## Completed in this re-audit

### AIR owns the CFG input domain

`CfgDomainProjection::from_local_body` now derives types, symbols, strings,
spans, places, and parameter-drop facts from the issuing local AIR and its
existing type pool/interner. It no longer walks AIR beside `SemanticBody` or
reconciles two instruction/place schemas. Compile-time-only module/error types
are excluded because they cannot survive CFG lowering; every runtime type still
requires an exact canonical identity. Compact local handles can intentionally
represent more than one semantic identity—for example, the synthetic nominal
`[i32]` and `Slice<i32>`—so the projection seeds its reverse lookup from the
materializer's existing admitted identity entries before deriving the remaining
AIR types. This preserves the one authoritative representation without adding
a shadow domain structure.

Six order-balanced parent/current Lattice pairs on the post-RUE-1538 trunk
produced the exact same executable hash (`b893e76c…85e5`). The complete compiler
root favored the change by 13.84 ms paired median and peak RSS by 25,346,048
bytes, but the targeted domain counter regressed by 2.47 ms paired median. The
root and memory samples include unrelated host and allocator variance, so this
note does not claim a compiler speedup from a change whose own counter became
slower. The result is an ownership and maintainability win. It also identifies
canonical type and callable projection, rather than schema lockstep validation,
as the remaining measured cost at this boundary.

### CodegenUnit owns the object-generation input

`CodegenUnit` is the canonical typed link-ready terminal: its defined symbol,
section metadata, ordered text/rodata atoms, normalized relocations, requested
typed backend artifacts, and content fingerprint are retained exactly once.
Text is one atom; rodata atom boundaries, order, duplicates, empty values, and
UTF-8 bytes are preserved. Object projection now accepts `&CodegenUnit`
directly, validates section shape and target relocation compatibility with
typed failures, and makes only transient `Vec`/`String` copies required by the
linker-owned `ObjectBuilder`. Those builder copies are not retained compiler
shadow state. Runtime-symbol discovery derives from ordered relocations, and
exact query equality compares all retained typed fields rather than a Debug
presentation string or hash alone.

Six order-balanced parent/current release Lattice pairs produced the exact same
1,662,976-byte executable (`45784ce7…9a89e7`). Peak RSS favored the canonical
terminal by 5,767,168 bytes paired median externally and 5,840,896 bytes in the
compiler report. Complete-root time was effectively neutral at -0.77 ms paired
median. Object serialization itself regressed by 1.09 ms paired median because
the direct projector now validates the typed section contract before borrowing
its contents. This is therefore a measured retention and ownership win, not a
wall-time speedup; eliminating the retained aliases does not eliminate the
linker-owned transient copies or the new fail-closed validation.

### Optimized CFG retains shared domains, not a shadow body

Disposable Lattice attribution measured 58,929,825 logical retained bytes
across 52,605 unoptimized-CFG pins and 36,379,773 bytes across 1,280 optimized
CFG pins. The optimized records' component accounting included 3,051,356 AIR
bytes, 3,056,851 optimized-CFG bytes, 13,269,961 domain bytes, 4,306,650 type-
pool bytes, 1,838,510 interner bytes, and 8,508,519 codegen-domain bytes. Those
figures are logical reachability charges, not distinct physical allocations:
the optimized terminal and its exact unoptimized dependency share the same
Arcs. A retained-edit witness grew and then reclaimed both families' superseded
generation (`cfg` 6,117→8,988→6,117; `optimized-cfg`
5,653→8,430→5,653). Removing the AIR/domain fields would therefore save only
Arc headers while weakening the terminal's self-contained typed boundary. The
suspected retention appendix is rejected.

### Saturated adaptive batches stay in the current task

The outer optimized-CFG batch normally reserves all nested worker capacity.
Inner adaptive layout/drop batches were consequently creating structured child
tasks and donating permits even though they could not recruit another worker.
`query_registered_adaptive_batch` now preserves the one-worker streaming path,
atomically reserves wider-runtime capacity once, and runs stable-order exact
queries in the current task when the reservation is zero. A nonzero reservation
still uses the structured parallel path, and public exact-batch semantics are
unchanged.

Six order-balanced parent/current Lattice pairs at four workers produced the
exact same 1,662,976-byte executable (`45784ce7…9a89e7`). Complete-root time
improved by 50.81 ms paired median. Ready items fell from 64,720 to 11,174,
permit donations from 5,362 to 1,542, summed ready wait by 34.72 seconds, and
maximum ready wait by 49.45 ms; claims were unchanged. Peak RSS increased by
about 1.88 MB externally and 1.85 MB in the compiler report. Six one-worker
control pairs preserved identical work and output and favored the change by
6.12 ms paired median, within run noise; their ready-item and donation counts
remained zero. This is a scheduling speedup, not eliminated compiler
computation: all exact layout/drop queries and their invalidation edges remain.

## Ranked next work

The ownership and measured query-scheduling appendices identified by this
re-audit are closed. Further work should start from a fresh profile rather than
the rejected optimized-CFG split or aggregation of layout/drop facts. Exact
repeated requests remain necessary invalidation edges; future changes must
identify a measured computation, representation, or scheduling owner without a
global cache or coarser body invalidation.

## Structural falsifiers

- no second frontend, semantic engine, CFG builder, optimizer, backend, or
  presentation-selected compiler path;
- no process-global mutable semantic/type arena or new broad lock;
- no whole-program fact aggregate in a body or CFG key;
- no coarser body invalidation or loss of independent scheduling;
- no native local index crossing its artifact boundary without checked remap;
- no performance claim without same-output paired measurement;
- no architectural cleanup described as a speedup when its measured clock
  result is neutral.
