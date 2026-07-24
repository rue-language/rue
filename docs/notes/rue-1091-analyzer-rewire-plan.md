# RUE-1091 provider-driven analyzer rewire — plan (ANALYSIS ONLY)

Branch `claude/rue-1083-investigation-gi72gu`, head `1359057c` (B2 counters atop trunk
`3253bf0a`). Scope: rewire rue-air body analysis (`analyze_one_body_instance` call graph) to
consume ONLY `BodyFactProvider` (`crates/rue-air/src/sema/provider.rs`) + `BodySemanticOverlay` +
body-local config, byte-identical artifacts, as a sequence of INERT reviewable slices. Production
epoch path untouched until the flip. This plan resolves B1 (the "no provider-driven analyzer" wall
from `3d-findings.md`) into an ordered build.

Prereqs read: `AGENTS.md`, `scratchpad/rue-1091-flip-plan.md`, `scratchpad/3d-findings.md`.

---

## EXECUTIVE SUMMARY (15 lines)

1. The hypothesis PARTIALLY holds. The internal resolution traits are the right boundary, but only
   ONE of ~11 resolution families is behind one today: `SemanticModulePathProvider` /
   `SemanticTypeSyntaxProvider` (`semantic_type_resolution.rs`), which fully covers TYPE-syntax and
   module-path resolution — logic is provider-generic; the `SemaTypeSyntaxProvider` impl
   (`typeck.rs:33-559`) is the only thing that reads `self.sema`.
2. The other ~10 families — endpoint/definition selection, free-function calls, method calls,
   operator overloads, module-member calls, aggregate/field/variant resolution, pattern
   enum-through-module, intrinsic/well-known-`Option`, and the whole-universe **InferenceContext
   snapshot** — read epoch tables DIRECTLY, inline, not behind any trait. ~130 direct read sites.
3. Every direct read maps cleanly to an existing `BodyFactProvider` op (§1 inventory). None needs a
   provider op that doesn't exist. So the hoists are mechanical, not design-open — with ONE heavy
   exception.
4. The heavy exception is `build_inference_context` (`declarations.rs:125`): it iterates the ENTIRE
   epoch universe (`functions`, `structs_by_file_name`, `methods`, `value_consts()`, …) into an
   `InferenceContext` once per body. This is the O(U) per-body cost RUE-1090 reads as `ACTIVATE`.
   Making it demand-populated is the substantive rewire.
5. Diagnostic rendering does NOT create dependency edges. `format_type_name`/`is_type_copy`
   (`typeck.rs:1493,1559`) read only `type_pool` + `ctor_type_displays` (materialized state); no
   whole-universe "did you mean" scan exists. Edges are recorded at MATERIALIZATION
   (`observe_materialized_type`, `record_*`), never at render. Recommendation grounded in §2.
6. Seam design: keep `SemanticTypeSyntaxProvider`; add `SemanticCallResolutionProvider` (calls +
   methods + operators + module members + aggregates) and `BodyEndpointProvider` (endpoint/token →
   selection), and make `InferenceContext` a lazily-populated cache behind a `SignatureFacts` seam.
   Two impl families per trait: **EpochFacts** (today's behavior, byte-identical, becomes THE
   production impl immediately — refactor-only) and **ProviderFacts** (BodyFactProvider+overlay,
   test-only until flip).
7. Slice r1 hoists all direct reads behind the traits and extracts EpochFacts, re-points
   production at it — a byte-identical refactor proven by full corpus + oracle, zero counter/gate
   movement. r2..rN add ProviderFacts per domain, each with domain-scoped differential tests.
8. Verdict: internal traits as boundary — **PARTIALLY HOLDS**. 1 family already hoisted; ~10
   families / ~130 direct reads need hoists, all bounded/mechanical except the InferenceContext
   demand-population (1 L-sized sub-effort). After r1..rFinal the flip is the honest one-slice
   delete the envelope rule requires.

---

## 1. READ INVENTORY

Scope: every epoch-table read reachable from `analyze_one_body_instance` (`one_body.rs:992`) →
`analyze_one_body` → `analyze_definition`/`analyze_named_method`/`analyze_named_destructor`/
`analyze_anonymous_member`/specialization → `analyze_single_function`/`analyze_method_function`
(`analysis/functions.rs`) → expression/statement/pattern/call/aggregate/intrinsic/operator checking,
comptime evaluation, specialization (`specialize.rs`), reference projection, and diagnostic
rendering.

**Epoch universe** = `DeclarationNamespace` fields (`sema/mod.rs:132`): `functions`,
`functions_by_file_name`, `function_source_names`, `builtin_structs`, `structs_by_file_name`,
`builtin_enums`, `enums_by_file_name`, `methods`, `named_method_declarations`, `const_resolutions`
(via `value_const`/`module_binding`); plus `Sema` epoch fields: `stable_definition_endpoints`,
`stable_definition_tokens`, `stable_module_endpoints`, `stable_module_tokens`, `module_registry`,
`declaration_index`, `anonymous_methods`, `named_callable_methods_by_symbol`,
`anonymous_callable_methods_by_symbol`, `rir`, `interner`, `well_known_option_by_payload`,
`trusted_standard_library_files`, `file_paths`, `builtin_arch_id`/`os_id`/`data_model_id`.

**Body-local (NOT epoch — stays, feeds diagnostics)**: `type_pool` (grows as types materialize),
`ctor_type_displays`, `canonical_anonymous_types`, `anon_struct_*`, all `body_*_dependencies` /
`declaration_type_*` observers, `generated_structs`/`generated_enums`, `one_body_*`.

### 1A. Endpoint / definition selection — DIRECT (one_body.rs)

