//! Shared allocation, index-scaling, and bounds-check policy.
//!
//! This module owns the facts that must not be rediscovered by an instruction
//! selector: every ABI slot is eight bytes, a pointer's element width is the
//! pointee's aggregate slot width, index scaling is unsigned byte arithmetic,
//! and array bounds use an unsigned `index < length` condition.  Backends only
//! lower the resulting plans to their arithmetic, compare, branch, and trap
//! instructions.

use rue_air::{FrozenTypeInternPool, Type, TypeKind};

use crate::types;
use crate::vreg::{LabelId, VReg};

/// The byte width of one logical ABI/storage slot.
pub const SLOT_BYTES: u64 = 8;

/// The semantic purpose of a scaling operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalePurpose {
    /// Scaling an array projection index before forming an address.
    IndexOffset,
    /// Scaling a raw pointer offset before adding it to a pointer.
    PointerOffset,
    /// Scaling an allocation count for a runtime heap call.
    AllocationSize,
}

/// Whether a scaled product is allowed to wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowBehavior {
    /// Trap if the 64-bit byte product overflows.
    Trap,
    /// Preserve the raw pointer arithmetic's wrapping behavior.
    Wrap,
}

/// The normalized constant multiplier selected by shared policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleKind {
    /// A zero-sized element; the result is always zero.
    Zero,
    /// A one-byte element; the source can be copied unchanged.
    Identity,
    /// A nontrivial constant byte multiplier.
    Constant(u64),
}

/// A complete target-neutral scaling plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalePlan {
    pub kind: ScaleKind,
    pub purpose: ScalePurpose,
    pub overflow: OverflowBehavior,
}

/// A type's logical storage width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotWidth {
    pub slots: u32,
    pub bytes: u64,
}

impl SlotWidth {
    #[inline]
    pub const fn from_slots(slots: u32) -> Self {
        Self {
            slots,
            bytes: slots as u64 * SLOT_BYTES,
        }
    }
}

/// Compute the canonical aggregate/storage width for `ty`.
#[inline]
pub fn type_width(type_pool: &FrozenTypeInternPool, ty: Type) -> SlotWidth {
    SlotWidth::from_slots(types::type_slot_count(type_pool, ty))
}

/// Compute the canonical element width for a pointer type.
///
/// Sema guarantees the pointer shape for allocation and pointer intrinsics.
/// The defensive scalar fallback preserves the old error-path behavior if a
/// malformed CFG reaches code generation.
#[inline]
pub fn pointer_element_width(type_pool: &FrozenTypeInternPool, ptr_ty: Type) -> SlotWidth {
    let pointee = match ptr_ty.kind() {
        TypeKind::PtrMut(id) => type_pool.ptr_mut_def(id),
        TypeKind::PtrConst(id) => type_pool.ptr_const_def(id),
        _ => return SlotWidth::from_slots(1),
    };
    type_width(type_pool, pointee)
}

/// Select the shared constant scaling operation for a byte width.
#[inline]
pub const fn scale_kind(bytes: u64) -> ScaleKind {
    match bytes {
        0 => ScaleKind::Zero,
        1 => ScaleKind::Identity,
        bytes => ScaleKind::Constant(bytes),
    }
}

/// Build the scaling plan used for an array projection.
#[inline]
pub fn index_scale_plan(type_pool: &FrozenTypeInternPool, array_type: Type) -> ScalePlan {
    let (element_type, _) =
        types::array_type_def_from_type(type_pool, array_type).unwrap_or((array_type, 0));
    ScalePlan {
        kind: scale_kind(type_width(type_pool, element_type).bytes),
        purpose: ScalePurpose::IndexOffset,
        overflow: OverflowBehavior::Wrap,
    }
}

/// Build the scaling plan used by `@ptr_offset`.
#[inline]
pub fn pointer_offset_scale_plan(type_pool: &FrozenTypeInternPool, ptr_ty: Type) -> ScalePlan {
    ScalePlan {
        kind: scale_kind(pointer_element_width(type_pool, ptr_ty).bytes),
        purpose: ScalePurpose::PointerOffset,
        overflow: OverflowBehavior::Wrap,
    }
}

