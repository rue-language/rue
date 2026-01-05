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

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use super::liveness;
use super::mir::{Operand, Reg, X86Inst, X86Mir};
use crate::vreg::LabelId;

/// A node in the scheduling dependency graph.
#[derive(Debug)]
struct SchedNode {
    /// Index of this instruction in the original sequence.
    inst_idx: usize,
    /// Instructions this depends on (must execute before this).
    deps: Vec<usize>,
    /// Instructions that depend on this (must execute after this).
    users: Vec<usize>,
    /// Scheduling priority (higher = schedule earlier).
    priority: u32,
    /// Latency in cycles until result is ready.
    latency: u32,
}

impl SchedNode {
    fn new(inst_idx: usize, latency: u32) -> Self {
        Self {
            inst_idx,
            deps: Vec::new(),
            users: Vec::new(),
            priority: 0,
            latency,
        }
    }
}

/// A ready instruction with its priority, for the scheduling queue.
#[derive(Debug, Eq, PartialEq)]
struct ReadyInst {
    priority: u32,
    idx: usize,
}

impl Ord for ReadyInst {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first, break ties by lower index (original order)
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.idx.cmp(&self.idx))
    }
}

impl PartialOrd for ReadyInst {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
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
        X86Inst::MovRM { .. } | X86Inst::MovRMIndexed { .. } | X86Inst::MovRMSib { .. } => 4,

        // Memory stores: 1 cycle to retire (store buffer)
        X86Inst::MovMR { .. } | X86Inst::MovMRIndexed { .. } | X86Inst::MovMRSib { .. } => 1,

        // Simple arithmetic: 1 cycle
        X86Inst::AddRR { .. }
        | X86Inst::AddRR64 { .. }
        | X86Inst::AddRI { .. }
        | X86Inst::SubRR { .. }
        | X86Inst::SubRR64 { .. } => 1,

        // Multiply: 3 cycles
        X86Inst::ImulRR { .. } | X86Inst::ImulRR64 { .. } => 3,

        // Division: 20-80 cycles (highly variable)
        X86Inst::IdivR { .. } | X86Inst::DivR { .. } => 20,
        X86Inst::Cdq => 1,

        // Negation: 1 cycle
        X86Inst::Neg { .. } | X86Inst::Neg64 { .. } => 1,

        // Logical operations: 1 cycle
        X86Inst::AndRR { .. }
        | X86Inst::OrRR { .. }
        | X86Inst::XorRR { .. }
        | X86Inst::XorRI { .. }
        | X86Inst::NotR { .. } => 1,

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
        | X86Inst::TestRR { .. } => 1,

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
        | X86Inst::Ret => 1,

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
            | X86Inst::Push { .. }
            | X86Inst::Pop { .. }
    )
}

