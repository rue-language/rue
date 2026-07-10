//! Shared type utilities for code generation backends.
//!
//! This module provides common functions for calculating type sizes and
//! field offsets, shared between x86_64 and aarch64 backends.
//!
//! As of Phase 3 (ADR-0024), all struct/enum lookups go through `TypeInternPool`
//! instead of separate `&[StructDef]` slices.

use rue_air::{ArrayTypeId, EnumId, StructId, TypeInternPool, TypeKind};
use rue_cfg::{Cfg, CfgInstData, CfgValue, Type};
use std::collections::HashMap;

use crate::vreg::VReg;

/// Extract the ArrayTypeId from a Type::Array.
/// Returns None if the type is not an array type.
#[inline]
pub fn extract_array_type_id(ty: Type) -> Option<ArrayTypeId> {
    match ty.kind() {
        TypeKind::Array(id) => Some(id),
        _ => None,
    }
}

/// Get the array type definition for an array type ID.
///
/// Returns `(element_type, length)`.
pub fn array_type_def(type_pool: &TypeInternPool, array_type_id: ArrayTypeId) -> (Type, u64) {
    type_pool.array_def(array_type_id)
}

/// Get the array type definition from a Type.
///
/// Returns `Some((element_type, length))` if the type is an array type, `None` otherwise.
#[inline]
pub fn array_type_def_from_type(type_pool: &TypeInternPool, ty: Type) -> Option<(Type, u64)> {
    extract_array_type_id(ty).map(|id| array_type_def(type_pool, id))
}

/// Get the length of an array from its Type.
/// Returns 0 if the type is not an array.
#[inline]
pub fn array_length_from_type(type_pool: &TypeInternPool, ty: Type) -> u64 {
    array_type_def_from_type(type_pool, ty)
        .map(|(_element_type, length)| length)
        .unwrap_or(0)
}

/// Calculate the slot count for a single element of an array from its Type.
#[inline]
pub fn array_element_slot_count_from_type(type_pool: &TypeInternPool, ty: Type) -> u32 {
    if let Some((element_type, _length)) = array_type_def_from_type(type_pool, ty) {
        type_slot_count(type_pool, element_type)
    } else {
        1
    }
}

/// Calculate the total number of slots needed to store a type.
///
/// For scalars, this is 1. For arrays, it's `length * slot_count(element_type)`.
/// For structs, this is the sum of slot counts for all fields.
/// For nested types, this recursively calculates.
/// Zero-sized types (unit, never, empty structs, zero-length arrays) return 0.
pub fn type_slot_count(type_pool: &TypeInternPool, ty: Type) -> u32 {
    match ty.kind() {
        // Zero-sized types
        TypeKind::Unit | TypeKind::Never => 0,
        TypeKind::Array(array_type_id) => {
            // Zero-length arrays naturally get 0 slots (0 * element_slots).
            // SATURATING, not truncating-u32: sema rejects oversized types
            // with E0906 before lowering (RUE-561), so this value is never
            // used for real allocation, but it must not silently wrap
            // (`[i32; 4294967296]` truncated to 0 slots) or panic in debug.
            let (element_type, length) = type_pool.array_def(array_type_id);
            let elem_slots = u64::from(type_slot_count(type_pool, element_type));
            u32::try_from(elem_slots.saturating_mul(length)).unwrap_or(u32::MAX)
        }
        TypeKind::Struct(struct_id) => {
            // Sum the slot counts of all fields
            // Empty structs naturally get 0 slots here
            let struct_def = type_pool.struct_def(struct_id);
            let mut total = 0u32;
            for field in &struct_def.fields {
                total = total.saturating_add(type_slot_count(type_pool, field.ty));
            }
            total
        }
        TypeKind::Enum(enum_id) => {
            // Tagged-union layout (RUE-221, ADR-0038): slot 0 holds the
            // discriminant; the payload area that follows is sized to the
            // largest variant. A discriminant-only (C-like) enum has no
            // payload and therefore occupies exactly one slot, unchanged from
            // before payloads existed.
            let enum_def = type_pool.enum_def(enum_id);
            let mut max_payload = 0u32;
            for i in 0..enum_def.variant_count() {
                let mut variant_slots = 0u32;
                for &ty in enum_def.variant_payload(i) {
                    variant_slots = variant_slots.saturating_add(type_slot_count(type_pool, ty));
                }
                max_payload = max_payload.max(variant_slots);
            }
            1u32.saturating_add(max_payload)
        }
        // Scalars and other types use 1 slot
        _ => 1,
    }
}

