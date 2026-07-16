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
    Air, AirInst, AirInstData, AirPattern, AirRef, AnalyzedFunction, EnumId, FrozenTypeInternPool,
    StructDef, Type, TypeKind,
};
use rue_span::Span;

/// Check if a type needs drop.
fn type_needs_drop(ty: Type, type_pool: &FrozenTypeInternPool) -> bool {
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

        // An enum needs drop if any variant payload needs drop (RUE-221): at
        // scope exit its active-variant drop glue switches on the discriminant
        // and drops the selected payload. Discriminant-only enums are trivial.
        TypeKind::Enum(enum_id) => {
            let enum_def = type_pool.enum_def(enum_id);
            enum_def
                .variant_payloads
                .iter()
                .flatten()
                .any(|&ty| type_needs_drop(ty, type_pool))
        }

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

/// Synthesize drop glue functions for all structs and arrays that need them.
///
/// Returns a list of synthesized functions that should be added to the compilation.
pub fn synthesize_drop_glue(type_pool: &FrozenTypeInternPool) -> Vec<AnalyzedFunction> {
    let mut drop_glue_functions = Vec::new();

    // Create drop glue for structs
    for struct_id in type_pool.all_struct_ids() {
        let struct_def = type_pool.struct_def(struct_id);
        // Skip structs that don't need drop
        let struct_ty = Type::new_struct(struct_id);
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
        let func = create_struct_drop_glue_function(struct_def, struct_id, type_pool);
        drop_glue_functions.push(func);
    }

    // Create drop glue for arrays
    for array_id in type_pool.all_array_ids() {
        // Skip arrays that don't need drop
        let array_ty = Type::new_array(array_id);
        if !type_needs_drop(array_ty, type_pool) {
            continue;
        }

        // Create drop glue function for array
        let func = create_array_drop_glue_function(array_id, type_pool);
        drop_glue_functions.push(func);
    }

    // Create drop glue for payload-carrying enums (RUE-221). The glue switches
    // on the discriminant and drops the active variant's payload.
    for enum_id in type_pool.all_enum_ids() {
        let enum_ty = Type::new_enum(enum_id);
        if !type_needs_drop(enum_ty, type_pool) {
            continue;
        }

        let func = create_enum_drop_glue_function(enum_id, type_pool);
        drop_glue_functions.push(func);
    }

    drop_glue_functions
}

/// Create a drop glue function for a single struct.
fn create_struct_drop_glue_function(
    struct_def: &StructDef,
    struct_id: rue_air::StructId,
    type_pool: &FrozenTypeInternPool,
) -> AnalyzedFunction {
    // File-qualified when the struct name spans files (RUE-571) — must match
    // the call side in both codegen backends' cfg_lower.
    let fn_name = format!("__rue_drop_{}", type_pool.struct_symbol_name(struct_id));
    let span = Span::new(0, 0); // Synthetic span

    // Create AIR for the drop glue function
    let mut air = Air::new(Type::UNIT);

    // Use the canonical aggregate layout query for the complete flattened ABI.
    let num_param_slots = type_pool.abi_slot_count(Type::new_struct(struct_id));

    // Collect drop statements - these are side-effects that must be executed
    let mut drop_statements = Vec::new();

    // For each field that needs drop, emit a Drop instruction.
    // We need to reconstruct the field values from the flattened parameters.
    let mut current_param_slot = 0u32;

    for field in &struct_def.fields {
        let field_slot_count = type_pool.abi_slot_count(field.ty);

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
                        ty: Type::new_struct(nested_struct_id),
                        span,
                    });
                    let drop_ref = air.add_inst(AirInst {
                        data: AirInstData::Drop { value: param_ref },
                        ty: Type::UNIT,
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
                        ty: Type::new_array(array_id),
                        span,
                    });
                    let drop_ref = air.add_inst(AirInst {
                        data: AirInstData::Drop { value: param_ref },
                        ty: Type::UNIT,
                        span,
                    });
                    drop_statements.push(drop_ref);
                }
                TypeKind::Enum(enum_id) => {
                    // Payload-carrying enum field - load it and drop it. The
                    // enum drop glue dispatches on the active discriminant and
                    // drops only the selected variant's payload.
                    let param_ref = air.add_inst(AirInst {
                        data: AirInstData::Param {
                            index: current_param_slot,
                        },
                        ty: Type::new_enum(enum_id),
                        span,
                    });
                    let drop_ref = air.add_inst(AirInst {
                        data: AirInstData::Drop { value: param_ref },
                        ty: Type::UNIT,
                        span,
                    });
                    drop_statements.push(drop_ref);
                }
                _ => {}
            }
        }

        current_param_slot += field_slot_count;
    }

    // Create the unit value for return
    let unit_const = air.add_inst(AirInst {
        data: AirInstData::UnitConst,
        ty: Type::UNIT,
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
            ty: Type::UNIT,
            span,
        })
    };

    // Add return instruction
    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(return_value)),
        ty: Type::UNIT,
        span,
    });

    // All parameters are passed by value (normal mode)
    let param_modes = vec![false; num_param_slots as usize];

    AnalyzedFunction {
        ordinary_owner: None,
        name: fn_name,
        implicit_drop_source: if struct_def.name.starts_with("__anon_struct_") {
            None
        } else {
            Some(rue_air::ImplicitDropDependencySourceEvent::NamedStruct {
                file: struct_def.file_id.index(),
                name: struct_def.name.clone(),
            })
        },
        air,
        num_locals: 0,
        num_param_slots,
        param_modes: param_modes.into(),
        allow_unreachable_code: false,
    }
}

