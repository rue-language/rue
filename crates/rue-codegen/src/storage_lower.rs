//! Shared local and parameter storage lowering (RUE-612).
//!
//! CFG storage operations choose between scalar and flattened aggregate values,
//! frame slots and received by-reference pointers, and eager and lazy aggregate
//! materialization. Those choices are target-independent. Backends retain only
//! recursive struct flattening and the exact base-only pointer-store MIR leaf.

use rue_cfg::CfgValue;

use crate::agg_slots::{self, SlotBackend};
use crate::place_lower::PlaceLowerBackend;
use crate::vreg::VReg;

/// Narrow target-specific leaves used by shared storage lowering.
pub(crate) trait StorageLowerBackend: PlaceLowerBackend {
    /// Store through `ptr` using the backend's base-only MIR form.
    ///
    /// AArch64 intentionally distinguishes `StrIndexed` from
    /// `StrIndexedOffset { offset: 0 }`; preserving that distinction keeps this
    /// extraction byte-for-byte and MIR-for-MIR neutral.
    fn emit_store_ptr_base(&mut self, src: VReg, ptr: VReg);
}

/// Lower a local allocation and its initializer.
pub(crate) fn lower_alloc<B: StorageLowerBackend>(b: &mut B, slot: u32, init: CfgValue) {
    let init_plan = crate::value_plan::ValuePlan::for_value(b.ctx(), init);
    if init_plan.is_strbuf {
        // StrBuf is always the ptr/len/cap three-slot fat pointer. Keep this
        // before generic aggregates so a layout drift fails loudly.
        let field_vregs = agg_slots::require_aggregate_slots(b, init);
        assert_eq!(
            field_vregs.len(),
            3,
            "string should have 3 fields (ptr, len, cap)"
        );
        agg_slots::store_slots(b, &field_vregs, slot);
    } else if init_plan.shape.requires_complete_slots() {
        // Every multi-slot aggregate initializer has one vreg per logical slot.
        let scalar_vregs = agg_slots::require_aggregate_slots(b, init);
        agg_slots::store_slots(b, &scalar_vregs, slot);
    } else if b.ctx().cfg.get_inst(init).ty.is_array() {
        // Zero- and one-slot arrays use their scalar/ZST representation.
        let scalar_vregs = b.collect_array_scalars(init);
        agg_slots::store_slots(b, &scalar_vregs, slot);
    } else {
        let init_vreg = b.get_vreg(init);
        b.emit_store_slot(init_vreg, slot);
    }
}

/// Lower a local load, including flattened aggregate cache bookkeeping.
pub(crate) fn lower_load<B: StorageLowerBackend>(b: &mut B, value: CfgValue, slot: u32) {
    let load_plan = crate::value_plan::ValuePlan::for_value(b.ctx(), value);

    if load_plan.is_strbuf {
        let slot_vregs = agg_slots::load_slots_at_low(b, slot + 2, 3);
        let ptr_vreg = slot_vregs[0];
        b.slot_cache().insert(value, slot_vregs);
        b.map_value(value, ptr_vreg);
    } else if load_plan.shape.requires_complete_slots() {
        let slot_count = load_plan.shape.slot_count();
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

/// Lower a store to either a local frame slot or a by-ref parameter's pointee.
pub(crate) fn lower_store<B: StorageLowerBackend>(b: &mut B, slot: u32, value: CfgValue) {
    let value_plan = crate::value_plan::ValuePlan::for_value(b.ctx(), value);
    let aggregate_vregs = value_plan
        .shape
        .requires_complete_slots()
        .then(|| agg_slots::require_aggregate_slots(b, value));

    if let Some(slot_vregs) = aggregate_vregs {
        if let Some(param_slot) = by_ref_param_for_slot(b, slot) {
            let ptr = b.ensure_by_ref_param_ptr(param_slot);
            agg_slots::store_slots_through_ptr(b, &slot_vregs, ptr, 0);
        } else {
            agg_slots::store_slots(b, &slot_vregs, slot);
        }
    } else {
        let value_vreg = b.get_vreg(value);
        if let Some(param_slot) = by_ref_param_for_slot(b, slot) {
            let ptr = b.ensure_by_ref_param_ptr(param_slot);
            b.emit_store_ptr_base(value_vreg, ptr);
        } else {
            b.emit_store_slot(value_vreg, slot);
        }
    }
}

/// Lower a whole-value assignment through a writable by-ref parameter pointer.
pub(crate) fn lower_param_store<B: StorageLowerBackend>(
    b: &mut B,
    param_slot: u32,
    value: CfgValue,
) {
    if !b.ctx().cfg.is_param_writable(param_slot) {
        panic!("ParamStore used on non-writable param slot {}", param_slot);
    }

    let value_plan = crate::value_plan::ValuePlan::for_value(b.ctx(), value);
    let aggregate_vregs = value_plan
        .shape
        .requires_complete_slots()
        .then(|| agg_slots::require_aggregate_slots(b, value));

    let ptr = b.ensure_by_ref_param_ptr(param_slot);
    if let Some(slot_vregs) = aggregate_vregs {
        agg_slots::store_slots_through_ptr(b, &slot_vregs, ptr, 0);
    } else {
        let value_vreg = b.get_vreg(value);
        b.emit_store_ptr_base(value_vreg, ptr);
    }
}

fn by_ref_param_for_slot<B: SlotBackend>(b: &B, slot: u32) -> Option<u32> {
    b.ctx()
        .slot_to_param_index(slot)
        .filter(|&param_slot| b.ctx().cfg.is_param_by_ref(param_slot))
}