/// Return the `(Some, None)` discriminant values for an `Option`-shaped enum
/// type, i.e. the variant indices of its `Some` and `None` variants.
///
/// Used to lower the fallible intrinsics (`@read_line`, `@parse_*`), whose
/// runtime writes the caller's `Some`/`None` discriminant into the tagged-union
/// result (RUE-6, ADR-0038). Sema has already validated the result is
/// `Option`-shaped, so both variants are present; a missing one is a compiler
/// bug and panics (a correctness guard, RUE-45).
pub fn option_variant_discriminants(type_pool: &TypeInternPool, ty: Type) -> (u64, u64) {
    let enum_id = ty
        .as_enum()
        .expect("fallible intrinsic result must be an Option enum");
    let enum_def = type_pool.enum_def(enum_id);
    let some_disc = enum_def
        .find_variant("Some")
        .expect("Option result enum must have a `Some` variant") as u64;
    let none_disc = enum_def
        .find_variant("None")
        .expect("Option result enum must have a `None` variant") as u64;
    (some_disc, none_disc)
}

/// Whether comparing a slot holding `ty` needs a full 64-bit compare.
///
/// Narrow scalars (i8..i32, bool) live in 32-bit-comparable slots; 64-bit
/// integers and raw pointers must be compared at full width so two pointers or
/// i64s that differ only in their high 32 bits are not judged equal. Raw
/// pointers compare by ADDRESS (identity), which a 64-bit compare of the
/// pointer value gives directly.
pub fn slot_needs_wide_compare(ty: Type) -> bool {
    matches!(
        ty.kind(),
        TypeKind::I64 | TypeKind::U64 | TypeKind::PtrConst(_) | TypeKind::PtrMut(_)
    )
}

/// Flatten an aggregate type into the list of leaf types, one entry per storage
/// slot, in slot order. The length equals [`type_slot_count`].
///
/// This drives structural equality (RUE-285): a struct/array/payload-enum `==`
/// compares every slot pairwise, and each slot's leaf type selects the compare
/// width via [`slot_needs_wide_compare`].
///
/// Layout mirrors [`type_slot_count`]:
/// - **struct**: fields in declaration order, each recursively flattened
/// - **array**: the element leaf types repeated `length` times
/// - **payload enum**: slot 0 is the discriminant (a narrow tag), followed by
///   the payload area sized to the widest variant. For each payload slot we
///   pick a representative leaf type that is wide if *any* variant places a
///   wide leaf there, so a mixed `i64`/`i32` payload slot is compared at 64
///   bits (narrow values are zero-extended in their slot, so a wide compare of
///   a narrow value is still correct). Padding slots beyond every variant's
///   payload are zero and compare as narrow.
/// - **unit / never**: zero slots (no entries)
pub fn aggregate_leaf_types(type_pool: &TypeInternPool, ty: Type) -> Vec<Type> {
    let mut out = Vec::new();
    push_leaf_types(type_pool, ty, &mut out);
    out
}

fn push_leaf_types(type_pool: &TypeInternPool, ty: Type, out: &mut Vec<Type>) {
    match ty.kind() {
        TypeKind::Unit | TypeKind::Never => {}
        TypeKind::Struct(struct_id) => {
            let struct_def = type_pool.struct_def(struct_id);
            for field in &struct_def.fields {
                push_leaf_types(type_pool, field.ty, out);
            }
        }
        TypeKind::Array(array_id) => {
            let (element_type, length) = type_pool.array_def(array_id);
            for _ in 0..length {
                push_leaf_types(type_pool, element_type, out);
            }
        }
        TypeKind::Enum(enum_id) => {
            // Slot 0: the discriminant. Variant indices are small, and both the
            // tag and any padding are materialized as clean (zero-extended)
            // values, so a narrow compare of the tag is correct.
            out.push(Type::I32);
            // Payload area sized to the widest variant. For each slot position,
            // prefer a wide leaf type if any variant carries one there.
            let enum_def = type_pool.enum_def(enum_id);
            let mut per_slot: Vec<Type> = Vec::new();
            for v in 0..enum_def.variant_count() {
                let mut variant_leaves: Vec<Type> = Vec::new();
                for &pty in enum_def.variant_payload(v) {
                    push_leaf_types(type_pool, pty, &mut variant_leaves);
                }
                for (i, leaf) in variant_leaves.into_iter().enumerate() {
                    if i >= per_slot.len() {
                        per_slot.push(leaf);
                    } else if slot_needs_wide_compare(leaf) && !slot_needs_wide_compare(per_slot[i])
                    {
                        per_slot[i] = leaf;
                    }
                }
            }
            out.extend(per_slot);
        }
        _ => out.push(ty),
    }
}

