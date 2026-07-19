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

use rue_air::layout::SLOT_BYTES;

/// Call-boundary stack alignment. Both supported targets keep the frame
/// 16-byte aligned at calls.
pub const STACK_FRAME_ALIGNMENT: u64 = 16;

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
    frame_cell_bytes() as i32 * num_slots as i32
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
    let align = STACK_FRAME_ALIGNMENT as i32;
    ((bytes + align - 1) / align) * align
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
    /// Bytes reserved for saved registers between the frame pointer and the
    /// slot region, given `num_callee_saved` saved general-purpose registers.
    pub fn saved_area_bytes(self, num_callee_saved: usize) -> u64 {
        match self {
            SavedRegScheme::X86_64 => num_callee_saved as u64 * frame_cell_bytes(),
            // FP/LR pair plus the paired callee-saved registers.
            SavedRegScheme::Aarch64 => {
                STACK_FRAME_ALIGNMENT + aarch64_callee_saved_pairs_bytes(num_callee_saved)
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
    saved_area_bytes: u64,
    num_slots: u32,
}

impl FrameLayout {
    /// Build a frame layout for `num_slots` slot cells sitting below the saved
    /// registers described by `scheme`.
    pub fn new(scheme: SavedRegScheme, num_callee_saved: usize, num_slots: u32) -> Self {
        Self {
            scheme,
            saved_area_bytes: scheme.saved_area_bytes(num_callee_saved),
            num_slots,
        }
    }

    /// Bytes reserved for saved registers between the frame pointer and the
    /// slot region.
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

    /// Byte size of frame slot `slot`. Uniform [`frame_cell_bytes`] today.
    #[inline]
    pub fn slot_size(&self, _slot: u32) -> u64 {
        frame_cell_bytes()
    }

    /// Total frame size in bytes, including the saved-register area and the
    /// 16-byte-aligned slot region.
    pub fn frame_size(&self) -> u64 {
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
        let layout = FrameLayout::new(SavedRegScheme::X86_64, 1, 2);
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
        let layout = FrameLayout::new(SavedRegScheme::Aarch64, 1, 3);
        // saved = 16 + 16 = 32, slots = 24 -> round16(24) = 32; total = 64.
        assert_eq!(layout.frame_size(), 64);
        assert_eq!(layout.slot_offset(0), -32 - 8);
    }
}