/// Get registers read by an instruction (for dependency analysis).
fn regs_read(inst: &X86Inst) -> Vec<Reg> {
    let mut result = Vec::new();

    let add_if_phys = |op: &Operand, vec: &mut Vec<Reg>| {
        if let Operand::Physical(reg) = op {
            vec.push(*reg);
        }
    };

    match inst {
        X86Inst::MovRI32 { .. } | X86Inst::MovRI64 { .. } => {}
        X86Inst::MovRR { src, .. } => add_if_phys(src, &mut result),
        X86Inst::MovRM { base, .. } => result.push(*base),
        X86Inst::MovMR { base, src, .. } => {
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
        | X86Inst::XorRR { dst, src } => {
            add_if_phys(dst, &mut result);
            add_if_phys(src, &mut result);
        }
        X86Inst::AddRI { dst, .. } | X86Inst::XorRI { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::Neg { dst } | X86Inst::Neg64 { dst } | X86Inst::NotR { dst } => {
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
        X86Inst::IdivR { src } | X86Inst::DivR { src } => {
            add_if_phys(src, &mut result);
            result.push(Reg::Rax);
            result.push(Reg::Rdx);
        }
        X86Inst::Cdq => result.push(Reg::Rax),
        X86Inst::CmpRR { src1, src2 }
        | X86Inst::Cmp64RR { src1, src2 }
        | X86Inst::TestRR { src1, src2 } => {
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
        X86Inst::MovRMIndexed { .. } => {
            // base is VReg, handled separately
        }
        X86Inst::MovMRIndexed { src, .. } => {
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
        _ => {}
    }

    result
}

/// Get registers written by an instruction (for dependency analysis).
fn regs_written(inst: &X86Inst) -> Vec<Reg> {
    let mut result = Vec::new();

    let add_if_phys = |op: &Operand, vec: &mut Vec<Reg>| {
        if let Operand::Physical(reg) = op {
            vec.push(*reg);
        }
    };

    match inst {
        X86Inst::MovRI32 { dst, .. }
        | X86Inst::MovRI64 { dst, .. }
        | X86Inst::MovRR { dst, .. }
        | X86Inst::MovRM { dst, .. } => {
            add_if_phys(dst, &mut result);
        }
        X86Inst::MovMR { .. } => {}
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
        | X86Inst::NotR { dst }
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
        X86Inst::IdivR { .. } | X86Inst::DivR { .. } => {
            result.push(Reg::Rax);
            result.push(Reg::Rdx);
        }
        X86Inst::Cdq => result.push(Reg::Rdx),
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
        X86Inst::MovRMIndexed { dst, .. } => add_if_phys(dst, &mut result),
        X86Inst::MovMRIndexed { .. } => {}
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
        _ => {}
    }

    result
}

/// Check if an instruction writes to FLAGS.
fn writes_flags(inst: &X86Inst) -> bool {
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
            | X86Inst::Neg { .. }
            | X86Inst::Neg64 { .. }
            // Logical (set SF, ZF, PF; clear OF, CF)
            | X86Inst::AndRR { .. }
            | X86Inst::OrRR { .. }
            | X86Inst::XorRR { .. }
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
    )
}

/// Check if an instruction reads FLAGS.
fn reads_flags(inst: &X86Inst) -> bool {
    matches!(
        inst,
        // Conditional set
        X86Inst::Sete { .. }
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

/// Build the dependency graph for a basic block of instructions.
fn build_dep_graph(instructions: &[X86Inst], start: usize, end: usize) -> Vec<SchedNode> {
    let block_len = end - start;
    let mut nodes: Vec<SchedNode> = instructions[start..end]
        .iter()
        .enumerate()
        .map(|(i, inst)| SchedNode::new(i, get_latency(inst)))
        .collect();

    // Track last writer of each register
    let mut last_writer: HashMap<Reg, usize> = HashMap::new();
    // Track last readers of each register (for WAR dependencies)
    let mut last_readers: HashMap<Reg, Vec<usize>> = HashMap::new();
    // Track last memory access (conservative)
    let mut last_memory_access: Option<usize> = None;
    // Track last FLAGS writer and readers
    let mut last_flags_writer: Option<usize> = None;
    let mut last_flags_readers: Vec<usize> = Vec::new();

    for i in 0..block_len {
        let inst = &instructions[start + i];
        let reads = regs_read(inst);
        let writes = regs_written(inst);

        // RAW (Read After Write): this instruction reads what another wrote
        for reg in &reads {
            if let Some(&writer) = last_writer.get(reg) {
                if !nodes[i].deps.contains(&writer) {
                    nodes[i].deps.push(writer);
                    nodes[writer].users.push(i);
                }
            }
        }

        // WAW (Write After Write): this instruction writes what another wrote
        for reg in &writes {
            if let Some(&prev_writer) = last_writer.get(reg) {
                if !nodes[i].deps.contains(&prev_writer) {
                    nodes[i].deps.push(prev_writer);
                    nodes[prev_writer].users.push(i);
                }
            }
        }

        // WAR (Write After Read): this instruction writes what another read
        for reg in &writes {
            if let Some(readers) = last_readers.get(reg) {
                for &reader in readers {
                    if reader != i && !nodes[i].deps.contains(&reader) {
                        nodes[i].deps.push(reader);
                        nodes[reader].users.push(i);
                    }
                }
            }
        }

        // FLAGS dependencies
        // RAW: instruction reads flags written by another
        if reads_flags(inst) {
            if let Some(writer) = last_flags_writer {
                if !nodes[i].deps.contains(&writer) {
                    nodes[i].deps.push(writer);
                    nodes[writer].users.push(i);
                }
            }
        }

        // WAW: instruction writes flags written by another
        if writes_flags(inst) {
            if let Some(prev_writer) = last_flags_writer {
                if !nodes[i].deps.contains(&prev_writer) {
                    nodes[i].deps.push(prev_writer);
                    nodes[prev_writer].users.push(i);
                }
            }
        }

        // WAR: instruction writes flags read by another
        if writes_flags(inst) {
            for &reader in &last_flags_readers {
                if reader != i && !nodes[i].deps.contains(&reader) {
                    nodes[i].deps.push(reader);
                    nodes[reader].users.push(i);
                }
            }
        }

        // Memory dependencies (conservative: order all memory accesses)
        if accesses_memory(inst) {
            if let Some(prev) = last_memory_access {
                if !nodes[i].deps.contains(&prev) {
                    nodes[i].deps.push(prev);
                    nodes[prev].users.push(i);
                }
            }
            last_memory_access = Some(i);
        }

        // Clobber dependencies
        for &clobbered in inst.clobbers() {
            // This instruction clobbers the register, so it must come after any readers
            if let Some(readers) = last_readers.get(&clobbered) {
                for &reader in readers {
                    if reader != i && !nodes[i].deps.contains(&reader) {
                        nodes[i].deps.push(reader);
                        nodes[reader].users.push(i);
                    }
                }
            }
            // And after the last writer
            if let Some(&writer) = last_writer.get(&clobbered) {
                if !nodes[i].deps.contains(&writer) {
                    nodes[i].deps.push(writer);
                    nodes[writer].users.push(i);
                }
            }
        }

        // Update tracking
        for reg in writes {
            last_writer.insert(reg, i);
            last_readers.remove(&reg);
        }
        for reg in reads {
            last_readers.entry(reg).or_default().push(i);
        }

        // Update FLAGS tracking
        if writes_flags(inst) {
            last_flags_writer = Some(i);
            last_flags_readers.clear();
        }
        if reads_flags(inst) {
            last_flags_readers.push(i);
        }
    }

    nodes
}

/// Calculate priority for each node (critical path length to exit).
fn calculate_priorities(nodes: &mut [SchedNode]) {
    let mut memo: HashMap<usize, u32> = HashMap::new();

    fn dfs(nodes: &[SchedNode], idx: usize, memo: &mut HashMap<usize, u32>) -> u32 {
        if let Some(&cached) = memo.get(&idx) {
            return cached;
        }

        let node = &nodes[idx];
        let max_user = node
            .users
            .iter()
            .map(|&u| dfs(nodes, u, memo))
            .max()
            .unwrap_or(0);

        let result = node.latency + max_user;
        memo.insert(idx, result);
        result
    }

    for i in 0..nodes.len() {
        let priority = dfs(nodes, i, &mut memo);
        nodes[i].priority = priority;
    }
}

/// Schedule instructions within a basic block using list scheduling.
fn schedule_block(nodes: &[SchedNode]) -> Vec<usize> {
    if nodes.is_empty() {
        return Vec::new();
    }

    let mut scheduled = Vec::with_capacity(nodes.len());
    let mut completed: HashSet<usize> = HashSet::new();
    let mut ready: BinaryHeap<ReadyInst> = BinaryHeap::new();

    // Seed with instructions that have no dependencies
    for (idx, node) in nodes.iter().enumerate() {
        if node.deps.is_empty() {
            ready.push(ReadyInst {
                priority: node.priority,
                idx,
            });
        }
    }

    while let Some(ReadyInst { idx, .. }) = ready.pop() {
        if completed.contains(&idx) {
            continue;
        }

        scheduled.push(idx);
        completed.insert(idx);

        // Add newly ready instructions
        for &user in &nodes[idx].users {
            if !completed.contains(&user) {
                let all_deps_complete = nodes[user].deps.iter().all(|d| completed.contains(d));
                if all_deps_complete {
                    ready.push(ReadyInst {
                        priority: nodes[user].priority,
                        idx: user,
                    });
                }
            }
        }
    }

    scheduled
}

/// Schedule instructions in the MIR.
///
/// This function reorders instructions within basic blocks to improve performance.
/// Control flow boundaries (branches, labels) are respected.
pub fn schedule(mir: &mut X86Mir) {
    let instructions = mir.instructions_vec_mut();
    if instructions.len() < 3 {
        // Not worth scheduling very small functions
        return;
    }

    // Find basic block boundaries
    let mut block_starts = vec![0usize];
    for (i, inst) in instructions.iter().enumerate() {
        if is_barrier(inst) {
            // The barrier is the last instruction of the current block
            // Next block starts after the barrier
            if i + 1 < instructions.len() {
                block_starts.push(i + 1);
            }
        }
    }
    block_starts.push(instructions.len());

    // Schedule each basic block
    let mut new_instructions = Vec::with_capacity(instructions.len());

    for window in block_starts.windows(2) {
        let start = window[0];
        let end = window[1];

        // Don't schedule blocks that are too small or end with a barrier at start
        // Also skip if block has only 1-2 instructions
        if end - start <= 2 {
            for i in start..end {
                new_instructions.push(instructions[i].clone());
            }
            continue;
        }

        // Check if block ends with barrier, and if so, exclude it from scheduling
        let last_is_barrier = is_barrier(&instructions[end - 1]);
        let sched_end = if last_is_barrier { end - 1 } else { end };

        if sched_end - start <= 2 {
            // Not enough instructions to schedule
            for i in start..end {
                new_instructions.push(instructions[i].clone());
            }
            continue;
        }

        // Build dependency graph and schedule
        let mut nodes = build_dep_graph(instructions, start, sched_end);
        calculate_priorities(&mut nodes);
        let order = schedule_block(&nodes);

        // Emit instructions in scheduled order
        for &idx in &order {
            new_instructions.push(instructions[start + idx].clone());
        }

        // Emit the barrier at the end if there was one
        if last_is_barrier {
            new_instructions.push(instructions[end - 1].clone());
        }
    }

    *instructions = new_instructions;
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(is_barrier(&X86Inst::CallRel { symbol_id: 0 }));

        assert!(!is_barrier(&X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Physical(Reg::Rbx),
        }));
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
