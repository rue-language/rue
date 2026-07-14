//! Shared by-ref (inout/borrow) call-argument address formation (RUE-143).
//!
//! A by-ref argument is passed as the ADDRESS of a place: a plain variable
//! (`f(inout x)`), a field projection (`f(borrow o.f)`), an array element
//! (`f(inout a[i])`), or a projection through a by-ref parameter the caller
//! itself received (`f(inout p.f)` inside `fn g(inout p: T)`). Historically
//! each backend handled only the plain-variable shapes and panicked on
//! projections ("by-ref argument must be a variable"); sema mirrored the
//! limitation by rejecting projections with the now-retired E0438.
//!
//! Like [`crate::agg_slots`], the decision logic here is target-independent —
//! which argument shapes are addressable, that every index projection is
//! bounds-checked before the address is formed — so backends only provide
//! the leaf operations in [`crate::place_lower::PlaceLowerBackend`]. Bounds checks and
//! projected-place address formation are shared through
//! [`crate::agg_slots::SlotBackend`]
//! (the indexed-place-read materialization in `agg_slots` needs them too,
//! RUE-188). `emit_place_addr` is the same math the backend's
//! `PlaceRead`/`PlaceWrite` lowering uses: it yields the place's LOW-end
//! address, from which aggregate slots ascend (`slot k` at `+k*8`), uniform
//! for frame- and heap-rooted places (ADR-0040 / RUE-311).

use crate::place_lower::PlaceLowerBackend;
use crate::vreg::VReg;
use rue_cfg::{CfgInstData, CfgValue, Place, Projection};

/// Lower a by-ref (inout/borrow) call argument to the vreg holding its
/// address.
///
/// Sema accepts as a by-ref argument a place: a variable (`Load`/`Param`) or a
/// field/index projection chain rooted at one (`PlaceRead`). A non-place value
/// with no storage to address is a violated sema/CFG invariant (RUE-760).
pub fn lower_byref_arg_addr<B: PlaceLowerBackend + ?Sized>(
    b: &mut B,
    arg_value: CfgValue,
    is_inout: bool,
) -> VReg {
    // Extract the small Copy payload first so the CFG borrow ends before we
    // emit (emission needs `&mut B`).
    enum ArgShape {
        Local(u32),
        Param(u32),
        Place(Place),
        NotAPlace,
    }
    // Slot count of the addressed value: a by-ref pointer must point at the
    // place's LOW end (ADR-0040 / RUE-311), which for a frame-resident
    // aggregate is its highest-numbered slot `base + count - 1`.
    let arg_ty = b.ctx().cfg.get_inst(arg_value).ty;
    let low_shift = b.ctx().type_slot_count(arg_ty).saturating_sub(1);
    let shape = match &b.ctx().cfg.get_inst(arg_value).data {
        CfgInstData::Load { slot } => ArgShape::Local(*slot),
        CfgInstData::Param { index } => ArgShape::Param(*index),
        CfgInstData::PlaceRead { place } => ArgShape::Place(*place),
        _ => ArgShape::NotAPlace,
    };

    match shape {
        ArgShape::NotAPlace => {
            unreachable!(
                "malformed CFG: {} argument is not an addressable place",
                if is_inout { "inout" } else { "borrow" }
            )
        }
        ArgShape::Local(slot) => {
            let addr_vreg = b.alloc_vreg();
            b.emit_frame_addr(addr_vreg, slot + low_shift);
            addr_vreg
        }
        ArgShape::Param(index) => {
            let addr_vreg = b.alloc_vreg();
            if b.ctx().cfg.is_param_by_ref(index) {
                // Forwarding a by-ref param: pass along the pointer we
                // received. ensure_by_ref_param_ptr covers params never
                // accessed via a Param instruction.
                let ptr_vreg = b.ensure_by_ref_param_ptr(index);
                b.emit_reg_move(addr_vreg, ptr_vreg);
            } else {
                // Normal param: it lives in a frame slot after the locals.
                let slot = b.ctx().num_locals + index;
                b.emit_frame_addr(addr_vreg, slot + low_shift);
            }
            addr_vreg
        }
        ArgShape::Place(place) => {
            // Bounds-check every index projection BEFORE forming the address:
            // the callee accesses memory through this pointer, so an
            // out-of-bounds index must trap at the call site (the projected
            // PlaceRead the arg expression also lowers to is a dead value —
            // its own bounds check does not protect the address).
            let projections: Vec<Projection> = b.ctx().cfg.get_place_projections(&place).to_vec();
            for proj in &projections {
                if let Projection::Index { array_type, index } = proj {
                    let length = b.ctx().array_length(*array_type);
                    let index_vreg = b.get_vreg(*index);
                    b.emit_bounds_check(index_vreg, length);
                }
            }
            let addr_vreg = b.alloc_vreg();
            b.emit_place_addr(addr_vreg, &place);
            addr_vreg
        }
    }
}
