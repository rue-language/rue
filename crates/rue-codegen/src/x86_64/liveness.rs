//! Liveness analysis for register allocation.
//!
//! This module computes which virtual registers are "live" (their values may still
//! be used) at each program point. This information is used by the register
//! allocator to determine when registers can be reused.
//!
//! The analysis handles control flow by:
//! 1. Building a CFG from labels and branch instructions
//! 2. Computing live-out sets using backward dataflow analysis
//! 3. Extending live ranges to account for values live across branches

use std::collections::{HashMap, HashSet};

use super::mir::{LabelId, Operand, Reg, VReg, X86Inst, X86Mir};
use crate::index_map::IndexMap;

// Re-export shared types from the regalloc module
pub use crate::regalloc::{InstructionLiveness, LiveRange, LivenessDebugInfo};

/// Type alias for x86_64-specific liveness info.
pub type LivenessInfo = crate::regalloc::LivenessInfo<Reg>;

/// Compute liveness information for X86Mir.
///
/// This performs proper dataflow analysis that handles control flow:
/// 1. Build a map of labels to instruction indices
/// 2. For each instruction, compute successors (next instruction or branch targets)
/// 3. Do backward dataflow to compute live-in/live-out sets
/// 4. Build live ranges from the dataflow results
pub fn analyze(mir: &X86Mir) -> LivenessInfo {
    let instructions = mir.instructions();
    let num_insts = instructions.len();
    let vreg_count = mir.vreg_count();

    if num_insts == 0 {
        return LivenessInfo {
            ranges: IndexMap::new(),
            live_at: Vec::new(),
            clobbers_at: Vec::new(),
        };
    }

    // Step 1: Build label -> instruction index map
    let mut label_to_idx: HashMap<LabelId, usize> = HashMap::new();
    for (idx, inst) in instructions.iter().enumerate() {
        if let X86Inst::Label { id } = inst {
            label_to_idx.insert(*id, idx);
        }
    }

    // Step 2: Build successor lists for each instruction
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); num_insts];
    for (idx, inst) in instructions.iter().enumerate() {
        match inst {
            // Unconditional jump - only successor is the target
            X86Inst::Jmp { label } => {
                if let Some(&target) = label_to_idx.get(label) {
                    successors[idx].push(target);
                }
            }
            // Conditional branches - successor is both target and fall-through
            X86Inst::Jz { label }
            | X86Inst::Jnz { label }
            | X86Inst::Jo { label }
            | X86Inst::Jno { label }
            | X86Inst::Jb { label }
            | X86Inst::Jae { label }
            | X86Inst::Jbe { label }
            | X86Inst::Jge { label }
            | X86Inst::Jle { label } => {
                // Fall-through to next instruction
                if idx + 1 < num_insts {
                    successors[idx].push(idx + 1);
                }
                // Branch target
                if let Some(&target) = label_to_idx.get(label) {
                    successors[idx].push(target);
                }
            }
            // Return has no successors
            X86Inst::Ret => {}
            // Function calls fall through (callee returns)
            X86Inst::CallRel { .. } => {
                if idx + 1 < num_insts {
                    successors[idx].push(idx + 1);
                }
            }
            // All other instructions fall through to the next
            _ => {
                if idx + 1 < num_insts {
                    successors[idx].push(idx + 1);
                }
            }
        }
    }

    // Step 3: Backward dataflow analysis to compute live sets
    // live_out[i] = union of live_in[s] for all successors s of i
    // live_in[i] = uses[i] ∪ (live_out[i] - defs[i])
    let mut live_in: Vec<HashSet<VReg>> = vec![HashSet::new(); num_insts];
    let mut live_out: Vec<HashSet<VReg>> = vec![HashSet::new(); num_insts];

    // Pre-compute uses and defs for each instruction
    let inst_uses: Vec<Vec<VReg>> = instructions.iter().map(uses).collect();
    let inst_defs: Vec<Vec<VReg>> = instructions.iter().map(defs).collect();

    // Iterate until fixed point
    let mut changed = true;
    while changed {
        changed = false;

        // Process instructions in reverse order for faster convergence
        for idx in (0..num_insts).rev() {
            // Compute live_out as union of live_in of all successors
            let mut new_live_out = HashSet::new();
            for &succ in &successors[idx] {
                new_live_out.extend(&live_in[succ]);
            }

            // Compute live_in = uses ∪ (live_out - defs)
            let mut new_live_in: HashSet<VReg> = new_live_out.clone();
            for vreg in &inst_defs[idx] {
                new_live_in.remove(vreg);
            }
            for vreg in &inst_uses[idx] {
                new_live_in.insert(*vreg);
            }

            // Check if anything changed
            if new_live_in != live_in[idx] || new_live_out != live_out[idx] {
                changed = true;
                live_in[idx] = new_live_in;
                live_out[idx] = new_live_out;
            }
        }
    }

    // Step 4: Build live ranges from dataflow results
    // A vreg is live at instruction i if it's in live_in[i] or live_out[i] or defined/used at i
    let mut first_live: HashMap<VReg, usize> = HashMap::new();
    let mut last_live: HashMap<VReg, usize> = HashMap::new();

    for idx in 0..num_insts {
        // Check definitions
        for vreg in &inst_defs[idx] {
            first_live.entry(*vreg).or_insert(idx);
            last_live.insert(*vreg, idx);
        }
        // Check uses
        for vreg in &inst_uses[idx] {
            first_live.entry(*vreg).or_insert(idx);
            last_live.insert(*vreg, idx);
        }
        // Check live_in
        for vreg in &live_in[idx] {
            first_live.entry(*vreg).or_insert(idx);
            if last_live.get(vreg).is_none_or(|&last| idx > last) {
                last_live.insert(*vreg, idx);
            }
        }
        // Check live_out
        for vreg in &live_out[idx] {
            first_live.entry(*vreg).or_insert(idx);
            if last_live.get(vreg).is_none_or(|&last| idx > last) {
                last_live.insert(*vreg, idx);
            }
        }
    }

    // Build ranges using dense Vec storage
    let mut ranges: IndexMap<VReg, Option<LiveRange>> =
        IndexMap::with_capacity(vreg_count as usize);
    ranges.resize(vreg_count as usize, None);
    for vreg_idx in 0..vreg_count {
        let vreg = VReg::new(vreg_idx);
        if let (Some(&start), Some(&end)) = (first_live.get(&vreg), last_live.get(&vreg)) {
            ranges[vreg] = Some(LiveRange::new(start, end));
        }
    }

    // Compute live_at for each instruction (union of live_in and live_out)
    let mut live_at = vec![HashSet::new(); num_insts];
    for (idx, (li, lo)) in live_in.iter().zip(live_out.iter()).enumerate() {
        live_at[idx].extend(li);
        live_at[idx].extend(lo);
    }

    // Collect clobbers
    let clobbers_at: Vec<Vec<Reg>> = instructions.iter().map(|i| i.clobbers().to_vec()).collect();

    LivenessInfo {
        ranges,
        live_at,
        clobbers_at,
    }
}

