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
use std::iter::Take;
use std::ops::Deref;

use fixedbitset::FixedBitSet;

use crate::index_map::IndexMap;
use crate::reg_class::VRegClasses;
use crate::regalloc::{InstructionLiveness, LiveRange, LivenessDebugInfo, LivenessInfo, LoopInfo};
use crate::vreg::{LabelId, VReg};

/// A fixed-capacity, inline list for bounded machine-instruction facts.
///
/// MIR defines exact small maxima for the facts liveness extracts. Keeping the
/// storage inline removes per-instruction allocation without making the outer
/// fact tables wider than their former `Vec` elements. `push` fails loudly if
/// a future instruction exceeds the audited bound, so extending MIR requires
/// extending the representation in the same change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineList<T, const N: usize> {
    values: [T; N],
    len: usize,
}

impl<T, const N: usize> InlineList<T, N>
where
    T: Copy + Default,
{
    /// Create an empty list backed by its inline storage.
    #[must_use]
    pub fn new() -> Self {
        Self {
            values: [T::default(); N],
            len: 0,
        }
    }

    /// Append one fact to the list.
    ///
    /// # Panics
    ///
    /// Panics when the list's audited MIR-specific capacity is exceeded.
    pub fn push(&mut self, value: T) {
        assert!(self.len < N, "inline liveness fact capacity {N} exceeded");
        self.values[self.len] = value;
        self.len += 1;
    }
}

impl<T, const N: usize> Default for InlineList<T, N>
where
    T: Copy + Default,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for InlineList<T, N> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.values[..self.len]
    }
}

impl<T, const N: usize> AsRef<[T]> for InlineList<T, N> {
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T, const N: usize> FromIterator<T> for InlineList<T, N>
where
    T: Copy + Default,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut values = Self::new();
        for value in iter {
            values.push(value);
        }
        values
    }
}

impl<T, const N: usize> IntoIterator for InlineList<T, N> {
    type Item = T;
    type IntoIter = Take<std::array::IntoIter<T, N>>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.into_iter().take(self.len)
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a InlineList<T, N> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Virtual-register facts attached to one machine instruction.
///
/// Current MIR forms have at most three virtual operands; four slots preserve
/// one slot of headroom without making this representation wider than `Vec`.
pub type VRegList = InlineList<VReg, 4>;

/// Control-flow successors attached to one machine instruction.
///
/// MIR instructions have at most a fallthrough and one branch target, so the
/// complete successor list fits inline.
pub type SuccessorList = InlineList<usize, 2>;

/// Backend-specific facts required by the shared liveness orchestration.
///
/// Backends own instruction semantics (`uses`, `defs`, labels, successors, and
/// physical-register clobbers). This module owns the repeated wrapper shape:
/// fetch the instruction slice and vreg count, then run normal liveness,
/// debug-liveness, or loop analysis through the same callbacks.
pub trait LivenessAdapter {
    /// Backend MIR instruction type.
    type Inst;
    /// Backend physical register type.
    type Reg: Copy + Eq + std::hash::Hash;

    /// Instruction sequence to analyze.
    fn instructions(&self) -> &[Self::Inst];

    /// Number of virtual registers allocated in the MIR.
    fn vreg_count(&self) -> u32;

    /// The register class of every virtual register in the MIR.
    ///
    /// The MIR records this as it mints registers, and liveness copies it into
    /// [`LivenessInfo::vreg_classes`] so allocation and coalescing read one
    /// authoritative table. The returned table must have one entry per
    /// register counted by [`LivenessAdapter::vreg_count`] (RUE-1067).
    fn vreg_classes(&self) -> &VRegClasses;

    /// Return the label ID if `inst` is a label.
    fn label(&self, inst: &Self::Inst) -> Option<LabelId>;

    /// Return successor instruction indices for `inst`.
    fn successors(
        &self,
        idx: usize,
        inst: &Self::Inst,
        label_to_idx: &HashMap<LabelId, usize>,
    ) -> SuccessorList;

    /// Return virtual registers read by `inst`.
    fn uses(&self, inst: &Self::Inst) -> VRegList;

    /// Return virtual registers written by `inst`.
    fn defs(&self, inst: &Self::Inst) -> VRegList;

    /// Return physical registers clobbered by `inst`.
    fn clobbers(&self, inst: &Self::Inst) -> Vec<Self::Reg>;

