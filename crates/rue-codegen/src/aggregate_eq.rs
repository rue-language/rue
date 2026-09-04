//! Shared aggregate equality lowering.
//!
//! The plan preserves semantic leaf comparisons independently of the physical
//! slot carrier. Enum union payload slots are GP bit carriers because their
//! active variant may change register class or FP width; guards select the
//! active variant before its payload comparison contributes to the result.

use rue_air::{FrozenTypeInternPool, Type, TypeKind};

use crate::vreg::VReg;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagGuard {
    pub slot: u32,
    pub discriminant: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqualityLeaf {
    pub slot: u32,
    pub ty: Type,
    pub bit_carrier: bool,
    pub guards: Vec<TagGuard>,
}

pub fn equality_leaves(type_pool: &FrozenTypeInternPool, ty: Type) -> Vec<EqualityLeaf> {
    let mut leaves = Vec::new();
    push_equality_leaves(type_pool, ty, 0, false, &[], &mut leaves);
    leaves
}

fn push_equality_leaves(
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    base: u32,
    bit_carrier: bool,
    guards: &[TagGuard],
    out: &mut Vec<EqualityLeaf>,
) -> u32 {
    match ty.kind() {
        TypeKind::Unit | TypeKind::Never => 0,
        TypeKind::Struct(id) => {
            let mut offset = 0;
            for field in &type_pool.struct_def(id).fields {
                offset += push_equality_leaves(
                    type_pool,
                    field.ty,
                    base + offset,
                    bit_carrier,
                    guards,
                    out,
                );
            }
            offset
        }
        TypeKind::Array(id) => {
            let (element, length) = type_pool.array_def(id);
            let mut offset = 0;
            for _ in 0..length {
                offset += push_equality_leaves(
                    type_pool,
                    element,
                    base + offset,
                    bit_carrier,
                    guards,
                    out,
                );
            }
            offset
        }
        TypeKind::Enum(id) => {
            out.push(EqualityLeaf {
                slot: base,
                ty: Type::I32,
                bit_carrier,
                guards: guards.to_vec(),
            });
            let def = type_pool.enum_def(id);
            let mut max_payload = 0;
            for variant in 0..def.variant_count() {
                let mut variant_guards = guards.to_vec();
                variant_guards.push(TagGuard {
                    slot: base,
                    discriminant: variant as u32,
                });
                let mut offset = 0;
                for &payload_ty in def.variant_payload(variant) {
                    offset += push_equality_leaves(
                        type_pool,
                        payload_ty,
                        base + 1 + offset,
                        true,
                        &variant_guards,
                        out,
                    );
                }
                max_payload = max_payload.max(offset);
            }
            1 + max_payload
        }
        _ => {
            out.push(EqualityLeaf {
                slot: base,
                ty,
                bit_carrier,
                guards: guards.to_vec(),
            });
            1
        }
    }
}

pub trait AggregateEqPlanBackend {
    fn alloc_vreg(&mut self) -> VReg;
    fn emit_bool_const(&mut self, dst: VReg, value: bool);
    fn emit_slot_eq(&mut self, dst: VReg, lhs: VReg, rhs: VReg, leaf_ty: Type, bit_carrier: bool);
    fn emit_tag_eq(&mut self, dst: VReg, tag: VReg, discriminant: u32);
    fn emit_bool_and(&mut self, acc: VReg, rhs: VReg);
    fn emit_bool_not(&mut self, value: VReg);
}

pub fn emit_aggregate_equality_plan<B: AggregateEqPlanBackend>(
    b: &mut B,
    lhs_slots: &[VReg],
    rhs_slots: &[VReg],
    leaves: &[EqualityLeaf],
    invert: bool,
) -> VReg {
    assert_eq!(lhs_slots.len(), rhs_slots.len());
    let result = b.alloc_vreg();
    b.emit_bool_const(result, true);
    for leaf in leaves {
        let slot = leaf.slot as usize;
        assert!(
            slot < lhs_slots.len(),
            "aggregate equality slot out of range"
        );
        let cmp = b.alloc_vreg();
        b.emit_slot_eq(
            cmp,
            lhs_slots[slot],
            rhs_slots[slot],
            leaf.ty,
            leaf.bit_carrier,
        );
        if leaf.guards.is_empty() {
            b.emit_bool_and(result, cmp);
            continue;
        }

        let active = b.alloc_vreg();
        b.emit_bool_const(active, true);
        for guard in &leaf.guards {
            let guard_result = b.alloc_vreg();
            b.emit_tag_eq(
                guard_result,
                lhs_slots[guard.slot as usize],
                guard.discriminant,
            );
            b.emit_bool_and(active, guard_result);
        }
        // (!active || cmp), expressed using the existing boolean primitives.
        b.emit_bool_not(cmp);
        b.emit_bool_and(active, cmp);
        b.emit_bool_not(active);
        b.emit_bool_and(result, active);
    }
    if invert {
        b.emit_bool_not(result);
    }
    result
}
