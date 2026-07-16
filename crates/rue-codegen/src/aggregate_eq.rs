//! Shared aggregate equality lowering.
//!
//! Structural equality for multi-slot aggregates is target-independent at the
//! algorithm level: flatten both operands into storage slots, compare each slot
//! using the leaf type's compare width, AND the per-slot results, and optionally
//! invert for `!=`. Backends provide only the scalar instruction emission.

use rue_cfg::Type;

use crate::types;
use crate::vreg::VReg;

/// Target hooks used after shared CFG lowering has flattened the operands.
/// This interface deliberately has no CFG handles or type-pool access.
pub trait AggregateEqPlanBackend {
    fn alloc_vreg(&mut self) -> VReg;
    fn emit_bool_const(&mut self, dst: VReg, value: bool);
    fn emit_slot_eq(&mut self, dst: VReg, lhs: VReg, rhs: VReg, wide: bool);
    fn emit_bool_and(&mut self, acc: VReg, rhs: VReg);
    fn emit_bool_not(&mut self, value: VReg);
}

pub fn emit_aggregate_equality_plan<B: AggregateEqPlanBackend>(
    b: &mut B,
    lhs_slots: &[VReg],
    rhs_slots: &[VReg],
    leaf_types: &[Type],
    invert: bool,
) -> VReg {
    assert_eq!(
        lhs_slots.len(),
        leaf_types.len(),
        "normalized aggregate lhs slot count mismatch"
    );
    assert_eq!(
        rhs_slots.len(),
        leaf_types.len(),
        "normalized aggregate rhs slot count mismatch"
    );
    let result = b.alloc_vreg();
    b.emit_bool_const(result, true);
    for ((lhs, rhs), leaf_ty) in lhs_slots.iter().zip(rhs_slots).zip(leaf_types) {
        let cmp = b.alloc_vreg();
        b.emit_slot_eq(cmp, *lhs, *rhs, types::slot_needs_wide_compare(*leaf_ty));
        b.emit_bool_and(result, cmp);
    }
    if invert {
        b.emit_bool_not(result);
    }
    result
}
