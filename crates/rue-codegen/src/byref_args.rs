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
//! the leaf operations via [`ByrefAddrBackend`]: take a frame slot's address,
//! fetch a received by-ref pointer, emit a bounds check, and form a projected
//! place's address (the same math the backend's `PlaceRead`/`PlaceWrite`
//! lowering uses: frame base + static field-slot offsets, minus dynamic index
//! offsets; through a by-ref pointer the offsets descend per the ABI
//! convention — caller slots grow downward).

use rue_cfg::{CfgInstData, CfgValue, Place, Projection};

use crate::agg_slots::SlotBackend;
use crate::vreg::VReg;

/// The per-backend leaf operations by-ref address formation needs, on top of
/// the [`SlotBackend`] basics (`ctx`, `alloc_vreg`, `get_vreg`,
/// `emit_reg_move`).
pub trait ByrefAddrBackend: SlotBackend {
    /// Get (or lazily materialize) the pointer vreg of a by-ref (inout or
    /// borrow) parameter of the CURRENT function.
    fn ensure_inout_param_ptr(&mut self, param_slot: u32) -> VReg;

    /// Emit `dst = address of frame slot` (`lea dst, [rbp+off]` /
    /// `add dst, fp, #off`).
    fn emit_frame_addr(&mut self, dst: VReg, slot: u32);

    /// Emit a bounds check trapping when `index_vreg >= length`.
    fn emit_bounds_check(&mut self, index_vreg: VReg, length: u64);

    /// Emit `dst = address of the (possibly projected) place`. Does NOT
    /// bounds-check index projections; the shared logic does that first.
    fn emit_place_addr(&mut self, dst: VReg, place: &Place);
}

/// Lower a by-ref (inout/borrow) call argument to the vreg holding its
/// address.
///
/// Sema guarantees the argument is a place: a variable (`Load`/`Param`) or a
/// field/index projection chain rooted at one (`PlaceRead`). Anything else is
/// a compiler bug, hence the panic.
pub fn lower_byref_arg_addr<B: ByrefAddrBackend + ?Sized>(b: &mut B, arg_value: CfgValue) -> VReg {
    // Extract the small Copy payload first so the CFG borrow ends before we
    // emit (emission needs `&mut B`).
    enum ArgShape {
        Local(u32),
        Param(u32),
        Place(Place),
    }
    let shape = match &b.ctx().cfg.get_inst(arg_value).data {
        CfgInstData::Load { slot } => ArgShape::Local(*slot),
        CfgInstData::Param { index } => ArgShape::Param(*index),
        CfgInstData::PlaceRead { place } => ArgShape::Place(*place),
        other => panic!("by-ref argument must be a place, not {:?}", other),
    };

    match shape {
        ArgShape::Local(slot) => {
            let addr_vreg = b.alloc_vreg();
            b.emit_frame_addr(addr_vreg, slot);
            addr_vreg
        }
        ArgShape::Param(index) => {
            let addr_vreg = b.alloc_vreg();
            if b.ctx().cfg.is_param_inout(index) {
                // Forwarding a by-ref param: pass along the pointer we
                // received. ensure_inout_param_ptr covers params never
                // accessed via a Param instruction.
                let ptr_vreg = b.ensure_inout_param_ptr(index);
                b.emit_reg_move(addr_vreg, ptr_vreg);
            } else {
                // Normal param: it lives in a frame slot after the locals.
                let slot = b.ctx().num_locals + index;
                b.emit_frame_addr(addr_vreg, slot);
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
