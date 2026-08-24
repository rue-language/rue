//! Instruction scheduling for x86-64.
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

use super::mir::{Operand, Reg, X86Inst, X86Mir};
use crate::reg_class::RegClass;
use crate::schedule_core::{self, RegList, SchedulerAdapter};

struct X86Scheduler;

impl SchedulerAdapter for X86Scheduler {
    type Inst = X86Inst;
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

/// Get the latency for an x86-64 instruction.
///
/// These values are approximate for modern Intel/AMD processors.
/// They represent the number of cycles until the result is ready.
fn get_latency(inst: &X86Inst) -> u32 {
    match inst {
        // Register moves: 0-1 cycle (often eliminated by renaming)
        X86Inst::MovRR { .. } => 1,
        X86Inst::MovRI32 { .. } | X86Inst::MovRI64 { .. } => 1,

        // Memory loads: ~4 cycles (L1 cache hit)
        X86Inst::MovRM { .. }
        | X86Inst::MovRMIndexed { .. }
        | X86Inst::MovRMSib { .. }
        | X86Inst::Movzx8RM { .. }
        | X86Inst::NarrowLoadRM { .. }
        | X86Inst::NarrowLoadIndexed { .. } => 4,

        // Memory stores: 1 cycle to retire (store buffer)
        X86Inst::MovMR { .. }
        | X86Inst::MovMRIndexed { .. }
        | X86Inst::MovMRSib { .. }
        | X86Inst::MovMR8 { .. }
        | X86Inst::NarrowStoreMR { .. }
        | X86Inst::NarrowStoreIndexed { .. } => 1,

        // Simple arithmetic: 1 cycle
        X86Inst::AddRR { .. }
        | X86Inst::AddRR64 { .. }
        | X86Inst::AddRI { .. }
        | X86Inst::SubRR { .. }
        | X86Inst::SubRR64 { .. } => 1,

        // Multiply: 3 cycles
        X86Inst::ImulRR { .. }
        | X86Inst::ImulRR64 { .. }
        | X86Inst::MulR { .. }
        | X86Inst::Mul64R { .. } => 3,

        // Division: 20-80 cycles (highly variable)
        X86Inst::IdivR { .. }
        | X86Inst::DivR { .. }
        | X86Inst::Idiv64R { .. }
        | X86Inst::Div64R { .. } => 20,
        X86Inst::Cdq | X86Inst::Cqo => 1,

        // Negation: 1 cycle
        X86Inst::Neg { .. } | X86Inst::Neg64 { .. } => 1,

        // Logical operations: 1 cycle
        X86Inst::AndRR { .. }
        | X86Inst::OrRR { .. }
        | X86Inst::XorRR { .. }
        | X86Inst::And64RR { .. }
        | X86Inst::Or64RR { .. }
        | X86Inst::Xor64RR { .. }
        | X86Inst::XorRI { .. }
        | X86Inst::NotR { .. }
        | X86Inst::Not64R { .. } => 1,

        // Shifts: 1 cycle
        X86Inst::ShlRCl { .. }
        | X86Inst::Shl32RCl { .. }
        | X86Inst::ShlRI { .. }
        | X86Inst::Shl32RI { .. }
        | X86Inst::ShrRCl { .. }
        | X86Inst::Shr32RCl { .. }
        | X86Inst::ShrRI { .. }
        | X86Inst::Shr32RI { .. }
        | X86Inst::SarRCl { .. }
        | X86Inst::Sar32RCl { .. }
        | X86Inst::SarRI { .. }
        | X86Inst::Sar32RI { .. }
        | X86Inst::Shl { .. } => 1,

        // Comparisons: 1 cycle
        X86Inst::CmpRR { .. }
        | X86Inst::Cmp64RR { .. }
        | X86Inst::CmpRI { .. }
        | X86Inst::Cmp64RI { .. }
        | X86Inst::TestRR { .. }
        | X86Inst::Test64RR { .. } => 1,

        // Setcc: 1 cycle
        X86Inst::Sete { .. }
        | X86Inst::Setne { .. }
        | X86Inst::Setl { .. }
        | X86Inst::Setg { .. }
        | X86Inst::Setle { .. }
        | X86Inst::Setge { .. }
        | X86Inst::Setb { .. }
        | X86Inst::Seta { .. }
        | X86Inst::Setbe { .. }
        | X86Inst::Setae { .. } => 1,

        // Sign/zero extension: 1 cycle
        X86Inst::Movzx { .. }
        | X86Inst::Movsx8To64 { .. }
        | X86Inst::Movsx16To64 { .. }
        | X86Inst::Movsx32To64 { .. }
        | X86Inst::Movzx8To64 { .. }
        | X86Inst::Movzx16To64 { .. } => 1,

        // LEA: 1 cycle
        X86Inst::Lea { .. } => 1,

        // Stack operations: 1-4 cycles
        X86Inst::Push { .. } => 1,
        X86Inst::Pop { .. } => 4,

        // Calls: 5+ cycles (variable, includes return prediction)
        X86Inst::CallRel { .. } => 5,
        X86Inst::Syscall => 100, // Syscalls are very slow

        // Control flow (don't schedule across these)
        X86Inst::Jz { .. }
        | X86Inst::Jnz { .. }
        | X86Inst::Jo { .. }
        | X86Inst::Jno { .. }
        | X86Inst::Jb { .. }
        | X86Inst::Jae { .. }
        | X86Inst::Jbe { .. }
        | X86Inst::Jge { .. }
        | X86Inst::Jle { .. }
        | X86Inst::Jmp { .. }
        | X86Inst::Ret
        | X86Inst::Ud2 => 1,

        // Labels are not real instructions
        X86Inst::Label { .. } => 0,

        // String constants (pseudo-instructions)
        X86Inst::StringConstPtr { .. }
        | X86Inst::StringConstLen { .. }
        | X86Inst::StringConstCap { .. } => 1,
    }
}

/// Check if an instruction is a scheduling barrier.
///
/// Barriers prevent reordering across them. This includes:
/// - Control flow (branches, jumps, labels)
/// - Calls (clobber many registers)
/// - Return
fn is_barrier(inst: &X86Inst) -> bool {
    matches!(
        inst,
        X86Inst::Jz { .. }
            | X86Inst::Jnz { .. }
            | X86Inst::Jo { .. }
            | X86Inst::Jno { .. }
            | X86Inst::Jb { .. }
            | X86Inst::Jae { .. }
            | X86Inst::Jbe { .. }
            | X86Inst::Jge { .. }
            | X86Inst::Jle { .. }
            | X86Inst::Jmp { .. }
            | X86Inst::Label { .. }
            | X86Inst::CallRel { .. }
            | X86Inst::Syscall
            | X86Inst::Ret
            | X86Inst::Ud2
    )
}

/// Check if an instruction accesses memory.
fn accesses_memory(inst: &X86Inst) -> bool {
    matches!(
        inst,
        X86Inst::MovRM { .. }
            | X86Inst::MovMR { .. }
            | X86Inst::MovRMIndexed { .. }
            | X86Inst::MovMRIndexed { .. }
            | X86Inst::MovRMSib { .. }
            | X86Inst::MovMRSib { .. }
            | X86Inst::Movzx8RM { .. }
            | X86Inst::MovMR8 { .. }
            | X86Inst::NarrowLoadRM { .. }
            | X86Inst::NarrowStoreMR { .. }
            | X86Inst::NarrowLoadIndexed { .. }
            | X86Inst::NarrowStoreIndexed { .. }
            | X86Inst::Push { .. }
            | X86Inst::Pop { .. }
    )
}

/// Get registers read by an instruction (for dependency analysis).
pub(super) fn regs_read(inst: &X86Inst) -> RegList<Reg> {
    let mut result = RegList::new();

    let add_if_phys = |op: &Operand, regs: &mut RegList<Reg>| {
        if let Operand::Physical(reg) = op {
            regs.push(*reg);
        }
    };

    match inst {
        X86Inst::MovRI32 { .. } | X86Inst::MovRI64 { .. } => {}
        X86Inst::MovRR { src, .. } => add_if_phys(src, &mut result),
        X86Inst::MovRM { base, .. }
        | X86Inst::Movzx8RM { base, .. }
        | X86Inst::NarrowLoadRM { base, .. } => result.push(*base),
        X86Inst::MovMR { base, src, .. }
        | X86Inst::MovMR8 { base, src, .. }
        | X86Inst::NarrowStoreMR { base, src, .. } => {
            result.push(*base);
            add_if_phys(src, &mut result);
        }
        X86Inst::AddRR { dst, src }
        | X86Inst::AddRR64 { dst, src }
        | X86Inst::SubRR { dst, src }
        | X86Inst::SubRR64 { dst, src }
        | X86Inst::ImulRR { dst, src }
        | X86Inst::ImulRR64 { dst, src }
        | X86Inst::AndRR { dst, src }
        | X86Inst::OrRR { dst, src }
        | X86Inst::XorRR { dst, src }
        | X86Inst::And64RR { dst, src }
        | X86Inst::Or64RR { dst, src }
        | X86Inst::Xor64RR { dst, src } => {
            add_if_phys(dst, &mut result);
            add_if_phys(src, &mut result);
        }
        X86Inst::AddRI { dst, .. } | X86Inst::XorRI { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::Neg { dst }
        | X86Inst::Neg64 { dst }
        | X86Inst::NotR { dst }
        | X86Inst::Not64R { dst } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::ShlRCl { dst }
        | X86Inst::Shl32RCl { dst }
        | X86Inst::ShrRCl { dst }
        | X86Inst::Shr32RCl { dst }
        | X86Inst::SarRCl { dst }
        | X86Inst::Sar32RCl { dst } => {
            add_if_phys(dst, &mut result);
            result.push(Reg::Rcx); // CL is implicit
        }
        X86Inst::ShlRI { dst, .. }
        | X86Inst::Shl32RI { dst, .. }
        | X86Inst::ShrRI { dst, .. }
        | X86Inst::Shr32RI { dst, .. }
        | X86Inst::SarRI { dst, .. }
        | X86Inst::Sar32RI { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::IdivR { src }
        | X86Inst::DivR { src }
        | X86Inst::Idiv64R { src }
        | X86Inst::Div64R { src } => {
            add_if_phys(src, &mut result);
            result.push(Reg::Rax);
            result.push(Reg::Rdx);
        }
        X86Inst::MulR { src } | X86Inst::Mul64R { src } => {
            // One-operand MUL reads RAX (and src); RDX is write-only (high half)
            add_if_phys(src, &mut result);
            result.push(Reg::Rax);
        }
        X86Inst::Cdq | X86Inst::Cqo => result.push(Reg::Rax),
        X86Inst::CmpRR { src1, src2 }
        | X86Inst::Cmp64RR { src1, src2 }
        | X86Inst::TestRR { src1, src2 }
        | X86Inst::Test64RR { src1, src2 } => {
            add_if_phys(src1, &mut result);
            add_if_phys(src2, &mut result);
        }
        X86Inst::CmpRI { src, .. } | X86Inst::Cmp64RI { src, .. } => {
            add_if_phys(src, &mut result);
        }
        X86Inst::Sete { .. }
        | X86Inst::Setne { .. }
        | X86Inst::Setl { .. }
        | X86Inst::Setg { .. }
        | X86Inst::Setle { .. }
        | X86Inst::Setge { .. }
        | X86Inst::Setb { .. }
        | X86Inst::Seta { .. }
        | X86Inst::Setbe { .. }
        | X86Inst::Setae { .. } => {
            // These read flags, but we don't track flags explicitly
        }
        X86Inst::Movzx { src, .. }
        | X86Inst::Movsx8To64 { src, .. }
        | X86Inst::Movsx16To64 { src, .. }
        | X86Inst::Movsx32To64 { src, .. }
        | X86Inst::Movzx8To64 { src, .. }
        | X86Inst::Movzx16To64 { src, .. } => {
            add_if_phys(src, &mut result);
        }
        X86Inst::Push { src } => {
            add_if_phys(src, &mut result);
            result.push(Reg::Rsp); // Push reads RSP
        }
        X86Inst::Pop { .. } => {
            result.push(Reg::Rsp); // Pop reads RSP
        }
        X86Inst::Lea { base, .. } => result.push(*base),
        X86Inst::Shl { dst, count } => {
            add_if_phys(dst, &mut result);
            add_if_phys(count, &mut result);
        }
        X86Inst::MovRMIndexed { .. } | X86Inst::NarrowLoadIndexed { .. } => {
            // Pre-regalloc indexed load. The scheduler runs after regalloc,
            // which rewrites this variant; the virtual base has no physical
            // register to record here.
        }
        X86Inst::MovMRIndexed { src, .. } | X86Inst::NarrowStoreIndexed { src, .. } => {
            // Pre-regalloc indexed store. The scheduler runs after regalloc,
            // which rewrites this variant; the virtual base has no physical
            // register to record here.
            add_if_phys(src, &mut result);
        }
        X86Inst::MovRMSib { base, index, .. } => {
            add_if_phys(base, &mut result);
            add_if_phys(index, &mut result);
        }
        X86Inst::MovMRSib {
            base, index, src, ..
        } => {
            add_if_phys(base, &mut result);
            add_if_phys(index, &mut result);
            add_if_phys(src, &mut result);
        }
        X86Inst::StringConstPtr { .. }
        | X86Inst::StringConstLen { .. }
        | X86Inst::StringConstCap { .. }
        | X86Inst::CallRel { .. }
        | X86Inst::Syscall
        | X86Inst::Jz { .. }
        | X86Inst::Jnz { .. }
        | X86Inst::Jo { .. }
        | X86Inst::Jno { .. }
        | X86Inst::Jb { .. }
        | X86Inst::Jae { .. }
        | X86Inst::Jbe { .. }
        | X86Inst::Jge { .. }
        | X86Inst::Jle { .. }
        | X86Inst::Jmp { .. }
        | X86Inst::Label { .. }
        | X86Inst::Ret
        | X86Inst::Ud2 => {}
    }

    result
}

/// Get registers written by an instruction (for dependency analysis).
pub(super) fn regs_written(inst: &X86Inst) -> RegList<Reg> {
    let mut result = RegList::new();

    let add_if_phys = |op: &Operand, regs: &mut RegList<Reg>| {
        if let Operand::Physical(reg) = op {
            regs.push(*reg);
        }
    };

    match inst {
        X86Inst::MovRI32 { dst, .. }
        | X86Inst::MovRI64 { dst, .. }
        | X86Inst::MovRR { dst, .. }
        | X86Inst::MovRM { dst, .. }
        | X86Inst::Movzx8RM { dst, .. }
        | X86Inst::NarrowLoadRM { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::MovMR { .. } | X86Inst::MovMR8 { .. } | X86Inst::NarrowStoreMR { .. } => {}
        X86Inst::AddRR { dst, .. }
        | X86Inst::AddRR64 { dst, .. }
        | X86Inst::AddRI { dst, .. }
        | X86Inst::SubRR { dst, .. }
        | X86Inst::SubRR64 { dst, .. }
        | X86Inst::ImulRR { dst, .. }
        | X86Inst::ImulRR64 { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::Neg { dst } | X86Inst::Neg64 { dst } | X86Inst::XorRI { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::AndRR { dst, .. }
        | X86Inst::OrRR { dst, .. }
        | X86Inst::XorRR { dst, .. }
        | X86Inst::And64RR { dst, .. }
        | X86Inst::Or64RR { dst, .. }
        | X86Inst::Xor64RR { dst, .. }
        | X86Inst::NotR { dst }
        | X86Inst::Not64R { dst }
        | X86Inst::ShlRCl { dst }
        | X86Inst::Shl32RCl { dst }
        | X86Inst::ShlRI { dst, .. }
        | X86Inst::Shl32RI { dst, .. }
        | X86Inst::ShrRCl { dst }
        | X86Inst::Shr32RCl { dst }
        | X86Inst::ShrRI { dst, .. }
        | X86Inst::Shr32RI { dst, .. }
        | X86Inst::SarRCl { dst }
        | X86Inst::Sar32RCl { dst }
        | X86Inst::SarRI { dst, .. }
        | X86Inst::Sar32RI { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::IdivR { .. }
        | X86Inst::DivR { .. }
        | X86Inst::Idiv64R { .. }
        | X86Inst::Div64R { .. }
        | X86Inst::MulR { .. }
        | X86Inst::Mul64R { .. } => {
            result.push(Reg::Rax);
            result.push(Reg::Rdx);
        }
        X86Inst::Cdq | X86Inst::Cqo => result.push(Reg::Rdx),
        X86Inst::Sete { dst }
        | X86Inst::Setne { dst }
        | X86Inst::Setl { dst }
        | X86Inst::Setg { dst }
        | X86Inst::Setle { dst }
        | X86Inst::Setge { dst }
        | X86Inst::Setb { dst }
        | X86Inst::Seta { dst }
        | X86Inst::Setbe { dst }
        | X86Inst::Setae { dst } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::Movzx { dst, .. }
        | X86Inst::Movsx8To64 { dst, .. }
        | X86Inst::Movsx16To64 { dst, .. }
        | X86Inst::Movsx32To64 { dst, .. }
        | X86Inst::Movzx8To64 { dst, .. }
        | X86Inst::Movzx16To64 { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::Pop { dst } => {
            add_if_phys(dst, &mut result);
            result.push(Reg::Rsp); // Pop writes RSP
        }
        X86Inst::Push { .. } => {
            result.push(Reg::Rsp); // Push writes RSP
        }
        X86Inst::Lea { dst, .. } => add_if_phys(dst, &mut result),
        X86Inst::Shl { dst, .. } => add_if_phys(dst, &mut result),
        X86Inst::MovRMIndexed { dst, .. } | X86Inst::NarrowLoadIndexed { dst, .. } => {
            add_if_phys(dst, &mut result)
        }
        X86Inst::MovMRIndexed { .. } | X86Inst::NarrowStoreIndexed { .. } => {}
        X86Inst::MovRMSib { dst, .. } => add_if_phys(dst, &mut result),
        X86Inst::MovMRSib { .. } => {} // Store doesn't write to register (only memory)
        X86Inst::CallRel { .. } | X86Inst::Syscall => {
            // Clobbers handled separately via clobbers()
        }
        X86Inst::StringConstPtr { dst, .. }
        | X86Inst::StringConstLen { dst, .. }
        | X86Inst::StringConstCap { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::CmpRR { .. }
        | X86Inst::Cmp64RR { .. }
        | X86Inst::CmpRI { .. }
        | X86Inst::Cmp64RI { .. }
        | X86Inst::TestRR { .. }
        | X86Inst::Test64RR { .. }
        | X86Inst::Jz { .. }
        | X86Inst::Jnz { .. }
        | X86Inst::Jo { .. }
        | X86Inst::Jno { .. }
        | X86Inst::Jb { .. }
        | X86Inst::Jae { .. }
        | X86Inst::Jbe { .. }
        | X86Inst::Jge { .. }
        | X86Inst::Jle { .. }
        | X86Inst::Jmp { .. }
        | X86Inst::Label { .. }
        | X86Inst::Ret
        | X86Inst::Ud2 => {}
    }

    result
}

/// Check if an instruction writes to FLAGS.
///
/// Shared with the peephole pass, which must not change or drop a flags
/// write that a later reader observes (RUE-152).
pub(super) fn writes_flags(inst: &X86Inst) -> bool {
    matches!(
        inst,
        // Arithmetic (set OF, SF, ZF, CF, PF, AF)
        X86Inst::AddRR { .. }
            | X86Inst::AddRR64 { .. }
            | X86Inst::AddRI { .. }
            | X86Inst::SubRR { .. }
            | X86Inst::SubRR64 { .. }
            | X86Inst::ImulRR { .. }
            | X86Inst::ImulRR64 { .. }
            | X86Inst::IdivR { .. }
            | X86Inst::DivR { .. }
            | X86Inst::Idiv64R { .. }
            | X86Inst::Div64R { .. }
            | X86Inst::MulR { .. }
            | X86Inst::Mul64R { .. }
            | X86Inst::Neg { .. }
            | X86Inst::Neg64 { .. }
            // Logical (set SF, ZF, PF; clear OF, CF)
            | X86Inst::AndRR { .. }
            | X86Inst::OrRR { .. }
            | X86Inst::XorRR { .. }
            | X86Inst::And64RR { .. }
            | X86Inst::Or64RR { .. }
            | X86Inst::Xor64RR { .. }
            | X86Inst::XorRI { .. }
            // Shifts (set CF, and SF/ZF/PF for non-zero counts)
            | X86Inst::ShlRCl { .. }
            | X86Inst::Shl32RCl { .. }
            | X86Inst::ShlRI { .. }
            | X86Inst::Shl32RI { .. }
            | X86Inst::ShrRCl { .. }
            | X86Inst::Shr32RCl { .. }
            | X86Inst::ShrRI { .. }
            | X86Inst::Shr32RI { .. }
            | X86Inst::SarRCl { .. }
            | X86Inst::Sar32RCl { .. }
            | X86Inst::SarRI { .. }
            | X86Inst::Sar32RI { .. }
            | X86Inst::Shl { .. }
            // Comparison (set all flags)
            | X86Inst::CmpRR { .. }
            | X86Inst::Cmp64RR { .. }
            | X86Inst::CmpRI { .. }
            | X86Inst::Cmp64RI { .. }
            | X86Inst::TestRR { .. }
            | X86Inst::Test64RR { .. }
    )
}

/// Check if an instruction reads FLAGS.
///
/// Shared with the peephole pass (see [`writes_flags`]).
pub(super) fn reads_flags(inst: &X86Inst) -> bool {
    matches!(
        inst,
        // A runtime count of zero preserves FLAGS. Model variable-count
        // shifts as read/write dependencies so scheduling keeps both the
        // incoming FLAGS value and the shift's non-zero result ordered.
        X86Inst::Shl { .. }
            | X86Inst::ShlRCl { .. }
            | X86Inst::Shl32RCl { .. }
            | X86Inst::ShrRCl { .. }
            | X86Inst::Shr32RCl { .. }
            | X86Inst::SarRCl { .. }
            | X86Inst::Sar32RCl { .. }
        // Conditional set
            | X86Inst::Sete { .. }
            | X86Inst::Setne { .. }
            | X86Inst::Setl { .. }
            | X86Inst::Setg { .. }
            | X86Inst::Setle { .. }
            | X86Inst::Setge { .. }
            | X86Inst::Setb { .. }
            | X86Inst::Seta { .. }
            | X86Inst::Setbe { .. }
            | X86Inst::Setae { .. }
            // Conditional jumps
            | X86Inst::Jz { .. }
            | X86Inst::Jnz { .. }
            | X86Inst::Jo { .. }
            | X86Inst::Jno { .. }
            | X86Inst::Jb { .. }
            | X86Inst::Jae { .. }
            | X86Inst::Jbe { .. }
            | X86Inst::Jge { .. }
            | X86Inst::Jle { .. }
    )
}

#[cfg(test)]
fn build_dep_graph(
    instructions: &[X86Inst],
    start: usize,
    end: usize,
) -> Vec<schedule_core::SchedNode> {
    schedule_core::build_dep_graph(instructions, start, end, &X86Scheduler)
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
pub fn schedule(mir: &mut X86Mir) {
    schedule_core::schedule_instructions(mir.instructions_vec_mut(), &X86Scheduler);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vreg::LabelId;

    #[test]
    fn test_latency_values() {
        // Verify latency values are reasonable
        assert_eq!(
            get_latency(&X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Physical(Reg::Rbx),
            }),
            1
        );

        assert_eq!(
            get_latency(&X86Inst::MovRM {
                dst: Operand::Physical(Reg::Rax),
                base: Reg::Rbp,
                offset: -8,
            }),
            4
        );

        assert_eq!(
            get_latency(&X86Inst::ImulRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Physical(Reg::Rbx),
            }),
            3
        );

        assert_eq!(
            get_latency(&X86Inst::IdivR {
                src: Operand::Physical(Reg::Rbx),
            }),
            20
        );
    }

    #[test]
    fn test_barrier_detection() {
        assert!(is_barrier(&X86Inst::Jmp {
            label: LabelId::new(0)
        }));
        assert!(is_barrier(&X86Inst::Label {
            id: LabelId::new(0)
        }));
        assert!(is_barrier(&X86Inst::Ret));
        assert!(is_barrier(&X86Inst::call(0)));

        assert!(!is_barrier(&X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rbx),
        }));
    }

    #[test]
    fn test_clobber_orders_later_write() {
        // A later write to a clobbered register must depend on the clobberer
        // (WAW through the clobber set): if `mov rdx, 7` were hoisted above
        // CQO, the sign-extension would destroy the 7.
        let insts = vec![
            X86Inst::Cqo, // clobbers RDX
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rdx),
                imm: 7,
            },
        ];
        let nodes = build_dep_graph(&insts, 0, insts.len());
        assert!(
            nodes[1].deps.contains(&0),
            "write to clobbered RDX must depend on the clobbering CQO"
        );
    }

    #[test]
    fn test_clobber_orders_later_read() {
        // A later read of a clobbered register must depend on the clobberer
        // (RAW through the clobber set): reading RDX after CQO must observe
        // the sign-extension result, not the pre-CQO value.
        let insts = vec![
            X86Inst::Cqo, // clobbers RDX
            X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rbx),
                src: Operand::Physical(Reg::Rdx),
            },
        ];
        let nodes = build_dep_graph(&insts, 0, insts.len());
        assert!(
            nodes[1].deps.contains(&0),
            "read of clobbered RDX must depend on the clobbering CQO"
        );
    }

    #[test]
    fn test_memory_access_detection() {
        assert!(accesses_memory(&X86Inst::MovRM {
            dst: Operand::Physical(Reg::Rax),
            base: Reg::Rbp,
            offset: -8,
        }));
        assert!(accesses_memory(&X86Inst::MovMR {
            base: Reg::Rbp,
            offset: -8,
            src: Operand::Physical(Reg::Rax),
        }));
        assert!(accesses_memory(&X86Inst::Push {
            src: Operand::Physical(Reg::Rax),
        }));

        assert!(!accesses_memory(&X86Inst::AddRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rbx),
        }));
    }

