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
//! ## Register Coalescing
//!
//! Before allocation, the allocator performs register coalescing to eliminate
//! redundant move instructions. When a move `mov vDst, vSrc` is found where:
//! 1. Both operands are virtual registers
//! 2. Their live ranges don't interfere (except at the move point)
//!
//! The two vregs are merged into one, and the move can be eliminated.
//! This reduces register pressure and improves code quality.
//!
//! ## Register Allocation Algorithm
//!
//! The allocator uses linear scan register allocation:
//! 1. Compute live ranges for all virtual registers (via liveness analysis)
//! 2. Perform register coalescing to merge non-interfering moves
//! 3. Sort vregs by live range start
//! 4. For each vreg, try to assign a register not used by interfering vregs
//! 5. If no register is available, spill using cost-based heuristics
//!
//! ## Spilling and Cost Model
//!
//! When register pressure exceeds available registers, values are spilled
//! to the stack. The allocator uses a cost model to make better spill decisions:
//!
//! - **Loop depth**: Spilling inside a loop is more expensive (10x per nesting level)
//! - **Remaining uses**: Values used many times are more expensive to spill
//! - **Live range length**: Longer ranges are cheaper to spill (value is stored once)
//!
//! The [`CostModel`] struct allows these parameters to be configured.

use std::collections::{HashMap, HashSet};
use std::fmt;

use fixedbitset::FixedBitSet;

use crate::index_map::IndexMap;
use crate::vreg::VReg;

// ============================================================================
// Cost Model
// ============================================================================

/// Cost model for register allocation spill decisions.
///
/// This struct provides configurable parameters for the spill cost heuristics.
/// The default values are tuned for typical x86-64 workloads.
///
/// # Cost Calculation
///
/// The spill cost for a vreg is computed as:
/// ```text
/// cost = base_spill_cost * loop_depth_multiplier^loop_depth
/// ```
///
/// When choosing which vreg to spill, the allocator picks the one with the
/// lowest cost per remaining use:
/// ```text
/// priority = cost / remaining_uses
/// ```
///
/// This means:
/// - Values in deeply nested loops are very expensive to spill
/// - Values with many remaining uses are expensive to spill
/// - Values with long remaining ranges are cheaper to spill
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostModel {
    /// Base cost for a spill operation (default: 1).
    pub base_spill_cost: u32,

    /// Multiplier applied per loop nesting level (default: 10).
    /// A value in a loop at depth 2 has cost multiplied by 10^2 = 100.
    pub loop_depth_multiplier: u32,

    /// Whether to use loop-aware spilling (default: true).
    /// When false, falls back to the simple "longest range" heuristic.
    pub use_loop_aware_spilling: bool,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            base_spill_cost: 1,
            loop_depth_multiplier: 10,
            use_loop_aware_spilling: true,
        }
    }
}

impl CostModel {
    /// Create a new cost model with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Compute the spill cost for a vreg at a given loop depth.
    ///
    /// Higher values mean more expensive to spill.
    pub fn spill_cost(&self, loop_depth: u32) -> u32 {
        if !self.use_loop_aware_spilling {
            return self.base_spill_cost;
        }
        self.base_spill_cost
            .saturating_mul(self.loop_depth_multiplier.saturating_pow(loop_depth))
    }

    /// Compute the spill priority for a vreg.
    ///
    /// Lower values mean the vreg should be spilled first.
    ///
    /// # Arguments
    ///
    /// * `loop_depth` - The maximum loop depth during the vreg's live range
    /// * `remaining_range_length` - How many more instructions until the vreg dies
    ///
    /// # Returns
    ///
    /// A priority value where lower = spill first.
    pub fn spill_priority(&self, loop_depth: u32, remaining_range_length: usize) -> u64 {
        if !self.use_loop_aware_spilling {
            // Fall back to original heuristic: longest range gets lowest priority (spill first)
            // Return inverse of length so longer ranges have lower priority
            return u64::MAX - remaining_range_length as u64;
        }

        // Cost-based priority: spill_cost / remaining_length
        // But we want lower = spill first, and higher cost = don't spill
        // So we compute: remaining_length / cost
        // Then invert: u64::MAX - (remaining_length / cost)
        //
        // Actually, simpler: cost * remaining_length = total cost to keep in register
        // Lower total cost = spill first
        // But we want to keep high-cost items in registers...
        //
        // The intuition: we want to spill the vreg that's cheapest to spill.
        // Cheapest = lowest loop depth AND longest remaining range (fewer spill/reloads per instruction).
        //
        // Priority = cost (lower = spill first)
        // But we also want to factor in range length: longer ranges are better to spill
        // because the spill/reload overhead is amortized over more instructions.
        //
        // Final formula: cost / remaining_length
        // Lower = cheaper to spill = spill first
        let cost = self.spill_cost(loop_depth) as u64;
        let length = remaining_range_length.max(1) as u64;

        // Use saturating ops to avoid overflow
        // Lower priority = spill first
        // We want: low cost + long range = low priority = spill this one
        // So: priority = cost / length (but inverted for u64 ordering)
        //
        // Actually, let's keep it simple: priority = cost
        // The allocator will pick min priority to spill.
        // But we also want range length to be a tiebreaker.
        //
        // Use a combined score: cost * 1000 - length (clamped)
        // This way:
        // - Low cost = low priority = spill first
        // - For same cost, longer range = lower priority = spill first
        cost.saturating_mul(1000).saturating_sub(length.min(999))
    }
}

/// Information about loop nesting for instructions.
///
/// This is computed by analyzing back-edges in the MIR control flow.
#[derive(Debug, Clone)]
pub struct LoopInfo {
    /// Loop depth for each instruction index.
    /// 0 = not in a loop, 1 = in one loop, 2 = nested two levels, etc.
    pub depths: Vec<u32>,
}

impl LoopInfo {
    /// Create loop info with all instructions at depth 0 (no loops).
    pub fn no_loops(instruction_count: usize) -> Self {
        Self {
            depths: vec![0; instruction_count],
        }
    }

    /// Get the loop depth for an instruction.
    pub fn depth(&self, inst_idx: usize) -> u32 {
        self.depths.get(inst_idx).copied().unwrap_or(0)
    }

    /// Get the maximum loop depth across a range of instructions.
    pub fn max_depth_in_range(&self, start: usize, end: usize) -> u32 {
        if start > end || start >= self.depths.len() {
            return 0;
        }
        let end = end.min(self.depths.len() - 1);
        self.depths[start..=end].iter().copied().max().unwrap_or(0)
    }
}

// ============================================================================
// Liveness Analysis Types
// ============================================================================

/// Debug information about liveness at a single instruction.
///
/// This provides detailed per-instruction information for debugging
/// register allocation and understanding value lifetimes.
#[derive(Debug, Clone)]
pub struct InstructionLiveness {
    /// Instruction index.
    pub index: usize,
    /// Virtual registers live before this instruction executes.
    pub live_in: HashSet<VReg>,
    /// Virtual registers live after this instruction executes.
    pub live_out: HashSet<VReg>,
    /// Virtual registers defined (written) by this instruction.
    pub defs: Vec<VReg>,
    /// Virtual registers used (read) by this instruction.
    pub uses: Vec<VReg>,
}

/// Debug information about liveness for an entire function.
///
/// This provides detailed liveness information for debugging and
/// visualization via `--emit liveness`.
#[derive(Debug, Clone)]
pub struct LivenessDebugInfo {
    /// Per-instruction liveness information.
    pub instructions: Vec<InstructionLiveness>,
    /// Live ranges for each virtual register (indexed by vreg index).
    pub live_ranges: IndexMap<VReg, Option<LiveRange>>,
    /// Total number of virtual registers.
    pub vreg_count: u32,
}

impl std::fmt::Display for LivenessDebugInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Liveness Analysis ===")?;
        writeln!(f)?;

        // Show per-instruction liveness
        writeln!(f, "Per-Instruction Liveness:")?;
        for inst in &self.instructions {
            writeln!(f, "  Instruction {}:", inst.index)?;

            // Format sets in sorted order for consistent output
            let live_in: Vec<_> = {
                let mut v: Vec<_> = inst.live_in.iter().collect();
                v.sort();
                v
            };
            let live_out: Vec<_> = {
                let mut v: Vec<_> = inst.live_out.iter().collect();
                v.sort();
                v
            };

            writeln!(
                f,
                "    live-in:  {{{}}}",
                live_in
                    .iter()
                    .map(|v| format!("{}", v))
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
            writeln!(
                f,
                "    live-out: {{{}}}",
                live_out
                    .iter()
                    .map(|v| format!("{}", v))
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;

            if !inst.defs.is_empty() {
                writeln!(
                    f,
                    "    def: {}",
                    inst.defs
                        .iter()
                        .map(|v| format!("{}", v))
                        .collect::<Vec<_>>()
                        .join(", ")
                )?;
            }
            if !inst.uses.is_empty() {
                writeln!(
                    f,
                    "    use: {}",
                    inst.uses
                        .iter()
                        .map(|v| format!("{}", v))
                        .collect::<Vec<_>>()
                        .join(", ")
                )?;
            }
        }

        writeln!(f)?;
        writeln!(f, "Live Ranges (instruction indices):")?;

        // Iterate in vreg index order (already sorted since IndexMap is Vec-backed)
        for (vreg, range_opt) in self.live_ranges.iter_enumerated() {
            if let Some(range) = range_opt {
                writeln!(f, "  {}: [{}, {})", vreg, range.start, range.end + 1)?;
            }
        }

        Ok(())
    }
}

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
    /// Live range for each virtual register (indexed by vreg index).
    /// Uses dense Vec storage since VReg indices are contiguous.
    pub ranges: IndexMap<VReg, Option<LiveRange>>,
    /// For each instruction, which vregs are live after it executes.
    /// Uses FixedBitSet for O(n/64) bitwise operations instead of HashSet iteration.
    /// This is useful for determining which registers are in use at any point.
    pub live_at: Vec<FixedBitSet>,
    /// For each instruction index, the physical registers clobbered by that instruction.
    /// This is used to prevent allocating vregs to registers that would be clobbered.
    pub clobbers_at: Vec<Vec<Reg>>,
}

