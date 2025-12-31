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
use crate::regalloc::{InstructionLiveness, LiveRange, LivenessDebugInfo, LivenessInfo};
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
}
