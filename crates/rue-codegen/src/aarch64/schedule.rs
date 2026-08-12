//! Instruction scheduling for AArch64.
//!
//! This module implements a list scheduling algorithm to optimize instruction order
//! for better performance. The scheduler runs after register allocation and reorders
//! instructions within basic blocks to:
//!
//! 1. Hide latencies by scheduling independent instructions between definition and use
//! 2. Reduce register pressure by keeping definitions close to their uses
//! 3. Improve instruction-level parallelism (ILP)
//!
//! # Algorithm
//!
//! The scheduler uses a standard list scheduling algorithm:
//! 1. Build a dependency graph from instructions
//! 2. Calculate priority for each instruction (critical path length)
//! 3. Greedily schedule highest-priority ready instructions
//!
//! # Constraints
//!
//! The scheduler maintains correctness by respecting:
//! - Data dependencies (RAW, WAR, WAW)
//! - Control flow (branches and labels stay in order)
//! - Memory ordering (conservative: all memory ops stay in order)
//! - Call conventions (arguments before call, results after)
//!
//! # Scope
//!
//! Currently schedules only within basic blocks (no cross-block motion).
//! Memory dependencies are handled conservatively (all loads/stores ordered).

use super::mir::{Aarch64Inst, Aarch64Mir, Operand, Reg};
use crate::reg_class::RegClass;
use crate::schedule_core::{self, RegList, SchedulerAdapter};

struct Aarch64Scheduler;

impl SchedulerAdapter for Aarch64Scheduler {
    type Inst = Aarch64Inst;
    type Reg = Reg;

    fn reg_class(&self, reg: Self::Reg) -> RegClass {
        reg.class()
    }

    fn reg_index(&self, reg: Self::Reg) -> usize {
        reg as usize
    }

    fn latency(&self, inst: &Self::Inst) -> u32 {
        get_latency(inst)
    }

    fn is_barrier(&self, inst: &Self::Inst) -> bool {
        is_barrier(inst)
    }

    fn accesses_memory(&self, inst: &Self::Inst) -> bool {
        accesses_memory(inst)
    }

    fn regs_read(&self, inst: &Self::Inst) -> RegList<Self::Reg> {
        regs_read(inst)
    }

    fn regs_written(&self, inst: &Self::Inst) -> RegList<Self::Reg> {
        regs_written(inst)
    }

    fn clobbers(&self, inst: &Self::Inst) -> &[Self::Reg] {
        inst.clobbers()
    }

    fn writes_flags(&self, inst: &Self::Inst) -> bool {
        writes_flags(inst)
    }

    fn reads_flags(&self, inst: &Self::Inst) -> bool {
        reads_flags(inst)
    }
}

