# ADR-0071 Phase 2 semantic-to-CFG ownership audit

Status: current-source audit, 2026-08-13. This is the focused Phase 2
extension of the
[post-ADR-0063 architecture audit](post-adr-0063-cold-compiler-architecture-audit.md).
It records the live owners and consumers between parsed declarations and
optimized CFG, interprets the Phase 1 reference evidence, and ranks the work
authorized by ADR-0071's vertical milestones. Source and tests are authoritative;
issue descriptions and completed migration notes are context only.

## Result

Rue has one semantic-to-CFG path and the right broad ownership shape. Parsed
modules, declaration facts, semantic bodies, type facts, CFGs, and optimized
CFGs are immutable query results. Body analysis and CFG construction execute
independently, observe exact prerequisite terminals, and use compact local
indices only after the stable semantic boundary. There is no whole-program
mutable semantic arena, peer frontend, or compatibility CFG builder to remove.

The review did find two material present-tense costs:

1. Body analysis repeatedly converts the same declaration-level semantic facts
   into owned body-local provider values. Fixed one-worker Lattice records
   61,474 name lookups, 52,337 declaration-fact reads, and 19,211 durable fact
   materializations across 1,263 dependency-ready body transactions. The exact
   query edges are valuable; repeated owned payload construction is not.
2. Every runtime body then selects an exact fact closure and converts its
   immutable `CanonicalBody` into a complete local AIR/type/symbol/string epoch
   before CFG construction. Lattice performs 1,280 selections and successful
   materializations. Optimization clones the validated CFG into a new immutable
   record while sharing the other local domains. This is the correct locality
   boundary, but its full conversion and retained payload are the next
   architecture-level cost to measure and reduce.

The direction is therefore an immutable, Arc-shareable declaration-fact
substrate feeding exact body-local views, followed by a leaner local semantic
domain for CFG construction. It is not a process-global owner, a new lock, a
whole-program CFG key, or a second frontend.

## Evidence and an important timing correction

The Phase 1 reference is the exact `fresh_source_to_native_v1`, release-ThinLTO,
Rue `-O3`, x86_64 Linux run
`9b7d4bdb4bf398301fa5bd90b6a57e75f4764a982aacf2723c485b59b5398394`.
Lattice's median process time was 2,887.26 ms and compiler-root time was
2,716.88 ms. The fixed scaling run used the same compiler commit and boundary.

The scaling report's `toolchain_acquisition_ns` is an inclusive tracing span
around `FilesystemCompilerHost::acquire_reached_toolchain_modules` in
`crates/rue/src/source_loader.rs`. That operation calls
`rooted_or_toolchain_park`, so on the ordinary no-park path its 1,280.70 ms
one-worker / 1,136.40 ms automatic-worker duration contains the semantic root
attempt. It is not a separate 1.2-second filesystem or standard-library phase,
must not be added to semantic time, and does not establish toolchain I/O as a
bottleneck. The exclusive phase partition remains the authority for phase
budgeting. The inclusive envelope proves only how long the host's park-aware
operation remains active.

The deterministic one-worker Lattice evidence used below is:

| Signal | Exact work |
| --- | ---: |
| source shape | 161 modules, 1,221 source functions, 155,488 tokens |
| semantic name/import lookups | 61,474 / 0 |
| semantic declaration facts | 52,337 |
| durable provider materializations | 19,211 |
| anonymous / producer / toolchain facts | 0 / 14,391 / 456 |
| reachability scans / keys | 12 / 1,298 |
| ready frontiers / scheduled keys | 11 / 1,263 |
| serial transaction fallback | 0 |
| CFG fact index builds | 1 |
| declarations / type nodes indexed | 2,287 / 4,344 |
| exact CFG fact selections | 1,280 |
| CFG retained-charge scans | 1,280 |
| interner entries / UTF-8 bytes scanned | 41,882 / 10.9 MB |
| query claims / reuses | 39,092 / 366,912 |
| validation traversals | 154,956 |
| input / dependency observations | 31,406 / 614,202 |

Counts come from the compiler-produced `CompilerWork` in the Phase 1 scaling
report. Source-derived cardinalities below are called out explicitly rather
than presented as measured events.

## Artifact ledger