/// Slot offset of payload field `field_index` within an enum's payload area.
///
/// The discriminant occupies slot 0, so payload fields begin at slot 1. This
/// accounts for the slot sizes of all preceding payload fields of the same
/// variant (RUE-221).
pub fn enum_payload_slot_offset(
    type_pool: &TypeInternPool,
    enum_id: EnumId,
    variant_index: u32,
    field_index: u32,
) -> u32 {
    let enum_def = type_pool.enum_def(enum_id);
    let payload = enum_def.variant_payload(variant_index as usize);
    // 1 for the discriminant slot.
    let mut offset = 1u32;
    for i in 0..(field_index as usize) {
        if let Some(&ty) = payload.get(i) {
            offset += type_slot_count(type_pool, ty);
        }
    }
    offset
}

/// Calculate the slot count for a single element of an array type.
pub fn array_element_slot_count(type_pool: &TypeInternPool, array_type_id: ArrayTypeId) -> u32 {
    let (element_type, _length) = type_pool.array_def(array_type_id);
    type_slot_count(type_pool, element_type)
}

/// Calculate the size in bytes of a type.
///
/// This is used for pointer arithmetic where we need the actual byte size,
/// not the slot count. Each slot is 8 bytes, but primitive types may use
/// fewer bytes (e.g., i8 uses 1 byte, i16 uses 2 bytes).
///
/// However, for simplicity and alignment purposes, all types currently use
/// 8 bytes per slot. This function returns `slot_count * 8`.
pub fn type_size_bytes(type_pool: &TypeInternPool, ty: Type) -> u64 {
    let slots = type_slot_count(type_pool, ty);
    (slots as u64) * 8
}

/// Total slot count of a struct: the sum of every field's slot count. Equal to
/// `type_slot_count(Struct(struct_id))`, but reachable directly from a
/// `StructId` (used to anchor ascending place addressing at a struct root —
/// ADR-0040 / RUE-311).
pub fn struct_slot_count(type_pool: &TypeInternPool, struct_id: StructId) -> u32 {
    let struct_def = type_pool.struct_def(struct_id);
    let mut total = 0u32;
    for field in &struct_def.fields {
        total += type_slot_count(type_pool, field.ty);
    }
    total
}

/// Calculate the slot offset for a field within a struct.
///
/// This accounts for the sizes of all preceding fields.
pub fn struct_field_slot_offset(
    type_pool: &TypeInternPool,
    struct_id: StructId,
    field_index: u32,
) -> u32 {
    let struct_def = type_pool.struct_def(struct_id);
    let mut offset = 0u32;
    for i in 0..(field_index as usize) {
        if let Some(field) = struct_def.fields.get(i) {
            offset += type_slot_count(type_pool, field.ty);
        }
    }
    offset
}

/// Recursively collect all scalar vregs from an array value.
///
/// For nested arrays, this flattens them to a list of scalar vregs.
/// This is used during code generation to handle array arguments that need
/// to be passed in registers or stored to memory slot by slot.
///
/// # Arguments
/// * `cfg` - The control flow graph containing the instructions
/// * `struct_slot_vregs` - Cache mapping CFG values to their slot vregs
/// * `value` - The CFG value to collect vregs from
/// * `get_vreg` - Closure to get/allocate a vreg for a given CFG value
pub fn collect_array_scalar_vregs(
    cfg: &Cfg,
    struct_slot_vregs: &HashMap<CfgValue, Vec<VReg>>,
    value: CfgValue,
    get_vreg: &mut impl FnMut(CfgValue) -> VReg,
) -> Vec<VReg> {
    let inst = cfg.get_inst(value);
    match &inst.data {
        CfgInstData::ArrayInit {
            elements_start,
            elements_len,
            ..
        } => {
            let elements = cfg.get_extra(*elements_start, *elements_len);
            let mut result = Vec::new();
            // Collect elements in logical order (element 0 first), matching
            // `lower_array_init`: the slot cache is uniform ascending order and
            // the ascending physical layout is produced at store time (ADR-0040
            // / RUE-311).
            for elem in elements.iter() {
                let elem_inst = cfg.get_inst(*elem);
                if elem_inst.ty.is_array() {
                    // Recursively collect from nested array
                    result.extend(collect_array_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *elem,
                        get_vreg,
                    ));
                } else if elem_inst.ty.is_struct() {
                    // Recursively collect from struct element (includes builtin String)
                    result.extend(collect_struct_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *elem,
                        get_vreg,
                    ));
                } else {
                    // Scalar element - get its vreg
                    result.push(get_vreg(*elem));
                }
            }
            result
        }
        _ => {
            // For non-ArrayInit sources, try struct_slot_vregs cache
            if let Some(vregs) = struct_slot_vregs.get(&value).cloned() {
                vregs
            } else {
                vec![get_vreg(value)]
            }
        }
    }
}