/// Create a drop glue function for an array type.
///
/// The function receives all element slots as parameters (flattened) and drops
/// each element in index order.
fn create_array_drop_glue_function(
    array_id: rue_air::ArrayTypeId,
    type_pool: &FrozenTypeInternPool,
) -> AnalyzedFunction {
    let fn_name = array_drop_glue_name(array_id, type_pool);
    let span = Span::new(0, 0); // Synthetic span

    // Get array element type and length
    let (element_type, length) = type_pool.array_def(array_id);

    // Create AIR for the drop glue function
    let mut air = Air::new(Type::UNIT);

    // Use the canonical aggregate layout query for both the element stride and
    // the complete flattened ABI.
    let element_slot_count = type_pool.abi_slot_count(element_type);
    let num_param_slots = type_pool.abi_slot_count(Type::new_array(array_id));

    // Collect drop statements for each element
    let mut drop_statements = Vec::new();

    // For each element, emit a Drop instruction, in ASCENDING index order
    // (element 0 dropped first — the language-visible order the oracle checks).
    //
    // The flattened parameter slots are in LOGICAL order (ADR-0040 / RUE-311):
    // element 0 occupies the first element-chunk, element N-1 the last. So
    // iterate chunks front-to-back to drop in ascending index order. Each chunk
    // is read back as one aggregate `Param`, which the codegen `Drop` lowering
    // hands over in the reversed by-value ABI order, so the drop caller reverses
    // each element's own slots to compensate (see the array `Drop` handler).
    // Type::Struct handles both user-defined structs and builtin String.
    for phys_chunk in 0..length {
        let current_param_slot = phys_chunk as u32 * element_slot_count;

        // Emit Drop for this element
        match element_type.kind() {
            TypeKind::Struct(struct_id) => {
                let param_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: current_param_slot,
                    },
                    ty: Type::new_struct(struct_id),
                    span,
                });
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: param_ref },
                    ty: Type::UNIT,
                    span,
                });
                drop_statements.push(drop_ref);
            }
            TypeKind::Array(nested_array_id) => {
                let param_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: current_param_slot,
                    },
                    ty: Type::new_array(nested_array_id),
                    span,
                });
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: param_ref },
                    ty: Type::UNIT,
                    span,
                });
                drop_statements.push(drop_ref);
            }
            TypeKind::Enum(enum_id) => {
                let param_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: current_param_slot,
                    },
                    ty: Type::new_enum(enum_id),
                    span,
                });
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: param_ref },
                    ty: Type::UNIT,
                    span,
                });
                drop_statements.push(drop_ref);
            }
            _ => {}
        }
    }

    // Create the unit value for return
    let unit_const = air.add_inst(AirInst {
        data: AirInstData::UnitConst,
        ty: Type::UNIT,
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
            ty: Type::UNIT,
            span,
        })
    };

    // Add return instruction
    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(return_value)),
        ty: Type::UNIT,
        span,
    });

    // All parameters are passed by value (normal mode)
    let param_modes = vec![false; num_param_slots as usize];

    AnalyzedFunction {
        ordinary_owner: None,
        name: fn_name,
        implicit_drop_source: None,
        air,
        num_locals: 0,
        num_param_slots,
        param_modes: param_modes.into(),
        allow_unreachable_code: false,
    }
}