The table groups projections which share one owner and invalidation boundary.
“Derived” describes cold construction; retained revisions can validate and
reuse the same immutable terminal instead.

| Artifact | Canonical owner and live consumers | Lifetime, movement, and cold work | Invalidation boundary |
| --- | --- | --- | --- |
| Parsed syntax: `ParsedModule` / `ParsedSyntaxPayload` | `compiler.parse-module` in `revisioned_query_database.rs`; consumed by `ModuleIndex`, declaration occurrence/order/shell/raw-syntax projections, candidate RIR artifacts, and presentation views. `parsed_modules.rs` owns the payload, exact candidate locators, ordered RIR recipes, and cheap snapshot-local aliases. | One query per demanded module. Lattice has 161 modules. Source text, tokens, AST, resolver, definition index, and import sites are Arc-shared; `ParsedAstView` retains the exact module rather than copying the AST. Query hashing uses `ModuleId`, not source bytes. | The exact module-source input and `ModuleRevision`; another module's edit does not dirty this node. |
| Module declaration indexes: `ModuleIndex`, `DeclarationOccurrenceIndex`, `DeclarationOrder`, `DeclarationShellFact` | Their registered families in `revisioned_query_database.rs`; live consumers are exact name/import lookup, raw declaration syntax, semantic nucleus, body source selection, and warning syntax. | `ModuleIndex::new` traverses one module's definitions/imports once and builds name/import partitions. Occurrence and order projections derive one module-local capability/order table; shell and raw syntax select one declaration. The current Lattice CFG index later sees 2,287 durable declarations, which is a lower-bound witness for declaration breadth, not a direct count of every frontend projection. | The owning parsed module. Equality-stable per-declaration projections can remain green when unrelated source positions move. |
| Candidate RIR and whole-program presentation: `DeclarationBodyPlanArtifacts`, transient `CandidateModuleRirOutput`, `CanonicalRirOutput` | One `compiler.declaration-body-plan-artifacts` terminal owns one canonical packed envelope for candidate-local AstGen output, declaration-relative spans, dense spellings, anchors, root, and optional method owner. Semantic bodies and `CompilerSession::rir` decode that same envelope directly; no retained normalized `ValidatedRir`, separate candidate symbol table/basis, registered module-RIR family, or module-wide AstGen evaluator remains. | Native compilation demands only reached candidate terminals. An explicit RIR request demands all candidates in each requested module, decodes/remaps them into module order, then projects the ordered whole-program RIR. Struct recipes provide the sole typed cross-candidate edge from a methodless shell to its method roots. | Exact packed bytes, including declaration-relative diagnostic coordinates. Absolute relocation and `FileId` refresh the current locator without changing the packed artifact; whole-program presentation additionally observes the exact module order. |
| Declaration facts: `SemanticNucleusValue` | `compiler.semantic-nucleus`; exact consumers are `CompilerBodyFactProvider`, type/layout/ABI/drop-glue families, the declaration aggregate, and producer/comptime queries. | One typed node per identity, signature, well-formedness, const/comptime call, anonymous nominal, or deferred-ownership key. Values use stable keys and mostly Arc-backed variable-width fields. Lattice body analysis performs 52,337 fact reads. Provider task caches retain terminals, but trait results are cloned by value. | Exact declaration key, target/preview configuration, and the precise lookup/raw-syntax/nucleus dependencies observed by that projection. |
| Request-wide declaration aggregate: `SemanticNucleusProjection` | `compiler.declaration-semantics-projection`; consumed by body reachability/closure and `RootedBodyGraph`, whose slices feed CFG fact selection. | Once per `(sorted modules, configuration)` root it traverses module occurrence/order tables, requests exact nucleus projections, and allocates sorted declaration, anonymous-nominal, dependency, and C-export slices. Lattice later indexes 2,287 declarations and 4,344 reachable type nodes. This aggregate copies Arc-backed fact values; it does not replace their exact query ownership. | Any observed module membership, declaration order/capability, or requested semantic fact. It is intentionally request-wide; body terminals do not use it as their invalidation key. |
| Per-body toolchain demand: `BodyToolchainDemand` | `compiler.body-toolchain-demands`; consumed by body reachability before transaction dispatch and by the body provider for language-item fallback. | One pure projection of the candidate artifact's typed fallible-intrinsic set per reached body. Canonical packing derives that set while visiting typed RIR, so runtime demand performs no raw-body scan or lexer re-entry. Body analysis performs 456 additional provider reads of those memoized facts. The value is a small sorted Arc slice and performs no filesystem I/O. | Exact candidate artifact and body/configuration key. Host acquisition publishes a successor only when a reached body demands an absent trusted module. |
| Semantic body transaction: `BodyTransaction` and shared `CanonicalBody` | `compiler.body-transaction` owns analysis; `compiler.canonical-body`, `body-references`, `body-produced-anonymous`, and `body-analysis-bundle` are typed projections over that result. Live consumers are reachability, closure publication, warning production, and CFG input assembly. | The dependency-ready scheduler dispatches 1,263 Lattice transactions with zero serial fallback. A success owns one `Arc<CanonicalBody>` shared by transaction, canonical projection, bundle, and `CfgBodyInput`; RUE-1346 made deep copies unrepresentable. Body analysis nevertheless performs 61,474 name lookups and 19,211 owned durable materializations through `CompilerBodyDurableSource`. | Exact body key/configuration plus candidate artifact, signature, name/import, semantic nucleus, producer, type, and toolchain terminals actually observed. Raw declaration bodies remain a separate on-demand input only for deferred comptime evaluation. Independent bodies retain independent invalidation. |
| Reachability and published body graph: `BodyReachabilityOutput`, `BodyClosureOutput` | `compiler.body-reachability`, `compiler.body-closure`, and `compiler.body-closure-publication`; `CompilerSession::rooted_body_graph_with_cancellation` is the live consumer. | One root schedules exact callable/drop-glue reachability and publishes leases for the reached terminal cones. Lattice uses 12 pending scans over 1,298 keys, 11 frontiers, and 1,263 scheduled keys. The closure shares body terminals and aggregates sorted declaration/anonymous slices for downstream selection. | Root modules, root functions, configuration, and exact reached dependency cones. Unreachable body edits do not enter the published graph. |
| Type, layout, ABI, and drop-glue facts | `compiler.type-shape`, `type-facts`, `layout`, `call-abi`, and `drop-glue`; consumers are reachability, CFG materialization, CFG build, and codegen-domain construction. | Stable `TypeInstanceKey`/`FunctionInstanceKey` nodes recursively request exact component facts. `TypeFacts` intentionally repeats `TypeShape` for ownership enumeration while `Layout` observes the separately stamped shape. Variable-width outputs use Arc slices, but shape payloads exist in both terminals. Their traffic is included in the 39,092 claims, 366,912 reuses, and 614,202 dependency observations; Phase 1 does not publish a family-by-family count. | Exact stable type/callable identity, target/preview configuration, and recursively observed semantic/type facts. |
| Request-local CFG selection index: `LocalFactSelectionIndex` | Built once in `CompilerSession::rooted_cfg_with_cancellation` from the rooted graph's declaration/anonymous slices; consumed only by `select_materialization_facts` and dropped before CFG queries run. | Lattice builds one index, scanning 2,287 declarations and 4,344 type nodes, then performs 1,280 selections. The index borrows canonical slices. Each selected `LocalMaterializationFacts`, however, allocates new Arc slices and clones the exact declaration, anonymous, callable, metadata, module, builtin, and required-type values into its `CfgQueryKey`. Equal facts selected for different bodies therefore have independent list allocations and repeated payload clones. | The index is request-local. Each selected CFG key contains only its exact fact closure, preserving body-local invalidation. |
| Unoptimized local semantic epoch and CFG: `CfgRecord` from `compiler.cfg` | `cfg_query::materialize_and_build_cfg` imports `CanonicalBody` plus selected facts into `SemanticLocalMaterialization`, then `CfgBuilder` publishes a `CfgRecord`. Consumers are optimized CFG, AIR/CFG presentation, and focused artifact tests. | One successful local materialization/CFG build per runtime body or drop-glue input; Lattice selects 1,280. The record owns local AIR, validated CFG, type pool, interner, strings, local atoms, codegen domain, and warnings. These are intentionally body-local compact-index domains. Retained-charge accounting scans 41,882 interner entries / 10.9 MB once across the 1,280 publications. | `CfgQueryKey` equality includes function, configuration, canonical body, and exact selected facts. Its hash deliberately uses only function/configuration; typed deep equality remains authoritative on collisions without hashing body payloads on every lookup. |
| Optimized CFG: `CfgRecord` from `compiler.optimized-cfg` | `evaluate_optimized_cfg`; consumed by rooted CFG collection, codegen-unit queries, and CFG presentation. | Optimization clones the unoptimized `ValidatedCfg`, mutates/validates the clone, and publishes a second record. Ordinary records Arc-share AIR, interner, strings, atoms, codegen domain, and warnings; accessor roots additionally import exact accessor CFG/domain payloads. Thus equal non-CFG domains do not allocate independently, but unoptimized and optimized CFG storage coexist. `OptimizedCfgQueryKey` includes the optimization level and exact accessor dependency keys. | Exact unoptimized CFG, optimization level, and accessor dependencies. A local body/fact edit does not invalidate unrelated optimized CFGs. |
| Rooted CFG projection | `RootedCfgOutput` / `RootedCfgUnit` in `session.rs`; live consumers are backend query assembly and the single unstable presentation adapter. | A thin vector of function identities, optimized keys, and Arc records. It does not recompute semantic or CFG artifacts. Its public `air()` accessor keeps AIR reachable through the optimized record even for normal codegen; whether that retained payload is material to the 128 MiB goal is a Phase 3 measurement question. | Same rooted body graph and exact optimized CFG terminals; no peer invalidation domain. |