/// Build the checked scaling plan used by `@alloc`, `@free`, and `@realloc`.
#[inline]
pub fn allocation_size_scale_plan(type_pool: &FrozenTypeInternPool, ptr_ty: Type) -> ScalePlan {
    let width = pointer_element_width(type_pool, ptr_ty);
    debug_assert!(
        checked_byte_size(1, width.bytes).is_some(),
        "canonical slot width must fit in a 64-bit byte size"
    );
    ScalePlan {
        kind: scale_kind(width.bytes),
        purpose: ScalePurpose::AllocationSize,
        overflow: OverflowBehavior::Trap,
    }
}

/// Compute the product used by a checked allocation-size calculation.
#[inline]
pub const fn checked_byte_size(count: u64, element_bytes: u64) -> Option<u64> {
    count.checked_mul(element_bytes)
}

/// The only bounds condition currently produced by the language-level policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundsCondition {
    /// The index is valid exactly when it is below the array length as an
    /// unsigned 64-bit value. This also rejects negative signed indices after
    /// their canonical extension to the index width.
    UnsignedIndexLessThanLength,
}

/// The normalized trap edge for a bounds failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundsTrap {
    IndexOutOfBounds,
}

/// A complete target-neutral bounds-check plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundsCheckPlan {
    pub index: VReg,
    pub length: u64,
    pub condition: BoundsCondition,
    pub trap: BoundsTrap,
}

impl BoundsCheckPlan {
    #[inline]
    pub const fn new(index: VReg, length: u64) -> Self {
        Self {
            index,
            length,
            condition: BoundsCondition::UnsignedIndexLessThanLength,
            trap: BoundsTrap::IndexOutOfBounds,
        }
    }
}

/// Target leaves used by the shared bounds-check edge builder.
pub trait BoundsCheckBackend {
    fn alloc_bounds_length(&mut self, length: u64) -> VReg;
    fn emit_bounds_compare(&mut self, index: VReg, length: VReg);
    fn alloc_bounds_label(&mut self) -> LabelId;
    fn emit_bounds_branch(&mut self, condition: BoundsCondition, label: LabelId);
    fn emit_bounds_trap(&mut self, trap: BoundsTrap);
    fn emit_bounds_label(&mut self, label: LabelId);
}

/// Lower one normalized bounds check. The condition, failure edge, and edge
/// order are shared; only the individual target operations are supplied by the
/// backend.
pub fn lower_bounds_check<B: BoundsCheckBackend + ?Sized>(b: &mut B, plan: BoundsCheckPlan) {
    let length = b.alloc_bounds_length(plan.length);
    b.emit_bounds_compare(plan.index, length);
    let ok = b.alloc_bounds_label();
    b.emit_bounds_branch(plan.condition, ok);
    b.emit_bounds_trap(plan.trap);
    b.emit_bounds_label(ok);
}

/// Target leaf for a normalized scaling operation.
pub trait ScaleBackend {
    fn alloc_scale_result(&mut self) -> VReg;
    fn emit_scale(&mut self, dst: VReg, src: VReg, plan: ScalePlan);
}

/// Lower a target-neutral scaling plan to one backend result vreg.
#[inline]
pub fn lower_scale<B: ScaleBackend + ?Sized>(b: &mut B, src: VReg, plan: ScalePlan) -> VReg {
    let dst = b.alloc_scale_result();
    b.emit_scale(dst, src, plan);
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_policy_covers_zero_identity_and_constant_widths() {
        assert_eq!(scale_kind(0), ScaleKind::Zero);
        assert_eq!(scale_kind(1), ScaleKind::Identity);
        assert_eq!(scale_kind(8), ScaleKind::Constant(8));
        assert_eq!(
            scale_kind(u32::MAX as u64 * SLOT_BYTES),
            ScaleKind::Constant(u32::MAX as u64 * 8)
        );
    }

    #[test]
    fn checked_allocation_size_rejects_maximum_length_overflow() {
        assert_eq!(checked_byte_size(u64::MAX, 0), Some(0));
        assert_eq!(checked_byte_size(u64::MAX, 1), Some(u64::MAX));
        assert_eq!(checked_byte_size(u64::MAX, 8), None);
        assert_eq!(checked_byte_size(u64::MAX / 8, 8), Some(u64::MAX - 7));
    }

    #[test]
    fn bounds_policy_is_unsigned_less_than_with_a_trap_edge() {
        let plan = BoundsCheckPlan::new(VReg::new(3), u64::MAX);
        assert_eq!(plan.condition, BoundsCondition::UnsignedIndexLessThanLength);
        assert_eq!(plan.trap, BoundsTrap::IndexOutOfBounds);
        assert_eq!(plan.index, VReg::new(3));
    }
}
