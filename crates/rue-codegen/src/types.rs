//! Shared type utilities for code generation backends.
//!
//! This module provides common functions for calculating type sizes and
//! field offsets, shared between x86_64 and aarch64 backends.

use rue_air::{ArrayTypeDef, ArrayTypeId};
use rue_cfg::{Cfg, CfgInstData, CfgValue, StructDef, StructId, Type};
use std::collections::HashMap;

use crate::vreg::VReg;

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
/// Zero-sized types (unit, never, empty structs, zero-length arrays) return 0.
pub fn type_slot_count(struct_defs: &[StructDef], array_types: &[ArrayTypeDef], ty: Type) -> u32 {
    match ty {
        // Zero-sized types
        Type::Unit | Type::Never => 0,
        Type::Array(array_type_id) => {
            // Zero-length arrays naturally get 0 slots (0 * element_slots)
            if let Some(def) = array_type_def(array_types, array_type_id) {
                let elem_slots = type_slot_count(struct_defs, array_types, def.element_type);
                (def.length as u32) * elem_slots
            } else {
                1
            }
        }
        Type::Struct(struct_id) => {
            // Sum the slot counts of all fields
            // Empty structs naturally get 0 slots here
            if let Some(struct_def) = struct_defs.get(struct_id.0 as usize) {
                let mut total = 0u32;
                for field in &struct_def.fields {
                    total += type_slot_count(struct_defs, array_types, field.ty);
                }
                total
            } else {
                1
            }
        }
        // Strings are (ptr + len + cap), so 3 slots
        Type::String => 3,
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
                    // Recursively collect from struct element
                    result.extend(collect_struct_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *elem,
                        get_vreg,
                    ));
                } else if elem_inst.ty == Type::String {
                    // String element - has 3 slots (ptr, len, cap)
                    if let Some(vregs) = struct_slot_vregs.get(elem).cloned() {
                        result.extend(vregs);
                    } else {
                        result.push(get_vreg(*elem));
                    }
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
                    // Recursively collect from nested struct field
                    result.extend(collect_struct_scalar_vregs(
                        cfg,
                        struct_slot_vregs,
                        *field,
                        get_vreg,
                    ));
                } else if field_inst.ty == Type::String {
                    // String field - has 3 slots (ptr, len, cap)
                    // Look up the field's slot vregs from the cache
                    if let Some(vregs) = struct_slot_vregs.get(field).cloned() {
                        result.extend(vregs);
                    } else {
                        // Fallback: just get the main vreg (should not happen for properly lowered String)
                        result.push(get_vreg(*field));
                    }
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
