---
id: 0048
title: "Shared codegen middle layer (reduce x86-64/aarch64 backend duplication)"
status: accepted
tags: [codegen, architecture, backends, refactor, maintainability]
created: 2026-07-11
accepted: 2026-07-15
implemented:
spec-sections: []
supersedes:
relates: ["RUE-250", "RUE-819", "RUE-820", "RUE-821", "RUE-822", "RUE-823", "RUE-824", "RUE-825", "RUE-3", "RUE-31", "RUE-237", "RUE-248", "RUE-205", "RUE-45"]
---

# ADR-0048: Shared Codegen Middle Layer

## Status

Accepted as the M3 shared-backend policy design by RUE-819, 2026-07-15. This
ADR is a design-only increment. It authorizes the dependency-defined
implementation slices below; it does not implement any of them.

## Summary

Rue already has a shared codegen spine. `agg_slots`, place lowering, storage
lowering, aggregate equality, by-reference argument addressing, liveness,
scheduling, register-allocation algorithms, stack-frame accounting, and pass
sequencing are single-copy code or shared algorithms with target adapters. The
remaining problem is that the two `CfgLower` implementations still mix three
responsibilities in the same methods:

1. language and CFG policy (value routing, aggregate completeness, block
   parameters, sret decisions, and terminator semantics);
2. target ABI policy (register and stack locations, call/return marshalling,
   platform runtime entry points); and
3. instruction selection (MIR variants, flags, immediates, scratch registers,
   and target encodings).

M3 makes the language/CFG responsibility single-copy and makes logical ABI
planning explicit; physical ABI facts remain in the adapters. The shared layer
produces normalized lowering plans. An x86-64 or AArch64 adapter consumes those
plans and selects instructions. There is no target-independent instruction set,
register enum, or third generic backend.

## Current source audit

The current source, rather than the historical RUE-250 design pass, is the
authority for this decision. The shared top-level codegen modules currently
contain 8,404 lines including tests (`crates/rue-codegen/src/*.rs`). The backend
files currently measure as follows; `code` stops immediately before each file's
`#[cfg(test)]` module.

| Backend file | x86-64 total/code | AArch64 total/code | Current classification |
| --- | ---: | ---: | --- |
| `cfg_lower.rs` | 4,221 / 4,033 | 4,309 / 4,089 | mixed CFG policy, ABI policy, and instruction selection; primary M3 target |
| `mir.rs` | 973 / 916 | 1,435 / 1,338 | target instruction and register definitions plus duplicated container plumbing |
| `regalloc.rs` | 1,712 / 1,247 | 1,690 / 1,223 | shared algorithm calls plus target rewrite and spill emission |
| `liveness.rs` | 724 / 449 | 664 / 391 | shared analysis adapter plus target uses/defs/clobbers facts |
| `schedule.rs` | 1,028 / 629 | 817 / 511 | shared scheduler adapter plus target latency/dependency facts |
| `peephole.rs` | 1,001 / 301 | 978 / 267 | target flag model and instruction patterns |
| `emit.rs` | 4,858 / 2,758 | 3,761 / 2,240 | target encoding, relocations, prologue, epilogue, and ABI entry |

The current shared seams are concrete, not proposed abstractions:

- `crate::liveness::analyze_adapter`, `analyze_debug_adapter`, and
  `analyze_loops_adapter` own the dataflow algorithms; the backend files only
  provide instruction facts ([`liveness.rs`](../../crates/rue-codegen/src/liveness.rs:35)).
- `schedule_core::schedule_instructions` owns dependency construction and list
  scheduling; each backend supplies latency, memory, register, and flag facts
  ([`schedule_core.rs`](../../crates/rue-codegen/src/schedule_core.rs:12)).
- `agg_slots`, `place_lower`, `storage_lower`, `aggregate_eq`, and `byref_args`
  already own aggregate, place, storage, equality, and by-reference policy
  ([`agg_slots.rs`](../../crates/rue-codegen/src/agg_slots.rs:35),
  [`place_lower.rs`](../../crates/rue-codegen/src/place_lower.rs:21)).
