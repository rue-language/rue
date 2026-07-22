# RUE-1089 code-side site inventory (prep survey at checkout f0545a8f)

For cross-check against ADR-0066's conversion inventory. Every reference verified in-checkout; behavior claims captured executably in the companion current-behavior artifact.

## A. Cross-producer structural equality (the assignability engine) — crates/rue-air/src/sema/anon_structs.rs

- `find_or_create_anon_struct` (331–453): structural match search + `min_by anonymous_key_cmp` stable-min representative + alias install (`canonical_anonymous_aliases`, `canonical_anonymous_types`); the `existing`-branch representative swap (353–381) also retracts prior methods. **Delete the structural search + representative/alias logic; keep one producer key → one entity.**
- `find_or_create_anon_enum` (469–572): same pattern for enums. The pool does **not** structurally dedup enums — the synthetic name uses a unique `__anon_enum_{len}` counter (525) deliberately defeating name-interning; structural equality lives entirely in the `existing` filter (479–520). **Delete.**
- `find_compatible_anon_enum` (578–603): lookup-only structural min-representative search. **Delete/rework.**
- `types_equivalent` / `types_equivalent_inner` / `anonymous_structs_structurally_equal` / `anonymous_struct_matches` / `method_signatures_equivalent` / `method_types_equivalent` (611–782): the recursive structural relation for anonymous nominals. **Anonymous arms collapse to exact `AnonymousNominalKey` identity comparison** (named/array/ptr recursion stays).
- `anonymous_key_cmp` (50–85): stable-min ordering for representative selection. **Delete.**
- `types_compatible` (784–786) and its ~25 callers (typeck/aggregates/calls/control_flow/pointers/ownership/intrinsics) remain but now compare exact identity.

Identity keys are **already producer-nominal** — crates/rue-air/src/semantic_identity.rs: `AnonymousNominalKey{ kind, producer, anchor, arguments }` (73–79). The structural layer sits *above* them in `AnonymousNominalIdentitySet{ representative, aliases }` (85–89). **`AnonymousNominalIdentitySet` (representative/aliases) is the core deletion.**

Sema state fields to remove/simplify — crates/rue-air/src/sema/mod.rs: `canonical_anonymous_aliases` (307), the representative-map semantics of `canonical_anonymous_types` (304), `anon_struct_method_sigs` (420), `anon_struct_captured_values` (426) (only needed for structural matching).

## B. Representative machinery / restart / scheduling

- crates/rue-air/src/sema/analysis.rs: `anonymous_representative_changed` (1705–1716) and its restart `continue 'body_attempt` (3137–3139). **Delete restart; body attempts no longer invalidate on representative change.**
- crates/rue-compiler/src/session.rs: `canonicalize_reached_anonymous_member` (1316–1369), `stable_anonymous_nominal_cmp` (1371–1378), `produced_anonymous_survives_closure_restart` (1414–1423); in the `'closure` loop: `stable_produced_seed` (5197, 5343, 5710–5734), `representative_changed` (5345, 5685–5749, 6019–6022 `continue 'closure`). **Delete canonicalization-to-representative and the restart; producer-dependency scheduling (`deferred_producers`/`priority_pending`/`deferred_dependency_chain`, 5363–5411) REMAINS per ADR-0066 §2, as does the specialization-depth budget.**
- Downstream alias fabrication: crates/rue-compiler/src/revisioned_query_database.rs `materialize_contextual_anonymous_aliases` (1736–1768) + `projected_anonymous_nominal_for_identity` (1710–1734), called at session.rs:6052 — contextual-anchor aliasing; audit/simplify (fail-closed relative-occurrence aliasing may still be needed for anchor-prefix projection; the structural-equivalence rationale goes away).
- Sema tests to delete/rewrite (crates/rue-air/src/sema/tests.rs): `structurally_equal_anonymous_types_retain_all_producer_aliases` (3054), `body_types_export_the_stable_minimum_anonymous_representative` (3080), `late_anonymous_representative_retracts_stale_method_errors` (3117), `late_anonymous_representative_recomputes_outbound_reachability` (3149), `representative_restart_restores_consumed_body_candidates` (3388), `anonymous_structural_equivalence_is_recursive_at_semantic_choke_points` (2990), `producer_distinct_anonymous_methods_destructors_and_nested_payloads_are_compatible` (3022), plus the `anonymous_method_*_participates_in_structural_identity` trio (2891/2922/2955) and `inference_does_not_compare_every_pair_of_anonymous_types` (3725). Retain/repurpose (producer-local reachability, still valid): `abandoned_anonymous_method_cannot_root_its_nested_destructor` (3211), `final_anonymous_method_nested_destructor_remains_rooted` (3276), `independently_reachable_abandoned_nested_type_keeps_its_destructor` (3331).

