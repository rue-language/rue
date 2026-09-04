//! Shared aggregate-slot materialization.
//!
//! Multi-slot aggregates (structs, fixed-size arrays, the 3-slot `StrBuf` fat
//! pointer) are tracked during CFG lowering as a list of one vreg per slot in
//! the lowerer's `struct_slot_vregs` cache. Both backends use this module so
//! every aggregate consumer observes the same representation.
//!
//! This module is the single implementation. The slot *logic* is entirely
//! target-independent — "load N consecutive frame slots", "walk static field
//! projections" — so backends only provide the leaf operations via
//! [`SlotBackend`]: vreg allocation, value lookup, and the one genuinely
//! per-architecture instruction (load a frame slot into a vreg).
//!
//! Shared value planning eagerly populates the slot cache. Every consumer-side
//! aggregate write (Alloc, Store, PlaceWrite, and
//! `inout` writeback) goes through [`store_slots`] or
//! [`store_slots_through_ptr`]. [`store_slots_to_sret`] handles callee-side
//! sret writes; `crate::cfg_lower::type_uses_sret_return` owns the shared
//! convention decision. Physical-register and stack marshalling, sret
//! readback, scheduling, and liveness remain architecture-specific.

use ahash::AHashMap;
use rue_air::Type;
use rue_air::layout::{PaddingRange, SLOT_BYTES};
use rue_cfg::CfgValue;

use crate::allocation::BoundsCheckBackend;
use crate::cfg_lower::CfgLowerContext;
use crate::vreg::VReg;

/// The per-backend leaf operations the shared slot logic needs.
///
/// Implementations are thin: `emit_load_slot` is a single frame-relative load
/// instruction (`mov dst, [rbp+offset]` / `ldr dst, [fp, #offset]`); the rest
/// expose existing lowerer state.
pub(crate) trait SlotBackend: BoundsCheckBackend {
    /// The shared lowering context (CFG, type pool, slot counts).
    fn ctx(&self) -> &CfgLowerContext<'_>;

    /// The aggregate slot cache (`struct_slot_vregs`).
    fn slot_cache(&mut self) -> &mut AHashMap<CfgValue, Vec<VReg>>;

    /// Allocate a fresh virtual register.
    fn alloc_vreg(&mut self) -> VReg;

    fn alloc_float_vreg(&mut self) -> VReg;

    /// Get (or lazily create) the primary vreg for a CFG value.
    fn get_vreg(&mut self, value: CfgValue) -> VReg;

    /// Emit a load of frame slot `slot` into `dst`.
    fn emit_load_slot(&mut self, dst: VReg, slot: u32);

    fn emit_typed_float_load_slot(
        &mut self,
        dst: VReg,
        slot: u32,
        width: crate::value_plan::FloatWidth,
    );

    /// Emit a register-to-register move.
    fn emit_reg_move(&mut self, dst: VReg, src: VReg);

    /// Emit a store of `src` to frame slot `slot`.
    fn emit_store_slot(&mut self, src: VReg, slot: u32);

    fn emit_typed_float_store_slot(
        &mut self,
        src: VReg,
        slot: u32,
        width: crate::value_plan::FloatWidth,
    );

    /// Emit a store of `src` through pointer `ptr` at `byte_offset` from it.
    fn emit_store_through_ptr(&mut self, src: VReg, ptr: VReg, byte_offset: i32);

    /// Emit a load of the value at `byte_offset` from pointer `ptr` into `dst`.
    fn emit_load_through_ptr(&mut self, dst: VReg, ptr: VReg, byte_offset: i32);

    fn emit_float_store_through_ptr(
        &mut self,
        src: VReg,
        ptr: VReg,
        byte_offset: i32,
        width: crate::value_plan::FloatWidth,
    );

    fn emit_float_load_through_ptr(
        &mut self,
        dst: VReg,
        ptr: VReg,
        byte_offset: i32,
        width: crate::value_plan::FloatWidth,
    );

    fn emit_float_to_bits(&mut self, dst: VReg, src: VReg, width: crate::value_plan::FloatWidth);

    fn emit_bits_to_float(&mut self, dst: VReg, src: VReg, width: crate::value_plan::FloatWidth);