/// Get the latency for an AArch64 instruction.
///
/// These values are approximate for Apple M-series and Cortex-A processors.
/// They represent the number of cycles until the result is ready.
fn get_latency(inst: &Aarch64Inst) -> u32 {
    match inst {
        // Register moves: 1 cycle (may be eliminated by renaming)
        Aarch64Inst::MovRR { .. } | Aarch64Inst::MovImm { .. } => 1,

        // Memory loads: 4 cycles (L1 cache hit)
        Aarch64Inst::Ldr { .. }
        | Aarch64Inst::LdrIndexed { .. }
        | Aarch64Inst::LdrIndexedOffset { .. }
        | Aarch64Inst::NarrowLoad { .. }
        | Aarch64Inst::NarrowLoadIndexed { .. }
        | Aarch64Inst::LdpPost { .. } => 4,

        // Memory stores: 1 cycle to retire (store buffer)
        Aarch64Inst::Str { .. }
        | Aarch64Inst::StrIndexed { .. }
        | Aarch64Inst::StrIndexedOffset { .. }
        | Aarch64Inst::NarrowStore { .. }
        | Aarch64Inst::NarrowStoreIndexed { .. }
        | Aarch64Inst::StpPre { .. } => 1,

        // Simple arithmetic: 1 cycle
        Aarch64Inst::AddRR { .. }
        | Aarch64Inst::AddsRR { .. }
        | Aarch64Inst::AddsRR64 { .. }
        | Aarch64Inst::AddImm { .. }
        | Aarch64Inst::SubRR { .. }
        | Aarch64Inst::SubsRR { .. }
        | Aarch64Inst::SubsRR64 { .. }
        | Aarch64Inst::SubImm { .. }
        | Aarch64Inst::Neg { .. }
        | Aarch64Inst::Negs { .. }
        | Aarch64Inst::Negs32 { .. } => 1,

        // Multiply: 3 cycles (integer multiply)
        Aarch64Inst::MulRR { .. }
        | Aarch64Inst::SmullRR { .. }
        | Aarch64Inst::UmullRR { .. }
        | Aarch64Inst::SmulhRR { .. }
        | Aarch64Inst::UmulhRR { .. }
        | Aarch64Inst::Msub { .. }
        | Aarch64Inst::Msub64 { .. } => 3,

        // Division: 12-20 cycles (highly variable)
        Aarch64Inst::SdivRR { .. }
        | Aarch64Inst::UdivRR { .. }
        | Aarch64Inst::Sdiv64RR { .. }
        | Aarch64Inst::Udiv64RR { .. } => 12,

        // Logical operations: 1 cycle
        Aarch64Inst::AndRR { .. }
        | Aarch64Inst::OrrRR { .. }
        | Aarch64Inst::EorRR { .. }
        | Aarch64Inst::EorImm { .. }
        | Aarch64Inst::MvnRR { .. }
        | Aarch64Inst::Mvn32RR { .. } => 1,

        // Shifts: 1 cycle
        Aarch64Inst::LslRR { .. }
        | Aarch64Inst::Lsl32RR { .. }
        | Aarch64Inst::LslImm { .. }
        | Aarch64Inst::Lsl32Imm { .. }
        | Aarch64Inst::LsrRR { .. }
        | Aarch64Inst::Lsr32RR { .. }
        | Aarch64Inst::Lsr32Imm { .. }
        | Aarch64Inst::Lsr64Imm { .. }
        | Aarch64Inst::AsrRR { .. }
        | Aarch64Inst::Asr32RR { .. }
        | Aarch64Inst::Asr32Imm { .. }
        | Aarch64Inst::Asr64Imm { .. } => 1,

        // Comparisons: 1 cycle
        Aarch64Inst::CmpRR { .. }
        | Aarch64Inst::Cmp64RR { .. }
        | Aarch64Inst::CmpImm { .. }
        | Aarch64Inst::TstRR { .. } => 1,

        // Conditional set: 1 cycle
        Aarch64Inst::Cset { .. } => 1,

        // Sign/zero extension: 1 cycle
        Aarch64Inst::Sxtb { .. }
        | Aarch64Inst::Sxth { .. }
        | Aarch64Inst::Sxtw { .. }
        | Aarch64Inst::Uxtb { .. }
        | Aarch64Inst::Uxth { .. } => 1,

        // Calls: 5+ cycles (variable, includes return prediction)
        Aarch64Inst::Bl { .. } => 5,

        // Control flow (don't schedule across these)
        Aarch64Inst::B { .. }
        | Aarch64Inst::BCond { .. }
        | Aarch64Inst::Bvs { .. }
        | Aarch64Inst::Bvc { .. }
        | Aarch64Inst::Cbz { .. }
        | Aarch64Inst::Cbnz { .. }
        | Aarch64Inst::Ret
        | Aarch64Inst::Brk => 1,

        // Labels are not real instructions
        Aarch64Inst::Label { .. } => 0,

        // String constants (pseudo-instructions)
        Aarch64Inst::StringConstPtr { .. }
        | Aarch64Inst::StringConstLen { .. }
        | Aarch64Inst::StringConstCap { .. } => 1,

        // Syscall instruction
        Aarch64Inst::Svc { .. } => 1,
    }
}