| file:line | fact | behind-trait? | BodyFactProvider op | hoist |
|---|---|---|---|---|
| `one_body.rs:870,1365,1441,1501,1521` | `stable_definition_endpoints` / `stable_module_endpoints` get by token → (file,name,kind,owner) | direct | `declaration_identity`, endpoint via `ModuleRef`/`DeclarationRef` handle | M |
| `one_body.rs:889,1374,1541,1673,1699` | `functions_by_file_name` / `structs_by_file_name` / `named_method_declarations` (file,sym)→id | direct | `lookup_unqualified/qualified` (ModuleItem/Destructor ns) | M |
| `one_body.rs:906,1553` | `function_info(name)` / `functions[&name]` → `FunctionInfo` (is_generic,is_extern,file_id) | direct | `signature` + `declaration_identity` | M |
| `one_body.rs:1562,1826` | `declaration_index.first_free_function` / `.destructors()` → RIR `InstRef` | direct | RIR body handle carried on `DeclarationRef` (body key is body-local input) | M |
| `one_body.rs:1183,1692` | `method_info((struct_id,name))` → `MethodInfo` | direct | `method_candidates` + `signature` | M |
| `one_body.rs:1405-1508` | `materialize_instance_type`: `builtin_structs`, `generated_structs`, `builtin_enums`, `structs_by_file_name`, `enums_by_file_name`, `anon_struct_identities`, `module_registry` | direct | `lookup_*` + `signature` + `language_item`; anon from body-local | M |
| `one_body.rs:245,645` | `target_dependency`/`body_references`: `builtin_enums`/`builtin_structs`/`generated_structs` membership, `stable_definition_token` | direct | `lookup_*` classification (candidate `kind`) | S |

### 1B. Type-syntax + module-path resolution — ALREADY BEHIND INTERNAL TRAIT

| file:line | fact | behind-trait? | BodyFactProvider op | hoist |
|---|---|---|---|---|
| `semantic_type_resolution.rs:165,552,704` | `resolve_semantic_module_path` / `_comptime_call` / `_type_syntax` — generic over `P` | **trait (logic)** | n/a (logic is provider-generic) | — |
| `typeck.rs:67-115` (`SemanticModulePathProvider` impl) | `resolve_module_binding_in_file`, `module_registry.get_def`, `get_file_path` | impl reads sema | `resolve_import` + `lookup_qualified` (ModuleItem) | S (swap impl) |
| `typeck.rs:153-321` (`root_/module_struct/enum/type_alias`) | `structs_by_file_name`, `enums_by_file_name`, `struct_id_for_name`, `resolve_builtin_*`, `type_pool.*_metadata`, `resolve_const_info_in_file`, `value_const` | impl reads sema | `lookup_unqualified/qualified` (ModuleItem) + `signature`/nominal facts | S (swap impl) |
| `typeck.rs:442-509` (`root_/module_constructor`) | `resolve_function_name_local`, `functions.get`, `record_declaration_type_call_head` | impl reads sema | `lookup_*` + `signature` (returns_type from sig) | S (swap impl) |
| `typeck.rs:537-558` (`reduce_comptime_call`) | `reduce_type_ctor_body` (comptime interp over materialized types) | impl reads sema | `const_comptime` + body-local reduction | M |
| declaration-binding branches (`typeck.rs:240,307,455,487`) | `collect_free_function_signature_during_binding`, `try_resolve_indexed_const_during_binding` | gated `declaration_binding_active` = **FALSE during body query** (`mod.rs:1179`, set true only `declarations.rs:543`) | inert on query path — no hoist | — |

Because the type-syntax family is provider-generic already, the ProviderFacts rewire here = a second
impl of the two traits. **This is the template the other families must be refactored to match.**

### 1C. Free-function / method / operator / module-member call resolution — DIRECT (calls.rs, analysis.rs, builtin_ops.rs)

| file:line | fact | behind-trait? | BodyFactProvider op | hoist |
|---|---|---|---|---|
| `calls.rs:226,246,273` | free call: `resolve_const_info_in_file` (alias), `resolve_function_name_local`, `source_function_name` | direct | `lookup_unqualified` (ModuleItem) + `const_comptime` (fn-valued const) | M |
| `calls.rs:275-279`, `analysis.rs:1432,1515,1841,2124` | `functions.get(name)` → `FunctionInfo` | direct | `signature` + `declaration_identity` | M |
| `calls.rs:816,909,1342`, `analysis.rs:337,789,833,2178`, `builtin_ops.rs:95`, `ownership.rs:374,385`, `aggregates.rs:203` | `method_info`, `method_symbol`, `_methods_by_symbol` — method + operator overload selection | direct | `method_candidates` / `operator_candidates` + `signature` | M |
| `calls.rs:1129-1146`, `aggregates.rs:365,635,929,961,1058` | module-member call/type: `module_registry.get_def`, `value_const(mfile,name)`, `resolve_function_name_local` | direct | `resolve_import` + `lookup_qualified` | M |
| `analysis.rs:2371`, `one_body.rs:1699` | `named_method_declarations` → RIR decl | direct | `method_candidates` handle → body key | M |

### 1D. Aggregate / field / variant / pattern resolution — DIRECT (aggregates.rs, visibility.rs)

| file:line | fact | behind-trait? | BodyFactProvider op | hoist |
|---|---|---|---|---|
| `aggregates.rs:36-76,402-428` | struct/enum literal head: `value_const`, `enums_by_file_name`, `structs_by_file_name`, `resolve_builtin_*` | direct | `lookup_unqualified` (ModuleItem) + `signature` | M |
| `aggregates.rs:649,690,967,1008,1061,739` | module-qualified struct/enum/const member | direct | `lookup_qualified` + `resolve_import` | M |
| `visibility.rs:115` (`resolve_enum_through_module`) | `enums_by_file_name` for `mod.Enum::Variant` patterns | direct | `lookup_qualified` (ModuleItem, Enum kind) | S |
| `visibility.rs:27-89` (`is_accessible`, `check_unqualified_visibility`) | `get_file_path` → `file_paths` domain + `is_pub` | direct | `NameResolution.visible()` + candidate `is_public` + `SemanticVisibilityDomain` (already on facts) | S |

### 1E. Inference context (whole-universe snapshot) — DIRECT, HEAVY (declarations.rs, type_inference.rs)

| file:line | fact | behind-trait? | BodyFactProvider op | hoist |
|---|---|---|---|---|
| `declarations.rs:125-…` (`build_inference_context`) | iterates `functions`, `builtin_structs`, `structs_by_file_name`, `builtin_enums`, `enums_by_file_name`, `methods`, `anonymous_methods`, `value_consts()` → whole-universe `InferenceContext` (`inference_ctx.rs:28`) | direct, O(U) | per-key demand: `signature` (func_sigs), `method_candidates`+`signature` (method_sigs), `lookup_*`+`signature` (struct/enum types), `const_comptime` (const maps) | **L** |
| `type_inference.rs:23,38,59,66,73,112,149,304` | inference-time member/const/function/method lookups: `value_const`, `resolve_function_name_local`, `module_registry`, `method_info`, `anonymous_methods` | direct | same ops as above, on demand | M |
| `type_inference.rs:353-357` | `InferenceContext` seeded from epoch by-file maps | direct | demand-populated cache | L (with above) |