    /// Emit a narrow (1/2/4-byte) store of the low `access.width` bytes of `src`
    /// through pointer `ptr` at `byte_offset` from it (RUE-1000).
    fn emit_narrow_store_through_ptr(
        &mut self,
        src: VReg,
        ptr: VReg,
        byte_offset: i32,
        access: crate::types::NarrowScalar,
    );

    /// Emit a narrow (1/2/4-byte) load at `byte_offset` from pointer `ptr` into
    /// `dst`, extended per `access.signed` into the slot-shaped vreg (RUE-1000).
    fn emit_narrow_load_through_ptr(
        &mut self,
        dst: VReg,
        ptr: VReg,
        byte_offset: i32,
        access: crate::types::NarrowScalar,
    );

    /// Allocate a fresh vreg holding the constant zero, used to clear a compact
    /// image's padding bytes (ADR-0052 ruling 5).
    fn emit_zero_vreg(&mut self) -> VReg;

    /// Set the existing vreg `dst` to the constant zero (RUE-1037). Used on a
    /// tag-dispatched image load to clear the payload slots the active variant
    /// does not write, so the loaded value matches the zeroed decomposition of a
    /// shorter variant.
    fn emit_set_zero(&mut self, dst: VReg);

    fn emit_set_float_zero(&mut self, dst: VReg, width: crate::value_plan::FloatWidth);

    /// Allocate a fresh label for a tag-dispatch marshalling edge (RUE-1037).
    fn alloc_marshal_label(&mut self) -> crate::vreg::LabelId;

    /// Compare the discriminant in `tag` against `discriminant` and branch to
    /// `label` when they are NOT equal (RUE-1037): the fall-through path runs the
    /// matched variant's arm, the branch skips to the next arm.
    fn emit_marshal_branch_if_tag_ne(
        &mut self,
        tag: VReg,
        discriminant: u64,
        label: crate::vreg::LabelId,
    );

    /// Emit an unconditional jump to `label` (RUE-1037), used to leave a matched
    /// variant's arm for the dispatch's merge point.
    fn emit_marshal_jump(&mut self, label: crate::vreg::LabelId);

    /// Bind `label` at the current position (RUE-1037).
    fn emit_marshal_label(&mut self, label: crate::vreg::LabelId);
}

/// Zero the padding byte `ranges` of a compact memory image reached through
/// `ptr` (ADR-0052 ruling 5, deterministic zero on construction). Each range is
/// covered by the widest aligned zero stores (8/4/2/1 bytes) from one reused
/// zero vreg; a compact aggregate's padding gaps are each at most `alignment - 1`
/// (< 8) bytes, so this is a handful of stores per image. Empty `ranges` — every
/// gate-off build, and any padding-free type — emit nothing, keeping gate-off
/// code byte-identical.
pub(crate) fn zero_padding_through_ptr<B: SlotBackend>(
    b: &mut B,
    ptr: VReg,
    ranges: &[PaddingRange],
) {
    if ranges.is_empty() {
        return;
    }
    let zero = b.emit_zero_vreg();
    for range in ranges {
        let mut offset = range.start as i32;
        let end = range.end as i32;
        while offset < end {
            let remaining = (end - offset) as u32;
            if remaining >= 8 {
                b.emit_store_through_ptr(zero, ptr, offset);
                offset += SLOT_BYTES as i32;
            } else {
                let width: u8 = if remaining >= 4 {
                    4
                } else if remaining >= 2 {
                    2
                } else {
                    1
                };
                b.emit_narrow_store_through_ptr(
                    zero,
                    ptr,
                    offset,
                    crate::types::NarrowScalar {
                        width,
                        signed: false,
                    },
                );
                offset += i32::from(width);
            }
        }
    }
}