- `codegen_pipeline::prepare_mir` already owns pass order and the distinction
  between existing frame slots and emitted locals
  ([`codegen_pipeline.rs`](../../crates/rue-codegen/src/codegen_pipeline.rs:19)).
- `rue-compiler::backend` dispatches to the two target lowerers; it is not a
  second lowering implementation ([`backend.rs`](../../crates/rue-compiler/src/backend.rs:233)).

The stale ADR-0048 claims that shared codegen is approximately 8,450 lines,
that the lowerers are roughly 4,295/4,310 lines, and that the work is two
optional extractions. Those figures are no longer true. More importantly,
"extract the skeleton" is not a sufficient ownership boundary: the current
`lower_value` and `lower_terminator` bodies still decide language and ABI
semantics while constructing target instructions.

## Function-level ownership inventory

This is the complete inventory of duplicated non-test lowering functions and
their current owner. A name appearing in both backend files does not imply
that its implementation should be shared; the classification below records
the reason.

### CFG lowering

| Current function(s) | Current locations | M3 owner | Classification and action |
| --- | --- | --- | --- |
| `new`, `lower`, `lower_with_debug` | x86-64 `cfg_lower.rs:81,548,575`; AArch64 `cfg_lower.rs:74,372,399` | target entrypoint + shared core | Keep target construction and return types in the two entrypoints, but make both call the same core walk. Debug output must observe the same plans as normal lowering. |
| `lower_block`, `lower_value`, `lower_terminator`, `get_vreg` | x86-64 `cfg_lower.rs:794,812,3566,3808`; AArch64 `cfg_lower.rs:582,600,3637,3873` | shared CFG core | Move the block walk, value cache lookup, dependency recursion, terminator case analysis, and fall-through/label policy into one generic driver. The driver passes normalized plans to an adapter. |
| `copy_aggregate_to_block_param` | x86-64 `:516`; AArch64 `:340` | shared CFG core + `agg_slots` | Move block-parameter slot routing into the core. Require complete logical slot vectors and retain the existing ascending-layout invariant. |
| `get_or_compute_field_vregs`, `require_aggregate_slots` | x86-64 `:503,509`; AArch64 `:327,333` | `agg_slots` | These are duplicated wrappers around the existing shared aggregate policy. Delete the wrappers or make them direct calls; do not create a second aggregate path in the new core. |
| `collect_array_scalar_vregs`, `collect_struct_scalar_vregs` | x86-64 `:181,189`; AArch64 `:176,184` | `types`/shared CFG core | Keep only the shared recursive collection algorithm and a core-owned cache view. |
| `alloc_element_size`, `is_unsigned_comparison`, `try_power_of_two_shift`, `type_bits`, `type_range`, `shift_count_mask` | both `cfg_lower.rs` files | shared pure policy | Deduplicate the type/range/stride calculations. The adapter receives the resulting width, signedness, and immediate policy rather than re-reading CFG types. |
| `ensure_by_ref_param_ptr`, `preload_by_ref_param_ptrs` | x86-64 `:202,227`; AArch64 `:192,217` | shared by-ref policy + adapter load | The cache, parameter classification, and “load once” rule are shared. Loading the pointer and any required move remain adapter leaves. Bounds checks must precede address formation. |
| `scale_by_size` | x86-64 `:139`; AArch64 `:133` | shared size plan + target adapter | Share overflow intent and element-size calculation. Keep the multiply/high-half/flag sequence per target. |
| `emit_bounds_check`, `emit_masked_shift_count`, `emit_subword_narrow`, `emit_comparison`, `emit_signed_div_overflow_check` | both `cfg_lower.rs` files | target adapter consuming a normalized plan | The language rule and operands are shared; comparison conditions, flags, narrowing instructions, and overflow sequences are target facts. |
| `emit_call_with_slot_args` | x86-64 `:275`; AArch64 `:268` | shared `CallPlan` + target ABI adapter | Flattening, by-ref arguments, sret selection, and logical slot order are shared. Register assignment, stack argument offsets, alignment, and the runtime call instruction remain target-specific. |
| `get_lowering_rationale`, `get_terminator_rationale` | x86-64 `:680,774`; AArch64 `:504,562` | presentation adapter | These are debug presentation, not compiler policy. They may consume shared plan metadata, but must not become a second lowering implementation. Target ABI names in the explanation remain target-specific. |
| `intern_symbol`, `new_label`, `block_label`, `ctx`, `slot_cache`, `alloc_vreg`, `map_value`, `emit_aggregate_equality`, and the `SlotBackend`/`PlaceLowerBackend`/`StorageLowerBackend`/`AggregateEqBackend` methods | both backend files | target adapter plumbing | These are state access and instruction leaves. Retain the existing adapter traits; do not move target MIR construction into a shared module. |