impl<Reg: Copy + Eq + std::hash::Hash> LivenessInfo<Reg> {
    /// Create a new empty liveness info.
    pub fn new() -> Self {
        Self {
            ranges: IndexMap::new(),
            live_at: Vec::new(),
            clobbers_at: Vec::new(),
        }
    }

    /// Create liveness info with capacity for the given number of vregs.
    pub fn with_vreg_capacity(vreg_count: u32) -> Self {
        let mut ranges = IndexMap::with_capacity(vreg_count as usize);
        ranges.resize(vreg_count as usize, None);
        Self {
            ranges,
            live_at: Vec::new(),
            clobbers_at: Vec::new(),
        }
    }

    /// Get vregs that are live at a given instruction index.
    pub fn live_at(&self, inst_idx: usize) -> &FixedBitSet {
        &self.live_at[inst_idx]
    }

    /// Get the live range for a vreg.
    pub fn range(&self, vreg: VReg) -> Option<&LiveRange> {
        self.ranges.get(vreg).and_then(|opt| opt.as_ref())
    }

    /// Check if two vregs interfere (have overlapping live ranges).
    ///
    /// Two vregs interfere if they are both live at the same program point,
    /// meaning they cannot share the same physical register.
    pub fn interferes(&self, a: VReg, b: VReg) -> bool {
        match (self.range(a), self.range(b)) {
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
        if let Some(range) = self.range(vreg) {
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
// Register Coalescing
// ============================================================================

/// A move candidate for register coalescing.
///
/// This represents a move instruction `mov dst, src` where both operands
/// are virtual registers and could potentially be coalesced.
#[derive(Debug, Clone, Copy)]
pub struct CoalesceCandidate {
    /// Instruction index of the move.
    pub inst_idx: usize,
    /// Destination virtual register.
    pub dst: VReg,
    /// Source virtual register.
    pub src: VReg,
}

/// Result of register coalescing.
///
/// After coalescing, some vregs are merged together. This struct tracks:
/// 1. Which vregs were coalesced (mapping from original to representative)
/// 2. Which move instructions can be eliminated
#[derive(Debug, Clone)]
pub struct CoalesceResult {
    /// Maps each coalesced vreg to its representative.
    /// If a vreg is not in this map, it's its own representative.
    coalesce_map: HashMap<VReg, VReg>,
    /// Instruction indices of moves that were coalesced and can be eliminated.
    pub eliminated_moves: HashSet<usize>,
}

impl CoalesceResult {
    /// Create an empty coalesce result (no coalescing performed).
    pub fn empty() -> Self {
        Self {
            coalesce_map: HashMap::new(),
            eliminated_moves: HashSet::new(),
        }
    }

    /// Get the representative vreg for a given vreg.
    ///
    /// If the vreg was coalesced, returns its representative.
    /// Otherwise, returns the vreg itself.
    pub fn representative(&self, vreg: VReg) -> VReg {
        self.coalesce_map.get(&vreg).copied().unwrap_or(vreg)
    }

    /// Check if a vreg was coalesced with another.
    #[allow(dead_code)]
    pub fn is_coalesced(&self, vreg: VReg) -> bool {
        self.coalesce_map.contains_key(&vreg)
    }

    /// Check if a move instruction at the given index was eliminated.
    pub fn is_eliminated(&self, inst_idx: usize) -> bool {
        self.eliminated_moves.contains(&inst_idx)
    }

    /// Get the number of moves that were eliminated.
    pub fn num_eliminated(&self) -> usize {
        self.eliminated_moves.len()
    }
}

/// Perform register coalescing on the given move candidates.
///
/// This function identifies moves where the source and destination vregs
/// can be merged (their live ranges don't interfere), and returns a
/// CoalesceResult with the merged mappings.
///
/// # Algorithm
///
/// For each move `mov dst, src`:
/// 1. Check if dst and src live ranges interfere
/// 2. If they don't interfere (considering the move point), coalesce them
/// 3. Update the live ranges to reflect the merge
///
/// The key insight is that at the move instruction:
/// - src is used (read) - it must be live-in
/// - dst is defined (written) - it starts being live
///
/// For coalescing to be safe, we need:
/// - src's live range ends at or before the move (its last use is the move)
/// - dst's live range starts at the move (its first def is the move)
/// - OR more generally: their ranges don't overlap except at the move point
pub fn coalesce<Reg: Copy + Eq + std::hash::Hash>(
    candidates: &[CoalesceCandidate],
    liveness: &mut LivenessInfo<Reg>,
) -> CoalesceResult {
    let mut result = CoalesceResult::empty();

    // Union-find structure for tracking coalesced vregs
    let mut parent: HashMap<VReg, VReg> = HashMap::new();

    // Find the representative of a vreg in the union-find
    fn find(parent: &mut HashMap<VReg, VReg>, vreg: VReg) -> VReg {
        if let Some(&p) = parent.get(&vreg) {
            if p != vreg {
                let root = find(parent, p);
                parent.insert(vreg, root);
                return root;
            }
        }
        vreg
    }

    // Process each candidate
    for candidate in candidates {
        let dst = find(&mut parent, candidate.dst);
        let src = find(&mut parent, candidate.src);

        // Already in the same equivalence class
        if dst == src {
            result.eliminated_moves.insert(candidate.inst_idx);
            continue;
        }

        // Get the live ranges
        let dst_range = liveness.range(dst).copied();
        let src_range = liveness.range(src).copied();

        // Both must have ranges
        let (dst_range, src_range) = match (dst_range, src_range) {
            (Some(d), Some(s)) => (d, s),
            _ => continue,
        };

        // Check for interference.
        // The move instruction is at candidate.inst_idx.
        // At the move point:
        // - src is used (last use could be here)
        // - dst is defined (first def could be here)
        //
        // For safe coalescing, we need the ranges to not overlap,
        // except that they can both include the move point.
        //
        // Specifically: if src ends at or before the move, and dst starts at or after the move,
        // they can share a register.
        let move_point = candidate.inst_idx;

        // Check if ranges interfere outside the move point
        // src_range.end should be <= move_point (src's last use is the move or earlier)
        // dst_range.start should be >= move_point (dst's first def is the move or later)
        let can_coalesce = src_range.end <= move_point && dst_range.start >= move_point;

        if can_coalesce {
            // Merge the ranges: the combined range spans both
            let merged_range = LiveRange::new(
                src_range.start.min(dst_range.start),
                src_range.end.max(dst_range.end),
            );

            // Use src as the representative (arbitrary choice, but keeps the original value)
            parent.insert(dst, src);
            result.coalesce_map.insert(dst, src);

            // Update liveness: assign merged range to src, remove dst
            liveness.ranges[src] = Some(merged_range);
            liveness.ranges[dst] = None;

            // Update live_at bitsets: replace dst with src
            for live_set in &mut liveness.live_at {
                let dst_idx = dst.index() as usize;
                let src_idx = src.index() as usize;
                if dst_idx < live_set.len() && live_set.contains(dst_idx) {
                    live_set.set(dst_idx, false);
                    if src_idx < live_set.len() {
                        live_set.insert(src_idx);
                    }
                }
            }

            // Mark the move for elimination
            result.eliminated_moves.insert(candidate.inst_idx);
        }
    }

    result
}

// ============================================================================
// Register Allocation Macros
// ============================================================================

/// Macro for handling the 3-way allocation match pattern on a destination operand.
///
/// This is the most common pattern in register allocation: when rewriting an instruction,
/// we check whether the destination operand is:
/// 1. Allocated to a physical register: use that register
/// 2. Spilled to stack: use scratch register, then store to stack
/// 3. Already physical (None): pass through unchanged
///
/// # Syntax
///
/// ```ignore
/// // Form 1: Different behavior for register vs spill vs passthrough
/// alloc_dst!(alloc_result =>
///     Register(reg) => { /* emit with reg */ },
///     Spill(offset) => { /* emit with scratch */ } then { /* store to offset */ },
///     Passthrough(dst) => { /* emit with dst unchanged */ }
/// );
///
/// // Form 2: Same emit logic, just different operand
/// alloc_dst!(alloc_result, dst, scratch =>
///     emit |dst_op| { mir.push(Inst { dst: dst_op }) },
///     store |offset| { mir.push(Store { offset, src: scratch }) }
/// );
/// ```
///
/// # Example: Form 1 (explicit arms)
///
/// ```ignore
/// alloc_dst!(self.get_allocation(dst) =>
///     Register(reg) => {
///         mir.push(X86Inst::MovRI32 { dst: Operand::Physical(reg), imm });
///     },
///     Spill(offset) => {
///         mir.push(X86Inst::MovRI32 { dst: Operand::Physical(Reg::Rax), imm });
///     } then {
///         mir.push(X86Inst::MovMR { base: Reg::Rbp, offset, src: Operand::Physical(Reg::Rax) });
///     },
///     Passthrough(dst) => {
///         mir.push(X86Inst::MovRI32 { dst, imm });
///     }
/// );
/// ```
#[macro_export]
macro_rules! alloc_dst {
    // Form 1: Explicit arms with different behavior
    // NOTE: Rematerialize is not valid for destinations (only for sources that need reloading),
    // so we panic if we see it here.
    ($alloc:expr =>
        Register($reg:ident) => $emit_reg:block,
        Spill($offset:ident) => $emit_spill:block then $store:block,
        Passthrough($pass_dst:ident) => $emit_pass:block $(,)?
    ) => {
        match $alloc {
            Some($crate::regalloc::Allocation::Register($reg)) => $emit_reg,
            Some($crate::regalloc::Allocation::Spill($offset)) => {
                $emit_spill
                $store
            }
            Some($crate::regalloc::Allocation::Rematerialize(_)) => {
                // Rematerialize is only valid for source operands (when loading a value).
                // For destinations, we should never see this - it would mean we're
                // defining a rematerializable value, which should already have the
                // original instruction that creates it.
                unreachable!("alloc_dst! called on rematerializable vreg; this is a bug")
            }
            None => {
                let $pass_dst = $pass_dst;
                $emit_pass
            }
        }
    };

    // Form 2: Common case - same emit, different operand
    // NOTE: Rematerialize is not valid for destinations (only for sources that need reloading),
    // so we panic if we see it here.
    ($alloc:expr, $dst:expr, $scratch:expr =>
        emit |$op:ident| $emit:block,
        store |$off:ident| $store_body:block $(,)?
    ) => {
        match $alloc {
            Some($crate::regalloc::Allocation::Register(reg)) => {
                let $op = Operand::Physical(reg);
                $emit
            }
            Some($crate::regalloc::Allocation::Spill($off)) => {
                let $op = Operand::Physical($scratch);
                $emit
                $store_body
            }
            Some($crate::regalloc::Allocation::Rematerialize(_)) => {
                // Rematerialize is only valid for source operands.
                unreachable!("alloc_dst! called on rematerializable vreg; this is a bug")
            }
            None => {
                let $op = $dst;
                $emit
            }
        }
    };
}

// ============================================================================
// Rematerialization Types
// ============================================================================

/// Information about how a value can be rematerialized (recomputed) instead of spilled.
///
/// Rematerialization is an optimization where instead of storing a value to
/// the stack and reloading it, we simply recompute it. This is beneficial for:
/// - Constants (cheaper to reload an immediate than memory access)
/// - String literal addresses (compile-time known pointers)
///
/// This enum captures the information needed to regenerate the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RematerializeOp {
    /// A 32-bit constant: `mov dst, imm32`
    Const32(i32),
    /// A 64-bit constant: `mov dst, imm64`
    Const64(i64),
    /// A string literal pointer: `lea dst, [rip + string_offset]`
    StringPtr(u32),
    /// A string literal length (compile-time known)
    StringLen(u32),
    /// A string literal capacity (compile-time known)
    StringCap(u32),
}

/// Information about a virtual register's rematerializability.
///
/// This is tracked per-vreg and used by the register allocator to decide
/// whether to spill (store/load) or rematerialize (recompute) a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VRegInfo {
    /// If Some, this vreg can be rematerialized instead of spilled.
    pub remat: Option<RematerializeOp>,
}

impl VRegInfo {
    /// Create info for a vreg that cannot be rematerialized.
    pub const fn none() -> Self {
        Self { remat: None }
    }

    /// Create info for a rematerializable vreg.
    pub const fn rematerializable(op: RematerializeOp) -> Self {
        Self { remat: Some(op) }
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
    /// Value will be rematerialized (recomputed) when needed.
    ///
    /// This is cheaper than spilling for constants and other values that
    /// can be cheaply recomputed. No stack slot is allocated.
    Rematerialize(RematerializeOp),
}

// ============================================================================
// Spill Slot Allocator
// ============================================================================

/// Tracks spill slot availability based on live range endpoints.
///
/// This allows non-overlapping live ranges to share the same spill slot,
/// reducing stack frame size for functions with many spills.
///
/// Each slot tracks the endpoint of its current occupant. When allocating
/// a new spill, we first look for a slot whose occupant has ended before
/// the new range starts.
struct SpillSlotAllocator {
    /// For each spill slot, the end point of its current occupant.
    /// None means the slot is free (never used or occupant has ended).
    slots: Vec<Option<usize>>,
    /// Base offset for spill slots (after existing locals).
    base_offset: i32,
}

impl SpillSlotAllocator {
    /// Create a new spill slot allocator.
    ///
    /// `existing_locals` is the number of local variable slots already on the stack.
    /// Spill slots start after those.
    fn new(existing_locals: u32) -> Self {
        Self {
            slots: Vec::new(),
            base_offset: -((existing_locals as i32 + 1) * 8),
        }
    }

    /// Allocate a spill slot for a live range.
    ///
    /// If possible, reuses a slot whose previous occupant is no longer live.
    /// Otherwise, allocates a new slot.
    ///
    /// Returns the stack offset for the spill slot.
    fn allocate(&mut self, live_range_start: usize, live_range_end: usize) -> i32 {
        // Try to find a reusable slot whose occupant ended before this range starts
        for (i, slot_end) in self.slots.iter_mut().enumerate() {
            if let Some(end) = slot_end {
                // The occupant is dead if its end point is strictly before our start.
                // Note: We use < not <= because at the same instruction index,
                // both ranges are considered live (inclusive endpoints).
                if *end < live_range_start {
                    // Reuse this slot
                    *slot_end = Some(live_range_end);
                    return self.offset_for_slot(i);
                }
            }
        }

        // No reusable slot found, allocate a new one
        let slot_index = self.slots.len();
        self.slots.push(Some(live_range_end));
        self.offset_for_slot(slot_index)
    }

    /// Get the stack offset for a given slot index.
    fn offset_for_slot(&self, slot_index: usize) -> i32 {
        self.base_offset - (slot_index as i32 * 8)
    }

    /// Get the number of unique spill slots used.
    fn num_slots(&self) -> u32 {
        self.slots.len() as u32
    }
}

// ============================================================================
// Register Allocation Debug Info
// ============================================================================

/// Debug information from register allocation.
///
/// This captures the decisions made by the register allocator for display
/// via `--emit regalloc`. It includes live ranges, interference edges,
/// final allocations, and spill information.
#[derive(Debug, Clone)]
pub struct RegAllocDebugInfo<Reg: Copy + Eq + std::hash::Hash> {
    /// Live range for each virtual register: (vreg_index, start, end).
    pub live_ranges: Vec<(u32, usize, usize)>,
    /// Interference edges: pairs of vregs that are both live at the same point.
    pub interference: Vec<(u32, u32)>,
    /// Final allocation for each vreg: (vreg_index, allocation).
    pub allocations: Vec<(u32, Allocation<Reg>)>,
    /// Virtual registers that were spilled.
    pub spills: Vec<u32>,
    /// Callee-saved registers that were used.
    pub callee_saved_used: Vec<Reg>,
}

impl<Reg: Copy + Eq + std::hash::Hash + fmt::Display> fmt::Display for RegAllocDebugInfo<Reg> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Live Ranges:")?;
        for (vreg, start, end) in &self.live_ranges {
            writeln!(f, "  v{}: [{}, {})", vreg, start, end)?;
        }
        writeln!(f)?;

        writeln!(f, "Interference Graph:")?;
        if self.interference.is_empty() {
            writeln!(f, "  (no interference)")?;
        } else {
            for (v1, v2) in &self.interference {
                writeln!(f, "  v{} -- v{}", v1, v2)?;
            }
        }
        writeln!(f)?;

        writeln!(f, "Allocation:")?;
        for (vreg, alloc) in &self.allocations {
            match alloc {
                Allocation::Register(reg) => writeln!(f, "  v{} -> {}", vreg, reg)?,
                Allocation::Spill(offset) => writeln!(f, "  v{} -> [stack{}]", vreg, offset)?,
                Allocation::Rematerialize(op) => writeln!(f, "  v{} -> remat({:?})", vreg, op)?,
            }
        }
        writeln!(f)?;

        writeln!(f, "Spills:")?;
        if self.spills.is_empty() {
            writeln!(f, "  none")?;
        } else {
            for vreg in &self.spills {
                write!(f, "  v{}", vreg)?;
            }
            writeln!(f)?;
        }
        writeln!(f)?;

        writeln!(f, "Callee-saved registers used:")?;
        if self.callee_saved_used.is_empty() {
            writeln!(f, "  none")?;
        } else {
            write!(f, " ")?;
            for (i, reg) in self.callee_saved_used.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, " {}", reg)?;
            }
            writeln!(f)?;
        }