/// Store a whole compact enum value's `vals` (one vreg per internal slot, slot 0
/// first) through `ptr` using the enum's physical slot map (ADR-0052 phase 5.6,
/// RUE-1000). Each slot truncates to its physical width at its compact byte
/// offset: the discriminant to the narrow tag at offset 0, each payload slot to
/// its union field position. `ptr` is the enum's low-end address (`@alloc` /
/// `@raw` / `@ptr_offset`).
pub(crate) fn store_enum_slots_through_ptr<B: SlotBackend>(
    b: &mut B,
    vals: &[VReg],
    ptr: VReg,
    map: &[crate::types::PhysicalEnumSlot],
    padding: &[PaddingRange],
) {
    // Deterministic zero on construction (ADR-0052 ruling 5): clear the image's
    // padding gaps first, then the field stores below overwrite every non-padding
    // byte, so the whole image is initialized regardless of prior memory contents.
    zero_padding_through_ptr(b, ptr, padding);
    for (val, slot) in vals.iter().zip(map.iter()) {
        match (slot.float_width, slot.access) {
            (Some(width), _) if slot.bit_carrier => {
                let fp = b.alloc_float_vreg();
                b.emit_bits_to_float(fp, *val, width);
                b.emit_float_store_through_ptr(fp, ptr, slot.byte_offset, width);
            }
            (Some(width), _) => b.emit_float_store_through_ptr(*val, ptr, slot.byte_offset, width),
            (None, None) => b.emit_store_through_ptr(*val, ptr, slot.byte_offset),
            (None, Some(access)) => {
                b.emit_narrow_store_through_ptr(*val, ptr, slot.byte_offset, access)
            }
        }
    }
}

/// Load a whole compact enum value's internal slots from `ptr` using the enum's
/// physical slot map (ADR-0052 phase 5.6, RUE-1000), returning the vregs in
/// internal slot order. Each slot extends its narrow physical bytes back into the
/// slot-shaped vreg (the discriminant zero-extends; payload slots extend per the
/// widest field's signedness). The counterpart of [`store_enum_slots_through_ptr`].
pub(crate) fn load_enum_slots_through_ptr<B: SlotBackend>(
    b: &mut B,
    ptr: VReg,
    map: &[crate::types::PhysicalEnumSlot],
) -> Vec<VReg> {
    let mut vregs = Vec::with_capacity(map.len());
    for slot in map {
        let dst = if slot.float_width.is_some() && !slot.bit_carrier {
            b.alloc_float_vreg()
        } else {
            b.alloc_vreg()
        };
        match (slot.float_width, slot.access) {
            (Some(width), _) if slot.bit_carrier => {
                let fp = b.alloc_float_vreg();
                b.emit_float_load_through_ptr(fp, ptr, slot.byte_offset, width);
                b.emit_float_to_bits(dst, fp, width);
            }
            (Some(width), _) => b.emit_float_load_through_ptr(dst, ptr, slot.byte_offset, width),
            (None, None) => b.emit_load_through_ptr(dst, ptr, slot.byte_offset),
            (None, Some(access)) => {
                b.emit_narrow_load_through_ptr(dst, ptr, slot.byte_offset, access)
            }
        }
        vregs.push(dst);
    }
    vregs
}

/// Unmarshal a by-value indirect compact aggregate parameter into its frame
/// slots at function entry (ADR-0052 phase 5.8, RUE-1005).
///
/// The callee prologue homed a single incoming pointer to the parameter's base
/// frame slot `base_slot`. This reads that pointer, loads the aggregate's
/// compact memory image through it (each slot extended from its physical width
/// at its compact byte offset), and stores the resulting slot-shaped values back
/// into the parameter's frame region in the ascending layout every consumer
/// reads — so ordinary field projection and whole-value materialization both see
/// the correct decomposition without a special parameter path. The pointer is
/// fully consumed (its loads emitted) before the frame stores overwrite
/// `base_slot`.
pub(crate) fn unmarshal_indirect_value_param<B: SlotBackend>(
    b: &mut B,
    base_slot: u32,
    map: &[crate::types::PhysicalEnumSlot],
) {
    let ptr = b.alloc_vreg();
    b.emit_load_slot(ptr, base_slot);
    let vals = load_enum_slots_through_ptr(b, ptr, map);
    store_slots(b, &vals, base_slot);
}