The non-paired functions are evidence for the boundary, not missing work:

- x86-64 keeps `emit_div_core`, `emit_overflow_check`, and
  `emit_builtin_eq_call` for its implicit RAX/RDX and FLAGS behavior.
- AArch64 keeps `emit_overflow_check_add`, `emit_overflow_check_sub`,
  `emit_overflow_check_mul`, `emit_overflow_check_neg`, `emit_string_eq_call`,
  `emit_subword_range_check`, and `push_cmp_rr` for its NZCV and high-half
  arithmetic behavior.
- Platform-specific AArch64 runtime entry selection remains in the AArch64
  adapter (`cfg_lower.rs` carries the Linux/macOS distinction).

### Other duplicated backend modules

| Module/function family | Current source evidence | Owner after M3 |
| --- | --- | --- |
| `liveness::{analyze, analyze_debug, analyze_loops}` | x86-64 `liveness.rs:63-73`; AArch64 `liveness.rs:63-73` | Thin target wrappers over shared analysis. `get_label`, `get_successors`, `uses`, `defs`, and `clobbers` remain per instruction set. |
| `schedule::{schedule}` and `SchedulerAdapter` forwarding methods | x86-64 `schedule.rs:34-68,653`; AArch64 `schedule.rs:34-68,535` | Thin wrappers over `schedule_core`. `get_latency`, memory classification, register reads/writes, and FLAGS/NZCV facts remain per target. |
| `RegAlloc::{new, allocate, allocate_with_spills, allocate_with_debug, assign_registers, assign_registers_with_debug, rewrite_instructions, rewrite_inst, load_operand, get_allocation}` plus shared-shape helpers `emit_binop`, `emit_unop`, `emit_unop_imm` (x86-64) and `emit_binop`, `emit_ternop`, `emit_binop_imm` (AArch64) | x86-64 `regalloc.rs:65-221,996`; AArch64 `regalloc.rs:68-209,1051` | Shared linear scan, coalescing, spill-slot allocation, and cost model stay in `regalloc.rs`. The target driver may remain thin; `ALLOCATABLE_REGS`, operand rewriting, scratch registers, and spill/reload MIR remain local. |
| `X86Mir`/`Aarch64Mir` container methods (`new`, `push`, `alloc_vreg`, `alloc_label`, `instructions`, `instructions_mut`, `into_instructions`, counts) | x86-64 `mir.rs:761-903`; AArch64 `mir.rs:1162-1325` | Defer outside M3. The current container duplication is recorded for audit only; `Inst`, `Reg`, `Cond`, operand encodings, and target display remain local. |
| `peephole::optimize` and helpers | x86-64 `peephole.rs:44-301`; AArch64 `peephole.rs:44-267` | Keep completely per target. The two passes do not have the same flag semantics: x86 FLAGS and AArch64 NZCV are materially different. |
| `emit::{emit, emit_all}` and all encoder helpers | x86-64 `emit.rs:346-355`; AArch64 `emit.rs:457-466` | Keep completely per target. ModRM/REX, fixed-width AArch64 words, relocations, prologues, epilogues, and native ABI entry are not middle-layer policy. |

## Accepted policy and adapter boundary

### Shared policy

The shared layer owns one `CfgLowerCore` instantiated once for each concrete
adapter. It may depend on `CfgLowerContext`, CFG types, `agg_slots`,
`place_lower`, `storage_lower`, `aggregate_eq`, `byref_args`, and the shared
index/vreg types. It owns:

