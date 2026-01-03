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

use rue_air::{
    Air, AirInst, AirInstData, AnalyzedFunction, StructDef, Type, TypeInternPool, TypeKind,
};
use rue_span::Span;

/// Check if a type needs drop.
fn type_needs_drop(ty: Type, type_pool: &TypeInternPool) -> bool {
    match ty.kind() {
        // Primitive types are trivially droppable
        // ComptimeType is comptime-only, no runtime representation
        TypeKind::I8
        | TypeKind::I16
        | TypeKind::I32
        | TypeKind::I64
        | TypeKind::U8
        | TypeKind::U16
        | TypeKind::U32
        | TypeKind::U64
        | TypeKind::Bool
        | TypeKind::Unit
        | TypeKind::Never
        | TypeKind::Error
        | TypeKind::ComptimeType => false,

        // Enum types are trivially droppable (just discriminant values)
        TypeKind::Enum(_) => false,

        // Struct types need drop if they have a destructor (e.g., builtin String)
        // or if any field needs drop
        TypeKind::Struct(struct_id) => {
            let struct_def = type_pool.struct_def(struct_id);
            // Builtins with destructors (like String) need drop
            if struct_def.destructor.is_some() {
                return true;
            }
            // Otherwise, check if any field needs drop
            struct_def
                .fields
                .iter()
                .any(|f| type_needs_drop(f.ty, type_pool))
        }

        // Note: String is now Type::Struct with is_builtin=true, handled above

        // Array types need drop if element type needs drop
        TypeKind::Array(array_id) => {
            let (element_type, _length) = type_pool.array_def(array_id);
            type_needs_drop(element_type, type_pool)
        }

        // Pointer types don't need drop (they're just addresses)
        TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => false,

        // Module types don't need drop (compile-time only)
        TypeKind::Module(_) => false,
    }
}

/// Count the number of ABI slots a type uses (flattened).
fn type_slot_count(ty: Type, type_pool: &TypeInternPool) -> u32 {
    match ty.kind() {
        // Primitives use 1 slot
        // ComptimeType uses 0 slots (comptime-only, no runtime representation)
        TypeKind::I8
        | TypeKind::I16
        | TypeKind::I32
        | TypeKind::I64
        | TypeKind::U8
        | TypeKind::U16
        | TypeKind::U32
        | TypeKind::U64
        | TypeKind::Bool
        | TypeKind::Unit
        | TypeKind::Never
        | TypeKind::Error
        | TypeKind::Enum(_) => 1,
        TypeKind::ComptimeType => 0,

        // Struct uses sum of all field slots (including builtin String with 3 fields)
        TypeKind::Struct(struct_id) => {
            let struct_def = type_pool.struct_def(struct_id);
            struct_def
                .fields
                .iter()
                .map(|f| type_slot_count(f.ty, type_pool))
                .sum()
        }

        // Note: String is now Type::Struct with is_builtin=true, handled above

        // Array uses element slots * length
        TypeKind::Array(array_id) => {
            let (element_type, length) = type_pool.array_def(array_id);
            type_slot_count(element_type, type_pool) * length as u32
        }

        // Pointer types use 1 slot (they're 64-bit addresses)
        TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => 1,

        // Module types don't take ABI slots (compile-time only)
        TypeKind::Module(_) => 0,
    }
}

/// Synthesize drop glue functions for all structs and arrays that need them.
///
/// Returns a list of synthesized functions that should be added to the compilation.
pub fn synthesize_drop_glue(type_pool: &TypeInternPool) -> Vec<AnalyzedFunction> {
    let mut drop_glue_functions = Vec::new();

    // Create drop glue for structs
    for struct_id in type_pool.all_struct_ids() {
        let struct_def = type_pool.struct_def(struct_id);
        // Skip structs that don't need drop
        let struct_ty = Type::Struct(struct_id);
        if !type_needs_drop(struct_ty, type_pool) {
            continue;
        }

        // Skip builtins that have runtime-provided destructors (e.g., String)
        // to avoid duplicate symbol errors. User-defined destructors still need
        // synthesized drop glue.
        if struct_def.is_builtin && struct_def.destructor.is_some() {
            continue;
        }

        // Create drop glue function for struct
        let func = create_struct_drop_glue_function(&struct_def, struct_id, type_pool);
        drop_glue_functions.push(func);
    }

    // Create drop glue for arrays
    for array_id in type_pool.all_array_ids() {
        // Skip arrays that don't need drop
        let array_ty = Type::Array(array_id);
        if !type_needs_drop(array_ty, type_pool) {
            continue;
        }

        // Create drop glue function for array
        let func = create_array_drop_glue_function(array_id, type_pool);
        drop_glue_functions.push(func);
    }

    drop_glue_functions
}

