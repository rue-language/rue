//! Shared aggregate equality lowering.
//!
//! Structural equality for multi-slot aggregates is target-independent at the
//! algorithm level: flatten both operands into storage slots, compare each slot
//! using the leaf type's compare width, AND the per-slot results, and optionally
//! invert for `!=`. Backends provide only the scalar instruction emission.

use rue_cfg::{CfgValue, Type};

use crate::cfg_lower::CfgLowerContext;
use crate::types;
use crate::vreg::VReg;

/// Per-backend scalar operations required by aggregate equality lowering.
pub trait AggregateEqBackend {
    /// The shared lowering context.
    fn ctx(&self) -> &CfgLowerContext<'_>;

    /// Allocate a fresh virtual register.
    fn alloc_vreg(&mut self) -> VReg;

    /// Record `vreg` as the primary vreg for `value`.
    fn map_value(&mut self, value: CfgValue, vreg: VReg);

    /// Return the flattened slot vregs for an aggregate CFG value.
    fn aggregate_slots(&mut self, value: CfgValue) -> Option<Vec<VReg>>;

    /// Emit `dst = value` for boolean values represented as 0/1 integers.
    fn emit_bool_const(&mut self, dst: VReg, value: bool);

    /// Emit `dst = lhs == rhs`, comparing at the target's wide width when
    /// `wide` is true and at the target's normal scalar width otherwise.
    fn emit_slot_eq(&mut self, dst: VReg, lhs: VReg, rhs: VReg, wide: bool);

    /// Emit `acc = acc & rhs`.
    fn emit_bool_and(&mut self, acc: VReg, rhs: VReg);

    /// Emit `value = !value` for a 0/1 boolean vreg.
    fn emit_bool_not(&mut self, value: VReg);
}

/// Emit structural equality or inequality for a multi-slot aggregate.
pub fn emit_aggregate_equality<B: AggregateEqBackend>(
    b: &mut B,
    value: CfgValue,
    lhs: CfgValue,
    rhs: CfgValue,
    ty: Type,
    invert: bool,
) {
    let result_vreg = b.alloc_vreg();
    b.map_value(value, result_vreg);

    let leaf_types = types::aggregate_leaf_types(b.ctx().type_pool, ty);

    if leaf_types.is_empty() {
        // Zero-slot aggregate (empty struct, zero-length array): always equal.
        b.emit_bool_const(result_vreg, !invert);
        return;
    }

    let lhs_slots = b
        .aggregate_slots(lhs)
        .expect("aggregate should have slot vregs");
    let rhs_slots = b
        .aggregate_slots(rhs)
        .expect("aggregate should have slot vregs");
    assert_aggregate_slot_count("lhs", lhs_slots.len(), leaf_types.len());
    assert_aggregate_slot_count("rhs", rhs_slots.len(), leaf_types.len());

    b.emit_bool_const(result_vreg, true);

    for (i, leaf_ty) in leaf_types.iter().enumerate() {
        let cmp_vreg = b.alloc_vreg();
        b.emit_slot_eq(
            cmp_vreg,
            lhs_slots[i],
            rhs_slots[i],
            types::slot_needs_wide_compare(*leaf_ty),
        );
        b.emit_bool_and(result_vreg, cmp_vreg);
    }

    if invert {
        b.emit_bool_not(result_vreg);
    }
}

fn assert_aggregate_slot_count(side: &str, actual: usize, expected: usize) {
    assert_eq!(
        actual, expected,
        "aggregate equality {side} slot count mismatch: value has {actual} slots, type has {expected} leaf slots"
    );
}

#[cfg(test)]
mod tests {
    use super::assert_aggregate_slot_count;

    #[test]
    fn matching_aggregate_slot_count_is_valid() {
        assert_aggregate_slot_count("lhs", 3, 3);
        assert_aggregate_slot_count("rhs", 0, 0);
    }

    #[test]
    #[should_panic(
        expected = "aggregate equality lhs slot count mismatch: value has 2 slots, type has 3 leaf slots"
    )]
    fn mismatched_lhs_aggregate_slot_count_panics() {
        assert_aggregate_slot_count("lhs", 2, 3);
    }

    #[test]
    #[should_panic(
        expected = "aggregate equality rhs slot count mismatch: value has 4 slots, type has 1 leaf slots"
    )]
    fn mismatched_rhs_aggregate_slot_count_panics() {
        assert_aggregate_slot_count("rhs", 4, 1);
    }
}
