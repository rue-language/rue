//! Shared local and parameter storage lowering (RUE-612).
//!
//! CFG storage operations choose between scalar and flattened aggregate values,
//! frame slots and received inout pointers, and eager and lazy aggregate
//! materialization. Those choices are target-independent. Backends retain only
//! recursive struct flattening and the exact base-only pointer-store MIR leaf.

use rue_cfg::CfgValue;

use crate::agg_slots::{self, SlotBackend};
use crate::place_lower::PlaceLowerBackend;
use crate::vreg::VReg;

/// Narrow target-specific leaves used by shared storage lowering.
pub(crate) trait StorageLowerBackend: PlaceLowerBackend {
    /// Recursively flatten a struct value not modeled by the slot accessor.
    fn collect_struct_scalars(&mut self, value: CfgValue) -> Vec<VReg>;

    /// Store through `ptr` using the backend's base-only MIR form.
    ///
    /// AArch64 intentionally distinguishes `StrIndexed` from
    /// `StrIndexedOffset { offset: 0 }`; preserving that distinction keeps this
    /// extraction byte-for-byte and MIR-for-MIR neutral.
    fn emit_store_ptr_base(&mut self, src: VReg, ptr: VReg);
}

/// Lower a local allocation and its initializer.
pub(crate) fn lower_alloc<B: StorageLowerBackend>(b: &mut B, slot: u32, init: CfgValue) {
    let init_type = b.ctx().cfg.get_inst(init).ty;
    if init_type.is_array() {
        // Materialize lazily-sourced arrays through the accessor and fall back
        // to recursively flattening ArrayInit values.
        let scalar_vregs = agg_slots::get_or_compute_field_vregs(b, init)
            .unwrap_or_else(|| b.collect_array_scalars(init));
        agg_slots::store_slots(b, &scalar_vregs, slot);
    } else if b.ctx().is_builtin_string(init_type) {
        // StrBuf is always the ptr/len/cap three-slot fat pointer. Keep this
        // before generic structs so a layout drift fails loudly.
        let field_vregs = agg_slots::get_or_compute_field_vregs(b, init)
            .expect("string should have fat pointer fields in Alloc");
        assert_eq!(
            field_vregs.len(),
            3,
            "string should have 3 fields (ptr, len, cap)"
        );
        agg_slots::store_slots(b, &field_vregs, slot);
    } else if b.ctx().is_multislot_aggregate(init_type) {
        // Struct or payload enum: the accessor handles cached and lazy values;
        // retain the recursive fallback for sources it does not model.
        let scalar_vregs = agg_slots::get_or_compute_field_vregs(b, init)
            .unwrap_or_else(|| b.collect_struct_scalars(init));
        agg_slots::store_slots(b, &scalar_vregs, slot);
    } else {
        let init_vreg = b.get_vreg(init);
        b.emit_store_slot(init_vreg, slot);
    }
}

/// Lower a local load, including flattened aggregate cache bookkeeping.
pub(crate) fn lower_load<B: StorageLowerBackend>(b: &mut B, value: CfgValue, slot: u32) {
    let load_type = b.ctx().cfg.get_inst(value).ty;

    if b.ctx().is_builtin_string(load_type) {
        let slot_vregs = agg_slots::load_slots_at_low(b, slot + 2, 3);
        let ptr_vreg = slot_vregs[0];
        b.slot_cache().insert(value, slot_vregs);
        b.map_value(value, ptr_vreg);
    } else if load_type.is_array() || b.ctx().is_multislot_aggregate(load_type) {
        let slot_count = b.ctx().type_slot_count(load_type);
        let slot_vregs = if slot_count > 0 {
            agg_slots::load_slots_at_low(b, slot + slot_count - 1, slot_count)
        } else {
            Vec::new()
        };

        b.slot_cache().insert(value, slot_vregs.clone());
        if let Some(&primary) = slot_vregs.first() {
            b.map_value(value, primary);
        } else {
            let primary = b.alloc_vreg();
            b.map_value(value, primary);
        }
    } else {
        let vreg = b.alloc_vreg();
        b.map_value(value, vreg);
        b.emit_load_slot(vreg, slot);
    }
}

/// Lower a store to either a local frame slot or an inout parameter's pointee.
pub(crate) fn lower_store<B: StorageLowerBackend>(b: &mut B, slot: u32, value: CfgValue) {
    let value_type = b.ctx().cfg.get_inst(value).ty;
    let aggregate_vregs = if b.ctx().is_multislot_aggregate(value_type) {
        agg_slots::get_or_compute_field_vregs(b, value)
    } else {
        None
    };

    if let Some(slot_vregs) = aggregate_vregs {
        if let Some(param_slot) = inout_param_for_slot(b, slot) {
            let ptr = b.ensure_inout_param_ptr(param_slot);
            agg_slots::store_slots_through_ptr(b, &slot_vregs, ptr, 0);
        } else {
            agg_slots::store_slots(b, &slot_vregs, slot);
        }
    } else {
        let value_vreg = b.get_vreg(value);
        if let Some(param_slot) = inout_param_for_slot(b, slot) {
            let ptr = b.ensure_inout_param_ptr(param_slot);
            b.emit_store_ptr_base(value_vreg, ptr);
        } else {
            b.emit_store_slot(value_vreg, slot);
        }
    }
}

/// Lower a whole-value assignment through an inout parameter pointer.
pub(crate) fn lower_param_store<B: StorageLowerBackend>(
    b: &mut B,
    param_slot: u32,
    value: CfgValue,
) {
    if !b.ctx().cfg.is_param_inout(param_slot) {
        panic!("ParamStore used on non-inout param slot {}", param_slot);
    }

    let value_type = b.ctx().cfg.get_inst(value).ty;
    let aggregate_vregs = if b.ctx().is_multislot_aggregate(value_type) {
        agg_slots::get_or_compute_field_vregs(b, value)
    } else {
        None
    };

    let ptr = b.ensure_inout_param_ptr(param_slot);
    if let Some(slot_vregs) = aggregate_vregs {
        agg_slots::store_slots_through_ptr(b, &slot_vregs, ptr, 0);
    } else {
        let value_vreg = b.get_vreg(value);
        b.emit_store_ptr_base(value_vreg, ptr);
    }
}

fn inout_param_for_slot<B: SlotBackend>(b: &B, slot: u32) -> Option<u32> {
    b.ctx()
        .slot_to_inout_param_index(slot)
        .filter(|&param_slot| b.ctx().cfg.is_param_inout(param_slot))
}
