//! Drop glue synthesis for structs and arrays with non-trivial fields/elements.
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
//!
//! For arrays like `[String; 3]`, we generate a function `__rue_drop_array_String_3` that:
//! 1. Receives all element slots as parameters (flattened)
//! 2. Drops each element in index order (element 0 first, then 1, etc.)

use rue_air::{Air, AirInst, AirInstData, AnalyzedFunction, ArrayTypeDef, StructDef, Type};
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

        // Struct types need drop if they have a destructor (e.g., builtin String)
        // or if any field needs drop
        Type::Struct(struct_id) => {
            let struct_def = &struct_defs[struct_id.0 as usize];
            // Builtins with destructors (like String) need drop
            if struct_def.destructor.is_some() {
                return true;
            }
            // Otherwise, check if any field needs drop
            struct_def
                .fields
                .iter()
                .any(|f| type_needs_drop(f.ty, struct_defs, array_types))
        }

        // Note: String is now Type::Struct with is_builtin=true, handled above

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

        // Struct uses sum of all field slots (including builtin String with 3 fields)
        Type::Struct(struct_id) => {
            let struct_def = &struct_defs[struct_id.0 as usize];
            struct_def
                .fields
                .iter()
                .map(|f| type_slot_count(f.ty, struct_defs, array_types))
                .sum()
        }

        // Note: String is now Type::Struct with is_builtin=true, handled above

        // Array uses element slots * length
        Type::Array(array_id) => {
            let array_def = &array_types[array_id.0 as usize];
            type_slot_count(array_def.element_type, struct_defs, array_types)
                * array_def.length as u32
        }
    }
}

/// Synthesize drop glue functions for all structs and arrays that need them.
///
/// Returns a list of synthesized functions that should be added to the compilation.
pub fn synthesize_drop_glue(
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
) -> Vec<AnalyzedFunction> {
    let mut drop_glue_functions = Vec::new();

    // Create drop glue for structs
    for (struct_idx, struct_def) in struct_defs.iter().enumerate() {
        // Skip structs that don't need drop
        let struct_id = rue_air::StructId(struct_idx as u32);
        let struct_ty = Type::Struct(struct_id);
        if !type_needs_drop(struct_ty, struct_defs, array_types) {
            continue;
        }

        // Skip builtins that have runtime-provided destructors (e.g., String)
        // to avoid duplicate symbol errors. User-defined destructors still need
        // synthesized drop glue.
        if struct_def.is_builtin && struct_def.destructor.is_some() {
            continue;
        }

        // Create drop glue function for struct
        let func =
            create_struct_drop_glue_function(struct_def, struct_id, struct_defs, array_types);
        drop_glue_functions.push(func);
    }

    // Create drop glue for arrays
    for (array_idx, array_def) in array_types.iter().enumerate() {
        // Skip arrays that don't need drop
        let array_id = rue_air::ArrayTypeId(array_idx as u32);
        let array_ty = Type::Array(array_id);
        if !type_needs_drop(array_ty, struct_defs, array_types) {
            continue;
        }

        // Create drop glue function for array
        let func = create_array_drop_glue_function(array_def, array_id, struct_defs, array_types);
        drop_glue_functions.push(func);
    }

    drop_glue_functions
}

/// Create a drop glue function for a single struct.
fn create_struct_drop_glue_function(
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
            // Emit Drop for this field.
            // Type::Struct handles both user-defined structs and builtin String.
            match field.ty {
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
                Type::Array(array_id) => {
                    // Array field - load it and drop it
                    // The array drop glue will handle dropping each element
                    let param_ref = air.add_inst(AirInst {
                        data: AirInstData::Param {
                            index: current_param_slot,
                        },
                        ty: Type::Array(array_id),
                        span,
                    });
                    let drop_ref = air.add_inst(AirInst {
                        data: AirInstData::Drop { value: param_ref },
                        ty: Type::Unit,
                        span,
                    });
                    drop_statements.push(drop_ref);
                }
                // Other types don't need drop
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

/// Create a drop glue function for an array type.
///
/// The function receives all element slots as parameters (flattened) and drops
/// each element in index order.
fn create_array_drop_glue_function(
    array_def: &ArrayTypeDef,
    array_id: rue_air::ArrayTypeId,
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
) -> AnalyzedFunction {
    let fn_name = array_drop_glue_name(array_id, array_types, struct_defs);
    let span = Span::new(0, 0); // Synthetic span

    // Create AIR for the drop glue function
    let mut air = Air::new(Type::Unit);

    // Calculate total parameter slots (element slots * length)
    let element_slot_count = type_slot_count(array_def.element_type, struct_defs, array_types);
    let num_param_slots = element_slot_count * array_def.length as u32;

    // Collect drop statements for each element
    let mut drop_statements = Vec::new();

    // For each element, emit a Drop instruction.
    // Type::Struct handles both user-defined structs and builtin String.
    for elem_idx in 0..array_def.length {
        let current_param_slot = elem_idx as u32 * element_slot_count;

        // Emit Drop for this element
        match array_def.element_type {
            Type::Struct(struct_id) => {
                let param_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: current_param_slot,
                    },
                    ty: Type::Struct(struct_id),
                    span,
                });
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: param_ref },
                    ty: Type::Unit,
                    span,
                });
                drop_statements.push(drop_ref);
            }
            Type::Array(nested_array_id) => {
                let param_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: current_param_slot,
                    },
                    ty: Type::Array(nested_array_id),
                    span,
                });
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: param_ref },
                    ty: Type::Unit,
                    span,
                });
                drop_statements.push(drop_ref);
            }
            // Primitives don't need drop - this shouldn't happen since we check type_needs_drop
            _ => {}
        }
    }

    // Create the unit value for return
    let unit_const = air.add_inst(AirInst {
        data: AirInstData::UnitConst,
        ty: Type::Unit,
        span,
    });

    // If we have drop statements, wrap them in a Block so they get executed
    let return_value = if drop_statements.is_empty() {
        unit_const
    } else {
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

/// Generate the drop glue function name for an array type.
///
/// The name encodes the element type and length, e.g., `__rue_drop_array_String_3`
pub fn array_drop_glue_name(
    array_id: rue_air::ArrayTypeId,
    array_types: &[ArrayTypeDef],
    struct_defs: &[StructDef],
) -> String {
    let array_def = &array_types[array_id.0 as usize];
    let element_type_name = type_name(array_def.element_type, struct_defs, array_types);
    format!(
        "__rue_drop_array_{}_{}",
        element_type_name, array_def.length
    )
}

/// Get a name for a type (used for generating drop glue function names).
fn type_name(ty: Type, struct_defs: &[StructDef], array_types: &[ArrayTypeDef]) -> String {
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
        Type::Enum(enum_id) => format!("enum{}", enum_id.0),
        // Struct types include builtin types like String
        Type::Struct(struct_id) => struct_defs[struct_id.0 as usize].name.clone(),
        Type::Array(array_id) => {
            let array_def = &array_types[array_id.0 as usize];
            let elem_name = type_name(array_def.element_type, struct_defs, array_types);
            format!("array_{}_{}", elem_name, array_def.length)
        }
    }
}
