# RUE-1089 producer-nominal acceptance ledger

> Archived pre-cutover acceptance record. Current source and tests are
> authoritative; paths and “Today” status descriptions below refer to the
> historical implementation.

Executable acceptance corpus for the producer-nominal anonymous-type identity
cut (ADR-0066 / RUE-1089), encoding reviewer (codex)'s acceptance list.

The corpus is valuable in two states:

- **Today** — it documents the deliberately fail-closed frontier precisely.
  Every listed test passes against the current worktree tree
  (`rue-1089-wip`). The passing cases assert real current behavior; the cases
  that currently hit the fail-closed **E9000** blocker assert that exact loud
  failure.
- **Post-anchor-fix** — the E9000 tests flip to green and become the exit-42
  execution regression suite. Each flip point is marked
  `FLIPS-POST-ANCHOR-FIX` in the source with the exact mechanical edit.

Background: `docs/notes/rue-1089-current-anonymous-type-behavior.md`,
`SITE_INVENTORY_SCRATCH.md`.

## Test homes

| Home | File | Why |
| --- | --- | --- |
| Spec runner | `crates/rue-spec/cases/expressions/producer_nominal_acceptance.toml` (NEW, auto-discovered by the `glob(["cases/**"])` filegroup) | Behavioral exit-code / compile-fail cases that pass today. |
| Compiler unit | `crates/rue-compiler/src/producer_nominal_acceptance_tests.rs` (NEW, registered in `crates/rue-compiler/src/lib.rs`) | Programmatic assertions (warm/fresh/cold parity, symbol-set comparison) and the E9000 cases the spec/CLI runners reject as ICEs. |

### Key mechanical finding