### 1F. Intrinsic / well-known Option / comptime — DIRECT (intrinsics.rs, comptime_eval.rs)

| file:line | fact | behind-trait? | BodyFactProvider op | hoist |
|---|---|---|---|---|
| `intrinsics.rs:1217,1330,1503` (`resolve_option_result_type`) | `well_known_option_by_payload` map | direct | overlay-local `WellKnownTypes` (B3) fed by per-body demand resolution | M |
| `comptime_eval.rs` (10 sites) | const/type reduction over materialized `type_pool` + `value_const` | mostly body-local; `value_const` direct | `const_comptime` for imported consts; rest body-local | M |
| `analysis/intrinsics.rs` `@target_*` | `builtin_arch_id`/`os_id`/`data_model_id` + `target` | direct | body-local config (`SemanticQueryConfiguration`); enums are body-local materialized | S |

### 1G. Diagnostic rendering — reads MATERIALIZED state only (NO epoch reads, NO new edges)

| file:line | fact | behind-trait? | edge? | hoist |
|---|---|---|---|---|
| `typeck.rs:1493` (`format_type_name`) | `ctor_type_displays`, `type_pool.struct_metadata(id).name`/`enum_metadata`/`array_def`/`ptr_*_def` | body-local materialized | NO — edge recorded when the type was materialized via the type-syntax provider op | — |
| `typeck.rs:1559` (`is_type_copy`) | `type_pool.struct_def(id).is_copy`, `enum_def`, `array_def` | body-local materialized | NO — same | — |
| error construction across `analysis.rs`/`calls.rs`/`aggregates.rs` | `CompileError` from names already in hand | body-local | NO whole-universe suggestion/"did you mean" scan exists (grep-verified) | — |

**Verified**: no diagnostic path scans `functions`/`structs_by_file_name`/`methods` to render a
suggestion. The only `.iter()`/`.keys()` in the diagnostic neighborhood (`analysis.rs:1637,3169,3275`)
are match-arm suggestions over local scrutinee state, not epoch tables. So rendering-only reads of
the epoch universe **do not exist**.

**Direct-read tally**: type-syntax family = 1 (already behind a trait). Families needing hoists:
1B-impl-swap (types), 1A (endpoints), 1C (calls/methods/operators/members), 1D (aggregates/patterns),
1E (inference context), 1F (intrinsics/comptime) = ~10 families, ~130 raw sites. All map to an
existing provider op; only 1E is L (demand-population), the rest S/M and mechanical.

---

## 2. SEAM DESIGN — the internal-trait boundary

### 2.1 Existing trait (keep)
- `SemanticModulePathProvider<S,M,A>` + `SemanticTypeSyntaxProvider<S,M,A,K,N,T,V>`
  (`semantic_type_resolution.rs:71,274`). Logic is already provider-generic
  (`resolve_semantic_type_syntax`, `_comptime_call`, `_module_path`). Covers ALL type-syntax and
  module-path resolution and its visibility/observation. **No change to the traits; add a second
  impl (ProviderFacts).**

### 2.2 New internal traits (hoist the direct reads behind these)
Mirror the type-syntax design: extract the inline resolution LOGIC into provider-generic free
functions, and define point-query traits whose impls supply facts.

- **`SemanticCallResolutionProvider`** — the value/call analog of `SemanticTypeSyntaxProvider`.
  Methods (each a point query, candidate-set-returning where applicable):
  `root_function(scope,name)`, `module_function(module,name)`, `const_callable_alias(scope,name)`,
  `method_candidates(receiver,name)`, `operator_candidates(receiver,op)`,
  `struct_literal_head(scope,name)` / `module_struct_head`, `enum_variant_head`,
  `field_signature(receiver,field)`. Backs `calls.rs`, `analysis.rs` call/method sites,
  `builtin_ops.rs` operators, `aggregates.rs` literals/fields/variants, `visibility.rs`
  enum-through-module. Maps 1:1 onto `BodyFactProvider::lookup_unqualified/qualified`,
  `method_candidates`, `operator_candidates`, `signature`, `const_comptime`, `resolve_import`.
  Selection (candidate-set → winner, visibility filter, kind filter) stays in the provider-generic
  logic (uses `NameResolution.of_kind/visible`) — **not** in the impl. This is what keeps
  BodyFactProvider's "candidate sets, not winners" contract honored (RISK R1 driver).

- **`BodyEndpointProvider`** — endpoint/token → selection for `one_body.rs`
  (`analyze_definition`/`analyze_named_method`/`analyze_named_destructor`/specialization base/
  `materialize_instance_type`/`materialize_argument_value`). Methods: `endpoint(token) →
  (file,name,kind,owner)`, `select_free_function`, `select_named_method`, `select_destructor`,
  `select_nominal(token)`, `select_module(token)`. Maps onto `declaration_identity`, `lookup_*`,
  `signature`, and the `DeclarationRef`/`ModuleRef`/`ReceiverType` associated handles + the RIR
  body handle (body key is a permitted body-local input, `flip-plan §1 #9`).

- **`SignatureFacts` (InferenceContext demand seam)** — the L item. Replace the eager
  `build_inference_context` whole-universe projection with an `InferenceContext` that is a
  lazily-populated cache: on first lookup of a `func_sig`/`method_sig`/struct-or-enum-type/const,
  materialize it through `SemanticCallResolutionProvider`/`SemanticTypeSyntaxProvider` and cache.
  The `InferenceContext` public shape (`inference_ctx.rs:28`) can stay; only its POPULATION changes
  from "iterate epoch" to "fill on miss via provider". Constraint generation is untouched.

- **B3 `WellKnownTypes` overlay-local** — `intrinsics.rs:resolve_option_result_type` reads
  `well_known_option_by_payload`; port `install_well_known_option_types`
  (`binding_manifest.rs:2000+`) to populate an overlay-owned map from the per-body demand
  resolution, never from the epoch `Sema`. Consumer is the rewired analyzer; ships with r-endpoints.