/// Check if an instruction is a scheduling barrier.
///
/// Barriers prevent reordering across them. This includes:
/// - Control flow (branches, jumps, labels)
/// - Calls (clobber many registers)
/// - Return
fn is_barrier(inst: &Aarch64Inst) -> bool {
    matches!(
        inst,
        Aarch64Inst::B { .. }
            | Aarch64Inst::BCond { .. }
            | Aarch64Inst::Bvs { .. }
            | Aarch64Inst::Bvc { .. }
            | Aarch64Inst::Cbz { .. }
            | Aarch64Inst::Cbnz { .. }
            | Aarch64Inst::Label { .. }
            | Aarch64Inst::Bl { .. }
            // A syscall is a barrier exactly like a call: the kernel reads its
            // argument registers and writes x0. Without this, the scheduler was
            // free to hoist the x0 result capture ABOVE `svc #0`, so every used
            // @syscall result read garbage. x86's barrier set includes Syscall;
            // this was pure backend drift. (RUE-129)
            | Aarch64Inst::Svc { .. }
            | Aarch64Inst::Ret
            | Aarch64Inst::Brk
    )
}

/// Check if an instruction accesses memory.
fn accesses_memory(inst: &Aarch64Inst) -> bool {
    matches!(
        inst,
        Aarch64Inst::Ldr { .. }
            | Aarch64Inst::Str { .. }
            | Aarch64Inst::LdrIndexed { .. }
            | Aarch64Inst::StrIndexed { .. }
            | Aarch64Inst::LdrIndexedOffset { .. }
            | Aarch64Inst::StrIndexedOffset { .. }
            | Aarch64Inst::StpPre { .. }
            | Aarch64Inst::LdpPost { .. }
            | Aarch64Inst::NarrowLoad { .. }
            | Aarch64Inst::NarrowStore { .. }
            | Aarch64Inst::NarrowLoadIndexed { .. }
            | Aarch64Inst::NarrowStoreIndexed { .. }
    )
}

