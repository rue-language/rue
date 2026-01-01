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

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use super::mir::{Aarch64Inst, Aarch64Mir, Operand, Reg};
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
        | Aarch64Inst::LdpPost { .. } => 4,

        // Memory stores: 1 cycle to retire (store buffer)
        Aarch64Inst::Str { .. }
        | Aarch64Inst::StrIndexed { .. }
        | Aarch64Inst::StrIndexedOffset { .. }
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
        | Aarch64Inst::Msub { .. } => 3,

        // Division: 12-20 cycles (highly variable)
        Aarch64Inst::SdivRR { .. } => 12,

        // Logical operations: 1 cycle
        Aarch64Inst::AndRR { .. }
        | Aarch64Inst::OrrRR { .. }
        | Aarch64Inst::EorRR { .. }
        | Aarch64Inst::EorImm { .. }
        | Aarch64Inst::MvnRR { .. } => 1,

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
        | Aarch64Inst::Ret => 1,

        // Labels are not real instructions
        Aarch64Inst::Label { .. } => 0,

        // String constants (pseudo-instructions)
        Aarch64Inst::StringConstPtr { .. }
        | Aarch64Inst::StringConstLen { .. }
        | Aarch64Inst::StringConstCap { .. } => 1,
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
            | Aarch64Inst::Ret
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
    )
}

/// Get registers read by an instruction (for dependency analysis).
fn regs_read(inst: &Aarch64Inst) -> Vec<Reg> {
    let mut result = Vec::new();

    let add_if_phys = |op: &Operand, vec: &mut Vec<Reg>| {
        if let Operand::Physical(reg) = op {
            vec.push(*reg);
        }
    };

    match inst {
        Aarch64Inst::MovImm { .. } => {}
        Aarch64Inst::MovRR { src, .. } => add_if_phys(src, &mut result),
        Aarch64Inst::Ldr { base, .. } => result.push(*base),
        Aarch64Inst::Str { src, base, .. } => {
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
        } => {
            add_if_phys(src1, &mut result);
            add_if_phys(src2, &mut result);
            add_if_phys(src3, &mut result);
        }
        Aarch64Inst::Neg { src, .. }
        | Aarch64Inst::Negs { src, .. }
        | Aarch64Inst::Negs32 { src, .. }
        | Aarch64Inst::MvnRR { src, .. }
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
        | Aarch64Inst::StrIndexed { .. }
        | Aarch64Inst::LdrIndexedOffset { .. }
        | Aarch64Inst::StrIndexedOffset { .. } => {
            // base is VReg, handled separately
        }
        _ => {}
    }

    result
}

/// Get registers written by an instruction (for dependency analysis).
fn regs_written(inst: &Aarch64Inst) -> Vec<Reg> {
    let mut result = Vec::new();

    let add_if_phys = |op: &Operand, vec: &mut Vec<Reg>| {
        if let Operand::Physical(reg) = op {
            vec.push(*reg);
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
        | Aarch64Inst::Msub { dst, .. }
        | Aarch64Inst::Neg { dst, .. }
        | Aarch64Inst::Negs { dst, .. }
        | Aarch64Inst::Negs32 { dst, .. }
        | Aarch64Inst::AndRR { dst, .. }
        | Aarch64Inst::OrrRR { dst, .. }
        | Aarch64Inst::EorRR { dst, .. }
        | Aarch64Inst::EorImm { dst, .. }
        | Aarch64Inst::MvnRR { dst, .. }
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
        | Aarch64Inst::StrIndexedOffset { .. } => {
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
        _ => {}
    }

    result
}

/// Check if an instruction writes to NZCV flags.
fn writes_flags(inst: &Aarch64Inst) -> bool {
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

/// Build the dependency graph for a basic block of instructions.
fn build_dep_graph(instructions: &[Aarch64Inst], start: usize, end: usize) -> Vec<SchedNode> {
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
pub fn schedule(mir: &mut Aarch64Mir) {
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

        // Don't schedule blocks that are too small
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
        assert!(is_barrier(&Aarch64Inst::Bl { symbol_id: 0 }));

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
