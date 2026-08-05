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
use std::sync::Arc;

type IssuedTypeInstanceKey =
    rue_air::TypeInstanceKey<rue_air::SemanticDefinitionToken, rue_air::SemanticModuleToken>;

pub(crate) fn semantic_type_from_instance(
    ty: &crate::TypeInstanceKey,
) -> rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId> {
    use crate::TypeInstanceKey as T;
    use rue_air::SemanticImportType as S;
    match ty {
        T::I8 => S::I8,
        T::I16 => S::I16,
        T::I32 => S::I32,
        T::I64 => S::I64,
        T::U8 => S::U8,
        T::U16 => S::U16,
        T::U32 => S::U32,
        T::U64 => S::U64,
        T::Bool => S::Bool,
        T::Unit => S::Unit,
        T::Never => S::Never,
        T::ComptimeType => S::ComptimeType,
        T::BuiltinNominal { kind, name } => S::BuiltinNominal {
            kind: match kind {
                crate::AnonymousNominalKind::Struct => rue_air::SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
            },
            name: name.clone(),
        },
        T::Nominal(crate::NominalInstanceKey::Named(key)) => S::Nominal(key.clone()),
        T::Nominal(crate::NominalInstanceKey::Anonymous(key)) => S::AnonymousNominal(key.clone()),
        T::Nominal(crate::NominalInstanceKey::Builtin { kind, name }) => S::BuiltinNominal {
            kind: match kind {
                crate::AnonymousNominalKind::Struct => rue_air::SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
            },
            name: name.clone(),
        },
        T::Array { element, len } => S::Array {
            element: Box::new(semantic_type_from_instance(element)),
            len: *len,
        },
        T::Slice { element, name } => S::Slice {
            element: Box::new(semantic_type_from_instance(element)),
            name: name.clone(),
        },
        T::PtrConst(element) => S::PtrConst(Box::new(semantic_type_from_instance(element))),
        T::PtrMut(element) => S::PtrMut(Box::new(semantic_type_from_instance(element))),
        T::Module(module) => S::Module(module.clone()),
        T::GenericParameter(index) => S::GenericParameter(*index),
    }
}