/// Get or compute the slot vregs for a multi-slot aggregate value
/// (struct, builtin String, or fixed-size array).
///
/// Sources are lowered by the shared value dispatcher. This accessor only
/// retrieves the complete slot vector that the dispatcher cached; it does not
/// inspect raw CFG operations or repeat place, parameter, or bounds policy.
///
/// Returns `None` for non-aggregate types. Valid multi-slot aggregate values
/// are either materialized directly here or are lowered on demand and read
/// from the cache populated by their lowering rule.
pub(crate) fn get_or_compute_field_vregs<B: SlotBackend>(
    b: &mut B,
    value: CfgValue,
) -> Option<Vec<VReg>> {
    if let Some(vregs) = b.slot_cache().get(&value).cloned() {
        return Some(vregs);
    }

    let ty = b.ctx().cfg.get_inst(value).ty;
    if !b.ctx().is_multislot_aggregate(ty) {
        return None;
    }
    b.get_vreg(value);
    b.slot_cache().get(&value).cloned()
}

/// Materialize and return the complete representation of a multi-slot
/// aggregate value.
///
/// Every valid CFG producer of a multi-slot aggregate has exactly one vreg per
/// logical type slot. Consumers must use this total accessor: continuing with
/// a primary vreg alone would emit a partial value. Missing or incorrectly
/// sized representations therefore indicate malformed internal IR and fail an
/// invariant in release builds.
pub(crate) fn require_aggregate_slots<B: SlotBackend>(b: &mut B, value: CfgValue) -> Vec<VReg> {
    let ty = b.ctx().cfg.get_inst(value).ty;
    let plan = crate::value_plan::ValuePlan::for_value(b.ctx(), value);
    assert!(
        plan.shape.requires_complete_slots(),
        "aggregate slot materialization requires a multi-slot aggregate value: {value} (type={ty:?}, shape={:?})",
        plan.shape,
    );
    let expected = plan.shape.slot_count() as usize;
    let slots = get_or_compute_field_vregs(b, value).unwrap_or_else(|| match expected {
        // A zero-slot aggregate's empty representation is complete. Producers
        // intentionally have no slot-cache entry because there are no vregs to
        // record (for example, an ArrayBuf element whose type is a ZST).
        0 => Vec::new(),
        // A one-slot aggregate's primary vreg is its complete representation.
        // Some scalar-producing intrinsics intentionally do not populate the
        // aggregate slot cache for that case.
        1 => vec![b.get_vreg(value)],
        _ => panic!("multi-slot aggregate {value} has no complete slot representation"),
    });
    assert_complete_slot_count(value, slots.len(), expected);
    slots
}

fn assert_complete_slot_count(value: CfgValue, actual: usize, expected: usize) {
    assert_eq!(
        actual, expected,
        "multi-slot aggregate {value} slot count mismatch: representation has {actual} slots, type requires {expected}"
    );
}

/// Pre-allocate the per-slot vregs for an aggregate block parameter.
///
/// Block-parameter lowering is a special case: each backend has already
/// allocated and mapped the parameter's primary vreg before aggregate-slot
/// bookkeeping runs. For multi-slot aggregate params, the slot cache must hold
/// exactly `type_slot_count(ty)` vregs, using the primary vreg for logical slot
/// 0 and freshly-allocated vregs for the remaining slots. Zero-slot aggregates
/// intentionally cache an empty list, matching their source values and keeping
/// join-edge slot-count checks honest (RUE-167, RUE-194, RUE-248).
pub(crate) fn preallocate_block_param_slots<B: SlotBackend>(
    b: &mut B,
    param_value: CfgValue,
    ty: Type,
    primary_vreg: VReg,
) {
    if !b.ctx().is_multislot_aggregate(ty) {
        return;
    }

    let slot_count = b.ctx().type_slot_count(ty);
    let leaf_types = crate::types::aggregate_leaf_types(b.ctx().type_pool, ty);
    let mut slot_vregs = Vec::with_capacity(slot_count as usize);
    if slot_count > 0 {
        slot_vregs.push(primary_vreg);
    }
    for slot in 1..slot_count {
        slot_vregs.push(
            if crate::value_plan::float_width(leaf_types[slot as usize]).is_some() {
                b.alloc_float_vreg()
            } else {
                b.alloc_vreg()
            },
        );
    }
    b.slot_cache().insert(param_value, slot_vregs);
}

