---
id: 0048
title: "Shared codegen middle layer (reduce x86-64/aarch64 backend duplication)"
status: proposal
tags: [codegen, architecture, backends, refactor, maintainability]
created: 2026-07-11
accepted:
implemented:
spec-sections: []
supersedes:
relates: ["RUE-250", "RUE-3", "RUE-31", "RUE-237", "RUE-248", "RUE-205", "RUE-45"]
---

# ADR-0048: Shared Codegen Middle Layer

## Status

Proposed — Fable, 2026-07-11, as the design pass RUE-250 asked for ("a real look
at the current structure before committing"). This ADR **maps** the duplication
and recommends where a shared middle layer pays off; it does **not** implement the
refactor. Executing any increment below is a separate, human-approved decision
(the refactor touches working codegen — see [Risks](#risks)).

## Summary

RUE-250 proposed factoring the target-independent parts of codegen into a shared
middle layer so each backend implements only the genuinely target-specific bits
(register set, instruction encoding, calling convention). A structural audit of
`crates/rue-codegen/src/{x86_64,aarch64}/` against the shared top-level modules
shows that **most of that middle layer already exists** and both backends already
plug into it through adapter traits. The crate is not two monolithic parallel
backends; it is a shared spine with per-target leaves.

The remaining duplication is concentrated in two places: (1) the two
`cfg_lower.rs` files, whose *routing skeleton* is near-identical but whose leaf
instruction emission is inlined rather than pushed through a trait, and (2) the
`mir.rs` instruction/operand **container** (as opposed to the instruction *set*,
which is legitimately per-target). The single largest, lowest-risk wins are
extracting the `cfg_lower.rs` terminator/value routing skeleton and making the MIR
container generic.

## Current state — what is already shared

A target-independent layer at `rue-codegen/src/` (≈8,450 LOC) is consumed by both
backends via traits. Each backend's `CfgLower` already implements five shared
traits (`SlotBackend`, `PlaceLowerBackend`, `StorageLowerBackend`,
`AggregateEqBackend`, `ByrefAddrBackend`).

| Shared module | LOC | Role | Seam |
|---|---|---|---|
| `regalloc.rs` | 2659 | Linear-scan, cost model, coalescing, spill-slot alloc, remat | generic `linear_scan*<Reg>`, `coalesce<Reg>` |
| `liveness.rs` | 984 | Backward dataflow, live ranges, loop info, pressure | `LivenessAdapter` |
| `stack_frame.rs` | 678 | Stack-frame / ABI analysis | operates on CFG |
| `cfg_lower.rs` | 610 | `CfgLowerContext` (type/slot/ABI/sret helpers), formatting | shared struct + free fns |
| `agg_slots.rs` | 569 | Aggregate slot routing (struct/array/enum/String, sret store) | `SlotBackend` |
| `place_lower.rs` | 511 | Place/lvalue addressing | `PlaceLowerBackend` |
| `schedule_core.rs` | 374 | Dependency DAG, list scheduling, BB splitting | `SchedulerAdapter` |
| `types.rs` | 423 | Type slot math | primitive |
| `storage_lower.rs` / `aggregate_eq.rs` / `byref_args.rs` | 147/121/129 | load/store, structural eq, byref args | traits |
| `codegen_pipeline.rs` | 136 | `prepare_mir` orchestration | generic `<Mir, Reg>` + closures |
| `index_map.rs` / `vreg.rs` | 220/87 | dense maps, `VReg`/`LabelId` | primitives |

So the algorithms RUE-250 named as candidates — CFG-lowering *helpers*, liveness,
scheduling, aggregate-slot routing — are **already single-copy**. The dataflow is
generic over a trait; only the per-instruction fact tables live in the backends.

## Residual duplication (per-file)

LOC is total; "code" excludes the trailing `#[cfg(test)]` module.

| File | x86 (code) | aarch64 (code) | Structural similarity | Est. still-shareable | What blocks it |
|---|---|---|---|---|---|
| `cfg_lower.rs` | 4295 (4237) | 4310 (4218) | routing skeleton near-identical; leaf emission + overflow strategy diverge | **~55–60%** | inlined leaf `Inst` construction; genuinely different overflow/flag logic |
| `emit.rs` | 4788 (2708) | 3703 (2202) | fundamentally different (ModRM/REX vs fixed 32-bit words) | ~0–5% | machine encoding is the definition of target-specific |
| `regalloc.rs` | 1646 (1181) | 1624 (1157) | driver near-identical (already thin over shared); `rewrite_inst` per-target | ~20% | `ALLOCATABLE_REGS`, spill/reload emit target instructions |
| `mir.rs` | 933 (876) | 1295 (1198) | `Mir` container + `Operand` identical shape; `Inst`/`Reg` specific | ~20–25% | instruction set (91 vs 73 variants), `Reg` file, encodings |
| `schedule.rs` | 1013 (614) | 801 (495) | adapter glue byte-identical; fact tables category-parallel | ~30% | latency numbers + variant names per-target |
| `liveness.rs` | 724 (449) | 664 (391) | adapter glue identical; uses/defs category-parallel, per-variant | ~30% | uses/defs bound to each `Inst` enum |
| `peephole.rs` | 1001 (~300) | 978 (~300) | standalone `optimize`; target pattern matches | ~10–15% | patterns are per-ISA |

### The `cfg_lower.rs` skeleton is copied by hand

`lower_terminator` is essentially line-for-line identical between backends
(`x86_64/cfg_lower.rs:3759` vs `aarch64/cfg_lower.rs:3754`): same arm order
(`Goto`/`Branch`/`Switch`/`Return`/`Unreachable`/`None`), same aggregate-vs-scalar
block-param routing, same fall-through elision, same Switch lowering, same sret
return path via shared `agg_slots::store_slots_to_sret`. **The RUE-ticket comments
(RUE-237, RUE-92, RUE-118) are copied verbatim in both files.** The only leaf
differences are instruction names (`X86Inst::Jmp/CmpRI+Jz` vs
`Aarch64Inst::B/Cbz`) and one real semantic delta (x86 iterates return slots
`.rev()` for Rax-scratch avoidance; aarch64 forward). `lower_value`
(`x86_64/cfg_lower.rs:827` vs `aarch64/cfg_lower.rs:617`) shares the same preamble
and arm order for `Const`/`BoolConst`/`StringConst`/`Param`/block-param/aggregate
arms.

That hand-kept lockstep — not the raw line count — is the real cost RUE-250 should
quantify: it is the mechanism behind the divergence-by-omission bug class (e.g.
RUE-31's aarch64-only comparison-width bug, RUE-237's aarch64-only enum ICE where
the x86 fix did not share the gate). Every codegen fix in the shared skeleton is
applied twice today and can be forgotten once.

### Where the two `cfg_lower.rs` files genuinely diverge

The ~40% that is *not* shareable is real target-specific logic, chiefly overflow
and flags: x86 uses the hardware overflow flag with a single `emit_overflow_check`
+ `Jo` (`x86_64/cfg_lower.rs:3290`), while aarch64 synthesizes overflow and has
four routines `emit_overflow_check_{add,sub,mul,neg}`
(`aarch64/cfg_lower.rs:2957/2992/3026/3230`; the `mul` case is ~140 lines using
`smulh`). Comparisons are cmp+`setcc` (closure-based, `x86_64:3634`) vs cmp+`cset`
(`Cond`-based, `aarch64:3575`). This belongs in the backends.

## Recommendation

Rank the increments by payoff/risk; do them independently, each behind the
existing oracle-diff (RUE-205) + release-mode CI (RUE-45) safety net, one PR each,
with the differential oracle green before and after.

1. **Extract the `cfg_lower.rs` terminator + value routing skeleton (highest
   payoff).** Turn `lower_terminator` (~240 lines/backend, ~95% identical) into a
   shared generic driver that calls an enriched instruction-builder trait
   (`emit_jump`, `emit_cond_branch_zero`, `emit_cmp_imm`, `emit_trap`,
   `emit_call`) — the backends already expose most of these as trait methods. Do
   the scalar/const/param/string/block-param/aggregate arms of `lower_value` the
   same way. Realistic ~55–60% of two ~4,200-line files, and it directly closes
   the divergence-by-omission gap for terminators.
2. **Make the MIR container generic (`Mir<Inst>`) with a shared `Operand<Reg>`.**
   The container methods (`new`/`push`/`alloc_vreg`/`alloc_label`/`instructions*`/
   `into_instructions`) are the same names with the same bodies
   (`x86_64/mir.rs:739` ≡ `aarch64/mir.rs:1043`); `Operand` is already identical.
   Mechanical, ~150 lines/backend, low risk.
3. **(Optional, modest) Declarative fact tables for liveness/schedule.** A
   per-variant "operand roles / instruction-class" descriptor could let a shared
   helper derive uses/defs/latency, but it is bounded by each `Inst` enum;
   probably not worth more than ~30% and lower priority than 1–2.

**Explicitly keep per-target:** `emit.rs` (encoding), `regalloc.rs::rewrite_inst`
+ `ALLOCATABLE_REGS` (register file + spill emission), `peephole.rs` patterns, the
`Inst`/`Reg`/`Cond` enum definitions, and the overflow/flag portions of
`cfg_lower.rs`. These are the true register-set / encoding / calling-convention
core, and forcing them into a shared abstraction would add indirection without
removing real duplication.

## Risks

- The refactor touches working codegen; a botched extraction is a silent
  miscompile. Mitigation: the differential oracle (RUE-205) and release-mode CI
  (RUE-45) now exist precisely to catch this — neither did when RUE-250 was filed,
  which is why it is safer to attempt now. Gate each increment on oracle-green.
- Over-abstracting the leaf emission (pushing genuinely target-specific overflow
  logic behind a trait) trades duplication for indirection and can *hide* the
  x86-vs-aarch64 semantic differences that reviewers need to see. Increment 1 is
  scoped to the routing skeleton, not the flag model, for this reason.
- aarch64 is not runnable on an x86 dev box (ADR-0034 / RUE-36); correctness of
  the aarch64 side rides on CI's native arm64 + macOS jobs. Land increments where
  CI can execute both backends.

## Decision needed

Whether to execute increments 1–2 (and file them as tracked implementation
issues), or to accept the current architecture as "good enough" and instead invest
only in the cheaper divergence-guard (a shared golden test that fails when the two
`lower_terminator`s drift). This ADR does not decide that — it hands Steve the map.
