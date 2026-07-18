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

use std::collections::HashMap;

use rue_air::Type;
use rue_air::layout::SLOT_BYTES;
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
    fn slot_cache(&mut self) -> &mut HashMap<CfgValue, Vec<VReg>>;

    /// Allocate a fresh virtual register.
    fn alloc_vreg(&mut self) -> VReg;

    /// Get (or lazily create) the primary vreg for a CFG value.
    fn get_vreg(&mut self, value: CfgValue) -> VReg;

    /// Emit a load of frame slot `slot` into `dst`.
    fn emit_load_slot(&mut self, dst: VReg, slot: u32);

    /// Emit a register-to-register move.
    fn emit_reg_move(&mut self, dst: VReg, src: VReg);

    /// Emit a store of `src` to frame slot `slot`.
    fn emit_store_slot(&mut self, src: VReg, slot: u32);

    /// Emit a store of `src` through pointer `ptr` at `byte_offset` from it.
    fn emit_store_through_ptr(&mut self, src: VReg, ptr: VReg, byte_offset: i32);

    /// Emit a load of the value at `byte_offset` from pointer `ptr` into `dst`.
    fn emit_load_through_ptr(&mut self, dst: VReg, ptr: VReg, byte_offset: i32);

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
) {
    for (val, slot) in vals.iter().zip(map.iter()) {
        match slot.access {
            None => b.emit_store_through_ptr(*val, ptr, slot.byte_offset),
            Some(access) => b.emit_narrow_store_through_ptr(*val, ptr, slot.byte_offset, access),
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
        let dst = b.alloc_vreg();
        match slot.access {
            None => b.emit_load_through_ptr(dst, ptr, slot.byte_offset),
            Some(access) => b.emit_narrow_load_through_ptr(dst, ptr, slot.byte_offset, access),
        }
        vregs.push(dst);
    }
    vregs
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
    let mut slot_vregs = Vec::with_capacity(slot_count as usize);
    if slot_count > 0 {
        slot_vregs.push(primary_vreg);
    }
    for _ in 1..slot_count {
        slot_vregs.push(b.alloc_vreg());
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
) {
    let ptr = b.alloc_vreg();
    let sret_slot = b.ctx().sret_ptr_slot();
    b.emit_load_slot(ptr, sret_slot);
    store_enum_slots_through_ptr(b, vals, ptr, map);
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
pub(crate) fn load_slots_at_low<B: SlotBackend>(b: &mut B, low_slot: u32, count: u32) -> Vec<VReg> {
    let mut vregs = Vec::with_capacity(count as usize);
    for k in 0..count {
        let vreg = b.alloc_vreg();
        b.emit_load_slot(vreg, low_slot - k);
        vregs.push(vreg);
    }
    vregs
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
