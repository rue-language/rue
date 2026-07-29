//! Byte-based stack-frame layout authority (ADR-0052 phase 4).
//!
//! Stack frames, register-allocator spill slots, and temporaries are a
//! byte-based product of the canonical layout authority rather than a literal
//! `* 8` re-derived independently at every frame-arithmetic site. The per-slot
//! storage width and the call-boundary frame alignment are sourced here from
//! the layout authority's [`SLOT_BYTES`], so `cfg_lower`'s local addressing,
//! both backends' prologue/epilogue sizing, both spill allocators, and the
//! `--emit stackframe` reporter cannot drift apart.
//!
//! ADR-0052 keeps three representations separate. Frame *layout* is physical:
//! each cell has a byte offset, size, and alignment. The internal *value
//! decomposition* stays slot-shaped — a spilled or stack-homed value fragment
//! still occupies exactly one [`SLOT_BYTES`] cell — so every frame cell is one
//! `SLOT_BYTES` cell today and the gate-off frame is byte-for-byte identical to
//! the historical slot model. A future narrow-frame phase varies the per-cell
//! width without disturbing the consumers that read offsets and sizes from this
//! authority; conversion between a slot-shaped value and narrow memory happens
//! at explicit pack/unpack boundaries, not in frame arithmetic.

use rue_air::layout::{MAX_FUNCTION_FRAME_BYTES, SLOT_BYTES, STACK_FRAME_ALIGNMENT};

/// A frame or transient call area exceeded the one displacement-sized budget
/// shared by semantic analysis, lowering, emission, and presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBudgetExceeded;

/// Convert a byte count into an aligned, backend-immediate-sized stack region.
#[inline]
pub fn checked_aligned_region_bytes(bytes: u64) -> Result<u32, FrameBudgetExceeded> {
    let aligned = bytes
        .checked_add(STACK_FRAME_ALIGNMENT - 1)
        .ok_or(FrameBudgetExceeded)?
        / STACK_FRAME_ALIGNMENT
        * STACK_FRAME_ALIGNMENT;
    if aligned > MAX_FUNCTION_FRAME_BYTES {
        return Err(FrameBudgetExceeded);
    }
    Ok(aligned as u32)
}

/// Validate one outgoing call's simultaneous stack reservation: hidden return
/// storage, compact by-value argument copies, and stack-passed ABI slots.
pub fn checked_call_area_bytes(
    stack_slots: u64,
    sret_storage_bytes: u64,
    indirect_argument_bytes: u64,
) -> Result<u32, FrameBudgetExceeded> {
    let stack_bytes = stack_slots
        .checked_mul(frame_cell_bytes())
        .ok_or(FrameBudgetExceeded)?;
    let stack_bytes = u64::from(checked_aligned_region_bytes(stack_bytes)?);
    let total = sret_storage_bytes
        .checked_add(indirect_argument_bytes)
        .and_then(|bytes| bytes.checked_add(stack_bytes))
        .ok_or(FrameBudgetExceeded)?;
    checked_aligned_region_bytes(total)
}

/// Byte size of one frame storage cell holding a slot-shaped value fragment.
///
/// Sourced from the canonical layout authority so the frame cannot drift from
/// the physical slot width. Every local, parameter home, sret pointer cell, and
/// register-allocator spill slot is one such cell today (the internal value
/// decomposition keeps slot-shaped fragments); a narrow-frame phase later
/// varies this per cell.
#[inline]
pub const fn frame_cell_bytes() -> u64 {
    SLOT_BYTES
}

/// FP-relative byte offset of frame slot `slot`, before the saved-register area
/// (callee-saved registers, and the FP/LR pair on AArch64) is accounted for.
///
/// Slot 0 is the cell immediately below the frame pointer; slots descend. With
/// uniform [`frame_cell_bytes`] cells this is `-((slot + 1) * cell_bytes)`; the
/// backends add the saved-register offset via their own adjustment.
#[inline]
pub fn slot_offset_pre_saved(slot: u32) -> i32 {
    -(frame_cell_bytes() as i32 * (slot as i32 + 1))
}

/// Total byte span of `num_slots` contiguous frame cells.
#[inline]
pub fn slot_region_bytes(num_slots: u32) -> i32 {
    i32::try_from(frame_cell_bytes() * u64::from(num_slots))
        .expect("frame slot region must be validated before offset calculation")
}

