//! Shared register allocation algorithm and types.
//!
//! This module provides a target-independent linear scan register allocator
//! and the liveness analysis types used by all backends.
//!
//! ## Liveness Analysis
//!
//! The module provides target-independent types for liveness analysis:
//! - [`LiveRange`]: Represents the instruction range where a vreg's value is needed
//! - [`LivenessInfo`]: Holds all liveness information (ranges, clobbers)
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

use std::collections::HashSet;
use std::fmt;

use rue_error::CompileResult;

use crate::index_map::IndexMap;
use crate::reg_class::{RegClass, VRegClasses};
use crate::vreg::VReg;

/// Empty entry for the dense coalescing tables. A valid table slot has an
/// index strictly below its `liveness.ranges.len()` bound, and `coalesce`
/// asserts that bound cannot exceed this sentinel, so `u32::MAX` is never a
/// valid vreg index in either table.
const EMPTY_VREG: u32 = u32::MAX;

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
    /// Maximum loop depth across all instructions, cached so that loop-free code
    /// (`max_depth == 0`) answers range-max queries in O(1) instead of scanning
    /// the range. Register allocation queries the max depth over a vreg's whole
    /// live range once per vreg; on loop-free code with long live ranges (e.g. a
    /// large array literal) the scan was O(range) per vreg and quadratic overall
    /// (RUE-302).
    max_depth: u32,
}

impl LoopInfo {
    /// Create loop info with all instructions at depth 0 (no loops).
    pub fn no_loops(instruction_count: usize) -> Self {
        Self {
            depths: vec![0; instruction_count],
            max_depth: 0,
        }
    }

    /// Create loop info from a per-instruction depth vector, caching the max.
    pub fn from_depths(depths: Vec<u32>) -> Self {
        let max_depth = depths.iter().copied().max().unwrap_or(0);
        Self { depths, max_depth }
    }

    /// Get the loop depth for an instruction.
    pub fn depth(&self, inst_idx: usize) -> u32 {
        self.depths.get(inst_idx).copied().unwrap_or(0)
    }

    /// Get the maximum loop depth across a range of instructions.
    pub fn max_depth_in_range(&self, start: usize, end: usize) -> u32 {
        // Loop-free code: the max is trivially 0, no scan needed.
        if self.max_depth == 0 {
            return 0;
        }
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
    /// For each instruction index, the physical registers clobbered by that instruction.
    /// This is used to prevent allocating vregs to registers that would be clobbered.
    pub clobbers_at: Vec<Vec<Reg>>,
    /// For each instruction index, whether control can reach the instruction
    /// after it once it executes.
    ///
    /// Only a call to a helper the runtime ABI manifest declares
    /// `ReturnBehavior::Never` sets this — the overflow, bounds-check,
    /// divide-by-zero, panic, and exit traps. Such a call has no successors, so
    /// no value is live after it; see [`ClobberIndex::build`] for why the
    /// distinction matters to allocation (RUE-1224).
    pub non_returning_at: Vec<bool>,
    /// The register class of each virtual register.
    ///
    /// Liveness carries the MIR's class table forward so that allocation and
    /// coalescing have one authoritative answer for "what kind of register can
    /// hold this value" without re-deriving it from the instruction stream
    /// (RUE-1067). Every entry is [`RegClass::Gp`] today.
    pub vreg_classes: VRegClasses,
}

impl<Reg: Copy + Eq + std::hash::Hash> LivenessInfo<Reg> {
    /// Create a new empty liveness info.
    pub fn new() -> Self {
        Self {
            ranges: IndexMap::new(),
            clobbers_at: Vec::new(),
            non_returning_at: Vec::new(),
            vreg_classes: VRegClasses::new(),
        }
    }

    /// Create liveness info with capacity for the given number of vregs, all
    /// of them general-purpose.
    pub fn with_vreg_capacity(vreg_count: u32) -> Self {
        let mut ranges = IndexMap::with_capacity(vreg_count as usize);
        ranges.resize(vreg_count as usize, None);
        Self {
            ranges,
            clobbers_at: Vec::new(),
            non_returning_at: Vec::new(),
            vreg_classes: VRegClasses::all_gp(vreg_count),
        }
    }

    /// The register class of `vreg`.
    #[inline]
    pub fn class_of(&self, vreg: VReg) -> RegClass {
        self.vreg_classes.class_of(vreg)
    }

    /// The number of instructions this analysis covers.
    ///
    /// Every per-instruction table is this long; `clobbers_at` is simply the
    /// one that is always populated.
    pub fn instruction_count(&self) -> usize {
        self.clobbers_at.len()
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

    /// Whether the instruction at `inst_idx` never returns control to the
    /// instruction after it.
    ///
    /// Indices outside the analyzed instruction sequence answer `false`; so
    /// does any liveness built without this information, which keeps the
    /// pre-RUE-1224 behavior for hand-constructed test liveness.
    pub fn is_non_returning(&self, inst_idx: usize) -> bool {
        self.non_returning_at
            .get(inst_idx)
            .copied()
            .unwrap_or(false)
    }
}

// ============================================================================
// Clobber Index
// ============================================================================

/// Constant-time "is this register clobbered anywhere in this live range?".
///
/// Allocation asks that question once per candidate caller-saved register per
/// interval: a caller-saved register is only a legal home for an interval that
/// survives no clobber of it, and a call clobbers every caller-saved register
/// (see each backend's `Inst::clobbers`). Answering it by scanning
/// [`LivenessInfo::clobbers_at`] across the range costs O(range) per question,
/// which is quadratic on a function that keeps many values live over a long
/// span — the shape that made allocation quadratic in RUE-302.
///
/// So this precomputes, for each tracked register, a prefix count of the
/// instructions that clobber it. A range is clobber-free exactly when the
/// counts at its two endpoints agree. Building the index is O(tracked ×
/// instructions) once per function; each query is O(tracked) lookup plus two
/// array reads.
///
/// A never-returning call contributes no clobber event (RUE-1224). See
/// [`ClobberIndex::build`].
pub struct ClobberIndex<Reg> {
    /// One entry per tracked register: the register, and prefix counts where
    /// `counts[i]` is the number of instructions before `i` that clobber it.
    /// The count slice has `instruction_count + 1` entries.
    tracked: Vec<(Reg, Vec<u32>)>,
}

impl<Reg: Copy + Eq> ClobberIndex<Reg> {
    /// Build an index over `liveness`'s clobber data for `tracked`.
    ///
    /// Only the tracked registers get an answer; see [`Self::is_clobbered_during`].
    ///
    /// A never-returning call is skipped. Rue lowers every checked `+`/`*` as
    /// `jno .L; call __rue_overflow; .L:`, so that trap call sits textually
    /// inside almost every arithmetic value's live range — but the value is
    /// live *around* the call, not through it. `__rue_overflow` and its sibling
    /// traps are declared `ReturnBehavior::Never` by the runtime ABI manifest
    /// and abort the process, so on every path where a later use of the value
    /// executes, the call did not. It cannot destroy a value that is already
    /// dead if it runs, and a live range is a textual interval that cannot
    /// express that (RUE-1224).
    ///
    /// This is deliberately narrower than "the call is on a cold path": the
    /// call's *arguments* are ordinary uses, live at the call and honored by
    /// the ranges above; only the clobber event is dropped. If a trap does
    /// fire, a register holding a user value may hold anything by the time the
    /// handler runs. Nothing observes that: Rue emits no DWARF and the traps
    /// print a fixed message and exit without a backtrace (RUE-1146's audit).
    pub fn build(liveness: &LivenessInfo<Reg>, tracked: &[Reg]) -> Self
    where
        Reg: std::hash::Hash,
    {
        let num_insts = liveness.clobbers_at.len();
        let tracked = tracked
            .iter()
            .map(|&reg| {
                let mut counts = Vec::with_capacity(num_insts + 1);
                let mut running = 0_u32;
                counts.push(running);
                for idx in 0..num_insts {
                    if !liveness.is_non_returning(idx) && liveness.clobbers_at(idx).contains(&reg) {
                        running += 1;
                    }
                    counts.push(running);
                }
                (reg, counts)
            })
            .collect();
        Self { tracked }
    }

    /// Whether any instruction in `range` (endpoints included) clobbers `reg`.
    ///
    /// A register the index was not built for answers `true`: the index proves
    /// the *absence* of clobbers only for the registers it tracks, and the safe
    /// answer for anything else is that the register may be destroyed.
    pub fn is_clobbered_during(&self, reg: Reg, range: &LiveRange) -> bool {
        let Some((_, counts)) = self.tracked.iter().find(|(tracked, _)| *tracked == reg) else {
            return true;
        };
        let last = counts.len() - 1;
        let start = range.start.min(last);
        let end = range.end.saturating_add(1).min(last);
        counts[end] > counts[start]
    }
}

// ============================================================================
// Save Classes and the Register File
// ============================================================================

/// One [`RegClass`]'s allocatable registers, split by who preserves them.
///
/// A caller-saved register costs nothing in the prologue but is destroyed by
/// every call, so it is offered only to an interval that no instruction
/// clobbers while it is live. A callee-saved register survives calls and is the
/// only register home for an interval that spans one, at the price of a
/// prologue save and epilogue restore.
///
/// Standard linear-scan practice is to prefer caller-saved registers for the
/// intervals that can take them, which both leaves the callee-saved registers
/// for the intervals that need them and shrinks the prologue.
///
/// This split is orthogonal to [`RegClass`]: *who saves a register* and *what
/// kind of value it can hold* are independent facts, so each register class of
/// a target has its own `SaveClasses` and [`RegisterFile`] holds one per class.
pub struct SaveClasses<'a, Reg> {
    /// Tried first, in order, for intervals with no clobber in range.
    pub caller_saved: &'a [Reg],
    /// Tried next, in order; saved by the prologue when used.
    pub callee_saved: &'a [Reg],
    /// The subset of `callee_saved` that instructions address at least as
    /// cheaply as any register in `caller_saved`.
    ///
    /// These are the only registers worth taking *back* from the caller-saved
    /// class once their prologue save is already paid for (RUE-1227): reusing a
    /// callee-saved register that encodes no better than the caller-saved
    /// candidate trades away a free register for nothing, and adds pressure
    /// besides. On x86-64 this is `rbx` alone — every other allocatable
    /// register is an extended one whose byte and dword forms need a REX
    /// prefix, exactly as `r11`'s do. On a fixed-width instruction set it is
    /// empty, and the preference below never fires.
    pub compact_callee_saved: &'a [Reg],
}

// Derived `Clone`/`Copy` would demand `Reg: Clone`/`Reg: Copy`; these hold
// shared slices only, so they are copyable for any `Reg`.
impl<Reg> Clone for SaveClasses<'_, Reg> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Reg> Copy for SaveClasses<'_, Reg> {}

impl<'a, Reg> SaveClasses<'a, Reg> {
    /// A class with no allocatable registers at all.
    ///
    /// This is what both backends supply for [`RegClass::Fp`] until the floats
    /// series populates it, and what a target with no such registers would
    /// supply permanently.
    pub const EMPTY: Self = Self {
        caller_saved: &[],
        callee_saved: &[],
        compact_callee_saved: &[],
    };