- the single CFG block walk and value memoization path;
- `CfgInstData` classification and dependency ordering;
- scalar versus complete aggregate slot representation;
- block-parameter routing and ascending logical slot order;
- edge-local value/cleanup work and deterministic successor order;
- place bounds-check ordering, address-versus-value decisions, and by-reference
  parameter classification;
- the language-level overflow, narrowing, division, comparison, and shift
  *requirements*;
- logical call and return plans, including flattened arguments, by-ref
  pointers, sret selection, stack-slot counts, and alignment requirements;
- allocation assignment, spill-slot selection, rewrite bookkeeping, and
  before/after spill insertion order;
- the one-shot pass and debug-plan sequencing; and
- shared invariants and diagnostics when a CFG or slot vector is malformed.

The shared layer must emit no `X86Inst`, `Aarch64Inst`, physical register, or
target encoding. It must not know that a result is in RAX, X0, or any other
register.

### Adapter interface

The new interface is an event boundary, not a universal backend API. The
implementation may use Rust traits and generic monomorphization, but the
interface must remain no larger than these six policy-facing operations in
addition to the existing state/leaf traits:

```text
emit_value(ValuePlan) -> ValueResult
emit_call(CallPlan) -> CallResult
emit_terminator(TerminatorPlan)
emit_intrinsic(IntrinsicPlan)
emit_checked_arithmetic(ArithmeticPlan)
emit_trap(TrapKind)
```

`ValuePlan`, `CallPlan`, `TerminatorPlan`, `IntrinsicPlan`, and
`ArithmeticPlan` are target-neutral records/enums containing vregs, logical
slot vectors, type width/signedness, symbol identities, logical labels, and
the already-decided language/ABI requirements. They contain no `Reg`, `Cond`,
target instruction, or raw CFG reference. Existing `SlotBackend`,
`PlaceLowerBackend`, `StorageLowerBackend`, `AggregateEqBackend`, and
`byref_args::lower_byref_arg_addr` and the existing storage/place traits remain
the narrow leaf interfaces for memory and aggregate operations; they are not
expanded to accept CFG policy.

The six operations are separate domain events, not one catch-all event enum:
`emit_value` covers the exhaustive `CfgValue`/aggregate materialization tree;
`emit_call` covers a fully classified logical call; `emit_terminator` covers
block topology, branches, returns, fallthrough, and edge work;
`emit_intrinsic` covers normalized runtime/intrinsic operations;
`emit_checked_arithmetic` covers arithmetic with its already-decided width,
signedness, and checked-overflow requirement; and `emit_trap` covers the
normalized trap selected by shared policy. Each plan is exhaustively matched
within its own operation, so adding a language-level case changes the shared
dispatcher and both adapters without creating a target-neutral instruction
enum.

The adapter may allocate target virtual registers and target label handles,
append target MIR, consult its register file and instruction set, choose
immediates, model flags, and emit target ABI instructions. The core owns the
logical labels, topology, vreg/slot identity, and event order; adapter-local
handles cannot create new CFG edges or reorder logical slots. An adapter may
not inspect `Cfg`, recompute type slot counts, choose sret versus register
return, reorder aggregate slots, classify an argument as by-reference, or
implement a second terminator/value dispatcher. Those operations are only
valid in the core. A backend adapter that needs a new semantic fact must extend
the shared plan, not read the CFG again.

This boundary is deliberately not a `Backend` trait with a generic `Inst`, nor
a single target-neutral instruction/event enum. There are two concrete
adapters—x86-64 and AArch64—and their MIR enums remain the instruction-selection
endpoint. A future third architecture can implement the same plans without
requiring a third generic backend in the core.

### Invariants at the boundary

Every migration slice must preserve these invariants:

1. A CFG value has one primary vreg mapping; aggregate values additionally
   have exactly `type_slot_count` logical slot vregs, including the `StrBuf`
   three-slot representation.
2. Logical slot zero and ascending memory layout follow ADR-0040 everywhere;
   target adapters cannot reverse a slot vector to compensate for a policy
   mistake. Any target-required emission order is an adapter-local operation
   after the plan is formed.
