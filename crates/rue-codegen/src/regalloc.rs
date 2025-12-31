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
//! 5. If no register is available, spill the longest-range vreg to stack
//!
//! ## Spilling
//!
//! When register pressure exceeds available registers, values are spilled
//! to the stack. The allocator uses a heuristic that spills the vreg with
//! the longest remaining live range, as this frees up a register for the
//! longest time.

use std::collections::{HashMap, HashSet};
use std::fmt;

use fixedbitset::FixedBitSet;

use crate::index_map::IndexMap;
use crate::vreg::VReg;

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
            None => {
                let $pass_dst = $pass_dst;
                $emit_pass
            }
        }
    };

    // Form 2: Common case - same emit, different operand
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
            None => {
                let $op = $dst;
                $emit
            }
        }
    };
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
    let (allocation, num_spills, used_callee_saved, _debug_info) =
        linear_scan_impl(vreg_count, liveness, allocatable_regs, existing_locals);
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
    linear_scan_impl(vreg_count, liveness, allocatable_regs, existing_locals)
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
                // Current vreg has the longest range, spill it
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
}
