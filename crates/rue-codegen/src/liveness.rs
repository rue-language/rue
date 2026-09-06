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

#[cfg(test)]
use std::cell::Cell;
use std::collections::VecDeque;
use std::iter::Take;
use std::ops::Deref;

use ahash::{AHashMap, AHashSet};
use fixedbitset::FixedBitSet;

use crate::index_map::IndexMap;
use crate::reg_class::VRegClasses;
use crate::regalloc::{InstructionLiveness, LiveRange, LivenessDebugInfo, LivenessInfo, LoopInfo};
use crate::vreg::{LabelId, VReg};

#[cfg(test)]
thread_local! {
    static DATAFLOW_CALLS: Cell<usize> = const { Cell::new(0) };
    /// Full-width bitset words the liveness dataflow touches.
    ///
    /// The solve and the range construction both work at block granularity, so
    /// this total is proportional to blocks x register-set width plus the
    /// instruction count — never to instructions x width. A scaling test reads
    /// it at two problem sizes, which is what makes a reintroduced
    /// per-instruction table fail rather than merely slow the compiler down
    /// (RUE-1545).
    static DATAFLOW_BITSET_WORDS: Cell<u64> = const { Cell::new(0) };
}

/// Record `times` full-width passes over a `words`-word bitset.
///
/// Compiles to nothing outside tests; the counter exists only so the scaling
/// test can assert the shape of the work rather than its wall time.
#[inline]
fn note_bitset_words(_words: usize, _times: usize) {
    #[cfg(test)]
    DATAFLOW_BITSET_WORDS.with(|counter| {
        counter.set(counter.get() + (_words as u64) * (_times as u64));
    });
}

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
    type Reg: Copy + Eq + std::hash::Hash + 'static;

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
        label_to_idx: &AHashMap<LabelId, usize>,
    ) -> SuccessorList;

    /// Return virtual registers read by `inst`.
    fn uses(&self, inst: &Self::Inst) -> VRegList;

    /// Return virtual registers written by `inst`.
    fn defs(&self, inst: &Self::Inst) -> VRegList;

    /// Return physical registers clobbered by `inst`.
    fn clobbers(&self, inst: &Self::Inst) -> &'static [Self::Reg];

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