/// Compute detailed liveness debug information for X86Mir.
///
/// This provides more detailed output than `analyze()`, including per-instruction
/// live-in/live-out sets and def/use information. Used by `--emit liveness`.
pub fn analyze_debug(mir: &X86Mir) -> LivenessDebugInfo {
    let instructions = mir.instructions();
    let num_insts = instructions.len();
    let vreg_count = mir.vreg_count();

    if num_insts == 0 {
        return LivenessDebugInfo {
            instructions: Vec::new(),
            live_ranges: IndexMap::new(),
            vreg_count,
        };
    }

    // Step 1: Build label -> instruction index map
    let mut label_to_idx: HashMap<LabelId, usize> = HashMap::new();
    for (idx, inst) in instructions.iter().enumerate() {
        if let X86Inst::Label { id } = inst {
            label_to_idx.insert(*id, idx);
        }
    }

    // Step 2: Build successor lists for each instruction
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); num_insts];
    for (idx, inst) in instructions.iter().enumerate() {
        match inst {
            X86Inst::Jmp { label } => {
                if let Some(&target) = label_to_idx.get(label) {
                    successors[idx].push(target);
                }
            }
            X86Inst::Jz { label }
            | X86Inst::Jnz { label }
            | X86Inst::Jo { label }
            | X86Inst::Jno { label }
            | X86Inst::Jb { label }
            | X86Inst::Jae { label }
            | X86Inst::Jbe { label }
            | X86Inst::Jge { label }
            | X86Inst::Jle { label } => {
                if idx + 1 < num_insts {
                    successors[idx].push(idx + 1);
                }
                if let Some(&target) = label_to_idx.get(label) {
                    successors[idx].push(target);
                }
            }
            X86Inst::Ret => {}
            X86Inst::CallRel { .. } => {
                if idx + 1 < num_insts {
                    successors[idx].push(idx + 1);
                }
            }
            _ => {
                if idx + 1 < num_insts {
                    successors[idx].push(idx + 1);
                }
            }
        }
    }

    // Step 3: Backward dataflow analysis
    let mut live_in: Vec<HashSet<VReg>> = vec![HashSet::new(); num_insts];
    let mut live_out: Vec<HashSet<VReg>> = vec![HashSet::new(); num_insts];

    let inst_uses: Vec<Vec<VReg>> = instructions.iter().map(uses).collect();
    let inst_defs: Vec<Vec<VReg>> = instructions.iter().map(defs).collect();

    let mut changed = true;
    while changed {
        changed = false;

        for idx in (0..num_insts).rev() {
            let mut new_live_out = HashSet::new();
            for &succ in &successors[idx] {
                new_live_out.extend(&live_in[succ]);
            }

            let mut new_live_in: HashSet<VReg> = new_live_out.clone();
            for vreg in &inst_defs[idx] {
                new_live_in.remove(vreg);
            }
            for vreg in &inst_uses[idx] {
                new_live_in.insert(*vreg);
            }

            if new_live_in != live_in[idx] || new_live_out != live_out[idx] {
                changed = true;
                live_in[idx] = new_live_in;
                live_out[idx] = new_live_out;
            }
        }
    }

    // Step 4: Build live ranges from dataflow results
    let mut first_live: HashMap<VReg, usize> = HashMap::new();
    let mut last_live: HashMap<VReg, usize> = HashMap::new();

    for idx in 0..num_insts {
        for vreg in &inst_defs[idx] {
            first_live.entry(*vreg).or_insert(idx);
            last_live.insert(*vreg, idx);
        }
        for vreg in &inst_uses[idx] {
            first_live.entry(*vreg).or_insert(idx);
            last_live.insert(*vreg, idx);
        }
        for vreg in &live_in[idx] {
            first_live.entry(*vreg).or_insert(idx);
            if last_live.get(vreg).is_none_or(|&last| idx > last) {
                last_live.insert(*vreg, idx);
            }
        }
        for vreg in &live_out[idx] {
            first_live.entry(*vreg).or_insert(idx);
            if last_live.get(vreg).is_none_or(|&last| idx > last) {
                last_live.insert(*vreg, idx);
            }
        }
    }

    // Build ranges using dense Vec storage
    let mut live_ranges: IndexMap<VReg, Option<crate::regalloc::LiveRange>> =
        IndexMap::with_capacity(vreg_count as usize);
    live_ranges.resize(vreg_count as usize, None);
    for vreg_idx in 0..vreg_count {
        let vreg = VReg::new(vreg_idx);
        if let (Some(&start), Some(&end)) = (first_live.get(&vreg), last_live.get(&vreg)) {
            live_ranges[vreg] = Some(crate::regalloc::LiveRange::new(start, end));
        }
    }

    // Build per-instruction liveness info
    let instruction_liveness: Vec<InstructionLiveness> = (0..num_insts)
        .map(|idx| InstructionLiveness {
            index: idx,
            live_in: live_in[idx].clone(),
            live_out: live_out[idx].clone(),
            defs: inst_defs[idx].clone(),
            uses: inst_uses[idx].clone(),
        })
        .collect();

    LivenessDebugInfo {
        instructions: instruction_liveness,
        live_ranges,
        vreg_count: mir.vreg_count(),
    }
}

