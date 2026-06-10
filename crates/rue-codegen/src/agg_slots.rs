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
//! Phase 1 covers the slot accessor. The StructInit/ArrayInit flattening and
//! the consumer-side store loops still live per-backend; they migrate here in
//! later phases once their (currently subtly different) behaviors are
//! reconciled deliberately rather than incidentally.

use std::collections::HashMap;

use rue_air::TypeKind;
use rue_cfg::{CfgInstData, CfgValue, PlaceBase, Projection};

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

    /// Emit a load of frame slot `slot` into `dst`.
    fn emit_load_slot(&mut self, dst: VReg, slot: u32);
}

/// Get or compute the slot vregs for a multi-slot aggregate value
/// (struct, builtin String, or fixed-size array).
///
/// Sources handled:
/// - cache hit (StructInit/ArrayInit/Call/BlockParam populate eagerly)
/// - `StructInit`: the field values' vregs directly
/// - `Load`: load `slot_count` consecutive stack slots
/// - `Param`: load from the parameter slot area
/// - `PlaceRead` with static field projections: load from the field's slots
///   (dynamic array indexing and inout params return `None`, preserving the
///   callers' fallback behavior)
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
            let base_slot = b.ctx().num_locals + index;
            let slot_count = b.ctx().type_slot_count(ty);
            Some(load_consecutive(b, base_slot, slot_count))
        }
        CfgInstData::PlaceRead { place } => {
            // A multi-slot aggregate read from a place — e.g. `let s2 = h.s;`.
            // The base place-read lowering only materializes the first slot;
            // here we materialize all of them so consumers see the full value.
            // Only static field projections are handled — dynamic array
            // indexing and inout params fall through to None. (RUE-22/63/94)
            let projections = b.ctx().cfg.get_place_projections(&place).to_vec();
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
                    Projection::Index { .. } => return None,
                }
            }
            let base_slot = match place.base {
                PlaceBase::Local(slot) => slot + static_slot_offset,
                PlaceBase::Param(param_slot) => {
                    if b.ctx().cfg.is_param_inout(param_slot) {
                        return None;
                    }
                    b.ctx().num_locals + param_slot + static_slot_offset
                }
            };
            let slot_count = b.ctx().type_slot_count(ty);
            Some(load_consecutive(b, base_slot, slot_count))
        }
        // BlockParam and Call should already have slot vregs in the cache
        _ => None,
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