## Repetition and allocation findings

### Present: repeated body-local declaration materialization

`CompilerBodyFactProvider` correctly records exact name and semantic-nucleus
query dependencies. `CompilerBodyDurableSource`, however, must currently return
owned `DurableConst`, `DurableNominal`, `DurableFunction`, and `DurableMethod`
values to `rue-air`. Nominal fields, enum payload vectors, callable parameters,
and type syntax are reconstructed when another body requests the same fact.
The 19,211 materializations are therefore not 19,211 distinct declarations;
they are body-local imports of a much smaller canonical declaration set.

This is the highest-ranked ownership target because it lies in the semantic
phase that scales from 1,253.49 ms to only 1,106.75 ms while summed body time
nearly doubles under automatic workers. The evidence does not prove that all
semantic scaling loss is allocation, but it proves substantial repeated work
at exactly that boundary.

### Present: full canonical-body to local-AIR conversion before CFG

`CanonicalBody` is already the shared stable semantic result. CFG evaluation
then imports the whole body and its selected facts into fresh local AIR, type,
symbol, string, and atom domains before `CfgBuilder` traverses that AIR. This
preserves small dense indices and independent body execution, but cold builds
cannot reuse those domains: every one of Lattice's 1,280 CFG inputs performs the
conversion once.

The optimized record shares all non-CFG domains with the unoptimized record, so
there is no second AIR import. It does retain the AIR through the optimized root
for presentation and artifact access even though backend lowering consumes CFG,
type, interner, string, atom, and codegen-domain data. That is a plausible
resident-memory reduction, not yet a demonstrated clock-time win.

