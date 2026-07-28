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
    AirEditor, AirPattern, AirRef, AirValidationContext, AnalyzedFunction, EnumId,
    FrozenTypeInternPool, ParamSlotModes, StructDef, Type, TypeKind,
};
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::Span;

/// Check if a type needs drop.
fn type_needs_drop(ty: Type, type_pool: &FrozenTypeInternPool) -> bool {
    type_pool.type_needs_drop(ty)
}

/// Synthesize drop glue functions for all structs and arrays that need them.
///
/// Returns a list of synthesized functions that should be added to the compilation.
pub fn synthesize_drop_glue(
    type_pool: &FrozenTypeInternPool,
    type_identities: &std::collections::HashMap<
        Type,
        rue_air::TypeInstanceKey<rue_air::SemanticDefinitionToken, rue_air::SemanticModuleToken>,
    >,
) -> CompileResult<Vec<AnalyzedFunction>> {
    let mut drop_glue_functions = Vec::new();

    // Create drop glue for structs
    for struct_id in type_pool.all_struct_ids() {
        let struct_def = type_pool.struct_def(struct_id);
        let struct_ty = Type::new_struct(struct_id);
        // Body analysis may retain inert request-local pool slots from an
        // abandoned anonymous representative attempt. Only the active
        // semantic identity map is authoritative for terminal glue emission.
        if !type_identities.contains_key(&struct_ty) {
            continue;
        }
        // Skip structs that don't need drop
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
        let identity = type_identities.get(&struct_ty).cloned().ok_or_else(|| {
            CompileError::without_span(ErrorKind::InternalError(
                "missing canonical struct identity for drop glue".into(),
            ))
        })?;
        let func = create_struct_drop_glue_function(struct_def, struct_id, type_pool, identity)?;
        drop_glue_functions.push(func);
    }

    // Create drop glue for arrays
    for array_id in type_pool.all_array_ids() {
        let array_ty = Type::new_array(array_id);
        if !type_identities.contains_key(&array_ty) {
            continue;
        }
        // Skip arrays that don't need drop
        if !type_needs_drop(array_ty, type_pool) {
            continue;
        }

        // Create drop glue function for array
        let identity = type_identities.get(&array_ty).cloned().ok_or_else(|| {
            CompileError::without_span(ErrorKind::InternalError(
                "missing canonical array identity for drop glue".into(),
            ))
        })?;
        let func = create_array_drop_glue_function(array_id, type_pool, identity)?;
        drop_glue_functions.push(func);
    }

    // Create drop glue for payload-carrying enums (RUE-221). The glue switches
    // on the discriminant and drops the active variant's payload.
    for enum_id in type_pool.all_enum_ids() {
        let enum_ty = Type::new_enum(enum_id);
        if !type_identities.contains_key(&enum_ty) {
            continue;
        }
        if !type_needs_drop(enum_ty, type_pool) {
            continue;
        }

        let identity = type_identities.get(&enum_ty).cloned().ok_or_else(|| {
            CompileError::without_span(ErrorKind::InternalError(
                "missing canonical enum identity for drop glue".into(),
            ))
        })?;
        let func = create_enum_drop_glue_function(enum_id, type_pool, identity)?;
        drop_glue_functions.push(func);
    }

    Ok(drop_glue_functions)
}