/// Build the canonical AIR-shaped body for one exact drop-glue fact. Layout
/// slots are supplied by registered Layout dependencies observed by the CFG
/// evaluator, so this path never needs a caller-owned frozen pool.
pub(crate) fn synthesize_canonical_drop_glue(
    owner: &crate::TypeInstanceKey,
    facts: &crate::type_queries::DropGlueFacts,
    abi_slots: &std::collections::BTreeMap<crate::TypeInstanceKey, u32>,
) -> Result<rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>, Arc<str>> {
    use rue_air::{SemanticBodyAnchor, SemanticBodyInst, SemanticBodyInstData};
    type Body = rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>;
    type Ty = rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>;

    let slots = |ty: &crate::TypeInstanceKey| {
        abi_slots
            .get(ty)
            .copied()
            .ok_or_else(|| Arc::<str>::from(format!("missing layout for drop-glue type {ty:?}")))
    };
    let anchor = SemanticBodyAnchor { start: 0, end: 0 };
    let mut instructions = Vec::<SemanticBodyInst<_, _>>::new();
    let mut add = |data, ty: Ty| {
        let index = u32::try_from(instructions.len()).expect("drop-glue body fits u32");
        instructions.push(SemanticBodyInst { data, ty, anchor });
        index
    };
    let mut statements = Vec::new();
    let num_param_slots = slots(owner)?;

    match &facts.plan {
        crate::type_queries::DropGluePlan::Struct { fields } => {
            let mut current = 0;
            for field in fields.iter() {
                let width = slots(&field.ty)?;
                if field.drop {
                    let ty = semantic_type_from_instance(&field.ty);
                    let param = add(SemanticBodyInstData::Param { index: current }, ty.clone());
                    statements.push(add(SemanticBodyInstData::Drop { value: param }, ty));
                }
                current = current.saturating_add(width);
            }
        }
        crate::type_queries::DropGluePlan::Array {
            element,
            len,
            drop_element,
        } => {
            let width = slots(element)?;
            if *drop_element {
                for index in 0..*len {
                    let ty = semantic_type_from_instance(element);
                    let param = add(
                        SemanticBodyInstData::Param {
                            index: u32::try_from(index)
                                .unwrap_or(u32::MAX)
                                .saturating_mul(width),
                        },
                        ty.clone(),
                    );
                    statements.push(add(SemanticBodyInstData::Drop { value: param }, ty));
                }
            }
        }
        crate::type_queries::DropGluePlan::Enum { variants } => {
            let disc_ty = if variants.is_empty() {
                Ty::Never
            } else if variants.len() <= 256 {
                Ty::U8
            } else if variants.len() <= 65_536 {
                Ty::U16
            } else if variants.len() <= 4_294_967_296usize {
                Ty::U32
            } else {
                Ty::U64
            };
            let disc = add(
                SemanticBodyInstData::Param {
                    index: num_param_slots.saturating_sub(1),
                },
                disc_ty.clone(),
            );
            let unit = add(SemanticBodyInstData::UnitConst, Ty::Unit);
            let mut arms = Vec::new();
            for (variant_index, variant) in variants.iter().enumerate() {
                if !variant.fields.iter().any(|field| field.drop) {
                    continue;
                }
                let mut field_slot = 1u32;
                let mut drops = Vec::new();
                for field in variant.fields.iter() {
                    let width = slots(&field.ty)?;
                    if field.drop {
                        let ty = semantic_type_from_instance(&field.ty);
                        let param = add(
                            SemanticBodyInstData::Param {
                                index: num_param_slots
                                    .saturating_sub(field_slot.saturating_add(width)),
                            },
                            ty.clone(),
                        );
                        drops.push(add(SemanticBodyInstData::Drop { value: param }, ty));
                    }
                    field_slot = field_slot.saturating_add(width);
                }
                let body = add(
                    SemanticBodyInstData::Block {
                        statements: drops.into(),
                        value: unit,
                    },
                    Ty::Unit,
                );
                arms.push(rue_air::SemanticBodyMatchArm {
                    pattern: rue_air::SemanticBodyPattern::Int(variant_index as i64),
                    body,
                });
            }
            arms.push(rue_air::SemanticBodyMatchArm {
                pattern: rue_air::SemanticBodyPattern::Wildcard,
                body: unit,
            });
            statements.push(add(
                SemanticBodyInstData::Match {
                    scrutinee: disc,
                    arms: arms.into(),
                },
                Ty::Unit,
            ));
        }
        crate::type_queries::DropGluePlan::None => {}
    }
    let unit = add(SemanticBodyInstData::UnitConst, Ty::Unit);
    let value = if statements.is_empty() {
        unit
    } else {
        add(
            SemanticBodyInstData::Block {
                statements: statements.into(),
                value: unit,
            },
            Ty::Unit,
        )
    };
    add(SemanticBodyInstData::Ret(Some(value)), Ty::Unit);
    Ok(Body {
        return_type: Ty::Unit,
        instructions: instructions.into(),
        places: Arc::new([]),
        strings: Arc::new([]),
        local_atoms: Arc::new([]),
        param_drops: Arc::new([]),
        borrow_slots: Arc::new([]),
        num_locals: 0,
        num_param_slots,
        param_by_ref: vec![false; num_param_slots as usize].into(),
        param_writable: vec![false; num_param_slots as usize].into(),
        allow_unreachable_code: false,
        warnings: Arc::new([]),
        method_references: Arc::new([]),
    })
}