### Present but bounded: repeated aggregate indexes

`SemanticNucleusProjection` first allocates a sorted declaration slice.
`body-reachability` clones that slice into a `BTreeMap` for reachability facts;
`rooted_cfg_with_cancellation` later builds a borrowing
`LocalFactSelectionIndex` over the same slice. This is two request-local indexes
over 2,287 Lattice declarations, not an N-bodies-times-N-declarations path:
RUE-1356 already removed the latter. Consolidating the immutable lookup
substrate would improve ownership clarity and remove bounded work, but it cannot
by itself close a multi-second gap.

### Present but secondary: publication-time retained-charge scans

Every `CfgRecord` walks its local append-only interner to publish a logical
retained charge. The scan is linear in that body's entries and totals 41,882
entries / 10.9 MB of UTF-8 for Lattice. It is exact bookkeeping and only 1,280
scans, so it ranks below semantic and AIR materialization unless a CPU profile
shows otherwise. A pre-accounted interner could remove the scan without changing
ownership.

### Not a separate bottleneck: toolchain acquisition

The inclusive host envelope contains the semantic root attempt. The 456
toolchain-fact reads are real semantic-provider observations, but the 1.1--1.3
second duration cannot be attributed to those reads or to I/O. Treating that
span as an additive phase would optimize the wrong owner.