3. Every index projection is bounds-checked before address formation, including
   by-reference and aggregate paths.
4. `type_uses_sret_return` remains the single sret decision. Callers and
   callees consume the same `CallPlan`/`ReturnPlan` shape.
5. Logical call arguments and returns are fully classified before the adapter
   assigns registers or stack offsets. Hidden sret pointers and by-reference
   arguments are included in the plan, not rediscovered by instruction
   selection.
6. Block labels and inline labels remain disjoint and deterministic.
7. A plan is lowered once. `--emit lowering`, normal compilation, and debug
   views observe the same plan and do not invoke a presentation-specific
   lowerer.

## M3 migration slices and dependencies

RUE-820, RUE-821, RUE-822, RUE-823, and RUE-824 are parallel implementation
slices. Each depends on RUE-819 and may proceed independently; none is a
precondition for another. RUE-825 depends on all five and is the final
convergence slice. The issue titles and scopes below are the canonical Linear
scopes, mirrored here so an implementation cannot silently drift into a
different slice.

| Issue | Linear scope and endpoint | Required review/test gate |
| --- | --- | --- |
| RUE-820 — Share allocation sizing, index scaling, and bounds-check lowering | One shared algorithm decides allocation element/aggregate slot sizing, constant/dynamic index scaling, checked arithmetic, bounds conditions/trap edges, zero-width/overflow handling, and address-versus-value decisions. Backend hooks emit arithmetic, compare, and branch instructions only. | Array, string, pointer, zero-sized, maximum-length, and overflow tests on both backends; exhaustive hook removal must fail to compile; slot/index invariants, cross-target lowering/assembly, and differential oracle coverage. |
| RUE-821 — Share call ABI slot planning and argument materialization | One target-independent `CallPlan`/`ReturnPlan` covers hidden sret storage, argument modes and logical slots, zero-width/multi-slot values, address/value materialization, ordering, and return reconstruction. Physical registers, moves, stack offsets, and target ABI instructions remain local. Both backends consume the same plan and assert that every slot is handled. | Ordinary, method, intrinsic, runtime, by-ref, inout, aggregate, zero-sized, and stack-spill ABI cases on both backends; native execution plus cross-target assembly/encoding assertions. |
| RUE-822 — Share scalar and aggregate CFG value materialization | One exhaustive decision tree covers `CfgValue`, constants/coercions, loads/stores, flattened aggregate collection/reconstruction, by-reference preloads, comparison preparation, and width/signedness. Adapters retain target MIR and flags behavior and cannot choose slot counts or aggregate widths. | Cross-backend MIR/debug traces for every CFG value variant, aggregate and `StrBuf` cases, no-single-slot fallback assertions, native x86-64 and AArch64 execution, and cross-target lowering tests. |
| RUE-823 — Share CFG control-flow and terminator lowering | One shared path owns block/label mapping, conditional/unconditional branches, switch planning, return/trap/fallthrough, edge-local value/cleanup work, and deterministic order. Branch instructions, flags, and fixups remain local. | Diamond, loop, switch, trap, cleanup, and multi-return topology/trace comparisons on both backends, with identical successor topology before encoding; native execution and cross-target assembly/encoding tests. |
| RUE-824 — Consolidate register-allocation rewrite and spill orchestration | One implementation owns assignment, spill-slot allocation, rewrite bookkeeping, and before/after spill insertion sequencing. Hooks provide operand constraints, physical/scratch registers, implicit uses/defs, and concrete load/store instructions. | High-pressure, fixed-register, call-clobber, loop, and spill-heavy tests on both backends; liveness/regalloc/scheduling/stack-frame tests, deterministic debug output with intentional golden updates where needed, oracle-diff, release-mode, and native execution. |
| RUE-825 — Final convergence after RUE-820–824 | Move the remaining shared traversal/dispatch shell into the accepted layer, make each hook a documented target fact, remove superseded wrappers, and refresh the contribution checklist. Do not add MIR-container or other new abstractions here. | Full `scripts/rue test`, ADR/index validation, formatting, complete native x86-64 and AArch64 matrices, cross-target assembly/encoding CI, and the endpoint criteria below. |