/// Get registers read by an instruction (for dependency analysis).
pub(super) fn regs_read(inst: &Aarch64Inst) -> RegList<Reg> {
    let mut result = RegList::new();

    let add_if_phys = |op: &Operand, regs: &mut RegList<Reg>| {
        if let Operand::Physical(reg) = op {
            regs.push(*reg);
        }
    };

    match inst {
        Aarch64Inst::MovImm { .. } => {}
        Aarch64Inst::MovRR { src, .. } => add_if_phys(src, &mut result),
        Aarch64Inst::Ldr { base, .. } | Aarch64Inst::NarrowLoad { base, .. } => result.push(*base),
        Aarch64Inst::Str { src, base, .. } | Aarch64Inst::NarrowStore { src, base, .. } => {
            add_if_phys(src, &mut result);
            result.push(*base);
        }
        Aarch64Inst::AddRR { src1, src2, .. }
        | Aarch64Inst::AddsRR { src1, src2, .. }
        | Aarch64Inst::AddsRR64 { src1, src2, .. }
        | Aarch64Inst::SubRR { src1, src2, .. }
        | Aarch64Inst::SubsRR { src1, src2, .. }
        | Aarch64Inst::SubsRR64 { src1, src2, .. }
        | Aarch64Inst::MulRR { src1, src2, .. }
        | Aarch64Inst::SmullRR { src1, src2, .. }
        | Aarch64Inst::UmullRR { src1, src2, .. }
        | Aarch64Inst::SmulhRR { src1, src2, .. }
        | Aarch64Inst::UmulhRR { src1, src2, .. }
        | Aarch64Inst::SdivRR { src1, src2, .. }
        | Aarch64Inst::UdivRR { src1, src2, .. }
        | Aarch64Inst::Sdiv64RR { src1, src2, .. }
        | Aarch64Inst::Udiv64RR { src1, src2, .. }
        | Aarch64Inst::AndRR { src1, src2, .. }
        | Aarch64Inst::OrrRR { src1, src2, .. }
        | Aarch64Inst::EorRR { src1, src2, .. }
        | Aarch64Inst::LslRR { src1, src2, .. }
        | Aarch64Inst::Lsl32RR { src1, src2, .. }
        | Aarch64Inst::LsrRR { src1, src2, .. }
        | Aarch64Inst::Lsr32RR { src1, src2, .. }
        | Aarch64Inst::AsrRR { src1, src2, .. }
        | Aarch64Inst::Asr32RR { src1, src2, .. } => {
            add_if_phys(src1, &mut result);
            add_if_phys(src2, &mut result);
        }
        Aarch64Inst::AddImm { src, .. }
        | Aarch64Inst::SubImm { src, .. }
        | Aarch64Inst::LslImm { src, .. }
        | Aarch64Inst::Lsl32Imm { src, .. }
        | Aarch64Inst::Lsr32Imm { src, .. }
        | Aarch64Inst::Lsr64Imm { src, .. }
        | Aarch64Inst::Asr32Imm { src, .. }
        | Aarch64Inst::Asr64Imm { src, .. }
        | Aarch64Inst::EorImm { src, .. } => {
            add_if_phys(src, &mut result);
        }
        Aarch64Inst::Msub {
            src1, src2, src3, ..
        }
        | Aarch64Inst::Msub64 {
            src1, src2, src3, ..
        } => {
            add_if_phys(src1, &mut result);
            add_if_phys(src2, &mut result);
            add_if_phys(src3, &mut result);
        }
        Aarch64Inst::Neg { src, .. }
        | Aarch64Inst::Negs { src, .. }
        | Aarch64Inst::Negs32 { src, .. }
        | Aarch64Inst::MvnRR { src, .. }
        | Aarch64Inst::Mvn32RR { src, .. }
        | Aarch64Inst::Sxtb { src, .. }
        | Aarch64Inst::Sxth { src, .. }
        | Aarch64Inst::Sxtw { src, .. }
        | Aarch64Inst::Uxtb { src, .. }
        | Aarch64Inst::Uxth { src, .. } => {
            add_if_phys(src, &mut result);
        }
        Aarch64Inst::CmpRR { src1, src2 }
        | Aarch64Inst::Cmp64RR { src1, src2 }
        | Aarch64Inst::TstRR { src1, src2 } => {
            add_if_phys(src1, &mut result);
            add_if_phys(src2, &mut result);
        }
        Aarch64Inst::CmpImm { src, .. } => add_if_phys(src, &mut result),
        Aarch64Inst::Cbz { src, .. } | Aarch64Inst::Cbnz { src, .. } => {
            add_if_phys(src, &mut result);
        }
        Aarch64Inst::StpPre { src1, src2, .. } => {
            add_if_phys(src1, &mut result);
            add_if_phys(src2, &mut result);
            result.push(Reg::Sp); // Pre-indexed STP reads SP before writing
        }
        Aarch64Inst::LdpPost { .. } => {
            result.push(Reg::Sp); // Post-indexed LDP reads SP before writing
        }
        Aarch64Inst::LdrIndexed { .. }
        | Aarch64Inst::LdrIndexedOffset { .. }
        | Aarch64Inst::NarrowLoadIndexed { .. } => {
            // Pre-regalloc indexed load. The scheduler runs after regalloc,
            // which rewrites this variant; the virtual base has no physical
            // register to record here.
        }
        Aarch64Inst::StrIndexed { src, .. }
        | Aarch64Inst::StrIndexedOffset { src, .. }
        | Aarch64Inst::NarrowStoreIndexed { src, .. } => {
            // Pre-regalloc indexed store. The scheduler runs after regalloc,
            // which rewrites this variant; the virtual base has no physical
            // register to record here.
            add_if_phys(src, &mut result);
        }
        Aarch64Inst::Cset { .. }
        | Aarch64Inst::B { .. }
        | Aarch64Inst::BCond { .. }
        | Aarch64Inst::Bvs { .. }
        | Aarch64Inst::Bvc { .. }
        | Aarch64Inst::Label { .. }
        | Aarch64Inst::Bl { .. }
        | Aarch64Inst::Ret
        | Aarch64Inst::Brk
        | Aarch64Inst::Svc { .. }
        | Aarch64Inst::StringConstPtr { .. }
        | Aarch64Inst::StringConstLen { .. }
        | Aarch64Inst::StringConstCap { .. } => {}
    }

    result
}