/// Create a drop glue function for a single struct.
fn create_struct_drop_glue_function(
    struct_def: &StructDef,
    _struct_id: rue_air::StructId,
    type_pool: &TypeInternPool,
) -> AnalyzedFunction {
    let fn_name = format!("__rue_drop_{}", struct_def.name);
    let span = Span::new(0, 0); // Synthetic span

    // Create AIR for the drop glue function
    let mut air = Air::new(Type::Unit);

    // Calculate total parameter slots
    let mut num_param_slots = 0u32;
    for field in &struct_def.fields {
        num_param_slots += type_slot_count(field.ty, type_pool);
    }

    // Collect drop statements - these are side-effects that must be executed
    let mut drop_statements = Vec::new();

    // For each field that needs drop, emit a Drop instruction.
    // We need to reconstruct the field values from the flattened parameters.
    let mut current_param_slot = 0u32;

    for field in &struct_def.fields {
        let field_slot_count = type_slot_count(field.ty, type_pool);

        if type_needs_drop(field.ty, type_pool) {
            // Emit Drop for this field.
            // Type::Struct handles both user-defined structs and builtin String.
            match field.ty.kind() {
                TypeKind::Struct(nested_struct_id) => {
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
                TypeKind::Array(array_id) => {
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
    array_id: rue_air::ArrayTypeId,
    type_pool: &TypeInternPool,
) -> AnalyzedFunction {
    let fn_name = array_drop_glue_name(array_id, type_pool);
    let span = Span::new(0, 0); // Synthetic span

    // Get array element type and length
    let (element_type, length) = type_pool.array_def(array_id);

    // Create AIR for the drop glue function
    let mut air = Air::new(Type::Unit);

    // Calculate total parameter slots (element slots * length)
    let element_slot_count = type_slot_count(element_type, type_pool);
    let num_param_slots = element_slot_count * length as u32;

    // Collect drop statements for each element
    let mut drop_statements = Vec::new();

    // For each element, emit a Drop instruction.
    // Type::Struct handles both user-defined structs and builtin String.
    for elem_idx in 0..length {
        let current_param_slot = elem_idx as u32 * element_slot_count;

        // Emit Drop for this element
        match element_type.kind() {
            TypeKind::Struct(struct_id) => {
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
            TypeKind::Array(nested_array_id) => {
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
pub fn array_drop_glue_name(array_id: rue_air::ArrayTypeId, type_pool: &TypeInternPool) -> String {
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
        // ComptimeType only exists at compile time
        TypeKind::ComptimeType => "comptime_type".to_string(),
        TypeKind::Enum(enum_id) => format!("enum{}", enum_id.0),
        // Struct types include builtin types like String
        TypeKind::Struct(struct_id) => type_pool.struct_def(struct_id).name.clone(),
        TypeKind::Array(array_id) => {
            let (element_type, length) = type_pool.array_def(array_id);
            let elem_name = type_name(element_type, type_pool);
            format!("array_{}_{}", elem_name, length)
        }
        TypeKind::PtrConst(ptr_id) => {
            let pointee_type = type_pool.get_ptr_pointee(ptr_id);
            let pointee_name = type_name(pointee_type, type_pool);
            format!("ptr_const_{}", pointee_name)
        }
        TypeKind::PtrMut(ptr_id) => {
            let pointee_type = type_pool.get_ptr_pointee(ptr_id);
            let pointee_name = type_name(pointee_type, type_pool);
            format!("ptr_mut_{}", pointee_name)
        }
        // Module types should never reach drop glue (compile-time only)
        TypeKind::Module(_) => "module".to_string(),
    }
}