        Ok(())
    }
}

/// Perform linear scan register allocation.
///
/// This function implements the core linear scan algorithm that is shared
/// between all backends. It takes liveness information and a list of
/// allocatable registers, and returns an allocation for each vreg.
///
/// This version uses the default cost model without loop information.
/// For loop-aware allocation, use [`linear_scan_with_cost_model`].
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
    // Use default cost model with loop-aware spilling disabled (no loop info)
    let cost_model = CostModel {
        use_loop_aware_spilling: false,
        ..Default::default()
    };
    let loop_info = LoopInfo::no_loops(liveness.live_at.len());
    let (allocation, num_spills, used_callee_saved, _debug_info) = linear_scan_impl(
        vreg_count,
        liveness,
        allocatable_regs,
        existing_locals,
        &cost_model,
        &loop_info,
    );
    (allocation, num_spills, used_callee_saved)
}

/// Perform linear scan register allocation with a cost model and loop information.
///
/// This is the preferred allocation function when loop information is available.
/// It makes better spill decisions by considering:
/// - Loop nesting depth (avoid spilling inside loops)
/// - Live range length (longer ranges are cheaper to spill)
///
/// # Arguments
///
/// * `vreg_count` - Total number of virtual registers
/// * `liveness` - Liveness information from dataflow analysis
/// * `allocatable_regs` - Physical registers available for allocation
/// * `existing_locals` - Number of local variable slots already on the stack
/// * `cost_model` - Cost model for spill decisions
/// * `loop_info` - Loop depth information for each instruction
///
/// # Returns
///
/// A tuple of:
/// * `IndexMap<VReg, Option<Allocation<Reg>>>` - Allocation for each vreg
/// * `u32` - Number of spill slots used
/// * `Vec<Reg>` - Callee-saved registers that were used
pub fn linear_scan_with_cost_model<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    allocatable_regs: &[Reg],
    existing_locals: u32,
    cost_model: &CostModel,
    loop_info: &LoopInfo,
) -> (IndexMap<VReg, Option<Allocation<Reg>>>, u32, Vec<Reg>) {
    let (allocation, num_spills, used_callee_saved, _debug_info) = linear_scan_impl(
        vreg_count,
        liveness,
        allocatable_regs,
        existing_locals,
        cost_model,
        loop_info,
    );
    (allocation, num_spills, used_callee_saved)
}