/// Get registers written by an instruction (for dependency analysis).
pub(super) fn regs_written(inst: &Aarch64Inst) -> RegList<Reg> {
    let mut result = RegList::new();

    let add_if_phys = |op: &Operand, regs: &mut RegList<Reg>| {
        if let Operand::Physical(reg) = op {
            regs.push(*reg);
        }
    };

    match inst {
        Aarch64Inst::MovImm { dst, .. }
        | Aarch64Inst::MovRR { dst, .. }
        | Aarch64Inst::Ldr { dst, .. }
        | Aarch64Inst::AddRR { dst, .. }
        | Aarch64Inst::AddsRR { dst, .. }
        | Aarch64Inst::AddsRR64 { dst, .. }
        | Aarch64Inst::AddImm { dst, .. }
        | Aarch64Inst::SubRR { dst, .. }
        | Aarch64Inst::SubsRR { dst, .. }
        | Aarch64Inst::SubsRR64 { dst, .. }
        | Aarch64Inst::SubImm { dst, .. }
        | Aarch64Inst::MulRR { dst, .. }
        | Aarch64Inst::SmullRR { dst, .. }
        | Aarch64Inst::UmullRR { dst, .. }
        | Aarch64Inst::SmulhRR { dst, .. }
        | Aarch64Inst::UmulhRR { dst, .. }
        | Aarch64Inst::SdivRR { dst, .. }
        | Aarch64Inst::UdivRR { dst, .. }
        | Aarch64Inst::Sdiv64RR { dst, .. }
        | Aarch64Inst::Udiv64RR { dst, .. }
        | Aarch64Inst::Msub { dst, .. }
        | Aarch64Inst::Msub64 { dst, .. }
        | Aarch64Inst::Neg { dst, .. }
        | Aarch64Inst::Negs { dst, .. }
        | Aarch64Inst::Negs32 { dst, .. }
        | Aarch64Inst::AndRR { dst, .. }
        | Aarch64Inst::OrrRR { dst, .. }
        | Aarch64Inst::EorRR { dst, .. }
        | Aarch64Inst::EorImm { dst, .. }
        | Aarch64Inst::MvnRR { dst, .. }
        | Aarch64Inst::Mvn32RR { dst, .. }
        | Aarch64Inst::LslRR { dst, .. }
        | Aarch64Inst::Lsl32RR { dst, .. }
        | Aarch64Inst::LslImm { dst, .. }
        | Aarch64Inst::Lsl32Imm { dst, .. }
        | Aarch64Inst::LsrRR { dst, .. }
        | Aarch64Inst::Lsr32RR { dst, .. }
        | Aarch64Inst::Lsr32Imm { dst, .. }
        | Aarch64Inst::Lsr64Imm { dst, .. }
        | Aarch64Inst::AsrRR { dst, .. }
        | Aarch64Inst::Asr32RR { dst, .. }
        | Aarch64Inst::Asr32Imm { dst, .. }
        | Aarch64Inst::Asr64Imm { dst, .. }
        | Aarch64Inst::Cset { dst, .. }
        | Aarch64Inst::Sxtb { dst, .. }
        | Aarch64Inst::Sxth { dst, .. }
        | Aarch64Inst::Sxtw { dst, .. }
        | Aarch64Inst::Uxtb { dst, .. }
        | Aarch64Inst::Uxth { dst, .. }
        | Aarch64Inst::LdrIndexed { dst, .. }
        | Aarch64Inst::LdrIndexedOffset { dst, .. }
        | Aarch64Inst::NarrowLoad { dst, .. }
        | Aarch64Inst::NarrowLoadIndexed { dst, .. }
        | Aarch64Inst::StringConstPtr { dst, .. }
        | Aarch64Inst::StringConstLen { dst, .. }
        | Aarch64Inst::StringConstCap { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        Aarch64Inst::LdpPost { dst1, dst2, .. } => {
            add_if_phys(dst1, &mut result);
            add_if_phys(dst2, &mut result);
            result.push(Reg::Sp); // Post-indexed LDP writes SP
        }
        Aarch64Inst::Str { .. }
        | Aarch64Inst::StrIndexed { .. }
        | Aarch64Inst::StrIndexedOffset { .. }
        | Aarch64Inst::NarrowStore { .. }
        | Aarch64Inst::NarrowStoreIndexed { .. } => {
            // Writes to memory, not registers
        }
        Aarch64Inst::StpPre { .. } => {
            // Writes to memory AND writes SP (pre-indexed)
            result.push(Reg::Sp);
        }
        Aarch64Inst::CmpRR { .. }
        | Aarch64Inst::Cmp64RR { .. }
        | Aarch64Inst::CmpImm { .. }
        | Aarch64Inst::TstRR { .. } => {
            // Only sets flags
        }
        Aarch64Inst::Bl { .. } => {
            // Clobbers handled separately via clobbers()
        }
        Aarch64Inst::Svc { .. } => {
            // Clobbers handled separately via clobbers()
        }
        Aarch64Inst::B { .. }
        | Aarch64Inst::BCond { .. }
        | Aarch64Inst::Bvs { .. }
        | Aarch64Inst::Bvc { .. }
        | Aarch64Inst::Cbz { .. }
        | Aarch64Inst::Cbnz { .. }
        | Aarch64Inst::Label { .. }
        | Aarch64Inst::Ret
        | Aarch64Inst::Brk => {}
    }

    result
}

/// Check if an instruction writes to NZCV flags.
///
/// Shared with the peephole pass, which must not change flags that a later
/// reader observes (RUE-152).
pub(super) fn writes_flags(inst: &Aarch64Inst) -> bool {
    matches!(
        inst,
        // Flag-setting arithmetic
        Aarch64Inst::AddsRR { .. }
            | Aarch64Inst::AddsRR64 { .. }
            | Aarch64Inst::SubsRR { .. }
            | Aarch64Inst::SubsRR64 { .. }
            | Aarch64Inst::Negs { .. }
            | Aarch64Inst::Negs32 { .. }
            // Comparisons
            | Aarch64Inst::CmpRR { .. }
            | Aarch64Inst::Cmp64RR { .. }
            | Aarch64Inst::CmpImm { .. }
            | Aarch64Inst::TstRR { .. }
    )
}

/// Check if an instruction reads NZCV flags.
fn reads_flags(inst: &Aarch64Inst) -> bool {
    matches!(
        inst,
        // Conditional set
        Aarch64Inst::Cset { .. }
            // Conditional branches
            | Aarch64Inst::BCond { .. }
            | Aarch64Inst::Bvs { .. }
            | Aarch64Inst::Bvc { .. }
    )
}

#[cfg(test)]
fn build_dep_graph(
    instructions: &[Aarch64Inst],
    start: usize,
    end: usize,
) -> Vec<schedule_core::SchedNode> {
    schedule_core::build_dep_graph(instructions, start, end, &Aarch64Scheduler)
}

#[cfg(test)]
fn calculate_priorities(nodes: &mut [schedule_core::SchedNode]) {
    schedule_core::calculate_priorities(nodes);
}

#[cfg(test)]
fn schedule_block(nodes: &[schedule_core::SchedNode]) -> Vec<usize> {
    schedule_core::schedule_block(nodes)
}

/// Schedule instructions in the MIR.
///
/// This function reorders instructions within basic blocks to improve performance.
/// Control flow boundaries (branches, labels) are respected.
pub fn schedule(mir: &mut Aarch64Mir) {
    schedule_core::schedule_instructions(mir.instructions_vec_mut(), &Aarch64Scheduler);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vreg::LabelId;

    #[test]
    fn test_clobber_orders_later_write_and_read() {
        // Clobbers must be recorded as writes in the dep graph: a later write
        // to (WAW) or read of (RAW) a clobbered register must depend on the
        // clobberer. Today every aarch64 clobberer (Bl, Svc) is also a
        // scheduling barrier, so this is defense-in-depth at the dep-graph
        // level — build_dep_graph itself must stay correct for any future
        // non-barrier clobberer (and for the x86 backend, where CQO/IDIV
        // clobber without being barriers).
        let insts = vec![
            Aarch64Inst::Svc { imm: 0 }, // clobbers X0, X8, X16
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 7,
            },
            Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X1),
                src: Operand::Physical(Reg::X8),
            },
        ];
        let nodes = build_dep_graph(&insts, 0, insts.len());
        assert!(
            nodes[1].deps.contains(&0),
            "write to clobbered X0 must depend on the clobbering SVC"
        );
        assert!(
            nodes[2].deps.contains(&0),
            "read of clobbered X8 must depend on the clobbering SVC"
        );
    }

    #[test]
    fn test_latency_values() {
        // Verify latency values are reasonable
        assert_eq!(
            get_latency(&Aarch64Inst::MovRR {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X1),
            }),
            1
        );

        assert_eq!(
            get_latency(&Aarch64Inst::Ldr {
                dst: Operand::Physical(Reg::X0),
                base: Reg::Fp,
                offset: -8,
            }),
            4
        );

        assert_eq!(
            get_latency(&Aarch64Inst::MulRR {
                dst: Operand::Physical(Reg::X0),
                src1: Operand::Physical(Reg::X1),
                src2: Operand::Physical(Reg::X2),
            }),
            3
        );

        assert_eq!(
            get_latency(&Aarch64Inst::SdivRR {
                dst: Operand::Physical(Reg::X0),
                src1: Operand::Physical(Reg::X1),
                src2: Operand::Physical(Reg::X2),
            }),
            12
        );
    }

    #[test]
    fn test_barrier_detection() {
        assert!(is_barrier(&Aarch64Inst::B {
            label: LabelId::new(0)
        }));
        assert!(is_barrier(&Aarch64Inst::Label {
            id: LabelId::new(0)
        }));
        assert!(is_barrier(&Aarch64Inst::Ret));
        assert!(is_barrier(&Aarch64Inst::call(0)));

        assert!(!is_barrier(&Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Physical(Reg::X1),
        }));
    }

    #[test]
    fn test_memory_access_detection() {
        assert!(accesses_memory(&Aarch64Inst::Ldr {
            dst: Operand::Physical(Reg::X0),
            base: Reg::Fp,
            offset: -8,
        }));
        assert!(accesses_memory(&Aarch64Inst::Str {
            src: Operand::Physical(Reg::X0),
            base: Reg::Fp,
            offset: -8,
        }));

        assert!(!accesses_memory(&Aarch64Inst::AddRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        }));
    }

    #[test]
    fn test_regs_read() {
        let regs = regs_read(&Aarch64Inst::AddRR {
            dst: Operand::Physical(Reg::X0),
            src1: Operand::Physical(Reg::X1),
            src2: Operand::Physical(Reg::X2),
        });
        assert!(regs.contains(&Reg::X1));
        assert!(regs.contains(&Reg::X2));
        assert!(!regs.contains(&Reg::X0)); // dst is only written in AArch64

        let regs = regs_read(&Aarch64Inst::Ldr {
            dst: Operand::Physical(Reg::X0),
            base: Reg::Fp,
            offset: -8,
        });
        assert!(regs.contains(&Reg::Fp));
        assert!(!regs.contains(&Reg::X0));
    }

    #[test]
    fn test_regs_written() {
        let regs = regs_written(&Aarch64Inst::MovImm {
            dst: Operand::Physical(Reg::X0),
            imm: 42,
        });
        assert!(regs.contains(&Reg::X0));

        let regs = regs_written(&Aarch64Inst::LdpPost {
            dst1: Operand::Physical(Reg::Fp),
            dst2: Operand::Physical(Reg::Lr),
            offset: 16,
        });
        assert!(regs.contains(&Reg::Fp));
        assert!(regs.contains(&Reg::Lr));
    }

    #[test]
    fn test_dependency_graph_raw() {
        // Test RAW (Read After Write) dependency
        // mov x0, #42
        // add x1, x0, x2  (reads x0, must come after)
        let instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            Aarch64Inst::AddRR {
                dst: Operand::Physical(Reg::X1),
                src1: Operand::Physical(Reg::X0),
                src2: Operand::Physical(Reg::X2),
            },
        ];

        let nodes = build_dep_graph(&instructions, 0, 2);
        assert_eq!(nodes.len(), 2);
        assert!(nodes[0].deps.is_empty()); // First instruction has no deps
        assert!(nodes[1].deps.contains(&0)); // Second depends on first
    }

    #[test]
    fn test_dependency_graph_waw() {
        // Test WAW (Write After Write) dependency
        // mov x0, #42
        // mov x0, #100  (writes x0, must come after first write)
        let instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 100,
            },
        ];

        let nodes = build_dep_graph(&instructions, 0, 2);
        assert!(nodes[1].deps.contains(&0));
    }

    #[test]
    fn test_independent_instructions() {
        // Two independent instructions can be reordered
        // mov x0, #42
        // mov x1, #100
        let instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 42,
            },
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X1),
                imm: 100,
            },
        ];

        let nodes = build_dep_graph(&instructions, 0, 2);
        assert!(nodes[0].deps.is_empty());
        assert!(nodes[1].deps.is_empty());
    }

    #[test]
    fn test_schedule_respects_deps() {
        // Create a chain of dependencies and verify order is preserved
        let instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X0),
                imm: 1,
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 2,
            },
            Aarch64Inst::AddImm {
                dst: Operand::Physical(Reg::X0),
                src: Operand::Physical(Reg::X0),
                imm: 3,
            },
        ];

        let mut nodes = build_dep_graph(&instructions, 0, 3);
        calculate_priorities(&mut nodes);
        let order = schedule_block(&nodes);

        // Must maintain order: 0 -> 1 -> 2
        let pos_0 = order.iter().position(|&x| x == 0).unwrap();
        let pos_1 = order.iter().position(|&x| x == 1).unwrap();
        let pos_2 = order.iter().position(|&x| x == 2).unwrap();
        assert!(pos_0 < pos_1);
        assert!(pos_1 < pos_2);
    }

    #[test]
    fn test_schedule_prioritizes_long_latency() {
        // Long-latency instruction should be scheduled early
        // When we have:
        // - mul x0, x1, x2 (3 cycles)
        // - mov x3, #42 (1 cycle, independent)
        // - add x4, x0, x5 (depends on mul)
        // The scheduler should prefer: mul, mov, add
        // to hide the latency of mul
        let instructions = vec![
            Aarch64Inst::MovImm {
                dst: Operand::Physical(Reg::X3),
                imm: 42,
            },
            Aarch64Inst::MulRR {
                dst: Operand::Physical(Reg::X0),
                src1: Operand::Physical(Reg::X1),
                src2: Operand::Physical(Reg::X2),
            },
            Aarch64Inst::AddRR {
                dst: Operand::Physical(Reg::X4),
                src1: Operand::Physical(Reg::X0),
                src2: Operand::Physical(Reg::X5),
            },
        ];

        let mut nodes = build_dep_graph(&instructions, 0, 3);
        calculate_priorities(&mut nodes);

        // mul has higher priority because it's on the critical path (latency 3 + 1 = 4)
        // mov has priority 1
        assert!(nodes[1].priority > nodes[0].priority);
    }
}