/// Compute liveness and loop information together for a target adapter.
pub fn analyze_with_loops_adapter<A>(adapter: &A) -> (LivenessInfo<A::Reg>, LoopInfo)
where
    A: LivenessAdapter,
{
    analyze_with_loops(
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

/// Compute liveness, its diagnostic projection, and loop information together
/// for a target adapter.
pub fn analyze_with_debug_and_loops_adapter<A>(
    adapter: &A,
) -> (LivenessInfo<A::Reg>, LivenessDebugInfo, LoopInfo)
where
    A: LivenessAdapter,
{
    analyze_with_debug_and_loops(
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
pub fn branch_successor(label: LabelId, label_to_idx: &AHashMap<LabelId, usize>) -> SuccessorList {
    label_to_idx.get(&label).copied().into_iter().collect()
}

/// Successor list for a conditional branch: fall-through first, then branch
/// target if the label was found.
pub fn conditional_successors(
    idx: usize,
    label: LabelId,
    label_to_idx: &AHashMap<LabelId, usize>,
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
    get_successors: impl Fn(usize, &I, &AHashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
    get_clobbers: impl Fn(&I) -> &'static [R],
    get_non_returning: impl Fn(&I) -> bool,
) -> LivenessInfo<R>
where
    R: Copy + Eq + std::hash::Hash + 'static,
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
    get_successors: impl Fn(usize, &I, &AHashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
    get_clobbers: impl Fn(&I) -> &'static [R],
    get_non_returning: impl Fn(&I) -> bool,
) -> (LivenessInfo<R>, LivenessDebugInfo)
where
    R: Copy + Eq + std::hash::Hash + 'static,
{
    let (liveness, debug, _) = analyze_inner(
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
        false,
    );
    (
        liveness,
        debug.expect("debug liveness requested from the canonical analysis"),
    )
}

/// Compute production liveness and loop information in one walk of the MIR.
///
/// The register allocator needs both, and both derive from the same label map
/// and successor lists. Computing them through separate entry points walked the
/// instruction stream twice to rebuild byte-identical tables (RUE-1846).
#[allow(clippy::too_many_arguments)]
pub fn analyze_with_loops<I, R>(
    instructions: &[I],
    vreg_count: u32,
    vreg_classes: VRegClasses,
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &AHashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
    get_clobbers: impl Fn(&I) -> &'static [R],
    get_non_returning: impl Fn(&I) -> bool,
) -> (LivenessInfo<R>, LoopInfo)
where
    R: Copy + Eq + std::hash::Hash + 'static,
{
    let (liveness, _, loops) = analyze_inner(
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
        true,
    );
    (
        liveness,
        loops.expect("loop info requested from the canonical analysis"),
    )
}

/// As [`analyze_with_loops`], additionally retaining the diagnostic projection
/// of the same dataflow.
#[allow(clippy::too_many_arguments)]
pub fn analyze_with_debug_and_loops<I, R>(
    instructions: &[I],
    vreg_count: u32,
    vreg_classes: VRegClasses,
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &AHashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
    get_clobbers: impl Fn(&I) -> &'static [R],
    get_non_returning: impl Fn(&I) -> bool,
) -> (LivenessInfo<R>, LivenessDebugInfo, LoopInfo)
where
    R: Copy + Eq + std::hash::Hash + 'static,
{
    let (liveness, debug, loops) = analyze_inner(
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
        true,
    );
    (
        liveness,
        debug.expect("debug liveness requested from the canonical analysis"),
        loops.expect("loop info requested from the canonical analysis"),
    )
}

#[allow(clippy::too_many_arguments)]
fn analyze_inner<I, R>(
    instructions: &[I],
    vreg_count: u32,
    vreg_classes: VRegClasses,
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &AHashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
    get_clobbers: impl Fn(&I) -> &'static [R],
    get_non_returning: impl Fn(&I) -> bool,
    collect_debug: bool,
    collect_loops: bool,
) -> (LivenessInfo<R>, Option<LivenessDebugInfo>, Option<LoopInfo>)
where
    R: Copy + Eq + std::hash::Hash + 'static,
{
    let num_insts = instructions.len();

    if num_insts == 0 {
        return (
            LivenessInfo {
                ranges: IndexMap::new(),
                clobbers_at: Vec::new(),
                non_returning_at: Vec::new(),
                vreg_classes,
            },
            collect_debug.then(|| LivenessDebugInfo {
                instructions: Vec::new(),
                live_ranges: IndexMap::new(),
                vreg_count,
            }),
            collect_loops.then(|| LoopInfo::no_loops(0)),
        );
    }

    // Step 1: Build label -> instruction index map
    let label_to_idx = build_label_map(instructions, &get_label);

    // Step 2: Build successor lists for each instruction
    let successors = build_successor_lists(instructions, &label_to_idx, &get_successors);

    // Loop info is a pure function of that successor table (RUE-1846). Deriving
    // it here lets the allocator's two analyses share one label-map and
    // successor-list construction; previously `analyze_loops` walked the
    // instruction stream again to rebuild byte-identical tables.
    let loop_info = collect_loops.then(|| compute_loop_info(num_insts, &successors));

    // Step 3: Pre-compute uses and defs for each instruction
    let inst_uses: Vec<VRegList> = instructions.iter().map(&get_uses).collect();
    let inst_defs: Vec<VRegList> = instructions.iter().map(&get_defs).collect();

    // Step 4: Backward dataflow analysis, at block granularity. The solve keeps
    // one live-in bitset per *basic block* rather than per instruction, which
    // is what keeps the pass proportional to blocks x register-set width
    // instead of instructions x width (RUE-1545). Without a back edge, reverse
    // block order is already a topological order and one sweep computes the
    // fixed point exactly; production range construction does not consume the
    // sets at all in that case (RUE-302), so the loop-free path skips the solve
    // entirely. Debug output still asks for the sets, because its projection is
    // per instruction by definition.
    let has_back_edge = has_back_edge(&successors);
    let block_liveness = (collect_debug || has_back_edge).then(|| {
        let blocks = partition_blocks(num_insts, &successors);
        let live_in = solve_block_liveness(
            vreg_count,
            &successors,
            &inst_uses,
            &inst_defs,
            &blocks,
            has_back_edge,
        )
        .0;
        BlockLiveness { blocks, live_in }
    });

    // Step 5: Build live ranges. The loop-free path reads the def/use facts
    // alone; the cyclic path additionally reads each block's boundary sets.
    let ranges = build_live_ranges(
        num_insts,
        vreg_count,
        &inst_uses,
        &inst_defs,
        &successors,
        if has_back_edge {
            block_liveness.as_ref()
        } else {
            None
        },
    );

    let debug = collect_debug.then(|| {
        let block_liveness = block_liveness
            .as_ref()
            .expect("the debug projection requests the block sets it re-derives from");
        LivenessDebugInfo {
            instructions: instruction_liveness_rows(
                num_insts,
                vreg_count,
                &successors,
                &inst_uses,
                &inst_defs,
                block_liveness,
            ),
            live_ranges: ranges.clone(),
            vreg_count,
        }
    });

    // Step 6: Collect clobbers and the never-returning call sites (RUE-1224)
    let clobbers_at: Vec<&'static [R]> = instructions.iter().map(&get_clobbers).collect();
    let non_returning_at: Vec<bool> = instructions.iter().map(&get_non_returning).collect();

    (
        LivenessInfo {
            ranges,
            clobbers_at,
            non_returning_at,
            vreg_classes,
        },
        debug,
        loop_info,
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
    get_successors: impl Fn(usize, &I, &AHashMap<LabelId, usize>) -> SuccessorList,
    get_uses: impl Fn(&I) -> VRegList,
    get_defs: impl Fn(&I) -> VRegList,
) -> LivenessDebugInfo
where
    R: Copy + Eq + std::hash::Hash + 'static,
{
    analyze_with_debug(
        instructions,
        vreg_count,
        vreg_classes,
        get_label,
        get_successors,
        get_uses,
        get_defs,
        |_| &[] as &'static [R],
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
) -> AHashMap<LabelId, usize> {
    let mut label_to_idx = AHashMap::new();
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
    label_to_idx: &AHashMap<LabelId, usize>,
    get_successors: impl Fn(usize, &I, &AHashMap<LabelId, usize>) -> SuccessorList,
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

/// A partition of the instruction stream into basic blocks.
///
/// Liveness is a backward analysis, so what matters about a block is that the
/// transfer function inside it is a straight composition: every instruction
/// before the block's last one has exactly one successor, the next
/// instruction, and nothing branches into the middle of the run. Blocks are
/// therefore delimited by leaders — instruction 0, any branch target, and the
/// instruction after any row that is not a plain fallthrough.
///
/// Both consequences this module relies on follow from that shape:
///
/// * the dataflow fixed point can be solved with one live-in set per block
///   instead of one per instruction, and
/// * inside a block a vreg's live-in membership changes only at that vreg's own
///   defs and uses, so live ranges need the block's two boundary sets and
///   nothing in between.
struct BlockPartition {
    /// Instruction index each block starts at, with a terminating `num_insts`,
    /// so block `b` covers `starts[b]..starts[b + 1]`.
    starts: Vec<usize>,
    /// Block owning each instruction.
    block_of: Vec<u32>,
}

impl BlockPartition {
    fn len(&self) -> usize {
        self.starts.len() - 1
    }

    /// First and last instruction index of `block`, both inclusive.
    fn span(&self, block: usize) -> (usize, usize) {
        (self.starts[block], self.starts[block + 1] - 1)
    }

    fn of(&self, inst: usize) -> usize {
        self.block_of[inst] as usize
    }
}

/// The block-granular liveness solution the cyclic paths consume.
struct BlockLiveness {
    blocks: BlockPartition,
    /// Virtual registers live before each block's first instruction.
    live_in: Vec<FixedBitSet>,
}

impl BlockLiveness {
    /// This block's live-out set, written into `out`.
    fn live_out_into(&self, block: usize, successors: &[SuccessorList], out: &mut FixedBitSet) {
        collect_block_live_out(block, &self.blocks, successors, &self.live_in, out);
    }
}

/// Write `block`'s live-out set into `out`.
///
/// A block's live-out set is the union of the live-in sets of the blocks its
/// terminator can reach. Successor instruction indices are always leaders, so
/// every one of them names a block start. This is the one place the block-level
/// meet is computed, for the solve and for both of its readers.
fn collect_block_live_out(
    block: usize,
    blocks: &BlockPartition,
    successors: &[SuccessorList],
    live_in: &[FixedBitSet],
    out: &mut FixedBitSet,
) {
    let (_, end) = blocks.span(block);
    out.clear();
    note_bitset_words(out.as_slice().len(), 1 + successors[end].len());
    for &successor in &successors[end] {
        out.union_with(&live_in[blocks.of(successor)]);
    }
}

/// Split the instruction stream into basic blocks.
///
/// A malformed successor table indexes past the instruction stream here, which
/// is the same loud failure the per-instruction dataflow gave when it indexed
/// its rows: a backend that reports an unreachable target has a bug, and
/// quietly dropping the edge would silently shorten live ranges.
fn partition_blocks(num_insts: usize, successors: &[SuccessorList]) -> BlockPartition {
    assert!(num_insts > 0, "an empty MIR has no blocks to partition");
    assert!(
        u32::try_from(num_insts).is_ok(),
        "instruction indices are recorded as u32 block ownership entries"
    );

    let mut is_leader = vec![false; num_insts];
    is_leader[0] = true;
    for (idx, succs) in successors.iter().enumerate() {
        let falls_through = succs.len() == 1 && succs[0] == idx + 1;
        if !falls_through && idx + 1 < num_insts {
            is_leader[idx + 1] = true;
        }
        for &successor in succs {
            if successor != idx + 1 {
                is_leader[successor] = true;
            }
        }
    }

    let mut starts = Vec::new();
    let mut block_of = vec![0u32; num_insts];
    for (idx, &leader) in is_leader.iter().enumerate() {
        if leader {
            starts.push(idx);
        }
        block_of[idx] = (starts.len() - 1) as u32;
    }
    starts.push(num_insts);
    BlockPartition { starts, block_of }
}

/// Apply one block's backward transfer function into `scratch`.
///
/// `scratch` receives the block's live-out set and then the block's
/// instructions in reverse, each applied with point operations. A visit
/// therefore costs the block's length plus one full-width union per successor,
/// not the block's length times the register-set width.
fn transfer_block(
    block: usize,
    blocks: &BlockPartition,
    successors: &[SuccessorList],
    inst_uses: &[VRegList],
    inst_defs: &[VRegList],
    live_in: &[FixedBitSet],
    scratch: &mut FixedBitSet,
) {
    let (start, end) = blocks.span(block);
    collect_block_live_out(block, blocks, successors, live_in, scratch);
    for idx in (start..=end).rev() {
        for vreg in &inst_defs[idx] {
            scratch.set(vreg.index() as usize, false);
        }
        for vreg in &inst_uses[idx] {
            scratch.insert(vreg.index() as usize);
        }
    }
}

#[cfg(test)]
type BlockVisitCount = usize;
#[cfg(not(test))]
type BlockVisitCount = ();

/// Solve backward liveness for every block.
///
/// The equations are the textbook ones, read at block boundaries:
/// - live_out[b] = union of live_in[s] over the successor blocks of b
/// - live_in[b]  = the block's instructions applied backward to live_out[b]
///
/// Storage is one bitset per block plus one scratch set, so the solve is
/// proportional to blocks x width and never to instructions x width. The
/// solution agrees with a per-instruction fixed point at every block start,
/// because a block's interior rows are exactly the composition this transfer
/// function performs, and both start from the empty set and only add.
fn solve_block_liveness(
    vreg_count: u32,
    successors: &[SuccessorList],
    inst_uses: &[VRegList],
    inst_defs: &[VRegList],
    blocks: &BlockPartition,
    has_back_edge: bool,
) -> (Vec<FixedBitSet>, BlockVisitCount) {
    let width = vreg_count as usize;
    let block_count = blocks.len();

    #[cfg(test)]
    DATAFLOW_CALLS.with(|calls| calls.set(calls.get() + 1));

    let mut live_in: Vec<FixedBitSet> = vec![FixedBitSet::with_capacity(width); block_count];
    let mut scratch = FixedBitSet::with_capacity(width);
    let words = scratch.as_slice().len();
    note_bitset_words(words, block_count + 1);
    #[cfg(test)]
    let mut block_visits = 0;

    if has_back_edge {
        // Cyclic control flow needs repeated propagation, and rescanning every
        // block on every round revisits blocks whose successors did not move.
        // Seed a deterministic descending worklist over block successors, then
        // requeue only the predecessors of a block whose live-in set changed.
        let block_successors: Vec<SuccessorList> = (0..block_count)
            .map(|block| {
                let (_, end) = blocks.span(block);
                successors[end]
                    .iter()
                    .map(|&successor| blocks.of(successor))
                    .collect()
            })
            .collect();
        let (predecessor_offsets, predecessors) = build_predecessor_csr(&block_successors);
        let mut queue = VecDeque::with_capacity(block_count);
        let mut queued = vec![false; block_count];
        for block in (0..block_count).rev() {
            queue.push_back(block);
            queued[block] = true;
        }

        while let Some(block) = queue.pop_front() {
            queued[block] = false;
            #[cfg(test)]
            {
                block_visits += 1;
            }

            transfer_block(
                block,
                blocks,
                successors,
                inst_uses,
                inst_defs,
                &live_in,
                &mut scratch,
            );
            note_bitset_words(words, 1);
            if scratch != live_in[block] {
                note_bitset_words(words, 1);
                live_in[block].clone_from(&scratch);
                for &predecessor in
                    &predecessors[predecessor_offsets[block]..predecessor_offsets[block + 1]]
                {
                    if !queued[predecessor] {
                        queued[predecessor] = true;
                        queue.push_back(predecessor);
                    }
                }
            }
        }
    } else {
        // Loop-free control flow keeps every successor at a higher instruction
        // index, so every successor block has a higher block index and reverse
        // block order is a topological order: one sweep is the fixed point.
        for block in (0..block_count).rev() {
            #[cfg(test)]
            {
                block_visits += 1;
            }

            transfer_block(
                block,
                blocks,
                successors,
                inst_uses,
                inst_defs,
                &live_in,
                &mut scratch,
            );
            note_bitset_words(words, 2);
            live_in[block].clone_from(&scratch);
        }
    }

    #[cfg(test)]
    let block_visit_count = block_visits;
    #[cfg(not(test))]
    let block_visit_count = ();
    (live_in, block_visit_count)
}

/// Build a flat CSR predecessor index for a successor table.
///
/// The offsets and edge arrays are transient to one cyclic dataflow solve.
/// Inserting source rows in descending order makes each predecessor slice
/// deterministic without sorting or per-row `Vec` allocations.
fn build_predecessor_csr(successors: &[SuccessorList]) -> (Vec<usize>, Vec<usize>) {
    let mut offsets = vec![0usize; successors.len() + 1];
    for successor_list in successors {
        for &successor in successor_list {
            offsets[successor + 1] += 1;
        }
    }
    for idx in 1..offsets.len() {
        offsets[idx] += offsets[idx - 1];
    }

    let mut predecessors = vec![0usize; offsets[successors.len()]];
    for source in (0..successors.len()).rev() {
        for &successor in &successors[source] {
            // Reuse the offset entries as per-row insertion cursors. They
            // currently contain each row's start; after filling, they hold
            // each row's end and can be shifted back in place below. This
            // avoids a second instruction-sized cursor allocation.
            let slot = offsets[successor];
            predecessors[slot] = source;
            offsets[successor] += 1;
        }
    }
    // Restore the CSR starts while retaining the final total at offsets[n].
    // Descending order is required because each source entry is still the
    // preceding row's end until it is copied.
    for idx in (1..successors.len()).rev() {
        offsets[idx] = offsets[idx - 1];
    }
    offsets[0] = 0;
    (offsets, predecessors)
}

/// Re-derive the per-instruction live-in and live-out sets `--emit liveness`
/// prints.
///
/// This is the only consumer that genuinely wants a row per instruction, and
/// it is asked for only when the diagnostic projection is requested, so the
/// production path never pays for the table (RUE-1545). Each block is swept
/// backward once from its live-out set, which reproduces exactly the rows a
/// per-instruction fixed point would have left behind.
fn instruction_liveness_rows(
    num_insts: usize,
    vreg_count: u32,
    successors: &[SuccessorList],
    inst_uses: &[VRegList],
    inst_defs: &[VRegList],
    block_liveness: &BlockLiveness,
) -> Vec<InstructionLiveness> {
    let to_set = |set: &FixedBitSet| -> AHashSet<VReg> {
        set.ones().map(|idx| VReg::new(idx as u32)).collect()
    };

    let mut rows = Vec::with_capacity(num_insts);
    let mut block_rows = Vec::new();
    let mut scratch = FixedBitSet::with_capacity(vreg_count as usize);
    for block in 0..block_liveness.blocks.len() {
        let (start, end) = block_liveness.blocks.span(block);
        block_liveness.live_out_into(block, successors, &mut scratch);
        for idx in (start..=end).rev() {
            // Sweeping backward, `scratch` holds live-out of the row about to
            // be transferred, and holds its live-in once the row's defs and
            // uses have been applied.
            let live_out = to_set(&scratch);
            for vreg in &inst_defs[idx] {
                scratch.set(vreg.index() as usize, false);
            }
            for vreg in &inst_uses[idx] {
                scratch.insert(vreg.index() as usize);
            }
            note_bitset_words(scratch.as_slice().len(), 2);
            block_rows.push(InstructionLiveness {
                index: idx,
                live_in: to_set(&scratch),
                live_out,
                defs: inst_defs[idx].to_vec(),
                uses: inst_uses[idx].to_vec(),
            });
        }
        block_rows.reverse();
        rows.append(&mut block_rows);
    }
    assert_eq!(
        rows.len(),
        num_insts,
        "the block partition must cover every instruction exactly once"
    );
    rows
}

/// Build live ranges without ever materializing a per-instruction live set.
///
/// A range is derived from three sources, none of which walks the live set of
/// an instruction:
///
/// * **Definitions and uses.** Every def and use index is inside its vreg's
///   range, so one linear pass over the fact lists gives the `[first, last]`
///   extent of the textual def/use span in O(defs + uses).
/// * **Loop-free control flow adds nothing.** A vreg can only be live at an
///   instruction that is neither a def nor a use of it if that instruction sits
///   *between* a def and a later use (already inside the def/use span) or across
///   a back-edge. Without back-edges the def/use pass is therefore the whole
///   answer, and the dataflow is not even solved (RUE-302).
/// * **Back-edges extend it, at block boundaries only.** With loops a value is
///   live past its textual last use, so liveness has to be consulted — but only
///   at each block's two boundaries, which is all the block-granular solve
///   keeps (RUE-1545).
///
/// # Why the two boundary sets of each block are enough
///
/// Inside a block every row but the last has exactly one successor, the next
/// row, so the transfer function is
/// `live_in[idx] = uses[idx] ∪ (live_in[idx + 1] - defs[idx])`. A vreg that the
/// row neither uses nor defines therefore has the same membership at `idx` as
/// at `idx + 1`: within a block, membership changes only at that vreg's own
/// defs and uses.
///
/// Take any index `idx` where a vreg is live-in but which is neither a def nor
/// a use of it, and walk outwards inside its block:
///
/// * downwards, membership persists until the nearest lower def or use of the
///   vreg — already in the def/use extent — or, if there is none, all the way
///   to the block's first instruction, where the vreg is in `live_in[b]`;
/// * upwards, membership persists until the nearest higher def or use — again
///   in the extent — or to the block's last instruction, whose live-out set is
///   `live_out[b]`.
///
/// So `idx` always lies between two contributed indices and is covered by the
/// resulting interval. Every contributed index is itself a point where the vreg
/// is live or defined, so nothing over-extends either: a value in `live_out[b]`
/// that is not live-in at the block's last instruction was defined there, and
/// that index is in the def/use extent regardless. The endpoints therefore
/// match a scan of every instruction's live-in set exactly, which is what
/// `live_in_scan_reference` asserts.
///
/// Scanning live-out at every instruction would be redundant either way:
/// `live_in = uses ∪ (live_out - defs)`, so every live-out value is already
/// either live-in or defined at that instruction.
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
    successors: &[SuccessorList],
    cyclic: Option<&BlockLiveness>,
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

    for idx in 0..num_insts {
        for vreg in &inst_defs[idx] {
            extend(*vreg, idx);
        }
        for vreg in &inst_uses[idx] {
            extend(*vreg, idx);
        }
    }

    let Some(block_liveness) = cyclic else {
        return ranges;
    };

    let mut live_out = FixedBitSet::with_capacity(vreg_count as usize);
    note_bitset_words(live_out.as_slice().len(), 1);
    for block in 0..block_liveness.blocks.len() {
        let (start, end) = block_liveness.blocks.span(block);
        note_bitset_words(live_out.as_slice().len(), 2);
        for vreg_idx in block_liveness.live_in[block].ones() {
            extend(VReg::new(vreg_idx as u32), start);
        }
        block_liveness.live_out_into(block, successors, &mut live_out);
        for vreg_idx in live_out.ones() {
            extend(VReg::new(vreg_idx as u32), end);
        }
    }

    ranges
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
/// 2. Record each back-edge (from -> to) as an interval `[to, from]` in a
///    difference array: `+1` at `to`, `-1` just past `from`
/// 3. Prefix-sum the difference array, so each instruction's depth is the
///    number of enclosing loop intervals covering it
///
/// Accumulating intervals rather than marking each range instruction by
/// instruction keeps this linear in instructions plus back-edges; the marking
/// form was back-edges times instructions, which dominated the pass on large
/// bodies with many back-edges.
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

    // Find all back-edges: edges where we jump to an earlier or same instruction.
    // A back-edge from instruction `from` to instruction `to` (where to <= from)
    // indicates a loop from `to` to `from`, inclusive at both ends. Each one is
    // recorded as a delta pair rather than painted across its range, so the cost
    // is one entry per back-edge instead of one per covered instruction. The
    // ranges need no sorting: interval sums do not depend on their order.
    let mut deltas = vec![0i64; num_insts + 1];

    for (from, succs) in successors.iter().enumerate() {
        for &to in succs.as_ref() {
            if to <= from {
                deltas[to] += 1;
                deltas[from + 1] -= 1;
            }
        }
    }

    // An instruction's depth is the number of loop ranges covering it, which one
    // prefix sum over the deltas yields. Accumulate wide and saturate on the way
    // out, matching the per-range `saturating_add` this replaced for counts
    // beyond what a `u32` holds.
    let mut enclosing = 0i64;
    let depths = deltas[..num_insts]
        .iter()
        .map(|delta| {
            enclosing += delta;
            u32::try_from(enclosing).unwrap_or(u32::MAX)
        })
        .collect();

    LoopInfo::from_depths(depths)
}

/// Compute loop info from instructions using the provided callbacks.
///
/// This is a convenience function that builds the label map and successor lists,
/// then calls `compute_loop_info`.
pub fn analyze_loops<I>(
    instructions: &[I],
    get_label: impl Fn(&I) -> Option<LabelId>,
    get_successors: impl Fn(usize, &I, &AHashMap<LabelId, usize>) -> SuccessorList,
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

    #[test]
    fn large_mir_clobber_table_uses_one_borrowed_slice_pointer_per_instruction() {
        const NUM_INSTRUCTIONS: usize = 4096;
        static REPRESENTATIVE_CLOBBERS: [u32; 3] = [3, 5, 8];
        let instructions = vec![TestInst::Ret; NUM_INSTRUCTIONS];

        let info: LivenessInfo<u32> = analyze(
            &instructions,
            0,
            VRegClasses::all_gp(0),
            test_get_label,
            |idx, inst, label_to_idx| {
                test_get_successors(idx, inst, label_to_idx, NUM_INSTRUCTIONS)
            },
            test_get_uses,
            test_get_defs,
            |_| &REPRESENTATIVE_CLOBBERS,
            |_| false,
        );

        // A clobber entry is only a fat pointer; the instruction table owns
        // no per-entry register vectors or copied register elements.
        assert_eq!(
            std::mem::size_of::<&'static [u32]>(),
            2 * std::mem::size_of::<usize>()
        );
        assert_eq!(info.clobbers_at.len(), NUM_INSTRUCTIONS);
        assert!(info.clobbers_at.capacity() >= NUM_INSTRUCTIONS);
        assert!(
            info.clobbers_at
                .iter()
                .all(|clobbers| std::ptr::eq(*clobbers, &REPRESENTATIVE_CLOBBERS[..]))
        );
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
        label_to_idx: &AHashMap<LabelId, usize>,
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

    fn test_get_clobbers(_inst: &TestInst) -> &'static [u32] {
        &[]
    }

    fn reset_dataflow_call_count() {
        DATAFLOW_CALLS.with(|calls| calls.set(0));
    }

    fn dataflow_call_count() -> usize {
        DATAFLOW_CALLS.with(Cell::get)
    }

    fn reference_dataflow(
        num_insts: usize,
        vreg_count: u32,
        successors: &[SuccessorList],
        inst_uses: &[VRegList],
        inst_defs: &[VRegList],
    ) -> (Vec<FixedBitSet>, Vec<FixedBitSet>) {
        let mut live_in: Vec<FixedBitSet> =
            vec![FixedBitSet::with_capacity(vreg_count as usize); num_insts];
        let mut scratch = FixedBitSet::with_capacity(vreg_count as usize);
        loop {
            let mut changed = false;
            for idx in (0..num_insts).rev() {
                scratch.clear();
                for &successor in &successors[idx] {
                    scratch.union_with(&live_in[successor]);
                }
                for vreg in &inst_defs[idx] {
                    scratch.set(vreg.index() as usize, false);
                }
                for vreg in &inst_uses[idx] {
                    scratch.insert(vreg.index() as usize);
                }
                if scratch != live_in[idx] {
                    changed = true;
                    live_in[idx].clone_from(&scratch);
                }
            }
            if !changed {
                break;
            }
        }
        let live_out = live_in
            .iter()
            .enumerate()
            .map(|(idx, _)| {
                let mut set = FixedBitSet::with_capacity(vreg_count as usize);
                for &successor in &successors[idx] {
                    set.union_with(&live_in[successor]);
                }
                set
            })
            .collect();
        (live_in, live_out)
    }

    /// Solve liveness at block granularity, then re-derive the per-instruction
    /// rows, so a block-level fixed point can be compared against a
    /// per-instruction one row for row.
    fn solved_liveness(
        num_insts: usize,
        vreg_count: u32,
        successors: &[SuccessorList],
        inst_uses: &[VRegList],
        inst_defs: &[VRegList],
    ) -> (BlockLiveness, Vec<FixedBitSet>, Vec<FixedBitSet>) {
        let blocks = partition_blocks(num_insts, successors);
        let live_in = solve_block_liveness(
            vreg_count,
            successors,
            inst_uses,
            inst_defs,
            &blocks,
            has_back_edge(successors),
        )
        .0;
        let block_liveness = BlockLiveness { blocks, live_in };
        let rows = instruction_liveness_rows(
            num_insts,
            vreg_count,
            successors,
            inst_uses,
            inst_defs,
            &block_liveness,
        );
        let to_bitset = |set: &AHashSet<VReg>| {
            let mut bits = FixedBitSet::with_capacity(vreg_count as usize);
            for vreg in set {
                bits.insert(vreg.index() as usize);
            }
            bits
        };
        let row_live_in = rows.iter().map(|row| to_bitset(&row.live_in)).collect();
        let row_live_out = rows.iter().map(|row| to_bitset(&row.live_out)).collect();
        (block_liveness, row_live_in, row_live_out)
    }

    fn assert_worklist_matches_reference(
        num_insts: usize,
        vreg_count: u32,
        successors: &[SuccessorList],
        inst_uses: &[VRegList],
        inst_defs: &[VRegList],
    ) {
        assert!(has_back_edge(successors));
        let (expected_in, expected_out) =
            reference_dataflow(num_insts, vreg_count, successors, inst_uses, inst_defs);
        let (_, live_in, live_out) =
            solved_liveness(num_insts, vreg_count, successors, inst_uses, inst_defs);
        assert_eq!(live_in, expected_in);
        assert_eq!(live_out, expected_out);
    }

    #[test]
    fn predecessor_csr_is_descending_and_flat() {
        let successors: Vec<SuccessorList> = [
            [1, 2].into_iter().collect(),
            [2].into_iter().collect(),
            [1].into_iter().collect(),
            SuccessorList::new(),
        ]
        .into();
        let (offsets, predecessors) = build_predecessor_csr(&successors);
        assert_eq!(offsets, vec![0, 0, 2, 4, 4]);
        assert_eq!(predecessors, vec![2, 0, 1, 0]);
    }

    #[test]
    fn cyclic_worklist_matches_reverse_sweep_on_bounded_cfgs() {
        let mut state = 0x5eed_u64;
        let next = |state: &mut u64| {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (*state >> 32) as usize
        };

        for _case in 0..64 {
            let num_insts = 4 + next(&mut state) % 20;
            let vreg_count = 1 + (next(&mut state) % 8) as u32;
            let mut successors = vec![SuccessorList::new(); num_insts];
            for idx in 0..num_insts {
                if idx + 1 < num_insts {
                    successors[idx].push(idx + 1);
                }
                if idx > 1 && next(&mut state) % 3 == 0 {
                    successors[idx].push(next(&mut state) % idx);
                }
            }
            // Ensure the generated graph is cyclic while retaining the
            // bounded two-successor MIR contract.
            successors[num_insts - 1] = [num_insts - 2, 1].into_iter().collect();

            let mut uses = Vec::with_capacity(num_insts);
            let mut defs = Vec::with_capacity(num_insts);
            for _ in 0..num_insts {
                let mut use_facts = VRegList::new();
                let mut def_facts = VRegList::new();
                if next(&mut state) % 2 == 0 {
                    use_facts.push(VReg::new((next(&mut state) % vreg_count as usize) as u32));
                }
                if next(&mut state) % 2 == 0 {
                    def_facts.push(VReg::new((next(&mut state) % vreg_count as usize) as u32));
                }
                uses.push(use_facts);
                defs.push(def_facts);
            }
            assert_worklist_matches_reference(num_insts, vreg_count, &successors, &uses, &defs);
        }
    }

    /// The construction RUE-2011 replaced: extend a vreg's range at every
    /// instruction where it is defined, used, or live-in.
    ///
    /// This is the specification the boundary-driven construction in
    /// [`build_live_ranges`] answers, kept here so the two can be compared
    /// directly. It is O(instructions x live set); the production construction
    /// reads the same ranges off the ends of each live run instead.
    fn live_in_scan_reference(
        num_insts: usize,
        vreg_count: u32,
        inst_uses: &[VRegList],
        inst_defs: &[VRegList],
        live_in: &[FixedBitSet],
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
        }

        ranges
    }

    fn assert_ranges_match_scan(
        label: &str,
        num_insts: usize,
        vreg_count: u32,
        successors: &[SuccessorList],
        inst_uses: &[VRegList],
        inst_defs: &[VRegList],
    ) {
        // Both constructions are compared on the path that consumes the
        // dataflow, so the range builder is asked for the cyclic treatment even
        // when the generated graph happens to be loop-free. The loop-free
        // production path is a separate, deliberate approximation that never
        // reads liveness at all (RUE-302), so it is not what this equivalence
        // is about.
        let (block_liveness, live_in, _) =
            solved_liveness(num_insts, vreg_count, successors, inst_uses, inst_defs);

        let built = build_live_ranges(
            num_insts,
            vreg_count,
            inst_uses,
            inst_defs,
            successors,
            Some(&block_liveness),
        );
        let expected =
            live_in_scan_reference(num_insts, vreg_count, inst_uses, inst_defs, &live_in);
        assert_eq!(
            built.iter().collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>(),
            "{label}: block-boundary ranges disagree with the live-in scan"
        );
    }

    /// Build the successor, use, and def tables a `TestInst` program implies.
    fn test_program_facts(
        instructions: &[TestInst],
    ) -> (Vec<SuccessorList>, Vec<VRegList>, Vec<VRegList>) {
        let num_insts = instructions.len();
        let label_to_idx = build_label_map(instructions, test_get_label);
        let successors = build_successor_lists(instructions, &label_to_idx, |idx, inst, labels| {
            test_get_successors(idx, inst, labels, num_insts)
        });
        let uses = instructions.iter().map(test_get_uses).collect();
        let defs = instructions.iter().map(test_get_defs).collect();
        (successors, uses, defs)
    }

    fn assert_program_ranges_match_scan(label: &str, instructions: &[TestInst], vreg_count: u32) {
        let (successors, uses, defs) = test_program_facts(instructions);
        assert_ranges_match_scan(
            label,
            instructions.len(),
            vreg_count,
            &successors,
            &uses,
            &defs,
        );
    }

    #[test]
    fn live_ranges_match_a_live_in_scan_on_loop_shapes() {
        let l0 = LabelId::new(0);
        let l1 = LabelId::new(1);
        let l2 = LabelId::new(2);

        // A value defined before a loop and used after it stays live around the
        // back-edge, so its range covers the whole loop body even though the
        // body neither defines nor uses it.
        assert_program_ranges_match_scan(
            "value live across a back-edge",
            &[
                TestInst::Def { dst: 0 },
                TestInst::Def { dst: 1 },
                TestInst::Label { id: l0 },
                TestInst::Def { dst: 2 },
                TestInst::Use { src: 2 },
                TestInst::Use { src: 1 },
                TestInst::Branch { label: l0 },
                TestInst::Use { src: 0 },
                TestInst::Ret,
            ],
            3,
        );

        // A loop-carried value whose use precedes its definition inside the
        // body: liveness runs backwards from the use to the loop header and
        // forwards from the definition to the back-edge.
        assert_program_ranges_match_scan(
            "loop-carried value used before it is redefined",
            &[
                TestInst::Def { dst: 0 },
                TestInst::Label { id: l0 },
                TestInst::Use { src: 0 },
                TestInst::Def { dst: 1 },
                TestInst::Move { dst: 0, src: 1 },
                TestInst::Branch { label: l0 },
                TestInst::Ret,
            ],
            2,
        );

        // Nested loops: the outer back-edge extends a value the inner body only
        // reads, and the inner back-edge extends one the outer body defines.
        assert_program_ranges_match_scan(
            "nested loops",
            &[
                TestInst::Def { dst: 0 },
                TestInst::Label { id: l0 },
                TestInst::Def { dst: 1 },
                TestInst::Label { id: l1 },
                TestInst::Use { src: 0 },
                TestInst::Use { src: 1 },
                TestInst::Def { dst: 2 },
                TestInst::Branch { label: l1 },
                TestInst::Use { src: 2 },
                TestInst::Branch { label: l0 },
                TestInst::Use { src: 1 },
                TestInst::Ret,
            ],
            3,
        );

        // A value defined inside a loop and dead before it: the range must not
        // reach back to the header even though the header is inside the loop.
        assert_program_ranges_match_scan(
            "value defined in a loop and dead before it",
            &[
                TestInst::Def { dst: 0 },
                TestInst::Label { id: l0 },
                TestInst::Use { src: 0 },
                TestInst::Def { dst: 1 },
                TestInst::Use { src: 1 },
                TestInst::Branch { label: l0 },
                TestInst::Ret,
            ],
            2,
        );

        // A value with no definition is live-in to the function, so its range
        // starts at instruction 0 rather than at its first use.
        assert_program_ranges_match_scan(
            "value live-in to the function",
            &[
                TestInst::Def { dst: 0 },
                TestInst::Label { id: l0 },
                TestInst::Use { src: 0 },
                TestInst::Branch { label: l0 },
                TestInst::Use { src: 1 },
                TestInst::Ret,
            ],
            2,
        );

        // A loop whose body is skipped by a forward branch, so the rows next to
        // each other in the instruction stream are not adjacent in the CFG.
        assert_program_ranges_match_scan(
            "forward branch over a loop body",
            &[
                TestInst::Def { dst: 0 },
                TestInst::Branch { label: l2 },
                TestInst::Label { id: l1 },
                TestInst::Def { dst: 1 },
                TestInst::Use { src: 1 },
                TestInst::Branch { label: l1 },
                TestInst::Label { id: l2 },
                TestInst::Use { src: 0 },
                TestInst::Ret,
            ],
            2,
        );

        // A self-looping instruction: its own successor is itself, which is a
        // back-edge that is not a straight-line row.
        assert_program_ranges_match_scan(
            "self loop",
            &[
                TestInst::Def { dst: 0 },
                TestInst::Label { id: l0 },
                TestInst::Branch { label: l0 },
                TestInst::Use { src: 0 },
                TestInst::Ret,
            ],
            1,
        );
    }

    #[test]
    fn live_ranges_match_a_live_in_scan_on_generated_cfgs() {
        let mut state = 0xd1ce_u64;
        let next = |state: &mut u64| {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (*state >> 32) as usize
        };

        for case in 0..256 {
            let num_insts = 4 + next(&mut state) % 40;
            // Exercise both a single dataflow word and several of them, since
            // the transition scan walks the rows word by word.
            let vreg_count = 1 + (next(&mut state) % 140) as u32;
            let mut successors = vec![SuccessorList::new(); num_insts];
            for idx in 0..num_insts {
                match next(&mut state) % 8 {
                    // A return or trap: no successors at all.
                    0 => {}
                    // A backward branch, which is what makes the graph cyclic.
                    1 | 2 if idx > 0 => {
                        if idx + 1 < num_insts {
                            successors[idx].push(idx + 1);
                        }
                        successors[idx].push(next(&mut state) % (idx + 1));
                    }
                    // A forward branch, so neighbouring rows are not always
                    // adjacent in the CFG.
                    3 if idx + 2 < num_insts => {
                        successors[idx].push(idx + 1);
                        successors[idx]
                            .push(idx + 2 + next(&mut state) % (num_insts - idx - 2).max(1));
                    }
                    _ => {
                        if idx + 1 < num_insts {
                            successors[idx].push(idx + 1);
                        }
                    }
                }
            }
            // Some cases stay loop-free on purpose; the rest carry a back-edge.
            if case % 4 != 0 {
                successors[num_insts - 1] = [num_insts / 2].into_iter().collect();
            }

            let mut uses = Vec::with_capacity(num_insts);
            let mut defs = Vec::with_capacity(num_insts);
            for _ in 0..num_insts {
                let mut use_facts = VRegList::new();
                let mut def_facts = VRegList::new();
                if next(&mut state) % 2 == 0 {
                    use_facts.push(VReg::new((next(&mut state) % vreg_count as usize) as u32));
                }
                // Definitions never name vreg 0, so every case has one value
                // that is used without ever being defined and is therefore
                // live-in to the whole function.
                if vreg_count > 1 && next(&mut state) % 2 == 0 {
                    let defined = 1 + next(&mut state) % (vreg_count as usize - 1);
                    def_facts.push(VReg::new(defined as u32));
                }
                uses.push(use_facts);
                defs.push(def_facts);
            }

            assert_ranges_match_scan(
                &format!("generated case {case}"),
                num_insts,
                vreg_count,
                &successors,
                &uses,
                &defs,
            );
        }
    }

    #[test]
    #[should_panic]
    fn malformed_successor_target_still_panics() {
        let successors: Vec<SuccessorList> = [[0, 99].into_iter().collect()].into();
        let _ = partition_blocks(1, &successors);
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
    fn acyclic_production_skips_dataflow_but_debug_retains_exact_liveness() {
        let instructions = vec![
            TestInst::Def { dst: 0 },
            TestInst::Use { src: 0 },
            TestInst::Ret,
        ];
        let num_insts = instructions.len();
        let classes = VRegClasses::all_gp(1);
        reset_dataflow_call_count();
        let production = analyze(
            &instructions,
            1,
            classes.clone(),
            test_get_label,
            |idx, inst, labels| test_get_successors(idx, inst, labels, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
            |_| false,
        );
        assert_eq!(dataflow_call_count(), 0);

        reset_dataflow_call_count();
        let (debug_liveness, debug) = analyze_with_debug(
            &instructions,
            1,
            classes,
            test_get_label,
            |idx, inst, labels| test_get_successors(idx, inst, labels, num_insts),
            test_get_uses,
            test_get_defs,
            test_get_clobbers,
            |_| false,
        );
        assert_eq!(dataflow_call_count(), 1);
        assert_eq!(
            debug_liveness.range(VReg::new(0)),
            production.range(VReg::new(0))
        );
        assert!(debug.instructions[0].live_in.is_empty());
        assert!(debug.instructions[0].live_out.contains(&VReg::new(0)));
        assert!(debug.instructions[1].live_in.contains(&VReg::new(0)));
        assert!(debug.instructions[2].live_in.is_empty());
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

        reset_dataflow_call_count();
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
        // last textual use is instruction 2. This is the cyclic extent that
        // must survive when range construction omits the redundant live-out
        // scan.
        assert_eq!(info.range(VReg::new(0)), Some(&LiveRange::new(0, 3)));
        assert_eq!(dataflow_call_count(), 1);
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
    fn dataflow_scratch_is_cleared_across_blocks_and_fixed_point_rounds() {
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

        // Instructions 1 and 2 form one block, and the use at instruction 1
        // reaches instruction 2 only through the back-edge, so convergence
        // requires revisiting that block. Instruction 0 is a block of its own,
        // processed after the live one, and remains empty — proof that one
        // block's scratch bits leak neither into the next block nor into the
        // next round.
        let blocks = partition_blocks(4, &successors);
        assert_eq!(blocks.starts, vec![0, 1, 3, 4]);
        let (block_live_in, block_visits) = solve_block_liveness(
            1,
            &successors,
            &uses,
            &defs,
            &blocks,
            has_back_edge(&successors),
        );
        assert_eq!(
            block_visits, 4,
            "the worklist must revisit the block whose successors changed"
        );

        let (_, live_in, live_out) = solved_liveness(4, 1, &successors, &uses, &defs);
        let ones = |set: &FixedBitSet| set.ones().collect::<Vec<_>>();
        assert!(block_live_in[0].is_clear());
        assert_eq!(ones(&block_live_in[1]), vec![0]);
        assert!(block_live_in[2].is_clear());
        assert!(live_in[0].is_clear());
        assert_eq!(ones(&live_in[1]), vec![0]);
        assert_eq!(ones(&live_in[2]), vec![0]);
        assert!(live_in[3].is_clear());
        assert!(live_out[0].is_clear());
        assert_eq!(ones(&live_out[1]), vec![0]);
        assert_eq!(ones(&live_out[2]), vec![0]);
        assert!(live_out[3].is_clear());
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

        // Straight-line control flow is one block, so the sweep is one visit
        // whatever the instruction count.
        let blocks = partition_blocks(3, &successors);
        assert_eq!(blocks.starts, vec![0, 3]);
        let (_, block_visits) = solve_block_liveness(
            1,
            &successors,
            &uses,
            &defs,
            &blocks,
            has_back_edge(&successors),
        );
        assert_eq!(
            block_visits, 1,
            "forward-only dataflow visits each block once"
        );

        let (_, live_in, live_out) = solved_liveness(3, 1, &successors, &uses, &defs);
        let ones = |set: &FixedBitSet| set.ones().collect::<Vec<_>>();
        assert_eq!(ones(&live_in[0]), vec![0]);
        assert_eq!(ones(&live_in[1]), vec![0]);
        assert_eq!(ones(&live_in[2]), vec![0]);
        assert_eq!(ones(&live_out[0]), vec![0]);
        assert_eq!(ones(&live_out[1]), vec![0]);
        assert!(live_out[2].is_clear());
    }

    #[test]
    fn blocks_split_at_branch_targets_and_at_every_non_fallthrough_row() {
        // 0 falls through; 1 branches to 4 or falls through; 2 and 3 fall
        // through; 4 returns. Instruction 2 leads a block because instruction 1
        // is not a plain fallthrough, and instruction 4 leads one because it is
        // a branch target even though instruction 3 falls into it.
        let successors: Vec<SuccessorList> = [
            [1].into_iter().collect(),
            [2, 4].into_iter().collect(),
            [3].into_iter().collect(),
            [4].into_iter().collect(),
            SuccessorList::new(),
        ]
        .into();
        let blocks = partition_blocks(5, &successors);
        assert_eq!(blocks.starts, vec![0, 2, 4, 5]);
        assert_eq!(blocks.span(0), (0, 1));
        assert_eq!(blocks.span(1), (2, 3));
        assert_eq!(blocks.span(2), (4, 4));
        assert_eq!(
            (0..5).map(|idx| blocks.of(idx)).collect::<Vec<_>>(),
            vec![0, 0, 1, 1, 2]
        );
    }

    #[test]
    fn cyclic_dataflow_work_stays_proportional_to_blocks_not_instructions() {
        // A loop whose body is `size` instructions over `size` virtual
        // registers: the synthetic instructions x vregs shape RUE-1545 is
        // about. A per-instruction live-in table costs at least one full-width
        // pass per instruction to build and another to read; the block-granular
        // solve costs a handful of passes per *block*. So doubling the size
        // roughly doubles the counted bitset work, and would roughly quadruple
        // it if a dense table came back.
        //
        // The counter is maintained by `note_bitset_words`, which every
        // full-width operation in the solve, the range construction, and the
        // debug projection reports through. New dataflow storage has to report
        // through it too, or this test is measuring something other than the
        // work that was added.
        fn dataflow_words(size: u32) -> (u64, usize) {
            let count = size as usize + 2;
            let mut successors = vec![SuccessorList::new(); count];
            let mut uses = vec![VRegList::new(); count];
            let mut defs = vec![VRegList::new(); count];
            for idx in 0..count - 2 {
                successors[idx] = [idx + 1].into_iter().collect();
                defs[idx].push(VReg::new(idx as u32 % size));
                uses[idx].push(VReg::new((idx as u32 + 1) % size));
            }
            // The back-edge closes the loop; the exit row keeps the body from
            // being the whole function.
            successors[count - 2] = [count - 1, 0].into_iter().collect();

            DATAFLOW_BITSET_WORDS.with(|counter| counter.set(0));
            let blocks = partition_blocks(count, &successors);
            let live_in = solve_block_liveness(size, &successors, &uses, &defs, &blocks, true).0;
            let block_liveness = BlockLiveness { blocks, live_in };
            let ranges = build_live_ranges(
                count,
                size,
                &uses,
                &defs,
                &successors,
                Some(&block_liveness),
            );
            assert_eq!(ranges.len(), size as usize);
            (DATAFLOW_BITSET_WORDS.with(Cell::get), count)
        }

        let (small, small_insts) = dataflow_words(512);
        let (large, large_insts) = dataflow_words(1024);
        assert!(
            small < small_insts as u64 && large < large_insts as u64,
            "a per-instruction table costs at least one word per instruction: \
             {small} words for {small_insts} instructions, \
             {large} for {large_insts}"
        );
        assert!(
            large < small * 3,
            "doubling the loop body must not multiply dataflow bitset work by four: \
             {small} words at 512, {large} at 1024"
        );
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

    #[test]
    fn test_analyze_with_loops_matches_the_separate_analyses() {
        // RUE-1846: the allocator now takes liveness and loop info from one
        // walk of the MIR, sharing the label map and successor lists it used to
        // build twice. That combined path must agree exactly with the two
        // standalone analyses it replaced.
        let loop_label = LabelId::new(0);
        let instructions = vec![
            TestInst::Def { dst: 0 },
            TestInst::Label { id: loop_label },
            TestInst::Use { src: 0 },
            TestInst::Branch { label: loop_label },
            TestInst::Ret,
        ];
        let num_insts = instructions.len();

        let separate_liveness: LivenessInfo<u32> = analyze(
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
        let separate_loops =
            analyze_loops(&instructions, test_get_label, |idx, inst, label_to_idx| {
                test_get_successors(idx, inst, label_to_idx, num_insts)
            });

        let (combined_liveness, combined_loops): (LivenessInfo<u32>, _) = analyze_with_loops(
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

        for idx in 0..num_insts {
            assert_eq!(
                combined_loops.depth(idx),
                separate_loops.depth(idx),
                "loop depth diverged at instruction {idx}"
            );
        }
        assert_eq!(
            combined_liveness.range(VReg::new(0)),
            separate_liveness.range(VReg::new(0)),
            "live range diverged from the standalone analysis"
        );
        // The fixture really does contain a back-edge, so the comparison above
        // is not vacuously over an all-zero depth vector.
        assert_eq!(separate_loops.depth(2), 1, "fixture must contain a loop");
    }

    // ========================================
    // RUE-1558: interval accumulation
    // ========================================

    /// The marking form `compute_loop_info` replaced: paint every instruction
    /// covered by every back-edge range. Retained as the differential oracle,
    /// because RUE-1558 is an implementation optimization and the depth vector
    /// it produces must not move.
    fn marked_loop_depths<S: AsRef<[usize]>>(num_insts: usize, successors: &[S]) -> Vec<u32> {
        let mut depths = vec![0u32; num_insts];
        for (from, succs) in successors.iter().enumerate() {
            for &to in succs.as_ref() {
                if to <= from {
                    for depth in depths.iter_mut().take(from + 1).skip(to) {
                        *depth = depth.saturating_add(1);
                    }
                }
            }
        }
        depths
    }

    fn assert_matches_marking_reference(num_insts: usize, successors: &[Vec<usize>], case: &str) {
        let expected = marked_loop_depths(num_insts, successors);
        let actual = compute_loop_info(num_insts, successors);
        for (index, depth) in expected.iter().enumerate() {
            assert_eq!(
                actual.depth(index),
                *depth,
                "{case}: depth diverged from the marking reference at instruction {index}"
            );
        }
        assert_eq!(
            actual.max_depth_in_range(0, num_insts.saturating_sub(1)),
            expected.iter().copied().max().unwrap_or(0),
            "{case}: cached max depth diverged from the marking reference"
        );
    }

    #[test]
    fn interval_accumulation_matches_marking_on_nested_and_overlapping_loops() {
        // Triply nested: 1..8 encloses 2..7 encloses 3..6.
        assert_matches_marking_reference(
            9,
            &[
                vec![1],
                vec![2],
                vec![3],
                vec![4],
                vec![5],
                vec![6],
                vec![3],
                vec![2],
                vec![1],
            ],
            "triply nested",
        );

        // Overlapping but not nested: 0..5 and 3..7 share only 3..5. The
        // marking form and the interval form must agree that the shared
        // stretch is depth 2 and each tail is depth 1.
        assert_matches_marking_reference(
            8,
            &[
                vec![1],
                vec![2],
                vec![3],
                vec![4],
                vec![0],
                vec![6],
                vec![7],
                vec![3],
            ],
            "overlapping",
        );

        // Self-loop: `to == from` is a back-edge covering exactly one
        // instruction, the inclusive-at-both-ends boundary case.
        assert_matches_marking_reference(3, &[vec![1], vec![1], vec![]], "self loop");

        // Two back-edges landing on the same header, and a range covering the
        // whole function.
        assert_matches_marking_reference(
            6,
            &[vec![1], vec![2], vec![1], vec![4], vec![1], vec![0]],
            "shared header",
        );

        // No back-edges at all: every depth stays zero.
        assert_matches_marking_reference(4, &[vec![1], vec![2], vec![3], vec![]], "straight line");
    }

    #[test]
    fn interval_accumulation_matches_marking_on_randomized_graphs() {
        // A fixed-seed xorshift, so a failure reproduces exactly rather than
        // depending on the run.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for case in 0..200 {
            let num_insts = 1 + (next() % 64) as usize;
            let successors: Vec<Vec<usize>> = (0..num_insts)
                .map(|from| {
                    // Up to three successors per instruction, each anywhere in
                    // range, so back-edges, forward edges and self-loops all
                    // appear at every density.
                    (0..next() % 4)
                        .map(|_| (next() as usize) % num_insts)
                        .chain(if from + 1 < num_insts {
                            Some(from + 1)
                        } else {
                            None
                        })
                        .collect()
                })
                .collect();
            assert_matches_marking_reference(
                num_insts,
                &successors,
                &format!("randomized case {case} ({num_insts} instructions)"),
            );
        }
    }

    #[test]
    fn many_overlapping_back_edges_stay_linear_in_instructions() {
        // Every instruction in the second half carries a back-edge to the
        // matching instruction in the first half, so the ranges overlap
        // heavily and the marking form would touch ~n^2/4 cells. The interval
        // form pays one delta pair per back-edge; this case exists so a
        // reintroduced painting loop shows up as a stalled test rather than a
        // silent slowdown.
        const NUM_INSTS: usize = 20_000;
        let half = NUM_INSTS / 2;
        let successors: Vec<Vec<usize>> = (0..NUM_INSTS)
            .map(|index| {
                if index >= half {
                    vec![index + 1, index - half]
                } else {
                    vec![index + 1]
                }
            })
            .collect();

        let info = compute_loop_info(NUM_INSTS, &successors);

        // Instruction i is covered by every back-edge range [j - half, j] with
        // j >= half and j - half <= i <= j, i.e. by j in [max(half, i), i +
        // half], clamped to the instructions that have back-edges.
        for index in [0, 1, half - 1, half, half + 1, NUM_INSTS - 1] {
            let lowest = half.max(index);
            let highest = (index + half).min(NUM_INSTS - 1);
            let expected = u32::try_from(highest.saturating_sub(lowest) + 1).unwrap();
            assert_eq!(
                info.depth(index),
                expected,
                "instruction {index} sits inside {expected} overlapping loop ranges"
            );
        }
    }

    #[test]
    fn back_edge_ranges_are_inclusive_at_both_ends() {
        // 2 -> 1 covers exactly instructions 1 and 2: the header it lands on
        // and the instruction the edge leaves from. An off-by-one in the
        // difference array would drop one end or bleed into instruction 3.
        let info = compute_loop_info(4, &[vec![1], vec![2], vec![1, 3], vec![]]);
        assert_eq!(info.depth(0), 0, "before the header");
        assert_eq!(info.depth(1), 1, "the header is inside its own loop");
        assert_eq!(info.depth(2), 1, "the back-edge source is inside the loop");
        assert_eq!(info.depth(3), 0, "the exit is outside the loop");
    }
}
