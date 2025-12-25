//! Shared register allocation algorithm and types.
//!
//! This module provides a target-independent linear scan register allocator
//! and the liveness analysis types used by all backends.
//!
//! ## Liveness Analysis
//!
//! The module provides target-independent types for liveness analysis:
//! - [`LiveRange`]: Represents the instruction range where a vreg's value is needed
//! - [`LivenessInfo`]: Holds all liveness information (ranges, live_at, clobbers)
//!
//! Each backend implements its own `analyze()` function that populates these types
//! based on its specific instruction set and control flow.
//!
//! ## Register Allocation Algorithm
//!
//! The allocator uses linear scan register allocation:
//! 1. Compute live ranges for all virtual registers (via liveness analysis)
//! 2. Sort vregs by live range start
//! 3. For each vreg, try to assign a register not used by interfering vregs
//! 4. If no register is available, spill the longest-range vreg to stack
//!
//! ## Spilling
//!
//! When register pressure exceeds available registers, values are spilled
//! to the stack. The allocator uses a heuristic that spills the vreg with
//! the longest remaining live range, as this frees up a register for the
//! longest time.

use std::collections::{HashMap, HashSet};

use crate::index_map::IndexMap;
use crate::vreg::VReg;

// ============================================================================
// Liveness Analysis Types
// ============================================================================

/// Live range for a virtual register.
///
/// Represents the instruction range where this vreg's value is needed.
/// Live ranges are [start, end] inclusive intervals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveRange {
    /// Instruction index where the vreg is defined (first write).
    pub start: usize,
    /// Instruction index where the vreg is last used (last read).
    pub end: usize,
}

