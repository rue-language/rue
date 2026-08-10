//! Shared allocation, index-scaling, and bounds-check policy.
//!
//! This module owns the facts that must not be rediscovered by an instruction
//! selector: a pointer's element width is the pointee's canonical layout size,
//! index scaling is unsigned byte arithmetic, and array bounds use an unsigned
//! `index < length` condition.  Byte sizes come from the canonical layout
//! authority ([`rue_air::layout`]); backends only lower the resulting plans to
//! their arithmetic, compare, branch, and trap instructions.

use rue_air::{FrozenTypeInternPool, Type, TypeKind};
use rue_runtime_abi::RuntimeHelperId;

use crate::types;
use crate::vreg::{LabelId, VReg};

/// The byte width of one logical ABI/storage slot, re-exported from the
/// canonical layout authority.
pub use rue_air::layout::SLOT_BYTES;

/// The semantic purpose of a scaling operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalePurpose {
    /// Scaling an array projection index before forming an address.
    IndexOffset,
    /// Scaling a raw pointer offset before adding it to a pointer.
    PointerOffset,
    /// Scaling an element count into a trapping byte product.
    ///
    /// No plan carries this purpose today: the allocation family became
    /// byte-shaped in ADR-0059 Phase 3 (RUE-961), so `count * @size_of(T)` is
    /// ordinary source arithmetic whose overflow the language already traps
    /// (§8.1), not a codegen-private multiply. The purpose and both backends'
    /// checked-multiply lowering are retained as the one place a trapping
    /// scale is expressed, should another consumer need it.
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
        let bytes = checked_byte_size(slots as u64, SLOT_BYTES)
            .expect("canonical slot width must fit in a 64-bit byte size");
        Self { slots, bytes }
    }
}

/// Compute the canonical aggregate/storage width for `ty`.
///
/// The slot count is the internal value decomposition; the byte size is the
/// canonical physical layout. They agree by construction (`bytes == slots *
/// SLOT_BYTES`).
#[inline]
pub fn type_width(type_pool: &FrozenTypeInternPool, ty: Type) -> SlotWidth {
    SlotWidth {
        slots: types::type_slot_count(type_pool, ty),
        bytes: type_pool.layout(ty).size,
    }
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

/// Build the scaling plan used for an array `[]` projection.
///
/// Array values are stored slot-shaped in the frame (RUE-975), so element
/// addressing strides by the *slot* stride — `abi_slot_count(element) *
/// SLOT_BYTES` — not the compact element size. Under the slot model the two are
/// identical (every leaf is eight bytes); under the compact layout (ADR-0052)
/// they diverge, and the slot stride is the physically correct one because
/// `[]`-indexed arrays only ever address slot-based storage (a frame value or a
/// by-reference pointer into the caller's slot-based frame). Heap element
/// stepping uses `@ptr_offset` (`pointer_offset_scale_plan`), which strides by
/// the compact element size against a compact heap image (RUE-1014).
#[inline]
pub fn index_scale_plan(type_pool: &FrozenTypeInternPool, array_type: Type) -> ScalePlan {
    let (element_type, _) =
        types::array_type_def_from_type(type_pool, array_type).unwrap_or((array_type, 0));
    let slot_stride = u64::from(types::type_slot_count(type_pool, element_type)) * SLOT_BYTES;
    ScalePlan {
        kind: scale_kind(slot_stride),
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

/// Runtime entries selected by shared language-level trap policy. Adapters
/// receive these typed identities and only resolve symbols when building MIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeTrapHelpers {
    pub bounds: RuntimeHelperId,
    pub overflow: RuntimeHelperId,
    pub div_by_zero: RuntimeHelperId,
    pub intcast_overflow: RuntimeHelperId,
}

pub const RUNTIME_TRAP_HELPERS: RuntimeTrapHelpers = RuntimeTrapHelpers {
    bounds: RuntimeHelperId::BoundsCheck,
    overflow: RuntimeHelperId::Overflow,
    div_by_zero: RuntimeHelperId::DivByZero,
    intcast_overflow: RuntimeHelperId::IntcastOverflow,
};

/// A complete target-neutral bounds-check plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundsCheckPlan {
    pub index: VReg,
    pub length: u64,
    pub condition: BoundsCondition,
    pub trap: BoundsTrap,
    pub trap_call: crate::runtime_call_plan::RuntimeCallPlan,
}

impl BoundsCheckPlan {
    #[inline]
    pub fn new(index: VReg, length: u64) -> Self {
        Self {
            index,
            length,
            condition: BoundsCondition::UnsignedIndexLessThanLength,
            trap: BoundsTrap::IndexOutOfBounds,
            trap_call: crate::runtime_call_plan::RuntimeCallPlan::no_args(
                RUNTIME_TRAP_HELPERS.bounds,
            ),
        }
    }
}

/// Target leaves used by the shared bounds-check edge builder.
pub trait BoundsCheckBackend {
    fn alloc_bounds_length(&mut self, length: u64) -> VReg;
    fn emit_bounds_compare(&mut self, index: VReg, length: VReg);
    fn alloc_bounds_label(&mut self) -> LabelId;
    fn emit_bounds_branch(&mut self, condition: BoundsCondition, label: LabelId);
    fn emit_bounds_trap(
        &mut self,
        trap: BoundsTrap,
        call: crate::runtime_call_plan::RuntimeCallPlan,
    );
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
    b.emit_bounds_trap(plan.trap, plan.trap_call);
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
