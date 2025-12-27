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

use super::mir::{Aarch64Inst, Aarch64Mir, LabelId, Operand, Reg, VReg};

// Re-export shared types from the regalloc module
pub use crate::regalloc::{InstructionLiveness, LiveRange, LivenessDebugInfo};

/// Type alias for aarch64-specific liveness info.
pub type LivenessInfo = crate::regalloc::LivenessInfo<Reg>;

/// Compute liveness information for Aarch64Mir.
///
/// This performs proper dataflow analysis that handles control flow:
/// 1. Build a map of labels to instruction indices
/// 2. For each instruction, compute successors (next instruction or branch targets)
/// 3. Do backward dataflow to compute live-in/live-out sets
/// 4. Build live ranges from the dataflow results
pub fn analyze(mir: &Aarch64Mir) -> LivenessInfo {
    let instructions = mir.instructions();
    let num_insts = instructions.len();

    if num_insts == 0 {
        return LivenessInfo {
            ranges: HashMap::new(),
            live_at: Vec::new(),
            clobbers_at: Vec::new(),
        };
    }

    // Step 1: Build label -> instruction index map
    let mut label_to_idx: HashMap<LabelId, usize> = HashMap::new();
    for (idx, inst) in instructions.iter().enumerate() {
        if let Aarch64Inst::Label { id } = inst {
            label_to_idx.insert(*id, idx);
        }
    }

    // Step 2: Build successor lists for each instruction
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); num_insts];
    for (idx, inst) in instructions.iter().enumerate() {
        match inst {
            // Unconditional branch - only successor is the target
            Aarch64Inst::B { label } => {
                if let Some(&target) = label_to_idx.get(label) {
                    successors[idx].push(target);
                }
            }
            // Conditional branches - successor is both target and fall-through
            Aarch64Inst::BCond { label, .. }
            | Aarch64Inst::Bvs { label }
            | Aarch64Inst::Bvc { label }
            | Aarch64Inst::Cbz { label, .. }
            | Aarch64Inst::Cbnz { label, .. } => {
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
            Aarch64Inst::Ret => {}
            // Function calls fall through (callee returns)
            Aarch64Inst::Bl { .. } => {
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
            if last_live.get(vreg).map_or(true, |&last| idx > last) {
                last_live.insert(*vreg, idx);
            }
        }
        // Check live_out
        for vreg in &live_out[idx] {
            first_live.entry(*vreg).or_insert(idx);
            if last_live.get(vreg).map_or(true, |&last| idx > last) {
                last_live.insert(*vreg, idx);
            }
        }
    }

    // Build ranges
    let mut ranges: HashMap<VReg, LiveRange> = HashMap::new();
    for vreg_idx in 0..mir.vreg_count() {
        let vreg = VReg::new(vreg_idx);
        if let (Some(&start), Some(&end)) = (first_live.get(&vreg), last_live.get(&vreg)) {
            ranges.insert(vreg, LiveRange::new(start, end));
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

/// Compute detailed liveness debug information for Aarch64Mir.
///
/// This provides more detailed output than `analyze()`, including per-instruction
/// live-in/live-out sets and def/use information. Used by `--emit liveness`.
pub fn analyze_debug(mir: &Aarch64Mir) -> LivenessDebugInfo {
    let instructions = mir.instructions();
    let num_insts = instructions.len();

    if num_insts == 0 {
        return LivenessDebugInfo {
            instructions: Vec::new(),
            live_ranges: HashMap::new(),
            vreg_count: mir.vreg_count(),
        };
    }

    // Step 1: Build label -> instruction index map
    let mut label_to_idx: HashMap<LabelId, usize> = HashMap::new();
    for (idx, inst) in instructions.iter().enumerate() {
        if let Aarch64Inst::Label { id } = inst {
            label_to_idx.insert(*id, idx);
        }
    }

    // Step 2: Build successor lists for each instruction
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); num_insts];
    for (idx, inst) in instructions.iter().enumerate() {
        match inst {
            Aarch64Inst::B { label } => {
                if let Some(&target) = label_to_idx.get(label) {
                    successors[idx].push(target);
                }
            }
            Aarch64Inst::BCond { label, .. }
            | Aarch64Inst::Bvs { label }
            | Aarch64Inst::Bvc { label }
            | Aarch64Inst::Cbz { label, .. }
            | Aarch64Inst::Cbnz { label, .. } => {
                if idx + 1 < num_insts {
                    successors[idx].push(idx + 1);
                }
                if let Some(&target) = label_to_idx.get(label) {
                    successors[idx].push(target);
                }
            }
            Aarch64Inst::Ret => {}
            Aarch64Inst::Bl { .. } => {
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
            if last_live.get(vreg).map_or(true, |&last| idx > last) {
                last_live.insert(*vreg, idx);
            }
        }
        for vreg in &live_out[idx] {
            first_live.entry(*vreg).or_insert(idx);
            if last_live.get(vreg).map_or(true, |&last| idx > last) {
                last_live.insert(*vreg, idx);
            }
        }
    }

    // Build ranges
    let mut live_ranges: HashMap<VReg, crate::regalloc::LiveRange> = HashMap::new();
    for vreg_idx in 0..mir.vreg_count() {
        let vreg = VReg::new(vreg_idx);
        if let (Some(&start), Some(&end)) = (first_live.get(&vreg), last_live.get(&vreg)) {
            live_ranges.insert(vreg, crate::regalloc::LiveRange::new(start, end));
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
fn uses(inst: &Aarch64Inst) -> Vec<VReg> {
    // Most instructions have 0-2 operands; pre-allocate for common case
    let mut result = Vec::with_capacity(2);

    let add_if_virtual = |op: &Operand, vec: &mut Vec<VReg>| {
        if let Operand::Virtual(vreg) = op {
            vec.push(*vreg);
        }
    };

    match inst {
        Aarch64Inst::MovImm { .. } => {
            // Only defines
        }
        Aarch64Inst::MovRR { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::Ldr { .. } => {
            // Reads from memory via base (physical register)
        }
        Aarch64Inst::Str { src, .. } => {
            add_if_virtual(src, &mut result);
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
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        Aarch64Inst::MvnRR { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::AddImm { src, .. }
        | Aarch64Inst::SubImm { src, .. }
        | Aarch64Inst::Lsr64Imm { src, .. }
        | Aarch64Inst::Asr64Imm { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::Msub {
            src1, src2, src3, ..
        } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
            add_if_virtual(src3, &mut result);
        }
        Aarch64Inst::Neg { src, .. }
        | Aarch64Inst::Negs { src, .. }
        | Aarch64Inst::Negs32 { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::EorImm { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::CmpRR { src1, src2 }
        | Aarch64Inst::Cmp64RR { src1, src2 }
        | Aarch64Inst::TstRR { src1, src2 } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        Aarch64Inst::CmpImm { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::Cbz { src, .. } | Aarch64Inst::Cbnz { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::Cset { .. } => {
            // Only defines
        }
        Aarch64Inst::Sxtb { src, .. }
        | Aarch64Inst::Sxth { src, .. }
        | Aarch64Inst::Sxtw { src, .. }
        | Aarch64Inst::Uxtb { src, .. }
        | Aarch64Inst::Uxth { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::StpPre { src1, src2, .. } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        Aarch64Inst::LdpPost { .. } => {
            // Only defines
        }
        Aarch64Inst::LdrIndexed { base, .. } => {
            // base is a VReg directly, not an Operand
            result.push(*base);
        }
        Aarch64Inst::StrIndexed { src, base } => {
            add_if_virtual(src, &mut result);
            result.push(*base);
        }
        Aarch64Inst::LdrIndexedOffset { base, .. } => {
            result.push(*base);
        }
        Aarch64Inst::StrIndexedOffset { src, base, .. } => {
            add_if_virtual(src, &mut result);
            result.push(*base);
        }
        Aarch64Inst::LslImm { src, .. }
        | Aarch64Inst::Lsl32Imm { src, .. }
        | Aarch64Inst::Lsr32Imm { src, .. }
        | Aarch64Inst::Asr32Imm { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::StringConstPtr { .. }
        | Aarch64Inst::StringConstLen { .. }
        | Aarch64Inst::StringConstCap { .. } => {
            // Only defines, no uses
        }
        Aarch64Inst::B { .. }
        | Aarch64Inst::BCond { .. }
        | Aarch64Inst::Bvs { .. }
        | Aarch64Inst::Bvc { .. }
        | Aarch64Inst::Label { .. }
        | Aarch64Inst::Bl { .. }
        | Aarch64Inst::Ret => {
            // No vreg operands
        }
    }

    result
}

/// Get virtual registers defined (written) by an instruction.
fn defs(inst: &Aarch64Inst) -> Vec<VReg> {
    // Most instructions define 0-1 registers; pre-allocate for common case
    let mut result = Vec::with_capacity(1);

    let add_if_virtual = |op: &Operand, vec: &mut Vec<VReg>| {
        if let Operand::Virtual(vreg) = op {
            vec.push(*vreg);
        }
    };

    match inst {
        Aarch64Inst::MovImm { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::MovRR { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::Ldr { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::Str { .. } => {
            // Writes to memory
        }
        Aarch64Inst::AddRR { dst, .. }
        | Aarch64Inst::AddsRR { dst, .. }
        | Aarch64Inst::AddsRR64 { dst, .. }
        | Aarch64Inst::SubRR { dst, .. }
        | Aarch64Inst::SubsRR { dst, .. }
        | Aarch64Inst::SubsRR64 { dst, .. }
        | Aarch64Inst::AddImm { dst, .. }
        | Aarch64Inst::SubImm { dst, .. }
        | Aarch64Inst::MulRR { dst, .. }
        | Aarch64Inst::SmullRR { dst, .. }
        | Aarch64Inst::UmullRR { dst, .. }
        | Aarch64Inst::SmulhRR { dst, .. }
        | Aarch64Inst::UmulhRR { dst, .. }
        | Aarch64Inst::Lsr64Imm { dst, .. }
        | Aarch64Inst::Asr64Imm { dst, .. }
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
        | Aarch64Inst::LsrRR { dst, .. }
        | Aarch64Inst::Lsr32RR { dst, .. }
        | Aarch64Inst::AsrRR { dst, .. }
        | Aarch64Inst::Asr32RR { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::CmpRR { .. }
        | Aarch64Inst::Cmp64RR { .. }
        | Aarch64Inst::CmpImm { .. }
        | Aarch64Inst::TstRR { .. } => {
            // Only sets flags
        }
        Aarch64Inst::Cbz { .. } | Aarch64Inst::Cbnz { .. } => {
            // Branch instruction, no def
        }
        Aarch64Inst::Cset { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::Sxtb { dst, .. }
        | Aarch64Inst::Sxth { dst, .. }
        | Aarch64Inst::Sxtw { dst, .. }
        | Aarch64Inst::Uxtb { dst, .. }
        | Aarch64Inst::Uxth { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::StpPre { .. } => {
            // Writes to memory
        }
        Aarch64Inst::LdpPost { dst1, dst2, .. } => {
            add_if_virtual(dst1, &mut result);
            add_if_virtual(dst2, &mut result);
        }
        Aarch64Inst::LdrIndexed { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::StrIndexed { .. } => {
            // Writes to memory
        }
        Aarch64Inst::LdrIndexedOffset { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::StrIndexedOffset { .. } => {
            // Writes to memory
        }
        Aarch64Inst::LslImm { dst, .. }
        | Aarch64Inst::Lsl32Imm { dst, .. }
        | Aarch64Inst::Lsr32Imm { dst, .. }
        | Aarch64Inst::Asr32Imm { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::StringConstPtr { dst, .. }
        | Aarch64Inst::StringConstLen { dst, .. }
        | Aarch64Inst::StringConstCap { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        Aarch64Inst::B { .. }
        | Aarch64Inst::BCond { .. }
        | Aarch64Inst::Bvs { .. }
        | Aarch64Inst::Bvc { .. }
        | Aarch64Inst::Label { .. }
        | Aarch64Inst::Bl { .. }
        | Aarch64Inst::Ret => {
            // No vreg operands
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aarch64::mir::Cond;

    #[test]
    fn test_simple_liveness() {
        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v0),
            imm: 42,
        });
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        let info = analyze(&mir);

        assert_eq!(info.ranges.get(&v0), Some(&LiveRange::new(0, 1)));
        assert_eq!(info.ranges.get(&v1), Some(&LiveRange::new(1, 1)));
    }

    #[test]
    fn test_overlapping_ranges() {
        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let v2 = mir.alloc_vreg();

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v0),
            imm: 1,
        });
        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v1),
            imm: 2,
        });
        mir.push(Aarch64Inst::AddRR {
            dst: Operand::Virtual(v2),
            src1: Operand::Virtual(v0),
            src2: Operand::Virtual(v1),
        });

        let info = analyze(&mir);

        assert!(info.interferes(v0, v1));
    }

    #[test]
    fn test_empty_mir() {
        let mir = Aarch64Mir::new();
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
        //   cbz v0, label_else
        //   v1 = v0  ; v0 used in then branch
        //   b label_end
        // label_else:
        //   v1 = 2
        // label_end:
        //   ret

        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let label_else = mir.alloc_label();
        let label_end = mir.alloc_label();

        // v0 = 1
        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v0),
            imm: 1,
        });

        // cbz v0, label_else
        mir.push(Aarch64Inst::Cbz {
            src: Operand::Virtual(v0),
            label: label_else,
        });

        // v1 = v0 (then branch)
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        // b label_end
        mir.push(Aarch64Inst::B { label: label_end });

        // label_else:
        mir.push(Aarch64Inst::Label { id: label_else });

        // v1 = 2 (else branch)
        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v1),
            imm: 2,
        });

        // label_end:
        mir.push(Aarch64Inst::Label { id: label_end });

        // Use v1 at the end
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Virtual(v1),
        });

        mir.push(Aarch64Inst::Ret);

        let info = analyze(&mir);

        // v0 should be live from definition (0) through use in CBZ (1) and MOV (2)
        let v0_range = info.ranges.get(&v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0);
        assert!(
            v0_range.end >= 2,
            "v0 should be live through its last use at instruction 2"
        );

        // v1 should be live from first definition through final use
        let v1_range = info.ranges.get(&v1).expect("v1 should have a range");
        assert!(v1_range.end >= 7, "v1 should be live until the MOV to X0");
    }

    #[test]
    fn test_liveness_with_conditional_branch() {
        // Test B.cond handling
        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();
        let skip_label = mir.alloc_label();

        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v0),
            imm: 42,
        });

        mir.push(Aarch64Inst::CmpImm {
            src: Operand::Virtual(v0),
            imm: 0,
        });

        mir.push(Aarch64Inst::BCond {
            cond: Cond::Eq,
            label: skip_label,
        });

        // v0 is used after the conditional branch
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Physical(Reg::X0),
            src: Operand::Virtual(v0),
        });

        mir.push(Aarch64Inst::Label { id: skip_label });

        mir.push(Aarch64Inst::Ret);

        let info = analyze(&mir);

        // v0 should be live from 0 through at least instruction 3 (use in MOV)
        let v0_range = info.ranges.get(&v0).expect("v0 should have a range");
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
        //   cbnz v0, loop
        //   ret

        let mut mir = Aarch64Mir::new();
        let v0 = mir.alloc_vreg();
        let v1 = mir.alloc_vreg();
        let loop_label = mir.alloc_label();

        // v0 = 10
        mir.push(Aarch64Inst::MovImm {
            dst: Operand::Virtual(v0),
            imm: 10,
        });

        // loop:
        mir.push(Aarch64Inst::Label { id: loop_label });

        // v1 = v0
        mir.push(Aarch64Inst::MovRR {
            dst: Operand::Virtual(v1),
            src: Operand::Virtual(v0),
        });

        // v0 = v0 - 1 (using SubImm)
        mir.push(Aarch64Inst::SubImm {
            dst: Operand::Virtual(v0),
            src: Operand::Virtual(v0),
            imm: 1,
        });

        // cbnz v0, loop
        mir.push(Aarch64Inst::Cbnz {
            src: Operand::Virtual(v0),
            label: loop_label,
        });

        mir.push(Aarch64Inst::Ret);

        let info = analyze(&mir);

        // v0 should be live throughout the loop (from def to last use in CBNZ)
        let v0_range = info.ranges.get(&v0).expect("v0 should have a range");
        assert_eq!(v0_range.start, 0);
        assert!(v0_range.end >= 4, "v0 should be live through CBNZ");
    }
}