### 2.3 Two impl families per trait
- **EpochFacts** (production, immediate): reads today's `self.sema` epoch tables — the current
  `SemaTypeSyntaxProvider` impl is already exactly this. r1 extracts EpochFacts impls for the NEW
  traits from the inline call/method/aggregate/endpoint code, and re-points production at the
  provider-generic logic driven by EpochFacts. **Byte-identical, refactor-only.** No `#[cfg(test)]`.
- **ProviderFacts** (test-only until flip): reads `&dyn BodyFactProvider` + `BodySemanticOverlay`;
  materializes each consulted fact into the overlay (mint token / intern type into overlay
  `type_pool` with byte-identical metadata). `#[cfg(test)]` behind the differential adapter until
  the flip. Constructed per body from `CompilerBodyFactProvider` (flip-plan §2, compiler side).

### 2.4 Does diagnostic rendering create dependency edges?
**No — it is fed from already-materialized overlay/type_pool state; recommend not edge-recording at
render.** Grounding: ADR-0066 §4 "the typed provider call that supplies the semantic fact is the
dependency observation." Every fact whose value can change diagnostic/artifact TEXT is recorded when
it is MATERIALIZED — `observe_selected_named_type`/`observe_materialized_type`
(`typeck.rs:323-348`) for types, `record_declaration_type_call_head`/`record_named_const_dependency`
for calls/consts, `method_candidates`/`signature` provider calls for methods. By the time
`format_type_name`/`is_type_copy` render, the struct/enum metadata is in the overlay's `type_pool`
(materialized through the provider), and `ctor_type_displays` is body-local. **Defensible rule
applied**: "any fact that can change the artifact/diagnostic TEXT must be edge-recorded" — it IS,
at materialization; and the audit found NO rendering-only read of the epoch universe (no suggestion
scan), so no render site needs a new edge. The only obligation this imposes on ProviderFacts: when
it materializes a nominal type it must populate `type_pool` metadata (name, file_id, is_pub,
is_builtin, is_copy) IDENTICALLY to the epoch install — that byte-identity is a differential-test
target (RISK R2), not a new edge.

---

## 3. DOMAIN SLICES (ordered, each inert + reviewable)

### r1 — hoist direct reads behind internal traits; extract EpochFacts; re-point production
- **Scope**: define `SemanticCallResolutionProvider`, `BodyEndpointProvider`, `SignatureFacts`
  seam; move inline resolution LOGIC from `calls.rs`/`analysis.rs`/`aggregates.rs`/`builtin_ops.rs`/
  `visibility.rs`/`one_body.rs` into provider-generic free functions; implement EpochFacts for each
  (verbatim current reads); re-point production at logic-over-EpochFacts. `InferenceContext` stays
  eager for now (EpochFacts fills it) — demand-population is r-inference. **No `#[cfg(test)]`, no new
  path: the epoch reads simply move behind a trait method.**
- **Files**: `sema/provider.rs` (traits alongside `BodyFactProvider`), new
  `sema/call_resolution.rs` (generic logic, mirror of the template module
  `crates/rue-air/src/semantic_type_resolution.rs` — note it lives at the crate `src/` root, NOT
  under `src/sema/`), `calls.rs`, `analysis.rs`, `aggregates.rs`, `builtin_ops.rs`, `ownership.rs`,
  `visibility.rs`, `one_body.rs`, `typeck.rs`.
- **Tests**: full corpus + generated-oracle (byte-identical AIR + diagnostics); `scripts/rue spec`,
  `ui`, `cli`; the existing overlay-equals-production tests (`body_overlay.rs:530,572`) stay green.
- **Size**: **L** (broad but purely mechanical; the risk is missed reads — mitigate with a guard
  that no target file reads the epoch tables outside an EpochFacts impl).

#### r1 packaging: landed as three independently gauntlet-proven sub-slices (r1a/r1b/r1c)
r1 preserves every design decision above (same traits, same §1 inventory, same seam, same
byte-identical bar); only its packaging is split, mirroring how the type-syntax family landed one
resolution family at a time. Each sub-slice is a self-contained, byte-identical refactor proven on
the full corpus + oracle before the next begins:
- **r1a** — `BodyEndpointProvider` + concrete `EpochFacts`, covering family **1A**
  (endpoint/definition selection) in `sema/one_body.rs`: the endpoint/nominal/module resolution
  behind `resolve_semantic_type_syntax`'s value-world analog. `build_inference_context` stays eager
  (r5 owns demand-population).
- **r1b** — `SemanticCallResolutionProvider` + `EpochFacts`, covering family **1C**
  (free-function/method/operator/module-member calls) in `calls.rs`/`analysis.rs`/`builtin_ops.rs`/
  `ownership.rs`. This is where the candidate-set-vs-winner selection (R1) moves into generic logic.
- **r1c** — aggregate/field/variant + `visibility.rs` enum-through-module, covering family **1D**,
  reusing r1b's `SemanticCallResolutionProvider`.
The `SignatureFacts` seam and the `call_resolution.rs` generic-logic module named above are
introduced with r1b/r1c; r1a introduces only the endpoint seam (`sema/body_endpoint.rs`).

### r2 — ProviderFacts: type-syntax / nominal
- **Scope**: second impl of `SemanticTypeSyntaxProvider`/`SemanticModulePathProvider` backed by
  BodyFactProvider+overlay; materialize consulted nominals into overlay `type_pool` with identical
  metadata. Behind the differential adapter; old path unchanged.
- **Files**: `typeck.rs` (or new `sema/provider_type_facts.rs`), `body_overlay.rs` (type
  materialization), differential adapter (`revisioned_query_database.rs:18500+`).
- **Tests**: domain differential — resolve every type-syntax shape (primitive, builtin, root/module
  struct/enum/alias, array/slice/ptr, comptime type call) via ProviderFacts vs EpochFacts, assert
  identical `Type` + `type_pool` metadata + observed edges.
- **Size**: **M**.

### r3 — ProviderFacts: callee / method / operator / module-member
- **Scope**: `SemanticCallResolutionProvider` ProviderFacts impl; candidate-set selection stays in
  generic logic; overlay mints callable/method endpoints.
- **Files**: `sema/call_resolution.rs` impl, `body_overlay.rs`, differential adapter.
- **Tests**: differential over unique/absent/ambiguous/visibility-filtered calls, method-vs-assoc-fn
  miscalls (`MethodCalledAsAssocFn`), all 6 operators, module-member calls.
- **Size**: **M**.