/// Create a drop glue function for a payload-carrying enum type (RUE-221).
///
/// The function receives the enum's flattened slots as parameters: slot 0 is
/// the discriminant, slots 1.. are the payload union sized to the largest
/// variant. It switches on the discriminant and, for the active variant, drops
/// each droppable payload field in declaration order. Variants whose payload
/// needs no drop (and discriminant-only variants) fall through to a no-op
/// wildcard default arm, so exactly the active variant's payload is dropped.
fn create_enum_drop_glue_function(
    enum_id: EnumId,
    type_pool: &FrozenTypeInternPool,
) -> AnalyzedFunction {
    let enum_def = type_pool.enum_def(enum_id);
    let fn_name = enum_drop_glue_name(enum_id, type_pool);
    let span = Span::new(0, 0); // Synthetic span

    let mut air = Air::new(Type::UNIT);

    // Total ABI slots: discriminant (slot 0) + payload area (largest variant).
    let num_param_slots = type_pool.abi_slot_count(Type::new_enum(enum_id));

    // The discriminant lives in param slot 0; the match switches on it.
    let disc_ty = enum_def.discriminant_type();
    let disc_param = air.add_inst(AirInst {
        data: AirInstData::Param { index: 0 },
        ty: disc_ty,
        span,
    });

    // A single shared unit value for every arm body and the outer block.
    let unit_const = air.add_inst(AirInst {
        data: AirInstData::UnitConst,
        ty: Type::UNIT,
        span,
    });

    // Build one Int-pattern arm per variant that carries a droppable payload.
    // Payload fields overlay the union starting at slot 1, so field j of a
    // variant sits at slot `1 + sum(slot_count(field_k) for k < j)`.
    let mut arm_data: Vec<u32> = Vec::new();
    let mut arm_count = 0u32;

    for variant_index in 0..enum_def.variant_count() {
        let payload = enum_def.variant_payload(variant_index);
        if !payload.iter().any(|&ty| type_needs_drop(ty, type_pool)) {
            continue;
        }

        let mut drop_stmts: Vec<AirRef> = Vec::new();
        let mut field_slot = 1u32; // slot 0 is the discriminant
        for &field_ty in payload {
            let field_slots = type_pool.abi_slot_count(field_ty);
            if type_needs_drop(field_ty, type_pool) {
                let param_ref = air.add_inst(AirInst {
                    data: AirInstData::Param { index: field_slot },
                    ty: field_ty,
                    span,
                });
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: param_ref },
                    ty: Type::UNIT,
                    span,
                });
                drop_stmts.push(drop_ref);
            }
            field_slot += field_slots;
        }

        // Arm body: a Block running the drops, yielding unit.
        let stmt_u32s: Vec<u32> = drop_stmts.iter().map(|r| r.as_u32()).collect();
        let stmts_start = air.add_extra(&stmt_u32s);
        let stmts_len = drop_stmts.len() as u32;
        let arm_body = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len,
                value: unit_const,
            },
            ty: Type::UNIT,
            span,
        });

        AirPattern::Int(variant_index as i64).encode(arm_body, &mut arm_data);
        arm_count += 1;
    }

    // A wildcard default arm (drops nothing) covers variants with no droppable
    // payload and keeps the switch total for codegen.
    AirPattern::Wildcard.encode(unit_const, &mut arm_data);
    arm_count += 1;

    let arms_start = air.add_extra(&arm_data);
    let match_ref = air.add_inst(AirInst {
        data: AirInstData::Match {
            scrutinee: disc_param,
            arms_start,
            arms_len: arm_count,
        },
        ty: Type::UNIT,
        span,
    });

    // Return unit; the match runs as a side-effecting statement of the body.
    let body_stmts = [match_ref.as_u32()];
    let body_start = air.add_extra(&body_stmts);
    let body = air.add_inst(AirInst {
        data: AirInstData::Block {
            stmts_start: body_start,
            stmts_len: 1,
            value: unit_const,
        },
        ty: Type::UNIT,
        span,
    });

    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(body)),
        ty: Type::UNIT,
        span,
    });

    let param_modes = vec![false; num_param_slots as usize];

    AnalyzedFunction {
        ordinary_owner: None,
        name: fn_name,
        implicit_drop_source: Some(rue_air::ImplicitDropDependencySourceEvent::NamedEnum {
            file: enum_def.file_id.index(),
            name: enum_def.name.clone(),
        }),
        air,
        num_locals: 0,
        num_param_slots,
        param_modes: param_modes.into(),
        allow_unreachable_code: false,
    }
}