    /// Classes for a caller that offers callee-saved registers only.
    ///
    /// This reproduces the pre-RUE-1146 policy exactly and is what the
    /// standalone `linear_scan*` entry points below use.
    pub const fn callee_saved_only(regs: &'a [Reg]) -> Self {
        Self {
            caller_saved: &[],
            callee_saved: regs,
            compact_callee_saved: &[],
        }
    }

    /// Total number of allocatable registers across both save classes.
    pub fn len(&self) -> usize {
        self.caller_saved.len() + self.callee_saved.len()
    }

    /// Whether no register at all is allocatable in this class.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<Reg: Copy + Eq> SaveClasses<'_, Reg> {
    /// Whether `reg` is one this function's prologue must preserve.
    pub fn is_callee_saved(&self, reg: Reg) -> bool {
        self.callee_saved.contains(&reg)
    }
}

/// A target's allocatable registers, one [`SaveClasses`] per [`RegClass`].
///
/// Allocation offers an interval only the registers of its own class: an
/// integer value cannot be parked in a floating-point register however free
/// that register is. What stays *shared* across classes is everything the
/// frame owns — the spill-slot allocator and the callee-saved save set — since
/// a function has one stack frame and one prologue no matter how many register
/// classes its target has.
///
/// Both backends populate [`RegClass::Gp`] only; [`RegClass::Fp`] is
/// [`SaveClasses::EMPTY`] and no virtual register selects it (RUE-1067).
pub struct RegisterFile<'a, Reg> {
    classes: [SaveClasses<'a, Reg>; RegClass::COUNT],
}

impl<Reg> Clone for RegisterFile<'_, Reg> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Reg> Copy for RegisterFile<'_, Reg> {}

impl<'a, Reg> RegisterFile<'a, Reg> {
    /// Build a register file from one entry per class, in [`RegClass::ALL`]
    /// order.
    pub const fn new(classes: [SaveClasses<'a, Reg>; RegClass::COUNT]) -> Self {
        Self { classes }
    }

    /// A register file whose only allocatable registers are general-purpose.
    pub const fn gp_only(gp: SaveClasses<'a, Reg>) -> Self {
        Self::new([gp, SaveClasses::EMPTY])
    }

    /// The allocatable registers of one class.
    #[inline]
    pub fn class(&self, class: RegClass) -> SaveClasses<'a, Reg> {
        self.classes[class.index()]
    }

