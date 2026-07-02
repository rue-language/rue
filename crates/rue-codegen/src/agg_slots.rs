//! Shared aggregate-slot materialization (RUE-121, phase 1).
//!
//! Multi-slot aggregates (structs, fixed-size arrays, the 3-slot `String` fat
//! pointer) are tracked during CFG lowering as a list of one vreg per slot in
//! the lowerer's `struct_slot_vregs` cache. Historically each backend carried
//! its own hand-mirrored copy of the logic that produces that list, and the
//! copies drifted — several confirmed miscompiles (the RUE-118 family, the
//! `is_struct`-before-`is_builtin_string` ordering bug) were pure drift
//! between the two `cfg_lower.rs` files.
//!
//! This module is the single implementation. The slot *logic* is entirely
//! target-independent — "load N consecutive frame slots", "walk static field
//! projections" — so backends only provide the leaf operations via
//! [`SlotBackend`]: vreg allocation, value lookup, and the one genuinely
//! per-architecture instruction (load a frame slot into a vreg).
//!
//! Phase 1 covered the slot accessor. Phase 2 moved the StructInit/ArrayInit
//! flattening (the eager population of the slot cache) here as
//! [`lower_struct_init`]/[`lower_array_init`]. Phase 3 moved the consumer-side
//! store loops here as [`store_slots`]/[`store_slots_through_ptr`] — every
//! site that writes an aggregate's slots (Alloc, Store, PlaceWrite, inout
//! writeback) iterates via these two primitives. Phase 4 (RUE-106) added the
//! callee side of the sret return as [`store_slots_to_sret`]; the convention
//! *decision* (which returns use sret) is shared in
//! `crate::cfg_lower::type_uses_sret_return`. Remaining per-backend: the
//! physical-register/stack marshalling of call args and sret readback (truly
//! arch-specific: push vs str, rsp vs sp) and the scheduler/liveness tables.

use std::collections::HashMap;

use rue_air::TypeKind;
use rue_cfg::{CfgInstData, CfgValue, Place, PlaceBase, Projection};

use crate::cfg_lower::CfgLowerContext;
use crate::vreg::VReg;

/// The per-backend leaf operations the shared slot logic needs.
///
/// Implementations are thin: `emit_load_slot` is a single frame-relative load
/// instruction (`mov dst, [rbp+offset]` / `ldr dst, [fp, #offset]`); the rest
/// expose existing lowerer state.
pub trait SlotBackend {
    /// The shared lowering context (CFG, type pool, slot counts).
    fn ctx(&self) -> &CfgLowerContext<'_>;

    /// The aggregate slot cache (`struct_slot_vregs`).
    fn slot_cache(&mut self) -> &mut HashMap<CfgValue, Vec<VReg>>;

    /// Allocate a fresh virtual register.
    fn alloc_vreg(&mut self) -> VReg;

    /// Get (or lazily create) the primary vreg for a CFG value.
    fn get_vreg(&mut self, value: CfgValue) -> VReg;

    /// Record `vreg` as the primary vreg for `value` (the `value_map` entry).
    fn map_value(&mut self, value: CfgValue, vreg: VReg);

    /// Emit a load of frame slot `slot` into `dst`.
    fn emit_load_slot(&mut self, dst: VReg, slot: u32);

    /// Emit a register-to-register move.
    fn emit_reg_move(&mut self, dst: VReg, src: VReg);

    /// Emit a load of the constant zero into `dst`.
    fn emit_load_zero(&mut self, dst: VReg);

    /// Recursively collect the scalar slot vregs of an array value
    /// (the backends' thin wrapper over `types::collect_array_scalar_vregs`).
    fn collect_array_scalars(&mut self, value: CfgValue) -> Vec<VReg>;

    /// Emit a store of `src` to frame slot `slot`.
    fn emit_store_slot(&mut self, src: VReg, slot: u32);

    /// Emit a store of `src` through pointer `ptr` at `byte_offset` from it.
    fn emit_store_through_ptr(&mut self, src: VReg, ptr: VReg, byte_offset: i32);

