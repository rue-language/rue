# RUE-1089 producer-nominal acceptance ledger

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
| **7** — artificial anchor disagreement fails closed | `divergent_anchor_transport_fails_closed_loud`, `resolve_level_transport_corruptions_never_miscompile`, `fault_probe_compiles_and_runs_cleanly_without_a_marker` → unit | **ENCODED, PASS** via a test-only fault-injection hook in `SemanticConstEvaluator::resolve_anonymous_anchor` keyed on a fragment-source marker (race-free, no global state). A **divergent** anchor (wrong-but-present) publishes a terminal a reached member cannot match → loud E9000, request `Err`. The resolve-level corruptions (missing/duplicate/kind) raise the fail-closed diagnostic before any terminal publishes; because no wrong terminal publishes, the frontend recovers the correct nominal — never a silent WRONG answer (asserted by compile+execute → 42 or fail-closed). **Finding:** a missing/duplicate/kind transport error is frontend-recoverable (correct answer), only a divergent published anchor is fatal; both satisfy "loud errors over silent wrong answers". |

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

## Empirically-characterized E9000 frontier

The fail-closed frontier is narrower than "any generic anonymous member":

| Shape | Result today |
| --- | --- |
| Non-generic struct method matching its anon-enum field | compiles, exit 42 |
| Generic **free function** matching an anon enum | compiles, exit 42 |
| Generic struct method declaring a nested anon **struct** | compiles, exit 42 |
| Generic struct with anon-enum field, method never reaches it | compiles, exit 7 |
| **Generic struct method matching its anon-enum field** (Wrap) | **E9000 fail-closed** |

Only the last shape — a generic struct producer whose method **reaches an
anonymous-enum member under the contextual (generic) anchor** — triggers the
divergence. This is the single remaining blocker the anchor-unification fix
targets.
