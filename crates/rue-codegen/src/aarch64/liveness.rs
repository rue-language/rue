//! Liveness analysis for register allocation.
//!
//! This module computes which virtual registers are "live" (their values may still
//! be used) at each program point. This information is used by the register
//! allocator to determine when registers can be reused.

use std::collections::{HashMap, HashSet};

use super::mir::{Aarch64Inst, Aarch64Mir, Operand, Reg, VReg};

/// Live range for a virtual register.
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
    pub live_at: Vec<HashSet<VReg>>,
    /// For each instruction index, the physical registers clobbered by that instruction.
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

/// Compute liveness information for Aarch64Mir.
pub fn analyze(mir: &Aarch64Mir) -> LivenessInfo {
    let instructions = mir.instructions();
    let num_insts = instructions.len();

    let mut first_def: HashMap<VReg, usize> = HashMap::new();
    let mut last_use: HashMap<VReg, usize> = HashMap::new();
    let mut clobbers_at: Vec<Vec<Reg>> = Vec::with_capacity(num_insts);

    for (idx, inst) in instructions.iter().enumerate() {
        // Record uses first
        for vreg in uses(inst) {
            last_use.insert(vreg, idx);
            if !first_def.contains_key(&vreg) {
                first_def.insert(vreg, 0);
            }
        }

        // Record definitions
        for vreg in defs(inst) {
            first_def.entry(vreg).or_insert(idx);
            last_use.entry(vreg).or_insert(idx);
        }

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
fn uses(inst: &Aarch64Inst) -> Vec<VReg> {
    let mut result = Vec::new();

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
        | Aarch64Inst::SubRR { src1, src2, .. }
        | Aarch64Inst::SubsRR { src1, src2, .. }
        | Aarch64Inst::MulRR { src1, src2, .. }
        | Aarch64Inst::SmullRR { src1, src2, .. }
        | Aarch64Inst::SdivRR { src1, src2, .. }
        | Aarch64Inst::AndRR { src1, src2, .. }
        | Aarch64Inst::OrrRR { src1, src2, .. }
        | Aarch64Inst::EorRR { src1, src2, .. } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
        }
        Aarch64Inst::AddImm { src, .. } | Aarch64Inst::SubImm { src, .. } => {
            add_if_virtual(src, &mut result);
        }
        Aarch64Inst::Msub {
            src1,
            src2,
            src3,
            ..
        } => {
            add_if_virtual(src1, &mut result);
            add_if_virtual(src2, &mut result);
            add_if_virtual(src3, &mut result);
        }
        Aarch64Inst::Neg { src, .. } | Aarch64Inst::Negs { src, .. } => {
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
    let mut result = Vec::new();

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
        | Aarch64Inst::SubRR { dst, .. }
        | Aarch64Inst::SubsRR { dst, .. }
        | Aarch64Inst::AddImm { dst, .. }
        | Aarch64Inst::SubImm { dst, .. }
        | Aarch64Inst::MulRR { dst, .. }
        | Aarch64Inst::SmullRR { dst, .. }
        | Aarch64Inst::SdivRR { dst, .. }
        | Aarch64Inst::Msub { dst, .. }
        | Aarch64Inst::Neg { dst, .. }
        | Aarch64Inst::Negs { dst, .. }
        | Aarch64Inst::AndRR { dst, .. }
        | Aarch64Inst::OrrRR { dst, .. }
        | Aarch64Inst::EorRR { dst, .. }
        | Aarch64Inst::EorImm { dst, .. } => {
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

        assert_eq!(info.ranges.get(&v0), Some(&LiveRange { start: 0, end: 1 }));
        assert_eq!(info.ranges.get(&v1), Some(&LiveRange { start: 1, end: 1 }));
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
}