    /// Emit a load of the value at `byte_offset` from pointer `ptr` into `dst`.
    fn emit_load_through_ptr(&mut self, dst: VReg, ptr: VReg, byte_offset: i32);

    /// Emit a bounds check trapping when `index_vreg >= length`.
    fn emit_bounds_check(&mut self, index_vreg: VReg, length: u64);

    /// Emit `dst = address of the (possibly projected) place` — the backend's
    /// `lower_place_addr` (frame base + static field-slot offsets, minus
    /// dynamic index offsets; through a by-ref pointer per the descending ABI
    /// convention). Does NOT bounds-check index projections; callers do that
    /// first.
    fn emit_place_addr(&mut self, dst: VReg, place: &Place);
}

/// Get or compute the slot vregs for a multi-slot aggregate value
/// (struct, builtin String, or fixed-size array).
///
/// Sources handled:
/// - cache hit (StructInit/ArrayInit/Call/BlockParam populate eagerly)
/// - `StructInit`: the field values' vregs directly
/// - `Load`: load `slot_count` consecutive stack slots
/// - `Param`: load from the parameter slot area (by-ref params load through
///   the received pointer instead — the frame slot holds an address)
/// - `PlaceRead` with only static field projections on a frame-slot base:
///   load from the field's consecutive slots
/// - `PlaceRead` with index projections (constant or dynamic, `arr[i]`,
///   `s.rows[i]`) or a by-ref param base: bounds-check every index, form the
///   place's address, and load all `slot_count` slots through it (RUE-188)
///
/// Returns `None` for non-aggregate types and unmodeled sources.
pub fn get_or_compute_field_vregs<B: SlotBackend>(b: &mut B, value: CfgValue) -> Option<Vec<VReg>> {
    // Check cache first
    if let Some(vregs) = b.slot_cache().get(&value).cloned() {
        return Some(vregs);
    }

    let (ty, data) = {
        let inst = b.ctx().cfg.get_inst(value);
        (inst.ty, inst.data.clone())
    };
    if !matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_)) {
        return None;
    }

    match data {
        CfgInstData::StructInit {
            fields_start,
            fields_len,
            ..
        } => {
            let fields = b.ctx().cfg.get_extra(fields_start, fields_len).to_vec();
            Some(fields.iter().map(|f| b.get_vreg(*f)).collect())
        }
        CfgInstData::Load { slot } => {
            let slot_count = b.ctx().type_slot_count(ty);
            Some(load_consecutive(b, slot, slot_count))
        }
        CfgInstData::Param { index } => {
            let slot_count = b.ctx().type_slot_count(ty);
            if b.ctx().cfg.is_param_inout(index) {
                // By-ref (inout/borrow) param: the frame slot holds a POINTER
                // to the caller's storage, not the value — load the slots
                // through it (emit_place_addr fetches the received pointer).
                let addr_vreg = b.alloc_vreg();
                b.emit_place_addr(addr_vreg, &Place::param(index));
                Some(load_through_ptr(b, addr_vreg, slot_count))
            } else {
                let base_slot = b.ctx().num_locals + index;
                Some(load_consecutive(b, base_slot, slot_count))
            }
        }
        CfgInstData::PlaceRead { place } => {
            // A multi-slot aggregate read from a place — e.g. `let s2 = h.s;`
            // or `let row = m[i];`. The base place-read lowering only
            // materializes the first slot; here we materialize all of them so
            // consumers see the full value. (RUE-22/63/94, RUE-118, RUE-188)
            let projections = b.ctx().cfg.get_place_projections(&place).to_vec();
            let has_index = projections
                .iter()
                .any(|p| matches!(p, Projection::Index { .. }));
            let is_by_ref_base = matches!(
                place.base,
                PlaceBase::Param(param_slot) if b.ctx().cfg.is_param_inout(param_slot)
            );
            let slot_count = b.ctx().type_slot_count(ty);

            if !has_index && !is_by_ref_base {
                // Purely static projection chain rooted at a frame slot:
                // load the field's consecutive slots directly.
                let mut static_slot_offset: u32 = 0;
                for proj in &projections {
                    match proj {
                        Projection::Field {
                            struct_id,
                            field_index,
                        } => {
                            static_slot_offset +=
                                b.ctx().struct_field_slot_offset(*struct_id, *field_index);
                        }
                        Projection::Index { .. } => unreachable!("has_index is false"),
                    }
                }
                let base_slot = match place.base {
                    PlaceBase::Local(slot) => slot + static_slot_offset,
                    PlaceBase::Param(param_slot) => {
                        b.ctx().num_locals + param_slot + static_slot_offset
                    }
                };
                return Some(load_consecutive(b, base_slot, slot_count));
            }

            // Indexed element (`arr[i]`, `s.rows[i]` — constant or dynamic
            // index) or a projection through a by-ref param: bounds-check
            // every index projection, form the place's address, then load
            // each slot through it. Slot i lives at addr - i*8 (frame slots
            // descend; the stack grows down), matching
            // `store_slots_through_ptr`. Previously these sources returned
            // None and every consumer's fallback materialized only slot 0 —
            // the RUE-188 miscompile family. (RUE-188/192)
            for proj in &projections {
                if let Projection::Index { array_type, index } = proj {
                    let length = b.ctx().array_length(*array_type);
                    let index_vreg = b.get_vreg(*index);
                    b.emit_bounds_check(index_vreg, length);
                }
            }
            let addr_vreg = b.alloc_vreg();
            b.emit_place_addr(addr_vreg, &place);
            Some(load_through_ptr(b, addr_vreg, slot_count))
        }
        // BlockParam and Call should already have slot vregs in the cache
        _ => None,
    }
}