No slice may change language semantics, the established Rue ABI, or the target
matrix as a refactor convenience. If a plan cannot express a behavior without
target facts, that behavior belongs to the adapter and the inventory must say
why.

## Explicit non-goals and target facts

The following remain per backend by design:

- `Reg`, `Cond`, `Inst`, operand encodings, and MIR display details;
- System V AMD64 versus AAPCS64 argument and return register sets, stack
  offsets, frame-pointer conventions, callee-saved registers, and syscall
  entry sequences;
- x86 FLAGS versus AArch64 NZCV, including overflow, carry, compare, shift,
  division, and narrowing sequences;
- x86 implicit RAX/RDX division operands and scratch-register constraints;
- AArch64 high-half multiplication and immediate encoding constraints;
- target-specific instruction selection, spill/reload instructions, liveness
  uses/defs/clobbers, schedule latency and flag facts;
- target peephole transformations and their flag-safety proofs;
- machine encoders, relocations, assembly formatting, prologues, epilogues,
  and native object/linker details; and
- target-specific runtime entry differences, including AArch64 Linux versus
  macOS behavior.

M3 does not attempt to make the backend files equal in line count, share
encoding code, genericize the MIR containers, introduce LLVM or another code
generator, redesign the Rue ABI, unify peephole optimizers, or create a third
generic backend. It also does not turn debug presentation into a second
semantic pipeline.

## Test and review obligations

Every slice must preserve the existing golden stages (`lowering`, `liveness`,
`regalloc`, `mir`, and `asm`) wherever target output is intentionally
unchanged. Where a plan changes textual formatting, the PR must explain the
format-only delta and retain machine-code and execution coverage.

The complete target obligation is:

- native x86-64 execution on the supported x86-64 CI platforms;
- native AArch64 execution on Linux AArch64 and macOS AArch64 CI;
- cross-target x86-64 and AArch64 lowering, assembly, and encoder tests from
  hosts that cannot natively execute the other target; and
- differential oracle and release-mode coverage for language/ABI behavior,
  with the isolated generated-oracle rerun rule in `AGENTS.md` applying to
  timeout-only local failures.

Reviewers must inspect both backend adapters for every slice and verify that
the shared core owns the semantic decision named in the diff. A passing output
corpus is not sufficient if a backend still reconstructs the decision from
the CFG.

## Objective completion criterion

RUE-825 is complete only when all of the following are measurable from the
current source and its tests:

1. The function inventory above has an owner for every non-test backend
   function; the remaining target-local functions are justified by a listed
   register, instruction, flag, immediate, encoding, or native-ABI fact.
2. There is exactly one implementation of CFG value/terminator routing,
   aggregate slot completeness, block-parameter routing, by-reference
   classification, and sret selection. Neither backend contains a second
   `lower_value`/`lower_terminator` policy dispatcher.
3. The shared adapter boundary has at most the six policy-facing operations
   specified above, exposes no generic target instruction or register type,
   and accepts normalized plans rather than raw CFG references.
4. The backend CFG lowerers consist of adapter state/leaf plumbing and target
   instruction selection; their remaining duplicated methods are justified by
   the target facts explicitly classified here. MIR-container genericization is
   not an RUE-825 completion requirement.
5. Native x86-64, native AArch64, cross-target assembly/encoding, oracle, and
   release-mode gates are green for the final source, and the documentation
   and generated ADR index validate.

The measurable endpoint is therefore ownership and semantic centralization,
not a percentage of lines deleted. Line count is retained only as audit
evidence and must not be used to force target-specific code through the
adapter.

## Risks and uncertainties

The principal risk is silent miscompilation when a policy decision moves from
one backend into a plan. The required native, cross-target, oracle, golden,
and release-mode gates are mandatory because a compile-only check cannot prove
the ABI and flag obligations.

Linear is the canonical source for the RUE-820 through RUE-825 titles,
descriptions, and dependency edges. This ADR mirrors those scopes and records
the shared ownership boundary; implementation PRs must keep their changes
within the corresponding Linear slice.