impl LiveRange {
    /// Create a new live range.
    #[inline]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Check if this live range overlaps with another.
    ///
    /// Two ranges overlap if they share at least one instruction index.
    #[inline]
    pub fn overlaps(&self, other: &LiveRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// Result of liveness analysis.
///
/// This struct is target-independent and holds all the information needed
/// by the register allocator. Each backend's `analyze()` function populates
/// an instance of this type.
pub struct LivenessInfo<Reg: Copy + Eq + std::hash::Hash> {
    /// Live range for each virtual register.
    pub ranges: HashMap<VReg, LiveRange>,
    /// For each instruction, which vregs are live after it executes.
    /// This is useful for determining which registers are in use at any point.
    pub live_at: Vec<HashSet<VReg>>,
    /// For each instruction index, the physical registers clobbered by that instruction.
    /// This is used to prevent allocating vregs to registers that would be clobbered.
    pub clobbers_at: Vec<Vec<Reg>>,
}

impl<Reg: Copy + Eq + std::hash::Hash> LivenessInfo<Reg> {
    /// Create a new empty liveness info.
    pub fn new() -> Self {
        Self {
            ranges: HashMap::new(),
            live_at: Vec::new(),
            clobbers_at: Vec::new(),
        }
    }

    /// Get vregs that are live at a given instruction index.
    pub fn live_at(&self, inst_idx: usize) -> &HashSet<VReg> {
        &self.live_at[inst_idx]
    }

    /// Get the live range for a vreg.
    pub fn range(&self, vreg: VReg) -> Option<&LiveRange> {
        self.ranges.get(&vreg)
    }

    /// Check if two vregs interfere (have overlapping live ranges).
    ///
    /// Two vregs interfere if they are both live at the same program point,
    /// meaning they cannot share the same physical register.
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
    /// This is used to prevent allocating a vreg to a register that would be clobbered
    /// before the vreg's last use.
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

impl<Reg: Copy + Eq + std::hash::Hash> Default for LivenessInfo<Reg> {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Register Allocation Types
// ============================================================================

/// Allocation result for a virtual register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allocation<Reg: Copy> {
    /// Allocated to a physical register.
    Register(Reg),
    /// Spilled to a stack slot (offset from frame pointer).
    Spill(i32),
}

/// Perform linear scan register allocation.
///
/// This function implements the core linear scan algorithm that is shared
/// between all backends. It takes liveness information and a list of
/// allocatable registers, and returns an allocation for each vreg.
///
/// # Arguments
///
/// * `vreg_count` - Total number of virtual registers
/// * `liveness` - Liveness information from dataflow analysis
/// * `allocatable_regs` - Physical registers available for allocation
/// * `existing_locals` - Number of local variable slots already on the stack
///
/// # Returns
///
/// A tuple of:
/// * `IndexMap<VReg, Option<Allocation<Reg>>>` - Allocation for each vreg
/// * `u32` - Number of spill slots used
/// * `Vec<Reg>` - Callee-saved registers that were used
pub fn linear_scan<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    allocatable_regs: &[Reg],
    existing_locals: u32,
) -> (IndexMap<VReg, Option<Allocation<Reg>>>, u32, Vec<Reg>) {
    let vreg_count_usize = vreg_count as usize;

    // Initialize allocation map
    let mut allocation: IndexMap<VReg, Option<Allocation<Reg>>> =
        IndexMap::with_capacity(vreg_count_usize);
    allocation.resize(vreg_count_usize, None);

    // Spill slots start after existing locals
    // Each local is 8 bytes, slot 0 is at [fp-8], etc.
    let mut next_spill_offset = -((existing_locals as i32 + 1) * 8);
    let mut num_spills = 0u32;
    let mut used_callee_saved: Vec<Reg> = Vec::new();

    // Collect vregs with live ranges and sort by start
    let mut vregs_by_start: Vec<(VReg, LiveRange)> = Vec::with_capacity(vreg_count_usize);
    for vreg_idx in 0..vreg_count {
        let vreg = VReg::new(vreg_idx);
        if let Some(&range) = liveness.range(vreg) {
            vregs_by_start.push((vreg, range));
        }
    }
    vregs_by_start.sort_by_key(|(_, range)| range.start);

    // Track which registers are currently in use and when they become free
    // Tuple: (vreg, physical reg, live range end)
    let mut active: Vec<(VReg, Reg, usize)> = Vec::with_capacity(allocatable_regs.len());

    for (vreg, range) in vregs_by_start {
        // Expire old intervals - remove registers whose vregs are no longer live
        active.retain(|&(_, _, end)| end >= range.start);

        // Find registers currently in use
        let used_regs: HashSet<Reg> = active.iter().map(|&(_, reg, _)| reg).collect();

        // Try to find a free register
        let mut allocated_reg = None;
        for &reg in allocatable_regs {
            if !used_regs.contains(&reg) {
                allocated_reg = Some(reg);
                break;
            }
        }

        if let Some(reg) = allocated_reg {
            // Assign this register
            allocation[vreg] = Some(Allocation::Register(reg));
            active.push((vreg, reg, range.end));
            // Track callee-saved register usage
            if !used_callee_saved.contains(&reg) {
                used_callee_saved.push(reg);
            }
        } else {
            // No free register - need to spill
            // Strategy: spill the vreg with the longest remaining live range
            // (including the current one)

            // Find the vreg with the longest remaining range
            let mut longest_idx = None;
            let mut longest_end = range.end;
            for (i, &(_, _, end)) in active.iter().enumerate() {
                if end > longest_end {
                    longest_end = end;
                    longest_idx = Some(i);
                }
            }

            if let Some(idx) = longest_idx {
                // Spill the existing vreg with longest range
                let (spilled_vreg, freed_reg, _) = active.remove(idx);
                let spill_offset = next_spill_offset;
                next_spill_offset -= 8;
                num_spills += 1;
                allocation[spilled_vreg] = Some(Allocation::Spill(spill_offset));

                // Give the freed register to the current vreg
                allocation[vreg] = Some(Allocation::Register(freed_reg));
                active.push((vreg, freed_reg, range.end));
            } else {
                // Current vreg has the longest range, spill it
                let spill_offset = next_spill_offset;
                next_spill_offset -= 8;
                num_spills += 1;
                allocation[vreg] = Some(Allocation::Spill(spill_offset));
            }
        }
    }

    (allocation, num_spills, used_callee_saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================
    // LiveRange tests
    // ========================================

    #[test]
    fn test_live_range_overlaps() {
        let r1 = LiveRange::new(0, 5);
        let r2 = LiveRange::new(3, 8);
        let r3 = LiveRange::new(6, 10);

        // r1 and r2 overlap at 3-5
        assert!(r1.overlaps(&r2));
        assert!(r2.overlaps(&r1));

        // r1 and r3 don't overlap (r1 ends at 5, r3 starts at 6)
        assert!(!r1.overlaps(&r3));
        assert!(!r3.overlaps(&r1));

        // r2 and r3 overlap at 6-8
        assert!(r2.overlaps(&r3));
        assert!(r3.overlaps(&r2));
    }

    #[test]
    fn test_live_range_adjacent_not_overlapping() {
        // Adjacent ranges should overlap (inclusive end)
        let r1 = LiveRange::new(0, 5);
        let r2 = LiveRange::new(5, 10);

        // At instruction 5, both ranges are active
        assert!(r1.overlaps(&r2));
    }

    #[test]
    fn test_live_range_same_point() {
        let r1 = LiveRange::new(5, 5);
        let r2 = LiveRange::new(5, 5);

        assert!(r1.overlaps(&r2));
    }

    // ========================================
    // Linear scan allocation tests
    // ========================================

    // Simple test register type
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct TestReg(u32);

    fn make_liveness(ranges: Vec<(u32, usize, usize)>) -> LivenessInfo<TestReg> {
        let mut info = LivenessInfo::new();
        for (vreg_idx, start, end) in ranges {
            info.ranges
                .insert(VReg::new(vreg_idx), LiveRange::new(start, end));
        }
        // Initialize live_at and clobbers_at based on max instruction index
        let max_inst = info.ranges.values().map(|r| r.end).max().unwrap_or(0);
        info.live_at = vec![HashSet::new(); max_inst + 1];
        info.clobbers_at = vec![Vec::new(); max_inst + 1];
        info
    }

    #[test]
    fn test_simple_allocation() {
        let allocatable = vec![TestReg(0), TestReg(1), TestReg(2)];
        let liveness = make_liveness(vec![(0, 0, 1)]);

        let (allocation, num_spills, used) = linear_scan(1, &liveness, &allocatable, 0);

        assert_eq!(num_spills, 0);
        assert_eq!(used.len(), 1);
        assert_eq!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(TestReg(0)))
        );
    }

    #[test]
    fn test_non_overlapping_share_register() {
        // Two vregs with non-overlapping ranges can share a register
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 1), // v0 lives from 0-1
            (1, 2, 3), // v1 lives from 2-3 (after v0 is dead)
        ]);

        let (allocation, num_spills, _) = linear_scan(2, &liveness, &allocatable, 0);

        assert_eq!(num_spills, 0);
        // Both should get the same register
        assert_eq!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(TestReg(0)))
        );
        assert_eq!(
            allocation[VReg::new(1)],
            Some(Allocation::Register(TestReg(0)))
        );
    }

    #[test]
    fn test_overlapping_different_registers() {
        // Two overlapping vregs need different registers
        let allocatable = vec![TestReg(0), TestReg(1)];
        let liveness = make_liveness(vec![
            (0, 0, 3), // v0 lives from 0-3
            (1, 1, 2), // v1 lives from 1-2 (overlaps with v0)
        ]);

        let (allocation, num_spills, used) = linear_scan(2, &liveness, &allocatable, 0);

        assert_eq!(num_spills, 0);
        assert_eq!(used.len(), 2);
        // Should have different registers
        assert_ne!(allocation[VReg::new(0)], allocation[VReg::new(1)]);
    }

    #[test]
    fn test_spilling() {
        // More vregs than registers forces spilling
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 5), // v0 lives from 0-5
            (1, 1, 4), // v1 lives from 1-4 (overlaps, will force spill)
        ]);

        let (allocation, num_spills, _) = linear_scan(2, &liveness, &allocatable, 0);

        assert_eq!(num_spills, 1);
        // The longer-lived vreg should be spilled
        assert!(matches!(
            allocation[VReg::new(0)],
            Some(Allocation::Spill(_))
        ));
        assert!(matches!(
            allocation[VReg::new(1)],
            Some(Allocation::Register(_))
        ));
    }

    #[test]
    fn test_spill_offset() {
        // Verify spill offsets are calculated correctly
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 10), // v0 - longest, will be spilled
            (1, 1, 9),  // v1 - second longest, will be spilled
            (2, 2, 8),  // v2 - gets the register
        ]);

        let (allocation, num_spills, _) = linear_scan(3, &liveness, &allocatable, 2);

        assert_eq!(num_spills, 2);

        // With 2 existing locals, first spill is at -24 (= -((2+1)*8))
        // Second spill is at -32
        let spill0 = match allocation[VReg::new(0)] {
            Some(Allocation::Spill(off)) => off,
            _ => panic!("v0 should be spilled"),
        };
        let spill1 = match allocation[VReg::new(1)] {
            Some(Allocation::Spill(off)) => off,
            _ => panic!("v1 should be spilled"),
        };

        assert_eq!(spill0, -24); // First spill
        assert_eq!(spill1, -32); // Second spill
    }
}