/// Lower a `StructInit`: flatten the field values into one vreg per slot,
/// cache the list, and bind the value's primary vreg (first slot, or zero for
/// a fieldless struct).
///
/// Nested struct/String fields contribute all their slot vregs via the
/// accessor — including a field read from another aggregate (`B { p: a.p }`).
/// Nested array fields flatten to their element scalar slots so the cached
/// slot list is fully flattened (consumers and the Alloc of this StructInit
/// rely on it). (RUE-118)
pub fn lower_struct_init<B: SlotBackend>(
    b: &mut B,
    value: CfgValue,
    fields_start: u32,
    fields_len: u32,
) {
    let vreg = b.alloc_vreg();
    b.map_value(value, vreg);

    let fields = b.ctx().cfg.get_extra(fields_start, fields_len).to_vec();
    let mut slot_vregs = Vec::new();
    for field in &fields {
        let field_ty = b.ctx().cfg.get_inst(*field).ty;
        match field_ty.kind() {
            TypeKind::Struct(_) => {
                let nested = get_or_compute_field_vregs(b, *field)
                    .expect("nested struct field should have slot vregs");
                slot_vregs.extend(nested);
            }
            TypeKind::Array(_) => {
                // Try the accessor first: it materializes lazily-sourced
                // arrays (Load/Param/PlaceRead — including an indexed read
                // like `S { arr: m[i], .. }`, RUE-188) and cache-hits eager
                // ones. Fall back to the recursive flattener for ArrayInit,
                // whose element vregs it gathers directly.
                if let Some(nested) = get_or_compute_field_vregs(b, *field) {
                    slot_vregs.extend(nested);
                } else {
                    slot_vregs.extend(b.collect_array_scalars(*field));
                }
            }
            _ => {
                // Scalar field - single vreg
                slot_vregs.push(b.get_vreg(*field));
            }
        }
    }

    if let Some(&first_vreg) = slot_vregs.first() {
        b.emit_reg_move(vreg, first_vreg);
    } else {
        b.emit_load_zero(vreg);
    }

    b.slot_cache().insert(value, slot_vregs);
}