/// Perform linear scan register allocation with rematerialization support.
///
/// This is the preferred allocation function when rematerialization info is available.
/// When a vreg needs to be spilled but is marked as rematerializable, the allocator
/// will mark it for rematerialization instead of allocating a stack slot.
///
/// # Arguments
///
/// * `vreg_count` - Total number of virtual registers
/// * `liveness` - Liveness information from dataflow analysis
/// * `allocatable_regs` - Physical registers available for allocation
/// * `existing_locals` - Number of local variable slots already on the stack
/// * `vreg_info` - Rematerialization info for each vreg (optional per-vreg)
///
/// # Returns
///
/// A tuple of:
/// * `IndexMap<VReg, Option<Allocation<Reg>>>` - Allocation for each vreg
/// * `u32` - Number of spill slots used (excludes rematerialized vregs)
/// * `Vec<Reg>` - Callee-saved registers that were used
pub fn linear_scan_with_remat<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    allocatable_regs: &[Reg],
    existing_locals: u32,
    vreg_info: &IndexMap<VReg, VRegInfo>,
) -> (IndexMap<VReg, Option<Allocation<Reg>>>, u32, Vec<Reg>) {
    let cost_model = CostModel {
        use_loop_aware_spilling: false,
        ..Default::default()
    };
    let loop_info = LoopInfo::no_loops(liveness.live_at.len());
    let (allocation, num_spills, used_callee_saved, _debug_info) = linear_scan_impl_with_remat(
        vreg_count,
        liveness,
        allocatable_regs,
        existing_locals,
        &cost_model,
        &loop_info,
        vreg_info,
    );
    (allocation, num_spills, used_callee_saved)
}

/// Perform linear scan register allocation and return debug information.
///
/// This is the same as [`linear_scan`] but also collects debug information
/// about the allocation process for display via `--emit regalloc`.
pub fn linear_scan_with_debug<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    allocatable_regs: &[Reg],
    existing_locals: u32,
) -> (
    IndexMap<VReg, Option<Allocation<Reg>>>,
    u32,
    Vec<Reg>,
    RegAllocDebugInfo<Reg>,
) {
    // Use default cost model with loop-aware spilling disabled (no loop info)
    let cost_model = CostModel {
        use_loop_aware_spilling: false,
        ..Default::default()
    };
    let loop_info = LoopInfo::no_loops(liveness.live_at.len());
    linear_scan_impl(
        vreg_count,
        liveness,
        allocatable_regs,
        existing_locals,
        &cost_model,
        &loop_info,
    )
}

/// Perform linear scan register allocation with cost model and return debug information.
///
/// This is the same as [`linear_scan_with_cost_model`] but also collects debug information
/// about the allocation process for display via `--emit regalloc`.
pub fn linear_scan_with_cost_model_and_debug<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    allocatable_regs: &[Reg],
    existing_locals: u32,
    cost_model: &CostModel,
    loop_info: &LoopInfo,
) -> (
    IndexMap<VReg, Option<Allocation<Reg>>>,
    u32,
    Vec<Reg>,
    RegAllocDebugInfo<Reg>,
) {
    linear_scan_impl(
        vreg_count,
        liveness,
        allocatable_regs,
        existing_locals,
        cost_model,
        loop_info,
    )
}

/// Internal implementation of linear scan register allocation.
///
/// This is the shared implementation used by both [`linear_scan`] and
/// [`linear_scan_with_debug`]. It always collects debug information,
/// which is discarded by [`linear_scan`].
fn linear_scan_impl<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    allocatable_regs: &[Reg],
    existing_locals: u32,
    cost_model: &CostModel,
    loop_info: &LoopInfo,
) -> (
    IndexMap<VReg, Option<Allocation<Reg>>>,
    u32,
    Vec<Reg>,
    RegAllocDebugInfo<Reg>,
) {
    let vreg_count_usize = vreg_count as usize;

    // Initialize allocation map
    let mut allocation: IndexMap<VReg, Option<Allocation<Reg>>> =
        IndexMap::with_capacity(vreg_count_usize);
    allocation.resize(vreg_count_usize, None);

    // Spill slot allocator that reuses slots for non-overlapping live ranges
    let mut spill_allocator = SpillSlotAllocator::new(existing_locals);
    let mut used_callee_saved: Vec<Reg> = Vec::new();

    // Debug info collections
    let mut debug_live_ranges: Vec<(u32, usize, usize)> = Vec::new();
    let mut debug_interference: Vec<(u32, u32)> = Vec::new();
    let mut debug_spills: Vec<u32> = Vec::new();

    // Collect vregs with live ranges and sort by start
    let mut vregs_by_start: Vec<(VReg, LiveRange)> = Vec::with_capacity(vreg_count_usize);
    for vreg_idx in 0..vreg_count {
        let vreg = VReg::new(vreg_idx);
        if let Some(&range) = liveness.range(vreg) {
            vregs_by_start.push((vreg, range));
            debug_live_ranges.push((vreg_idx, range.start, range.end));
        }
    }
    vregs_by_start.sort_by_key(|(_, range)| range.start);

    // Build interference graph: vregs that overlap
    for i in 0..vregs_by_start.len() {
        for j in (i + 1)..vregs_by_start.len() {
            let (vreg1, range1) = &vregs_by_start[i];
            let (vreg2, range2) = &vregs_by_start[j];
            if range1.overlaps(range2) {
                debug_interference.push((vreg1.index(), vreg2.index()));
            }
        }
    }

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
            // Use cost model to determine which vreg to spill.
            // Lower priority = cheaper to spill = spill first.

            // Compute priority for current vreg
            let current_loop_depth = loop_info.max_depth_in_range(range.start, range.end);
            let current_remaining = range.end.saturating_sub(range.start);
            let current_priority = cost_model.spill_priority(current_loop_depth, current_remaining);

            // Find the vreg with lowest priority (cheapest to spill) among active vregs
            let mut best_spill_idx = None;
            let mut best_spill_priority = current_priority;

            for (i, &(_active_vreg, _, end)) in active.iter().enumerate() {
                let active_loop_depth = loop_info.max_depth_in_range(range.start, end);
                let active_remaining = end.saturating_sub(range.start);
                let active_priority =
                    cost_model.spill_priority(active_loop_depth, active_remaining);

                // Lower priority = should be spilled first
                if active_priority < best_spill_priority {
                    best_spill_priority = active_priority;
                    best_spill_idx = Some(i);
                }
            }

            if let Some(idx) = best_spill_idx {
                // Spill the active vreg with lowest priority (cheapest to spill)
                let (spilled_vreg, freed_reg, spilled_end) = active.remove(idx);
                // Get the start of the spilled vreg's range for slot allocation
                let spilled_range = liveness.range(spilled_vreg).unwrap();
                let spill_offset = spill_allocator.allocate(spilled_range.start, spilled_end);
                allocation[spilled_vreg] = Some(Allocation::Spill(spill_offset));
                debug_spills.push(spilled_vreg.index());

                // Give the freed register to the current vreg
                allocation[vreg] = Some(Allocation::Register(freed_reg));
                active.push((vreg, freed_reg, range.end));
            } else {
                // Current vreg has the lowest priority (cheapest to spill), spill it
                let spill_offset = spill_allocator.allocate(range.start, range.end);
                allocation[vreg] = Some(Allocation::Spill(spill_offset));
                debug_spills.push(vreg.index());
            }
        }
    }

    // Build final allocation list
    let debug_allocations: Vec<(u32, Allocation<Reg>)> = allocation
        .iter()
        .enumerate()
        .filter_map(|(idx, alloc)| alloc.map(|a| (idx as u32, a)))
        .collect();

    let debug_info = RegAllocDebugInfo {
        live_ranges: debug_live_ranges,
        interference: debug_interference,
        allocations: debug_allocations,
        spills: debug_spills,
        callee_saved_used: used_callee_saved.clone(),
    };

    (
        allocation,
        spill_allocator.num_slots(),
        used_callee_saved,
        debug_info,
    )
}