## C. Symbol naming — collide-then-qualify → always-qualify

- crates/rue-air/src/intern_pool.rs: `nominal_name_collides` (1333–1341), `struct_symbol_name` (1343–1356), `enum_symbol_name` (1358–1371); public wrappers at 2307/2315 and 2792/2796. **Replace collide-gate with unconditional `{name}${file}` qualification.**
- Consumers: crates/rue-air/src/drop_glue_names.rs `struct_drop_glue_name`/`enum_drop_glue_name`/`type_name` (17–80); crates/rue-compiler/src/drop_glue.rs (135, 354, 443); rue-codegen/src/value_plan.rs (890, 919); rue-air/src/sema/typeck.rs:1799; rue-compiler/src/pipeline_tests.rs (1772, 1777).
- **Test expectations that flip:** intern_pool.rs `test_struct_symbol_name_qualifies_only_colliding_names` (4055; asserts `Q`→`Q`, `StrBufTest`→`StrBufTest` at 4082/4085) and the clone test (4127–4139); drop_glue_names.rs `struct_and_enum_glue_names` (unqualified `__rue_drop_Container`, 157–164) and `array_and_nested_array_glue_names` (184–191); durable_semantics.rs:1549 (`StrBuf` unqualified). CLI/UI golden symbol output: rue-cli-tests/cases/modules.toml and rue-ui-tests/cases/diagnostics/droppable_gate.toml contain `$`/`__rue_drop` symbols — audit.

## D. Annotation-position anonymous types (forbid per staged rule)

- Parser accepts them today: crates/rue-parser/src/parser/types.rs `ty()` at 55 (`struct {…}` → `anonymous_struct_type(false)`, def 163–212) and 56–63 (`enum {…}` → `TypeExpr::AnonymousEnum`); nesting via array-element/pointer-pointee recursion. `anonymous_struct_type` already rejects methods in type position (173/180).
- RIR/sema currently interns a synthetic name then fails to resolve: crates/rue-rir/src/astgen.rs 244–287, 1249/1282/1338. Verified end-to-end: `let p: struct { x: i32, y: i32 } = …` → E0100; `[struct { x: i32 }; 2]` → E0204 unknown type. **RUE-1089 must replace the incidental E0204/E0100 with a targeted syntactic rejection.**

## E. Corpus to migrate

- crates/rue-spec/cases/expressions/comptime.toml: flips-to-error — `anon_struct_structural_equality` (497), `anon_struct_structural_equality_with_methods` (1016), `anon_struct_method_order_is_not_structural` (1045), `anon_struct_nested_method_signature_types_are_structural` (1170). Delete/replace (representative-dependent) — `late_specialization_replaces_anonymous_method_with_stable_representative` (1074), `late_specialization_retracts_stale_anonymous_method_error` (1105), `late_specialization_does_not_root_abandoned_nested_destructor` (1136). Rename to same-producer — `anon_enum_structural_reuse` (2120), `instantiation_identity_is_structural_across_functions` (2675). Reword (keep negative) — `anon_enum_monomorphized_distinct` (2102), `instantiation_distinct_arguments_are_distinct_types` (2690). Keep + add cross-producer mismatch cases — `anon_struct_different_fields_different_types` (519), `anon_struct_different_field_types` (540). Add new: forwarding `Id` success; direct+nested annotation rejection; different-producer same-shape struct/enum mismatch.
- crates/rue-spec/cases/types/destructors.toml: delete/replace `structurally_equal_anon_destructor_uses_stable_representative` (1198; expects `@dbg` `7\n` not `107\n`) and `late_specialization_replaces_anonymous_destructor_with_stable_representative` (1225).
- CLI crates/rue-cli-tests/cases/lazy_specialization_references.toml: **`alternating_specialization_method_recursion_is_bounded` (88–113) FLIPS** — structural representative-convergence currently *bounds* a genuinely infinite `runaway(n)→Wrapper(n).go()→runaway(n+1)` recursion; producer-nominal makes each `Wrapper(n)` distinct → unbounded → specialization-depth error. Delete or convert to a depth-limit negative case. `specialization_registers_anonymous_destructor` (63) stays valid.
- CLI crates/rue-cli-tests/cases/aggregate_equality.toml is value `==`/`!=` (RUE-285), not type identity — unaffected; do not migrate.

