//! Liveness analysis for register allocation.
//!
//! This module computes which virtual registers are "live" (their values may still
//! be used) at each program point. This information is used by the register
//! allocator to determine when registers can be reused.
//!
//! The analysis works backwards through the program:
//! - A vreg becomes live when it is used (read)
//! - A vreg becomes dead when it is defined (written)
//!
//! For now, we compute live ranges as simple intervals [def, last_use] since
//! X86Mir is mostly straight-line code with jumps for control flow.

use std::collections::{HashMap, HashSet};

use super::mir::{Operand, Reg, VReg, X86Inst, X86Mir};

/// Live range for a virtual register.
///
/// Represents the instruction range where this vreg's value is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveRange {
    /// Instruction index where the vreg is defined (first write).
    pub start: usize,
    /// Instruction index where the vreg is last used (last read).
    pub end: usize,
}

impl LiveRange {
    /// Check if this live range overlaps with another.
    pub fn overlaps(&self, other: &LiveRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// Result of liveness analysis.
pub struct LivenessInfo {
    /// Live range for each virtual register.
    pub ranges: HashMap<VReg, LiveRange>,
    /// For each instruction, which vregs are live after it executes.
    /// This is useful for determining which registers are in use at any point.
    pub live_at: Vec<HashSet<VReg>>,
    /// For each instruction index, the physical registers clobbered by that instruction.
    /// This is used to prevent allocating vregs to registers that would be clobbered.
    pub clobbers_at: Vec<Vec<Reg>>,
}

impl LivenessInfo {
    /// Get vregs that are live at a given instruction index.
    pub fn live_at(&self, inst_idx: usize) -> &HashSet<VReg> {
        &self.live_at[inst_idx]
    }

    /// Get the live range for a vreg.
    pub fn range(&self, vreg: VReg) -> Option<&LiveRange> {
        self.ranges.get(&vreg)
    }

    /// Check if two vregs interfere (have overlapping live ranges).
    pub fn interferes(&self, a: VReg, b: VReg) -> bool {
        match (self.ranges.get(&a), self.ranges.get(&b)) {
            (Some(ra), Some(rb)) => ra.overlaps(rb),
            _ => false,
        }
    }

    /// Get the physical registers clobbered at a given instruction index.
    pub fn clobbers_at(&self, inst_idx: usize) -> &[Reg] {
        &self.clobbers_at[inst_idx]
    }

    /// Check if a physical register is clobbered while a vreg is live.
    ///
    /// Returns true if `reg` is clobbered by any instruction during the live range of `vreg`.
    pub fn is_clobbered_during(&self, vreg: VReg, reg: Reg) -> bool {
        if let Some(range) = self.ranges.get(&vreg) {
            for idx in range.start..=range.end {
                if idx < self.clobbers_at.len() && self.clobbers_at[idx].contains(&reg) {
                    return true;
                }
            }
        }
        false
    }
}

/// Compute liveness information for X86Mir.
pub fn analyze(mir: &X86Mir) -> LivenessInfo {
    let instructions = mir.instructions();
    let num_insts = instructions.len();

    // Track first definition and last use for each vreg
    let mut first_def: HashMap<VReg, usize> = HashMap::new();
    let mut last_use: HashMap<VReg, usize> = HashMap::new();

    // Track clobbers at each instruction
    let mut clobbers_at: Vec<Vec<Reg>> = Vec::with_capacity(num_insts);

    // Forward pass: find definitions, uses, and clobbers
    for (idx, inst) in instructions.iter().enumerate() {
        // Record uses (reads) first - this is important because a use before def
        // in the same instruction means the value was live before
        for vreg in uses(inst) {
            // Update last use (always update - later uses override earlier)
            last_use.insert(vreg, idx);
            // If not defined yet, this is an error in the IR, but we handle gracefully
            if !first_def.contains_key(&vreg) {
                first_def.insert(vreg, 0); // Assume defined at start
            }
        }

        // Record definitions (writes)
        for vreg in defs(inst) {
            // Only record first definition
            first_def.entry(vreg).or_insert(idx);
            // If this is also the first use, update last_use
            last_use.entry(vreg).or_insert(idx);
        }

        // Record clobbers
        clobbers_at.push(inst.clobbers().to_vec());
    }

    // Build live ranges
    let mut ranges: HashMap<VReg, LiveRange> = HashMap::new();
    for vreg_idx in 0..mir.vreg_count() {
        let vreg = VReg::new(vreg_idx);
        if let (Some(&start), Some(&end)) = (first_def.get(&vreg), last_use.get(&vreg)) {
            ranges.insert(vreg, LiveRange { start, end });
        }
    }

    // Compute live_at for each instruction
    // A vreg is live at instruction i if: start <= i <= end
    let mut live_at = vec![HashSet::new(); num_insts];
    for (&vreg, range) in &ranges {
        for i in range.start..=range.end.min(num_insts.saturating_sub(1)) {
            live_at[i].insert(vreg);
        }
    }

    LivenessInfo {
        ranges,
        live_at,
        clobbers_at,
    }
}

/// Get virtual registers used (read) by an instruction.
fn uses(inst: &X86Inst) -> Vec<VReg> {
    let mut result = Vec::new();

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
        X86Inst::AddRR { dst, src } | X86Inst::SubRR { dst, src } => {
            // dst is both read and written (dst = dst op src)
            add_if_virtual(dst, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::AddRI { dst, .. } => {
            // dst is both read and written (dst = dst + imm)
            add_if_virtual(dst, &mut result);
        }
        X86Inst::ImulRR { dst, src } => {
            add_if_virtual(dst, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::Neg { dst } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::XorRI { dst, .. } => {
            // dst is both read and written
            add_if_virtual(dst, &mut result);
        }
        X86Inst::AndRR { dst, src } | X86Inst::OrRR { dst, src } => {
            add_if_virtual(dst, &mut result);
            add_if_virtual(src, &mut result);
        }
        X86Inst::IdivR { src } => {
            add_if_virtual(src, &mut result);
            // Also implicitly uses RAX and RDX (physical)
        }
        X86Inst::TestRR { src1, src2 } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        X86Inst::CmpRR { src1, src2 } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        X86Inst::CmpRI { src, .. } => {
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
        X86Inst::Cdq
        | X86Inst::Jz { .. }
        | X86Inst::Jnz { .. }
        | X86Inst::Jo { .. }
        | X86Inst::Jno { .. }
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
    let mut result = Vec::new();

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
        | X86Inst::AddRI { dst, .. }
        | X86Inst::SubRR { dst, .. }
        | X86Inst::ImulRR { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::Neg { dst } | X86Inst::XorRI { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::AndRR { dst, .. } | X86Inst::OrRR { dst, .. } => {
            add_if_virtual(dst, &mut result);
        }
        X86Inst::IdivR { .. } => {
            // Implicitly defines RAX (quotient) and RDX (remainder), but those are physical
        }
        X86Inst::TestRR { .. } | X86Inst::CmpRR { .. } | X86Inst::CmpRI { .. } => {
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
        X86Inst::Cdq
        | X86Inst::Jz { .. }
        | X86Inst::Jnz { .. }
        | X86Inst::Jo { .. }
        | X86Inst::Jno { .. }
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
        assert_eq!(info.ranges.get(&v0), Some(&LiveRange { start: 0, end: 1 }));
        // v1 is defined at 1, last used at 1 (no further use)
        assert_eq!(info.ranges.get(&v1), Some(&LiveRange { start: 1, end: 1 }));
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
        // v2 = v0 + v1
        mir.push(X86Inst::MovRR {
            dst: Operand::Virtual(v2),
            src: Operand::Virtual(v0),
        });
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
}