/// FP-relative byte offset of frame slot `slot` on AArch64, matching the
/// backend prologue exactly.
///
/// AArch64 sets its frame pointer *at* the saved FP/LR pair
/// (`stp x29, x30, [sp, #-16]!; mov x29, sp`), so the FP/LR bytes sit at and
/// above `fp`; only the callee-saved pair region lies between `fp` and the slot
/// region. Unlike [`FrameLayout::slot_offset`], the FP/LR 16 bytes are therefore
/// *not* subtracted here. The emitter's parameter homing / sret store and the
/// `--emit stackframe` reporter both derive slot locations from this one
/// function so they cannot drift (RUE-774).
#[inline]
pub fn aarch64_slot_offset(num_callee_saved: usize, slot: u32) -> i32 {
    -(aarch64_callee_saved_pairs_bytes(num_callee_saved) as i32) + slot_offset_pre_saved(slot)
}

/// FP-relative byte offset of the low register of callee-saved pair
/// `pair_index` (0-based) on AArch64.
///
/// The prologue stores the first pair at `[fp, #-16]` via `stp .., [sp, #-16]!`
/// (after `fp` is set) and each subsequent pair another 16 bytes down; the high
/// register of a pair sits 8 bytes above this offset, and a trailing odd
/// register occupies the low half of the next 16-byte pair slot. Shared by the
/// emitter's prologue and the `--emit stackframe` reporter (RUE-774).
#[inline]
pub fn aarch64_callee_saved_pair_offset(pair_index: usize) -> i32 {
    -(STACK_FRAME_ALIGNMENT as i32) * (pair_index as i32 + 1)
}

/// Round a frame byte size up to [`STACK_FRAME_ALIGNMENT`].
#[inline]
pub fn align_frame_size(bytes: i32) -> i32 {
    i32::try_from(
        checked_aligned_region_bytes(bytes as u64)
            .expect("frame size must be validated before alignment"),
    )
    .expect("validated frame size fits i32")
}

/// Whether a function establishes a frame pointer at all (RUE-1171).
///
/// A frame pointer exists to address frame slots. A function with no frame
/// storage whatsoever — no locals, no spills, no homed parameters, no sret
/// pointer cell — and no calls has nothing to address and nothing to keep
/// aligned, so its prologue can skip the frame-pointer setup and the slot
/// region entirely. Calls are excluded because a call both consumes the
/// call-boundary alignment the slot region establishes and, on AArch64,
/// clobbers the link register the FP/LR save preserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePointer {
    /// The prologue establishes a frame pointer and allocates the slot region;
    /// every frame slot is addressed FP-relative.
    Established,
    /// No frame pointer and no slot region. Callee-saved registers, if the
    /// allocator used any, are still saved and restored — they are addressed by
    /// the push/pop (pre/post-indexed) instructions themselves, not the frame
    /// pointer — and their offsets below the entry stack pointer are the same
    /// numbers [`FrameLayout::slot_offset`]'s saved-area helpers report.
    Omitted,
}

/// How a target saves the registers that sit between the frame pointer and the
/// slot region. Their total byte span shifts every frame-slot offset down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedRegScheme {
    /// x86-64: each saved GPR is one 8-byte push; the saved RBP and return
    /// address sit *above* the frame pointer and are not part of this area.
    X86_64,
    /// AArch64: a 16-byte FP/LR pair at the top of the frame, then callee-saved
    /// registers stored in 16-byte pairs (rounded up).
    Aarch64,
}

/// Bytes AArch64 uses to store `num_callee_saved` callee-saved GPRs, saved in
/// 16-byte pairs (rounded up). This excludes the separate FP/LR pair.
#[inline]
pub fn aarch64_callee_saved_pairs_bytes(num_callee_saved: usize) -> u64 {
    let pairs = (num_callee_saved + 1) / 2;
    pairs as u64 * STACK_FRAME_ALIGNMENT
}

impl SavedRegScheme {
    /// Bytes the prologue's callee-saved pushes occupy, excluding any
    /// frame-pointer save. This is the whole saved area of a frameless function
    /// ([`FramePointer::Omitted`]).
    pub fn callee_saved_bytes(self, num_callee_saved: usize) -> u64 {
        match self {
            SavedRegScheme::X86_64 => num_callee_saved as u64 * frame_cell_bytes(),
            SavedRegScheme::Aarch64 => aarch64_callee_saved_pairs_bytes(num_callee_saved),
        }
    }