/// Store a whole aggregate value's `vals` (one vreg per logical slot, slot 0
/// first) to the frame region beginning at `base_slot`, laid out
/// ASCENDING-in-address (ADR-0040): logical slot 0 at the region's LOWEST
/// address, slot `k` at `base_low_addr + k*8`. Frame slots descend in address
/// (a higher slot number is a lower address), so logical slot `k` is stored at
/// frame slot `base_slot + (len-1) - k` — the region's low end is its
/// highest-numbered slot.
pub(crate) fn store_slots<B: SlotBackend>(b: &mut B, vals: &[VReg], base_slot: u32) {
    let low_slot = base_slot + (vals.len() as u32).saturating_sub(1);
    store_slots_at_low(b, vals, low_slot);
}

/// Store `vals` (logical slot 0 first) into the frame with the value's
/// low-end (slot-0) at frame slot `low_slot`; logical slot `k` lands at frame
/// slot `low_slot - k` (each higher slot at a higher address — ascending,
/// ADR-0040).
pub(crate) fn store_slots_at_low<B: SlotBackend>(b: &mut B, vals: &[VReg], low_slot: u32) {
    for (k, val) in vals.iter().enumerate() {
        b.emit_store_slot(*val, low_slot - k as u32);
    }
}

pub(crate) fn store_slots_typed<B: SlotBackend>(
    b: &mut B,
    vals: &[VReg],
    base_slot: u32,
    leaf_types: &[Type],
) {
    assert_eq!(vals.len(), leaf_types.len());
    let low_slot = base_slot + (vals.len() as u32).saturating_sub(1);
    for (k, (val, ty)) in vals.iter().zip(leaf_types).enumerate() {
        if let Some(width) = crate::value_plan::float_width(*ty) {
            b.emit_typed_float_store_slot(*val, low_slot - k as u32, width);
        } else {
            b.emit_store_slot(*val, low_slot - k as u32);
        }
    }
}

/// Store `vals` (logical slot 0 first) through `ptr` at ASCENDING byte
/// offsets: `vals[k]` goes to `ptr + static_byte_offset + k*8`. `ptr` is the
/// pointee's low-end address (what `@raw` and `@ptr_offset` yield, and where
/// `@alloc` blocks begin), so aggregate slots ascend uniformly for pointers of
/// every origin — stack, heap, or `@int_to_ptr` (ADR-0040 / RUE-311).
pub(crate) fn store_slots_through_ptr<B: SlotBackend>(
    b: &mut B,
    vals: &[VReg],
    ptr: VReg,
    static_byte_offset: i32,
) {
    for (i, val) in vals.iter().enumerate() {
        b.emit_store_through_ptr(
            *val,
            ptr,
            static_byte_offset + (i as i32) * SLOT_BYTES as i32,
        );
    }
}

/// Callee side of the sret return convention (RUE-106): load the sret buffer
/// pointer the prologue saved at the frame slot one past the param area, then
/// store every slot of the return value through it at ascending byte offsets
/// (`vals[i]` to `ptr + i*8`) — the same low-end-ascending convention every
/// aggregate memory access now uses (ADR-0040 / RUE-311).
///
/// See `crate::cfg_lower::type_uses_sret_return` for when returns use sret.
pub(crate) fn store_slots_to_sret<B: SlotBackend>(b: &mut B, vals: &[VReg]) {
    let ptr = b.alloc_vreg();
    let sret_slot = b.ctx().sret_ptr_slot();
    b.emit_load_slot(ptr, sret_slot);
    for (i, val) in vals.iter().enumerate() {
        b.emit_store_through_ptr(*val, ptr, (i as i32) * SLOT_BYTES as i32);
    }
}

/// Compact counterpart of [`store_slots_to_sret`] (ADR-0052 phase 5.7,
/// RUE-1004): load the sret buffer pointer and store the return value's slots as
/// the aggregate's compact memory image — each slot truncated to its physical
/// width at its compact byte offset (`map`) — rather than one eight-byte slot
/// per field. The caller reads the same image back with the matching map.
pub(crate) fn store_slots_to_sret_compact<B: SlotBackend>(
    b: &mut B,
    vals: &[VReg],
    map: &[crate::types::PhysicalEnumSlot],
    padding: &[PaddingRange],
) {
    let ptr = b.alloc_vreg();
    let sret_slot = b.ctx().sret_ptr_slot();
    b.emit_load_slot(ptr, sret_slot);
    store_enum_slots_through_ptr(b, vals, ptr, map, padding);
}