### r4 — ProviderFacts: aggregates / patterns / endpoints
- **Scope**: `BodyEndpointProvider` ProviderFacts impl + aggregate/field/variant + enum-through-module
  patterns via the call-resolution ProviderFacts.
- **Files**: `one_body.rs` seam consumers, `aggregates.rs` logic, `visibility.rs`, `body_overlay.rs`.
- **Tests**: differential over struct/enum literals, field access, `mod.Enum::Variant` patterns,
  specialization base selection, `materialize_instance_type` for every `TypeInstanceKey` arm.
- **Size**: **M**.

### r5 — ProviderFacts: InferenceContext demand-population + comptime
- **Scope**: convert `build_inference_context` (`declarations.rs:125`) to a lazily-filled cache;
  constraint generation consults the cache which materializes on miss via the ProviderFacts
  resolvers; comptime reduction over materialized types.
- **Files**: `declarations.rs`, `inference_ctx.rs`, `type_inference.rs`, `comptime_eval.rs`.
- **Tests**: differential over inference-heavy bodies (generic calls, const-typed refs, module-member
  type inference); assert identical inferred types + that only consumed signatures are materialized
  (the flat-in-decls property).
- **Size**: **L** (the substantive B1 item).

### r6 — ProviderFacts: intrinsics + well-known Option (B3) + target config
- **Scope**: overlay-local `WellKnownTypes`; `@target_*` from body-local config.
- **Files**: `intrinsics.rs`, `body_overlay.rs`, port of `binding_manifest.rs:2000+`.
- **Tests**: `?`/fallible-intrinsic corpus warm vs fresh; assert identical AIR + that
  `well_known_option_identities` are EXPORTED as produced anonymous nominals (not leaked as imports).
- **Size**: **M**.

### rFinal — whole-body differential + edge-completeness (original 3d (d)/(e))
- **Scope**: run FULL body analysis through ProviderFacts+overlay (no EpochFacts), diff artifacts +
  diagnostics + recorded edges against production for the entire harness corpus (all shapes: plain
  fns, methods, assoc fns, destructors, specializations, anonymous producers, well-known Option,
  ambiguous/absent/private lookups, multi-diagnostic bodies).
- **Files**: differential adapter, oracle harness.
- **Tests**: whole-body `transaction_equal` across schedule permutations; edge-completeness (no
  epoch namespace/table read remains — capability guard); forced-eviction + cancellation.
- **Carried from r0** (recorded per the r0 acceptance criteria): the full structural locality
  assertion — adding a same-named constant in an unrelated module does not change a body's
  recorded dependency-edge set. r0 could pin it only at the resolution-function level (the
  spooky-action unit test) and the counter level
  (`identity_per_body_lookup_invariant_to_unrelated_declarations`); the recorded-edge-set
  differential needs this slice's provider harness. The E0481 candidate-hint scan must also be
  shown to record no dependency edges (it is error-path diagnostic material, not resolution).
- **Size**: **M**. After this, ProviderFacts is proven equal for every corpus shape and the flip is
  a pure delete.

---

## Review carry-forwards (from r1a/r1b review; ratified while landing r2)

Recorded here so a later slice does not silently re-derive or drop them:

