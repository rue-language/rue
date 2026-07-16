---
id: 0048
title: "Shared codegen middle layer (reduce x86-64/aarch64 backend duplication)"
status: accepted
tags: [codegen, architecture, backends, refactor, maintainability]
created: 2026-07-11
accepted: 2026-07-15
implemented: RUE-825
spec-sections: []
relates: ["RUE-820", "RUE-821", "RUE-822", "RUE-823", "RUE-824", "RUE-825"]
---

# ADR-0048: Shared Codegen Middle Layer

## Decision

Rue has one target-independent CFG lowering core in
`crates/rue-codegen/src/value_plan.rs` and `terminator_plan.rs`. The core owns
raw `CfgInstData` classification, recursive dependency materialization, value
memoization, aggregate slot completeness, block-parameter routing, by-reference
classification/preloads, call and return planning, sret selection, and the
shared CFG/terminator walk. It emits no target MIR, physical register, flag,
encoder, or target label.

The core exposes six separate policy-facing domain events:

```text
emit_value(ValueEmissionPlan) -> ValueResult
emit_call(CallPlan) -> ValueResult
emit_terminator(TerminatorPlan)
emit_intrinsic(IntrinsicPlan) -> ValueResult
emit_checked_arithmetic(ArithmeticPlan) -> ValueResult
emit_trap(TrapPlan) -> ValueResult
```

`lower_value` is the single exhaustive language-level `CfgInstData` dispatcher.
The value domain plan excludes calls, intrinsics, checked arithmetic, and
traps; each of those has its own domain event. Plans contain materialized
vregs, logical slot vectors, widths, signedness, symbols, labels, and decided
ABI facts. They contain no raw CFG references. The core owns the association of
each resulting `ValueResult` with its source `CfgValue`; `cache_value` is the
opaque state hook used to perform that final map insertion, so its key is not a
policy input and cannot be used to reconstruct language semantics. Cleanup
plans likewise contain already-decided symbols, action order, and materialized
logical slot vectors. Domain plans and emission hooks contain no raw CFG
references.

The x86-64 and AArch64 adapters may match only their domain plan and target
facts. They retain MIR instruction definitions, physical registers, ABI
register/stack details, flags/NZCV behavior, immediates, encoders, scheduling,
peepholes, and native-runtime differences. There is no generic MIR container,
target-neutral instruction enum, or third generic backend.

## Current non-test CFG-lowering inventory

The authoritative inventory is the non-test `fn` list in these two files:

- [`x86_64/cfg_lower.rs`](../../crates/rue-codegen/src/x86_64/cfg_lower.rs)
- [`aarch64/cfg_lower.rs`](../../crates/rue-codegen/src/aarch64/cfg_lower.rs)

The following table names each same-purpose pair that remains in both
backends. Trait forwarding methods are included; test helpers are excluded.
Names not listed as pairs are target-specific functions, not duplicated
language policy.

| Same-purpose pair | x86-64 | AArch64 | Justifying target fact |
| --- | --- | --- | --- |
| `emit_value`, `emit_call`, `emit_intrinsic`, `emit_checked_arithmetic`, `emit_trap`, `lower_residual_value`, `lower_param_value`, `lower_checked_arithmetic`, `lower_call_plan`, `lower_intrinsic_plan`, `lower_option_intrinsic`, `lower_trap`, `lower_drop_plan`, `emit_slot_call` | yes | yes | target MIR instruction selection, call ABI, runtime entry sequence, and physical operands; semantic plans are already decided |
| `lower_comparison`, `lower_scalar_comparison`, `emit_masked_shift_count_vreg`, `emit_subword_narrow`, `emit_int_cast_check`, `emit_signed_div_overflow_check` | yes | yes | target compare/extension forms, fixed-register division sequences, and FLAGS versus NZCV |
| x86-64 `emit_overflow_check`; AArch64 `emit_overflow_check_add`, `emit_overflow_check_sub`, `emit_overflow_check_mul`, `emit_overflow_check_neg`, `emit_subword_range_check`, `push_cmp_rr` | yes | yes | one target-specific checked-arithmetic leaf family split differently to preserve x86 FLAGS and AArch64 NZCV/high-half/subword forms |
| `emit_load_slot`, `emit_store_slot`, `emit_load_ptr_base`, `emit_store_ptr_base`, `emit_load_through_ptr`, `emit_store_through_ptr`, `emit_frame_addr`, `emit_addr_add`, `emit_addr_add_imm`, `emit_scale_index_bytes`, `emit_scale`, `emit_zero_sized_place` | yes | yes | target MIR memory/address forms and pointer arithmetic |
| `alloc_bounds_length`, `emit_bounds_compare`, `alloc_bounds_label`, `emit_bounds_branch`, `emit_bounds_trap`, `emit_bounds_label`, `alloc_scale_result` | yes | yes | target virtual-register/label allocation and branch/trap encoding for shared bounds and scale plans |
| `materialize_scalar`, `materialize_aggregate`, `materialize_by_ref`, `materialize_sret_pointer`, `alloc_vreg`, `block_label`, `ensure_by_ref_param_ptr`, `get_vreg`, `value_is_lowered`, `reserve_value_result`, `cache_value`, `ctx`, `slot_cache`, `intern_symbol`, `resolve_symbol`, `call_arg_register_budget`, `return_register_budget` | yes | yes | target MIR/container state, symbol interning, ABI register budgets, and physical by-ref representation |
| `emit_reg_move`, `emit_bool_const`, `emit_slot_eq`, `emit_bool_and`, `emit_bool_not` | yes | yes | target move, compare, boolean, and flag-producing instruction forms |
| `emit_block_label`, `emit_terminator`, `emit_terminator_plan`, `emit_edge_moves`, `materialize_value`, `materialize_block_param`, `preload_by_ref_param_ptrs`, `preload_by_ref_params`, `prepare_block_param`, `require_aggregate_slots` | yes | yes | target CFG labels, edge moves, preloads, and aggregate register state |
| `new`, `lower`, `lower_with_debug`, `get_lowering_rationale`, `instruction_count`, `instruction_strings`, `value_description`, `value_rationale`, `terminator_rationale` | yes | yes | target MIR ownership and debug/presentation formatting |
| `ValueLowerAdapter`, `SlotBackend`, `PlaceLowerBackend`, `BoundsCheckBackend` forwarding methods | yes | yes | target register, label, and MIR APIs |