// ============================================================================
// Heterogeneous-enum tag-dispatched image marshalling (ADR-0052 phase 5.12,
// RUE-1037).
//
// A heterogeneous enum (or an aggregate containing one) has no single
// variant-independent slot->byte map, but its value decomposition IS
// variant-independent. So marshalling branches on the runtime tag at each
// dispatched region and runs the ACTIVE variant's leaf stores/loads — reusing
// the same per-leaf narrow/full emission the single-map path uses. A store
// zeros the whole image extent first, then writes the active variant's leaves,
// so every byte outside them is deterministically zero (RUE-1007).
// ============================================================================

/// Store a whole heterogeneous aggregate value's `vals` (one vreg per internal
/// slot, slot 0 first) through `ptr` as its tag-dispatched compact image
/// (RUE-1037). The full image extent is zeroed first, then each `Switch` region
/// branches on the value's tag slot and stores only the matched variant's leaves.
pub(crate) fn store_dispatch_image<B: SlotBackend>(
    b: &mut B,
    vals: &[VReg],
    ptr: VReg,
    image: &crate::types::DispatchImage,
) {
    zero_padding_through_ptr(
        b,
        ptr,
        &[PaddingRange {
            start: 0,
            end: u64::from(image.image_size),
        }],
    );
    store_image_segs(b, vals, ptr, &image.segs, false);
}

fn store_image_segs<B: SlotBackend>(
    b: &mut B,
    vals: &[VReg],
    ptr: VReg,
    segs: &[crate::types::ImageSeg],
    carrier: bool,
) {
    for seg in segs {
        match seg {
            crate::types::ImageSeg::Leaf { slot, phys } => {
                store_leaf(b, vals[*slot as usize], ptr, phys, carrier);
            }
            crate::types::ImageSeg::Switch(region) => {
                let tag = vals[region.tag_slot as usize];
                // The discriminant is a common leaf across every variant.
                b.emit_narrow_store_through_ptr(tag, ptr, region.tag_offset, region.tag_access);
                let end = b.alloc_marshal_label();
                for arm in &region.arms {
                    let next = b.alloc_marshal_label();
                    b.emit_marshal_branch_if_tag_ne(tag, arm.discriminant, next);
                    store_image_segs(b, vals, ptr, &arm.segs, true);
                    b.emit_marshal_jump(end);
                    b.emit_marshal_label(next);
                }
                b.emit_marshal_label(end);
            }
        }
    }
}

fn store_leaf<B: SlotBackend>(
    b: &mut B,
    val: VReg,
    ptr: VReg,
    phys: &crate::types::PhysicalEnumSlot,
    carrier: bool,
) {
    match (phys.float_width, phys.access) {
        (Some(width), _) if carrier || phys.bit_carrier => {
            let fp = b.alloc_float_vreg();
            b.emit_bits_to_float(fp, val, width);
            b.emit_float_store_through_ptr(fp, ptr, phys.byte_offset, width);
        }
        (Some(width), _) => b.emit_float_store_through_ptr(val, ptr, phys.byte_offset, width),
        (None, None) => b.emit_store_through_ptr(val, ptr, phys.byte_offset),
        (None, Some(access)) => b.emit_narrow_store_through_ptr(val, ptr, phys.byte_offset, access),
    }
}

/// Load a whole heterogeneous aggregate value's internal slots from `ptr` as its
/// tag-dispatched compact image (RUE-1037), returning `total_slots` vregs in
/// internal slot order. Each `Switch` region reads the tag, zeros its payload
/// slots, then branches on the tag to load only the matched variant's leaves —
/// so the unused payload slots read back zero, matching the value decomposition.
pub(crate) fn load_dispatch_image<B: SlotBackend>(
    b: &mut B,
    ptr: VReg,
    image: &crate::types::DispatchImage,
) -> Vec<VReg> {
    let mut float_widths = vec![None; image.total_slots as usize];
    collect_dispatch_float_widths(&image.segs, &mut float_widths);
    let result: Vec<VReg> = float_widths
        .iter()
        .map(|width| {
            if width.is_some() {
                b.alloc_float_vreg()
            } else {
                b.alloc_vreg()
            }
        })
        .collect();
    load_image_segs(b, ptr, &result, &float_widths, &image.segs, false);
    result
}