1. **Interner reads stay inline for now.** The ~12 `interner` reads in `one_body.rs` are NOT hoisted
   behind `BodyEndpointProvider`/EpochFacts yet; they remain inline pending an explicit decision
   before the "no epoch read outside EpochFacts" guard (§5 #4) lands. The guard must not fail on
   them until that decision is made.
2. **The guard must whitelist the outer-driver universe enumerations.** When the "no epoch read
   outside EpochFacts" guard lands, it must explicitly allow the whole-universe enumerations that
   belong to the outer driver, not to per-body analysis: `intern_named_callable_symbols`, the
   unused-function warning scan, the anonymous-destructor enqueue, and the universe-cardinality
   counters. These are driver-level passes, not body reads, and are out of scope for the
   provider-driven analyzer.
3. **rFinal differential enumerates variants explicitly.** The rFinal whole-body differential must
   enumerate every `TypeInstanceKey` arm and every `CanonicalArgumentValue` variant explicitly
   (module-typed comptime values, function-valued comptime args, anonymous nominals,
   destructor-bearing bodies included) — never rely on incidental corpus coverage, or an arm can be
   vacuously green.

**ProviderFacts landing order.** The ProviderFacts slices (r2..r6) land in the plan's own
**dependency order (r2→r6)**, NOT the r1a/r1b/r1c seam landing order. The endpoint-seam ProviderFacts
is **r4**, and it consumes r2's type materialization and r5's `SignatureFacts`
demand-population: `resolve_instance_type` and the `function_info`/`method_info` endpoint answers are
expressed in materialized-identity terms (`StructId`/`Type`/`FunctionInfo`), which the
`BodyEndpointProvider` seam cannot obtain from `BodyFactProvider` + overlay until r2/r5 exist. r2 is
therefore the type-syntax/nominal ProviderFacts (this slice), not the endpoint seam.
Scope honesty for r4's sizing: r2 supplies materialized durable nominal METADATA keyed by stable
identity (`materialized_nominals`), deliberately pool-free — it does NOT build an overlay-owned
id-minting `type_pool`. r4 still owns the full cost of the pool that mints epoch-compatible
`StructId`/`Type`/`FunctionInfo` identities on top of r2's metadata and r5's signatures.

---

## Rider: r3→r4 fold, r4 sub-slice map, and the pool keystone (recorded landing r4a-1)

**(a) The r3→r4 fold.** r3 (endpoint-seam call/method/operator/module-member ProviderFacts) and r4
(aggregates/patterns/endpoints ProviderFacts) are folded into one **r4** effort. Rationale: r1b's
`SemanticCallResolutionProvider` seam is **epoch-concrete**, not provider-generic like the
type-syntax seam — it drives the non-generic value/endpoint analyzer whose answers are expressed in
materialized-identity terms (`StructId`/`Type`/`FunctionInfo`). A call ProviderFacts has no
pool-free generic domain to resolve into the way `resolve_semantic_type_syntax` resolves into
`DurableType`: the call facts are inseparable from the id-minting pool that r2's pool-free metadata
deliberately withholds. Splitting "call resolution" from "endpoint/pool assembly" would draw a seam
through the middle of one indivisible act, so r3 and r4 land together as r4.

**(b) r4 sub-slice map.** r4 lands as byte-identical re-points (each proven on corpus + oracle
before the next), NOT one envelope:
- **r4a-1** (this slice): the `callable_symbol_method` boundary op — the keyed reverse of the epoch's
  `named_method_by_callable_symbol`, the sole missing symbol→method surface. Ships independent of the
  pool.
- **r4a-2a**: nominal / type-identity pool — the overlay-owned id-minting `type_pool` r2 withheld,
  minting `StructId`/`Type` from r2's durable metadata.
- **r4a-2b**: callable identities / `ParamRange` assembly (`FunctionInfo`/`MethodInfo`-equivalent)
  on top of r5's signatures.
- **r4a-2c**: the RIR-index answers (`first_free_function`/`destructors`/`named_method_declarations`
  → body key) the endpoint seam consumes.
- **r4b-1/2/3**: the ProviderFacts impls per family (calls, aggregates/patterns, endpoints) driven by
  r4a's pool, each a differential re-point of one family.

**(c) The pool keystone.** Published artifacts are durable-keyed **at export**
(`semantic_body_export.rs` is the single funnel), so the r4a pool may mint **internally-consistent**
ids with correct durable metadata — it need NOT reproduce epoch `StructId`/`Type` NUMBERING. Byte
identity is a property of the exported durable keys, not of the transient pool indices, so the
differential compares durable structure and materialized metadata (as r2 already does), never a
pool-relative index. r4a-1 is the first concrete instance: its answer is a durable-keyed
`(ReceiverType, method)` — a `ReceiverTypeIdentity` (module + type name + category) plus the owned
method name — never a `StructId`, exactly because the pool that would mint one is r4a-2a's job.

**(d) Builtin / slice name facts deferred to r6.** r4a-1's reverse covers **user named methods**: a
file-qualified symbol `Type$file.method` / `Type$file::method` whose defining module is recoverable
from the mangled component. A **bare** symbol (builtin / language-item / anonymous owner — no `$`)
carries no recoverable module and is returned `None`, deferred to r6 with the well-known `Option`
facts (the same slice that ports `install_well_known_option_types`). This matches the epoch: for an
import-free, string-free corpus the epoch's `named_callable_methods_by_symbol` contains exactly the
user methods, so the keyed reversal is complete over its differential scope.

**(e) Open Steve-level question (does not block r4a-1).** Whether import reversal is ultimately
**retired** in favor of recording the `(receiver, method)` dependency **at resolution** (so no
symbol ever needs reversing), or **kept** as a keyed op, is an open design question. r4a-1 ships the
keyed op **either way**: it is the honest provider analog of the epoch accessor the current consumer
(`classify_static_call`) needs, and it costs nothing if the record-at-resolution direction later
subsumes it — the op simply loses its caller, exactly like an EpochFacts impl at the flip.

**Rendering-parity note (source-verified while landing r4a-1).** The op reproduces
`TypeInternPool::struct_symbol_name` exactly, which under ADR-0066 / RUE-1089 is now **unconditional**
file-qualification for user nominals (`{name}${mangle(normalize(module_path))}`), not the older
RUE-571 "qualify only on ambiguity" the §1 tables paraphrase — the exemptions (builtin / language
item / `__anon_*`) keep their bare names. The reversal recovers the module by inverting the mangling
(`unmangle_symbol_component`, the exact inverse now paired with `mangle_symbol_component`, both public
in rue-air) and **re-renders forward** to require a byte-exact match before answering, so a symbol the
epoch could never have produced fails closed. Because the boundary is keyed and exposes no
program-wide method enumeration, the op is realized as a per-symbol keyed reversal (recover receiver
from the symbol → confirm the nominal and member via the existing `lookup_unqualified` /
`method_candidates` point queries, recording their edges) rather than a materialized whole-program
index; that is the boundary-honest form of "a reverse index built from the durable declaration set,"
and it fails closed on absent, ambiguous-receiver, wrong-`self`-form, and non-matching-render inputs.

**r4a-1 review carry-forwards (recorded obligations, not defects).**
- **Bare-owner reversal divergence → r4b differential xfail.** The epoch's callable index contains
  BARE symbols for builtin / lang-item / anonymous owners (`struct_symbol_name` exempts them from
  file-qualification), and imported bodies calling e.g. `StrBuf` methods reverse through them; the
  r4a-1 op refuses bare symbols by design pending r6's well-known/builtin-name facts. r4b's
  differential MUST record this class as an explicit known-divergence (xfail) tied to the r6 port,
  and pin at least one case where the epoch genuinely answers a bare symbol the op refuses — the
  current tests only exercise bare symbols the epoch also refuses, which masks the gap.
- **Provider edges are the post-flip truth.** The epoch's index lookup records no body edges; the
  op records the lookup-name and semantic-nucleus edges it genuinely consults. rFinal's
  edge-differential must treat the RICHER provider-era edge set as the new truth after the flip
  (the finer dependencies are the more-correct incremental behavior), not force the op to suppress
  edges to match the epoch's index-masked footprint.

---

## 4. RISKS (top 5 for this rewire)

| # | Risk | Mitigation |
|---|---|---|
| R1 | **Internal-trait granularity mismatch — candidate-set vs winner.** `BodyFactProvider` returns candidate SETS (`NameResolution::Ambiguous`, `method_candidates` spanning method+assoc-fn); today's inline code often reads a pre-selected winner (`functions.get(name)`, `method_info(key)`). Hoisting must move SELECTION (kind filter, visibility filter, ambiguity→diagnostic) OUT of the impl into the provider-generic logic, or ProviderFacts and EpochFacts diverge on ambiguous/private cases. | Put ALL selection in the generic logic using `NameResolution.of_kind/visible` + `MemberKind`; EpochFacts returns the same candidate set the provider would (not the winner). Prove parity in r1 (EpochFacts selection == old inline selection) BEFORE any ProviderFacts. Differential over ambiguous/private/kind-mismatch corpus in r3/r4. |
| R2 | **`type_pool` metadata byte-identity.** Diagnostics render from `type_pool` struct/enum metadata (name, is_pub, file_id, is_copy) and `ctor_type_displays`. If ProviderFacts materializes a nominal with even slightly different metadata (name spelling, is_copy), diagnostic TEXT diverges though no edge is missing. | Make overlay type materialization a direct port of the epoch install's metadata construction; assert `type_pool` metadata equality per materialized type in r2/r4 differential; extend the warm/fresh oracle to multi-diagnostic and `format_type_name`-exercising bodies. |
| R3 | **Borrow-checker fights threading the resolver through deep `&mut self` call stacks.** The type-syntax provider already borrows `&mut Sema` (it mutates `type_pool`, observers). Adding call/endpoint resolvers that also need `&mut` overlay while the analyzer holds `&mut sema` risks aliasing conflicts across the expression/statement recursion. | Follow the existing `SemaTypeSyntaxProvider` pattern (construct provider borrowing `&mut sema` for the duration of one resolution, flush observations, drop) rather than holding a long-lived `&dyn`. Keep the resolver a short-lived per-call borrow; materialization writes go through the overlay handle, not back into `sema`. Prototype the borrow shape in r1 for one call site before the broad move. |
| R4 | **InferenceContext demand-population changes inference order / diagnostics.** Eager `build_inference_context` makes every signature available before constraint generation; lazy population materializes on first consult. If constraint generation's behavior depends on a signature being present before it is first named (e.g. a whole-map scan, or ambiguity resolved by iteration), lazy fill diverges. | Audit `type_inference.rs` for any WHOLE-map iteration of `func_sigs`/`method_sigs` (point lookups are safe; scans are not); the current maps are consulted by key. Keep the `InferenceContext` public shape and fill-on-miss so constraint generation is textually unchanged. Differential over inference-heavy + generic bodies in r5; assert identical inferred types AND identical diagnostic order. |
| R5 | **Dynamic-dispatch / per-fact resolution regresses the hot path.** Eager whole-universe build is one O(U) pass; on-demand is many small point queries through `&dyn` + overlay materialization — could regress the warm pre-link budget on resolution-dense bodies. | The recipe cache (`recipe_cache.rs`) makes each declaration fact O(1) after first build; name/method lookups are O(1) keyed. Prefer generics/monomorphized impls over `&dyn` where the borrow allows (EpochFacts is a concrete type). Measure the Caldera budget + `cold_micros` baseline in SEPARATE runs (never with the counting allocator) at rFinal; the point of RUE-1091 is that per-body work is O(consumed facts) not O(U), so total cold work should DROP, not rise. |

---

## 5. FLIP DELTA — what remains after rFinal

After r1..rFinal, ProviderFacts is proven byte-identical for every corpus shape behind the
differential adapter, and production still runs EpochFacts (unchanged, byte-identical to trunk). The
flip slice reduces to the honest one-slice delete the envelope rule requires:

1. **Un-gate** `CompilerBodyFactProvider` + its `BodyFactProvider` impl, `lookup_imports`,
   `ObservedLookupRoot::record`, and drop `#![allow(dead_code)]` on `body_overlay.rs`/`recipe_cache.rs`
   (flip-plan §2).
2. **Re-point** `body_transaction`'s compute closure: construct provider + overlay per body, run the
   provider-generic analyzer driven by **ProviderFacts** instead of EpochFacts, publish through the
   overlay resolver (`BodySemanticOverlay: BodyOutcomeResolver`, already implemented). Re-source the
   four per-body counters to overlay operations (flip-plan §3).
3. **Delete** the epoch prefix in `analyze_body_query` (`prepare_/project_/install_*`,
   `issue_bound_definitions`, `install_body_owner_tokens`, `install_stable_identity_endpoints`,
   well-known-registry epoch install), `BoundOutcomeResolver`, the `analyze_body_via_overlay` test
   driver, the eager `build_inference_context` whole-universe projection, **and the EpochFacts impls
   themselves** (their only production caller is gone; ProviderFacts is now THE impl). Delete
   `module_declaration_sets` loop + `accepted_import_topology_input` + `declaration_modules` param;
   rewire the promotion hook to `take_observed_root()` (flip-plan §1, §3).
4. **Guard**: `body_analysis_has_no_whole_program_context_path` (flip-plan §5) — extended to assert
   the analyzer reaches `BodyFactProvider`/overlay and NOT the epoch namespace tables, in BOTH
   `canonical_semantic.rs` and rue-air (`one_body.rs`, `call_resolution.rs`, `typeck.rs`): no
   `structs_by_file_name`/`functions`/`method_info`/`build_inference_context`(eager)/`&Sema` reach.
5. **Matrices**: RUE-1090 both axes (expect CANCEL / all gated counters flat), RUE-1121 exact-flat +
   invalidation rows (expect PASS), recompute the growing-bodies-axis targets, Caldera budget.

The rewire has moved every substantive act (provider-driven analyzer, demand inference, overlay
materialization, B3) OUT of the flip and behind the differential adapter. What is left is un-gate +
re-point + delete-the-old-path + guard + matrices — a single reviewable envelope in which every
tracked row moves witness→target and no second selectable production path ever ships. The EpochFacts
family is the crucial trick: because r1 made it THE production impl (not a parallel path), the flip
deletes ONE impl family and points production at the other — an honest delete, not a fork.

---

## VERDICT

**Internal traits as the boundary: PARTIALLY HOLDS.**

- The trait PATTERN is correct and already proven for the entire type-syntax + module-path family
  (`SemanticTypeSyntaxProvider`/`SemanticModulePathProvider`): logic is provider-generic, one impl
  reads sema, a second impl (ProviderFacts) drops in. That is exactly the rewire shape the
  hypothesis wants.
- But only **1 of ~11 resolution families is behind an internal trait today.** The other ~10 —
  endpoint/definition selection, free-function calls, method calls, operator overloads,
  module-member calls, aggregate/field/variant resolution, pattern enum-through-module, intrinsic /
  well-known Option, and the whole-universe InferenceContext snapshot — read the epoch tables
  DIRECTLY and inline. **~130 direct read sites need hoisting behind new internal traits.**
- Every one of those reads maps to an EXISTING `BodyFactProvider` op (no missing provider surface),
  and all hoists are bounded/mechanical (S/M) EXCEPT the InferenceContext demand-population, which is
  the one genuinely L sub-effort (the true residue of B1). So the hypothesis's "typeck barely
  changes" is true for TYPES but not for the value/call/inference world, which needs the same
  trait-extraction treatment first.
- Net: the rewire = (r1) hoist all direct reads behind traits + extract EpochFacts as the production
  impl [byte-identical refactor] → (r2..r6) ProviderFacts per domain behind the differential adapter
  → (rFinal) whole-body differential. The flip then becomes the honest one-slice delete. The
  boundary holds as a design; it does not yet EXIST for most of the surface, and building it is the
  work.

**Count of direct reads needing hoists: ~130 sites across ~10 resolution families (1 family already
behind a trait; 1 of the 10 — InferenceContext — is L, the rest S/M).**

## Appendix: inference-context de-risk spike findings

A bounded throwaway prototype (nothing shipped) probed the plan's one Large item —
demand-populating `build_inference_context` — before slice scheduling. Findings:

- **Eleven of thirteen context families are consumed purely by key** and are
  order-insensitive: per-table lazy cells are sufficient, with two bounded costs the
  per-family estimates must include — the fill source threads to the consumer (the
  detached context cannot borrow `Sema`), and cached values return owned rather than
  borrowed across `&mut self` recursion.
- **Prototype proof (function-signature family)**: eager universe conversion deleted,
  fill-on-miss at the consumer; rue-air 568, rue-compiler 790, scaling 793, spec 2178,
  UI 204 all green and byte-identical; per-body consumed signatures flat at 0.91 while
  the declaration universe grew 8x — the O(U) to O(consumed) collapse, observed.
- **The exception the op inventory missed**: `const_values` and `const_type_aliases`
  each carry a program-wide bare-name uniqueness scan
  (`crates/rue-air/src/inference/generate.rs:715,730`, RUE-638 semantics: a bare name
  resolves only if exactly one constant matches program-wide). The predicate is
  incompatible with per-key demand population — a partially-filled map answers it
  wrongly, and small-corpus differentials cannot detect the divergence. This falsifies
  this plan's §1E claim that every read maps to an existing provider op.

### Resolution: bare const names follow ordinary scoped resolution

The maintainer ruling: the global bare-name uniqueness fallback is retired. Bare
names in array-length and type-alias-head positions resolve through the same
scoped resolution as every other name — same-module constants and imported
constants, with visibility applied — and a name that is not in scope is an
ordinary resolution error whose diagnostic suggests qualification or an import.
The whole-program uniqueness predicate predates the module system and
contradicts the language's scoping model: an unrelated constant added in a
distant module could flip resolution from unique to ambiguous and break code
that never referenced it. Scope-respecting resolution restores locality for
readers and makes the two const families ordinary keyed lookups, so §1E's op
inventory holds again with no new provider op: `const_comptime` and the lookup
families cover every consult.

### Slice r0 — retire the global-uniqueness fallback (production semantic change)

This is a language-behavior change and therefore cannot ride an inert slice; it
lands first, alone, with the full semantics-change discipline:

- Replace `unique_const_value` / `unique_const_type_alias`
  (`crates/rue-air/src/inference/generate.rs:715,730`) and their call sites with
  scoped resolution through the same module/import/visibility rules the
  surrounding resolution already uses; delete the whole-map scans.
- Amend the specification section governing array-length and alias-head name
  resolution to state the scoped rule; no new rule identifiers are expected —
  the positions inherit the general resolution rules.
- Corpus impact pass: compile std, examples, and the full spec/CLI corpora with
  the fallback patched to error loudly, enumerate every reliant case, and
  migrate each (qualification or import). Cases asserting the old ambiguity
  failure mode flip to the scoped diagnostic.
- Diagnostic quality: the not-in-scope error names the candidates that exist in
  other modules to guide the import. Discipline (review caution, adopted): the
  candidate set is computed when the diagnostic is materialized during
  analysis — never during rendering — with deterministic ordering, and it is
  error-path-only. An exhaustive cross-module candidate search is a global
  reverse-name dependency for failed lookups; that is tolerable pre-flip
  (bounded to erroring bodies under the eager epoch), but at the provider flip
  the hint must be served by a keyed (name, kind) suggestion index whose edge
  is recorded at diagnostic construction, or degrade to a generic suggestion.
  An erroring body may not retain a whole-universe dependency past the flip;
  rFinal's edge-completeness check covers this case explicitly.

Design review (2026-07-23) confirmed the direction with no blockers and
strengthened the acceptance criteria below; r0 is not done until all of them
hold.

**Scoped-resolution roots (the semantic contract, per position).** These are
the exact scopes the implementation resolves against, written down before code
moves:

- Array-length name in a function signature: the declaring file/module of that
  function.
- Array-length name inside a body: the body's file/module plus lexical comptime
  parameters and local comptime aliases. The existing precedence — comptime
  value parameters win over file-level constants
  (`resolve_infer_array_length_with_values`) — is preserved unchanged and
  covered by a test.
- Alias-head position: the alias declaration's file/module.

**Negative locality tests (required, beyond corpus migration).** The point of
r0 is locality, so the proof is not "old tests updated" but "unrelated
declarations no longer participate":

- An unreferenced sibling module declaring a same-named `pub const` does not
  affect bare resolution in another file — the local constant wins, no
  ambiguity, and a module never imported by the body cannot change the result
  at all (the spooky-action regression).
- `const m = @import(...)` alone does not put the imported module's members
  into bare scope; they are reached as `m.N` per existing member-access rules.
- The qualified path (`m.N`) in array-length/alias-head positions is pinned by
  a test — as working if the grammar accepts it there, as a diagnostic if not.
- A private (non-`pub`) constant in an imported module is not visible bare —
  visibility applies and has its own regression case.

**Value-vs-type discipline.** `const_values` and `const_type_aliases` stay
distinct consumers: array lengths demand a comptime integer value, alias heads
demand a comptime type. The scoped replacement resolves a declaration by
scoped name and validates value-vs-type at the consumer boundary — no fuzzy
"const named X" abstraction, and a wrong-kind resolution is a clear diagnostic,
never a silent miss that falls back elsewhere.

**Spec locality sentence.** The amended spec prose states explicitly, in
addition to inheriting the general resolution rules: a bare name in these
positions is resolved in the declaration/body scope; declarations outside that
scope do not participate merely because they are globally unique. This exists
to stop a future "helpful" reintroduction of the uniqueness fallback.

**Structural locality assertion.** If expressible before the provider rewrite:
adding a same-named constant in an unrelated module does not change a body's
resolved dependency set. If the pre-provider harness cannot express it, r0
records that explicitly and the case is carried as an rFinal acceptance
criterion (where the provider counters can also assert the const universe is
not materialized wholesale).

After r0, the inference-context work in r5 needs only the eleven keyed families
plus the two const families as ordinary keyed lazy cells — no global index, no
new op, no per-body universe residue inside the gated term.