fn plan_type_matches_live(
    planned: &IssuedTypeInstanceKey,
    live: Type,
    type_pool: &FrozenTypeInternPool,
    types_by_identity: &std::collections::HashMap<IssuedTypeInstanceKey, Type>,
) -> bool {
    use rue_air::TypeInstanceKey as P;
    match planned {
        P::I8 => live == Type::I8,
        P::I16 => live == Type::I16,
        P::I32 => live == Type::I32,
        P::I64 => live == Type::I64,
        P::U8 => live == Type::U8,
        P::U16 => live == Type::U16,
        P::U32 => live == Type::U32,
        P::U64 => live == Type::U64,
        P::Bool => live == Type::BOOL,
        P::Unit => live == Type::UNIT,
        P::Never => live == Type::NEVER,
        P::ComptimeType => live == Type::COMPTIME_TYPE,
        P::PtrConst(pointee) => live.as_ptr_const().is_some_and(|id| {
            plan_type_matches_live(
                pointee,
                type_pool.ptr_const_def(id),
                type_pool,
                types_by_identity,
            )
        }),
        P::PtrMut(pointee) => live.as_ptr_mut().is_some_and(|id| {
            plan_type_matches_live(
                pointee,
                type_pool.ptr_mut_def(id),
                type_pool,
                types_by_identity,
            )
        }),
        P::Array { .. } | P::Slice { .. } | P::BuiltinNominal { .. } | P::Nominal(_) => {
            types_by_identity.get(planned).is_some_and(|ty| *ty == live)
        }
        P::Module(_) | P::GenericParameter(_) => false,
    }
}

/// Synthesize glue for the exact reached owner set.
///
/// `types_by_identity` is the semantic epoch's direct reverse index.  This
/// path performs one keyed probe per demanded owner and never enumerates a
/// struct/array/enum pool.
pub fn synthesize_demanded_drop_glue(
    type_pool: &FrozenTypeInternPool,
    types_by_identity: &std::collections::HashMap<
        rue_air::TypeInstanceKey<rue_air::SemanticDefinitionToken, rue_air::SemanticModuleToken>,
        Type,
    >,
    demanded: impl IntoIterator<Item = IssuedTypeInstanceKey>,
    plans: &std::collections::BTreeMap<
        IssuedTypeInstanceKey,
        crate::type_queries::DropGlueFacts<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
    >,
) -> CompileResult<Vec<AnalyzedFunction>> {
    let mut drop_glue_functions = Vec::new();
    for identity in demanded {
        let plan = plans.get(&identity).ok_or_else(|| {
            CompileError::without_span(ErrorKind::InternalError(
                "demanded drop-glue owner has no authoritative query plan".into(),
            ))
        })?;
        let ty = types_by_identity.get(&identity).copied().ok_or_else(|| {
            CompileError::without_span(ErrorKind::InternalError(
                "demanded drop-glue owner has no live type materialization".into(),
            ))
        })?;
        if !plan.required || !plan.synthesize {
            continue;
        }
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = type_pool.struct_def(struct_id);
                drop_glue_functions.push(create_struct_drop_glue_function(
                    struct_def,
                    struct_id,
                    type_pool,
                    types_by_identity,
                    identity,
                    plan,
                )?);
            }
            TypeKind::Array(array_id) => {
                drop_glue_functions.push(create_array_drop_glue_function(
                    array_id,
                    type_pool,
                    types_by_identity,
                    identity,
                    plan,
                )?);
            }
            TypeKind::Enum(enum_id) => {
                drop_glue_functions.push(create_enum_drop_glue_function(
                    enum_id,
                    type_pool,
                    types_by_identity,
                    identity,
                    plan,
                )?);
            }
            _ => {}
        }
    }

    Ok(drop_glue_functions)
}

