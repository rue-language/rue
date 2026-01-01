//! Shared liveness analysis algorithm.
//!
//! This module provides a generic liveness analysis implementation that works
//! with any instruction set. Each backend provides instruction-specific
//! implementations of the [`InstructionInfo`] trait, and this module handles
//! the dataflow analysis algorithm.
//!
//! ## Architecture
//!
//! The liveness analysis is split into two parts:
//!
//! 1. **Generic algorithm** (this module): The dataflow analysis that computes
//!    live-in, live-out, and live ranges. This is completely instruction-agnostic.
//!
//! 2. **Instruction info** (per-backend): Each backend provides closures/functions
//!    that extract uses, defs, labels, and successors from its instruction type.
//!
//! This design eliminates ~800 lines of duplicated code between backends while
//! keeping the instruction-specific logic where it belongs.

use std::collections::HashMap;

use fixedbitset::FixedBitSet;

use crate::index_map::IndexMap;
use crate::regalloc::{InstructionLiveness, LiveRange, LivenessDebugInfo, LivenessInfo, LoopInfo};
use crate::vreg::{LabelId, VReg};

/// Compute liveness information using the generic dataflow algorithm.
///
/// This function performs backward dataflow analysis to compute which virtual
/// registers are live at each program point. It handles control flow by:
///
/// 1. Building a CFG from labels and branch instructions
/// 2. Computing live-out sets using backward dataflow analysis
/// 3. Building live ranges from the dataflow results
///
/// # Type Parameters
///
/// * `I` - The instruction type
/// * `R` - The physical register type
///
/// # Arguments
///
/// * `instructions` - The instruction sequence to analyze
/// * `vreg_count` - Total number of virtual registers
/// * `get_label` - Returns the label ID if the instruction is a label, None otherwise
/// * `get_successors` - Returns the successor instruction indices for control flow
/// * `get_uses` - Returns the virtual registers used (read) by the instruction
/// * `get_defs` - Returns the virtual registers defined (written) by the instruction
/// * `get_clobbers` - Returns the physical registers clobbered by the instruction
pub fn analyze<I, R>(
    instructions: &[I],
    vreg_count: u32,
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> Vec<usize>,
    get_uses: impl Fn(&I) -> Vec<VReg>,
    get_defs: impl Fn(&I) -> Vec<VReg>,
    get_clobbers: impl Fn(&I) -> Vec<R>,
) -> LivenessInfo<R>
where
    R: Copy + Eq + std::hash::Hash,
{
    let num_insts = instructions.len();

    if num_insts == 0 {
        return LivenessInfo {
            ranges: IndexMap::new(),
            live_at: Vec::new(),
            clobbers_at: Vec::new(),
        };
    }

    // Step 1: Build label -> instruction index map
    let label_to_idx = build_label_map(instructions, &get_label);

    // Step 2: Build successor lists for each instruction
    let successors = build_successor_lists(instructions, &label_to_idx, &get_successors);

    // Step 3: Pre-compute uses and defs for each instruction
    let inst_uses: Vec<Vec<VReg>> = instructions.iter().map(&get_uses).collect();
    let inst_defs: Vec<Vec<VReg>> = instructions.iter().map(&get_defs).collect();

    // Step 4: Backward dataflow analysis to compute live sets
    let (live_in, live_out) =
        compute_dataflow(num_insts, vreg_count, &successors, &inst_uses, &inst_defs);

    // Step 5: Build live ranges from dataflow results
    let ranges = build_live_ranges(
        num_insts, vreg_count, &inst_uses, &inst_defs, &live_in, &live_out,
    );

    // Step 6: Compute live_at for each instruction (union of live_in and live_out)
    let live_at = compute_live_at(num_insts, vreg_count, &live_in, &live_out);

    // Step 7: Collect clobbers
    let clobbers_at: Vec<Vec<R>> = instructions.iter().map(|i| get_clobbers(i)).collect();

    LivenessInfo {
        ranges,
        live_at,
        clobbers_at,
    }
}