/// Generate the drop glue function name for an array type.
///
/// The name encodes the element type and length, e.g., `__rue_drop_array_String_3`.
/// This must match the name generated by `rue_compiler::drop_glue::array_drop_glue_name`.
pub fn array_drop_glue_name(array_id: ArrayTypeId, type_pool: &TypeInternPool) -> String {
    let (element_type, length) = type_pool.array_def(array_id);
    let element_type_name = type_name(element_type, type_pool);
    format!("__rue_drop_array_{}_{}", element_type_name, length)
}

/// Get a name for a type (used for generating drop glue function names).
fn type_name(ty: Type, type_pool: &TypeInternPool) -> String {
    match ty.kind() {
        TypeKind::I8 => "i8".to_string(),
        TypeKind::I16 => "i16".to_string(),
        TypeKind::I32 => "i32".to_string(),
        TypeKind::I64 => "i64".to_string(),
        TypeKind::U8 => "u8".to_string(),
        TypeKind::U16 => "u16".to_string(),
        TypeKind::U32 => "u32".to_string(),
        TypeKind::U64 => "u64".to_string(),
        TypeKind::Bool => "bool".to_string(),
        TypeKind::Unit => "unit".to_string(),
        TypeKind::Never => "never".to_string(),
        TypeKind::Error => "error".to_string(),
        // ComptimeType only exists at compile time, no runtime representation
        TypeKind::ComptimeType => "comptime_type".to_string(),
        TypeKind::Enum(enum_id) => format!("enum{}", enum_id.0),
        // Struct types include builtin types like String
        TypeKind::Struct(struct_id) => type_pool.struct_def(struct_id).name.clone(),
        TypeKind::Array(array_id) => {
            let (element_type, length) = type_pool.array_def(array_id);
            let elem_name = type_name(element_type, type_pool);
            format!("array_{}_{}", elem_name, length)
        }
        // Module types should never reach codegen (compile-time only)
        TypeKind::Module(_) => "module".to_string(),
        // Pointer types
        TypeKind::PtrConst(ptr_id) => {
            let pointee = type_pool.ptr_const_def(ptr_id);
            format!("ptr_const_{}", type_name(pointee, type_pool))
        }
        TypeKind::PtrMut(ptr_id) => {
            let pointee = type_pool.ptr_mut_def(ptr_id);
            format!("ptr_mut_{}", type_name(pointee, type_pool))
        }
    }
}

/// Recursively collect all scalar vregs from a struct value.
///
/// This flattens any array fields to their scalar elements.
/// This is used during code generation to handle struct arguments that need
/// to be passed in registers or stored to memory slot by slot.
///
/// # Arguments
/// * `cfg` - The control flow graph containing the instructions
/// * `struct_slot_vregs` - Cache mapping CFG values to their slot vregs
/// * `value` - The CFG value to collect vregs from
/// * `get_vreg` - Closure to get/allocate a vreg for a given CFG value
pub fn collect_struct_scalar_vregs(
    cfg: &Cfg,
    struct_slot_vregs: &HashMap<CfgValue, Vec<VReg>>,
    value: CfgValue,
    get_vreg: &mut impl FnMut(CfgValue) -> VReg,
) -> Vec<VReg> {
    let inst = cfg.get_inst(value);
    match &inst.data {
        CfgInstData::StructInit {
            fields_start,
            fields_len,
            ..
        } => {
            let fields = cfg.get_extra(*fields_start, *fields_len);
            let mut result = Vec::new();
            for field in fields {
                let field_inst = cfg.get_inst(*field);
                if field_inst.ty.is_array() {
                    // Recursively collect from array field
                    result.extend(collect_array_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *field,
                        get_vreg,
                    ));
                } else if field_inst.ty.is_struct() {
                    // Recursively collect from nested struct field (includes builtin String)
                    result.extend(collect_struct_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *field,
                        get_vreg,
                    ));
                } else {
                    // Scalar field - get its vreg
                    result.push(get_vreg(*field));
                }
            }
            result
        }
        _ => {
            // For non-StructInit sources, try struct_slot_vregs cache
            if let Some(vregs) = struct_slot_vregs.get(&value).cloned() {
                vregs
            } else {
                vec![get_vreg(value)]
            }
        }
    }
}
