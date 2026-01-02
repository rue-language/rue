//! Shared type utilities for code generation backends.
//!
//! This module provides common functions for calculating type sizes and
//! field offsets, shared between x86_64 and aarch64 backends.
//!
//! As of Phase 3 (ADR-0024), all struct/enum lookups go through `TypeInternPool`
//! instead of separate `&[StructDef]` slices.

use rue_air::{ArrayTypeDef, ArrayTypeId, StructId, TypeInternPool};
use rue_cfg::{Cfg, CfgInstData, CfgValue, Type};
use std::collections::HashMap;

use crate::vreg::VReg;

/// Extract the ArrayTypeId from a Type::Array.
/// Returns None if the type is not an array type.
#[inline]
pub fn extract_array_type_id(ty: Type) -> Option<ArrayTypeId> {
    match ty {
        Type::Array(id) => Some(id),
        _ => None,
    }
}

/// Get the array type definition for an array type ID.
pub fn array_type_def<'a>(
    array_types: &'a [ArrayTypeDef],
    array_type_id: ArrayTypeId,
) -> Option<&'a ArrayTypeDef> {
    array_types.get(array_type_id.0 as usize)
}

/// Get the array type definition from a Type.
/// Returns None if the type is not an array type or if the ID is out of bounds.
#[inline]
pub fn array_type_def_from_type<'a>(
    array_types: &'a [ArrayTypeDef],
    ty: Type,
) -> Option<&'a ArrayTypeDef> {
    extract_array_type_id(ty).and_then(|id| array_type_def(array_types, id))
}

/// Get the length of an array from its Type.
/// Returns 0 if the type is not an array or the ID is invalid.
#[inline]
pub fn array_length_from_type(array_types: &[ArrayTypeDef], ty: Type) -> u64 {
    array_type_def_from_type(array_types, ty)
        .map(|def| def.length)
        .unwrap_or(0)
}

/// Calculate the slot count for a single element of an array from its Type.
#[inline]
pub fn array_element_slot_count_from_type(
    type_pool: &TypeInternPool,
    array_types: &[ArrayTypeDef],
    ty: Type,
) -> u32 {
    if let Some(def) = array_type_def_from_type(array_types, ty) {
        type_slot_count(type_pool, array_types, def.element_type)
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
pub fn type_slot_count(type_pool: &TypeInternPool, array_types: &[ArrayTypeDef], ty: Type) -> u32 {
    match ty {
        // Zero-sized types
        Type::Unit | Type::Never => 0,
        Type::Array(array_type_id) => {
            // Zero-length arrays naturally get 0 slots (0 * element_slots)
            if let Some(def) = array_type_def(array_types, array_type_id) {
                let elem_slots = type_slot_count(type_pool, array_types, def.element_type);
                (def.length as u32) * elem_slots
            } else {
                1
            }
        }
        Type::Struct(struct_id) => {
            // Sum the slot counts of all fields
            // Empty structs naturally get 0 slots here
            let struct_def = type_pool.struct_def(struct_id);
            let mut total = 0u32;
            for field in &struct_def.fields {
                total += type_slot_count(type_pool, array_types, field.ty);
            }
            total
        }
        // Scalars and other types use 1 slot
        _ => 1,
    }
}

/// Calculate the slot count for a single element of an array type.
pub fn array_element_slot_count(
    type_pool: &TypeInternPool,
    array_types: &[ArrayTypeDef],
    array_type_id: ArrayTypeId,
) -> u32 {
    if let Some(def) = array_type_def(array_types, array_type_id) {
        type_slot_count(type_pool, array_types, def.element_type)
    } else {
        1
    }
}

/// Calculate the slot offset for a field within a struct.
///
/// This accounts for the sizes of all preceding fields.
pub fn struct_field_slot_offset(
    type_pool: &TypeInternPool,
    array_types: &[ArrayTypeDef],
    struct_id: StructId,
    field_index: u32,
) -> u32 {
    let struct_def = type_pool.struct_def(struct_id);
    let mut offset = 0u32;
    for i in 0..(field_index as usize) {
        if let Some(field) = struct_def.fields.get(i) {
            offset += type_slot_count(type_pool, array_types, field.ty);
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
            for elem in elements {
                let elem_inst = cfg.get_inst(*elem);
                if matches!(elem_inst.ty, Type::Array(_)) {
                    // Recursively collect from nested array
                    result.extend(collect_array_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *elem,
                        get_vreg,
                    ));
                } else if matches!(elem_inst.ty, Type::Struct(_)) {
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
pub fn array_drop_glue_name(
    array_id: ArrayTypeId,
    array_types: &[ArrayTypeDef],
    type_pool: &TypeInternPool,
) -> String {
    let array_def = &array_types[array_id.0 as usize];
    let element_type_name = type_name(array_def.element_type, type_pool, array_types);
    format!(
        "__rue_drop_array_{}_{}",
        element_type_name, array_def.length
    )
}

/// Get a name for a type (used for generating drop glue function names).
fn type_name(ty: Type, type_pool: &TypeInternPool, array_types: &[ArrayTypeDef]) -> String {
    match ty {
        Type::I8 => "i8".to_string(),
        Type::I16 => "i16".to_string(),
        Type::I32 => "i32".to_string(),
        Type::I64 => "i64".to_string(),
        Type::U8 => "u8".to_string(),
        Type::U16 => "u16".to_string(),
        Type::U32 => "u32".to_string(),
        Type::U64 => "u64".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Unit => "unit".to_string(),
        Type::Never => "never".to_string(),
        Type::Error => "error".to_string(),
        // ComptimeType only exists at compile time, no runtime representation
        Type::ComptimeType => "comptime_type".to_string(),
        Type::Enum(enum_id) => format!("enum{}", enum_id.0),
        // Struct types include builtin types like String
        Type::Struct(struct_id) => type_pool.struct_def(struct_id).name.clone(),
        Type::Array(array_id) => {
            let array_def = &array_types[array_id.0 as usize];
            let elem_name = type_name(array_def.element_type, type_pool, array_types);
            format!("array_{}_{}", elem_name, array_def.length)
        }
        // Module types should never reach codegen (compile-time only)
        Type::Module(_) => "module".to_string(),
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
                if matches!(field_inst.ty, Type::Array(_)) {
                    // Recursively collect from array field
                    result.extend(collect_array_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *field,
                        get_vreg,
                    ));
                } else if matches!(field_inst.ty, Type::Struct(_)) {
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