/// Get virtual registers used (read) by an instruction.
fn uses(inst: &X86Inst) -> Vec<VReg> {
    // Most instructions have 0-2 operands; pre-allocate for common case
    let mut result = Vec::with_capacity(2);

    let add_if_virtual = |op: &Operand, vec: &mut Vec<VReg>| {
        if let Operand::Virtual(vreg) = op {
            vec.push(*vreg);
        }
    };

    match inst {
        X86Inst::MovRI32 { .. } | X86Inst::MovRI64 { .. } => {
            // Only defines, no uses
        }
        X86Inst::MovRR { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        X86Inst::MovRM { .. } => {
            // Reads from memory (base is physical), defines dst
        }
        X86Inst::MovMR { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        X86Inst::AddRR { dst, src }
        | X86Inst::AddRR64 { dst, src }
        | X86Inst::SubRR { dst, src }
        | X86Inst::SubRR64 { dst, src } => {
            // dst is both read and written (dst = dst op src)
            add_if_virtual(dst, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::AddRI { dst, .. } => {
            // dst is both read and written (dst = dst + imm)
            add_if_virtual(dst, &mut result);
        }
        X86Inst::ImulRR { dst, src } | X86Inst::ImulRR64 { dst, src } => {
            add_if_virtual(dst, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::Neg { dst } | X86Inst::Neg64 { dst } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::XorRI { dst, .. } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::AndRR { dst, src } | X86Inst::OrRR { dst, src } | X86Inst::XorRR { dst, src } => {
            add_if_virtual(dst, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::NotR { dst } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::ShlRCl { dst }
        | X86Inst::Shl32RCl { dst }
        | X86Inst::ShrRCl { dst }
        | X86Inst::Shr32RCl { dst }
        | X86Inst::SarRCl { dst }
        | X86Inst::Sar32RCl { dst } => {
            // dst is both read and written, CL is implicit physical register
            add_if_virtual(dst, &mut result);
        }
        X86Inst::ShlRI { dst, .. }
        | X86Inst::Shl32RI { dst, .. }
        | X86Inst::ShrRI { dst, .. }
        | X86Inst::Shr32RI { dst, .. }
        | X86Inst::SarRI { dst, .. }
        | X86Inst::Sar32RI { dst, .. } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::IdivR { src } => {
            add_if_virtual(src, &mut result);
            // Also implicitly uses RAX and RDX (physical)
        }
        X86Inst::TestRR { src1, src2 } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        X86Inst::CmpRR { src1, src2 } | X86Inst::Cmp64RR { src1, src2 } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        X86Inst::CmpRI { src, .. } | X86Inst::Cmp64RI { src, .. } => {
            add_if_virtual(src, &mut result);
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
            // Only defines dst, reads flags (implicit)
        }
        X86Inst::Movzx { src, .. }
        | X86Inst::Movsx8To64 { src, .. }
        | X86Inst::Movsx16To64 { src, .. }
        | X86Inst::Movsx32To64 { src, .. }
        | X86Inst::Movzx8To64 { src, .. }
        | X86Inst::Movzx16To64 { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        X86Inst::Pop { .. } => {
            // Only defines
        }
        X86Inst::Push { src } => {
            add_if_virtual(src, &mut result);
        }
        X86Inst::Lea { index, .. } => {
            // LEA only defines dst, it does not read dst's previous value.
            // base is physical register (Reg), so no vreg use.
            // index is an optional VReg that IS used if present.
            if let Some(idx) = index {
                result.push(*idx);
            }
        }
        X86Inst::Shl { dst, count } => {
            add_if_virtual(dst, &mut result);
            add_if_virtual(count, &mut result);
        }
        X86Inst::MovRMIndexed { base, .. } => {
            // base is a VReg
            result.push(*base);
        }
        X86Inst::MovMRIndexed { base, src, .. } => {
            result.push(*base);
            add_if_virtual(src, &mut result);
        }
        X86Inst::StringConstPtr { .. }
        | X86Inst::StringConstLen { .. }
        | X86Inst::StringConstCap { .. } => {
            // Only defines, no uses
        }
        X86Inst::Cdq
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
        | X86Inst::CallRel { .. }
        | X86Inst::Syscall
        | X86Inst::Ret => {
            // No vreg operands
        }
    }

    result
}

/// Get virtual registers defined (written) by an instruction.
fn defs(inst: &X86Inst) -> Vec<VReg> {
    // Most instructions define 0-1 registers; pre-allocate for common case
    let mut result = Vec::with_capacity(1);

    let add_if_virtual = |op: &Operand, vec: &mut Vec<VReg>| {
        if let Operand::Virtual(vreg) = op {
            vec.push(*vreg);
        }
    };

    match inst {
        X86Inst::MovRI32 { dst, .. } | X86Inst::MovRI64 { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovRR { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovRM { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovMR { .. } => {
            // Writes to memory, not to a register
        }
        X86Inst::AddRR { dst, .. }
        | X86Inst::AddRR64 { dst, .. }
        | X86Inst::AddRI { dst, .. }
        | X86Inst::SubRR { dst, .. }
        | X86Inst::SubRR64 { dst, .. }
        | X86Inst::ImulRR { dst, .. }
        | X86Inst::ImulRR64 { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Neg { dst } | X86Inst::Neg64 { dst } | X86Inst::XorRI { dst, .. } => {
            add_if_virtual(dst, &mut result);
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
            add_if_virtual(dst, &mut result);
        }
        X86Inst::IdivR { .. } => {
            // Implicitly defines RAX (quotient) and RDX (remainder), but those are physical
        }
        X86Inst::TestRR { .. }
        | X86Inst::CmpRR { .. }
        | X86Inst::Cmp64RR { .. }
        | X86Inst::CmpRI { .. }
        | X86Inst::Cmp64RI { .. } => {
            // Only sets flags, no register def
        }
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
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Movzx { dst, .. }
        | X86Inst::Movsx8To64 { dst, .. }
        | X86Inst::Movsx16To64 { dst, .. }
        | X86Inst::Movsx32To64 { dst, .. }
        | X86Inst::Movzx8To64 { dst, .. }
        | X86Inst::Movzx16To64 { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Pop { dst } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Push { .. } => {
            // Only reads, no definition
        }
        X86Inst::Lea { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Shl { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovRMIndexed { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::MovMRIndexed { .. } => {
            // Writes to memory
        }
        X86Inst::StringConstPtr { dst, .. }
        | X86Inst::StringConstLen { dst, .. }
        | X86Inst::StringConstCap { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Cdq
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
        | X86Inst::CallRel { .. }
        | X86Inst::Syscall
        | X86Inst::Ret => {
            // No vreg operands
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_liveness() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();

        // v0 = 42
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 42,
        });
        // v1 = v0
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        let info = analyze(&mir);

        // v0 is defined at 0, last used at 1
        assert_eq!(info.range(v0), Some(&LiveRange::new(0, 1)));
        // v1 is defined at 1, last used at 1 (no further use)
        assert_eq!(info.range(v1), Some(&LiveRange::new(1, 1)));
    }

    #[test]
    fn test_overlapping_ranges() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let v2 = mir.alloc_vreg();

        // v0 = 1
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 1,
        });
        // v1 = 2
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v1),
            imm: 2,
        });
        // v2 = v0
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v2),
            src: Operand::Virtual(v0),
        });
        // v2 += v1
        mir.push(X86Inst::AddRR {
            dst: Operand::Virtual(v2),
            src: Operand::Virtual(v1),
        });

        let info = analyze(&mir);

        // v0: defined at 0, used at 2 (in MovRR)
        // v1: defined at 1, used at 3 (in AddRR)
        // v2: defined at 2 (MovRR), used at 3 (AddRR reads and writes)

        // v0 and v1 should interfere (both live at instruction 2)
        assert!(info.interferes(v0, v1));
    }

    #[test]
    fn test_non_overlapping_ranges() {
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();

        // v0 = 1
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 1,
        });
        // (v0 is dead after this, not used again)
        // v1 = 2
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v1),
            imm: 2,
        });

        let info = analyze(&mir);

        // v0 and v1 don't interfere (v0 is not used after being defined)
        assert!(!info.interferes(v0, v1));
    }

    #[test]
    fn test_empty_mir() {
        let mir = X86Mir::new();
        let info = analyze(&mir);

        assert!(info.ranges.is_empty());
        assert!(info.live_at.is_empty());
        assert!(info.clobbers_at.is_empty());
    }

    #[test]
    fn test_liveness_across_branch() {
        // Test that liveness analysis correctly handles control flow.
        // Code pattern:
        //   v0 = 1
        //   cmp v0, 0
        //   jz label_else
        //   v1 = v0  ; v0 used in then branch
        //   jmp label_end
        // label_else:
        //   v1 = 2
        // label_end:
        //   ret

        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let label_else = mir.alloc_label();
        let label_end = mir.alloc_label();

        // v0 = 1
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 1,
        });

        // cmp v0, 0
        mir.push(X86Inst::CmpRI {
            src: Operand::Virtual(v0),
            imm: 0,
        });

        // jz label_else
        mir.push(X86Inst::Jz { label: label_else });

        // v1 = v0 (then branch)
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        // jmp label_end
        mir.push(X86Inst::Jmp { label: label_end });

        // label_else:
        mir.push(X86Inst::Label { id: label_else });

        // v1 = 2 (else branch)
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v1),
            imm: 2,
        });

        // label_end:
        mir.push(X86Inst::Label { id: label_end });

        // Use v1 at the end
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Virtual(v1),
        });

        mir.push(X86Inst::Ret);

        let info = analyze(&mir);

        // v0 should be live from definition (0) through use in CMP (1) and MOV (3)
        let v0_range = info.range(v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0);
        assert!(
            v0_range.end >= 3,
            "v0 should be live through its last use at instruction 3"
        );

        // v1 should be live from first definition through final use
        let v1_range = info.range(v1).expect("v1 should have a range");
        assert!(v1_range.end >= 8, "v1 should be live until the MOV to RAX");
    }

    #[test]
    fn test_liveness_with_conditional_branch() {
        // Test Jnz handling
        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let skip_label = mir.alloc_label();

        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 42,
        });

        mir.push(X86Inst::CmpRI {
            src: Operand::Virtual(v0),
            imm: 0,
        });

        mir.push(X86Inst::Jnz { label: skip_label });

        // v0 is used after the conditional branch
        mir.push(X86Inst::MovRR {
            dst: Operand::Physical(Reg::Rax),
            src: Operand::Virtual(v0),
        });

        mir.push(X86Inst::Label { id: skip_label });

        mir.push(X86Inst::Ret);

        let info = analyze(&mir);

        // v0 should be live from 0 through at least instruction 3 (use in MOV)
        let v0_range = info.range(v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0);
        assert!(v0_range.end >= 3);
    }

    #[test]
    fn test_liveness_loop_pattern() {
        // Test a simple loop pattern where a value is used across a back edge
        //   v0 = 10
        // loop:
        //   v1 = v0
        //   v0 = v0 - 1
        //   cmp v0, 0
        //   jnz loop
        //   ret

        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let loop_label = mir.alloc_label();

        // v0 = 10
        mir.push(X86Inst::MovRI32 {
            dst: Operand::Virtual(v0),
            imm: 10,
        });

        // loop:
        mir.push(X86Inst::Label { id: loop_label });

        // v1 = v0
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        // v0 = v0 - 1 (using SubRR with itself, which is a bit odd but works for test)
        // Actually, let's use AddRI with -1
        mir.push(X86Inst::AddRI {
            dst: Operand::Virtual(v0),
            imm: -1,
        });

        // cmp v0, 0
        mir.push(X86Inst::CmpRI {
            src: Operand::Virtual(v0),
            imm: 0,
        });

        // jnz loop
        mir.push(X86Inst::Jnz { label: loop_label });

        mir.push(X86Inst::Ret);

        let info = analyze(&mir);

        // v0 should be live throughout the loop (from def to last use in CMP)
        let v0_range = info.range(v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0);
        assert!(v0_range.end >= 4, "v0 should be live through CMP");
    }

    #[test]
    fn test_lea_only_defines_dst() {
        // LEA only defines its destination; it does NOT use it.
        // This is different from instructions like AddRR where dst is both read and written.
        // LEA computes an address and writes it to dst without reading dst's previous value.
        //
        // Bug: The uses() function was incorrectly listing dst as a use for LEA.

        let mut mir = X86Mir::new();
        let v0 = mir.alloc_vreg();

        // lea v0, [rbp-8]
        // This should ONLY define v0, not use it.
        mir.push(X86Inst::Lea {
            dst: Operand::Virtual(v0),
            base: Reg::Rbp,
            index: None,
            scale: 1,
            disp: -8,
        });

        let info = analyze(&mir);

        // v0 should have a range starting at 0 (where it's defined)
        let v0_range = info.range(v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0, "LEA defines v0 at instruction 0");

        // The instruction's uses should NOT include v0
        let inst_uses = uses(&mir.instructions()[0]);
        assert!(
            !inst_uses.contains(&v0),
            "LEA should not list dst in uses(); found uses: {:?}",
            inst_uses
        );
    }
}