## F. Downstream keyed by the equivalence machinery

- Durable body export uses the representative: `canonicalize_reached_anonymous_member` rewrites reached `AnonymousMember` keys before durable body/CFG/codegen consume them (session.rs closure → `durable_body_candidates`, 6023–6025). durable_body.rs `validate_anonymous_identity` (308) and `collect_anonymous_definition_keys` (487) are keyed on `AnonymousNominalKey` and stay, but no longer receive representative-rewritten keys. `SemanticImportType::AnonymousNominal` exports (durable_body.rs 258–276) currently carry representative identities — after the cut they carry the exact producer identity. Codegen symbol emission and drop-glue synthesis must agree with the always-qualified names.

---

## RUE-1089 progress dispositions (implementation session)

- **Stage 1 (AIR identity core)**: DONE, verified. Cross-producer structural collapse removed (anon_structs.rs). RETAINED anon_struct_method_sigs/captured_values/type_subst (durable-export load-bearing). anonymous_key_cmp kept (same-producer anchor ordering).
- **Stage 2 (representative machinery)**: DONE. After the anchor-transport fix (below), every candidate reconciliation path was instrumented and recorded ZERO reaches across the full unit+spec+cli corpus; `canonicalize_reached_anonymous_member` was already pure identity (representative-changed provably always false). DELETED: `materialize_contextual_anonymous_aliases`, `projected_anonymous_nominal_for_identity` (callers reduced to exact-identity lookup), the session.rs `'closure` restart + `representative_changed` + `stable_produced_seed` + `canonicalize_reached_anonymous_member` + `produced_anonymous_survives_closure_restart`, the sema/analysis.rs `'body_attempt` restart + `anonymous_representative_changed`, and the binding_manifest.rs same-producer/different-anchor alias install + relative-occurrence method-materialization fallback.
- **Stage 3 (Option mint)**: NOT done — STOPPED after genuine effort (see below). `find_compatible_anon_enum` KEPT (documented deviation, tracked for RUE-1112). The existing `canonical_builtin_nominal` mechanism cannot safely express a parameterized `Option(payload)`: a `BuiltinNominal` durable type carries only an opaque `name: Arc<str>`, not a structured payload TypeInstanceKey, so the payload identity cannot survive durable round-trip without a lossy name-reparse (collisions on same-display-name types; generic payloads unrecoverable). Additionally `EnumDef` has no `is_builtin` field, the enum classification in `canonical_type_instance` keys on the STATIC `BUILTIN_ENUMS` list (not a flag), and `builtin_nominal_kind` is a fixed non-parameterized name registry — so routing Option through it would require building new parameterized-builtin-enum machinery + a durable re-mint path, rebuilding the just-hardened durable round-trip. That crosses from "use the existing mechanism" into "improvise a third mechanism", which the brief forbids. `find_compatible_anon_enum` remains correct in practice (one canonical Option per payload in scope; try spec cases green) and is an architectural-purity deviation, not a correctness bug.
- **Stage 4 / Stage A (anon symbol stability)**: DONE, verified. The `__anon_struct_<id>`/`__anon_enum_<id>` allocation-order suffix is replaced by a STABLE 128-bit FNV-1a digest of the producer-nominal `AnonymousNominalKey` (tokens relocated to request-independent endpoint content first). Two cold compiles, warm/fresh, and method-reorder all emit identical anon symbols; distinct producers get distinct symbols. Acceptance corpus extended to assert anon symbols specifically. `.__drop` glue stays in sync (Box-destructor prints 100 then 7). air 565/565, compiler 684/684.
- **Stage 5 (annotation rejection)**: DONE, verified (E0102, span-accurate).
- **Stage 6 (spec + corpus)**: DONE. spec 14-comptime.md amended (4.14:8/15/21/22/25 + new 23a). comptime.toml/destructors.toml/lazy_specialization migrated. Scenario 6 added to notes.
- **Stage 7 / Stage C (downstream audit + std examples)**: DONE. §F durable export (`validate_anonymous_identity`, `collect_anonymous_definition_keys`, `SemanticImportType::AnonymousNominal`) verified to operate on exact producer identities with no representative-era assumptions; the `AnonymousNominalIdentitySet{representative,aliases}` projection survives as a now-DEGENERATE singleton (`aliases == [representative]`, assert holds) — correct, not stale, full removal deferred as a larger refactor. Stale restart/representative-change comments deleted in session.rs and analysis.rs. std examples run end-to-end with the worktree compiler (see acceptance ledger for timings/exit codes).
- **Stage D (transport corruption → fatal, reviewer ruling)**: DONE, verified. The four anchored-transport corruption modes (missing/duplicate/kind/divergent) now all fail closed with a typed E9000 and zero published semantic output; the `body_produced_anonymous` downgrade of a committed internal error to `Canceled` (which let AIR rescue by recomputing identity from RIR) is removed. Genuine unavailability still retries; unused table entries stay legal. Fault tests flipped.