fn collect_dispatch_float_widths(
    segs: &[crate::types::ImageSeg],
    widths: &mut [Option<crate::value_plan::FloatWidth>],
) {
    for seg in segs {
        match seg {
            crate::types::ImageSeg::Leaf { slot, phys } => {
                if phys.bit_carrier {
                    continue;
                }
                let entry = &mut widths[*slot as usize];
                assert!(
                    entry.is_none() || *entry == phys.float_width,
                    "one aggregate slot cannot have conflicting FP widths"
                );
                if phys.float_width.is_some() {
                    *entry = phys.float_width;
                }
            }
            // Slots below a switch are enum union payload bit carriers, so their
            // class cannot be inferred from any one variant's semantic type.
            crate::types::ImageSeg::Switch(_) => {}
        }
    }
}

fn load_image_segs<B: SlotBackend>(
    b: &mut B,
    ptr: VReg,
    result: &[VReg],
    float_widths: &[Option<crate::value_plan::FloatWidth>],
    segs: &[crate::types::ImageSeg],
    carrier: bool,
) {
    for seg in segs {
        match seg {
            crate::types::ImageSeg::Leaf { slot, phys } => {
                load_leaf(b, result[*slot as usize], ptr, phys, carrier);
            }
            crate::types::ImageSeg::Switch(region) => {
                let tag = result[region.tag_slot as usize];
                b.emit_narrow_load_through_ptr(tag, ptr, region.tag_offset, region.tag_access);
                // Zero the payload slots so a shorter variant's unwritten slots
                // read back zero on every path (matching the value model, and
                // giving every result slot a definition on all branches).
                for slot in (region.tag_slot + 1)..(region.tag_slot + region.span) {
                    if let Some(width) = float_widths[slot as usize] {
                        b.emit_set_float_zero(result[slot as usize], width);
                    } else {
                        b.emit_set_zero(result[slot as usize]);
                    }
                }
                let end = b.alloc_marshal_label();
                for arm in &region.arms {
                    let next = b.alloc_marshal_label();
                    b.emit_marshal_branch_if_tag_ne(tag, arm.discriminant, next);
                    load_image_segs(b, ptr, result, float_widths, &arm.segs, true);
                    b.emit_marshal_jump(end);
                    b.emit_marshal_label(next);
                }
                b.emit_marshal_label(end);
            }
        }
    }
}

fn load_leaf<B: SlotBackend>(
    b: &mut B,
    dst: VReg,
    ptr: VReg,
    phys: &crate::types::PhysicalEnumSlot,
    carrier: bool,
) {
    match (phys.float_width, phys.access) {
        (Some(width), _) if carrier || phys.bit_carrier => {
            let fp = b.alloc_float_vreg();
            b.emit_float_load_through_ptr(fp, ptr, phys.byte_offset, width);
            b.emit_float_to_bits(dst, fp, width);
        }
        (Some(width), _) => b.emit_float_load_through_ptr(dst, ptr, phys.byte_offset, width),
        (None, None) => b.emit_load_through_ptr(dst, ptr, phys.byte_offset),
        (None, Some(access)) => b.emit_narrow_load_through_ptr(dst, ptr, phys.byte_offset, access),
    }
}

/// Tag-dispatched counterpart of [`store_slots_to_sret_compact`] (RUE-1037):
/// load the sret buffer pointer and store the return value as its tag-dispatched
/// compact image.
pub(crate) fn store_dispatch_image_to_sret<B: SlotBackend>(
    b: &mut B,
    vals: &[VReg],
    image: &crate::types::DispatchImage,
) {
    let ptr = b.alloc_vreg();
    let sret_slot = b.ctx().sret_ptr_slot();
    b.emit_load_slot(ptr, sret_slot);
    store_dispatch_image(b, vals, ptr, image);
}

