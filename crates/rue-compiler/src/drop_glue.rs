//! Drop glue synthesis for structs with non-trivial fields.
//!
//! When a struct has fields that need drop (like String), the compiler needs to
//! generate a "drop glue" function that drops each field. This is similar to
//! Rust's drop glue.
//!
//! For example, for a struct like:
//! ```text
//! struct Container {
//!     name: String,
//!     value: i32,
//! }
//! ```
//!
//! We generate a function `__rue_drop_Container` that:
//! 1. Receives the struct's flattened fields as parameters
//! 2. Drops each field that needs dropping (in declaration order)

use rue_air::{Air, AirInst, AirInstData, AirRef, AnalyzedFunction, ArrayTypeDef, StructDef, Type};
use rue_span::Span;

/// Check if a type needs drop.
fn type_needs_drop(ty: Type, struct_defs: &[StructDef], array_types: &[ArrayTypeDef]) -> bool {
    match ty {
        // Primitive types are trivially droppable
        Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::Bool
        | Type::Unit
        | Type::Never
        | Type::Error => false,

        // Enum types are trivially droppable (just discriminant values)
        Type::Enum(_) => false,

        // String needs drop - heap-allocated strings must be freed
        Type::String => true,

        // Struct types need drop if any field needs drop
        Type::Struct(struct_id) => {
            let struct_def = &struct_defs[struct_id.0 as usize];
            struct_def
                .fields
                .iter()
                .any(|f| type_needs_drop(f.ty, struct_defs, array_types))
        }

        // Array types need drop if element type needs drop
        Type::Array(array_id) => {
            let array_def = &array_types[array_id.0 as usize];
            type_needs_drop(array_def.element_type, struct_defs, array_types)
        }
    }
}

/// Count the number of ABI slots a type uses (flattened).
fn type_slot_count(ty: Type, struct_defs: &[StructDef], array_types: &[ArrayTypeDef]) -> u32 {
    match ty {
        // Primitives use 1 slot
        Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::Bool
        | Type::Unit
        | Type::Never
        | Type::Error
        | Type::Enum(_) => 1,

        // String uses 3 slots (ptr, len, cap)
        Type::String => 3,

        // Struct uses sum of all field slots
        Type::Struct(struct_id) => {
            let struct_def = &struct_defs[struct_id.0 as usize];
            struct_def
                .fields
                .iter()
                .map(|f| type_slot_count(f.ty, struct_defs, array_types))
                .sum()
        }

        // Array uses element slots * length
        Type::Array(array_id) => {
            let array_def = &array_types[array_id.0 as usize];
            type_slot_count(array_def.element_type, struct_defs, array_types)
                * array_def.length as u32
        }
    }
}

/// Synthesize drop glue functions for all structs that need them.
///
/// Returns a list of synthesized functions that should be added to the compilation.
pub fn synthesize_drop_glue(
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
) -> Vec<AnalyzedFunction> {
    let mut drop_glue_functions = Vec::new();

    for (struct_idx, struct_def) in struct_defs.iter().enumerate() {
        // Skip structs that don't need drop
        let struct_id = rue_air::StructId(struct_idx as u32);
        let struct_ty = Type::Struct(struct_id);
        if !type_needs_drop(struct_ty, struct_defs, array_types) {
            continue;
        }

        // Create drop glue function
        let func = create_drop_glue_function(struct_def, struct_id, struct_defs, array_types);
        drop_glue_functions.push(func);
    }

    drop_glue_functions
}

/// Create a drop glue function for a single struct.
fn create_drop_glue_function(
    struct_def: &StructDef,
    _struct_id: rue_air::StructId,
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
) -> AnalyzedFunction {
    let fn_name = format!("__rue_drop_{}", struct_def.name);
    let span = Span::new(0, 0); // Synthetic span

    // Create AIR for the drop glue function
    let mut air = Air::new(Type::Unit);

    // Calculate total parameter slots
    let mut num_param_slots = 0u32;
    for field in &struct_def.fields {
        num_param_slots += type_slot_count(field.ty, struct_defs, array_types);
    }

    // Collect drop statements - these are side-effects that must be executed
    let mut drop_statements = Vec::new();

    // For each field that needs drop, emit a Drop instruction.
    // We need to reconstruct the field values from the flattened parameters.
    let mut current_param_slot = 0u32;

    for field in &struct_def.fields {
        let field_slot_count = type_slot_count(field.ty, struct_defs, array_types);

        if type_needs_drop(field.ty, struct_defs, array_types) {
            // Emit Drop for this field
            // For now, we handle String and nested structs
            match field.ty {
                Type::String => {
                    // String has 3 params (ptr, len, cap)
                    // We need to load all three and pass them to the Drop instruction
                    // The Drop instruction in AIR operates on a value, and the CFG/codegen
                    // will handle the flattening.

                    // For simplicity, emit a Param to get the first slot, then drop it.
                    // The codegen knows this is a String and will use all 3 slots.
                    let param_ref = air.add_inst(AirInst {
                        data: AirInstData::Param {
                            index: current_param_slot,
                        },
                        ty: Type::String,
                        span,
                    });
                    let drop_ref = air.add_inst(AirInst {
                        data: AirInstData::Drop { value: param_ref },
                        ty: Type::Unit,
                        span,
                    });
                    drop_statements.push(drop_ref);
                }
                Type::Struct(nested_struct_id) => {
                    // Nested struct - load it and drop it
                    // The recursive drop glue will handle its fields
                    let param_ref = air.add_inst(AirInst {
                        data: AirInstData::Param {
                            index: current_param_slot,
                        },
                        ty: Type::Struct(nested_struct_id),
                        span,
                    });
                    let drop_ref = air.add_inst(AirInst {
                        data: AirInstData::Drop { value: param_ref },
                        ty: Type::Unit,
                        span,
                    });
                    drop_statements.push(drop_ref);
                }
                // Arrays and other types can be added later
                _ => {}
            }
        }

        current_param_slot += field_slot_count;
    }

    // Create the unit value for return
    let unit_const = air.add_inst(AirInst {
        data: AirInstData::UnitConst,
        ty: Type::Unit,
        span,
    });

    // If we have drop statements, wrap them in a Block so they get executed
    // The CFG builder uses demand-driven lowering, so statements in a Block
    // are explicitly included as side-effects.
    let return_value = if drop_statements.is_empty() {
        unit_const
    } else {
        // Encode statements into extra array
        let stmt_u32s: Vec<u32> = drop_statements.iter().map(|r| r.as_u32()).collect();
        let stmts_start = air.add_extra(&stmt_u32s);
        let stmts_len = drop_statements.len() as u32;
        air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len,
                value: unit_const,
            },
            ty: Type::Unit,
            span,
        })
    };

    // Add return instruction
    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(return_value)),
        ty: Type::Unit,
        span,
    });

    // All parameters are passed by value (normal mode)
    let param_modes = vec![false; num_param_slots as usize];

    AnalyzedFunction {
        name: fn_name,
        air,
        num_locals: 0,
        num_param_slots,
        param_modes,
    }
}