    #[test]
    fn test_regs_read() {
        let regs = regs_read(&X86Inst::AddRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rbx),
        });
        assert!(regs.contains(&Reg::Rax)); // dst is both read and written
        assert!(regs.contains(&Reg::Rbx));

        let regs = regs_read(&X86Inst::MovRM {
            dst: Operand::Physical(Reg::Rax),
            base: Reg::Rbp,
            offset: -8,
        });
        assert!(regs.contains(&Reg::Rbp));
        assert!(!regs.contains(&Reg::Rax)); // dst is only written
    }

    #[test]
    fn test_regs_written() {
        let regs = regs_written(&X86Inst::MovRI32 {
            dst: Operand::Physical(Reg::Rax),
            imm: 42,
        });
        assert!(regs.contains(&Reg::Rax));

        let regs = regs_written(&X86Inst::IdivR {
            src: Operand::Physical(Reg::Rbx),
        });
        assert!(regs.contains(&Reg::Rax)); // quotient
        assert!(regs.contains(&Reg::Rdx)); // remainder
    }

    #[test]
    fn test_dependency_graph_raw() {
        // Test RAW (Read After Write) dependency
        // mov rax, 42
        // add rbx, rax  (reads rax, must come after)
        let instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            X86Inst::AddRR {
                dst: Operand::Physical(Reg::Rbx),
                src: Operand::Physical(Reg::Rax),
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
        // mov rax, 42
        // mov rax, 100  (writes rax, must come after first write)
        let instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 100,
            },
        ];

        let nodes = build_dep_graph(&instructions, 0, 2);
        assert!(nodes[1].deps.contains(&0));
    }

    #[test]
    fn test_dependency_graph_war() {
        // Test WAR (Write After Read) dependency
        // add rbx, rax  (reads rax)
        // mov rax, 42   (writes rax, must come after read)
        let instructions = vec![
            X86Inst::AddRR {
                dst: Operand::Physical(Reg::Rbx),
                src: Operand::Physical(Reg::Rax),
            },
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
        ];

        let nodes = build_dep_graph(&instructions, 0, 2);
        assert!(nodes[1].deps.contains(&0));
    }

    #[test]
    fn test_shift_count_move_and_use_stay_ordered() {
        let instructions = vec![
            X86Inst::MovRR {
                dst: Operand::Physical(Reg::Rcx),
                src: Operand::Physical(Reg::R11),
            },
            X86Inst::ShlRCl {
                dst: Operand::Physical(Reg::Rax),
            },
        ];

        let nodes = build_dep_graph(&instructions, 0, instructions.len());
        assert!(
            nodes[1].deps.contains(&0),
            "the RCX move must remain before the implicit-CL shift"
        );
    }

    #[test]
    fn test_runtime_shift_reads_and_writes_flags() {
        let shift = X86Inst::ShlRCl {
            dst: Operand::Physical(Reg::Rax),
        };
        let pre_regalloc_shift = X86Inst::Shl {
            dst: Operand::Physical(Reg::Rax),
            count: Operand::Physical(Reg::R11),
        };

        assert!(reads_flags(&shift));
        assert!(writes_flags(&shift));
        assert!(reads_flags(&pre_regalloc_shift));
        assert!(writes_flags(&pre_regalloc_shift));

        let instructions = vec![
            X86Inst::CmpRR {
                src1: Operand::Physical(Reg::Rax),
                src2: Operand::Physical(Reg::Rbx),
            },
            shift,
            X86Inst::Sete {
                dst: Operand::Physical(Reg::Rdx),
            },
        ];
        let nodes = build_dep_graph(&instructions, 0, instructions.len());
        assert!(nodes[1].deps.contains(&0));
        assert!(nodes[2].deps.contains(&1));
    }

    #[test]
    fn test_independent_instructions() {
        // Two independent instructions can be reordered
        // mov rax, 42
        // mov rbx, 100
        let instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 42,
            },
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rbx),
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
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rax),
                imm: 1,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
                imm: 2,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rax),
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
    fn test_rsp_dependency_push_add() {
        // Test that push and add rsp have correct dependencies
        // push rax      ; writes RSP
        // add rsp, 8    ; reads and writes RSP - must come after push
        let instructions = vec![
            X86Inst::Push {
                src: Operand::Physical(Reg::Rax),
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: 8,
            },
        ];

        let nodes = build_dep_graph(&instructions, 0, 2);
        // add rsp depends on push because push writes RSP and add reads RSP
        assert!(nodes[1].deps.contains(&0), "add rsp should depend on push");
    }

    #[test]
    fn test_rsp_dependency_multiple_pushes() {
        // Multiple pushes must be ordered
        // push rax      ; writes RSP
        // push rbx      ; reads and writes RSP - must come after first push
        let instructions = vec![
            X86Inst::Push {
                src: Operand::Physical(Reg::Rax),
            },
            X86Inst::Push {
                src: Operand::Physical(Reg::Rbx),
            },
        ];

        let nodes = build_dep_graph(&instructions, 0, 2);
        // second push depends on first push (RAW on RSP)
        assert!(
            nodes[1].deps.contains(&0),
            "second push should depend on first push"
        );
    }

    #[test]
    fn test_rsp_dependency_complex() {
        // Complex test with memory operations and RSP modifications
        // push rax         ; writes RSP, mem access
        // mov rbx, [rsp+0] ; reads RSP (indirectly), mem access
        // add rsp, 8       ; reads and writes RSP
        let instructions = vec![
            X86Inst::Push {
                src: Operand::Physical(Reg::Rax),
            },
            X86Inst::MovRM {
                dst: Operand::Physical(Reg::Rbx),
                base: Reg::Rsp,
                offset: 0,
            },
            X86Inst::AddRI {
                dst: Operand::Physical(Reg::Rsp),
                imm: 8,
            },
        ];

        let nodes = build_dep_graph(&instructions, 0, 3);
        // mov depends on push (memory ordering)
        assert!(
            nodes[1].deps.contains(&0),
            "mov should depend on push (memory)"
        );
        // add depends on push (RSP RAW)
        assert!(nodes[2].deps.contains(&0), "add rsp should depend on push");
        // add depends on mov (mov reads [rsp], add writes rsp)
        // Actually, MovRM doesn't "read" RSP in regs_read - it uses base as Reg, not Operand
        // But there should still be memory ordering
    }

    #[test]
    fn test_schedule_prioritizes_long_latency() {
        // Long-latency instruction should be scheduled early
        // When we have:
        // - imul rax, rbx (3 cycles)
        // - mov rcx, 42 (1 cycle, independent)
        // - add rdx, rax (depends on imul)
        // The scheduler should prefer: imul, mov, add
        // to hide the latency of imul
        let instructions = vec![
            X86Inst::MovRI32 {
                dst: Operand::Physical(Reg::Rcx),
                imm: 42,
            },
            X86Inst::ImulRR {
                dst: Operand::Physical(Reg::Rax),
                src: Operand::Physical(Reg::Rbx),
            },
            X86Inst::AddRR {
                dst: Operand::Physical(Reg::Rdx),
                src: Operand::Physical(Reg::Rax),
            },
        ];

        let mut nodes = build_dep_graph(&instructions, 0, 3);
        calculate_priorities(&mut nodes);

        // imul has higher priority because it's on the critical path (latency 3 + 1 = 4)
        // mov has priority 1
        assert!(nodes[1].priority > nodes[0].priority);
    }
}