The spec and CLI runners **intentionally reject an E9000 ("internal compiler
error") as an ICE failure that can never satisfy a `compile_fail` case**
(`rue-test-runner::ice_message`, applied *before* the `compile_fail` branch in
`run_test_case`). Therefore the current fail-closed state of criteria 5 and 6
**cannot** be encoded as a green spec/CLI case; those criteria are asserted
against the compiler API in the unit-test module instead, where the E9000 is a
returned `Err(CompileErrors)` (a controlled diagnostic, not a Rust panic).

## Ledger

| Criterion | Test name(s) → location | Current outcome (verified) | Expected post-anchor-fix |
| --- | --- | --- | --- |
| **1** — two anon decls of the same kind in ONE producer are distinct | `c1_producer_local_twin_structs_are_distinct`, `c1_producer_local_twin_structs_coexist` → spec TOML | PASS. Cross-assign of two same-shape anon structs is rejected (`E0206` type mismatch); both coexist and are usable (exit 42). | Unchanged (still PASS). |
| **2** — nested anon decls in different positions | `c2_nested_anon_different_statements`, `…_different_if_branches`, `…_different_match_branches`, `…_different_operands`, `…_different_methods`, `c2_nongeneric_wrap_matches_anon_enum_field`, `c2_generic_free_fn_matches_anon_enum`, `c2_generic_method_nested_anon_struct`, `c2_generic_wrap_field_untouched` → spec TOML | PASS. **Empirically: all nested positions compile and run today** (exit 42, or 7 for the field-untouched case). The generic free-fn match and the generic method with a nested anon *struct* both pass; only the generic-struct-method-over-anon-*enum*-field shape (criterion 5) fails — isolated separately. | Unchanged (still PASS). |
| **3** — identity stable under unrelated edits (method reorder, added decls) | `c3_identity_stable_baseline_order`, `c3_identity_stable_reordered_with_unrelated_decl` → spec TOML; `producer_nominal_identity_is_stable_under_unrelated_edits` → unit | PASS. Reordering sibling methods and adding an unrelated top-level fn leaves behavior unchanged (exit 42) and leaves the anonymous-identity count and named-symbol surface unchanged. | Unchanged (still PASS). |
| **4** — warm/fresh/cold produce identical semantic bodies/layouts/symbols | `producer_nominal_semantic_output_is_deterministic_across_cold_compiles`, `producer_nominal_warm_and_fresh_semantic_output_agree` → unit | PASS. Two cold compiles are byte-identical in `unstable_parity_snapshot` (bodies, layouts, type pool, dependencies) and emit an identical symbol set; a warm incremental compile equals a fresh one on the same projection. Reuses the scaling-harness parity oracle (`CanonicalSemanticOutput::unstable_parity_snapshot`) test-side. Symbols compared between two independent cold compiles via `struct_symbol_name`/`enum_symbol_name` + function `machine_name`. | Unchanged (still PASS). |
| **5** — Wrap single-nominal-identity, exit 42 | `wrap_single_nominal_identity_executes_to_the_payload` → unit; `c5_wrap_single_nominal_identity` → spec TOML | **FLIPPED, PASS.** The anchor is transported exactly into the durable evaluator, so the generic `Wrap` whose `get_or` method matches its anonymous-enum field `Option(T)` compiles and executes to **exit 42**. The receiver field type, the `Option(T)` in the match, the match enum key, the payload op, and the enum layout resolve to ONE Option identity — observable as exactly one anonymous enum in the type pool. |
| **6** — both backends execute the Wrap payload regression | `wrap_payload_executes_on_both_backend_targets` → unit | **FLIPPED, PASS.** Both `x86-64-linux` and `aarch64-linux` compile and link; the native x86-64 ELF executes → exit 42; aarch64 is a structural cross-compile check off its native host (mirrors `cli.abi_conformance`). |
| **7** — artificial anchor disagreement fails closed | `divergent_anchor_transport_fails_closed_loud`, `resolve_level_transport_corruptions_fail_closed_loud`, `fault_probe_compiles_and_runs_cleanly_without_a_marker` → unit | **PASS (HARDENED, Stage D).** All FOUR corruption modes now fail closed identically: a test-only fault-injection hook in `SemanticConstEvaluator::resolve_anonymous_anchor` (fragment-marker keyed, race-free) corrupts the transported table; each mode commits the typed E9000-class internal diagnostic and the request returns `Err` with **zero published semantic output**. Previously missing/duplicate/kind were "frontend-recoverable" — the committed producer failure was downgraded to a retryable `Canceled` abort and masked by a live AIR mint (a second identity authority). The reviewer ruled that unacceptable; `body_produced_anonymous` no longer downgrades a committed internal error, so the transported anchor is the sole identity authority and no RIR/AIR recomputation rescues a corrupt table. Genuine unavailability still retries; unused table entries stay legal (`selected_branch_consumes_a_subset_of_the_transported_table`). |

## Commands used

Worktree: `/home/user/rue-1089`, branch `rue-1089-wip`, base commit `df674c8d`.
Compiler binary snapshot (guard against cross-checkout buck-out clobber):
`/tmp/claude-0/-home-user/d68ffdca-d0e6-57e2-b16b-d07f4e7cb73a/scratchpad/rue-1089-acceptance-bin`.

```console
# Build the worktree compiler
scripts/rue-bin

# Empirical single-program checks (repro capture)
RUE="$(scripts/rue-bin)"; "$RUE" wrap.rue -o wrap        # -> E9000 …did not publish a terminal
"$RUE" wrap.rue -o wrap --target aarch64-linux           # -> same E9000 (frontend, backend-independent)

# New spec cases (auto-discovered new file)
scripts/rue spec producer_nominal_acceptance             # 13 passed

# New compiler unit tests
scripts/rue unit compiler producer_nominal               # 5 passed

# Traceability gate (unaffected; 3 pre-existing known-uncovered paragraphs)
./buck2 run //crates/rue-spec:rue-spec -- --traceability
```

## std example end-to-end runs (Stage C)

Compiled and executed with the worktree compiler (debug binary snapshot at
`…/scratchpad/rue-1089-final-bin`), `RUE_STD_PATH=$PWD/std`, x86-64-linux native.
`examples/caldera` is deliberately skipped (too slow; handled at integration).

| Example | Files | Compile wall | Compile exit | Run wall | Run exit |
| --- | --- | --- | --- | --- | --- |
| `examples/rill` | 1 root + std | 29.6s | 0 | 0.003s | 0 (output: `rillrillrill` / `55` / `true`) |
| `examples/meridian` | 265 `.rue` | see report | 0 | see report | 0 |

## Empirically-characterized E9000 frontier (HISTORICAL — pre-anchor-fix)

This table records the pre-anchor-fix frontier. After anchor-transport landed,
the last shape (Wrap) compiles and executes to exit 42 (criteria 5/6, FLIPPED),
so there is **no remaining E9000 frontier** in correct programs — an E9000 now
appears only when the transported table is genuinely corrupt (criterion 7).

| Shape | Result pre-fix | Result now |
| --- | --- | --- |
| Non-generic struct method matching its anon-enum field | compiles, exit 42 | unchanged |
| Generic **free function** matching an anon enum | compiles, exit 42 | unchanged |
| Generic struct method declaring a nested anon **struct** | compiles, exit 42 | unchanged |
| Generic struct with anon-enum field, method never reaches it | compiles, exit 7 | unchanged |
| **Generic struct method matching its anon-enum field** (Wrap) | **E9000 fail-closed** | **compiles, exit 42** |

## Review-response revision (adversarial re-review themes)

This section records the acceptance-relevant test changes made while addressing
the first four architectural review themes (2, 4a, 6). The deep themes (single
anchor authority, transport validate-once, the digest-collision registry, and
the escape-hatched bare-intrinsic `?`) are recorded in *Deep-review themes*
below.

- **Theme 6 — macOS execution guards.**
  `wrap_single_nominal_identity_executes_to_the_payload`,
  `fault_probe_compiles_and_runs_cleanly_without_a_marker`, and
  `evaluator_correspondence_two_same_kind_sites_do_not_swap` now gate the
  *execution* of the default `x86-64-linux` ELF behind
  `cfg!(all(target_os = "linux", target_arch = "x86_64"))` (the predicate
  `wrap_payload_executes_on_both_backend_targets` already used), keeping their
  semantic/compile/link assertions unconditional. On macOS they now compile and
  link the output without running a Linux binary.

- **Theme 2 — representative deletion.** `AnonymousNominalIdentitySet` and the
  compiler-side `CanonicalAnonymousNominalAssociation` are gone;
  `SemaOutput::anonymous_nominal_identities_by_type` is now a direct
  `Type -> AnonymousNominalKey`. The degenerate `canonical_anonymous_aliases`
  singleton map, alias insertions, stable-min selection, and the flatten
  consumers are removed. No acceptance criterion outcome changes; determinism is
  preserved (retained via the direct-key sort in `one_body.rs`). Proven by the
  criterion-4 cold/warm/fresh parity tests and the full spec suite.

- **Theme 4a — FileId-independent anonymous symbols.** NEW unit test
  `anonymous_symbols_are_stable_across_permuted_file_ids` (compiler): the same
  single-file logical program (same path) presented at `FileId(0)` vs
  `FileId(7)` must emit byte-identical `__anon_*` symbols. The digest now hashes
  the canonical logical module path (via `Sema::symbol_paths`) instead of the
  request-local numeric `FileId`. The test was confirmed to fail when the
  component is reverted to the numeric FileId.

Verification for this revision (Linux x86-64 host): `scripts/rue unit air`
(565), `scripts/rue unit compiler` (686, incl. `producer_nominal` 13 with the
Wrap→42 execution), `scripts/rue spec` (2173), clippy clean for `rue-air` and
`rue-compiler`.

## Deep-review themes (second revision)

This revision lands the three deep review themes in the safest-to-riskiest
order 5 → 1, stops the riskiest (3) at the escape hatch, and folds in the
reviewer's later Theme 4b. Sequenced so the safest work never depends on the
riskiest.

- **Theme 5 — transport validate-once, keyed lookup, fail-before-terminal.**
  `resolve_anonymous_anchor` no longer re-runs O(S²) whole-producer
  well-formedness on every one of S lookups (formerly O(S³) per producer). The
  transported table is validated exactly once when the reparsed fragment is
  built (`TransportedAnonymousSites` in `semantic_query_nucleus.rs`), producing
  an immutable `BTreeMap` keyed by fragment-local locator plus an authorized-
  anchor set; each eval lookup is now O(log S) with no revalidation. A divergent
  (wrong-but-present) anchor now fails the PRODUCER terminal itself:
  `validate_transported_anchor_authority` cross-checks every nominal a producer
  minted against the authorized-anchor set before the direct `ComptimeCall`/const
  terminal publishes, so no nominal/member/alias/cache entry is published from a
  corrupt table. NEW direct-terminal test
  `divergent_anchor_fails_the_producer_comptime_terminal_directly` (compiler)
  queries the `Wrap(i32)` `ComptimeCall` terminal and asserts it IS the typed
  E9000 (never a `ComptimeCall` projection carrying a divergent nominal),
  verified to fail against the pre-fix ordering. The three table-corruption fault
  modes inject at construction; the divergent mode injects at resolve.

- **Theme 1 — single anchor authority; AstGen consumes the walk.** `AstGen` no
  longer mints anonymous-type anchors from its own `structural_path` (the second,
  drift-prone algorithm). It populates a span-keyed table from the shared
  `rue_rir::anonymous_type_sites` walk when it enters each producer root and
  resolves every value-position anonymous struct/enum literal by exact source
  span; a missing locator or kind mismatch fails closed
  (`RirPayloadBuildError::InvalidBuilderInput` → E9000), with no recompute and no
  fallback. `anonymous_type_anchor`'s old `structural_path` computation is
  deleted; `structural_path` stays only for string-literal and read-only-data
  anchors. Lookup is by exact source span with no coordinate translation —
  verified: both the walk (`build_definition_index`) and `AstGen`
  (`lower_module_rir`) run on the original per-module AST, whose spans carry the
  same `(file_id, start, end)`. **Drift finding: none.** Behavior is neutral —
  every suite stayed green after the swap (no case disagreed), so the retained
  table and `AstGen` mint were in fact identical, as the lockstep verification
  claimed. The bijection tests are retained and re-documented as the walk-
  coverage guard (a fail-closed miss panics `AstGen::finish`).

- **Theme 3 — bare-intrinsic `?` demands std Option: STOPPED at the escape
  hatch.** `find_compatible_anon_enum` is unchanged and still selected. Precise
  blocker: the `?`-on-bare-intrinsic Option is resolved inside rue-air's
  synchronous one-body `Sema` (`analyze_fallible_intrinsic`
  → `find_compatible_anon_enum`), where (1) the `SemanticNucleusKey::ComptimeCall`
  machinery is not reachable — it is a rue-compiler query; rue-air has zero
  `SemanticNucleusKey` references and resolves comptime calls only through its
  in-process `reduce_type_ctor_body`, and the coordinator's deferred-producer
  demand path only schedules producers a body *syntactically references* (via
  `collect_instance_anonymous_nominals`), which a synthesized bare-intrinsic
  Option is not; (2) there is no std-Option-producer locator at that site — the
  current code shape-scans precisely because it has no
  `\0rue-std/option.rue::Option` key, and naming it needs new std-module
  resolution plumbing; (3) soundness/availability fails this cut — every existing
  `?` spec/CLI case supplies a USER-defined `Option` (and `@parse_i64(s)?` over
  builtin `i64` can be freestanding with no std Option loaded), so forcing a std
  demand would fail to resolve where std Option is absent and silently change
  which nominal the intrinsic's `?` binds. Implementing the required mechanism
  would mean inventing a new cross-crate AIR→nucleus demand path, which the theme
  forbids. Escalate as RUE-1112 blocker. **Consequence:** the `anonymous_key_cmp`
  retention rename is also blocked — `find_compatible_anon_enum` (anon_structs.rs)
  still uses `min_by(anonymous_key_cmp)` as a min/first-wins SELECTION consumer,
  so the comparator cannot yet be re-documented as presentation-order-only. The
  only other consumer (`one_body.rs` `sort_by`) is already pure export ordering.

- **Theme 4b — fail-closed anonymous digest-collision registry.** A deterministic
  exact-key registry (`Sema::anonymous_digest_owners`, `digest →
  AnonymousNominalKey`) now fronts BOTH anonymous struct and enum registration
  (shared guard `guard_anonymous_digest_collision` in
  `find_or_create_anon_struct`/`find_or_create_anon_enum`). Same digest + same key
  = reuse; same digest + a DISTINCT key = typed E9000 with zero publication (no
  panic, no silent `EnumId`/`StructId` name-dedup reuse). The diagnostic spells
  both stable keys and the digest with no pool indices. NEW tests
  (`theme4b_digest_collision_tests`, air) drive a forced-digest hook: distinct
  keys fail closed in both insertion orders with zero publication; the same key
  reuses before and after other registrations; verified to fail with the registry
  disabled.

Verification (Linux x86-64 host): `scripts/rue unit air` (568),
`scripts/rue unit compiler` (687, incl. `producer_nominal` 13 and the new direct
divergent-terminal test), `rir` (59), `cfg` (209), `codegen` (606), `parser`
(43), `query` (44); `scripts/rue spec` (2173); `scripts/rue ui` (204);
`scripts/rue cli lazy_specialization` (4) and `cli try` (21). Clippy clean for
`rue-air`, `rue-rir`, `rue-compiler`. Direct executions: Wrap→42, distinct
producers→E0206, recursion→E1200; Box-destructor ordering via
`scripts/rue spec destructors` (56).