    /// Whether `inst` never returns control to the instruction after it.
    ///
    /// This is true only for a call to a runtime helper the ABI manifest
    /// declares `ReturnBehavior::Never`. Such an instruction reports no
    /// successors, so nothing is live after it, and its clobbers cannot reach
    /// any value whose later uses execute (RUE-1224). Instructions that are
    /// terminal without being calls — `ret`, `ud2`, `brk` — clobber nothing, so
    /// classifying them either way is immaterial and they answer `false`.
    fn is_non_returning(&self, inst: &Self::Inst) -> bool;
}

/// Compute liveness for any backend implementing [`LivenessAdapter`].
pub fn analyze_adapter<A>(adapter: &A) -> LivenessInfo<A::Reg>
where
    A: LivenessAdapter,
{
    analyze(
        adapter.instructions(),
        adapter.vreg_count(),
        adapter.vreg_classes().clone(),
        |inst| adapter.label(inst),
        |idx, inst, label_to_idx| adapter.successors(idx, inst, label_to_idx),
        |inst| adapter.uses(inst),
        |inst| adapter.defs(inst),
        |inst| adapter.clobbers(inst),
        |inst| adapter.is_non_returning(inst),
    )
}

/// Compute detailed debug liveness for any backend implementing
/// [`LivenessAdapter`].
pub fn analyze_debug_adapter<A>(adapter: &A) -> LivenessDebugInfo
where
    A: LivenessAdapter,
{
    analyze_debug::<_, A::Reg>(
        adapter.instructions(),
        adapter.vreg_count(),
        adapter.vreg_classes().clone(),
        |inst| adapter.label(inst),
        |idx, inst, label_to_idx| adapter.successors(idx, inst, label_to_idx),
        |inst| adapter.uses(inst),
        |inst| adapter.defs(inst),
    )
}

/// Compute allocator liveness and its diagnostic projection in one dataflow
/// execution for a target adapter.
pub fn analyze_with_debug_adapter<A>(adapter: &A) -> (LivenessInfo<A::Reg>, LivenessDebugInfo)
where
    A: LivenessAdapter,
{
    analyze_with_debug(
        adapter.instructions(),
        adapter.vreg_count(),
        adapter.vreg_classes().clone(),
        |inst| adapter.label(inst),
        |idx, inst, label_to_idx| adapter.successors(idx, inst, label_to_idx),
        |inst| adapter.uses(inst),
        |inst| adapter.defs(inst),
        |inst| adapter.clobbers(inst),
        |inst| adapter.is_non_returning(inst),
    )
}

/// Compute loop information for any backend implementing [`LivenessAdapter`].
pub fn analyze_loops_adapter<A>(adapter: &A) -> LoopInfo
where
    A: LivenessAdapter,
{
    analyze_loops(
        adapter.instructions(),
        |inst| adapter.label(inst),
        |idx, inst, label_to_idx| adapter.successors(idx, inst, label_to_idx),
    )
}

/// Successor list for an instruction that falls through to the next
/// instruction, if there is one.
pub fn fallthrough_successor(idx: usize, num_insts: usize) -> SuccessorList {
    let mut successors = SuccessorList::new();
    if idx + 1 < num_insts {
        successors.push(idx + 1);
    }
    successors
}

/// Successor list for an unconditional branch to `label`.
pub fn branch_successor(label: LabelId, label_to_idx: &HashMap<LabelId, usize>) -> SuccessorList {
    label_to_idx.get(&label).copied().into_iter().collect()
}

/// Successor list for a conditional branch: fall-through first, then branch
/// target if the label was found.
pub fn conditional_successors(
    idx: usize,
    label: LabelId,
    label_to_idx: &HashMap<LabelId, usize>,
    num_insts: usize,
) -> SuccessorList {
    let mut succs = SuccessorList::new();
    if idx + 1 < num_insts {
        succs.push(idx + 1);
    }
    if let Some(&target) = label_to_idx.get(&label) {
        succs.push(target);
    }
    succs
}

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
/// * `vreg_classes` - The register class of each virtual register, carried
///   through into [`LivenessInfo::vreg_classes`] for allocation and coalescing
/// * `get_label` - Returns the label ID if the instruction is a label, None otherwise
/// * `get_successors` - Returns the successor instruction indices for control flow
/// * `get_uses` - Returns the virtual registers used (read) by the instruction
/// * `get_defs` - Returns the virtual registers defined (written) by the instruction
/// * `get_clobbers` - Returns the physical registers clobbered by the instruction
/// * `get_non_returning` - Returns whether the instruction never returns control
///   to the instruction after it
#[allow(clippy::too_many_arguments)]
pub fn analyze<I, R>(
    instructions: &[I],
    vreg_count: u32,
    vreg_classes: VRegClasses,
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
    get_clobbers: impl Fn(&I) -> Vec<R>,
    get_non_returning: impl Fn(&I) -> bool,
) -> LivenessInfo<R>
where
    R: Copy + Eq + std::hash::Hash,
{
    analyze_inner(
        instructions,
        vreg_count,
        vreg_classes,
        get_label,
        get_successors,
        get_uses,
        get_defs,
        get_clobbers,
        get_non_returning,
        false,
    )
    .0
}

/// Compute production liveness and its diagnostic projection in one dataflow
/// execution.
#[allow(clippy::too_many_arguments)]
pub fn analyze_with_debug<I, R>(
    instructions: &[I],
    vreg_count: u32,
    vreg_classes: VRegClasses,
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
    get_clobbers: impl Fn(&I) -> Vec<R>,
    get_non_returning: impl Fn(&I) -> bool,
) -> (LivenessInfo<R>, LivenessDebugInfo)
where
    R: Copy + Eq + std::hash::Hash,
{
    let (liveness, debug) = analyze_inner(
        instructions,
        vreg_count,
        vreg_classes,
        get_label,
        get_successors,
        get_uses,
        get_defs,
        get_clobbers,
        get_non_returning,
        true,
    );
    (
        liveness,
        debug.expect("debug liveness requested from the canonical analysis"),
    )
}

#[allow(clippy::too_many_arguments)]
fn analyze_inner<I, R>(
    instructions: &[I],
    vreg_count: u32,
    vreg_classes: VRegClasses,
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
    get_clobbers: impl Fn(&I) -> Vec<R>,
    get_non_returning: impl Fn(&I) -> bool,
    collect_debug: bool,
) -> (LivenessInfo<R>, Option<LivenessDebugInfo>)
where
    R: Copy + Eq + std::hash::Hash,
{
    let num_insts = instructions.len();

    if num_insts == 0 {
        return (
            LivenessInfo {
                ranges: IndexMap::new(),
                live_at: Vec::new(),
                clobbers_at: Vec::new(),
                non_returning_at: Vec::new(),
                vreg_classes,
            },
            collect_debug.then(|| LivenessDebugInfo {
                instructions: Vec::new(),
                live_ranges: IndexMap::new(),
                vreg_count,
            }),
        );
    }

    // Step 1: Build label -> instruction index map
    let label_to_idx = build_label_map(instructions, &get_label);

    // Step 2: Build successor lists for each instruction
    let successors = build_successor_lists(instructions, &label_to_idx, &get_successors);

    // Step 3: Pre-compute uses and defs for each instruction
    let inst_uses: Vec<VRegList> = instructions.iter().map(&get_uses).collect();
    let inst_defs: Vec<VRegList> = instructions.iter().map(&get_defs).collect();

    // Step 4: Backward dataflow analysis to compute live sets. Without a back
    // edge, reverse instruction order is already a topological order and one
    // sweep computes the fixed point exactly.
    let (live_in, live_out, has_back_edge, _) =
        compute_dataflow(num_insts, vreg_count, &successors, &inst_uses, &inst_defs);

    // Step 5: Build live ranges from dataflow results
    let ranges = build_live_ranges(
        num_insts,
        vreg_count,
        &inst_uses,
        &inst_defs,
        &live_in,
        &live_out,
        has_back_edge,
    );

    let debug = collect_debug.then(|| {
        let bitset_to_hashset = |bs: &FixedBitSet| -> std::collections::HashSet<VReg> {
            bs.ones().map(|idx| VReg::new(idx as u32)).collect()
        };
        let instruction_liveness = (0..num_insts)
            .map(|idx| InstructionLiveness {
                index: idx,
                live_in: bitset_to_hashset(&live_in[idx]),
                live_out: bitset_to_hashset(&live_out[idx]),
                defs: inst_defs[idx].to_vec(),
                uses: inst_uses[idx].to_vec(),
            })
            .collect();
        LivenessDebugInfo {
            instructions: instruction_liveness,
            live_ranges: ranges.clone(),
            vreg_count,
        }
    });

    // Step 6: Compute live_at for each instruction (union of live_in and live_out)
    let live_at = compute_live_at(live_in, &live_out);

    // Step 7: Collect clobbers and the never-returning call sites (RUE-1224)
    let clobbers_at: Vec<Vec<R>> = instructions.iter().map(|i| get_clobbers(i)).collect();
    let non_returning_at: Vec<bool> = instructions.iter().map(&get_non_returning).collect();

    (
        LivenessInfo {
            ranges,
            live_at,
            clobbers_at,
            non_returning_at,
            vreg_classes,
        },
        debug,
    )
}

/// Compute detailed liveness debug information.
///
/// This provides more detailed output than [`analyze`], including per-instruction
/// live-in/live-out sets and def/use information. Used by `--emit liveness`.
pub fn analyze_debug<I, R>(
    instructions: &[I],
    vreg_count: u32,
    vreg_classes: VRegClasses,
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
) -> LivenessDebugInfo
where
    R: Copy + Eq + std::hash::Hash,
{
    analyze_with_debug(
        instructions,
        vreg_count,
        vreg_classes,
        get_label,
        get_successors,
        get_uses,
        get_defs,
        |_| Vec::<R>::new(),
        |_| false,
    )
    .1
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
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> SuccessorList,
) -> Vec<SuccessorList> {
    instructions
        .iter()
        .enumerate()
        .map(|(idx, inst)| get_successors(idx, inst, label_to_idx))
        .collect()
}

/// Return `true` if any instruction has a successor at an index `<= its own`,
/// i.e. the control-flow graph contains a back-edge (a loop). Used to pick the
/// fast, loop-free live-range construction path in [`build_live_ranges`].
fn has_back_edge(successors: &[SuccessorList]) -> bool {
    successors
        .iter()
        .enumerate()
        .any(|(from, succs)| succs.iter().any(|&to| to <= from))
}

/// Perform backward dataflow analysis to compute live-in and live-out sets.
///
/// Uses the standard dataflow equations:
/// - live_out[i] = union of live_in[s] for all successors s of i
/// - live_in[i] = uses[i] ∪ (live_out[i] - defs[i])
#[cfg(test)]
type DataflowSweepCount = usize;
#[cfg(not(test))]
type DataflowSweepCount = ();

fn compute_dataflow(
    num_insts: usize,
    vreg_count: u32,
    successors: &[SuccessorList],
    inst_uses: &[VRegList],
    inst_defs: &[VRegList],
) -> (Vec<FixedBitSet>, Vec<FixedBitSet>, bool, DataflowSweepCount) {
    let vreg_count_usize = vreg_count as usize;
    let has_back_edge = has_back_edge(successors);

    let mut live_in: Vec<FixedBitSet> =
        vec![FixedBitSet::with_capacity(vreg_count_usize); num_insts];
    let mut live_out: Vec<FixedBitSet> =
        vec![FixedBitSet::with_capacity(vreg_count_usize); num_insts];
    // The transfer function needs two temporary sets regardless of instruction
    // count or convergence rounds. Reuse their backing storage instead of
    // allocating two full-width bitsets for every row on every pass.
    let mut new_live_in = FixedBitSet::with_capacity(vreg_count_usize);
    let mut new_live_out = FixedBitSet::with_capacity(vreg_count_usize);
    #[cfg(test)]
    let mut sweeps = 0;

    // Iterate until fixed point
    let mut changed = true;
    while changed {
        changed = false;

        // Process instructions in reverse order for faster convergence
        for idx in (0..num_insts).rev() {
            // Compute live_out as union of live_in of all successors
            new_live_out.clear();
            for &succ in &successors[idx] {
                new_live_out.union_with(&live_in[succ]);
            }

            // Compute live_in = uses ∪ (live_out - defs)
            new_live_in.clone_from(&new_live_out);
            for vreg in &inst_defs[idx] {
                new_live_in.set(vreg.index() as usize, false);
            }
            for vreg in &inst_uses[idx] {
                new_live_in.insert(vreg.index() as usize);
            }

            // Check if anything changed
            if new_live_in != live_in[idx] || new_live_out != live_out[idx] {
                changed = true;
                live_in[idx].clone_from(&new_live_in);
                live_out[idx].clone_from(&new_live_out);
            }
        }

        #[cfg(test)]
        {
            sweeps += 1;
        }
        if !has_back_edge {
            // Every successor has a greater instruction index, so this reverse
            // sweep visited successors before their predecessors. There can be
            // no information left to propagate on another sweep.
            break;
        }
    }

    #[cfg(test)]
    let sweep_count = sweeps;
    #[cfg(not(test))]
    let sweep_count = ();
    (live_in, live_out, has_back_edge, sweep_count)
}

/// Build live ranges from dataflow results.
///
/// `has_back_edge` selects the algorithm:
///
/// * **No back-edges (loop-free control flow):** a vreg's live range is exactly
///   `[first def/use, last def/use]`. Scanning `live_in`/`live_out` cannot widen
///   it — a vreg can only be live at an instruction that is neither a def nor a
///   use of it if that instruction sits *between* a def and a later use (already
///   inside the def/use span) or across a back-edge (excluded here). So we skip
///   the per-instruction bitset scan entirely and read ranges off the def/use
///   lists in O(defs + uses). This matters because a single basic block can hold
///   many simultaneously-live vregs (e.g. a large array literal materializes N
///   element values before storing them); the bitset scan is then O(N²) in the
///   number of live bits, whereas this path stays linear (RUE-302).
/// * **Has back-edges (loops):** loop-carried values are live past their textual
///   last use, so we extend the same dense range table from the exact
///   `live_in`/`live_out` scan as well as definitions and uses.
///
/// # A range is a textual interval, not liveness
///
/// Either way the result is a single `[start, end]` span of *instruction
/// indices*, and every index between the endpoints is inside it. That is not
/// the same claim as "the value is live at each of those instructions": a range
/// is a contiguous approximation of a set that dataflow may know to have holes.
/// Allocation is built on the interval reading — `LiveRange::overlaps` decides
/// interference, and [`ClobberIndex`](crate::regalloc::ClobberIndex) asks
/// whether anything in the span destroys a register — so anything that makes
/// the two readings disagree is a miscompile, not an imprecision.
///
/// This is why refining "the value is not really live there" needs the two-part
/// treatment RUE-1224 gave the non-returning trap calls, and not just the
/// dataflow half. Removing a call's successors empties its live-out and can
/// shorten a range that *ends* at the call, but a range that merely *spans* the
/// call does not shrink at all: its endpoints are a def before and a use after,
/// and every index between them stays in the interval no matter what
/// `live_out` says. RUE-1224 therefore also had to exclude those calls from
/// `ClobberIndex`, because the clobber remained inside the interval and would
/// otherwise have kept disqualifying the value from a caller-saved register.
///
/// So a future refinement here should expect the same shape: model the fact in
/// the dataflow *and* teach every consumer that reads the range as a dense
/// interval about the exclusion. Changing only one of the two silently changes
/// what allocation believes about register lifetimes.
fn build_live_ranges(
    num_insts: usize,
    vreg_count: u32,
    inst_uses: &[VRegList],
    inst_defs: &[VRegList],
    live_in: &[FixedBitSet],
    live_out: &[FixedBitSet],
    has_back_edge: bool,
) -> IndexMap<VReg, Option<LiveRange>> {
    let mut ranges: IndexMap<VReg, Option<LiveRange>> =
        IndexMap::with_capacity(vreg_count as usize);
    ranges.resize(vreg_count as usize, None);

    let mut extend = |vreg: VReg, idx: usize| match &mut ranges[vreg] {
        Some(range) => {
            if idx < range.start {
                range.start = idx;
            }
            if idx > range.end {
                range.end = idx;
            }
        }
        slot @ None => *slot = Some(LiveRange::new(idx, idx)),
    };

    if !has_back_edge {
        // Fast O(defs + uses) path: no loops, so def/use extents are the range.
        for idx in 0..num_insts {
            for vreg in &inst_defs[idx] {
                extend(*vreg, idx);
            }
            for vreg in &inst_uses[idx] {
                extend(*vreg, idx);
            }
        }
        return ranges;
    }

    for idx in 0..num_insts {
        for vreg in &inst_defs[idx] {
            extend(*vreg, idx);
        }
        for vreg in &inst_uses[idx] {
            extend(*vreg, idx);
        }
        for vreg_idx in live_in[idx].ones() {
            extend(VReg::new(vreg_idx as u32), idx);
        }
        for vreg_idx in live_out[idx].ones() {
            extend(VReg::new(vreg_idx as u32), idx);
        }
    }

    ranges
}

/// Compute live_at sets (union of live_in and live_out for each instruction).
fn compute_live_at(mut live_in: Vec<FixedBitSet>, live_out: &[FixedBitSet]) -> Vec<FixedBitSet> {
    // `live_in` has no consumer after ranges are built. Turn it into the
    // retained union in place instead of allocating a third full-width table.
    for (live_at, live_out) in live_in.iter_mut().zip(live_out) {
        live_at.union_with(live_out);
    }
    live_in
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
pub fn compute_loop_info<S>(num_insts: usize, successors: &[S]) -> LoopInfo
where
    S: AsRef<[usize]>,
{
    if num_insts == 0 {
        return LoopInfo::no_loops(0);
    }

    // Find all back-edges: edges where we jump to an earlier or same instruction
    // A back-edge from instruction `from` to instruction `to` (where to <= from)
    // indicates a loop from `to` to `from`
    let mut loop_ranges: Vec<(usize, usize)> = Vec::new();

    for (from, succs) in successors.iter().enumerate() {
        for &to in succs.as_ref() {
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

    LoopInfo::from_depths(depths)
}

/// Compute loop info from instructions using the provided callbacks.
///
/// This is a convenience function that builds the label map and successor lists,
/// then calls `compute_loop_info`.
pub fn analyze_loops<I>(
    instructions: &[I],
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &HashMap<LabelId, usize>) -> SuccessorList,
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

    #[test]
    fn inline_fact_lists_do_not_widen_the_outer_liveness_tables() {
        assert!(std::mem::size_of::<VRegList>() <= std::mem::size_of::<Vec<VReg>>());
        assert!(std::mem::size_of::<SuccessorList>() <= std::mem::size_of::<Vec<usize>>());

        let facts: VRegList = (0..4).map(VReg::new).collect();
        assert_eq!(
            facts.as_ref(),
            &[VReg::new(0), VReg::new(1), VReg::new(2), VReg::new(3)]
        );
    }

    #[test]
    #[should_panic(expected = "inline liveness fact capacity 4 exceeded")]
    fn inline_fact_lists_fail_loudly_when_the_audited_bound_changes() {
        let _: VRegList = (0..5).map(VReg::new).collect();
    }

    // Simple test instruction type
    #[derive(Debug, Clone)]
    enum TestInst {
        Def { dst: u32 },
        Use { src: u32 },
        Move { dst: u32, src: u32 },
        Label { id: LabelId },
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
    ) -> SuccessorList {
        match inst {
            TestInst::Branch { label } => {
                let mut succs = SuccessorList::new();
                if idx + 1 < num_insts {
                    succs.push(idx + 1);
                }
                if let Some(&target) = label_to_idx.get(label) {
                    succs.push(target);
                }
                succs
            }
            TestInst::Ret => SuccessorList::new(),
            _ => {
                let mut successors = SuccessorList::new();
                if idx + 1 < num_insts {
                    successors.push(idx + 1);
                }
                successors
            }
        }
    }

    fn test_get_uses(inst: &TestInst) -> VRegList {
        match inst {
            TestInst::Use { src } => [VReg::new(*src)].into_iter().collect(),
            TestInst::Move { src, .. } => [VReg::new(*src)].into_iter().collect(),
            _ => VRegList::new(),
        }
    }

    fn test_get_defs(inst: &TestInst) -> VRegList {
        match inst {
            TestInst::Def { dst } => [VReg::new(*dst)].into_iter().collect(),
            TestInst::Move { dst, .. } => [VReg::new(*dst)].into_iter().collect(),
            _ => VRegList::new(),
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
            VRegClasses::all_gp(2),
            test_get_label,
            |idx, inst, label_to_idx| test_get_successors(idx, inst, label_to_idx, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
            |_| false,
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
            VRegClasses::all_gp(1),
            test_get_label,
            |idx, inst, label_to_idx| test_get_successors(idx, inst, label_to_idx, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
            |_| false,
        );

        // v0: defined at 0, last used at 4
        let range = info.range(VReg::new(0)).expect("v0 should have a range");
        assert_eq!(range.start, 0);
        assert!(range.end >= 4);
    }

    #[test]
    fn looped_live_range_includes_dataflow_only_extent() {
        let loop_label = LabelId::new(0);
        let instructions = vec![
            TestInst::Def { dst: 0 },
            TestInst::Label { id: loop_label },
            TestInst::Use { src: 0 },
            TestInst::Branch { label: loop_label },
            TestInst::Ret,
        ];
        let num_insts = instructions.len();

        let info: LivenessInfo<u32> = analyze(
            &instructions,
            1,
            VRegClasses::all_gp(1),
            test_get_label,
            |idx, inst, label_to_idx| test_get_successors(idx, inst, label_to_idx, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
            |_| false,
        );

        // The back-edge keeps v0 live through instruction 3 even though its
        // last textual use is instruction 2.
        assert_eq!(info.range(VReg::new(0)), Some(&LiveRange::new(0, 3)));
    }

    #[test]
    fn test_empty_instructions() {
        let instructions: Vec<TestInst> = vec![];
        let num_insts = instructions.len();

        let info: LivenessInfo<u32> = analyze(
            &instructions,
            0,
            VRegClasses::all_gp(0),
            test_get_label,
            |idx, inst, label_to_idx| test_get_successors(idx, inst, label_to_idx, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
            |_| false,
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
            VRegClasses::all_gp(2),
            test_get_label,
            |idx, inst, label_to_idx| test_get_successors(idx, inst, label_to_idx, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
            |_| false,
        );

        // v0 and v1 should interfere (both live at instruction 2)
        assert!(info.interferes(VReg::new(0), VReg::new(1)));
    }

    #[test]
    fn dataflow_scratch_is_cleared_across_rows_and_fixed_point_rounds() {
        let successors: Vec<SuccessorList> = [
            SuccessorList::new(),
            [2].into_iter().collect(),
            [1, 3].into_iter().collect(),
            SuccessorList::new(),
        ]
        .into();
        let uses: Vec<VRegList> = [
            VRegList::new(),
            [VReg::new(0)].into_iter().collect(),
            VRegList::new(),
            VRegList::new(),
        ]
        .into();
        let defs = vec![VRegList::new(); successors.len()];

        // The use at instruction 1 reaches instruction 2 only through the
        // back-edge, so convergence requires another reverse pass. Instruction
        // 0 is processed after those live rows but remains empty, proving one
        // row's scratch bits do not leak to the next row or the next round.
        let (live_in, live_out, has_back_edge, sweeps) =
            compute_dataflow(4, 1, &successors, &uses, &defs);
        let ones = |set: &FixedBitSet| set.ones().collect::<Vec<_>>();
        assert!(live_in[0].is_clear());
        assert_eq!(ones(&live_in[1]), vec![0]);
        assert_eq!(ones(&live_in[2]), vec![0]);
        assert!(live_in[3].is_clear());
        assert!(live_out[0].is_clear());
        assert_eq!(ones(&live_out[1]), vec![0]);
        assert_eq!(ones(&live_out[2]), vec![0]);
        assert!(live_out[3].is_clear());
        assert!(has_back_edge);
        assert_eq!(sweeps, 3, "the back-edge requires fixed-point iteration");
    }

    #[test]
    fn acyclic_dataflow_reaches_its_fixed_point_in_one_reverse_sweep() {
        let successors: Vec<SuccessorList> = [
            [1].into_iter().collect(),
            [2].into_iter().collect(),
            SuccessorList::new(),
        ]
        .into();
        let uses: Vec<VRegList> = [
            VRegList::new(),
            VRegList::new(),
            [VReg::new(0)].into_iter().collect(),
        ]
        .into();
        let defs = vec![VRegList::new(); successors.len()];

        let (live_in, live_out, has_back_edge, sweeps) =
            compute_dataflow(3, 1, &successors, &uses, &defs);
        let ones = |set: &FixedBitSet| set.ones().collect::<Vec<_>>();

        assert_eq!(ones(&live_in[0]), vec![0]);
        assert_eq!(ones(&live_in[1]), vec![0]);
        assert_eq!(ones(&live_in[2]), vec![0]);
        assert_eq!(ones(&live_out[0]), vec![0]);
        assert_eq!(ones(&live_out[1]), vec![0]);
        assert!(live_out[2].is_clear());
        assert!(!has_back_edge);
        assert_eq!(sweeps, 1, "forward-only control flow is solved exactly");
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
