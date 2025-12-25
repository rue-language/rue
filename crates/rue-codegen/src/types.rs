//! Shared type utilities for code generation backends.
//!
//! This module provides common functions for calculating type sizes and
//! field offsets, shared between x86_64 and aarch64 backends.

use rue_air::{ArrayTypeDef, ArrayTypeId};
use rue_cfg::{StructDef, StructId, Type};

/// Get the array type definition for an array type ID.
pub fn array_type_def<'a>(
    array_types: &'a [ArrayTypeDef],
    array_type_id: ArrayTypeId,
) -> Option<&'a ArrayTypeDef> {
    array_types.get(array_type_id.0 as usize)
}

/// Calculate the total number of slots needed to store a type.
///
/// For scalars, this is 1. For arrays, it's `length * slot_count(element_type)`.
/// For structs, this is the sum of slot counts for all fields.
/// For nested types, this recursively calculates.
pub fn type_slot_count(struct_defs: &[StructDef], array_types: &[ArrayTypeDef], ty: Type) -> u32 {
    match ty {
        Type::Array(array_type_id) => {
            if let Some(def) = array_type_def(array_types, array_type_id) {
                let elem_slots = type_slot_count(struct_defs, array_types, def.element_type);
                (def.length as u32) * elem_slots
            } else {
                1
            }
        }
        Type::Struct(struct_id) => {
            // Sum the slot counts of all fields
            if let Some(struct_def) = struct_defs.get(struct_id.0 as usize) {
                let mut total = 0u32;
                for field in &struct_def.fields {
                    total += type_slot_count(struct_defs, array_types, field.ty);
                }
                total.max(1) // At least 1 slot even for empty struct
            } else {
                1
            }
        }
        _ => 1,
    }
}

/// Calculate the slot count for a single element of an array type.
pub fn array_element_slot_count(
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
    array_type_id: ArrayTypeId,
) -> u32 {
    if let Some(def) = array_type_def(array_types, array_type_id) {
        type_slot_count(struct_defs, array_types, def.element_type)
    } else {
        1
    }
}

/// Calculate the slot offset for a field within a struct.
///
/// This accounts for the sizes of all preceding fields.
pub fn struct_field_slot_offset(
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
    struct_id: StructId,
    field_index: u32,
) -> u32 {
    if let Some(struct_def) = struct_defs.get(struct_id.0 as usize) {
        let mut offset = 0u32;
        for i in 0..(field_index as usize) {
            if let Some(field) = struct_def.fields.get(i) {
                offset += type_slot_count(struct_defs, array_types, field.ty);
            }
        }
        offset
    } else {
        field_index // Fallback to field index if struct not found
    }
}