### Scratch-file readiness
Every inventory item (§A–§F) is now dispositioned to a final state. The single
open thread is Stage 3 (the `?`-intrinsic Option mint), which is intentionally
deferred to **RUE-1112** with `find_compatible_anon_enum` retained as a tracked,
correct-in-practice deviation. Once RUE-1112 is filed, this scratch file is ready
for deletion at integration (do not delete before then — it carries the Stage 3
stop-reason and the §F degenerate-singleton note).

### ANCHOR-FIX RESOLVED (anchor-transport, RUE-1089)
The independent traversal-order mint (`[Body, AnonymousType(next++)]`) is DELETED.
The frontend/module lowering is now the single mint: a shared astgen-mirroring
walk (`rue_rir::anonymous_type_sites`) yields the exact `AstGen` anchor for every
value-position anonymous type literal, validated lock-step against real astgen
RIR by a bijection test. At declaration-index construction — where module and
fragment coordinate systems are both known — each site's module span + anchor is
recorded; the raw const/body terminals slice them into fragment-relative
locators; the reparse shifts them into the fragment's synthetic-source space; and
`SemanticConstEvaluator::eval_type_literal` performs an EXACT locator lookup,
copying the anchor into the nominal identity. No span→RIR lookup from fragment
space; no path re-derivation in fragment space; no heuristic fallback. A missing/
duplicate/kind/collision disagreement fails closed with a typed E9000-class
diagnostic before any terminal or alias publishes. Blocker test green; Wrap → 42.

### (historical) CRITICAL ANCHOR-FIX FINDING (redirects codex's span-based recipe)
The durable declaration/const evaluator (`semantic_query_nucleus::parse_semantic_const` / `parse_semantic_body`) REPARSES each declaration fragment as an ISOLATED synthetic source (`const NAME = <init>;` / `fn __semantic_body() { <body> }`) with a fresh `FileId(0)`. Its span space is therefore DISJOINT from the module RIR's file-relative spans (verified: const-eval span `FileId(0) start:23` vs RIR span `FileId(1) start:38` for the same `enum {…}`).
=> Consuming the frontend/RIR anchor BY SPAN is impossible. A span-lookup implementation fixed the Wrap repro (→42) but regressed spec comptime 202→190 (fail-closed ICE on nested-generic cases whose fragment spans miss the RIR map) and was reverted.
The independent-minting site is `revisioned_query_database.rs SemanticConstEvaluator::eval_type_literal` (mints `[Body, AnonymousType(next_anonymous_type++)]`, a traversal-order approximation). astgen always uses occurrence `AnonymousType(0)`; uniqueness comes from the full structural PATH.
Correct fix options: (a) thread a structural path through the fragment evaluator's traversal, minting `path + AnonymousType(0)` to MATCH astgen exactly (span-independent, but fragile and not cross-checkable against the RIR in the fragment span space — miscompile risk on path divergence); or (b) re-architect so durable declaration semantics carry the frontend anchors rather than re-deriving them from a fragment reparse. Both are larger than a localized edit and need full warm/fresh/schedule-permuted parity verification.
