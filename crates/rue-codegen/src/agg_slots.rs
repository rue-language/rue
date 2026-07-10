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

use rue_air::{Type, TypeKind};
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

    /// Emit a load of the integer constant `imm` into `dst` (used for an
    /// enum discriminant, RUE-221).
    fn emit_load_imm(&mut self, dst: VReg, imm: i64);

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

    /// Emit `dst = low-end address of the (possibly projected) place` — the
    /// backend's `lower_place_addr`. The result is the place's lowest-address
    /// byte; aggregate slots ascend from it (`slot k` at `dst + k*8`), uniform
    /// for frame- and heap-rooted places (ADR-0040 / RUE-311). Does NOT
    /// bounds-check index projections; callers do that first.
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
    // Aggregates are structs, arrays, and payload-carrying enums (a
    // discriminant-only enum stays a 1-slot scalar and is handled by the
    // ordinary single-vreg path). (RUE-221)
    let is_payload_enum = matches!(ty.kind(), TypeKind::Enum(_)) && b.ctx().type_slot_count(ty) > 1;
    if !matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_)) && !is_payload_enum {
        return None;
    }

    match data {
        CfgInstData::StructInit {
            fields_start,
            fields_len,
            ..
        } => {
            let fields = b.ctx().cfg.get_extra(fields_start, fields_len).to_vec();
            // Zero-sized fields (unit, [T; 0]) occupy no slots (RUE-577):
            // including their dummy vregs would shift every later field.
            let mut vregs = Vec::with_capacity(fields.len());
            for f in &fields {
                let field_ty = b.ctx().cfg.get_inst(*f).ty;
                if b.ctx().type_slot_count(field_ty) == 0 {
                    continue;
                }
                vregs.push(b.get_vreg(*f));
            }
            Some(vregs)
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
                // Purely static projection chain rooted at a frame slot.
                // Ascending layout (ADR-0040 / RUE-311): the accessed field's
                // low end (its logical slot 0) sits at the ROOT object's low
                // end plus the summed field byte offset. In frame-slot terms
                // the root's low end is `root_base + (C_root - 1)` (its
                // highest-numbered = lowest-address slot), and adding the field
                // offset in ADDRESS space means SUBTRACTING it in slot number.
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
                let root_base = match place.base {
                    PlaceBase::Local(slot) => slot,
                    PlaceBase::Param(param_slot) => b.ctx().num_locals + param_slot,
                };
                let root_count = root_slot_count(b, &projections, slot_count);
                let field_low_slot = root_base + (root_count - 1) - static_slot_offset;
                return Some(load_slots_at_low(b, field_low_slot, slot_count));
            }

            // Indexed element (`arr[i]`, `s.rows[i]` — constant or dynamic
            // index) or a projection through a by-ref param: bounds-check
            // every index projection, form the place's low-end address, then
            // load each slot through it. Slot k lives at addr + k*8 (ascending,
            // ADR-0040 / RUE-311), matching `store_slots_through_ptr`.
            // Previously these sources returned None and every consumer's
            // fallback materialized only slot 0 — the RUE-188 miscompile
            // family. (RUE-188/192)
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

/// Total slot count of the ROOT object a projected place is rooted at — the
/// container the first projection indexes into. Ascending place addressing
/// (ADR-0040 / RUE-311) anchors every field/element at the root object's low
/// end (`root_base + root_count - 1` in frame slots), so the root's own slot
/// count is the one origin shift a projection chain needs; nested containers
/// contribute only their byte offsets from that anchor. With no projections the
/// place *is* the root, so `fallback` (the accessed value's own slot count) is
/// used.
pub fn root_slot_count<B: SlotBackend>(b: &B, projections: &[Projection], fallback: u32) -> u32 {
    match projections.first() {
        Some(Projection::Field { struct_id, .. }) => b.ctx().struct_total_slot_count(*struct_id),
        Some(Projection::Index { array_type, .. }) => b.ctx().type_slot_count(*array_type),
        None => fallback,
    }
}

/// Pre-allocate the per-slot vregs for an aggregate block parameter.
///
/// Block-parameter lowering is a special case: each backend has already
/// allocated and mapped the parameter's primary vreg before aggregate-slot
/// bookkeeping runs. For multi-slot aggregate params, the slot cache must hold
/// exactly `type_slot_count(ty)` vregs, using the primary vreg for logical slot
/// 0 and freshly-allocated vregs for the remaining slots. Zero-slot aggregates
/// intentionally cache an empty list, matching their source values and keeping
/// join-edge slot-count checks honest (RUE-167, RUE-194, RUE-248).
pub fn preallocate_block_param_slots<B: SlotBackend>(
    b: &mut B,
    param_value: CfgValue,
    ty: Type,
    primary_vreg: VReg,
) {
    if !b.ctx().is_multislot_aggregate(ty) {
        return;
    }

    let slot_count = b.ctx().type_slot_count(ty);
    let mut slot_vregs = Vec::with_capacity(slot_count as usize);
    if slot_count > 0 {
        slot_vregs.push(primary_vreg);
    }
    for _ in 1..slot_count {
        slot_vregs.push(b.alloc_vreg());
    }
    b.slot_cache().insert(param_value, slot_vregs);
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
        // A zero-sized field (unit, [T; 0], empty struct) occupies no slots
        // (RUE-577): pushing the dummy vreg CFG carries for it would shift
        // every later field one slot over — `Mixed { a: (), b: 42 }` stored
        // 42 into the wrong slot.
        if b.ctx().type_slot_count(field_ty) == 0 {
            continue;
        }
        match field_ty.kind() {
            TypeKind::Struct(_) | TypeKind::Enum(_) => {
                // Struct/payload-enum field: contribute every slot. A
                // payload enum here previously fell to the scalar `_` arm and
                // contributed only its discriminant vreg, so the struct stored
                // just the tag and dropped the payload (RUE-237). The accessor
                // returns None for a discriminant-only (1-slot) enum, which
                // correctly falls back to the single-vreg push.
                if let Some(nested) = get_or_compute_field_vregs(b, *field) {
                    slot_vregs.extend(nested);
                } else {
                    slot_vregs.push(b.get_vreg(*field));
                }
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

/// Lower an `EnumVariant` tuple-variant construction into slot vregs (RUE-221).
///
/// Tagged-union layout: slot 0 is the discriminant (`variant_index`), followed
/// by the payload operands flattened to one vreg per slot. The payload area is
/// padded with zero slots so every value of the enum type occupies the same
/// number of slots (sized to the largest variant), which is what consumers
/// (Alloc, Store, match extraction) expect. The value's primary vreg is slot 0
/// (the discriminant), so a `match` can switch on it directly.
pub fn lower_enum_variant<B: SlotBackend>(
    b: &mut B,
    value: CfgValue,
    payload_start: u32,
    payload_len: u32,
) {
    let ty = b.ctx().cfg.get_inst(value).ty;
    let total_slots = b.ctx().type_slot_count(ty);

    // Slot 0: the discriminant.
    let disc_vreg = b.alloc_vreg();
    let variant_index = match &b.ctx().cfg.get_inst(value).data {
        CfgInstData::EnumVariant { variant_index, .. } => *variant_index,
        _ => unreachable!("lower_enum_variant on non-EnumVariant"),
    };
    b.emit_load_imm(disc_vreg, variant_index as i64);
    b.map_value(value, disc_vreg);

    let mut slot_vregs: Vec<VReg> = Vec::with_capacity(total_slots as usize);
    slot_vregs.push(disc_vreg);

    // Payload slots, flattening nested aggregates like StructInit does.
    let payload = b.ctx().cfg.get_extra(payload_start, payload_len).to_vec();
    for field in &payload {
        let field_ty = b.ctx().cfg.get_inst(*field).ty;
        // Zero-sized payload fields occupy no slots (RUE-577): pushing their
        // dummy vreg would overfill the payload area before padding —
        // `Some(())` cached one slot too many and stores clobbered the
        // neighboring frame slot.
        if b.ctx().type_slot_count(field_ty) == 0 {
            continue;
        }
        match field_ty.kind() {
            TypeKind::Struct(_) | TypeKind::Enum(_) => {
                if let Some(nested) = get_or_compute_field_vregs(b, *field) {
                    slot_vregs.extend(nested);
                } else {
                    slot_vregs.push(b.get_vreg(*field));
                }
            }
            TypeKind::Array(_) => {
                if let Some(nested) = get_or_compute_field_vregs(b, *field) {
                    slot_vregs.extend(nested);
                } else {
                    slot_vregs.extend(b.collect_array_scalars(*field));
                }
            }
            _ => slot_vregs.push(b.get_vreg(*field)),
        }
    }

    // Pad the payload area to the enum's full slot count so all values of this
    // type are the same size (the union is sized to the largest variant).
    while (slot_vregs.len() as u32) < total_slots {
        let pad = b.alloc_vreg();
        b.emit_load_zero(pad);
        slot_vregs.push(pad);
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

    // The slot cache holds the aggregate's logical slots in ascending order —
    // element 0 first, then element 1, and so on, each element's own slots in
    // logical order (RUE-311). Physical ascending layout (element 0 at the
    // array's lowest address, element i at base + i*element_size — ADR-0040 /
    // RUE-243) is produced when this list is *stored*: `store_slots` reverses
    // it into the frame (logical slot 0 at the highest-numbered = lowest-
    // address slot), and `store_slots_through_ptr` writes it ascending from a
    // low-end pointer. So every consumer — frame store, `@ptr_write`, the call
    // ABI, structural equality — sees one uniform slot order.
    let elements = b.ctx().cfg.get_extra(elements_start, elements_len).to_vec();
    let mut element_vregs: Vec<VReg> = Vec::new();
    for e in elements.iter() {
        let e_ty = b.ctx().cfg.get_inst(*e).ty;
        // Payload enums are multi-slot aggregates too: a discriminant-only
        // enum (accessor returns None) stays a single-vreg scalar, but a
        // payload-carrying element must contribute all its slots or the array
        // stores only the tag and drops the payload (RUE-237).
        if matches!(
            e_ty.kind(),
            TypeKind::Struct(_) | TypeKind::Array(_) | TypeKind::Enum(_)
        ) {
            if let Some(nested) = get_or_compute_field_vregs(b, *e) {
                element_vregs.extend(nested);
            } else {
                element_vregs.push(b.get_vreg(*e));
            }
        } else {
            element_vregs.push(b.get_vreg(*e));
        }
    }
    b.slot_cache().insert(value, element_vregs);

    b.emit_load_zero(vreg);
}

/// Store a whole aggregate value's `vals` (one vreg per logical slot, slot 0
/// first) to the frame region beginning at `base_slot`, laid out
/// ASCENDING-in-address (ADR-0040): logical slot 0 at the region's LOWEST
/// address, slot `k` at `base_low_addr + k*8`. Frame slots descend in address
/// (a higher slot number is a lower address), so logical slot `k` is stored at
/// frame slot `base_slot + (len-1) - k` — the region's low end is its
/// highest-numbered slot.
pub fn store_slots<B: SlotBackend>(b: &mut B, vals: &[VReg], base_slot: u32) {
    let low_slot = base_slot + (vals.len() as u32).saturating_sub(1);
    store_slots_at_low(b, vals, low_slot);
}

/// Store `vals` (logical slot 0 first) into the frame with the value's
/// low-end (slot-0) at frame slot `low_slot`; logical slot `k` lands at frame
/// slot `low_slot - k` (each higher slot at a higher address — ascending,
/// ADR-0040).
pub fn store_slots_at_low<B: SlotBackend>(b: &mut B, vals: &[VReg], low_slot: u32) {
    for (k, val) in vals.iter().enumerate() {
        b.emit_store_slot(*val, low_slot - k as u32);
    }
}

/// Store `vals` (logical slot 0 first) through `ptr` at ASCENDING byte
/// offsets: `vals[k]` goes to `ptr + static_byte_offset + k*8`. `ptr` is the
/// pointee's low-end address (what `@raw` and `@ptr_offset` yield, and where
/// `@alloc` blocks begin), so aggregate slots ascend uniformly for pointers of
/// every origin — stack, heap, or `@int_to_ptr` (ADR-0040 / RUE-311).
pub fn store_slots_through_ptr<B: SlotBackend>(
    b: &mut B,
    vals: &[VReg],
    ptr: VReg,
    static_byte_offset: i32,
) {
    for (i, val) in vals.iter().enumerate() {
        b.emit_store_through_ptr(*val, ptr, static_byte_offset + (i as i32) * 8);
    }
}

/// Callee side of the sret return convention (RUE-106): load the sret buffer
/// pointer the prologue saved at the frame slot one past the param area, then
/// store every slot of the return value through it at ascending byte offsets
/// (`vals[i]` to `ptr + i*8`) — the same low-end-ascending convention every
/// aggregate memory access now uses (ADR-0040 / RUE-311).
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

/// Load `count` slots through `ptr` at ASCENDING byte offsets (slot k at
/// `ptr + k*8`), returning the freshly-allocated vregs in logical slot order.
/// The public counterpart of [`store_slots_through_ptr`]: used by `@ptr_read`
/// to read an aggregate pointee through a raw pointer, one 8-byte slot per
/// field (RUE-242). `ptr` is the pointee's low-end address; every value of an
/// aggregate type occupies `type_slot_count` consecutive ascending slots
/// (ADR-0040 / RUE-311).
pub fn load_slots_through_ptr<B: SlotBackend>(b: &mut B, ptr: VReg, count: u32) -> Vec<VReg> {
    load_through_ptr(b, ptr, count)
}

/// Load a whole aggregate value's `count` logical slots from the frame region
/// beginning at `base_slot` (ascending layout, ADR-0040): logical slot 0 is at
/// the region's low end (frame slot `base_slot + count - 1`), slot `k` at
/// `base_slot + count - 1 - k`. Returns the vregs in logical slot order.
fn load_consecutive<B: SlotBackend>(b: &mut B, base_slot: u32, count: u32) -> Vec<VReg> {
    load_slots_at_low(b, base_slot + count.saturating_sub(1), count)
}

/// Load `count` logical slots (slot 0 first) from the frame with the value's
/// low-end (slot 0) at frame slot `low_slot`; logical slot `k` is read from
/// frame slot `low_slot - k` (ascending, ADR-0040).
pub fn load_slots_at_low<B: SlotBackend>(b: &mut B, low_slot: u32, count: u32) -> Vec<VReg> {
    let mut vregs = Vec::with_capacity(count as usize);
    for k in 0..count {
        let vreg = b.alloc_vreg();
        b.emit_load_slot(vreg, low_slot - k);
        vregs.push(vreg);
    }
    vregs
}

/// Load `count` slots through `addr_vreg` at ASCENDING byte offsets (slot k at
/// `addr + k*8`, matching [`store_slots_through_ptr`]), returning the
/// freshly-allocated vregs in logical slot order. `addr_vreg` is the pointee's
/// low-end address (ADR-0040 / RUE-311).
fn load_through_ptr<B: SlotBackend>(b: &mut B, addr_vreg: VReg, count: u32) -> Vec<VReg> {
    let mut vregs = Vec::with_capacity(count as usize);
    for k in 0..count {
        let vreg = b.alloc_vreg();
        b.emit_load_through_ptr(vreg, addr_vreg, (k as i32) * 8);
        vregs.push(vreg);
    }
    vregs
}