/// Compute detailed liveness debug information.
///
/// This provides more detailed output than [`analyze`], including per-instruction
/// live-in/live-out sets and def/use information. Used by `--emit liveness`.
pub fn analyze_debug<I, R>(
    instructions: &[I],
    vreg_count: u32,
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> Vec<usize>,
    get_uses: impl Fn(&I) -> Vec<VReg>,
    get_defs: impl Fn(&I) -> Vec<VReg>,
) -> LivenessDebugInfo
where
    R: Copy + Eq + std::hash::Hash,
{
    let num_insts = instructions.len();

    if num_insts == 0 {
        return LivenessDebugInfo {
            instructions: Vec::new(),
            live_ranges: IndexMap::new(),
            vreg_count,
        };
    }

    // Step 1: Build label -> instruction index map
    let label_to_idx = build_label_map(instructions, &get_label);

    // Step 2: Build successor lists for each instruction
    let successors = build_successor_lists(instructions, &label_to_idx, &get_successors);

    // Step 3: Pre-compute uses and defs for each instruction
    let inst_uses: Vec<Vec<VReg>> = instructions.iter().map(&get_uses).collect();
    let inst_defs: Vec<Vec<VReg>> = instructions.iter().map(&get_defs).collect();

    // Step 4: Backward dataflow analysis to compute live sets
    let (live_in, live_out) =
        compute_dataflow(num_insts, vreg_count, &successors, &inst_uses, &inst_defs);

    // Step 5: Build live ranges from dataflow results
    let live_ranges = build_live_ranges(
        num_insts, vreg_count, &inst_uses, &inst_defs, &live_in, &live_out,
    );

    // Step 6: Build per-instruction liveness info
    let bitset_to_hashset = |bs: &FixedBitSet| -> std::collections::HashSet<VReg> {
        bs.ones().map(|idx| VReg::new(idx as u32)).collect()
    };

    let instruction_liveness: Vec<InstructionLiveness> = (0..num_insts)
        .map(|idx| InstructionLiveness {
            index: idx,
            live_in: bitset_to_hashset(&live_in[idx]),
            live_out: bitset_to_hashset(&live_out[idx]),
            defs: inst_defs[idx].clone(),
            uses: inst_uses[idx].clone(),
        })
        .collect();

    LivenessDebugInfo {
        instructions: instruction_liveness,
        live_ranges,
        vreg_count,
    }
}

// ============================================================================
// Internal helper functions
// ============================================================================

/// Build a map from label IDs to instruction indices.
fn build_label_map<I>(
    instructions: &[I],
    get_label: impl Fn(&I) -> Option<LabelId>,
) -> HashMap<LabelId, usize> {
    let mut label_to_idx = HashMap::new();
    for (idx, inst) in instructions.iter().enumerate() {
        if let Some(label) = get_label(inst) {
            label_to_idx.insert(label, idx);
        }
    }
    label_to_idx
}

/// Build successor lists for each instruction.
fn build_successor_lists<I>(
    instructions: &[I],
    label_to_idx: &HashMap<LabelId, usize>,
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> Vec<usize>,
) -> Vec<Vec<usize>> {
    instructions
        .iter()
        .enumerate()
        .map(|(idx, inst)| get_successors(idx, inst, label_to_idx))
        .collect()
}

/// Perform backward dataflow analysis to compute live-in and live-out sets.
///
/// Uses the standard dataflow equations:
/// - live_out[i] = union of live_in[s] for all successors s of i
/// - live_in[i] = uses[i] ∪ (live_out[i] - defs[i])
fn compute_dataflow(
    num_insts: usize,
    vreg_count: u32,
    successors: &[Vec<usize>],
    inst_uses: &[Vec<VReg>],
    inst_defs: &[Vec<VReg>],
) -> (Vec<FixedBitSet>, Vec<FixedBitSet>) {
    let vreg_count_usize = vreg_count as usize;

    let mut live_in: Vec<FixedBitSet> =
        vec![FixedBitSet::with_capacity(vreg_count_usize); num_insts];
    let mut live_out: Vec<FixedBitSet> =
        vec![FixedBitSet::with_capacity(vreg_count_usize); num_insts];

    // Iterate until fixed point
    let mut changed = true;
    while changed {
        changed = false;

        // Process instructions in reverse order for faster convergence
        for idx in (0..num_insts).rev() {
            // Compute live_out as union of live_in of all successors
            let mut new_live_out = FixedBitSet::with_capacity(vreg_count_usize);
            for &succ in &successors[idx] {
                new_live_out.union_with(&live_in[succ]);
            }

            // Compute live_in = uses ∪ (live_out - defs)
            let mut new_live_in = new_live_out.clone();
            for vreg in &inst_defs[idx] {
                new_live_in.set(vreg.index() as usize, false);
            }
            for vreg in &inst_uses[idx] {
                new_live_in.insert(vreg.index() as usize);
            }

            // Check if anything changed
            if new_live_in != live_in[idx] || new_live_out != live_out[idx] {
                changed = true;
                live_in[idx] = new_live_in;
                live_out[idx] = new_live_out;
            }
        }
    }

    (live_in, live_out)
}