    /// Bytes reserved for saved registers between the frame pointer and the
    /// slot region, given `num_callee_saved` saved general-purpose registers.
    pub fn saved_area_bytes(self, num_callee_saved: usize) -> u64 {
        match self {
            // The saved RBP sits *above* the frame pointer, so it is not part
            // of this area.
            SavedRegScheme::X86_64 => self.callee_saved_bytes(num_callee_saved),
            // FP/LR pair plus the paired callee-saved registers.
            SavedRegScheme::Aarch64 => {
                STACK_FRAME_ALIGNMENT + self.callee_saved_bytes(num_callee_saved)
            }
        }
    }
}

/// A byte-based description of one function's stack frame: the saved-register
/// area followed by a run of slot cells (locals, parameter homes, the optional
/// sret pointer cell, and register-allocator spill slots).
///
/// This is the single authority the reporter and the backends share so the
/// frame arithmetic is computed once.
#[derive(Debug, Clone, Copy)]
pub struct FrameLayout {
    scheme: SavedRegScheme,
    frame_pointer: FramePointer,
    saved_area_bytes: u64,
    num_slots: u32,
}

impl FrameLayout {
    /// Build the layout of a function that establishes no frame pointer and
    /// allocates no slot region: only the callee-saved registers the allocator
    /// actually used are pushed (RUE-1171).
    ///
    /// Callers must have established that the function has zero frame slots and
    /// makes no calls; see [`FramePointer::Omitted`]. No budget check is needed
    /// because the saved area is bounded by the target's allocatable
    /// callee-saved register count.
    pub fn frameless(scheme: SavedRegScheme, num_callee_saved: usize) -> Self {
        Self {
            scheme,
            frame_pointer: FramePointer::Omitted,
            saved_area_bytes: scheme.callee_saved_bytes(num_callee_saved),
            num_slots: 0,
        }
    }

    /// Build a frame layout for `num_slots` slot cells sitting below the saved
    /// registers described by `scheme`.
    pub fn try_new(
        scheme: SavedRegScheme,
        num_callee_saved: usize,
        num_slots: u32,
    ) -> Result<Self, FrameBudgetExceeded> {
        let saved_area_bytes = scheme.saved_area_bytes(num_callee_saved);
        let slot_bytes = frame_cell_bytes()
            .checked_mul(u64::from(num_slots))
            .ok_or(FrameBudgetExceeded)?;
        let unaligned = match scheme {
            SavedRegScheme::X86_64 => saved_area_bytes
                .checked_add(slot_bytes)
                .ok_or(FrameBudgetExceeded)?,
            SavedRegScheme::Aarch64 => slot_bytes,
        };
        checked_aligned_region_bytes(unaligned)?;
        if scheme == SavedRegScheme::Aarch64
            && saved_area_bytes
                .checked_add(u64::from(checked_aligned_region_bytes(slot_bytes)?))
                .is_none_or(|bytes| bytes > MAX_FUNCTION_FRAME_BYTES)
        {
            return Err(FrameBudgetExceeded);
        }
        Ok(Self {
            scheme,
            frame_pointer: FramePointer::Established,
            saved_area_bytes,
            num_slots,
        })
    }

    /// Whether this function establishes a frame pointer and a slot region.
    #[inline]
    pub fn frame_pointer(&self) -> FramePointer {
        self.frame_pointer
    }

    /// Bytes reserved for saved registers between the frame pointer and the
    /// slot region. With the frame pointer omitted this is the whole frame:
    /// the callee-saved pushes and nothing else.
    #[inline]
    pub fn saved_area_bytes(&self) -> i32 {
        self.saved_area_bytes as i32
    }

    /// FP-relative byte offset of frame slot `slot`, past the saved-register
    /// area.
    #[inline]
    pub fn slot_offset(&self, slot: u32) -> i32 {
        -self.saved_area_bytes() + slot_offset_pre_saved(slot)
    }