/// Generate the drop glue function name for a payload-carrying enum type.
///
/// Types share a namespace, so the enum's own name cannot collide with a
/// struct's `__rue_drop_<name>` glue.
pub fn enum_drop_glue_name(enum_id: EnumId, type_pool: &FrozenTypeInternPool) -> String {
    // File-qualified when the enum name spans files (RUE-571).
    format!("__rue_drop_{}", type_pool.enum_symbol_name(enum_id))
}

/// Generate the drop glue function name for an array type.
///
/// The name encodes the element type and length, e.g., `__rue_drop_array_String_3`
pub fn array_drop_glue_name(
    array_id: rue_air::ArrayTypeId,
    type_pool: &FrozenTypeInternPool,
) -> String {
    let (element_type, length) = type_pool.array_def(array_id);
    let element_type_name = type_name(element_type, type_pool);
    format!("__rue_drop_array_{}_{}", element_type_name, length)
}

/// Get a name for a type (used for generating drop glue function names).
fn type_name(ty: Type, type_pool: &FrozenTypeInternPool) -> String {
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
        // Struct types include builtin types like String. File-qualified
        // when the struct name spans files (RUE-571), so arrays of two
        // same-named element types get distinct glue.
        TypeKind::Struct(struct_id) => type_pool.struct_symbol_name(struct_id),
        TypeKind::Array(array_id) => {
            let (element_type, length) = type_pool.array_def(array_id);
            let elem_name = type_name(element_type, type_pool);
            format!("array_{}_{}", elem_name, length)
        }
        // Module types should never reach drop glue (compile-time only)
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

#[cfg(test)]
mod tests {
    use lasso::ThreadedRodeo;
    use rue_air::{EnumDef, StructField, TypeInternPool};

    use super::*;

    fn register_struct(
        type_pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        fields: Vec<StructField>,
        destructor: Option<&str>,
    ) -> rue_air::StructId {
        let symbol = interner.get_or_intern(name);
        type_pool
            .register_struct(
                symbol,
                StructDef {
                    name: name.to_string(),
                    fields,
                    is_copy: false,
                    is_linear: false,
                    destructor: destructor.map(str::to_string),
                    is_builtin: false,
                    is_pub: false,
                    file_id: rue_span::FileId::DEFAULT,
                },
            )
            .0
    }

    fn register_enum(
        type_pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        payloads: Vec<Vec<Type>>,
    ) -> EnumId {
        let symbol = interner.get_or_intern(name);
        let variants = (0..payloads.len()).map(|i| format!("V{i}")).collect();
        type_pool
            .register_enum(
                symbol,
                EnumDef {
                    name: name.to_string(),
                    variants,
                    variant_payloads: payloads,
                    is_pub: false,
                    file_id: rue_span::FileId::DEFAULT,
                },
            )
            .0
    }

    fn param_indices(function: &AnalyzedFunction) -> Vec<u32> {
        function
            .air
            .instructions()
            .iter()
            .filter_map(|inst| match inst.data {
                AirInstData::Param { index } => Some(index),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn synthesized_drop_glue_uses_canonical_abi_widths_for_totals_and_offsets() {
        let type_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();

        let drop_id = register_struct(
            &type_pool,
            &interner,
            "DropOne",
            vec![StructField {
                name: "value".into(),
                ty: Type::I32,
            }],
            Some("DropOne.__drop"),
        );
        let drop_ty = Type::new_struct(drop_id);

        let zst_mixed_id = register_struct(
            &type_pool,
            &interner,
            "ZstMixed",
            vec![
                StructField {
                    name: "leading".into(),
                    ty: Type::UNIT,
                },
                StructField {
                    name: "value".into(),
                    ty: Type::I64,
                },
                StructField {
                    name: "interior".into(),
                    ty: Type::NEVER,
                },
                StructField {
                    name: "tail".into(),
                    ty: Type::BOOL,
                },
            ],
            None,
        );
        let zst_mixed_ty = Type::new_struct(zst_mixed_id);
        let plain_enum_id = register_enum(
            &type_pool,
            &interner,
            "Plain",
            vec![vec![Type::UNIT], vec![Type::I32, Type::UNIT]],
        );
        let plain_enum_ty = Type::new_enum(plain_enum_id);
        let trivial_array_id = type_pool.intern_array_from_type(zst_mixed_ty, 2);
        let trivial_array_ty = Type::new_array(trivial_array_id);
        let const_ptr = Type::new_ptr_const(type_pool.intern_ptr_const_from_type(Type::UNIT));
        let mut_ptr = Type::new_ptr_mut(type_pool.intern_ptr_mut_from_type(Type::NEVER));

        let nested_id = register_struct(
            &type_pool,
            &interner,
            "NestedDrop",
            vec![
                StructField {
                    name: "leading".into(),
                    ty: Type::UNIT,
                },
                StructField {
                    name: "drop".into(),
                    ty: drop_ty,
                },
                StructField {
                    name: "interior".into(),
                    ty: Type::UNIT,
                },
                StructField {
                    name: "tail".into(),
                    ty: Type::I32,
                },
            ],
            None,
        );
        let nested_ty = Type::new_struct(nested_id);
        let drop_array_id = type_pool.intern_array_from_type(drop_ty, 2);
        let drop_array_ty = Type::new_array(drop_array_id);
        let drop_enum_id = register_enum(
            &type_pool,
            &interner,
            "DropEnum",
            vec![
                vec![Type::UNIT, drop_ty, Type::UNIT, drop_ty],
                vec![Type::I32, drop_ty],
            ],
        );
        let drop_enum_ty = Type::new_enum(drop_enum_id);

        // Every runtime TypeKind occurs either directly or through one of the
        // aggregate shapes below. Recovery and compile-time-only types are not
        // valid backend structural children. Keeping the droppable fields last
        // makes their Param indices a compact assertion over every preceding
        // canonical width.
        let outer_id = register_struct(
            &type_pool,
            &interner,
            "AllKinds",
            vec![
                Type::I8,
                Type::I16,
                Type::I32,
                Type::I64,
                Type::U8,
                Type::U16,
                Type::U32,
                Type::U64,
                Type::BOOL,
                Type::UNIT,
                Type::NEVER,
                const_ptr,
                mut_ptr,
                zst_mixed_ty,
                plain_enum_ty,
                trivial_array_ty,
                drop_ty,
                nested_ty,
                drop_array_ty,
                drop_enum_ty,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, ty)| StructField {
                name: format!("field{i}"),
                ty,
            })
            .collect(),
            None,
        );
        let outer_ty = Type::new_struct(outer_id);
        let type_pool = type_pool.freeze();
        let outer =
            create_struct_drop_glue_function(type_pool.struct_def(outer_id), outer_id, &type_pool);
        assert_eq!(outer.num_param_slots, type_pool.abi_slot_count(outer_ty));
        assert_eq!(outer.num_param_slots, 27);
        assert_eq!(param_indices(&outer), [19, 20, 22, 24]);

        let array = create_array_drop_glue_function(drop_array_id, &type_pool);
        assert_eq!(
            array.num_param_slots,
            type_pool.abi_slot_count(drop_array_ty)
        );
        assert_eq!(param_indices(&array), [0, 1]);

        let enum_glue = create_enum_drop_glue_function(drop_enum_id, &type_pool);
        assert_eq!(
            enum_glue.num_param_slots,
            type_pool.abi_slot_count(drop_enum_ty)
        );
        assert_eq!(param_indices(&enum_glue), [0, 1, 2, 2]);
    }
}