/// Build live ranges from dataflow results.
fn build_live_ranges(
    num_insts: usize,
    vreg_count: u32,
    inst_uses: &[Vec<VReg>],
    inst_defs: &[Vec<VReg>],
    live_in: &[FixedBitSet],
    live_out: &[FixedBitSet],
) -> IndexMap<VReg, Option<LiveRange>> {
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
        for vreg_idx in live_in[idx].ones() {
            let vreg = VReg::new(vreg_idx as u32);
            first_live.entry(vreg).or_insert(idx);
            if last_live.get(&vreg).is_none_or(|&last| idx > last) {
                last_live.insert(vreg, idx);
            }
        }
        // Check live_out
        for vreg_idx in live_out[idx].ones() {
            let vreg = VReg::new(vreg_idx as u32);
            first_live.entry(vreg).or_insert(idx);
            if last_live.get(&vreg).is_none_or(|&last| idx > last) {
                last_live.insert(vreg, idx);
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

    ranges
}

/// Compute live_at sets (union of live_in and live_out for each instruction).
fn compute_live_at(
    num_insts: usize,
    vreg_count: u32,
    live_in: &[FixedBitSet],
    live_out: &[FixedBitSet],
) -> Vec<FixedBitSet> {
    let vreg_count_usize = vreg_count as usize;
    let mut live_at: Vec<FixedBitSet> =
        vec![FixedBitSet::with_capacity(vreg_count_usize); num_insts];

    for (idx, (li, lo)) in live_in.iter().zip(live_out.iter()).enumerate() {
        live_at[idx].union_with(li);
        live_at[idx].union_with(lo);
    }

    live_at
}

// ============================================================================
// Loop Detection
// ============================================================================

/// Detect loops and compute loop depth for each instruction.
///
/// A loop is detected by finding back-edges: edges where a successor index is
/// less than or equal to the current instruction index. This indicates a jump
/// back to an earlier point in the code (a loop).
///
/// # Algorithm
///
/// 1. Identify back-edges by finding successors[i] where successor <= i
/// 2. For each back-edge (from -> to), mark all instructions in [to, from] as in a loop
/// 3. Handle nested loops by tracking depth (incremented for each enclosing loop)
///
/// # Arguments
///
/// * `num_insts` - Total number of instructions
/// * `successors` - Successor indices for each instruction
///
/// # Returns
///
/// A `LoopInfo` with loop depth for each instruction.
pub fn compute_loop_info(num_insts: usize, successors: &[Vec<usize>]) -> LoopInfo {
    if num_insts == 0 {
        return LoopInfo::no_loops(0);
    }

    // Find all back-edges: edges where we jump to an earlier or same instruction
    // A back-edge from instruction `from` to instruction `to` (where to <= from)
    // indicates a loop from `to` to `from`
    let mut loop_ranges: Vec<(usize, usize)> = Vec::new();

    for (from, succs) in successors.iter().enumerate() {
        for &to in succs {
            if to <= from {
                // This is a back-edge: we're jumping backwards
                // The loop spans from `to` (loop header) to `from` (back-edge source)
                loop_ranges.push((to, from));
            }
        }
    }

    // Sort loop ranges by start point for consistent processing
    loop_ranges.sort_by_key(|(start, _)| *start);

    // Compute loop depth for each instruction
    // Each loop range [start, end] increments the depth of all instructions in that range
    let mut depths = vec![0u32; num_insts];

    for (loop_start, loop_end) in &loop_ranges {
        for idx in *loop_start..=*loop_end {
            depths[idx] = depths[idx].saturating_add(1);
        }
    }

    LoopInfo { depths }
}

/// Compute loop info from instructions using the provided callbacks.
///
/// This is a convenience function that builds the label map and successor lists,
/// then calls `compute_loop_info`.
pub fn analyze_loops<I>(
    instructions: &[I],
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> Vec<usize>,
) -> LoopInfo {
    let num_insts = instructions.len();

    if num_insts == 0 {
        return LoopInfo::no_loops(0);
    }

    // Build label -> instruction index map
    let label_to_idx = build_label_map(instructions, &get_label);

    // Build successor lists
    let successors = build_successor_lists(instructions, &label_to_idx, &get_successors);

    compute_loop_info(num_insts, &successors)
}

// ============================================================================
// Pressure Analysis
// ============================================================================

/// Register pressure at each instruction.
///
/// This tracks how many virtual registers are live at each program point,
/// which helps the register allocator make better spill decisions.
#[derive(Debug, Clone)]
pub struct PressureInfo {
    /// Number of live vregs at each instruction index.
    pub pressure: Vec<u32>,
    /// Maximum pressure across all instructions.
    pub max_pressure: u32,
}

impl PressureInfo {
    /// Get the pressure at a specific instruction.
    pub fn at(&self, inst_idx: usize) -> u32 {
        self.pressure.get(inst_idx).copied().unwrap_or(0)
    }

    /// Find instructions where pressure exceeds a threshold.
    pub fn high_pressure_points(&self, threshold: u32) -> Vec<usize> {
        self.pressure
            .iter()
            .enumerate()
            .filter(|&(_, &p)| p > threshold)
            .map(|(idx, _)| idx)
            .collect()
    }
}

/// Compute register pressure from live_at sets.
///
/// Pressure is simply the count of live vregs at each instruction.
pub fn compute_pressure(live_at: &[FixedBitSet]) -> PressureInfo {
    let pressure: Vec<u32> = live_at.iter().map(|bs| bs.count_ones(..) as u32).collect();
    let max_pressure = pressure.iter().copied().max().unwrap_or(0);

    PressureInfo {
        pressure,
        max_pressure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Simple test instruction type
    #[derive(Debug, Clone)]
    enum TestInst {
        Def { dst: u32 },
        Use { src: u32 },
        Move { dst: u32, src: u32 },
        Label { id: LabelId },
        Jump { label: LabelId },
        Branch { label: LabelId },
        Ret,
    }

    fn test_get_label(inst: &TestInst) -> Option<LabelId> {
        match inst {
            TestInst::Label { id } => Some(*id),
            _ => None,
        }
    }

    fn test_get_successors(
        idx: usize,
        inst: &TestInst,
        label_to_idx: &HashMap<LabelId, usize>,
        num_insts: usize,
    ) -> Vec<usize> {
        match inst {
            TestInst::Jump { label } => label_to_idx.get(label).copied().into_iter().collect(),
            TestInst::Branch { label } => {
                let mut succs = Vec::new();
                if idx + 1 < num_insts {
                    succs.push(idx + 1);
                }
                if let Some(&target) = label_to_idx.get(label) {
                    succs.push(target);
                }
                succs
            }
            TestInst::Ret => Vec::new(),
            _ => {
                if idx + 1 < num_insts {
                    vec![idx + 1]
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn test_get_uses(inst: &TestInst) -> Vec<VReg> {
        match inst {
            TestInst::Use { src } => vec![VReg::new(*src)],
            TestInst::Move { src, .. } => vec![VReg::new(*src)],
            _ => Vec::new(),
        }
    }

    fn test_get_defs(inst: &TestInst) -> Vec<VReg> {
        match inst {
            TestInst::Def { dst } => vec![VReg::new(*dst)],
            TestInst::Move { dst, .. } => vec![VReg::new(*dst)],
            _ => Vec::new(),
        }
    }

    fn test_get_clobbers(_inst: &TestInst) -> Vec<u32> {
        Vec::new()
    }

    #[test]
    fn test_simple_liveness() {
        let instructions = vec![
            TestInst::Def { dst: 0 },          // v0 = ...
            TestInst::Move { dst: 1, src: 0 }, // v1 = v0
        ];
        let num_insts = instructions.len();

        let info: LivenessInfo<u32> = analyze(
            &instructions,
            2,
            test_get_label,
            |idx, inst, label_to_idx| test_get_successors(idx, inst, label_to_idx, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
        );

        // v0: defined at 0, used at 1
        assert_eq!(info.range(VReg::new(0)), Some(&LiveRange::new(0, 1)));
        // v1: defined at 1, not used after
        assert_eq!(info.range(VReg::new(1)), Some(&LiveRange::new(1, 1)));
    }

    #[test]
    fn test_liveness_with_branch() {
        let label = LabelId::new(0);
        let instructions = vec![
            TestInst::Def { dst: 0 },      // 0: v0 = ...
            TestInst::Branch { label },    // 1: if (...) goto label
            TestInst::Use { src: 0 },      // 2: ... = v0 (fall-through)
            TestInst::Label { id: label }, // 3: label:
            TestInst::Use { src: 0 },      // 4: ... = v0 (both paths)
            TestInst::Ret,                 // 5: return
        ];
        let num_insts = instructions.len();

        let info: LivenessInfo<u32> = analyze(
            &instructions,
            1,
            test_get_label,
            |idx, inst, label_to_idx| test_get_successors(idx, inst, label_to_idx, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
        );

        // v0: defined at 0, last used at 4
        let range = info.range(VReg::new(0)).expect("v0 should have a range");
        assert_eq!(range.start, 0);
        assert!(range.end >= 4);
    }

    #[test]
    fn test_empty_instructions() {
        let instructions: Vec<TestInst> = vec![];
        let num_insts = instructions.len();

        let info: LivenessInfo<u32> = analyze(
            &instructions,
            0,
            test_get_label,
            |idx, inst, label_to_idx| test_get_successors(idx, inst, label_to_idx, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
        );

        assert!(info.ranges.is_empty());
        assert!(info.live_at.is_empty());
        assert!(info.clobbers_at.is_empty());
    }

    #[test]
    fn test_interference() {
        let instructions = vec![
            TestInst::Def { dst: 0 }, // 0: v0 = ...
            TestInst::Def { dst: 1 }, // 1: v1 = ...
            TestInst::Use { src: 0 }, // 2: ... = v0
            TestInst::Use { src: 1 }, // 3: ... = v1
        ];
        let num_insts = instructions.len();

        let info: LivenessInfo<u32> = analyze(
            &instructions,
            2,
            test_get_label,
            |idx, inst, label_to_idx| test_get_successors(idx, inst, label_to_idx, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
        );

        // v0 and v1 should interfere (both live at instruction 2)
        assert!(info.interferes(VReg::new(0), VReg::new(1)));
    }

    // ========================================
    // Loop detection tests
    // ========================================

    #[test]
    fn test_no_loops() {
        // Linear code: no back-edges
        // 0 -> 1 -> 2 -> 3
        let successors = vec![vec![1], vec![2], vec![3], vec![]];
        let loop_info = compute_loop_info(4, &successors);

        // All instructions should have depth 0
        for i in 0..4 {
            assert_eq!(
                loop_info.depth(i),
                0,
                "Instruction {} should be at depth 0",
                i
            );
        }
    }

    #[test]
    fn test_simple_loop() {
        // Simple loop: 0 -> 1 -> 2 -> 1 (back-edge from 2 to 1)
        //              |         |
        //              v         v
        //              1 <-------+
        //              |
        //              v
        //              3 (exit)
        //
        // Instructions 1-2 are in the loop
        let successors = vec![
            vec![1],    // 0 -> 1
            vec![2],    // 1 -> 2
            vec![1, 3], // 2 -> 1 (back-edge), 2 -> 3 (exit)
            vec![],     // 3 (end)
        ];
        let loop_info = compute_loop_info(4, &successors);

        assert_eq!(loop_info.depth(0), 0, "Before loop");
        assert_eq!(loop_info.depth(1), 1, "Loop header");
        assert_eq!(loop_info.depth(2), 1, "Loop body");
        assert_eq!(loop_info.depth(3), 0, "After loop");
    }

    #[test]
    fn test_nested_loops() {
        // Nested loops:
        // 0 -> 1 -> 2 -> 3 -> 2 (inner back-edge)
        //      |         |
        //      |         v
        //      |         4 -> 1 (outer back-edge)
        //      |              |
        //      |              v
        //      +------------> 5 (exit)
        //
        // Outer loop: 1-4 (depth 1)
        // Inner loop: 2-3 (depth 2)
        let successors = vec![
            vec![1],    // 0 -> 1
            vec![2],    // 1 -> 2
            vec![3],    // 2 -> 3
            vec![2, 4], // 3 -> 2 (inner back-edge), 3 -> 4
            vec![1, 5], // 4 -> 1 (outer back-edge), 4 -> 5
            vec![],     // 5 (end)
        ];
        let loop_info = compute_loop_info(6, &successors);

        assert_eq!(loop_info.depth(0), 0, "Before loops");
        assert_eq!(loop_info.depth(1), 1, "Outer loop header");
        assert_eq!(loop_info.depth(2), 2, "Inner loop header (nested)");
        assert_eq!(loop_info.depth(3), 2, "Inner loop body (nested)");
        assert_eq!(loop_info.depth(4), 1, "Outer loop tail");
        assert_eq!(loop_info.depth(5), 0, "After loops");
    }

    #[test]
    fn test_loop_info_max_depth_in_range() {
        // Same nested loop structure as above
        let successors = vec![vec![1], vec![2], vec![3], vec![2, 4], vec![1, 5], vec![]];
        let loop_info = compute_loop_info(6, &successors);

        // Range spanning inner loop should have max depth 2
        assert_eq!(loop_info.max_depth_in_range(1, 4), 2);
        // Range outside loops
        assert_eq!(loop_info.max_depth_in_range(0, 0), 0);
        assert_eq!(loop_info.max_depth_in_range(5, 5), 0);
        // Range spanning only outer loop
        assert_eq!(loop_info.max_depth_in_range(1, 1), 1);
        assert_eq!(loop_info.max_depth_in_range(4, 4), 1);
    }

    #[test]
    fn test_analyze_loops_with_instructions() {
        // Test the high-level analyze_loops function
        let loop_label = LabelId::new(0);
        let instructions = vec![
            TestInst::Def { dst: 0 },               // 0: v0 = 10
            TestInst::Label { id: loop_label },     // 1: loop:
            TestInst::Use { src: 0 },               // 2: use v0
            TestInst::Branch { label: loop_label }, // 3: if (...) goto loop (back-edge!)
            TestInst::Ret,                          // 4: return
        ];
        let num_insts = instructions.len();

        let loop_info = analyze_loops(&instructions, test_get_label, |idx, inst, label_to_idx| {
            test_get_successors(idx, inst, label_to_idx, num_insts)
        });

        assert_eq!(loop_info.depth(0), 0, "Before loop");
        assert_eq!(loop_info.depth(1), 1, "Loop header");
        assert_eq!(loop_info.depth(2), 1, "Loop body");
        assert_eq!(loop_info.depth(3), 1, "Loop back-edge");
        assert_eq!(loop_info.depth(4), 0, "After loop");
    }

    // ========================================
    // Pressure analysis tests
    // ========================================

    #[test]
    fn test_pressure_simple() {
        let vreg_count = 3;
        let mut live_at = vec![
            FixedBitSet::with_capacity(vreg_count),
            FixedBitSet::with_capacity(vreg_count),
            FixedBitSet::with_capacity(vreg_count),
        ];

        // Instruction 0: 1 vreg live
        live_at[0].insert(0);

        // Instruction 1: 2 vregs live
        live_at[1].insert(0);
        live_at[1].insert(1);

        // Instruction 2: 3 vregs live
        live_at[2].insert(0);
        live_at[2].insert(1);
        live_at[2].insert(2);

        let pressure = compute_pressure(&live_at);

        assert_eq!(pressure.at(0), 1);
        assert_eq!(pressure.at(1), 2);
        assert_eq!(pressure.at(2), 3);
        assert_eq!(pressure.max_pressure, 3);
    }

    #[test]
    fn test_high_pressure_points() {
        let vreg_count = 5;
        let mut live_at = vec![
            FixedBitSet::with_capacity(vreg_count),
            FixedBitSet::with_capacity(vreg_count),
            FixedBitSet::with_capacity(vreg_count),
            FixedBitSet::with_capacity(vreg_count),
        ];

        // Low pressure at 0 and 3
        live_at[0].insert(0);
        live_at[3].insert(0);

        // High pressure at 1 and 2
        for i in 0..5 {
            live_at[1].insert(i);
            live_at[2].insert(i);
        }

        let pressure = compute_pressure(&live_at);
        let high_points = pressure.high_pressure_points(3);

        assert_eq!(high_points, vec![1, 2]);
    }
}