    /// Byte granularity this frame maintains.
    ///
    /// A framed function keeps the call-boundary [`STACK_FRAME_ALIGNMENT`]. A
    /// frameless leaf makes no calls, so it only has to keep the stack pointer
    /// on the granularity its saved-register pushes move it by: one cell per
    /// push on x86-64, a 16-byte pair on AArch64.
    pub fn alignment(&self) -> u64 {
        match (self.frame_pointer, self.scheme) {
            (FramePointer::Omitted, SavedRegScheme::X86_64) => frame_cell_bytes(),
            _ => STACK_FRAME_ALIGNMENT,
        }
    }

    /// Byte size of frame slot `slot`. Uniform [`frame_cell_bytes`] today.
    #[inline]
    pub fn slot_size(&self, _slot: u32) -> u64 {
        frame_cell_bytes()
    }

    /// Total frame size in bytes, including the saved-register area and the
    /// 16-byte-aligned slot region.
    pub fn frame_size(&self) -> u64 {
        if self.frame_pointer == FramePointer::Omitted {
            // No slot region to round up and no frame pointer to align against:
            // the callee-saved pushes are the entire frame.
            return self.saved_area_bytes;
        }
        let slots = slot_region_bytes(self.num_slots);
        match self.scheme {
            // The saved GPR pushes are not 16-aligned on x86-64, so the whole
            // (saved + slots) span is rounded together.
            SavedRegScheme::X86_64 => align_frame_size(self.saved_area_bytes() + slots) as u64,
            // The AArch64 saved area is already a multiple of 16, so only the
            // slot region is rounded before adding it.
            SavedRegScheme::Aarch64 => self.saved_area_bytes + align_frame_size(slots) as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_offsets_match_the_historical_slot_model() {
        // -((slot + 1) * 8) for every slot, gate-off byte-for-byte.
        assert_eq!(slot_offset_pre_saved(0), -8);
        assert_eq!(slot_offset_pre_saved(1), -16);
        assert_eq!(slot_offset_pre_saved(5), -48);
    }

    #[test]
    fn align_frame_size_rounds_up_to_sixteen() {
        assert_eq!(align_frame_size(0), 0);
        assert_eq!(align_frame_size(1), 16);
        assert_eq!(align_frame_size(8), 16);
        assert_eq!(align_frame_size(16), 16);
        assert_eq!(align_frame_size(17), 32);
    }

    #[test]
    fn x86_64_saved_area_is_eight_per_register() {
        assert_eq!(SavedRegScheme::X86_64.saved_area_bytes(0), 0);
        assert_eq!(SavedRegScheme::X86_64.saved_area_bytes(1), 8);
        assert_eq!(SavedRegScheme::X86_64.saved_area_bytes(3), 24);
    }

    #[test]
    fn aarch64_saved_area_pairs_and_reserves_fp_lr() {
        // FP/LR pair (16) plus paired callee-saved registers (16 per pair).
        assert_eq!(SavedRegScheme::Aarch64.saved_area_bytes(0), 16);
        assert_eq!(SavedRegScheme::Aarch64.saved_area_bytes(1), 32);
        assert_eq!(SavedRegScheme::Aarch64.saved_area_bytes(2), 32);
        assert_eq!(SavedRegScheme::Aarch64.saved_area_bytes(3), 48);
    }

    #[test]
    fn frame_size_matches_prior_x86_64_rounding() {
        // round16(callee_saved_size + total_slots * 8).
        let layout = FrameLayout::try_new(SavedRegScheme::X86_64, 1, 2).unwrap();
        // saved = 8, slots = 16 -> round16(24) = 32.
        assert_eq!(layout.frame_size(), 32);
        assert_eq!(layout.slot_offset(0), -8 - 8);
    }

    #[test]
    fn aarch64_slot_offset_excludes_the_fp_lr_pair() {
        // The emitter homes slots at `-callee_saved_pairs_bytes + -(slot+1)*8`;
        // the FP/LR 16 is NOT subtracted (FP points at the FP/LR save). These
        // are the offsets observed in gcd's emitted prologue (4 callee-saved
        // regs -> 32 pair bytes): locals at -40/-48/-56, params at -64/-72.
        assert_eq!(aarch64_slot_offset(4, 0), -40);
        assert_eq!(aarch64_slot_offset(4, 1), -48);
        assert_eq!(aarch64_slot_offset(4, 2), -56);
        assert_eq!(aarch64_slot_offset(4, 3), -64);
        assert_eq!(aarch64_slot_offset(4, 4), -72);
        // No callee-saved registers: only the slot region sits below FP.
        assert_eq!(aarch64_slot_offset(0, 0), -8);
        assert_eq!(aarch64_slot_offset(0, 1), -16);
        // Odd callee-saved count still reserves a full 16-byte pair slot.
        assert_eq!(aarch64_slot_offset(1, 0), -16 - 8);
        assert_eq!(aarch64_slot_offset(3, 0), -32 - 8);
    }

    #[test]
    fn aarch64_callee_saved_pairs_descend_from_minus_sixteen() {
        // First pair at [fp -16]/[fp -8], each subsequent pair another 16 down.
        assert_eq!(aarch64_callee_saved_pair_offset(0), -16);
        assert_eq!(aarch64_callee_saved_pair_offset(1), -32);
        assert_eq!(aarch64_callee_saved_pair_offset(2), -48);
    }

    #[test]
    fn frame_size_matches_prior_aarch64_rounding() {
        // fp_lr(16) + callee_saved_pairs*16 + round16(total_slots * 8).
        let layout = FrameLayout::try_new(SavedRegScheme::Aarch64, 1, 3).unwrap();
        // saved = 16 + 16 = 32, slots = 24 -> round16(24) = 32; total = 64.
        assert_eq!(layout.frame_size(), 64);
        assert_eq!(layout.slot_offset(0), -32 - 8);
    }

    #[test]
    fn frameless_layouts_are_only_the_callee_saved_pushes() {
        // x86-64: one 8-byte push per saved register, no `push rbp` and no
        // 16-byte rounding — a frameless leaf has no call boundary to align.
        let x86 = FrameLayout::frameless(SavedRegScheme::X86_64, 1);
        assert_eq!(x86.frame_pointer(), FramePointer::Omitted);
        assert_eq!(x86.frame_size(), 8);
        assert_eq!(x86.saved_area_bytes(), 8);
        assert_eq!(
            FrameLayout::frameless(SavedRegScheme::X86_64, 0).frame_size(),
            0
        );

        // AArch64: 16-byte pairs, and the FP/LR pair is no longer saved.
        let arm = FrameLayout::frameless(SavedRegScheme::Aarch64, 1);
        assert_eq!(arm.frame_size(), 16);
        assert_eq!(arm.saved_area_bytes(), 16);
        assert_eq!(
            FrameLayout::frameless(SavedRegScheme::Aarch64, 3).frame_size(),
            32
        );
        assert_eq!(
            FrameLayout::frameless(SavedRegScheme::Aarch64, 0).frame_size(),
            0
        );
    }

    #[test]
    fn a_frame_slot_restores_the_full_framed_layout() {
        // Adding one real frame consumer to the frameless x86-64 case above
        // brings back the frame pointer, the slot cell, and 16-byte rounding.
        let framed = FrameLayout::try_new(SavedRegScheme::X86_64, 1, 1).unwrap();
        assert_eq!(framed.frame_pointer(), FramePointer::Established);
        assert_eq!(framed.frame_size(), 16);
        // And on AArch64 the FP/LR pair is saved again.
        let framed = FrameLayout::try_new(SavedRegScheme::Aarch64, 1, 1).unwrap();
        assert_eq!(framed.saved_area_bytes(), 32);
        assert_eq!(framed.frame_size(), 48);
    }

    #[test]
    fn cumulative_frame_budget_covers_alignment_and_saved_areas() {
        let max_slots = (MAX_FUNCTION_FRAME_BYTES / frame_cell_bytes()) as u32;
        assert!(FrameLayout::try_new(SavedRegScheme::X86_64, 0, max_slots).is_ok());
        assert!(FrameLayout::try_new(SavedRegScheme::X86_64, 1, max_slots).is_err());
        assert!(FrameLayout::try_new(SavedRegScheme::Aarch64, 0, max_slots).is_err());
    }

    #[test]
    fn outgoing_call_budget_checks_every_simultaneous_region() {
        assert_eq!(checked_call_area_bytes(1, 16, 16), Ok(48));
        assert!(
            checked_call_area_bytes(1, MAX_FUNCTION_FRAME_BYTES, 0).is_err(),
            "alignment plus one stack slot must exceed the displacement budget"
        );
        assert!(checked_call_area_bytes(u64::MAX, 0, 0).is_err());
    }
}
