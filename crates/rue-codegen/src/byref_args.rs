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
//! Shared planning produces a [`ByRefAddressPlan`] before this target leaf is
//! called. The plan has already classified the addressable source, materialized
//! every projection index, and selected the checked-place policy. This module
//! only marshals that normalized address through target-neutral place leaves;
//! it does not inspect CFG instructions or re-read projections.

use crate::place_lower::PlaceLowerBackend;
use crate::value_plan::ByRefAddressPlan;
use crate::vreg::VReg;

/// Lower a by-ref (inout/borrow) call argument to the vreg holding its
/// address.
///
/// Sema accepts as a by-ref argument a place: a variable (`Load`/`Param`) or a
/// field/index projection chain rooted at one (`PlaceRead`). A non-place value
/// with no storage to address is a violated sema/CFG invariant (RUE-760).
pub(crate) fn lower_byref_arg_addr<B: PlaceLowerBackend + ?Sized>(
    b: &mut B,
    plan: &ByRefAddressPlan,
) -> VReg {
    match plan {
        ByRefAddressPlan::FrameSlot { slot, low_shift } => {
            let addr_vreg = b.alloc_vreg();
            b.emit_frame_addr(addr_vreg, slot + low_shift);
            addr_vreg
        }
        ByRefAddressPlan::Parameter {
            slot,
            by_ref,
            low_shift,
        } => {
            let addr_vreg = b.alloc_vreg();
            if *by_ref {
                let ptr_vreg = b.ensure_by_ref_param_ptr(*slot);
                b.emit_reg_move(addr_vreg, ptr_vreg);
            } else {
                // Addressing a by-value parameter requires its frame home;
                // the storage plan homes every such parameter (RUE-1170).
                let frame_slot = b.ctx().param_frame_slot(*slot);
                b.emit_frame_addr(addr_vreg, frame_slot + low_shift);
            }
            addr_vreg
        }
        ByRefAddressPlan::Place(place) => {
            let addr_vreg = b.alloc_vreg();
            crate::place_lower::lower_checked_place_addr_plan(b, addr_vreg, place);
            addr_vreg
        }
    }
}