/// Create a drop glue function for a single struct.
fn create_struct_drop_glue_function(
    struct_def: &StructDef,
    struct_id: rue_air::StructId,
    type_pool: &FrozenTypeInternPool,
    identity: rue_air::TypeInstanceKey<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >,
) -> CompileResult<AnalyzedFunction> {
    // Derived by the shared authority so glue synthesis and the backends'
    // lowering agree; file-qualified when the struct name spans files (RUE-571).
    let fn_name = rue_air::drop_glue_names::struct_drop_glue_name(struct_id, type_pool);
    let span = Span::new(0, 0); // Synthetic span

    // Create AIR for the drop glue function
    let mut air = AirEditor::new(Type::UNIT);

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
                    let param_ref =
                        air.add_param(current_param_slot, Type::new_struct(nested_struct_id), span);
                    let drop_ref = air.add_drop(param_ref, span);
                    drop_statements.push(drop_ref);
                }
                TypeKind::Array(array_id) => {
                    // Array field - load it and drop it
                    // The array drop glue will handle dropping each element
                    let param_ref =
                        air.add_param(current_param_slot, Type::new_array(array_id), span);
                    let drop_ref = air.add_drop(param_ref, span);
                    drop_statements.push(drop_ref);
                }
                TypeKind::Enum(enum_id) => {
                    // Payload-carrying enum field - load it and drop it. The
                    // enum drop glue dispatches on the active discriminant and
                    // drops only the selected variant's payload.
                    let param_ref =
                        air.add_param(current_param_slot, Type::new_enum(enum_id), span);
                    let drop_ref = air.add_drop(param_ref, span);
                    drop_statements.push(drop_ref);
                }
                _ => {}
            }
        }

        current_param_slot += field_slot_count;
    }

    // Create the unit value for return
    let unit_const = air.add_unit(span);

    // If we have drop statements, wrap them in a Block so they get executed
    // The CFG builder uses demand-driven lowering, so statements in a Block
    // are explicitly included as side-effects.
    let return_value = if drop_statements.is_empty() {
        unit_const
    } else {
        // Encode statements into extra array
        air.add_block(&drop_statements, unit_const, Type::UNIT, span)?
    };

    // Add return instruction
    air.add_ret(Some(return_value), Type::UNIT, span);

    // All parameters are passed by value (normal mode)
    let param_modes = vec![false; num_param_slots as usize];

    Ok(AnalyzedFunction {
        identity: rue_air::FunctionInstanceKey::DropGlue(Box::new(identity)),
        callable_kind: rue_air::AnalyzedCallableKind::DropGlue,
        ordinary_owner: None,
        name: fn_name,
        // Membership, not the generated-name prefix (RUE-1050).
        implicit_drop_source: if type_pool.is_anonymous_struct(struct_id) {
            None
        } else {
            Some(rue_air::ImplicitDropDependencySourceEvent::NamedStruct {
                file: struct_def.file_id.index(),
                name: struct_def.name.clone(),
            })
        },
        air: air
            .finish(AirValidationContext::Canonical(type_pool))
            .map_err(|error| {
                CompileError::new(ErrorKind::InternalError(error.to_string()), span)
            })?,
        local_atoms: Vec::new(),
        num_locals: 0,
        num_param_slots,
        param_modes: ParamSlotModes::new(param_modes.clone(), vec![false; param_modes.len()]),
        allow_unreachable_code: false,
    })
}

/// Create a drop glue function for an array type.
///
/// The function receives all element slots as parameters (flattened) and drops
/// each element in index order.
fn create_array_drop_glue_function(
    array_id: rue_air::ArrayTypeId,
    type_pool: &FrozenTypeInternPool,
    identity: rue_air::TypeInstanceKey<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >,
) -> CompileResult<AnalyzedFunction> {
    let fn_name = array_drop_glue_name(array_id, type_pool);
    let span = Span::new(0, 0); // Synthetic span

    // Get array element type and length
    let (element_type, length) = type_pool.array_def(array_id);

    // Create AIR for the drop glue function
    let mut air = AirEditor::new(Type::UNIT);

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
                let param_ref =
                    air.add_param(current_param_slot, Type::new_struct(struct_id), span);
                let drop_ref = air.add_drop(param_ref, span);
                drop_statements.push(drop_ref);
            }
            TypeKind::Array(nested_array_id) => {
                let param_ref =
                    air.add_param(current_param_slot, Type::new_array(nested_array_id), span);
                let drop_ref = air.add_drop(param_ref, span);
                drop_statements.push(drop_ref);
            }
            TypeKind::Enum(enum_id) => {
                let param_ref = air.add_param(current_param_slot, Type::new_enum(enum_id), span);
                let drop_ref = air.add_drop(param_ref, span);
                drop_statements.push(drop_ref);
            }
            _ => {}
        }
    }

    // Create the unit value for return
    let unit_const = air.add_unit(span);

    // If we have drop statements, wrap them in a Block so they get executed
    let return_value = if drop_statements.is_empty() {
        unit_const
    } else {
        air.add_block(&drop_statements, unit_const, Type::UNIT, span)?
    };

    // Add return instruction
    air.add_ret(Some(return_value), Type::UNIT, span);

    // All parameters are passed by value (normal mode)
    let param_modes = vec![false; num_param_slots as usize];

    Ok(AnalyzedFunction {
        identity: rue_air::FunctionInstanceKey::DropGlue(Box::new(identity)),
        callable_kind: rue_air::AnalyzedCallableKind::DropGlue,
        ordinary_owner: None,
        name: fn_name,
        implicit_drop_source: None,
        air: air
            .finish(AirValidationContext::Canonical(type_pool))
            .map_err(|error| {
                CompileError::new(ErrorKind::InternalError(error.to_string()), span)
            })?,
        local_atoms: Vec::new(),
        num_locals: 0,
        num_param_slots,
        param_modes: ParamSlotModes::new(param_modes.clone(), vec![false; param_modes.len()]),
        allow_unreachable_code: false,
    })
}