/// Lower an `ArrayInit`: flatten the element values into one vreg per slot
/// and cache the list. The value's primary vreg is a zero placeholder — an
/// array base has no single value; the actual storage is handled by the
/// `Alloc` that precedes this.
///
/// Nested aggregate elements (multidimensional arrays, arrays of structs) are
/// flattened to their scalar slots so the cached list is the full slot set —
/// their own primary vreg is just a placeholder. (RUE-118)
pub fn lower_array_init<B: SlotBackend>(
    b: &mut B,
    value: CfgValue,
    elements_start: u32,
    elements_len: u32,
) {
    let vreg = b.alloc_vreg();
    b.map_value(value, vreg);

    let elements = b.ctx().cfg.get_extra(elements_start, elements_len).to_vec();
    let mut element_vregs: Vec<VReg> = Vec::new();
    for e in &elements {
        let e_ty = b.ctx().cfg.get_inst(*e).ty;
        if matches!(e_ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_)) {
            let nested = get_or_compute_field_vregs(b, *e)
                .expect("nested aggregate element should have slot vregs");
            element_vregs.extend(nested);
        } else {
            element_vregs.push(b.get_vreg(*e));
        }
    }
    b.slot_cache().insert(value, element_vregs);

    b.emit_load_zero(vreg);
}

/// Store `vals` to consecutive frame slots starting at `base_slot`
/// (slot `base_slot + i` gets `vals[i]`).
pub fn store_slots<B: SlotBackend>(b: &mut B, vals: &[VReg], base_slot: u32) {
    for (i, val) in vals.iter().enumerate() {
        b.emit_store_slot(*val, base_slot + i as u32);
    }
}

/// Store `vals` through `ptr` at descending byte offsets: `vals[i]` goes to
/// `ptr - static_byte_offset - i*8`. Caller-frame slots descend from an inout
/// pointer (the stack grows down), matching the place-read path.
pub fn store_slots_through_ptr<B: SlotBackend>(
    b: &mut B,
    vals: &[VReg],
    ptr: VReg,
    static_byte_offset: i32,
) {
    for (i, val) in vals.iter().enumerate() {
        b.emit_store_through_ptr(*val, ptr, -static_byte_offset - (i as i32) * 8);
    }
}

/// Callee side of the sret return convention (RUE-106): load the sret buffer
/// pointer the prologue saved at the frame slot one past the param area, then
/// store every slot of the return value through it at ascending byte offsets
/// (`vals[i]` to `ptr + i*8` — the buffer is caller-allocated and addressed
/// upward, unlike the descending inout frame-slot writes).
///
/// See `crate::cfg_lower::type_uses_sret_return` for when returns use sret.
pub fn store_slots_to_sret<B: SlotBackend>(b: &mut B, vals: &[VReg]) {
    let ptr = b.alloc_vreg();
    let sret_slot = b.ctx().sret_ptr_slot();
    b.emit_load_slot(ptr, sret_slot);
    for (i, val) in vals.iter().enumerate() {
        b.emit_store_through_ptr(*val, ptr, (i as i32) * 8);
    }
}

/// Load `count` consecutive frame slots starting at `base_slot`, returning the
/// freshly-allocated vregs in slot order.
fn load_consecutive<B: SlotBackend>(b: &mut B, base_slot: u32, count: u32) -> Vec<VReg> {
    let mut vregs = Vec::with_capacity(count as usize);
    for i in 0..count {
        let vreg = b.alloc_vreg();
        b.emit_load_slot(vreg, base_slot + i);
        vregs.push(vreg);
    }
    vregs
}

/// Load `count` slots through `addr_vreg` at descending byte offsets (slot i
/// at `addr - i*8` — frame slots descend; the stack grows down, matching
/// [`store_slots_through_ptr`]), returning the freshly-allocated vregs in
/// slot order.
fn load_through_ptr<B: SlotBackend>(b: &mut B, addr_vreg: VReg, count: u32) -> Vec<VReg> {
    let mut vregs = Vec::with_capacity(count as usize);
    for i in 0..count {
        let vreg = b.alloc_vreg();
        b.emit_load_through_ptr(vreg, addr_vreg, -(i as i32) * 8);
        vregs.push(vreg);
    }
    vregs
}