/// Tag-dispatched counterpart of [`unmarshal_indirect_value_param`] (RUE-1037):
/// read the homed pointer, load the aggregate's tag-dispatched compact image
/// through it, and store the slot-shaped values into the parameter's frame region.
pub(crate) fn unmarshal_indirect_value_param_dispatch<B: SlotBackend>(
    b: &mut B,
    base_slot: u32,
    image: &crate::types::DispatchImage,
) {
    let ptr = b.alloc_vreg();
    b.emit_load_slot(ptr, base_slot);
    let vals = load_dispatch_image(b, ptr, image);
    store_slots(b, &vals, base_slot);
}

/// Load `count` slots through `ptr` at ASCENDING byte offsets (slot k at
/// `ptr + k*8`), returning the freshly-allocated vregs in logical slot order.
/// The public counterpart of [`store_slots_through_ptr`]: used by `@ptr_read`
/// to read an aggregate pointee through a raw pointer, one 8-byte slot per
/// field (RUE-242). `ptr` is the pointee's low-end address; every value of an
/// aggregate type occupies `type_slot_count` consecutive ascending slots
/// (ADR-0040 / RUE-311).
pub(crate) fn load_slots_through_ptr<B: SlotBackend>(
    b: &mut B,
    ptr: VReg,
    count: u32,
) -> Vec<VReg> {
    load_through_ptr(b, ptr, count)
}

/// Load a whole aggregate value's `count` logical slots from the frame region
/// beginning at `base_slot` (ascending layout, ADR-0040): logical slot 0 is at
/// the region's low end (frame slot `base_slot + count - 1`), slot `k` at
/// `base_slot + count - 1 - k`. Returns the vregs in logical slot order.
/// Load `count` logical slots (slot 0 first) from the frame with the value's
/// low-end (slot 0) at frame slot `low_slot`; logical slot `k` is read from
/// frame slot `low_slot - k` (ascending, ADR-0040).
pub(crate) fn load_slots_at_low_typed<B: SlotBackend>(
    b: &mut B,
    low_slot: u32,
    leaf_types: &[Type],
) -> Vec<VReg> {
    leaf_types
        .iter()
        .enumerate()
        .map(|(k, ty)| {
            if let Some(width) = crate::value_plan::float_width(*ty) {
                let vreg = b.alloc_float_vreg();
                b.emit_typed_float_load_slot(vreg, low_slot - k as u32, width);
                vreg
            } else {
                let vreg = b.alloc_vreg();
                b.emit_load_slot(vreg, low_slot - k as u32);
                vreg
            }
        })
        .collect()
}

/// Load `count` slots through `addr_vreg` at ASCENDING byte offsets (slot k at
/// `addr + k*8`, matching [`store_slots_through_ptr`]), returning the
/// freshly-allocated vregs in logical slot order. `addr_vreg` is the pointee's
/// low-end address (ADR-0040 / RUE-311).
fn load_through_ptr<B: SlotBackend>(b: &mut B, addr_vreg: VReg, count: u32) -> Vec<VReg> {
    let mut vregs = Vec::with_capacity(count as usize);
    for k in 0..count {
        let vreg = b.alloc_vreg();
        b.emit_load_through_ptr(vreg, addr_vreg, (k as i32) * SLOT_BYTES as i32);
        vregs.push(vreg);
    }
    vregs
}

#[cfg(test)]
mod tests {
    use rue_cfg::CfgValue;

    use super::assert_complete_slot_count;

    #[test]
    fn complete_aggregate_slot_count_is_valid() {
        assert_complete_slot_count(CfgValue::from_raw(7), 4, 4);
    }

    #[test]
    fn empty_zero_sized_aggregate_representation_is_valid() {
        assert_complete_slot_count(CfgValue::from_raw(7), 0, 0);
    }

    #[test]
    #[should_panic(
        expected = "multi-slot aggregate v7 slot count mismatch: representation has 1 slots, type requires 4"
    )]
    fn partial_aggregate_slot_representation_panics() {
        assert_complete_slot_count(CfgValue::from_raw(7), 1, 4);
    }
}