/// Create a drop glue function for a payload-carrying enum type (RUE-221).
///
/// The function receives the enum's flattened slots as one ordinary Rue
/// by-value argument. The ABI reverses that complete slot vector: the final
/// parameter slot is the discriminant and the preceding slots are the reversed
/// payload union. It switches on the discriminant and, for the active variant,
/// drops each droppable payload field in declaration order. Variants whose
/// payload needs no drop (and discriminant-only variants) fall through to a
/// no-op wildcard default arm, so exactly the active variant's payload is
/// dropped.
fn create_enum_drop_glue_function(
    enum_id: EnumId,
    type_pool: &FrozenTypeInternPool,
    identity: rue_air::TypeInstanceKey<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >,
) -> CompileResult<AnalyzedFunction> {
    let enum_def = type_pool.enum_def(enum_id);
    let fn_name = enum_drop_glue_name(enum_id, type_pool);
    let span = Span::new(0, 0); // Synthetic span

    let mut air = AirEditor::new(Type::UNIT);

    // Total ABI slots: discriminant (slot 0) + payload area (largest variant).
    let num_param_slots = type_pool.abi_slot_count(Type::new_enum(enum_id));

    // A whole enum is one multi-slot by-value argument, so its ABI slot vector
    // is reversed. The logical slot-0 discriminant is therefore last.
    let disc_ty = enum_def.discriminant_type();
    let disc_param = air.add_param(num_param_slots - 1, disc_ty, span);

    // A single shared unit value for every arm body and the outer block.
    let unit_const = air.add_unit(span);

    // Build one Int-pattern arm per variant that carries a droppable payload.
    // Payload fields overlay the union starting at slot 1, so field j of a
    // variant sits at slot `1 + sum(slot_count(field_k) for k < j)`.
    let mut arms = Vec::new();

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
                // The logical half-open range [field_slot, field_slot +
                // field_slots) becomes the reversed ABI range beginning here.
                // Reading an aggregate Param reverses that range once more,
                // reconstructing the field in logical order (RUE-998).
                let abi_field_slot = num_param_slots - (field_slot + field_slots);
                let param_ref = air.add_param(abi_field_slot, field_ty, span);
                let drop_ref = air.add_drop(param_ref, span);
                drop_stmts.push(drop_ref);
            }
            field_slot += field_slots;
        }

        // Arm body: a Block running the drops, yielding unit.
        let arm_body = air.add_block(&drop_stmts, unit_const, Type::UNIT, span)?;

        arms.push((AirPattern::Int(variant_index as i64), arm_body));
    }

    // A wildcard default arm (drops nothing) covers variants with no droppable
    // payload and keeps the switch total for codegen.
    arms.push((AirPattern::Wildcard, unit_const));

    let match_ref = air.add_match(disc_param, &arms, Type::UNIT, span)?;

    // Return unit; the match runs as a side-effecting statement of the body.
    let body = air.add_block(&[match_ref], unit_const, Type::UNIT, span)?;

    air.add_ret(Some(body), Type::UNIT, span);

    let param_modes = vec![false; num_param_slots as usize];

    Ok(AnalyzedFunction {
        identity: rue_air::FunctionInstanceKey::DropGlue(Box::new(identity)),
        callable_kind: rue_air::AnalyzedCallableKind::DropGlue,
        ordinary_owner: None,
        name: fn_name,
        implicit_drop_source: Some(rue_air::ImplicitDropDependencySourceEvent::NamedEnum {
            file: enum_def.file_id.index(),
            name: enum_def.name.clone(),
        }),
        air: air
            .finish(AirValidationContext::Canonical(type_pool))
            .map_err(|error| {
                CompileError::new(ErrorKind::InternalError(error.to_string()), span)
            })?,
        local_atoms: Vec::new(),
        num_locals: 0,
        num_param_slots,
        param_modes: ParamSlotModes::new(param_modes.clone(), vec![false; param_modes.len()]),
        allow_unreachable_code: false,
    })
}