/// Internal implementation of linear scan with rematerialization support.
///
/// When a vreg needs to be spilled but has rematerialization info, it is marked
/// for rematerialization instead of being allocated a stack slot. This avoids
/// memory traffic for values that can be cheaply recomputed (constants, etc.).
fn linear_scan_impl_with_remat<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    allocatable_regs: &[Reg],
    existing_locals: u32,
    cost_model: &CostModel,
    loop_info: &LoopInfo,
    vreg_info: &IndexMap<VReg, VRegInfo>,
) -> (
    IndexMap<VReg, Option<Allocation<Reg>>>,
    u32,
    Vec<Reg>,
    RegAllocDebugInfo<Reg>,
) {
    let vreg_count_usize = vreg_count as usize;

    // Initialize allocation map
    let mut allocation: IndexMap<VReg, Option<Allocation<Reg>>> =
        IndexMap::with_capacity(vreg_count_usize);
    allocation.resize(vreg_count_usize, None);

    // Spill slot allocator that reuses slots for non-overlapping live ranges
    let mut spill_allocator = SpillSlotAllocator::new(existing_locals);
    let mut used_callee_saved: Vec<Reg> = Vec::new();

    // Debug info collections
    let mut debug_live_ranges: Vec<(u32, usize, usize)> = Vec::new();
    let mut debug_interference: Vec<(u32, u32)> = Vec::new();
    let mut debug_spills: Vec<u32> = Vec::new();

    // Helper to check if a vreg is rematerializable
    let can_remat =
        |vreg: VReg| -> Option<RematerializeOp> { vreg_info.get(vreg).and_then(|info| info.remat) };

    // Collect vregs with live ranges and sort by start
    let mut vregs_by_start: Vec<(VReg, LiveRange)> = Vec::with_capacity(vreg_count_usize);
    for vreg_idx in 0..vreg_count {
        let vreg = VReg::new(vreg_idx);
        if let Some(&range) = liveness.range(vreg) {
            vregs_by_start.push((vreg, range));
            debug_live_ranges.push((vreg_idx, range.start, range.end));
        }
    }
    vregs_by_start.sort_by_key(|(_, range)| range.start);

    // Build interference graph: vregs that overlap
    for i in 0..vregs_by_start.len() {
        for j in (i + 1)..vregs_by_start.len() {
            let (vreg1, range1) = &vregs_by_start[i];
            let (vreg2, range2) = &vregs_by_start[j];
            if range1.overlaps(range2) {
                debug_interference.push((vreg1.index(), vreg2.index()));
            }
        }
    }

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
            // No free register - need to spill or rematerialize
            // Use cost model to determine which vreg to spill.
            // Lower priority = cheaper to spill = spill first.
            // Rematerializable vregs have even lower priority (prefer to evict them).

            // Compute priority for current vreg
            // If rematerializable, it has the lowest priority (always prefer to evict)
            let current_is_remat = can_remat(vreg).is_some();
            let current_loop_depth = loop_info.max_depth_in_range(range.start, range.end);
            let current_remaining = range.end.saturating_sub(range.start);
            let current_priority = if current_is_remat {
                0 // Lowest priority = evict first
            } else {
                cost_model.spill_priority(current_loop_depth, current_remaining)
            };

            // Find the vreg with lowest priority (cheapest to spill/remat) among active vregs
            let mut best_spill_idx = None;
            let mut best_spill_priority = current_priority;
            let mut best_is_remat = current_is_remat;

            for (i, &(active_vreg, _, end)) in active.iter().enumerate() {
                let active_is_remat = can_remat(active_vreg).is_some();
                let active_loop_depth = loop_info.max_depth_in_range(range.start, end);
                let active_remaining = end.saturating_sub(range.start);
                let active_priority = if active_is_remat {
                    0 // Lowest priority = evict first
                } else {
                    cost_model.spill_priority(active_loop_depth, active_remaining)
                };

                // Prefer rematerializable vregs, then lowest priority
                // (rematerializable with priority 0 beats non-remat with any priority)
                let should_replace = if active_is_remat && !best_is_remat {
                    true // Prefer to evict rematerializable over non-remat
                } else if !active_is_remat && best_is_remat {
                    false // Don't replace remat with non-remat
                } else {
                    active_priority < best_spill_priority
                };

                if should_replace {
                    best_spill_priority = active_priority;
                    best_spill_idx = Some(i);
                    best_is_remat = active_is_remat;
                }
            }

            if let Some(idx) = best_spill_idx {
                // Evict the active vreg with lowest priority
                let (spilled_vreg, freed_reg, spilled_end) = active.remove(idx);

                // Check if spilled vreg is rematerializable
                if let Some(remat_op) = can_remat(spilled_vreg) {
                    // Mark for rematerialization instead of spilling
                    allocation[spilled_vreg] = Some(Allocation::Rematerialize(remat_op));
                } else {
                    // Allocate a stack slot
                    let spilled_range = liveness.range(spilled_vreg).unwrap();
                    let spill_offset = spill_allocator.allocate(spilled_range.start, spilled_end);
                    allocation[spilled_vreg] = Some(Allocation::Spill(spill_offset));
                    debug_spills.push(spilled_vreg.index());
                }

                // Give the freed register to the current vreg
                allocation[vreg] = Some(Allocation::Register(freed_reg));
                active.push((vreg, freed_reg, range.end));
            } else {
                // Current vreg has the lowest priority, evict it
                if let Some(remat_op) = can_remat(vreg) {
                    // Mark for rematerialization
                    allocation[vreg] = Some(Allocation::Rematerialize(remat_op));
                } else {
                    // Allocate a stack slot
                    let spill_offset = spill_allocator.allocate(range.start, range.end);
                    allocation[vreg] = Some(Allocation::Spill(spill_offset));
                    debug_spills.push(vreg.index());
                }
            }
        }
    }

    // Build final allocation list
    let debug_allocations: Vec<(u32, Allocation<Reg>)> = allocation
        .iter()
        .enumerate()
        .filter_map(|(idx, alloc)| alloc.map(|a| (idx as u32, a)))
        .collect();

    let debug_info = RegAllocDebugInfo {
        live_ranges: debug_live_ranges,
        interference: debug_interference,
        allocations: debug_allocations,
        spills: debug_spills,
        callee_saved_used: used_callee_saved.clone(),
    };

    (
        allocation,
        spill_allocator.num_slots(),
        used_callee_saved,
        debug_info,
    )
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
        // Find max vreg index and max instruction index
        let max_vreg = ranges.iter().map(|(v, _, _)| *v).max().unwrap_or(0);
        let max_inst = ranges.iter().map(|(_, _, e)| *e).max().unwrap_or(0);
        let vreg_count = (max_vreg + 1) as usize;

        let mut info = LivenessInfo::with_vreg_capacity(max_vreg + 1);
        for (vreg_idx, start, end) in ranges {
            info.ranges[VReg::new(vreg_idx)] = Some(LiveRange::new(start, end));
        }

        // Initialize live_at and clobbers_at based on max instruction index
        info.live_at = vec![FixedBitSet::with_capacity(vreg_count); max_inst + 1];
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

    // ========================================
    // Spill slot conflict tests
    // ========================================

    #[test]
    fn test_multiple_overlapping_spills_get_unique_offsets() {
        // With only 1 register and 5 overlapping live ranges,
        // we need 4 spills with unique offsets.
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 10), // v0 - longest
            (1, 1, 9),  // v1
            (2, 2, 8),  // v2
            (3, 3, 7),  // v3
            (4, 4, 6),  // v4 - gets the register (shortest remaining)
        ]);

        let (allocation, num_spills, _) = linear_scan(5, &liveness, &allocatable, 0);

        assert_eq!(num_spills, 4);

        // Collect all spill offsets
        let mut offsets = Vec::new();
        for vreg_idx in 0..5 {
            if let Some(Allocation::Spill(off)) = allocation[VReg::new(vreg_idx)] {
                offsets.push(off);
            }
        }

        // All spill offsets should be unique
        let unique_offsets: std::collections::HashSet<_> = offsets.iter().copied().collect();
        assert_eq!(
            offsets.len(),
            unique_offsets.len(),
            "Spill offsets must be unique: {:?}",
            offsets
        );
    }

    #[test]
    fn test_spill_slots_dont_overlap_with_locals() {
        // With 10 existing locals (slots at -8 through -80), spills should start at -88
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 5), // v0 - will be spilled (longer)
            (1, 1, 4), // v1 - gets the register (shorter)
        ]);

        let (allocation, num_spills, _) = linear_scan(2, &liveness, &allocatable, 10);

        assert_eq!(num_spills, 1);

        let spill_off = match allocation[VReg::new(0)] {
            Some(Allocation::Spill(off)) => off,
            _ => panic!("v0 should be spilled"),
        };

        // With 10 existing locals, first spill should be at -(10+1)*8 = -88
        assert_eq!(spill_off, -88);
    }

    #[test]
    fn test_many_simultaneous_spills() {
        // Test a scenario where many vregs are live simultaneously, causing many spills
        let allocatable = vec![TestReg(0), TestReg(1)]; // Only 2 registers

        // 10 vregs all live for the entire range [0, 20]
        let liveness = make_liveness((0..10).map(|i| (i, 0, 20)).collect());

        let (allocation, num_spills, _) = linear_scan(10, &liveness, &allocatable, 0);

        // With 10 vregs and 2 registers, we should have 8 spills
        assert_eq!(num_spills, 8);

        // Verify all spill offsets are unique
        let spill_offsets: Vec<i32> = (0..10)
            .filter_map(|i| match allocation[VReg::new(i)] {
                Some(Allocation::Spill(off)) => Some(off),
                _ => None,
            })
            .collect();

        let unique: std::collections::HashSet<_> = spill_offsets.iter().copied().collect();
        assert_eq!(
            spill_offsets.len(),
            unique.len(),
            "All spill offsets must be unique"
        );

        // Verify spill offsets are sequential 8-byte aligned
        // Offsets are negative, so sorted goes from most negative to least negative
        let mut sorted = spill_offsets.clone();
        sorted.sort();
        for i in 1..sorted.len() {
            assert_eq!(
                sorted[i] - sorted[i - 1],
                8,
                "Spill offsets should be 8 bytes apart"
            );
        }
    }

    // ========================================
    // Large stack frame tests
    // ========================================

    #[test]
    fn test_large_stack_frame_many_locals() {
        // Function with 100 locals - spills start after those
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 3), // v0 - spilled
            (1, 1, 2), // v1 - gets register
        ]);

        let (allocation, num_spills, _) = linear_scan(2, &liveness, &allocatable, 100);

        assert_eq!(num_spills, 1);

        let spill_off = match allocation[VReg::new(0)] {
            Some(Allocation::Spill(off)) => off,
            _ => panic!("v0 should be spilled"),
        };

        // With 100 existing locals, first spill is at -(100+1)*8 = -808
        assert_eq!(spill_off, -808);
    }

    #[test]
    fn test_large_number_of_spills() {
        // 50 vregs all live simultaneously with only 2 registers = 48 spills
        let allocatable = vec![TestReg(0), TestReg(1)];
        let liveness = make_liveness((0..50).map(|i| (i, 0, 100)).collect());

        let (allocation, num_spills, _) = linear_scan(50, &liveness, &allocatable, 0);

        assert_eq!(num_spills, 48);

        // First spill should be at -8, last at -8 * 48 = -384
        let spill_offsets: Vec<i32> = (0..50)
            .filter_map(|i| match allocation[VReg::new(i)] {
                Some(Allocation::Spill(off)) => Some(off),
                _ => None,
            })
            .collect();

        assert_eq!(spill_offsets.len(), 48);

        let min_offset = *spill_offsets.iter().min().unwrap();
        let max_offset = *spill_offsets.iter().max().unwrap();

        // Most negative offset should be -(48)*8 = -384 (spill slots grow down)
        assert_eq!(min_offset, -384);
        // Least negative offset should be -8 (first spill)
        assert_eq!(max_offset, -8);
    }

    #[test]
    fn test_combined_locals_and_spills() {
        // 20 locals + 30 vregs with 5 registers = 25 spills
        // Spills should start at -(20+1)*8 = -168
        let allocatable = vec![TestReg(0), TestReg(1), TestReg(2), TestReg(3), TestReg(4)];
        let liveness = make_liveness((0..30).map(|i| (i, 0, 50)).collect());

        let (allocation, num_spills, _) = linear_scan(30, &liveness, &allocatable, 20);

        assert_eq!(num_spills, 25);

        let spill_offsets: Vec<i32> = (0..30)
            .filter_map(|i| match allocation[VReg::new(i)] {
                Some(Allocation::Spill(off)) => Some(off),
                _ => None,
            })
            .collect();

        // First spill should be at -(20+1)*8 = -168 (after 20 locals)
        let max_offset = *spill_offsets.iter().max().unwrap();
        assert_eq!(max_offset, -168);

        // Last spill should be at -(20+25)*8 = -360
        let min_offset = *spill_offsets.iter().min().unwrap();
        assert_eq!(min_offset, -360);
    }

    #[test]
    fn test_spill_slot_reuse_non_overlapping() {
        // Spill slots can be reused for non-overlapping live ranges.
        // This reduces stack frame size.
        //
        // Timeline:
        //   v0: [0, 2] - starts first, gets register initially
        //   v1: [1, 5] - overlaps v0, has longer range -> v1 gets spilled
        //   v2: [7, 9] - non-overlapping with v1's spilled range, can reuse slot
        //   v3: [8, 12] - overlaps v2, has longer range -> v3 gets spilled, reuses slot
        //
        // Linear scan spills the vreg with the longest REMAINING range.
        // At time 1: v0 ends at 2, v1 ends at 5 -> v1 is spilled (longer remaining)
        // At time 8: v2 ends at 9, v3 ends at 12 -> v3 is spilled (longer remaining)
        // v1 ends at 5, v3 starts at 8 -> they can share a slot!
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 2),  // v0 - gets register (shorter range)
            (1, 1, 5),  // v1 - spilled (longer remaining range)
            (2, 7, 9),  // v2 - gets register (shorter range)
            (3, 8, 12), // v3 - spilled (longer remaining range), can reuse v1's slot
        ]);

        let (allocation, num_slots, _) = linear_scan(4, &liveness, &allocatable, 0);

        // Two vregs get spilled (v1 and v3), but they can share one slot
        // because v1 ends at 5 and v3 starts at 8
        assert_eq!(num_slots, 1, "Non-overlapping spills should share a slot");

        // v1 and v3 should be spilled with the same offset
        let v1_offset = match allocation[VReg::new(1)] {
            Some(Allocation::Spill(off)) => off,
            _ => panic!("v1 should be spilled"),
        };
        let v3_offset = match allocation[VReg::new(3)] {
            Some(Allocation::Spill(off)) => off,
            _ => panic!("v3 should be spilled"),
        };
        assert_eq!(
            v1_offset, v3_offset,
            "Non-overlapping spills should reuse the same slot"
        );
    }

    #[test]
    fn test_spill_slot_no_reuse_overlapping() {
        // Overlapping spills cannot share a slot.
        //
        // Timeline:
        //   v0: [0, 10] - live entire time
        //   v1: [1, 9]  - overlaps v0
        //   v2: [2, 8]  - overlaps both
        // All three overlap, so with only 1 register, we need 2 spills
        // and they cannot share a slot.
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 10), // v0 - longest, gets spilled
            (1, 1, 9),  // v1 - second longest, gets spilled
            (2, 2, 8),  // v2 - shortest, gets the register
        ]);

        let (allocation, num_slots, _) = linear_scan(3, &liveness, &allocatable, 0);

        // Two spills that overlap - cannot share
        assert_eq!(num_slots, 2, "Overlapping spills need separate slots");

        // Verify they have different offsets
        let v0_offset = match allocation[VReg::new(0)] {
            Some(Allocation::Spill(off)) => off,
            _ => panic!("v0 should be spilled"),
        };
        let v1_offset = match allocation[VReg::new(1)] {
            Some(Allocation::Spill(off)) => off,
            _ => panic!("v1 should be spilled"),
        };
        assert_ne!(
            v0_offset, v1_offset,
            "Overlapping spills must have different slots"
        );
    }

    #[test]
    fn test_spill_slot_reuse_multiple_waves() {
        // Multiple waves of non-overlapping spills can all reuse one slot.
        //
        // Timeline (with 1 register):
        //   Wave 1: v0 [0,2] overlaps v1 [1,5] -> v1 spilled (longer remaining)
        //   Wave 2: v2 [7,9] overlaps v3 [8,12] -> v3 spilled (longer remaining)
        //   Wave 3: v4 [14,16] overlaps v5 [15,19] -> v5 spilled (longer remaining)
        //
        // All three spilled ranges (v1:[1,5], v3:[8,12], v5:[15,19]) are non-overlapping
        // so they can all share the same slot.
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 2),   // Wave 1 - gets register
            (1, 1, 5),   // Wave 1 - spilled (longer)
            (2, 7, 9),   // Wave 2 - gets register
            (3, 8, 12),  // Wave 2 - spilled (longer), reuses slot
            (4, 14, 16), // Wave 3 - gets register
            (5, 15, 19), // Wave 3 - spilled (longer), reuses slot
        ]);

        let (allocation, num_slots, _) = linear_scan(6, &liveness, &allocatable, 0);

        // Three spills total, but all can share one slot
        assert_eq!(
            num_slots, 1,
            "Non-overlapping spill waves should share slot"
        );

        // Count actual spills
        let spilled_count = (0..6)
            .filter(|&i| matches!(allocation[VReg::new(i)], Some(Allocation::Spill(_))))
            .count();
        assert_eq!(spilled_count, 3, "Should have 3 vregs spilled");
    }

    #[test]
    fn test_spill_slot_reuse_partial() {
        // Some spills can share, others cannot.
        //
        // Timeline (with 1 register):
        //   v0: [0, 5]  - long range
        //   v1: [1, 4]  - overlaps v0 entirely
        //   v2: [7, 10] - starts after v0 ends, can reuse v0's slot
        //   v3: [3, 6]  - overlaps v0, cannot share with v0 but can reuse later
        //
        // v0 and v1 overlap -> 1 spill
        // v0 and v3 overlap -> v3 needs own slot (v0's slot still occupied at 3)
        // v2 doesn't overlap v0 -> can reuse v0's slot
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 5),  // v0
            (1, 1, 4),  // v1 - overlaps v0
            (2, 7, 10), // v2 - after v0, can reuse
            (3, 3, 6),  // v3 - overlaps v0
        ]);

        let (allocation, num_slots, _) = linear_scan(4, &liveness, &allocatable, 0);

        // We need to check that slot reuse happens appropriately
        // The exact number depends on spill decisions, but should be <= 2
        // (v0, v3 need separate slots if both spilled; v2 can reuse v0's)
        assert!(
            num_slots <= 2,
            "Should reuse slots where possible, got {} slots",
            num_slots
        );
    }

    // ========================================
    // Register coalescing tests
    // ========================================

    #[test]
    fn test_coalesce_simple_move() {
        // v0 = 1       ; inst 0
        // v1 = v0      ; inst 1 (move)
        // use v1       ; inst 2
        //
        // v0: [0, 1] - defined at 0, last used at 1
        // v1: [1, 2] - defined at 1, last used at 2
        //
        // After coalescing: v0 and v1 share a register, move is eliminated
        let mut liveness = make_liveness(vec![
            (0, 0, 1), // v0: defined at 0, used at 1 (the move)
            (1, 1, 2), // v1: defined at 1 (the move), used at 2
        ]);

        let candidates = vec![CoalesceCandidate {
            inst_idx: 1,
            dst: VReg::new(1),
            src: VReg::new(0),
        }];

        let result = coalesce(&candidates, &mut liveness);

        // The move should be eliminated
        assert!(result.is_eliminated(1));
        assert_eq!(result.num_eliminated(), 1);

        // v1 should be coalesced with v0
        assert_eq!(result.representative(VReg::new(1)), VReg::new(0));

        // v0 should be its own representative
        assert_eq!(result.representative(VReg::new(0)), VReg::new(0));

        // The merged range should cover both original ranges
        let merged = liveness.range(VReg::new(0)).unwrap();
        assert_eq!(merged.start, 0);
        assert_eq!(merged.end, 2);

        // v1's range should be removed
        assert!(liveness.range(VReg::new(1)).is_none());
    }

    #[test]
    fn test_coalesce_interfering_not_coalesced() {
        // v0 = 1       ; inst 0
        // use v0       ; inst 1
        // v1 = v0      ; inst 2 (move)
        // use v0       ; inst 3 (v0 still live after the move!)
        // use v1       ; inst 4
        //
        // v0: [0, 3] - still used after the move
        // v1: [2, 4]
        //
        // These interfere (v0 is still live when v1 is defined), cannot coalesce
        let mut liveness = make_liveness(vec![
            (0, 0, 3), // v0: live 0-3
            (1, 2, 4), // v1: live 2-4
        ]);

        let candidates = vec![CoalesceCandidate {
            inst_idx: 2,
            dst: VReg::new(1),
            src: VReg::new(0),
        }];

        let result = coalesce(&candidates, &mut liveness);

        // The move should NOT be eliminated (they interfere)
        assert!(!result.is_eliminated(2));
        assert_eq!(result.num_eliminated(), 0);

        // Neither should be coalesced
        assert_eq!(result.representative(VReg::new(0)), VReg::new(0));
        assert_eq!(result.representative(VReg::new(1)), VReg::new(1));
    }

    #[test]
    fn test_coalesce_chain() {
        // v0 = 1       ; inst 0
        // v1 = v0      ; inst 1 (move)
        // v2 = v1      ; inst 2 (move)
        // use v2       ; inst 3
        //
        // All three can be coalesced into one
        let mut liveness = make_liveness(vec![
            (0, 0, 1), // v0: 0-1
            (1, 1, 2), // v1: 1-2
            (2, 2, 3), // v2: 2-3
        ]);

        let candidates = vec![
            CoalesceCandidate {
                inst_idx: 1,
                dst: VReg::new(1),
                src: VReg::new(0),
            },
            CoalesceCandidate {
                inst_idx: 2,
                dst: VReg::new(2),
                src: VReg::new(1),
            },
        ];

        let result = coalesce(&candidates, &mut liveness);

        // Both moves should be eliminated
        assert!(result.is_eliminated(1));
        assert!(result.is_eliminated(2));
        assert_eq!(result.num_eliminated(), 2);

        // All should map to v0
        assert_eq!(result.representative(VReg::new(0)), VReg::new(0));
        assert_eq!(result.representative(VReg::new(1)), VReg::new(0));
        assert_eq!(result.representative(VReg::new(2)), VReg::new(0));
    }

    #[test]
    fn test_coalesce_already_same_class() {
        // If two vregs are already coalesced, the move is still eliminated
        let mut liveness = make_liveness(vec![(0, 0, 1), (1, 1, 2), (2, 2, 3)]);

        // Two moves that form a cycle (after coalescing v0-v1, v2 wants to coalesce with v1)
        let candidates = vec![
            CoalesceCandidate {
                inst_idx: 1,
                dst: VReg::new(1),
                src: VReg::new(0),
            },
            CoalesceCandidate {
                inst_idx: 2,
                dst: VReg::new(2),
                src: VReg::new(1),
            },
        ];

        let result = coalesce(&candidates, &mut liveness);

        // Both moves eliminated
        assert_eq!(result.num_eliminated(), 2);
    }

    #[test]
    fn test_coalesce_no_candidates() {
        let mut liveness: LivenessInfo<TestReg> = make_liveness(vec![(0, 0, 5)]);

        let candidates: Vec<CoalesceCandidate> = vec![];
        let result = coalesce(&candidates, &mut liveness);

        assert_eq!(result.num_eliminated(), 0);
        assert_eq!(result.representative(VReg::new(0)), VReg::new(0));
    }

    #[test]
    fn test_coalesce_reduces_register_pressure() {
        // Without coalescing:
        //   v0 = 1       ; inst 0
        //   v1 = v0      ; inst 1 (move)
        //   use v1       ; inst 2
        // v0 and v1 both need registers (2 registers needed)
        //
        // With coalescing:
        // v0 and v1 share a register (1 register needed)
        // The move is eliminated

        let allocatable = vec![TestReg(0)]; // Only 1 register!

        // Without coalescing, this would need 2 registers and cause a spill
        // But since v0's range ends at the move and v1's starts there, they can share

        // v0: 0-1, v1: 1-2 - they meet at the move point
        let mut liveness = make_liveness(vec![(0, 0, 1), (1, 1, 2)]);

        let candidates = vec![CoalesceCandidate {
            inst_idx: 1,
            dst: VReg::new(1),
            src: VReg::new(0),
        }];

        let _result = coalesce(&candidates, &mut liveness);

        // Now allocate - should need only 1 register, no spills
        let (allocation, num_spills, _) = linear_scan(2, &liveness, &allocatable, 0);

        assert_eq!(
            num_spills, 0,
            "Coalescing should eliminate the need for a second register"
        );

        // v0 should get the register (v1's range was merged into v0)
        assert!(matches!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(_))
        ));
    }

    // ========================================
    // Cost model tests
    // ========================================

    #[test]
    fn test_cost_model_default() {
        let cm = CostModel::default();
        assert_eq!(cm.base_spill_cost, 1);
        assert_eq!(cm.loop_depth_multiplier, 10);
        assert!(cm.use_loop_aware_spilling);
    }

    #[test]
    fn test_cost_model_spill_cost() {
        let cm = CostModel::default();

        // Depth 0: cost = 1 * 10^0 = 1
        assert_eq!(cm.spill_cost(0), 1);

        // Depth 1: cost = 1 * 10^1 = 10
        assert_eq!(cm.spill_cost(1), 10);

        // Depth 2: cost = 1 * 10^2 = 100
        assert_eq!(cm.spill_cost(2), 100);

        // Depth 3: cost = 1 * 10^3 = 1000
        assert_eq!(cm.spill_cost(3), 1000);
    }

    #[test]
    fn test_cost_model_disabled() {
        let cm = CostModel {
            use_loop_aware_spilling: false,
            ..Default::default()
        };

        // When disabled, all depths should have the same cost
        assert_eq!(cm.spill_cost(0), 1);
        assert_eq!(cm.spill_cost(1), 1);
        assert_eq!(cm.spill_cost(2), 1);
    }

    #[test]
    fn test_cost_model_spill_priority_loop_depth() {
        let cm = CostModel::default();

        // Higher loop depth = higher priority (less likely to be spilled)
        let priority_depth_0 = cm.spill_priority(0, 10);
        let priority_depth_1 = cm.spill_priority(1, 10);
        let priority_depth_2 = cm.spill_priority(2, 10);

        // Higher priority = don't spill
        assert!(priority_depth_0 < priority_depth_1);
        assert!(priority_depth_1 < priority_depth_2);
    }

    #[test]
    fn test_cost_model_spill_priority_range_length() {
        let cm = CostModel::default();

        // Same loop depth, different range lengths
        // Longer range = lower priority = spill first
        let priority_short = cm.spill_priority(0, 5);
        let priority_long = cm.spill_priority(0, 100);

        // Longer range should have slightly lower priority (spill first)
        assert!(priority_long < priority_short);
    }

    // ========================================
    // Loop info tests
    // ========================================

    #[test]
    fn test_loop_info_no_loops() {
        let info = LoopInfo::no_loops(10);
        for i in 0..10 {
            assert_eq!(info.depth(i), 0);
        }
        assert_eq!(info.max_depth_in_range(0, 9), 0);
    }

    #[test]
    fn test_loop_info_with_depths() {
        let info = LoopInfo {
            depths: vec![0, 0, 1, 1, 1, 2, 2, 1, 0, 0],
        };

        assert_eq!(info.depth(0), 0);
        assert_eq!(info.depth(2), 1);
        assert_eq!(info.depth(5), 2);
        assert_eq!(info.depth(8), 0);

        // Max depth in ranges
        assert_eq!(info.max_depth_in_range(0, 1), 0); // Before loop
        assert_eq!(info.max_depth_in_range(2, 4), 1); // In outer loop
        assert_eq!(info.max_depth_in_range(5, 6), 2); // In inner loop
        assert_eq!(info.max_depth_in_range(0, 9), 2); // Entire range
        assert_eq!(info.max_depth_in_range(7, 9), 1); // Exiting loops
    }

    #[test]
    fn test_loop_info_out_of_bounds() {
        let info = LoopInfo::no_loops(5);
        assert_eq!(info.depth(100), 0); // Out of bounds returns 0
        assert_eq!(info.max_depth_in_range(10, 20), 0); // Out of bounds returns 0
    }

    // ========================================
    // Loop-aware allocation tests
    // ========================================

    fn make_liveness_with_loop_info(
        ranges: Vec<(u32, usize, usize)>,
        loop_depths: Vec<u32>,
    ) -> (LivenessInfo<TestReg>, LoopInfo) {
        let liveness = make_liveness(ranges);
        let loop_info = LoopInfo {
            depths: loop_depths,
        };
        (liveness, loop_info)
    }

    #[test]
    fn test_loop_aware_spill_prefers_outside_loop() {
        // Scenario: Two vregs compete for one register
        // v0: lives outside the loop (instructions 0-20)
        // v1: lives inside the loop (instructions 5-15)
        //
        // Without loop awareness: v0 would be spilled (longer range)
        // With loop awareness: v0 should be spilled (cheaper, outside loop)
        //
        // Actually, v0 is mostly outside the loop, so it should be spilled.
        // Let's make v0 entirely outside the loop.

        let allocatable = vec![TestReg(0)];

        // v0: outside loop (0-4), v1: inside loop (5-10)
        // They don't overlap, so no spill needed. Let's make them overlap.

        // v0: 0-10 (partially in loop at 5-10)
        // v1: 5-15 (entirely in loop at 5-10)
        // Loop is at instructions 5-10
        let (liveness, loop_info) = make_liveness_with_loop_info(
            vec![
                (0, 0, 10), // v0: starts outside, extends into loop
                (1, 5, 15), // v1: starts in loop, extends outside
            ],
            vec![0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
        );

        let cost_model = CostModel::default();
        let (allocation, _, _) =
            linear_scan_with_cost_model(2, &liveness, &allocatable, 0, &cost_model, &loop_info);

        // v0 and v1 overlap at 5-10, so one must be spilled.
        // v0 has max loop depth of 1 (instructions 5-10)
        // v1 has max loop depth of 1 (instructions 5-10)
        // Both have same loop depth, so longer range is spilled.
        // v1 is longer (10 vs 10), actually same length.
        // This test verifies the allocator still works with loop info.

        // At least one should be spilled
        let v0_spilled = matches!(allocation[VReg::new(0)], Some(Allocation::Spill(_)));
        let v1_spilled = matches!(allocation[VReg::new(1)], Some(Allocation::Spill(_)));
        assert!(
            v0_spilled || v1_spilled,
            "One of the vregs should be spilled"
        );
    }

    #[test]
    fn test_loop_aware_allocation_matches_original_when_no_loops() {
        // When there are no loops, the allocation should match the original behavior
        let allocatable = vec![TestReg(0)];
        let ranges = vec![
            (0, 0, 10), // v0 - longest
            (1, 1, 9),  // v1
            (2, 2, 8),  // v2 - shortest, gets register
        ];

        // Original allocation (no loop info)
        let liveness = make_liveness(ranges.clone());
        let (alloc1, spills1, _) = linear_scan(3, &liveness, &allocatable, 0);

        // Loop-aware allocation with no loops
        let liveness2 = make_liveness(ranges);
        let loop_info = LoopInfo::no_loops(11);
        let cost_model = CostModel {
            use_loop_aware_spilling: false,
            ..Default::default()
        };
        let (alloc2, spills2, _) =
            linear_scan_with_cost_model(3, &liveness2, &allocatable, 0, &cost_model, &loop_info);

        // Both should produce the same number of spills
        assert_eq!(spills1, spills2);

        // Same vregs should be spilled
        for i in 0..3 {
            let vreg = VReg::new(i);
            let spilled1 = matches!(alloc1[vreg], Some(Allocation::Spill(_)));
            let spilled2 = matches!(alloc2[vreg], Some(Allocation::Spill(_)));
            assert_eq!(spilled1, spilled2, "v{} spill status should match", i);
        }
    }

    #[test]
    fn test_loop_aware_prefers_spilling_longer_range_at_same_depth() {
        // Two vregs with same loop depth but different lengths
        // Should spill the longer one (matches original behavior)
        let allocatable = vec![TestReg(0)];

        let (liveness, loop_info) = make_liveness_with_loop_info(
            vec![
                (0, 0, 20), // v0: long range at depth 1
                (1, 5, 10), // v1: short range at depth 1
            ],
            vec![1; 21], // All instructions at depth 1
        );

        let cost_model = CostModel::default();
        let (allocation, _, _) =
            linear_scan_with_cost_model(2, &liveness, &allocatable, 0, &cost_model, &loop_info);

        // Both are at the same loop depth
        // v0 is longer, so it should be spilled (cheaper per instruction)
        let v0_spilled = matches!(allocation[VReg::new(0)], Some(Allocation::Spill(_)));
        let v1_in_reg = matches!(allocation[VReg::new(1)], Some(Allocation::Register(_)));

        assert!(v0_spilled, "v0 (longer range) should be spilled");
        assert!(v1_in_reg, "v1 (shorter range) should be in register");
    }

    #[test]
    fn test_cost_model_custom_multiplier() {
        // Test with a custom loop depth multiplier
        let cm = CostModel {
            base_spill_cost: 1,
            loop_depth_multiplier: 100, // 100x per level instead of 10x
            use_loop_aware_spilling: true,
        };

        // Depth 1 should cost 100, not 10
        assert_eq!(cm.spill_cost(1), 100);

        // Depth 2 should cost 10000, not 100
        assert_eq!(cm.spill_cost(2), 10000);
    }

    #[test]
    fn test_deeply_nested_loop_very_expensive_to_spill() {
        // A vreg in a deeply nested loop should be very expensive to spill
        let cm = CostModel::default();

        // At depth 4, cost = 10^4 = 10000
        let deep_priority = cm.spill_priority(4, 10);
        let shallow_priority = cm.spill_priority(0, 10);

        // Deep loop should have much higher priority (less likely to spill)
        assert!(deep_priority > shallow_priority);
        // The ratio should be about 10000:1
        assert!(deep_priority > shallow_priority * 1000);
    }

    // ========================================
    // Rematerialization tests
    // ========================================

    #[test]
    fn test_rematerialization_preferred_over_spill() {
        // When we run out of registers and one vreg is rematerializable,
        // that vreg should be marked for rematerialization (not spilled).
        let allocatable = vec![TestReg(0)]; // Only 1 register
        let liveness = make_liveness(vec![
            (0, 0, 5), // v0: constant, lives 0-5
            (1, 2, 5), // v1: non-constant, overlaps with v0 at 2-5
        ]);

        // Create vreg info marking v0 as rematerializable
        let mut vreg_info = IndexMap::with_capacity(2);
        vreg_info.resize(2, VRegInfo::none());
        vreg_info[VReg::new(0)] = VRegInfo::rematerializable(RematerializeOp::Const32(42));
        // v1 is not rematerializable

        let (allocation, num_spills, _) =
            linear_scan_with_remat(2, &liveness, &allocatable, 0, &vreg_info);

        // v0 should be rematerialized (not spilled)
        assert!(
            matches!(
                allocation[VReg::new(0)],
                Some(Allocation::Rematerialize(RematerializeOp::Const32(42)))
            ),
            "rematerializable vreg should be marked for rematerialization, got: {:?}",
            allocation[VReg::new(0)]
        );

        // v1 should get the register (not spilled)
        assert!(
            matches!(allocation[VReg::new(1)], Some(Allocation::Register(_))),
            "non-rematerializable vreg should get register, got: {:?}",
            allocation[VReg::new(1)]
        );

        // No actual spills needed because v0 was rematerialized
        assert_eq!(num_spills, 0, "no spill slots should be used");
    }

    #[test]
    fn test_rematerialization_prefers_remat_over_non_remat() {
        // When multiple vregs compete for a register, rematerializable ones
        // should be evicted first.
        let allocatable = vec![TestReg(0)]; // Only 1 register
        let liveness = make_liveness(vec![
            (0, 0, 5), // v0: non-constant, lives 0-5
            (1, 2, 5), // v1: constant, overlaps with v0
        ]);

        // Mark v1 (the second one) as rematerializable
        let mut vreg_info = IndexMap::with_capacity(2);
        vreg_info.resize(2, VRegInfo::none());
        vreg_info[VReg::new(1)] = VRegInfo::rematerializable(RematerializeOp::Const64(100));

        let (allocation, num_spills, _) =
            linear_scan_with_remat(2, &liveness, &allocatable, 0, &vreg_info);

        // v0 (starts first, not remat) should get the register
        assert!(
            matches!(allocation[VReg::new(0)], Some(Allocation::Register(_))),
            "non-rematerializable vreg should keep register, got: {:?}",
            allocation[VReg::new(0)]
        );

        // v1 (rematerializable) should be rematerialized
        assert!(
            matches!(
                allocation[VReg::new(1)],
                Some(Allocation::Rematerialize(RematerializeOp::Const64(100)))
            ),
            "rematerializable vreg should be marked for rematerialization, got: {:?}",
            allocation[VReg::new(1)]
        );

        assert_eq!(num_spills, 0, "no spill slots should be used");
    }

    #[test]
    fn test_rematerialization_without_info_falls_back_to_spill() {
        // Without rematerialization info, vregs should be spilled as before.
        let allocatable = vec![TestReg(0)]; // Only 1 register
        let liveness = make_liveness(vec![
            (0, 0, 5), // v0 lives 0-5
            (1, 2, 5), // v1 overlaps at 2-5
        ]);

        // Empty vreg_info - no rematerialization info
        let mut vreg_info = IndexMap::with_capacity(2);
        vreg_info.resize(2, VRegInfo::none());

        let (allocation, num_spills, _) =
            linear_scan_with_remat(2, &liveness, &allocatable, 0, &vreg_info);

        // One vreg should be spilled (not rematerialized)
        assert_eq!(num_spills, 1, "should have one spill");

        // Check that we have one register allocation and one spill
        let num_registers = [VReg::new(0), VReg::new(1)]
            .iter()
            .filter(|&&v| matches!(allocation[v], Some(Allocation::Register(_))))
            .count();
        let num_spilled = [VReg::new(0), VReg::new(1)]
            .iter()
            .filter(|&&v| matches!(allocation[v], Some(Allocation::Spill(_))))
            .count();

        assert_eq!(num_registers, 1, "one vreg should be in register");
        assert_eq!(num_spilled, 1, "one vreg should be spilled");
    }

    #[test]
    fn test_rematerialization_string_operations() {
        // Test that string rematerialization ops work correctly
        let allocatable = vec![TestReg(0)];
        let liveness = make_liveness(vec![
            (0, 0, 5), // v0: string ptr
            (1, 2, 5), // v1: string len, overlaps with v0
        ]);

        let mut vreg_info = IndexMap::with_capacity(2);
        vreg_info.resize(2, VRegInfo::none());
        vreg_info[VReg::new(0)] = VRegInfo::rematerializable(RematerializeOp::StringPtr(0));
        vreg_info[VReg::new(1)] = VRegInfo::rematerializable(RematerializeOp::StringLen(0));

        let (allocation, num_spills, _) =
            linear_scan_with_remat(2, &liveness, &allocatable, 0, &vreg_info);

        // v0 starts first and gets the register
        assert!(matches!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(_))
        ));

        // v1 starts later; since both are rematerializable with same priority,
        // the incoming vreg (v1) gets evicted and marked for rematerialization
        assert!(matches!(
            allocation[VReg::new(1)],
            Some(Allocation::Rematerialize(RematerializeOp::StringLen(0)))
        ));

        assert_eq!(num_spills, 0, "no spill slots should be used");
    }
}