## Historical and transitional classification

Historical issues are not live architecture findings:

- RUE-1346 removed deep `CanonicalBody` copies across transaction, projection,
  bundle, and CFG input; current source shares one immutable Arc.
- RUE-1351 removed the serial anonymous-producer transaction path on maintained
  workloads; Lattice now records zero serial transactions.
- RUE-1356 replaced one declaration/type scan per CFG selection with one
  request-local index; the remaining scan is linear in request facts.
- RUE-1477 and RUE-1480 removed peer semantic/CFG assembly and compatibility
  facade paths. Presentation, fuzzing, oracle, and one-shot compilation now
  consume the canonical rooted graph.

No semantic-to-CFG artifact in the current source is intentionally transitional.
Fresh linking is an accepted ADR-0063 boundary, not a transitional semantic/CFG
owner, and it is outside this audit. Canonical merged/RIR artifacts are explicit
on-demand parse projections with zero normal source-to-native work, not a
compatibility semantic path.

## Ranked Phase 3 work

### 1. Share canonical declaration payloads through body analysis

Prototype an immutable, Arc-shareable declaration-fact record owned by the
existing exact semantic-nucleus terminals. Adapt the `rue-air` durable-source
boundary so a body can import borrowed/shared parameter, nominal-field,
variant, and type-syntax payloads without rebuilding equivalent Vecs. Keep the
provider's exact terminal observations: sharing a payload must not replace a
per-declaration dependency with the request-wide aggregate.

Acceptance evidence:

- body parallelism and ready-frontier counts remain unchanged;
- exact body-query invalidation and retained-session edit behavior remain
  unchanged;
- durable materialization count is refined into payload-share versus owned-copy
  counters and the owned-copy count falls materially from 19,211;
- one-worker instructions/allocations and semantic wall time do not regress;
- output, diagnostics, warnings, and executable bytes remain exact.

### 2. Measure and narrow the local semantic epoch

Add exact selected-fact cardinalities and local AIR/type/string payload sizes,
then profile `materialize_canonical_body` and `CfgBuilder` separately. Use that
evidence to decide whether to:

- make local materialization lazy for facts/types not reached by CFG lowering;
- fuse one-way canonical-body import with CFG construction while retaining one
  canonical AIR presentation projection; or
- split optimized CFG's retained payload so normal backend roots do not keep
  presentation-only AIR alive.

The prototype must keep the existing `compiler.cfg` query as the sole
AIR-to-CFG owner. A second “fast CFG” evaluator or presentation-only semantic
compiler is forbidden.

### 3. Reuse one immutable request-wide lookup substrate

Give `SemanticNucleusProjection` an immutable index, or provide a single
request-owned index object consumed by both reachability and CFG fact selection.
Selected CFG keys must still own only exact fact closures. Measure elimination
of the second 2,287-declaration traversal and any allocation reduction; expect a
bounded architectural win, not the 500 ms milestone by itself.

### 4. Remove linear retained-charge rescans if still visible

Maintain exact entry/UTF-8 charge as the local interner grows and publish the
pre-accounted value. Preserve logical retained-charge semantics and confirm the
1,280 scans / 41,882 entries / 10.9 MB fall to zero. Do this after the dominant
materialization work unless the shifted profile elevates it.

## Structural constraints for every prototype

The following are non-negotiable falsifiers, not preferences:

- no process-global mutable semantic or type arena;
- no new global lock on provider reads, body analysis, fact selection, or CFG
  construction;
- no whole-program semantic aggregate embedded in a body or CFG memo key;
- no coarser invalidation than the exact terminals currently observed;
- no peer semantic, AIR, or CFG computation selected by emit/presentation mode;
- no loss of independent body scheduling or bounded ready frontiers;
- no timing win obtained by weakening Rue `-O3`, output, diagnostics, warnings,
  or the source-to-native boundary.

If a prototype needs a coarser query, retained `LoweredMir`, or a different
terminal boundary, it requires a separate architectural decision under ADR-0071
section 8. The Phase 3 parent should begin with the first bounded payload-sharing
experiment and reprofile after each accepted change rather than committing to a
single large semantic rewrite.