// Drop-glue symbol names are derived by the single authority in
// `rue_air::drop_glue_names` so glue synthesis here and the backends' lowering
// cannot spell them differently (RUE-796). Re-exported for the existing
// `rue_compiler::drop_glue::{enum,array}_drop_glue_name` call sites.
pub use rue_air::drop_glue_names::{array_drop_glue_name, enum_drop_glue_name};

#[cfg(test)]
mod tests {
    use lasso::ThreadedRodeo;
    use rue_air::{AirInstData, EnumDef, StructField, TypeInternPool};

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
        let outer = create_struct_drop_glue_function(
            type_pool.struct_def(outer_id),
            outer_id,
            &type_pool,
            rue_air::TypeInstanceKey::I32,
        )
        .unwrap();
        assert_eq!(outer.num_param_slots, type_pool.abi_slot_count(outer_ty));
        assert_eq!(outer.num_param_slots, 27);
        assert_eq!(param_indices(&outer), [19, 20, 22, 24]);

        let array = create_array_drop_glue_function(
            drop_array_id,
            &type_pool,
            rue_air::TypeInstanceKey::I32,
        )
        .unwrap();
        assert_eq!(
            array.num_param_slots,
            type_pool.abi_slot_count(drop_array_ty)
        );
        assert_eq!(param_indices(&array), [0, 1]);

        let enum_glue =
            create_enum_drop_glue_function(drop_enum_id, &type_pool, rue_air::TypeInstanceKey::I32)
                .unwrap();
        assert_eq!(
            enum_glue.num_param_slots,
            type_pool.abi_slot_count(drop_enum_ty)
        );
        assert_eq!(param_indices(&enum_glue), [2, 1, 0, 0]);
    }

    #[test]
    fn enum_array_drop_glue_names_are_owner_aware_and_match_codegen() {
        let type_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let symbol = interner.get_or_intern("Choice");
        let make_enum = |file_id| EnumDef {
            name: "Choice".into(),
            variants: vec!["Only".into()],
            variant_payloads: vec![vec![]],
            is_pub: false,
            file_id,
        };
        let (left, _) = type_pool.register_enum(symbol, make_enum(rue_span::FileId::new(1)));
        let (right, _) = type_pool.register_enum(symbol, make_enum(rue_span::FileId::new(2)));
        let left_array = type_pool.intern_array_from_type(Type::new_enum(left), 2);
        let right_array = type_pool.intern_array_from_type(Type::new_enum(right), 2);
        let type_pool = type_pool.freeze();

        let left_name = array_drop_glue_name(left_array, &type_pool);
        let right_name = array_drop_glue_name(right_array, &type_pool);
        assert_eq!(left_name, "__rue_drop_array_Choice$1_2");
        assert_eq!(right_name, "__rue_drop_array_Choice$2_2");
        assert_ne!(left_name, right_name);
        assert_eq!(
            left_name,
            rue_codegen::types::array_drop_glue_name(left_array, &type_pool)
        );
        assert_eq!(
            right_name,
            rue_codegen::types::array_drop_glue_name(right_array, &type_pool)
        );
    }
}