/// Create a drop glue function for a single struct.
fn create_struct_drop_glue_function(
    struct_def: &StructDef,
    struct_id: rue_air::StructId,
    type_pool: &FrozenTypeInternPool,
    types_by_identity: &std::collections::HashMap<IssuedTypeInstanceKey, Type>,
    identity: IssuedTypeInstanceKey,
    facts: &crate::type_queries::DropGlueFacts<
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

    let crate::type_queries::DropGluePlan::Struct {
        fields: planned_fields,
    } = &facts.plan
    else {
        return Err(CompileError::without_span(ErrorKind::InternalError(
            "struct drop-glue owner received a non-struct query plan".into(),
        )));
    };
    if planned_fields.len() != struct_def.fields.len() {
        return Err(CompileError::without_span(ErrorKind::InternalError(
            "struct drop-glue plan disagrees with live field count".into(),
        )));
    }
    if planned_fields
        .iter()
        .zip(&struct_def.fields)
        .any(|(planned, live)| {
            planned.name.as_ref() != live.name
                || !plan_type_matches_live(&planned.ty, live.ty, type_pool, types_by_identity)
        })
    {
        return Err(CompileError::without_span(ErrorKind::InternalError(
            "struct drop-glue plan disagrees with live field order or type".into(),
        )));
    }
    for (field_index, field) in struct_def.fields.iter().enumerate() {
        let field_slot_count = type_pool.abi_slot_count(field.ty);

        let should_drop = planned_fields[field_index].drop;
        if should_drop {
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
                name: struct_def.name.to_string(),
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
    types_by_identity: &std::collections::HashMap<IssuedTypeInstanceKey, Type>,
    identity: IssuedTypeInstanceKey,
    facts: &crate::type_queries::DropGlueFacts<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >,
) -> CompileResult<AnalyzedFunction> {
    let fn_name = array_drop_glue_name(array_id, type_pool);
    let span = Span::new(0, 0); // Synthetic span

    // Get array element type and length
    let (element_type, length) = type_pool.array_def(array_id);
    let crate::type_queries::DropGluePlan::Array {
        element,
        len,
        drop_element: planned_drop_element,
    } = &facts.plan
    else {
        return Err(CompileError::without_span(ErrorKind::InternalError(
            "array drop-glue owner received a non-array query plan".into(),
        )));
    };
    if *len != length
        || !plan_type_matches_live(element, element_type, type_pool, types_by_identity)
    {
        return Err(CompileError::without_span(ErrorKind::InternalError(
            "array drop-glue plan disagrees with live length or element type".into(),
        )));
    }

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
        if !*planned_drop_element {
            continue;
        }
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
    types_by_identity: &std::collections::HashMap<IssuedTypeInstanceKey, Type>,
    identity: IssuedTypeInstanceKey,
    facts: &crate::type_queries::DropGlueFacts<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    >,
) -> CompileResult<AnalyzedFunction> {
    let enum_def = type_pool.enum_def(enum_id);
    let crate::type_queries::DropGluePlan::Enum {
        variants: planned_variants,
    } = &facts.plan
    else {
        return Err(CompileError::without_span(ErrorKind::InternalError(
            "enum drop-glue owner received a non-enum query plan".into(),
        )));
    };
    if planned_variants.len() != enum_def.variant_count() {
        return Err(CompileError::without_span(ErrorKind::InternalError(
            "enum drop-glue plan disagrees with live variant count".into(),
        )));
    }
    if planned_variants
        .iter()
        .zip(enum_def.variants.iter())
        .any(|(planned, live)| planned.name.as_ref() != live.as_ref())
    {
        return Err(CompileError::without_span(ErrorKind::InternalError(
            "enum drop-glue plan disagrees with live variant order".into(),
        )));
    }
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
        let planned_fields = planned_variants[variant_index].fields.as_ref();
        if planned_fields.len() != payload.len() {
            return Err(CompileError::without_span(ErrorKind::InternalError(
                "enum drop-glue plan disagrees with live payload field count".into(),
            )));
        }
        if planned_fields.iter().zip(payload).any(|(planned, &live)| {
            !plan_type_matches_live(&planned.ty, live, type_pool, types_by_identity)
        }) {
            return Err(CompileError::without_span(ErrorKind::InternalError(
                "enum drop-glue plan disagrees with live payload field type".into(),
            )));
        }
        if !planned_fields.iter().any(|field| field.drop) {
            continue;
        }

        let mut drop_stmts: Vec<AirRef> = Vec::new();
        let mut field_slot = 1u32; // slot 0 is the discriminant
        for (field_index, &field_ty) in payload.iter().enumerate() {
            let field_slots = type_pool.abi_slot_count(field_ty);
            let should_drop = planned_fields[field_index].drop;
            if should_drop {
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
            name: enum_def.name.to_string(),
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
    use std::sync::Arc;

    use super::*;

    #[test]
    fn canonical_drop_glue_is_synthesized_from_exact_facts_and_layout_slots() {
        let element = crate::TypeInstanceKey::BuiltinNominal {
            kind: crate::AnonymousNominalKind::Struct,
            name: Arc::from("Owned"),
        };
        let owner = crate::TypeInstanceKey::Array {
            element: Box::new(element.clone()),
            len: 3,
        };
        let facts = crate::type_queries::DropGlueFacts {
            required: true,
            synthesize: true,
            destructor: None,
            nested: Arc::from([element.clone()]),
            plan: crate::type_queries::DropGluePlan::Array {
                element: element.clone(),
                len: 3,
                drop_element: true,
            },
        };
        let slots = std::collections::BTreeMap::from([(element, 2), (owner.clone(), 6)]);
        let body = synthesize_canonical_drop_glue(&owner, &facts, &slots).unwrap();
        assert_eq!(body.num_param_slots, 6);
        assert_eq!(body.param_by_ref.as_ref(), &[false; 6]);
        let params = body
            .instructions
            .iter()
            .filter_map(|instruction| match instruction.data {
                rue_air::SemanticBodyInstData::Param { index } => Some(index),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(params, [0, 2, 4]);
        assert_eq!(
            body.instructions
                .iter()
                .filter(|instruction| matches!(
                    instruction.data,
                    rue_air::SemanticBodyInstData::Drop { .. }
                ))
                .count(),
            3
        );
    }

    fn test_type_identity(ty: Type, type_pool: &FrozenTypeInternPool) -> IssuedTypeInstanceKey {
        use rue_air::{AnonymousNominalKind as K, TypeInstanceKey as T, TypeKind};
        match ty.kind() {
            TypeKind::I8 => T::I8,
            TypeKind::I16 => T::I16,
            TypeKind::I32 => T::I32,
            TypeKind::I64 => T::I64,
            TypeKind::U8 => T::U8,
            TypeKind::U16 => T::U16,
            TypeKind::U32 => T::U32,
            TypeKind::U64 => T::U64,
            TypeKind::Bool => T::Bool,
            TypeKind::Unit => T::Unit,
            TypeKind::Never => T::Never,
            TypeKind::ComptimeType => T::ComptimeType,
            TypeKind::Struct(id) => T::BuiltinNominal {
                kind: K::Struct,
                name: type_pool.struct_def(id).name.clone().into(),
            },
            TypeKind::Enum(id) => T::BuiltinNominal {
                kind: K::Enum,
                name: type_pool.enum_def(id).name.clone().into(),
            },
            TypeKind::Array(id) => {
                let (element, len) = type_pool.array_def(id);
                T::Array {
                    element: Box::new(test_type_identity(element, type_pool)),
                    len,
                }
            }
            TypeKind::PtrConst(id) => T::PtrConst(Box::new(test_type_identity(
                type_pool.ptr_const_def(id),
                type_pool,
            ))),
            TypeKind::PtrMut(id) => T::PtrMut(Box::new(test_type_identity(
                type_pool.ptr_mut_def(id),
                type_pool,
            ))),
            TypeKind::Module(_) | TypeKind::Error => {
                panic!("test drop-glue plans contain only materializable runtime types")
            }
        }
    }

    fn insert_test_type(
        ty: Type,
        type_pool: &FrozenTypeInternPool,
        output: &mut std::collections::HashMap<IssuedTypeInstanceKey, Type>,
    ) {
        match ty.kind() {
            TypeKind::Struct(id) => {
                output.insert(test_type_identity(ty, type_pool), ty);
                for field in &type_pool.struct_def(id).fields {
                    insert_test_type(field.ty, type_pool, output);
                }
            }
            TypeKind::Enum(id) => {
                output.insert(test_type_identity(ty, type_pool), ty);
                for payload in &type_pool.enum_def(id).variant_payloads {
                    for &field in payload {
                        insert_test_type(field, type_pool, output);
                    }
                }
            }
            TypeKind::Array(id) => {
                output.insert(test_type_identity(ty, type_pool), ty);
                insert_test_type(type_pool.array_def(id).0, type_pool, output);
            }
            TypeKind::PtrConst(id) => {
                insert_test_type(type_pool.ptr_const_def(id), type_pool, output);
            }
            TypeKind::PtrMut(id) => {
                insert_test_type(type_pool.ptr_mut_def(id), type_pool, output);
            }
            _ => {}
        }
    }

    fn test_facts(
        plan: crate::type_queries::DropGluePlan<
            rue_air::SemanticDefinitionToken,
            rue_air::SemanticModuleToken,
        >,
    ) -> crate::type_queries::DropGlueFacts<
        rue_air::SemanticDefinitionToken,
        rue_air::SemanticModuleToken,
    > {
        crate::type_queries::DropGlueFacts {
            required: true,
            synthesize: true,
            destructor: None,
            nested: Arc::from([]),
            plan,
        }
    }

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
                    name: Arc::from(name),
                    fields,
                    is_copy: false,
                    is_linear: false,
                    destructor: destructor.map(Arc::from),
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
        let variants = (0..payloads.len())
            .map(|i| Arc::from(format!("V{i}").as_str()))
            .collect();
        type_pool
            .register_enum(
                symbol,
                EnumDef {
                    name: Arc::from(name),
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
        let mut types_by_identity = std::collections::HashMap::new();
        insert_test_type(outer_ty, &type_pool, &mut types_by_identity);
        let outer_facts = test_facts(crate::type_queries::DropGluePlan::Struct {
            fields: type_pool
                .struct_def(outer_id)
                .fields
                .iter()
                .enumerate()
                .map(|(index, field)| crate::type_queries::DropGlueField {
                    name: field.name.clone().into(),
                    ty: test_type_identity(field.ty, &type_pool),
                    drop: index >= 16,
                })
                .collect::<Vec<_>>()
                .into(),
        });
        let outer = create_struct_drop_glue_function(
            type_pool.struct_def(outer_id),
            outer_id,
            &type_pool,
            &types_by_identity,
            test_type_identity(outer_ty, &type_pool),
            &outer_facts,
        )
        .unwrap();
        assert_eq!(outer.num_param_slots, type_pool.abi_slot_count(outer_ty));
        assert_eq!(outer.num_param_slots, 27);
        assert_eq!(param_indices(&outer), [19, 20, 22, 24]);

        let array_facts = test_facts(crate::type_queries::DropGluePlan::Array {
            element: test_type_identity(drop_ty, &type_pool),
            len: 2,
            drop_element: true,
        });
        let array = create_array_drop_glue_function(
            drop_array_id,
            &type_pool,
            &types_by_identity,
            test_type_identity(drop_array_ty, &type_pool),
            &array_facts,
        )
        .unwrap();
        assert_eq!(
            array.num_param_slots,
            type_pool.abi_slot_count(drop_array_ty)
        );
        assert_eq!(param_indices(&array), [0, 1]);

        let enum_facts = test_facts(crate::type_queries::DropGluePlan::Enum {
            variants: type_pool
                .enum_def(drop_enum_id)
                .variants
                .iter()
                .zip(&type_pool.enum_def(drop_enum_id).variant_payloads)
                .map(|(name, payload)| crate::type_queries::DropGlueVariant {
                    name: name.clone().into(),
                    fields: payload
                        .iter()
                        .map(|&field| crate::type_queries::DropGlueVariantField {
                            ty: test_type_identity(field, &type_pool),
                            drop: field == drop_ty,
                        })
                        .collect::<Vec<_>>()
                        .into(),
                })
                .collect::<Vec<_>>()
                .into(),
        });
        let enum_glue = create_enum_drop_glue_function(
            drop_enum_id,
            &type_pool,
            &types_by_identity,
            test_type_identity(drop_enum_ty, &type_pool),
            &enum_facts,
        )
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
            variants: Arc::from(["Only".into()]),
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