The shared policy helpers `integer_width`, `type_bits`, `shift_count_mask`,
`comparison_integer_width`, `integer_extension`, and aggregate-primary
selection have no target pair. They are deliberately absent from both
backend files.

The mechanically target-specific remainder is also intentional: x86-64
`emit_div_core` owns RAX/RDX division setup and `new_label` wraps its label
allocator; AArch64 has no same-purpose helper because those operations are
formed directly by its MIR allocator and checked-arithmetic leaves. The
different-name checked-arithmetic family is listed explicitly above rather
than being hidden by the name-only comparison.

The exact function names and line numbers are intentionally not duplicated in
this ADR because formatting changes them. Reviewers can mechanically verify
the paired-name set and the genuinely target-specific remainder with this
command; the `awk` exit makes the exclusion independent of test function names
and formatting below the test module. The first `comm` output is the complete
same-name set that must be accounted for by the table; the second output is the
target-specific remainder:

```bash
extract_cfg_fns() {
  awk '/^#\[cfg\(test\)\]/{exit} /^    (pub )?fn /{sub(/^    (pub )?fn /, ""); sub(/\(.*/, ""); print}' "$1" | sort -u
}
x86=$(mktemp)
arm=$(mktemp)
trap 'rm -f "$x86" "$arm"' EXIT
extract_cfg_fns crates/rue-codegen/src/x86_64/cfg_lower.rs >"$x86"
extract_cfg_fns crates/rue-codegen/src/aarch64/cfg_lower.rs >"$arm"
comm -12 "$x86" "$arm"
comm -3 "$x86" "$arm"
```

No backend contains a peer `lower_value` or raw semantic dispatcher. The only
language-level `lower_value` is the shared function in `value_plan.rs`; the
shared CFG traversal calls it and the shared terminator planner routes the
normalized terminator event.

## Boundary invariants

1. Every aggregate result has exactly `type_slot_count` logical slots,
   including zero-slot aggregates and the three-slot `StrBuf` representation.
2. Logical slot order follows ADR-0040. Adapters may choose machine emission
   order only after receiving the normalized vector.
3. Index bounds checks precede address formation on every place path.
4. `type_uses_sret_return` and the shared call/return plan are the canonical
   sret and ABI-shape computations.
5. Block parameters, by-reference preloads, and aggregate reads/writes use one
   shared cache and one shared slot policy.
6. Debug and golden lowering views consume the same plans as normal lowering.
7. Adding a language-level CFG operation requires an edit to the shared
   exhaustive dispatcher and its domain plan, not two backend CFG matches.

## Target facts that remain local

`Reg`, `Cond`, MIR instruction variants and display, physical register sets,
SysV AMD64 versus AAPCS64 register/stack conventions, frame-pointer offsets,
implicit RAX/RDX division operands, AArch64 high-half arithmetic, FLAGS versus
NZCV, immediate encodings, spill/reload forms, liveness uses/defs/clobbers,
scheduling latency, peepholes, relocations, encoders, prologues/epilogues, and
native runtime entry differences remain target-local by design.

## Validation and review

Codegen changes must exercise both adapters and preserve existing lowering,
liveness, regalloc, MIR, and assembly output unless a format-only change is
explicitly explained. Required evidence includes focused codegen tests, the
quick suite, the serialized full suite, documentation/ADR checks, native
x86-64 coverage, cross-target AArch64 lowering/assembly/encoding coverage, and
`git diff --check`.

Reviewers must verify that a semantic fact is computed in the shared core and
that every remaining backend pair is justified by a target fact from the
inventory above. Semantic emission hooks may not inspect `Cfg`, rederive slot
counts or sret selection, classify by-reference arguments, or introduce
another value or terminator dispatcher. Debug presentation helpers may inspect
the source CFG only to describe an already-lowered value.