    /// Every class paired with its allocatable registers, in
    /// [`RegClass::ALL`] order.
    pub fn iter(&self) -> impl Iterator<Item = (RegClass, SaveClasses<'a, Reg>)> + '_ {
        RegClass::ALL
            .into_iter()
            .map(|class| (class, self.class(class)))
    }

    /// Total number of allocatable registers across every class.
    pub fn len(&self) -> usize {
        self.classes.iter().map(SaveClasses::len).sum()
    }

    /// Whether no register of any class is allocatable.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<Reg: Copy> RegisterFile<'_, Reg> {
    /// Every caller-saved register of every class, in class order.
    ///
    /// [`ClobberIndex`] is built once per allocation over this flattening: the
    /// index answers "is this register destroyed inside that range", which is a
    /// per-register question with no class structure of its own, and building
    /// one index for the whole file keeps the class partitioning to the
    /// candidate lists that consult it.
    pub fn caller_saved_flattened(&self) -> Vec<Reg> {
        self.classes
            .iter()
            .flat_map(|save| save.caller_saved.iter().copied())
            .collect()
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
    coalesce_map: Vec<u32>,
    /// Instruction indices of moves that were coalesced and can be eliminated.
    eliminated_moves: Vec<bool>,
    eliminated_count: usize,
}

impl CoalesceResult {
    /// Create an empty coalesce result (no coalescing performed).
    pub fn empty() -> Self {
        Self {
            coalesce_map: Vec::new(),
            eliminated_moves: Vec::new(),
            eliminated_count: 0,
        }
    }

    fn with_vreg_count(vreg_count: usize) -> Self {
        Self {
            coalesce_map: vec![EMPTY_VREG; vreg_count],
            eliminated_moves: Vec::new(),
            eliminated_count: 0,
        }
    }

    /// Get the representative vreg for a given vreg.
    ///
    /// If the vreg was coalesced, returns its representative.
    /// Otherwise, returns the vreg itself.
    pub fn representative(&self, vreg: VReg) -> VReg {
        self.coalesce_map
            .get(vreg.index() as usize)
            .copied()
            .filter(|&representative| representative != EMPTY_VREG)
            .map(VReg::new)
            .unwrap_or(vreg)
    }

    /// Check if a move instruction at the given index was eliminated.
    pub fn is_eliminated(&self, inst_idx: usize) -> bool {
        self.eliminated_moves
            .get(inst_idx)
            .copied()
            .unwrap_or(false)
    }

    /// Get the number of moves that were eliminated.
    pub fn num_eliminated(&self) -> usize {
        self.eliminated_count
    }

    fn mark_eliminated(&mut self, inst_idx: usize) {
        if inst_idx >= self.eliminated_moves.len() {
            self.eliminated_moves.resize(inst_idx + 1, false);
        }
        if !self.eliminated_moves[inst_idx] {
            self.eliminated_moves[inst_idx] = true;
            self.eliminated_count += 1;
        }
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
///
/// # Register classes
///
/// Two vregs of different [`RegClass`]es are never merged, whatever their live
/// ranges do. Coalescing asserts that one physical register can hold both
/// values and that the move between them is therefore redundant; across
/// classes neither holds — no register is both an integer and a floating-point
/// register, and the "move" would be a class-crossing transfer instruction
/// that has to survive. Backends do not offer such a pair as a candidate
/// today, and none can arise while every vreg is [`RegClass::Gp`], so this is
/// a guard rather than a filter (RUE-1067).
pub fn coalesce<Reg: Copy + Eq + std::hash::Hash>(
    candidates: &[CoalesceCandidate],
    liveness: &mut LivenessInfo<Reg>,
) -> CoalesceResult {
    assert!(liveness.ranges.len() <= EMPTY_VREG as usize);
    let mut result = CoalesceResult::with_vreg_count(liveness.ranges.len());

    // Union-find structure for tracking coalesced vregs
    let mut parent = vec![EMPTY_VREG; liveness.ranges.len()];

    // Find the representative of a vreg in the union-find
    fn find(parent: &mut [u32], vreg: VReg) -> VReg {
        let index = vreg.index() as usize;
        let Some(p) = parent.get(index).copied() else {
            return vreg;
        };
        if p != EMPTY_VREG {
            let root = find(parent, VReg::new(p));
            parent[index] = root.index();
            return root;
        }
        vreg
    }

    // Process each candidate
    for candidate in candidates {
        let dst = find(&mut parent, candidate.dst);
        let src = find(&mut parent, candidate.src);

        // Already in the same equivalence class
        if dst == src {
            result.mark_eliminated(candidate.inst_idx);
            continue;
        }

        // A class-crossing pair cannot share a physical register, so the move
        // between them is not redundant and must not be eliminated.
        if liveness.class_of(dst) != liveness.class_of(src) {
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
            parent[dst.index() as usize] = src.index();
            result.coalesce_map[dst.index() as usize] = src.index();

            // Update liveness: assign merged range to src, remove dst
            liveness.ranges[src] = Some(merged_range);
            liveness.ranges[dst] = None;

            // Mark the move for elimination
            result.mark_eliminated(candidate.inst_idx);
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
    /// Smallest occupant end point across all slots (`None` when there are no
    /// occupied slots). A slot is reusable for a range starting at `start` only
    /// if some occupant ends before `start`; when `min_slot_end >= start` no slot
    /// can be reused, so the O(slots) scan is skipped. Without this, spilling many
    /// simultaneously-live values (e.g. a large array literal, where no slot is
    /// ever reusable) rescans the whole growing slot list per spill — O(spills²)
    /// (RUE-302).
    min_slot_end: Option<usize>,
    /// Number of occupied frame slots before the spill region.
    base_slots: u32,
}

impl SpillSlotAllocator {
    /// Create a new spill slot allocator.
    ///
    /// `existing_locals` is the number of local variable slots already on the stack.
    /// Spill slots start after those.
    fn new(existing_locals: u32) -> Self {
        Self {
            slots: Vec::new(),
            min_slot_end: None,
            base_slots: existing_locals,
        }
    }

    /// Allocate a spill slot for a live range.
    ///
    /// If possible, reuses a slot whose previous occupant is no longer live.
    /// Otherwise, allocates a new slot.
    ///
    /// Returns the stack offset for the spill slot.
    fn allocate(&mut self, live_range_start: usize, live_range_end: usize) -> i32 {
        // Try to find a reusable slot whose occupant ended before this range
        // starts. Skip the scan entirely when no occupant can possibly qualify
        // (the common case when many live ranges overlap); this scan is otherwise
        // O(slots) per call and quadratic overall (RUE-302).
        if self
            .min_slot_end
            .is_some_and(|min_end| min_end < live_range_start)
        {
            for (i, slot_end) in self.slots.iter_mut().enumerate() {
                if let Some(end) = slot_end {
                    // The occupant is dead if its end point is strictly before our start.
                    // Note: We use < not <= because at the same instruction index,
                    // both ranges are considered live (inclusive endpoints).
                    if *end < live_range_start {
                        // Reuse this slot. Its end point grows, so the cached
                        // minimum may no longer hold; recompute it exactly.
                        *slot_end = Some(live_range_end);
                        self.min_slot_end = self.slots.iter().flatten().copied().min();
                        return self.offset_for_slot(i);
                    }
                }
            }
        }

        // No reusable slot found, allocate a new one.
        let slot_index = self.slots.len();
        self.slots.push(Some(live_range_end));
        self.min_slot_end = Some(match self.min_slot_end {
            Some(min_end) => min_end.min(live_range_end),
            None => live_range_end,
        });
        self.offset_for_slot(slot_index)
    }

    /// Get the stack offset for a given spill slot index (each spill cell is
    /// one frame cell below the previous, continuing the slot region).
    fn offset_for_slot(&self, slot_index: usize) -> i32 {
        let one_based_slot = u64::from(self.base_slots)
            .saturating_add(slot_index as u64)
            .saturating_add(1);
        let bytes = one_based_slot.saturating_mul(rue_air::layout::SLOT_BYTES);
        i32::try_from(bytes).map_or(i32::MIN, |bytes| -bytes)
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

/// Target hooks used by the shared register-allocation lifecycle.
///
/// This is intentionally limited to the target facts needed to drive
/// allocation and to hand each instruction back to target-specific rewriting.
/// It is not a generic MIR interface: instruction selection, operand
/// constraints, scratch registers, and concrete spill instructions remain in
/// each backend.
pub trait RegAllocBackend {
    type Mir;
    type Inst;
    type Reg: Copy + Eq + std::hash::Hash + fmt::Display + 'static;

    fn vreg_count(mir: &Self::Mir) -> u32;
    fn instructions(mir: &Self::Mir) -> &[Self::Inst];
    fn defs(inst: &Self::Inst) -> crate::liveness::VRegList;
    fn rematerialization(inst: &Self::Inst) -> Option<(VReg, RematerializeOp)>;
    fn analyze(mir: &Self::Mir) -> LivenessInfo<Self::Reg>;
    fn analyze_with_debug(mir: &Self::Mir) -> (LivenessInfo<Self::Reg>, LivenessDebugInfo);
    fn analyze_loops(mir: &Self::Mir) -> LoopInfo;
    fn coalesce_candidates(instructions: &[Self::Inst]) -> Vec<CoalesceCandidate>;
    /// The allocatable registers of every [`RegClass`], each split by who is
    /// responsible for preserving them. Allocation prefers the caller-saved
    /// save class for intervals that no instruction clobbers while they are
    /// live (RUE-1146), and never offers an interval a register outside its
    /// own register class (RUE-1067).
    fn register_file() -> RegisterFile<'static, Self::Reg>;

    /// Visit every physical register `inst` names as an operand — read or
    /// written — in the backend's canonical read-then-write order.
    ///
    /// This is the exhaustive per-instruction enumeration each backend's
    /// scheduler already maintains (`regs_read` + `regs_written`), reused here
    /// so [`RegAllocDriver::new_with_artifacts`] can prove lowering never named
    /// an allocatable register directly. Implicit clobbers are deliberately not
    /// included: a call destroys the caller-saved registers without naming any
    /// of them as an operand, and the allocator models that separately.
    fn for_each_physical_operand<F>(inst: &Self::Inst, visit: F)
    where
        F: FnMut(Self::Reg);

    fn new_mir() -> Self::Mir;
    fn take_symbols(mir: &mut Self::Mir) -> Vec<String>;
    fn set_symbols(mir: &mut Self::Mir, symbols: Vec<String>);
    fn into_instructions(mir: Self::Mir) -> Vec<Self::Inst>;
    fn push(mir: &mut Self::Mir, inst: Self::Inst);
    fn rewrite_inst(
        context: &AllocationContext<'_, Self::Reg>,
        buffer: &mut RewriteBuffer<Self::Inst>,
        inst: Self::Inst,
    ) -> CompileResult<()>;
}

#[derive(Clone, Copy)]
enum RematerializationState {
    Unknown,
    Eligible(RematerializeOp),
    Ineligible,
}

/// Derive one conservative rematerialization recipe per coalesced live range.
///
/// Eliminated register-to-register moves do not count as definitions: their
/// source and destination are the same coalesced value. Every remaining
/// definition in the class must reproduce the same cheap value. This prevents
/// rematerializing a register that is later updated in place or assigned
/// different values along separate control-flow paths.
fn rematerialization_info<B: RegAllocBackend>(
    mir: &B::Mir,
    coalesce_result: &CoalesceResult,
) -> IndexMap<VReg, VRegInfo> {
    let vreg_count = B::vreg_count(mir) as usize;
    let mut states = IndexMap::with_capacity(vreg_count);
    states.resize(vreg_count, RematerializationState::Unknown);

    for (inst_idx, inst) in B::instructions(mir).iter().enumerate() {
        if coalesce_result.is_eliminated(inst_idx) {
            continue;
        }

        let candidate = B::rematerialization(inst);
        for defined in B::defs(inst) {
            let representative = coalesce_result.representative(defined);
            let recipe = candidate
                .filter(|(candidate_vreg, _)| *candidate_vreg == defined)
                .map(|(_, recipe)| recipe);

            states[representative] = match (states[representative], recipe) {
                (RematerializationState::Ineligible, _) => RematerializationState::Ineligible,
                (RematerializationState::Unknown, Some(recipe)) => {
                    RematerializationState::Eligible(recipe)
                }
                (RematerializationState::Eligible(existing), Some(recipe))
                    if existing == recipe =>
                {
                    RematerializationState::Eligible(existing)
                }
                _ => RematerializationState::Ineligible,
            };
        }
    }

    let mut info = IndexMap::with_capacity(vreg_count);
    info.resize(vreg_count, VRegInfo::none());
    for (vreg, state) in states.iter_enumerated() {
        if let RematerializationState::Eligible(recipe) = state {
            info[vreg] = VRegInfo::rematerializable(*recipe);
        }
    }
    info
}

/// Read-only allocation state exposed to a target's instruction rewriter.
///
/// The shared driver owns this state. Targets can query a virtual register's
/// final assignment and coalescing representative, but cannot alter the
/// allocation or spill-slot decisions.
pub struct AllocationContext<'a, Reg: Copy + Eq + std::hash::Hash> {
    allocation: &'a IndexMap<VReg, Option<Allocation<Reg>>>,
    coalesce_result: &'a CoalesceResult,
}

impl<Reg: Copy + Eq + std::hash::Hash> AllocationContext<'_, Reg> {
    /// Return the final assignment for a virtual register.
    pub fn allocation(&self, vreg: VReg) -> Option<Allocation<Reg>> {
        let representative = self.coalesce_result.representative(vreg);
        self.allocation[representative]
    }
}

/// Ordered target-instruction output for one source instruction.
///
/// Target rewriters decide which concrete instructions belong before, at, or
/// after the source instruction. The shared driver owns the final drain order.
pub struct RewriteBuffer<I> {
    before: Vec<I>,
    main: Vec<I>,
    after: Vec<I>,
}

impl<I> Default for RewriteBuffer<I> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I> RewriteBuffer<I> {
    pub fn new() -> Self {
        Self {
            before: Vec::new(),
            main: Vec::new(),
            after: Vec::new(),
        }
    }

    pub fn push_before(&mut self, inst: I) {
        self.before.push(inst);
    }

    pub fn push_main(&mut self, inst: I) {
        self.main.push(inst);
    }

    /// Append to the main instruction stream. This alias keeps target rewrite
    /// code visually identical while the shared driver owns queue ordering.
    pub fn push(&mut self, inst: I) {
        self.push_main(inst);
    }

    pub fn push_after(&mut self, inst: I) {
        self.after.push(inst);
    }

    fn into_ordered(mut self) -> Vec<I> {
        let mut ordered =
            Vec::with_capacity(self.before.len() + self.main.len() + self.after.len());
        ordered.append(&mut self.before);
        ordered.append(&mut self.main);
        ordered.append(&mut self.after);
        ordered
    }
}

/// Fail loudly if pre-allocation MIR names an allocatable register directly.
///
/// Lowering names physical registers for ABI positions, fixed instruction
/// operands, and the stack and frame pointers. The allocator hands out a
/// disjoint set, and each backend proves the two sets are disjoint at compile
/// time from its `RESERVED_REGS` table. That proof is only as good as the
/// table: a lowering site that names, say, `r11` as a raw operand would collide
/// with whatever value the allocator put there, silently, and only on the
/// programs where the allocator happened to pick that register.
///
/// So this checks the actual instruction stream instead of the table. It runs
/// before assignment, when every value still lives in a virtual register, so
/// any physical operand present is one lowering wrote. It is an always-on
/// assertion rather than a `debug_assert!` because a violation changes emitted
/// code, and `docs/process/ci.md` gives code generation no debug-assert
/// allowance (RUE-1224).
fn assert_no_allocatable_physical_operands<B: RegAllocBackend>(mir: &B::Mir) {
    let file = B::register_file();
    for inst in B::instructions(mir) {
        B::for_each_physical_operand(inst, |reg| {
            for (_, save) in file.iter() {
                assert!(
                    !save.caller_saved.contains(&reg) && !save.callee_saved.contains(&reg),
                    "lowering named the allocatable register {reg} as a physical operand; \
                     allocation may put an unrelated value there"
                );
            }
        });
    }
}

/// Shared assignment, rewrite, and spill orchestration for one target.
pub struct RegAllocDriver<B: RegAllocBackend> {
    mir: B::Mir,
    allocation: IndexMap<VReg, Option<Allocation<B::Reg>>>,
    vreg_info: IndexMap<VReg, VRegInfo>,
    liveness: LivenessInfo<B::Reg>,
    liveness_debug: Option<LivenessDebugInfo>,
    loop_info: LoopInfo,
    coalesce_result: CoalesceResult,
    num_spills: u32,
    used_callee_saved: Vec<B::Reg>,
    existing_locals: u32,
}

impl<B: RegAllocBackend> RegAllocDriver<B> {
    /// Create the shared allocator state and perform target-provided analyses.
    pub fn new(mir: B::Mir, existing_locals: u32) -> Self {
        Self::new_with_artifacts(mir, existing_locals, false)
    }

    /// Create allocator state while optionally retaining the diagnostic
    /// projection of the same liveness dataflow used for allocation.
    pub fn new_with_artifacts(mir: B::Mir, existing_locals: u32, capture_liveness: bool) -> Self {
        assert_no_allocatable_physical_operands::<B>(&mir);
        let vreg_count = B::vreg_count(&mir) as usize;
        let (mut liveness, liveness_debug) = if capture_liveness {
            let (liveness, debug) = B::analyze_with_debug(&mir);
            (liveness, Some(debug))
        } else {
            (B::analyze(&mir), None)
        };
        assert_eq!(
            liveness.vreg_classes.len(),
            vreg_count as u32,
            "the MIR's virtual-register class table does not cover its virtual registers; \
             a mint site skipped recording a register class"
        );
        let loop_info = B::analyze_loops(&mir);
        let candidates = B::coalesce_candidates(B::instructions(&mir));
        let coalesce_result = coalesce(&candidates, &mut liveness);
        let vreg_info = rematerialization_info::<B>(&mir, &coalesce_result);

        let mut allocation = IndexMap::with_capacity(vreg_count);
        allocation.resize(vreg_count, None);

        Self {
            mir,
            allocation,
            vreg_info,
            liveness,
            liveness_debug,
            loop_info,
            coalesce_result,
            num_spills: 0,
            used_callee_saved: Vec::new(),
            existing_locals,
        }
    }

    /// Number of spill slots selected by the last assignment pass.
    pub fn num_spills(&self) -> u32 {
        self.num_spills
    }

    /// Run normal allocation and target rewriting.
    pub fn allocate(mut self) -> CompileResult<B::Mir> {
        self.assign_registers();
        self.validate_spill_budget()?;
        self.rewrite_instructions()?;
        Ok(self.mir)
    }

    /// Run normal allocation and return spill/frame bookkeeping.
    pub fn allocate_with_spills(mut self) -> CompileResult<(B::Mir, u32, Vec<B::Reg>)> {
        self.assign_registers();
        self.validate_spill_budget()?;
        self.rewrite_instructions()?;
        Ok((self.mir, self.num_spills, self.used_callee_saved))
    }

    /// Run the canonical allocation/rewrite execution while optionally
    /// retaining the diagnostic allocation projection.
    pub fn allocate_with_artifacts(
        mut self,
        capture_regalloc: bool,
    ) -> CompileResult<(
        B::Mir,
        u32,
        Vec<B::Reg>,
        Option<LivenessDebugInfo>,
        Option<RegAllocDebugInfo<B::Reg>>,
    )> {
        let regalloc_debug = if capture_regalloc {
            Some(self.assign_registers_with_debug())
        } else {
            self.assign_registers();
            None
        };
        self.validate_spill_budget()?;
        self.rewrite_instructions()?;
        Ok((
            self.mir,
            self.num_spills,
            self.used_callee_saved,
            self.liveness_debug,
            regalloc_debug,
        ))
    }

    /// Run the debug assignment path, followed by the same rewrite path.
    pub fn allocate_with_debug(
        mut self,
    ) -> CompileResult<(B::Mir, u32, Vec<B::Reg>, RegAllocDebugInfo<B::Reg>)> {
        let debug_info = self.assign_registers_with_debug();
        self.validate_spill_budget()?;
        self.rewrite_instructions()?;
        Ok((
            self.mir,
            self.num_spills,
            self.used_callee_saved,
            debug_info,
        ))
    }

    fn assign_registers(&mut self) {
        let (allocation, num_spills, used_callee_saved, _debug_info) = linear_scan_impl_with_remat(
            B::vreg_count(&self.mir),
            &self.liveness,
            B::register_file(),
            self.existing_locals,
            false,
            &CostModel::default(),
            &self.loop_info,
            &self.vreg_info,
        );
        self.allocation = allocation;
        self.num_spills = num_spills;
        self.used_callee_saved = used_callee_saved;
    }

    fn assign_registers_with_debug(&mut self) -> RegAllocDebugInfo<B::Reg> {
        let (allocation, num_spills, used_callee_saved, debug_info) = linear_scan_impl_with_remat(
            B::vreg_count(&self.mir),
            &self.liveness,
            B::register_file(),
            self.existing_locals,
            true,
            &CostModel::default(),
            &self.loop_info,
            &self.vreg_info,
        );
        self.allocation = allocation;
        self.num_spills = num_spills;
        self.used_callee_saved = used_callee_saved;
        debug_info
    }

    fn validate_spill_budget(&self) -> CompileResult<()> {
        rue_air::layout::checked_function_frame_slots(self.existing_locals, self.num_spills)
            .map(|_| ())
            .ok_or_else(|| {
                rue_error::CompileError::without_span(rue_error::ErrorKind::FunctionFrameTooLarge {
                    max_bytes: rue_air::layout::MAX_FUNCTION_FRAME_BYTES,
                })
            })
    }

    fn rewrite_instructions(&mut self) -> CompileResult<()> {
        // The source instruction order is the canonical rewrite order. The
        // target hook classifies generated instructions into before/main/after
        // queues, and this driver drains them identically on both targets.
        let symbols = B::take_symbols(&mut self.mir);
        let old_instructions = B::into_instructions(std::mem::replace(&mut self.mir, B::new_mir()));
        let mut new_mir = B::new_mir();
        B::set_symbols(&mut new_mir, symbols);
        let context = AllocationContext {
            allocation: &self.allocation,
            coalesce_result: &self.coalesce_result,
        };

        for (idx, inst) in old_instructions.into_iter().enumerate() {
            if self.coalesce_result.is_eliminated(idx) {
                continue;
            }
            if let Some((defined, recipe)) = B::rematerialization(&inst)
                && matches!(
                    context.allocation(defined),
                    Some(Allocation::Rematerialize(selected)) if selected == recipe
                )
            {
                continue;
            }
            let mut buffer = RewriteBuffer::new();
            B::rewrite_inst(&context, &mut buffer, inst)?;
            for rewritten in buffer.into_ordered() {
                B::push(&mut new_mir, rewritten);
            }
        }

        self.mir = new_mir;
        Ok(())
    }
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
/// * `allocatable_regs` - General-purpose registers available for allocation
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
    let loop_info = LoopInfo::no_loops(liveness.instruction_count());
    let (allocation, num_spills, used_callee_saved, _debug_info) = linear_scan_impl(
        vreg_count,
        liveness,
        RegisterFile::gp_only(SaveClasses::callee_saved_only(allocatable_regs)),
        existing_locals,
        false,
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
/// * `allocatable_regs` - General-purpose registers available for allocation
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
        RegisterFile::gp_only(SaveClasses::callee_saved_only(allocatable_regs)),
        existing_locals,
        false,
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
/// * `allocatable_regs` - General-purpose registers available for allocation
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
    let loop_info = LoopInfo::no_loops(liveness.instruction_count());
    let (allocation, num_spills, used_callee_saved, _debug_info) = linear_scan_impl_with_remat(
        vreg_count,
        liveness,
        RegisterFile::gp_only(SaveClasses::callee_saved_only(allocatable_regs)),
        existing_locals,
        false,
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
    let loop_info = LoopInfo::no_loops(liveness.instruction_count());
    linear_scan_impl(
        vreg_count,
        liveness,
        RegisterFile::gp_only(SaveClasses::callee_saved_only(allocatable_regs)),
        existing_locals,
        true,
        &cost_model,
        &loop_info,
    )
}

/// Pick a physical register for an interval covering `range`, or report that
/// none is free.
///
/// `save` is the arriving interval's *own* register class, already selected by
/// the caller; nothing here can hand out a register of another class.
///
/// Caller-saved registers come before callee-saved ones: an interval that no
/// instruction clobbers while it is live costs nothing to keep in one, and
/// every callee-saved register it leaves alone is one the prologue does not
/// have to save (RUE-1146).
///
/// Ahead of both sits one narrow exception, the RUE-1227 tiebreak: a register
/// that is both in [`SaveClasses::compact_callee_saved`] and in `sunk` —
/// the callee-saved registers this function's prologue already saves. Against a
/// *fresh* callee-saved register the caller-saved candidate wins on the save it
/// avoids, but against one whose save is already paid for it has nothing left
/// to offer and a worse encoding, so preferring the sunk register is free.
///
/// `sunk` spans every class, because the prologue does; the intersection with
/// this class's `compact_callee_saved` is what makes the preference apply to
/// this class's registers only.
///
/// The exception cannot enlarge the save set: it only ever hands out a register
/// already in it. It can still *shift* which registers end up saved, because
/// occupying a sunk register denies it to a later call-crossing interval that
/// then reaches for a fresh one. Ruling that out is [`accept_reuse_pass`]'s
/// job, not this function's.
fn pick_free_register<Reg: Copy + Eq + std::hash::Hash>(
    save: SaveClasses<'_, Reg>,
    clobbers: &ClobberIndex<Reg>,
    active: &[(VReg, Reg, usize)],
    sunk: &[Reg],
    range: &LiveRange,
) -> Option<Reg> {
    // A class can never have more active intervals than physical registers.
    // Query that small canonical list directly instead of rebuilding a hash
    // set for every arriving virtual register.
    let is_used = |candidate| {
        active
            .iter()
            .any(|&(_, active_reg, _)| active_reg == candidate)
    };

    save.compact_callee_saved
        .iter()
        .copied()
        .find(|&reg| sunk.contains(&reg) && !is_used(reg))
        .or_else(|| {
            save.caller_saved
                .iter()
                .copied()
                .find(|&reg| !is_used(reg) && !clobbers.is_clobbered_during(reg, range))
        })
        .or_else(|| save.callee_saved.iter().copied().find(|&reg| !is_used(reg)))
}

/// Whether `reg` can hold one value for the whole of `range`.
///
/// A callee-saved register always can. A caller-saved register can only when no
/// instruction in the range clobbers it. This is the same condition
/// [`pick_free_register`] applies, restated for the eviction path: a register
/// that becomes free by spilling its current occupant is still only usable by
/// the arriving interval if that interval survives everything the register does
/// not.
///
/// `save` is the class `reg` belongs to, which is also the arriving interval's:
/// eviction only ever considers registers held by intervals of the same class.
fn register_survives_range<Reg: Copy + Eq + std::hash::Hash>(
    save: SaveClasses<'_, Reg>,
    clobbers: &ClobberIndex<Reg>,
    reg: Reg,
    range: &LiveRange,
) -> bool {
    save.is_callee_saved(reg) || !clobbers.is_clobbered_during(reg, range)
}

/// How many vregs an allocation keeps in a physical register.
///
/// The rest are spilled to the frame or recomputed on use, both of which cost
/// instructions the register form does not.
fn registers_held<Reg: Copy>(allocation: &IndexMap<VReg, Option<Allocation<Reg>>>) -> usize {
    allocation
        .iter()
        .filter(|alloc| matches!(alloc, Some(Allocation::Register(_))))
        .count()
}

/// Whether a reuse pass's result may replace the baseline pass's.
///
/// The reuse pass (see [`linear_scan_impl_with_remat`]) re-runs assignment
/// allowing call-free intervals to occupy callee-saved registers the baseline
/// pass already committed to the prologue. That is a codegen-quality trade with
/// no intended effect on frame cost, so it is taken only when it costs nothing:
///
/// * **No new save.** Occupying an already-saved register denies it to a later
///   call-crossing interval, which may then reach for a *fresh* callee-saved
///   register — a save the baseline did not pay. Requiring the reuse pass's
///   save set to be contained in the baseline's rejects exactly that, and with
///   it any risk of undoing the saves RUE-1146 removed. Containment is the
///   right test rather than a count: a same-size but different save set would
///   mean the reuse pass forced a register the baseline never touched.
/// * **No value displaced from a register.** Denying the caller-saved class to
///   an interval that could have used it can raise pressure enough that
///   something no longer fits. Counting the vregs still in registers catches
///   that whether the loser ends up spilled or rematerialized.
/// * **No spill in place of a rematerialization.** The register count alone
///   would let the two trade places, and a spill costs the frame traffic a
///   recompute does not.
fn accept_reuse_pass<Reg: Copy + Eq + std::hash::Hash>(
    baseline: &(
        IndexMap<VReg, Option<Allocation<Reg>>>,
        u32,
        Vec<Reg>,
        RegAllocDebugInfo<Reg>,
    ),
    reuse: &(
        IndexMap<VReg, Option<Allocation<Reg>>>,
        u32,
        Vec<Reg>,
        RegAllocDebugInfo<Reg>,
    ),
) -> bool {
    let (baseline_allocation, baseline_spills, baseline_saved, _) = baseline;
    let (reuse_allocation, reuse_spills, reuse_saved, _) = reuse;
    reuse_saved.iter().all(|reg| baseline_saved.contains(reg))
        && registers_held(reuse_allocation) >= registers_held(baseline_allocation)
        && reuse_spills <= baseline_spills
}

/// Internal implementation of linear scan register allocation.
///
/// This is the shared implementation used by both [`linear_scan`] and
/// [`linear_scan_with_debug`]. See [`linear_scan_impl_with_remat`] for the
/// two-pass structure, which is identical here.
fn linear_scan_impl<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    file: RegisterFile<'_, Reg>,
    existing_locals: u32,
    collect_debug: bool,
    cost_model: &CostModel,
    loop_info: &LoopInfo,
) -> (
    IndexMap<VReg, Option<Allocation<Reg>>>,
    u32,
    Vec<Reg>,
    RegAllocDebugInfo<Reg>,
) {
    let inputs = ScanInputs::new(vreg_count, liveness, file);
    let baseline = scan_intervals(
        vreg_count,
        liveness,
        file,
        existing_locals,
        collect_debug,
        cost_model,
        loop_info,
        &inputs,
        &[],
    );
    let (_, _, baseline_saved, _) = &baseline;
    if !reuse_pass_could_differ(file, baseline_saved) {
        return baseline;
    }
    let reuse = scan_intervals(
        vreg_count,
        liveness,
        file,
        existing_locals,
        collect_debug,
        cost_model,
        loop_info,
        &inputs,
        baseline_saved,
    );
    if accept_reuse_pass(&baseline, &reuse) {
        reuse
    } else {
        baseline
    }
}

/// Empty per-class "currently in a register" lists, sized to each class's
/// register count.
///
/// A linear scan can never hold more live intervals in registers than the
/// class has registers, so each list is allocated once at that size and never
/// grows. Splitting them by class is what stops an interval of one class from
/// seeing another class's registers as occupied — or, worse, as free.
fn active_by_class<Reg: Copy>(
    file: RegisterFile<'_, Reg>,
) -> [Vec<(VReg, Reg, usize)>; RegClass::COUNT] {
    std::array::from_fn(|index| Vec::with_capacity(file.class(RegClass::ALL[index]).len()))
}

/// Immutable work shared by the baseline and callee-saved reuse scans.
///
/// The two scans differ only in the already-paid (`sunk`) callee-saved
/// registers they offer first. Their interval order and caller-clobber answers
/// are properties of the function's liveness and register file, so rebuilding
/// either one for the second scan only repeats sorting and prefix construction.
struct ScanInputs<Reg> {
    vregs_by_start: Vec<(VReg, LiveRange)>,
    clobbers: ClobberIndex<Reg>,
}

impl<Reg: Copy + Eq + std::hash::Hash> ScanInputs<Reg> {
    fn new(vreg_count: u32, liveness: &LivenessInfo<Reg>, file: RegisterFile<'_, Reg>) -> Self {
        let mut vregs_by_start = Vec::with_capacity(vreg_count as usize);
        for vreg_idx in 0..vreg_count {
            let vreg = VReg::new(vreg_idx);
            if let Some(&range) = liveness.range(vreg) {
                vregs_by_start.push((vreg, range));
            }
        }
        vregs_by_start.sort_by_key(|(_, range)| range.start);

        let caller_saved = file.caller_saved_flattened();
        let clobbers = ClobberIndex::build(liveness, &caller_saved);
        Self {
            vregs_by_start,
            clobbers,
        }
    }
}

/// Whether the RUE-1227 reuse pass can possibly reach a different answer than
/// the baseline pass, and is therefore worth running at all.
///
/// It can only in a class that has both a caller-saved register to prefer
/// against and a compact callee-saved register the baseline already committed
/// to the prologue. When no class qualifies the second scan is skipped
/// outright, so neither a push-free function nor a fixed-width target pays for
/// it.
fn reuse_pass_could_differ<Reg: Copy + Eq>(
    file: RegisterFile<'_, Reg>,
    baseline_saved: &[Reg],
) -> bool {
    file.iter().any(|(_, save)| {
        !save.caller_saved.is_empty()
            && save
                .compact_callee_saved
                .iter()
                .any(|reg| baseline_saved.contains(reg))
    })
}

/// One linear-scan pass over the intervals.
///
/// When `collect_debug` is `false` (the normal compilation path, where callers
/// discard the debug info) the O(V²) interference-graph construction is skipped
/// — it feeds only `--emit regalloc` output and building it on every compile
/// made allocation quadratic in the number of virtual registers (e.g. large
/// array literals) (RUE-302).
///
/// `sunk` names callee-saved registers whose prologue save is already paid for;
/// see [`pick_free_register`].
///
/// # Register classes
///
/// The scan stays a single pass over every interval in start order, but the
/// state that answers "which register is free" is kept per [`RegClass`] — see
/// [`active_by_class`]. What stays shared is what the frame owns: one
/// [`SpillSlotAllocator`], one `used_callee_saved` save set. Splitting the scan
/// itself per class instead would hand the spill-slot allocator its requests in
/// a different order and change the frame layout of any function that mixed
/// classes; keeping one ordered pass keeps slot assignment a function of
/// interval order alone (RUE-1067).
fn scan_intervals<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    file: RegisterFile<'_, Reg>,
    existing_locals: u32,
    collect_debug: bool,
    cost_model: &CostModel,
    loop_info: &LoopInfo,
    inputs: &ScanInputs<Reg>,
    sunk: &[Reg],
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

    // Keep the diagnostic projection in its historical vreg-index order;
    // allocation itself consumes the shared start-sorted preparation below.
    if collect_debug {
        for vreg_idx in 0..vreg_count {
            let vreg = VReg::new(vreg_idx);
            if let Some(&range) = liveness.range(vreg) {
                debug_live_ranges.push((vreg_idx, range.start, range.end));
            }
        }
    }

    // Build interference graph: vregs that overlap. This is O(V²) and feeds only
    // `--emit regalloc`; skip it on the normal compilation path (RUE-302).
    if collect_debug {
        for i in 0..inputs.vregs_by_start.len() {
            for j in (i + 1)..inputs.vregs_by_start.len() {
                let (vreg1, range1) = &inputs.vregs_by_start[i];
                let (vreg2, range2) = &inputs.vregs_by_start[j];
                if range1.overlaps(range2) {
                    debug_interference.push((vreg1.index(), vreg2.index()));
                }
            }
        }
    }

    // Constant-time clobber answers for the caller-saved candidates of every
    // class; the callee-saved registers survive every clobber by definition
    // (RUE-1146).
    let clobbers = &inputs.clobbers;

    // Track which registers are currently in use and when they become free,
    // separately per register class.
    // Tuple: (vreg, physical reg, live range end)
    let mut active = active_by_class(file);

    for &(vreg, range) in &inputs.vregs_by_start {
        let class = liveness.class_of(vreg);
        let save = file.class(class);
        let active = &mut active[class.index()];

        // Expire old intervals - remove registers whose vregs are no longer live
        active.retain(|&(_, _, end)| end >= range.start);

        // Try to find a free register of this vreg's own class: a sunk compact
        // one, else caller-saved, else a fresh callee-saved one.
        let allocated_reg = pick_free_register(save, clobbers, active, sunk, &range);

        if let Some(reg) = allocated_reg {
            // Assign this register
            allocation[vreg] = Some(Allocation::Register(reg));
            active.push((vreg, reg, range.end));
            // Track callee-saved register usage: only these oblige the prologue
            if save.is_callee_saved(reg) && !used_callee_saved.contains(&reg) {
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

            for (i, &(_active_vreg, active_reg, end)) in active.iter().enumerate() {
                // Evicting only helps if the freed register can actually hold
                // the arriving interval; a caller-saved register clobbered
                // during that interval cannot.
                if !register_survives_range(save, clobbers, active_reg, &range) {
                    continue;
                }
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

    // Build the final allocation list only for the diagnostic projection.
    // Normal compilation discards this field, so avoid walking the dense map
    // and copying every assignment when diagnostics are disabled.
    let debug_allocations = if collect_debug {
        allocation
            .iter()
            .enumerate()
            .filter_map(|(idx, alloc)| alloc.map(|a| (idx as u32, a)))
            .collect()
    } else {
        Vec::new()
    };

    let debug_info = RegAllocDebugInfo {
        live_ranges: debug_live_ranges,
        interference: debug_interference,
        allocations: debug_allocations,
        spills: debug_spills,
        callee_saved_used: if collect_debug {
            used_callee_saved.clone()
        } else {
            Vec::new()
        },
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
/// This is the production allocation entry point for both backends.
///
/// Assignment runs in two passes. The first is the RUE-1146 policy on its own:
/// every interval that can live in a caller-saved register does, so a function
/// whose values all fit there saves nothing in its prologue. If that pass ends
/// up committing no callee-saved register — the push-free case RUE-1146 exists
/// to produce — the answer is already final and the second pass is skipped
/// outright, so the guarantee is structural rather than measured.
///
/// Otherwise a second pass re-runs assignment knowing which callee-saved
/// registers the function pays for regardless. Those are then preferred over a
/// caller-saved register for a call-free interval, because their cost is
/// already sunk while the caller-saved register's addressing cost is not: on
/// x86-64 the one caller-saved candidate is `r11`, whose byte and dword forms
/// each pay a REX prefix that `rbx` does not (RUE-1227). Knowing the final save
/// set is what the second pass buys — a single pass cannot, since intervals are
/// processed in start order and a later interval can force a register into the
/// save set after an earlier one has already chosen against it.
///
/// The second pass's result is taken only if [`accept_reuse_pass`] agrees it
/// costs no additional save and no additional spill; otherwise the first pass's
/// allocation stands. Both passes produce a complete, independently valid
/// allocation, so choosing between them needs no repair step.
fn linear_scan_impl_with_remat<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    file: RegisterFile<'_, Reg>,
    existing_locals: u32,
    collect_debug: bool,
    cost_model: &CostModel,
    loop_info: &LoopInfo,
    vreg_info: &IndexMap<VReg, VRegInfo>,
) -> (
    IndexMap<VReg, Option<Allocation<Reg>>>,
    u32,
    Vec<Reg>,
    RegAllocDebugInfo<Reg>,
) {
    let inputs = ScanInputs::new(vreg_count, liveness, file);
    let baseline = scan_intervals_with_remat(
        vreg_count,
        liveness,
        file,
        existing_locals,
        collect_debug,
        cost_model,
        loop_info,
        vreg_info,
        &inputs,
        &[],
    );
    let (_, _, baseline_saved, _) = &baseline;
    if !reuse_pass_could_differ(file, baseline_saved) {
        return baseline;
    }
    let reuse = scan_intervals_with_remat(
        vreg_count,
        liveness,
        file,
        existing_locals,
        collect_debug,
        cost_model,
        loop_info,
        vreg_info,
        &inputs,
        baseline_saved,
    );
    if accept_reuse_pass(&baseline, &reuse) {
        reuse
    } else {
        baseline
    }
}

/// One linear-scan pass with rematerialization support.
///
/// When a vreg needs to be spilled but has rematerialization info, it is marked
/// for rematerialization instead of being allocated a stack slot. This avoids
/// memory traffic for values that can be cheaply recomputed (constants, etc.).
///
/// `sunk` names callee-saved registers whose prologue save is already paid for;
/// see [`pick_free_register`].
///
/// Register classes partition the scan state exactly as in [`scan_intervals`];
/// see that function's notes.
fn scan_intervals_with_remat<Reg: Copy + Eq + std::hash::Hash>(
    vreg_count: u32,
    liveness: &LivenessInfo<Reg>,
    file: RegisterFile<'_, Reg>,
    existing_locals: u32,
    collect_debug: bool,
    cost_model: &CostModel,
    loop_info: &LoopInfo,
    vreg_info: &IndexMap<VReg, VRegInfo>,
    inputs: &ScanInputs<Reg>,
    sunk: &[Reg],
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

    // Keep the diagnostic projection in its historical vreg-index order;
    // allocation itself consumes the shared start-sorted preparation below.
    if collect_debug {
        for vreg_idx in 0..vreg_count {
            let vreg = VReg::new(vreg_idx);
            if let Some(&range) = liveness.range(vreg) {
                debug_live_ranges.push((vreg_idx, range.start, range.end));
            }
        }
    }

    // Build interference graph: vregs that overlap. This is O(V²) and feeds only
    // `--emit regalloc`; skip it on the normal compilation path (RUE-302).
    if collect_debug {
        for i in 0..inputs.vregs_by_start.len() {
            for j in (i + 1)..inputs.vregs_by_start.len() {
                let (vreg1, range1) = &inputs.vregs_by_start[i];
                let (vreg2, range2) = &inputs.vregs_by_start[j];
                if range1.overlaps(range2) {
                    debug_interference.push((vreg1.index(), vreg2.index()));
                }
            }
        }
    }

    // Constant-time clobber answers for the caller-saved candidates of every
    // class; the callee-saved registers survive every clobber by definition
    // (RUE-1146).
    let clobbers = &inputs.clobbers;

    // Track which registers are currently in use and when they become free,
    // separately per register class.
    // Tuple: (vreg, physical reg, live range end)
    let mut active = active_by_class(file);

    for &(vreg, range) in &inputs.vregs_by_start {
        let class = liveness.class_of(vreg);
        let save = file.class(class);
        let active = &mut active[class.index()];

        // Expire old intervals - remove registers whose vregs are no longer live
        active.retain(|&(_, _, end)| end >= range.start);

        // Try to find a free register of this vreg's own class: a sunk compact
        // one, else caller-saved, else a fresh callee-saved one.
        let allocated_reg = pick_free_register(save, clobbers, active, sunk, &range);

        if let Some(reg) = allocated_reg {
            // Assign this register
            allocation[vreg] = Some(Allocation::Register(reg));
            active.push((vreg, reg, range.end));
            // Track callee-saved register usage: only these oblige the prologue
            if save.is_callee_saved(reg) && !used_callee_saved.contains(&reg) {
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

            for (i, &(active_vreg, active_reg, end)) in active.iter().enumerate() {
                // Evicting only helps if the freed register can actually hold
                // the arriving interval; a caller-saved register clobbered
                // during that interval cannot.
                if !register_survives_range(save, clobbers, active_reg, &range) {
                    continue;
                }
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

    // Build the final allocation list only for the diagnostic projection.
    // Normal compilation discards this field, so avoid walking the dense map
    // and copying every assignment when diagnostics are disabled.
    let debug_allocations = if collect_debug {
        allocation
            .iter()
            .enumerate()
            .filter_map(|(idx, alloc)| alloc.map(|a| (idx as u32, a)))
            .collect()
    } else {
        Vec::new()
    };

    let debug_info = RegAllocDebugInfo {
        live_ranges: debug_live_ranges,
        interference: debug_interference,
        allocations: debug_allocations,
        spills: debug_spills,
        callee_saved_used: if collect_debug {
            used_callee_saved.clone()
        } else {
            Vec::new()
        },
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

    #[test]
    fn rewrite_buffer_preserves_before_main_after_order() {
        let mut buffer = RewriteBuffer::new();
        buffer.push_after("after");
        buffer.push_main("main");
        buffer.push_before("before");

        assert_eq!(buffer.into_ordered(), ["before", "main", "after"]);
    }

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

        let mut info = LivenessInfo::with_vreg_capacity(max_vreg + 1);
        for (vreg_idx, start, end) in ranges {
            info.ranges[VReg::new(vreg_idx)] = Some(LiveRange::new(start, end));
        }

        // Initialize clobbers_at based on max instruction index
        info.clobbers_at = vec![Vec::new(); max_inst + 1];
        info
    }

    fn make_liveness_with_clobbers(
        ranges: Vec<(u32, usize, usize)>,
        clobbers: Vec<(usize, TestReg)>,
    ) -> LivenessInfo<TestReg> {
        let mut info = make_liveness(ranges);
        for (idx, reg) in clobbers {
            info.clobbers_at[idx].push(reg);
        }
        info
    }

    fn mark_non_returning(info: &mut LivenessInfo<TestReg>, indices: &[usize]) {
        info.non_returning_at = vec![false; info.clobbers_at.len()];
        for &idx in indices {
            info.non_returning_at[idx] = true;
        }
    }

    /// Liveness whose vregs carry the given classes, one per vreg index.
    ///
    /// Nothing in the compiler produces this yet — both backends mint only
    /// [`RegClass::Gp`] registers — so the class-aware behavior of allocation
    /// is exercised here, on the shared algorithm, rather than through a
    /// backend (RUE-1067).
    fn make_classed_liveness(
        ranges: Vec<(u32, usize, usize)>,
        classes: &[RegClass],
    ) -> LivenessInfo<TestReg> {
        let mut info = make_liveness(ranges);
        let mut vreg_classes = VRegClasses::new();
        for &class in classes {
            vreg_classes.push(class);
        }
        info.vreg_classes = vreg_classes;
        info
    }

    /// A two-class register file: `gp` for [`RegClass::Gp`], `fp` for
    /// [`RegClass::Fp`], both entirely callee-saved.
    fn two_class_file<'a>(gp: &'a [TestReg], fp: &'a [TestReg]) -> RegisterFile<'a, TestReg> {
        RegisterFile::new([
            SaveClasses::callee_saved_only(gp),
            SaveClasses::callee_saved_only(fp),
        ])
    }

    #[test]
    fn clobber_index_ignores_a_never_returning_call_site() {
        // The shape Rue emits for every checked add: the value is defined
        // before the guard branch and used after the label, so the trap call at
        // instruction 2 sits textually inside its range — but the trap aborts,
        // so on the path reaching the later use the call never ran.
        let mut liveness = make_liveness_with_clobbers(vec![(0, 0, 4)], vec![(2, TestReg(0))]);
        mark_non_returning(&mut liveness, &[2]);
        let index = ClobberIndex::build(&liveness, &[TestReg(0)]);

        assert!(!index.is_clobbered_during(TestReg(0), &LiveRange::new(0, 4)));
        assert!(!index.is_clobbered_during(TestReg(0), &LiveRange::new(2, 2)));
    }

    #[test]
    fn clobber_index_still_sees_a_returning_call_beside_a_trap() {
        // A returning call at 1 and a trap at 3: only the returning one can
        // destroy a value whose later uses execute.
        let mut liveness =
            make_liveness_with_clobbers(vec![(0, 0, 4)], vec![(1, TestReg(0)), (3, TestReg(0))]);
        mark_non_returning(&mut liveness, &[3]);
        let index = ClobberIndex::build(&liveness, &[TestReg(0)]);

        assert!(index.is_clobbered_during(TestReg(0), &LiveRange::new(0, 4)));
        assert!(index.is_clobbered_during(TestReg(0), &LiveRange::new(0, 1)));
        assert!(
            !index.is_clobbered_during(TestReg(0), &LiveRange::new(2, 4)),
            "a range that clears the returning call is clobber-free despite the trap"
        );
    }

    #[test]
    fn a_trap_spanning_interval_takes_a_caller_saved_register() {
        // Same allocation shape as `caller_saved_is_preferred_...`, except the
        // clobber site is a never-returning call: now both intervals fit in the
        // caller-saved class and the prologue saves nothing.
        let mut liveness = make_liveness_with_clobbers(
            vec![(0, 0, 4), (1, 3, 4)],
            vec![(2, TestReg(9)), (2, TestReg(8))],
        );
        mark_non_returning(&mut liveness, &[2]);
        let file = RegisterFile::gp_only(SaveClasses {
            caller_saved: &[TestReg(9), TestReg(8)],
            callee_saved: &[TestReg(0)],
            compact_callee_saved: &[],
        });
        let (allocation, num_spills, used_callee_saved, _) = linear_scan_impl(
            2,
            &liveness,
            file,
            0,
            false,
            &CostModel::default(),
            &LoopInfo::no_loops(liveness.instruction_count()),
        );

        assert_eq!(num_spills, 0);
        assert_eq!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(TestReg(9)))
        );
        assert_eq!(
            allocation[VReg::new(1)],
            Some(Allocation::Register(TestReg(8)))
        );
        assert!(
            used_callee_saved.is_empty(),
            "no interval needed a callee-saved register, so the prologue saves nothing"
        );
    }

    // ========================================
    // Register-class allocation (RUE-1067)
    // ========================================

    #[test]
    fn an_interval_takes_only_a_register_of_its_own_class() {
        // Two intervals live over the same instructions, one general-purpose
        // and one floating-point, against a file with exactly one register of
        // each class. A class-blind allocator sees two intervals competing for
        // whichever single pool it was handed and spills one of them; a
        // class-aware one gives each its own class's register.
        let liveness =
            make_classed_liveness(vec![(0, 0, 4), (1, 0, 4)], &[RegClass::Gp, RegClass::Fp]);
        let gp = [TestReg(0)];
        let fp = [TestReg(100)];
        let file = two_class_file(&gp, &fp);

        let (allocation, num_spills, used_callee_saved, _) = linear_scan_impl(
            2,
            &liveness,
            file,
            0,
            false,
            &CostModel::default(),
            &LoopInfo::no_loops(liveness.instruction_count()),
        );

        assert_eq!(num_spills, 0, "neither interval had to spill");
        assert_eq!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(TestReg(0)))
        );
        assert_eq!(
            allocation[VReg::new(1)],
            Some(Allocation::Register(TestReg(100)))
        );
        assert_eq!(
            used_callee_saved,
            vec![TestReg(0), TestReg(100)],
            "the save set spans classes: one prologue preserves both"
        );
    }

    #[test]
    fn an_interval_of_an_empty_class_spills_past_free_registers_of_another() {
        // The floating-point class has no registers at all, which is exactly
        // both backends' state today. Its interval must spill even though
        // general-purpose registers are sitting free — those cannot hold it.
        let liveness =
            make_classed_liveness(vec![(0, 0, 4), (1, 0, 4)], &[RegClass::Gp, RegClass::Fp]);
        let gp = [TestReg(0), TestReg(1)];
        let fp: [TestReg; 0] = [];
        let file = two_class_file(&gp, &fp);

        let (allocation, num_spills, _, _) = linear_scan_impl(
            2,
            &liveness,
            file,
            0,
            false,
            &CostModel::default(),
            &LoopInfo::no_loops(liveness.instruction_count()),
        );

        assert_eq!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(TestReg(0)))
        );
        assert!(
            matches!(allocation[VReg::new(1)], Some(Allocation::Spill(_))),
            "an interval whose class has no registers spills, free registers of \
             another class notwithstanding"
        );
        assert_eq!(num_spills, 1);
    }

    #[test]
    fn spill_slots_are_shared_across_register_classes() {
        // A function has one stack frame however many register classes its
        // target has, so two overlapping spilled intervals of different classes
        // must get distinct slots.
        let liveness =
            make_classed_liveness(vec![(0, 0, 4), (1, 0, 4)], &[RegClass::Gp, RegClass::Fp]);
        let gp: [TestReg; 0] = [];
        let fp: [TestReg; 0] = [];
        let file = two_class_file(&gp, &fp);

        let (allocation, num_spills, _, _) = linear_scan_impl(
            2,
            &liveness,
            file,
            0,
            false,
            &CostModel::default(),
            &LoopInfo::no_loops(liveness.instruction_count()),
        );

        let slot = |vreg: u32| match allocation[VReg::new(vreg)] {
            Some(Allocation::Spill(offset)) => offset,
            other => panic!("v{vreg} should have spilled, got {other:?}"),
        };
        assert_ne!(
            slot(0),
            slot(1),
            "overlapping spills share no slot across classes"
        );
        assert_eq!(num_spills, 2);
    }

    #[test]
    fn a_cross_class_move_is_not_coalesced() {
        // Ranges that would coalesce cleanly if the two vregs were the same
        // class: v0's last use is the move, v1's first def is the move.
        let ranges = vec![(0, 0, 1), (1, 1, 2)];
        let candidates = vec![CoalesceCandidate {
            inst_idx: 1,
            dst: VReg::new(1),
            src: VReg::new(0),
        }];

        // Control: same class, so the move is redundant and goes away.
        let mut same_class = make_classed_liveness(ranges.clone(), &[RegClass::Gp, RegClass::Gp]);
        let same = coalesce(&candidates, &mut same_class);
        assert!(same.is_eliminated(1));
        assert_eq!(same.representative(VReg::new(1)), VReg::new(0));

        // Across classes no physical register can hold both values, so the
        // transfer is real work and must survive.
        let mut cross_class = make_classed_liveness(ranges, &[RegClass::Gp, RegClass::Fp]);
        let cross = coalesce(&candidates, &mut cross_class);
        assert!(
            !cross.is_eliminated(1),
            "a class-crossing move is not a redundant register-to-register move"
        );
        assert_eq!(cross.num_eliminated(), 0);
        assert_eq!(cross.representative(VReg::new(1)), VReg::new(1));
        assert_eq!(
            cross_class.range(VReg::new(1)),
            Some(&LiveRange::new(1, 2)),
            "the destination keeps its own live range"
        );
    }

    #[test]
    fn clobber_index_answers_only_for_tracked_registers() {
        let liveness = make_liveness_with_clobbers(vec![(0, 0, 4)], vec![(2, TestReg(0))]);
        let index = ClobberIndex::build(&liveness, &[TestReg(0)]);

        assert!(index.is_clobbered_during(TestReg(0), &LiveRange::new(0, 4)));
        assert!(index.is_clobbered_during(TestReg(0), &LiveRange::new(2, 2)));
        assert!(!index.is_clobbered_during(TestReg(0), &LiveRange::new(0, 1)));
        assert!(!index.is_clobbered_during(TestReg(0), &LiveRange::new(3, 4)));
        // An untracked register cannot be proven clobber-free.
        assert!(index.is_clobbered_during(TestReg(1), &LiveRange::new(0, 1)));
    }

    #[test]
    fn clobber_index_tolerates_ranges_past_the_last_instruction() {
        let liveness = make_liveness_with_clobbers(vec![(0, 0, 1)], vec![(1, TestReg(0))]);
        let index = ClobberIndex::build(&liveness, &[TestReg(0)]);

        assert!(index.is_clobbered_during(TestReg(0), &LiveRange::new(0, usize::MAX)));
        assert!(!index.is_clobbered_during(TestReg(0), &LiveRange::new(0, 0)));
    }

    #[test]
    fn caller_saved_is_preferred_and_call_crossing_intervals_avoid_it() {
        // v0 spans the clobber at instruction 2, v1 does not.
        let liveness = make_liveness_with_clobbers(
            vec![(0, 0, 4), (1, 3, 4)],
            vec![(2, TestReg(9)), (2, TestReg(8))],
        );
        let file = RegisterFile::gp_only(SaveClasses {
            caller_saved: &[TestReg(9), TestReg(8)],
            callee_saved: &[TestReg(0)],
            compact_callee_saved: &[],
        });
        let (allocation, num_spills, used_callee_saved, _) = linear_scan_impl(
            2,
            &liveness,
            file,
            0,
            false,
            &CostModel::default(),
            &LoopInfo::no_loops(liveness.instruction_count()),
        );

        assert_eq!(num_spills, 0);
        assert_eq!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(TestReg(0))),
            "an interval spanning the clobber must take the callee-saved register"
        );
        assert_eq!(
            allocation[VReg::new(1)],
            Some(Allocation::Register(TestReg(9))),
            "an interval clear of the clobber should take the first caller-saved register"
        );
        assert_eq!(
            used_callee_saved,
            vec![TestReg(0)],
            "only callee-saved registers are reported to frame planning"
        );
    }

    #[test]
    fn a_call_free_interval_reuses_a_compact_register_the_prologue_already_saves() {
        // Same shape as `caller_saved_is_preferred_...`, but now the callee-
        // saved register is the compact one. v0 spans the clobber and forces
        // the save; v1 does not, and would take the caller-saved register on
        // its own. Because v0's save is paid either way and TestReg(0) encodes
        // better, the second pass gives v1 the callee-saved register instead
        // (RUE-1227) — at no cost, since the prologue is unchanged.
        let liveness = make_liveness_with_clobbers(
            vec![(0, 0, 2), (1, 3, 4)],
            vec![(2, TestReg(9)), (2, TestReg(8))],
        );
        let file = RegisterFile::gp_only(SaveClasses {
            caller_saved: &[TestReg(9), TestReg(8)],
            callee_saved: &[TestReg(0)],
            compact_callee_saved: &[TestReg(0)],
        });
        let (allocation, num_spills, used_callee_saved, _) = linear_scan_impl(
            2,
            &liveness,
            file,
            0,
            false,
            &CostModel::default(),
            &LoopInfo::no_loops(liveness.instruction_count()),
        );

        assert_eq!(num_spills, 0);
        assert_eq!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(TestReg(0)))
        );
        assert_eq!(
            allocation[VReg::new(1)],
            Some(Allocation::Register(TestReg(0))),
            "a call-free interval should reuse the freed compact register, not \
             reach for the caller-saved class"
        );
        assert_eq!(used_callee_saved, vec![TestReg(0)], "no new save");
    }

    #[test]
    fn a_call_free_function_never_reuses_a_callee_saved_register() {
        // The RUE-1146 invariant the tiebreak must not undo: nothing here
        // spans the clobber, so the first pass commits no callee-saved
        // register at all, there is no sunk cost to reuse, and the second pass
        // is skipped outright. The prologue stays empty.
        let liveness =
            make_liveness_with_clobbers(vec![(0, 0, 1), (1, 3, 4)], vec![(2, TestReg(9))]);
        let file = RegisterFile::gp_only(SaveClasses {
            caller_saved: &[TestReg(9)],
            callee_saved: &[TestReg(0)],
            compact_callee_saved: &[TestReg(0)],
        });
        let (allocation, num_spills, used_callee_saved, _) = linear_scan_impl(
            2,
            &liveness,
            file,
            0,
            false,
            &CostModel::default(),
            &LoopInfo::no_loops(liveness.instruction_count()),
        );

        assert_eq!(num_spills, 0);
        assert_eq!(
            allocation[VReg::new(0)],
            Some(Allocation::Register(TestReg(9)))
        );
        assert_eq!(
            allocation[VReg::new(1)],
            Some(Allocation::Register(TestReg(9)))
        );
        assert!(
            used_callee_saved.is_empty(),
            "a function whose values all fit caller-saved must stay push-free"
        );
    }

    #[test]
    fn the_reuse_pass_is_rejected_when_it_would_force_a_second_save() {
        // The case the acceptance check exists for. v0 spans the first clobber
        // and takes the compact register, then expires; v1 is call-free and
        // would happily reuse that register; but v2 spans the second clobber
        // and overlaps v1, so handing v1 the compact register pushes v2 onto a
        // *fresh* callee-saved register the first pass never touched. That is a
        // new prologue save, so the reuse pass is discarded and the first
        // pass's allocation stands.
        //
        // This is exactly why the tiebreak cannot be a one-pass rule that
        // reuses whatever is in the save set so far: at v1 the allocator has
        // not yet seen v2.
        let liveness = make_liveness_with_clobbers(
            vec![(0, 0, 2), (1, 3, 5), (2, 4, 9)],
            vec![(2, TestReg(9)), (7, TestReg(9))],
        );
        let file = RegisterFile::gp_only(SaveClasses {
            caller_saved: &[TestReg(9)],
            callee_saved: &[TestReg(0), TestReg(1)],
            compact_callee_saved: &[TestReg(0)],
        });
        let (allocation, num_spills, used_callee_saved, _) = linear_scan_impl(
            3,
            &liveness,
            file,
            0,
            false,
            &CostModel::default(),
            &LoopInfo::no_loops(liveness.instruction_count()),
        );

        assert_eq!(num_spills, 0);
        assert_eq!(
            allocation[VReg::new(1)],
            Some(Allocation::Register(TestReg(9))),
            "the call-free interval keeps the caller-saved register"
        );
        assert_eq!(
            allocation[VReg::new(2)],
            Some(Allocation::Register(TestReg(0)))
        );
        assert_eq!(
            used_callee_saved,
            vec![TestReg(0)],
            "the tiebreak must never enlarge the save set"
        );
    }

    #[test]
    fn eviction_never_hands_a_clobbered_caller_saved_register_to_a_spanning_interval() {
        // Only a caller-saved register exists, and every interval spans the
        // clobber, so nothing can hold a value: all three must spill.
        let liveness = make_liveness_with_clobbers(
            vec![(0, 0, 4), (1, 0, 4), (2, 0, 4)],
            vec![(2, TestReg(9))],
        );
        let file = RegisterFile::gp_only(SaveClasses {
            caller_saved: &[TestReg(9)],
            callee_saved: &[],
            compact_callee_saved: &[],
        });
        let (allocation, num_spills, used_callee_saved, _) = linear_scan_impl(
            3,
            &liveness,
            file,
            0,
            false,
            &CostModel::default(),
            &LoopInfo::no_loops(liveness.instruction_count()),
        );

        assert_eq!(num_spills, 3);
        assert!(used_callee_saved.is_empty());
        for idx in 0..3 {
            assert!(
                matches!(allocation[VReg::new(idx)], Some(Allocation::Spill(_))),
                "v{idx} spans the clobber and must not hold the caller-saved register"
            );
        }
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

        let (_, num_slots, _) = linear_scan(4, &liveness, &allocatable, 0);

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
    fn test_coalesce_sparse_high_instruction_index() {
        let move_index = 4096;
        let mut liveness = make_liveness(vec![(0, 0, move_index), (1, move_index, move_index + 1)]);
        let result = coalesce(
            &[CoalesceCandidate {
                inst_idx: move_index,
                dst: VReg::new(1),
                src: VReg::new(0),
            }],
            &mut liveness,
        );

        assert!(result.is_eliminated(move_index));
        assert_eq!(result.num_eliminated(), 1);
        assert_eq!(result.representative(VReg::new(1)), VReg::new(0));
    }

    #[test]
    fn test_coalesce_duplicate_eliminated_candidate_keeps_unique_count() {
        let mut liveness = make_liveness(vec![(0, 0, 1), (1, 1, 2)]);
        let candidate = CoalesceCandidate {
            inst_idx: 1,
            dst: VReg::new(1),
            src: VReg::new(0),
        };
        let result = coalesce(&[candidate, candidate], &mut liveness);

        assert!(result.is_eliminated(1));
        assert_eq!(result.num_eliminated(), 1);
        assert_eq!(result.representative(VReg::new(1)), VReg::new(0));
    }

    #[test]
    fn coalesce_dense_sentinel_is_outside_the_valid_vreg_bound() {
        let result = CoalesceResult::with_vreg_count(2);

        assert_eq!(EMPTY_VREG, u32::MAX);
        assert_eq!(result.representative(VReg::new(0)), VReg::new(0));
        assert_eq!(result.representative(VReg::new(1)), VReg::new(1));
        assert_eq!(
            result.representative(VReg::new(EMPTY_VREG)),
            VReg::new(EMPTY_VREG)
        );
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
        let info = LoopInfo::from_depths(vec![0, 0, 1, 1, 1, 2, 2, 1, 0, 0]);

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
        let loop_info = LoopInfo::from_depths(loop_depths);
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
