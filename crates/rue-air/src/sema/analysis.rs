//! Function body analysis and AIR generation.
//!
//! This module contains the core semantic analysis functionality:
//! - Function analysis (analyze_single_function, analyze_method_function, analyze_destructor_function)
//! - Hindley-Milner type inference (run_type_inference)
//! - RIR to AIR instruction lowering (analyze_inst)
//! - Helper functions for expression analysis
//!
//! The demand-driven driver analyzes ordinary function and method bodies only
//! when they are reachable from `main`, per ADR-0045. Program shape does not
//! change which bodies are checked.

use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use std::collections::{HashMap, HashSet};

use lasso::{Spur, ThreadedRodeo};
use rue_error::{
    CompileError, CompileErrors, CompileResult, CompileWarning, ErrorKind,
    IntrinsicTypeMismatchError, MultiErrorResult, OptionExt, PreviewFeature, WarningKind,
};
use rue_rir::{InstData, InstRef, Rir, RirArgMode, RirCallArg, RirParamMode};
use rue_span::{FileId, Span};
use rue_target::{Arch, DataModel, Os};

use super::call_resolution::{CallResolutionFacts, resolve_static_call_reference};
use super::context::{AnalysisContext, AnalysisResult, CallLoanKind, ConstValue};
use super::{AnalyzedFunction, BodySema, InferenceContext, MethodInfo, ParamSlotModes, SemaOutput};
use crate::inference::{
    Constraint, ConstraintContext, ConstraintGenerator, InferType, ParamVarInfo, Unifier,
    UnifyResult,
};
use crate::inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPlaceBase, AirProjection, AirRef,
};
use crate::types::{ModuleId, StructField, StructId, Type, TypeKind};

/// Main entry point for analyzing all function bodies.
///
/// Called from Sema::analyze_all after declarations are collected.
/// Uses the demand-driven driver for every program shape.
pub(crate) fn analyze_all_function_bodies_for_test(
    sema: BodySema<'_>,
) -> MultiErrorResult<SemaOutput> {
    analyze_all_function_bodies_with_work_for_test(sema)
        .map_err(super::BodyAnalysisFailure::into_errors)
}

pub(crate) fn analyze_all_function_bodies_with_work_for_test(
    mut sema: BodySema<'_>,
) -> Result<SemaOutput, super::BodyAnalysisFailure> {
    let result = analyze_all_function_bodies_mut_for_test(&mut sema);
    let work = result
        .as_ref()
        .map(|output| output.body_analysis_work)
        .unwrap_or(sema.body_analysis_work);
    result.map_err(|errors| super::BodyAnalysisFailure::new(errors, work))
}

pub(crate) fn compose_queried_bodies(
    mut sema: BodySema<'_>,
    candidates: Vec<
        crate::SemanticQueriedBodyCandidate<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    >,
) -> Result<SemaOutput, super::BodyAnalysisFailure> {
    let result = compose_queried_bodies_inner(&mut sema, candidates);
    let work = result
        .as_ref()
        .map(|output| output.body_analysis_work)
        .unwrap_or(sema.body_analysis_work);
    result.map_err(|errors| super::BodyAnalysisFailure::new(errors, work))
}

fn compose_queried_bodies_inner(
    sema: &mut BodySema<'_>,
    candidates: Vec<
        crate::SemanticQueriedBodyCandidate<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    >,
) -> MultiErrorResult<SemaOutput> {
    intern_named_callable_symbols(sema);
    sema.get_or_create_str_struct(Span::default())
        .map_err(CompileErrors::from)?;
    let mut functions = Vec::with_capacity(candidates.len());
    let mut warnings = Vec::new();
    let mut seen_warnings = HashSet::new();
    // Query-native composition materializes aggregate identities only from the
    // exact reached AIR below. Merely declaring a type must not root layout,
    // ownership, destructor diagnostics, or drop glue.
    let mut active_types = HashSet::new();
    let mut composed_identities = std::collections::BTreeSet::new();
    let mut errors = CompileErrors::new();
    for candidate in candidates {
        let identity = match canonical_composed_identity(sema, &candidate.identity) {
            Ok(identity) => identity,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        if !composed_identities.insert(identity.clone()) {
            continue;
        }
        sema.body_analysis_work.ordinary_body_import_attempts += 1;
        let mut imported = match import_staged_body(sema, &candidate.body, candidate.body_span) {
            Ok(imported) => imported,
            Err(error) => {
                sema.body_analysis_work.ordinary_body_import_failures += 1;
                errors.push(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "queried body {:?} failed import-only composition: {error:?}",
                        candidate.identity
                    )),
                    candidate.body_span,
                ));
                continue;
            }
        };
        // Local atoms are owned by the candidate envelope, not by the durable
        // body's original projection. Anonymous-member candidates can be
        // rebound to a contextual owner during composition, so install their
        // atoms under that same final callable identity.
        for atom in &mut imported.local_atoms {
            atom.identity.producer = identity.clone();
        }
        sema.body_analysis_work.ordinary_body_import_successes += 1;
        sema.body_analysis_work
            .ordinary_body_import_instructions_installed += imported.air.len();
        sema.body_analysis_work
            .ordinary_body_import_places_installed += imported.air.places().len();
        sema.body_analysis_work
            .ordinary_body_import_strings_installed += imported.strings.len();
        let (name, kind, drop_source) = match composed_callable_metadata(
            sema,
            &identity,
            candidate.ordinary_owner,
            candidate.specialization_identity,
        ) {
            Ok(value) => value,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        for warning in imported.warnings.iter().cloned() {
            let unseen = warning.span().is_none_or(|span| {
                seen_warnings.insert((std::mem::discriminant(&warning.kind), span))
            });
            if unseen {
                warnings.push(warning);
            }
        }
        extend_owned_aggregate_types(
            sema,
            std::iter::once(imported.air.return_type())
                .chain(imported.air.instructions().iter().map(|inst| inst.ty))
                .chain(imported.air.places().iter().map(|place| place.base_type))
                .chain(imported.air.param_drops().iter().map(|&(_, ty)| ty)),
            &mut active_types,
        );
        functions.push((
            AnalyzedFunction {
                identity,
                name,
                callable_kind: kind,
                ordinary_owner: candidate.ordinary_owner,
                implicit_drop_source: drop_source,
                air: imported.air,
                local_atoms: imported.local_atoms,
                num_locals: imported.num_locals,
                num_param_slots: imported.num_param_slots,
                param_modes: imported.param_modes,
                allow_unreachable_code: imported.allow_unreachable_code,
            },
            imported.strings,
        ));
    }
    finalize_function_body_analysis(
        sema,
        functions,
        &active_types,
        warnings,
        &HashSet::new(),
        errors,
    )
}

fn materialize_argument_value(
    sema: &BodySema<'_>,
    value: &crate::CanonicalArgumentValue<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >,
) -> Result<super::ConstValue, crate::SemanticBodyExportFailure> {
    use crate::CanonicalArgumentValue as V;
    let facts = sema.endpoint_facts();
    Ok(match value {
        V::Integer(value) => super::ConstValue::Integer(*value),
        V::Bool(value) => super::ConstValue::Bool(*value),
        V::Type(value) => {
            super::ConstValue::Type(super::body_endpoint::resolve_instance_type(&facts, value)?)
        }
        V::Function(value) => {
            let crate::FunctionInstanceKey::Definition(token) = value.as_ref() else {
                return Err(crate::SemanticBodyExportFailure::MissingStableIdentity);
            };
            let symbol = super::body_endpoint::resolve_free_function_symbol(&facts, *token)
                .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)?;
            super::ConstValue::Function(symbol)
        }
        V::Unit => super::ConstValue::Unit,
        V::String(value) => super::ConstValue::String(sema.interner.get_or_intern(value.as_ref())),
    })
}

fn materialize_instance_type(
    sema: &BodySema<'_>,
    value: &crate::TypeInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
) -> Result<Type, crate::SemanticBodyExportFailure> {
    super::body_endpoint::resolve_instance_type(&sema.endpoint_facts(), value)
}

fn canonical_composed_identity(
    sema: &BodySema<'_>,
    identity: &crate::FunctionInstanceKey<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >,
) -> CompileResult<
    crate::FunctionInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
> {
    let crate::FunctionInstanceKey::AnonymousMember { owner, member } = identity else {
        return Ok(identity.clone());
    };
    let owner = materialize_instance_type(sema, owner).map_err(|_| {
        CompileError::without_span(ErrorKind::InvalidCompilerInput(
            "queried anonymous callable owner has no current composition endpoint".into(),
        ))
    })?;
    Ok(crate::FunctionInstanceKey::AnonymousMember {
        owner: Box::new(sema.canonical_type_instance(owner).map_err(|_| {
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "queried anonymous callable owner has no canonical representative".into(),
            ))
        })?),
        member: member.clone(),
    })
}

fn composed_callable_metadata(
    sema: &BodySema<'_>,
    identity: &crate::FunctionInstanceKey<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >,
    owner: Option<super::BodyOwnerToken>,
    specialization_identity: Option<
        crate::SemanticSpecializationIdentity<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    >,
) -> CompileResult<(
    String,
    crate::AnalyzedCallableKind,
    Option<super::ImplicitDropDependencySourceEvent>,
)> {
    use crate::FunctionInstanceKey as F;
    let missing = || {
        CompileError::without_span(ErrorKind::InvalidCompilerInput(
            "queried callable identity has no current composition endpoint".into(),
        ))
    };
    match identity {
        F::Definition(token) => {
            let endpoint = sema
                .stable_definition_endpoints
                .get(token)
                .ok_or_else(missing)?;
            let (name, kind, source) = match endpoint.kind {
                crate::StableDefinitionKind::Function => (
                    sema.interner
                        .resolve(&sema.internal_function_name(
                            sema.interner.get_or_intern(endpoint.name.as_ref()),
                            FileId::new(endpoint.file),
                        ))
                        .to_string(),
                    crate::AnalyzedCallableKind::Ordinary,
                    owner.map(
                        |token| super::ImplicitDropDependencySourceEvent::FreeFunction {
                            token,
                            file: endpoint.file,
                            name: endpoint.name.to_string(),
                        },
                    ),
                ),
                crate::StableDefinitionKind::Method
                | crate::StableDefinitionKind::AssociatedFunction => {
                    let owner_name = endpoint.owner.as_deref().ok_or_else(missing)?;
                    let owner_symbol = sema.interner.get(owner_name).ok_or_else(missing)?;
                    let struct_id = sema
                        .structs_by_file_name
                        .get(&(FileId::new(endpoint.file), owner_symbol))
                        .copied()
                        .ok_or_else(missing)?;
                    let name = sema.method_symbol(
                        struct_id,
                        endpoint.name.as_ref(),
                        endpoint.kind == crate::StableDefinitionKind::Method,
                    );
                    (
                        name,
                        crate::AnalyzedCallableKind::Ordinary,
                        owner.map(
                            |token| super::ImplicitDropDependencySourceEvent::NamedMethod {
                                token,
                                file: endpoint.file,
                                owner_name: owner_name.to_string(),
                                method_name: endpoint.name.to_string(),
                            },
                        ),
                    )
                }
                crate::StableDefinitionKind::Destructor => {
                    let owner_name = endpoint.owner.as_deref().ok_or_else(missing)?;
                    let owner_symbol = sema.interner.get(owner_name).ok_or_else(missing)?;
                    let struct_id = sema
                        .structs_by_file_name
                        .get(&(FileId::new(endpoint.file), owner_symbol))
                        .copied()
                        .ok_or_else(missing)?;
                    (
                        sema.destructor_symbol(struct_id),
                        crate::AnalyzedCallableKind::Destructor,
                        owner.map(|token| {
                            super::ImplicitDropDependencySourceEvent::NamedDestructor {
                                token,
                                file: endpoint.file,
                                owner_name: owner_name.to_string(),
                            }
                        }),
                    )
                }
                _ => return Err(missing()),
            };
            Ok((name, kind, source))
        }
        F::AnonymousMember { owner: ty, member } => {
            let ty = materialize_instance_type(sema, ty).map_err(|_| missing())?;
            let struct_id = ty.as_struct().ok_or_else(missing)?;
            let method = sema
                .interner
                .get(member.name.as_ref())
                .ok_or_else(missing)?;
            let info = sema
                .call_facts()
                .method_info(struct_id, method)
                .ok_or_else(missing)?;
            Ok((
                sema.method_symbol(struct_id, member.name.as_ref(), info.has_self),
                if member.kind == crate::AnonymousMemberKind::Destructor {
                    crate::AnalyzedCallableKind::Destructor
                } else {
                    crate::AnalyzedCallableKind::Ordinary
                },
                Some(super::ImplicitDropDependencySourceEvent::Anonymous),
            ))
        }
        F::Specialization { base, arguments } => {
            let F::Definition(base) = base.as_ref() else {
                return Err(missing());
            };
            let endpoint = sema
                .stable_definition_endpoints
                .get(base)
                .ok_or_else(missing)?;
            let types = arguments
                .types
                .iter()
                .map(|ty| materialize_instance_type(sema, ty))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| missing())?;
            let values = arguments
                .values
                .iter()
                .map(|value| materialize_argument_value(sema, value))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| missing())?;
            let identity = specialization_identity.ok_or_else(missing)?;
            let base_name = sema.internal_function_name(
                sema.interner.get_or_intern(endpoint.name.as_ref()),
                FileId::new(endpoint.file),
            );
            Ok((
                crate::specialize::mangle_specialized_name(
                    sema.interner.resolve(&base_name),
                    &types,
                    &values,
                ),
                crate::AnalyzedCallableKind::Ordinary,
                Some(super::ImplicitDropDependencySourceEvent::Specialization { identity }),
            ))
        }
        F::DropGlue(_) => Err(missing()),
    }
}

pub(crate) fn import_staged_body(
    sema: &mut BodySema<'_>,
    body: &crate::SemanticBody<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    body_span: Span,
) -> Result<
    crate::SemanticImportedBody<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    crate::SemanticBodyImportFailure,
> {
    use crate::{SemanticBodyImportFailure as BF, StableDefinitionKind as DK};
    use crate::{
        SemanticImportFailure as F, SemanticImportNominalKind as NK, SemanticImportType as T,
    };

    fn collect_fixed_strings<K, M>(ty: &T<K, M>, capacities: &mut std::collections::BTreeSet<u64>) {
        match ty {
            T::BuiltinNominal {
                name,
                kind: NK::Struct,
            } => {
                if let Some(capacity) = name
                    .strip_prefix("Str(")
                    .and_then(|name| name.strip_suffix(')'))
                    .and_then(|capacity| capacity.parse().ok())
                {
                    capacities.insert(capacity);
                }
            }
            T::Array { element, .. }
            | T::Slice { element, .. }
            | T::PtrConst(element)
            | T::PtrMut(element) => collect_fixed_strings(element, capacities),
            _ => {}
        }
    }

    let mut fixed_string_capacities = std::collections::BTreeSet::new();
    collect_fixed_strings(&body.return_type, &mut fixed_string_capacities);
    for instruction in body.instructions.iter() {
        collect_fixed_strings(&instruction.ty, &mut fixed_string_capacities);
        instruction.data.visit_dependencies(&mut |dependency| {
            if let crate::SemanticBodyInstDependency::Type(ty) = dependency {
                collect_fixed_strings(ty, &mut fixed_string_capacities);
            }
        });
    }
    for place in body.places.iter() {
        collect_fixed_strings(&place.base_type, &mut fixed_string_capacities);
        for projection in place.projections.iter() {
            if let crate::SemanticBodyProjection::Index { array_type, .. } = projection {
                collect_fixed_strings(array_type, &mut fixed_string_capacities);
            }
        }
    }
    for (_, ty) in body.param_drops.iter() {
        collect_fixed_strings(ty, &mut fixed_string_capacities);
    }
    for capacity in fixed_string_capacities {
        sema.get_or_create_str_fixed_struct(capacity, body_span)
            .map_err(|_| BF::Semantic(F::InvalidStructuralType))?;
    }

    fn definition_endpoint<'a>(
        sema: &'a BodySema<'_>,
        token: &crate::SemanticDefinitionToken,
    ) -> Result<&'a crate::SemanticDefinitionEndpoint, BF> {
        if let Some(endpoint) = sema.stable_definition_endpoints.get(token) {
            return Ok(endpoint);
        }
        let failure = if sema
            .stable_definition_endpoints
            .keys()
            .any(|candidate| candidate.issuer() == token.issuer())
        {
            crate::SemanticStableResolutionFailure::Missing
        } else {
            crate::SemanticStableResolutionFailure::ForeignIssuer
        };
        Err(BF::StableResolution(failure))
    }

    fn module_endpoint<'a>(
        sema: &'a BodySema<'_>,
        token: &crate::SemanticModuleToken,
    ) -> Result<&'a crate::SemanticModuleEndpoint, BF> {
        if let Some(endpoint) = sema.stable_module_endpoints.get(token) {
            return Ok(endpoint);
        }
        let failure = if sema
            .stable_module_endpoints
            .keys()
            .any(|candidate| candidate.issuer() == token.issuer())
        {
            crate::SemanticStableResolutionFailure::Missing
        } else {
            crate::SemanticStableResolutionFailure::ForeignIssuer
        };
        Err(BF::StableResolution(failure))
    }

    fn import_type(
        sema: &BodySema<'_>,
        pool: &crate::TypeInternPool,
        value: &T<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    ) -> Result<Type, BF> {
        Ok(match value {
            T::I8 => Type::I8,
            T::I16 => Type::I16,
            T::I32 => Type::I32,
            T::I64 => Type::I64,
            T::U8 => Type::U8,
            T::U16 => Type::U16,
            T::U32 => Type::U32,
            T::U64 => Type::U64,
            T::Bool => Type::BOOL,
            T::Unit => Type::UNIT,
            T::Never => Type::NEVER,
            T::ComptimeType => Type::COMPTIME_TYPE,
            T::BuiltinNominal { name, kind } => {
                let symbol = sema
                    .interner
                    .get(name.as_ref())
                    .ok_or(F::UnknownBuiltinNominal)?;
                match kind {
                    NK::Struct => Type::new_struct(
                        *sema
                            .builtin_structs
                            .get(&symbol)
                            .or_else(|| sema.generated_structs.get(&symbol))
                            .ok_or(F::UnknownBuiltinNominal)?,
                    ),
                    NK::Enum => Type::new_enum(
                        *sema
                            .builtin_enums
                            .get(&symbol)
                            .ok_or(F::UnknownBuiltinNominal)?,
                    ),
                }
            }
            T::Nominal(identity) => {
                let identity = definition_endpoint(sema, identity)?;
                let symbol = sema
                    .interner
                    .get(identity.name.as_ref())
                    .ok_or(F::MissingNominal)?;
                match identity.kind {
                    DK::Struct => Type::new_struct(
                        *sema
                            .structs_by_file_name
                            .get(&(FileId::new(identity.file), symbol))
                            .ok_or(F::MissingNominal)?,
                    ),
                    DK::Enum => Type::new_enum(
                        *sema
                            .enums_by_file_name
                            .get(&(FileId::new(identity.file), symbol))
                            .ok_or(F::MissingNominal)?,
                    ),
                    _ => {
                        return Err(BF::StableResolution(
                            crate::SemanticStableResolutionFailure::WrongKind,
                        ));
                    }
                }
            }
            T::AnonymousNominal(identity) => {
                // Validate the entire recursive key against this request's
                // issuer universe before consulting exact materialization.
                // Mapping is identity-preserving and allocates no AIR state.
                let _: crate::AnonymousNominalKey<_, _> = identity.try_map_identities(
                    &|token| {
                        definition_endpoint(sema, token)?;
                        Ok::<_, BF>(*token)
                    },
                    &|token| {
                        module_endpoint(sema, token)?;
                        Ok::<_, BF>(*token)
                    },
                )?;
                let ty = match identity.kind {
                    crate::AnonymousNominalKind::Struct => {
                        Type::new_struct(*sema.anon_struct_identities.get(identity).ok_or(
                            BF::StableResolution(crate::SemanticStableResolutionFailure::Missing),
                        )?)
                    }
                    crate::AnonymousNominalKind::Enum => {
                        Type::new_enum(*sema.anon_enum_identities.get(identity).ok_or(
                            BF::StableResolution(crate::SemanticStableResolutionFailure::Missing),
                        )?)
                    }
                };
                if sema.canonical_anonymous_types.get(&ty) != Some(identity) {
                    return Err(BF::StableResolution(
                        crate::SemanticStableResolutionFailure::WrongKind,
                    ));
                }
                ty
            }
            T::Array { element, len } => pool
                .try_intern_array(import_type(sema, pool, element)?, *len)
                .map_err(|_| F::InvalidStructuralType)?,
            T::Slice { element, name } => {
                let element = import_type(sema, pool, element)?;
                let symbol = sema
                    .interner
                    .get(name.as_ref())
                    .ok_or(F::UnknownBuiltinNominal)?;
                let id = *sema
                    .generated_structs
                    .get(&symbol)
                    .ok_or(F::UnknownBuiltinNominal)?;
                let def = sema.type_pool.struct_def(id);
                let matches_element = def.fields.first().is_some_and(|field| {
                    matches!(field.ty.kind(), TypeKind::PtrConst(pointer)
                        if sema.type_pool.ptr_const_def(pointer) == element)
                });
                if !matches_element {
                    return Err(BF::Semantic(F::InvalidStructuralType));
                }
                Type::new_struct(id)
            }
            T::PtrConst(value) => pool
                .try_intern_ptr_const(import_type(sema, pool, value)?)
                .map_err(|_| F::InvalidStructuralType)?,
            T::PtrMut(value) => pool
                .try_intern_ptr_mut(import_type(sema, pool, value)?)
                .map_err(|_| F::InvalidStructuralType)?,
            T::Module(module) => {
                let module = module_endpoint(sema, module)?;
                let id = (0..sema.module_registry.len())
                    .map(|i| ModuleId::new(i as u32))
                    .find(|id| {
                        sema.module_registry.get_def(*id).file_id == FileId::new(module.file)
                    })
                    .ok_or(BF::Semantic(F::MissingModule))?;
                Type::new_module(id)
            }
            T::GenericParameter(_) => {
                return Err(BF::Semantic(F::GenericParameterNeedsDeclarationContext));
            }
        })
    }

    fn resolve_struct_nominal(
        sema: &BodySema<'_>,
        identity: &crate::NominalInstanceKey<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    ) -> Result<crate::StructId, BF> {
        match identity {
            crate::NominalInstanceKey::Builtin {
                kind: crate::AnonymousNominalKind::Struct,
                name,
            } => {
                let name = sema
                    .interner
                    .get(name.as_ref())
                    .ok_or(BF::Semantic(F::MissingNominal))?;
                sema.builtin_structs
                    .get(&name)
                    .or_else(|| sema.generated_structs.get(&name))
                    .copied()
                    .ok_or(BF::Semantic(F::MissingNominal))
            }
            crate::NominalInstanceKey::Builtin { .. } => Err(BF::StableResolution(
                crate::SemanticStableResolutionFailure::WrongKind,
            )),
            crate::NominalInstanceKey::Named(token) => {
                let identity = definition_endpoint(sema, token)?;
                if identity.kind != DK::Struct {
                    return Err(BF::StableResolution(
                        crate::SemanticStableResolutionFailure::WrongKind,
                    ));
                }
                let name = sema
                    .interner
                    .get(identity.name.as_ref())
                    .ok_or(BF::Semantic(F::MissingNominal))?;
                sema.structs_by_file_name
                    .get(&(FileId::new(identity.file), name))
                    .copied()
                    .ok_or(BF::Semantic(F::MissingNominal))
            }
            crate::NominalInstanceKey::Anonymous(key)
                if key.kind == crate::AnonymousNominalKind::Struct =>
            {
                let _: crate::AnonymousNominalKey<_, _> = key.try_map_identities(
                    &|token| {
                        definition_endpoint(sema, token)?;
                        Ok::<_, BF>(*token)
                    },
                    &|token| {
                        module_endpoint(sema, token)?;
                        Ok::<_, BF>(*token)
                    },
                )?;
                sema.anon_struct_identities
                    .get(key)
                    .copied()
                    .ok_or(BF::StableResolution(
                        crate::SemanticStableResolutionFailure::Missing,
                    ))
            }
            crate::NominalInstanceKey::Anonymous(_) => Err(BF::StableResolution(
                crate::SemanticStableResolutionFailure::WrongKind,
            )),
        }
    }

    fn resolve_enum_nominal(
        sema: &BodySema<'_>,
        identity: &crate::NominalInstanceKey<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    ) -> Result<crate::EnumId, BF> {
        match identity {
            crate::NominalInstanceKey::Builtin {
                kind: crate::AnonymousNominalKind::Enum,
                name,
            } => {
                let name = sema
                    .interner
                    .get(name.as_ref())
                    .ok_or(BF::Semantic(F::MissingNominal))?;
                sema.builtin_enums
                    .get(&name)
                    .copied()
                    .ok_or(BF::Semantic(F::MissingNominal))
            }
            crate::NominalInstanceKey::Builtin { .. } => Err(BF::StableResolution(
                crate::SemanticStableResolutionFailure::WrongKind,
            )),
            crate::NominalInstanceKey::Named(token) => {
                let identity = definition_endpoint(sema, token)?;
                if identity.kind != DK::Enum {
                    return Err(BF::StableResolution(
                        crate::SemanticStableResolutionFailure::WrongKind,
                    ));
                }
                let name = sema
                    .interner
                    .get(identity.name.as_ref())
                    .ok_or(BF::Semantic(F::MissingNominal))?;
                sema.enums_by_file_name
                    .get(&(FileId::new(identity.file), name))
                    .copied()
                    .ok_or(BF::Semantic(F::MissingNominal))
            }
            crate::NominalInstanceKey::Anonymous(key)
                if key.kind == crate::AnonymousNominalKind::Enum =>
            {
                let _: crate::AnonymousNominalKey<_, _> = key.try_map_identities(
                    &|token| {
                        definition_endpoint(sema, token)?;
                        Ok::<_, BF>(*token)
                    },
                    &|token| {
                        module_endpoint(sema, token)?;
                        Ok::<_, BF>(*token)
                    },
                )?;
                sema.anon_enum_identities
                    .get(key)
                    .copied()
                    .ok_or(BF::StableResolution(
                        crate::SemanticStableResolutionFailure::Missing,
                    ))
            }
            crate::NominalInstanceKey::Anonymous(_) => Err(BF::StableResolution(
                crate::SemanticStableResolutionFailure::WrongKind,
            )),
        }
    }

    fn resolve_body_function_definition(
        sema: &BodySema<'_>,
        token: &crate::SemanticDefinitionToken,
    ) -> Result<Spur, BF> {
        let identity = definition_endpoint(sema, token)?;
        let name = sema
            .interner
            .get(identity.name.as_ref())
            .ok_or(BF::Semantic(F::MissingFunction))?;
        if identity.kind == DK::Function {
            return sema
                .functions_by_file_name
                .get(&(FileId::new(identity.file), name))
                .copied()
                .ok_or(BF::Semantic(F::MissingFunction));
        }
        let owner = identity
            .owner
            .as_deref()
            .and_then(|owner| sema.interner.get(owner))
            .ok_or(BF::Semantic(F::MissingFunction))?;
        let struct_id = sema
            .structs_by_file_name
            .get(&(FileId::new(identity.file), owner))
            .copied()
            .ok_or(BF::Semantic(F::MissingFunction))?;
        let info = sema
            .call_facts()
            .method_info(struct_id, name)
            .ok_or(BF::Semantic(F::MissingFunction))?;
        let expected = match identity.kind {
            DK::Method => info.has_self && identity.name.as_ref() != "__drop",
            DK::AssociatedFunction => !info.has_self,
            DK::Destructor => info.has_self && identity.name.as_ref() == "__drop",
            _ => {
                return Err(BF::StableResolution(
                    crate::SemanticStableResolutionFailure::WrongKind,
                ));
            }
        };
        if !expected {
            return Err(BF::Semantic(F::MissingFunction));
        }
        let symbol = sema.method_symbol(struct_id, identity.name.as_ref(), info.has_self);
        sema.interner
            .get(&symbol)
            .ok_or(BF::Semantic(F::MissingFunction))
    }

    fn resolve_body_function(
        sema: &BodySema<'_>,
        identity: &crate::FunctionInstanceKey<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    ) -> Result<Spur, BF> {
        match identity {
            crate::FunctionInstanceKey::Definition(token) => {
                resolve_body_function_definition(sema, token)
            }
            crate::FunctionInstanceKey::AnonymousMember { owner, member } => {
                let crate::TypeInstanceKey::Nominal(owner) = owner.as_ref() else {
                    return Err(BF::StableResolution(
                        crate::SemanticStableResolutionFailure::WrongKind,
                    ));
                };
                let struct_id = resolve_struct_nominal(sema, owner)?;
                let name = sema
                    .interner
                    .get(member.name.as_ref())
                    .ok_or(BF::Semantic(F::MissingFunction))?;
                let info = sema
                    .call_facts()
                    .method_info(struct_id, name)
                    .ok_or(BF::Semantic(F::MissingFunction))?;
                let expected = match member.kind {
                    crate::AnonymousMemberKind::Method => {
                        info.has_self && member.name.as_ref() != "__drop"
                    }
                    crate::AnonymousMemberKind::AssociatedFunction => !info.has_self,
                    crate::AnonymousMemberKind::Destructor => {
                        info.has_self && member.name.as_ref() == "__drop"
                    }
                };
                if !expected {
                    return Err(BF::StableResolution(
                        crate::SemanticStableResolutionFailure::WrongKind,
                    ));
                }
                let symbol = sema.method_symbol(struct_id, member.name.as_ref(), info.has_self);
                sema.interner
                    .get(&symbol)
                    .ok_or(BF::Semantic(F::MissingFunction))
            }
            _ => Err(BF::StableResolution(
                crate::SemanticStableResolutionFailure::WrongKind,
            )),
        }
    }

    // Intrinsic and declaration-derived callable symbols are required to
    // pre-exist. Runtime-call helpers are ABI symbols rather than declarations;
    // the durable body carries their exact RuntimeCallKind and imports the
    // corresponding helper symbol below.
    if body.instructions.iter().any(|inst| matches!(&inst.data,
        crate::SemanticBodyInstData::Intrinsic { name, .. } if sema.interner.get(name.as_ref()).is_none())) {
        return Err(BF::Semantic(F::MissingFunction));
    }
    let scratch = sema.type_pool.clone();
    let imported = crate::SemanticImportEpoch::import_body_with(
        body,
        body_span,
        &scratch,
        true,
        |value| import_type(sema, &scratch, value),
        |identity| resolve_struct_nominal(sema, identity),
        |identity| resolve_enum_nominal(sema, identity),
        |identity| resolve_body_function(sema, identity),
        |identity| {
            let base = resolve_body_function_definition(sema, &identity.base)?;
            let type_arguments = identity
                .type_arguments
                .iter()
                .map(|value| import_type(sema, &scratch, value))
                .collect::<Result<Vec<_>, _>>()?;
            let value_arguments = identity
                .value_arguments
                .iter()
                .map(|value| {
                    Ok(match value {
                        crate::SemanticImportConstValue::Integer(value) => {
                            crate::sema::ConstValue::Integer(*value)
                        }
                        crate::SemanticImportConstValue::Bool(value) => {
                            crate::sema::ConstValue::Bool(*value)
                        }
                        crate::SemanticImportConstValue::Type(value) => {
                            crate::sema::ConstValue::Type(import_type(sema, &scratch, value)?)
                        }
                        crate::SemanticImportConstValue::Function(value) => {
                            crate::sema::ConstValue::Function(resolve_body_function_definition(
                                sema, value,
                            )?)
                        }
                        crate::SemanticImportConstValue::Unit => crate::sema::ConstValue::Unit,
                        crate::SemanticImportConstValue::String(content) => {
                            crate::sema::ConstValue::String(
                                sema.interner.get_or_intern(content.as_ref()),
                            )
                        }
                    })
                })
                .collect::<Result<Vec<_>, BF>>()?;
            let name = crate::specialize::mangle_specialized_name(
                sema.interner.resolve(&base),
                &type_arguments,
                &value_arguments,
            );
            Ok((
                sema.interner.get_or_intern(&name),
                type_arguments,
                value_arguments,
            ))
        },
        |name| sema.interner.get_or_intern(name),
    )?;
    sema.type_pool = scratch;
    // Project the recorded method references into this session's live keys.
    // Every receiver/method a durable body legitimately recorded is resolvable
    // exactly when its corresponding `Call` resolved above; anything else
    // fails the whole import closed rather than dropping a reference.
    let mut imported = imported;
    let mut method_references = HashSet::with_capacity(body.method_references.len());
    for reference in body.method_references.iter() {
        let struct_id = resolve_struct_nominal(sema, &reference.receiver)?;
        let method = sema
            .interner
            .get(reference.method.as_ref())
            .ok_or(BF::Semantic(F::MissingFunction))?;
        method_references.insert((struct_id, method));
    }
    imported.method_references = method_references;
    Ok(imported)
}

/// The reachability references of an imported body: free functions are
/// classified by direct symbol membership in the declaration table, and method
/// references are read from the body's recorded reference set — the payload
/// captured when resolution selected each method winner — so no rendered
/// callable symbol is ever reversed back into a method key (RUE-1128).
pub(crate) fn imported_body_references(
    sema: &BodySema<'_>,
    imported: &crate::SemanticImportedBody<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >,
) -> (HashSet<Spur>, HashSet<(crate::StructId, Spur)>) {
    let mut functions = HashSet::new();
    let facts = sema.call_facts();
    for instruction in imported.air.instructions() {
        let AirInstData::Call { name, .. } = instruction.data else {
            continue;
        };
        if facts.function_contains(name) {
            functions.insert(name);
        }
    }
    (functions, imported.method_references.clone())
}

#[cfg(test)]
pub(crate) fn analyze_all_function_bodies_with_namespace_probe_for_test(
    mut sema: BodySema<'_>,
) -> (
    MultiErrorResult<SemaOutput>,
    super::NamespaceBoundarySnapshot,
    super::NamespaceBoundarySnapshot,
) {
    let before = sema.namespace_boundary_snapshot();
    let result = analyze_all_function_bodies_mut_for_test(&mut sema);
    let after = sema.namespace_boundary_snapshot();
    (result, before, after)
}

fn analyze_all_function_bodies_mut_for_test(
    sema: &mut BodySema<'_>,
) -> MultiErrorResult<SemaOutput> {
    debug_assert!(!sema.declaration_binding_active);
    debug_assert!(sema.const_resolution_in_progress.is_empty());
    debug_assert!(sema.fn_signatures_in_flight.is_empty());
    debug_assert!(sema.source_free_function_signatures_are_complete());
    intern_named_callable_symbols(sema);
    let bound_source_function_signature_count = sema.functions_by_file_name.len();

    // ADR-0045 defines reachability from `main` as the function-body analysis
    // frontier for every executable.
    let result = analyze_function_bodies_lazy(sema);
    debug_assert_eq!(
        sema.functions_by_file_name.len(),
        bound_source_function_signature_count,
        "body analysis may not insert or remove source function signatures"
    );
    debug_assert!(sema.source_free_function_signatures_are_complete());

    // Sema→CFG boundary invariant (RUE-153): a value may only carry the
    // `<error>` type as part of error recovery, i.e. when at least one
    // diagnostic has already been emitted. If analysis reports `Ok` (no
    // errors) yet some AIR value is still `<error>`-typed, an inference
    // variable decayed to `<error>` on a path that forgot to emit its
    // diagnostic (the RUE-149 class). That value would otherwise reach
    // codegen and hit an `unreachable!()`; convert it into an actionable
    // internal-error diagnostic (E9000) here instead.
    if let Ok(output) = &result {
        if let Some(err) = find_undiagnosed_error_type(output) {
            return Err(CompileErrors::from(err));
        }
    }

    result
}

/// Materialize every qualified named callable symbol after declaration
/// binding, before either fresh analysis or durable import can observe the
/// body worklist. Hash-map iteration order must not affect Spur allocation.
fn intern_named_callable_symbols(sema: &BodySema<'_>) {
    let mut symbols = sema
        .methods
        .iter()
        // Membership, not the generated-name prefix (RUE-1050).
        .filter(|((struct_id, _), _)| !sema.anonymous_struct_ids.contains(struct_id))
        .map(|(&(struct_id, method), info)| {
            sema.method_symbol(struct_id, sema.interner.resolve(&method), info.has_self)
        })
        .collect::<Vec<_>>();
    symbols.sort();
    symbols.dedup();
    for symbol in symbols {
        sema.interner.get_or_intern(&symbol);
    }
}

/// Scan analyzed AIR for an `<error>`-typed value that survived analysis with
/// no diagnostic emitted (see the invariant in the test-only whole-program oracle).
///
/// Returns the first offending instruction as an internal-error `CompileError`
/// (E9000), or `None` when every value is well-typed. Only called on the
/// success (`Ok`) path, so any `<error>` found here is by definition
/// undiagnosed and indicates a compiler bug.
fn find_undiagnosed_error_type(output: &SemaOutput) -> Option<CompileError> {
    if let Err(error) = output.type_pool.validate_for_success() {
        return Some(CompileError::without_span(ErrorKind::InternalError(
            format!(
                "the completed type pool contains an invalid or recovery-only type graph but no diagnostic was emitted: {error:?} (RUE-836)"
            ),
        )));
    }
    for func in &output.functions {
        if func.air.return_type().is_error() {
            return Some(CompileError::without_span(ErrorKind::InternalError(
                format!(
                    "function '{}' has an <error> return type but no diagnostic was \
                 emitted; an inference variable decayed to <error> without \
                 reporting an error (RUE-153)",
                    func.name
                ),
            )));
        }
        for (_air_ref, inst) in func.air.iter() {
            if inst.ty.is_error() {
                return Some(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "an <error>-typed value reached the end of semantic \
                         analysis in function '{}' but no diagnostic was \
                         emitted; an inference variable decayed to <error> \
                         without reporting an error (RUE-153)",
                        func.name
                    )),
                    inst.span,
                ));
            }
        }
    }
    None
}

/// Shared finalization for demand-driven function-body analysis.
fn finalize_function_body_analysis(
    sema: &mut BodySema<'_>,
    functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)>,
    active_aggregate_types: &HashSet<Type>,
    mut all_warnings: Vec<CompileWarning>,
    unused_function_roots: &HashSet<Spur>,
    errors: CompileErrors,
) -> MultiErrorResult<SemaOutput> {
    // String IDs flow into AIR, assembly, rodata, and relocations. Assign them
    // by content rather than function-discovery order so worklist scheduling
    // cannot change any downstream representation (RUE-624).
    let mut global_strings: Vec<String> = functions_with_strings
        .iter()
        .flat_map(|(_, local_strings)| local_strings.iter().cloned())
        .collect();
    global_strings.sort();
    global_strings.dedup();
    let global_string_table: HashMap<String, u32> = global_strings
        .iter()
        .cloned()
        .enumerate()
        .map(|(id, string)| (string, id as u32))
        .collect();

    let mut functions: Vec<AnalyzedFunction> = Vec::new();
    for (mut analyzed, local_strings) in functions_with_strings {
        sema.body_analysis_work.string_ids_remapped += local_strings.len();
        if !local_strings.is_empty() {
            let local_to_global: Vec<u32> = local_strings
                .into_iter()
                .map(|string| {
                    *global_string_table
                        .get(&string)
                        .expect("every local string was collected globally")
                })
                .collect();

            let mut editor = analyzed.air.into_editor();
            editor.remap_string_ids(|local_id| local_to_global[local_id as usize]);
            analyzed.air = match editor.finish(crate::AirValidationContext::SemanticWithSymbols(
                &sema.type_pool,
                sema.interner,
            )) {
                Ok(air) => air,
                Err(error) => return Err(CompileError::from(error).into()),
            };

            for atom in &mut analyzed.local_atoms {
                let Some(global_id) = local_to_global.get(atom.dense_id as usize) else {
                    return Err(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "local atom in '{}' references missing string {}",
                            analyzed.name, atom.dense_id
                        )),
                        Span::default(),
                    )
                    .into());
                };
                atom.dense_id = *global_id;
                if global_strings[*global_id as usize].as_str() != atom.content.as_ref() {
                    return Err(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "local atom content disagrees with string table in '{}'",
                            analyzed.name
                        )),
                        Span::default(),
                    )
                    .into());
                }
            }
        }
        analyzed
            .local_atoms
            .sort_by(|left, right| left.identity.cmp(&right.identity));
        if analyzed
            .local_atoms
            .windows(2)
            .any(|pair| pair[0].identity == pair[1].identity)
        {
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "duplicate local atom identity in '{}'",
                    analyzed.name
                )),
                Span::default(),
            )
            .into());
        }
        functions.push(analyzed);
    }

    let mut referenced_for_unused_warnings = collect_static_function_references(sema);
    referenced_for_unused_warnings.extend(unused_function_roots.iter().copied());
    add_unused_function_warnings(sema, &referenced_for_unused_warnings, &mut all_warnings);
    all_warnings.sort_by_key(|w| w.span().map(|s| s.start));

    // Error results do not expose a SemaOutput, so do not manufacture a
    // frozen pool merely to discard it. In particular, an unresolved
    // declaration must remain visibly Declared rather than being laundered
    // into an empty Complete definition to satisfy freeze.
    if !errors.is_empty() {
        return Err(errors);
    }

    let aggregate_type_identities_by_type = active_aggregate_types
        .iter()
        .copied()
        .map(|ty| {
            sema.canonical_type_instance(ty)
                .map(|identity| (ty, identity))
                .map_err(|failure| {
                    CompileError::without_span(ErrorKind::InternalError(format!(
                        "failed to issue canonical aggregate identity: {failure:?}"
                    )))
                })
        })
        .collect::<CompileResult<HashMap<_, _>>>()?;
    let aggregate_types_by_identity = aggregate_type_identities_by_type
        .iter()
        .map(|(ty, identity)| (identity.clone(), *ty))
        .collect();

    let output = SemaOutput {
        functions,
        strings: global_strings,
        warnings: all_warnings,
        anonymous_nominal_identities_by_type: sema
            .canonical_anonymous_types
            .iter()
            .filter(|(ty, _)| active_aggregate_types.contains(ty))
            .map(|(ty, identity)| (*ty, identity.clone()))
            .collect(),
        aggregate_type_identities_by_type,
        aggregate_types_by_identity,
        body_analysis_work: sema.body_analysis_work,
        ordinary_body_exports: std::mem::take(&mut sema.ordinary_body_exports),
        specialized_body_exports: std::mem::take(&mut sema.specialized_body_exports),
        analyzed_body_owners: {
            sema.analyzed_body_owners.sort();
            sema.analyzed_body_owners.dedup();
            sema.analyzed_body_owners.clone()
        },
        body_named_dependencies: {
            sema.body_named_dependencies.sort();
            sema.body_named_dependencies.dedup();
            sema.body_named_dependencies.clone()
        },
        declaration_type_dependencies: {
            sema.declaration_type_dependencies.sort();
            sema.declaration_type_dependencies.dedup();
            sema.declaration_type_dependencies.clone()
        },
        declaration_type_dependencies_complete: true,
        declaration_type_call_head_dependencies: {
            sema.declaration_type_call_head_dependencies.sort();
            sema.declaration_type_call_head_dependencies.dedup();
            sema.declaration_type_call_head_dependencies.clone()
        },
        // Every successful declaration type-call is resolved through one of
        // the observer-backed paths above: a named free type constructor or
        // the separately tagged `Str(N)` builtin. Unsupported/dynamic heads
        // fail type checking and therefore cannot produce a successful
        // manifest that silently omits an edge.
        declaration_type_call_head_dependencies_complete: true,
        declaration_builtin_type_call_head_dependencies: {
            sema.declaration_builtin_type_call_head_dependencies.sort();
            sema.declaration_builtin_type_call_head_dependencies.dedup();
            sema.declaration_builtin_type_call_head_dependencies.clone()
        },
        supported_type_call_heads_complete: true,
        named_const_dependencies: {
            let bound_constants = index_bound_named_constants(
                sema.value_consts()
                    .map(|(key, _)| key)
                    .map(|(file, name)| (file.index(), sema.interner.resolve(name).to_owned())),
            );
            retain_bound_named_const_dependencies(
                &mut sema.named_const_dependencies,
                &bound_constants,
            );
            sema.named_const_dependencies.sort();
            sema.named_const_dependencies.dedup();
            sema.named_const_dependencies.clone()
        },
        named_value_const_dependencies_complete: true,
        // Body analysis and specialization can intern composite and anonymous
        // types. Transfer the completed pool only after every finalization
        // consumer above has finished reading semantic state.
        // This transfer is deliberately last: specialization and anonymous
        // destructor discovery above are the final operations allowed to
        // extend the semantic type universe.
        type_pool: std::mem::take(&mut sema.type_pool).freeze(),
    };

    Ok(output)
}

type NamedConstSourceIndex = HashMap<u32, HashSet<String>>;

fn index_bound_named_constants(
    bound_constants: impl IntoIterator<Item = (u32, String)>,
) -> NamedConstSourceIndex {
    index_bound_named_constants_with_observer(bound_constants, || {})
}

#[inline(always)]
fn index_bound_named_constants_with_observer(
    bound_constants: impl IntoIterator<Item = (u32, String)>,
    mut observe_constant: impl FnMut(),
) -> NamedConstSourceIndex {
    let mut bound_by_file = NamedConstSourceIndex::new();
    for (file, name) in bound_constants {
        observe_constant();
        bound_by_file.entry(file).or_default().insert(name);
    }
    bound_by_file
}

fn retain_bound_named_const_dependencies(
    dependencies: &mut Vec<super::NamedConstDependencyEvent>,
    bound_constants: &NamedConstSourceIndex,
) {
    retain_bound_named_const_dependencies_with_observer(dependencies, bound_constants, || {});
}

#[inline(always)]
fn retain_bound_named_const_dependencies_with_observer(
    dependencies: &mut Vec<super::NamedConstDependencyEvent>,
    bound_constants: &NamedConstSourceIndex,
    mut observe_event: impl FnMut(),
) {
    dependencies.retain(|event| {
        observe_event();
        bound_constants
            .get(&event.source_file)
            .is_some_and(|names| names.contains(event.source_name.as_str()))
    });
}

/// Emit warnings for unused free functions.
///
/// This intentionally excludes methods/destructors; they have different
/// reachability rules and are not covered by the current spec/UI cases.
fn add_unused_function_warnings(
    sema: &BodySema<'_>,
    referenced_functions: &HashSet<Spur>,
    warnings: &mut Vec<CompileWarning>,
) {
    let main_sym = sema.interner.get("main");

    for (name, info) in &sema.functions {
        let source_name = sema.source_function_name(*name);
        let name_str = sema.interner.resolve(&source_name);
        if Some(*name) == main_sym
            || info.is_pub
            || info.allow_unused_function
            || name_str.starts_with('_')
            || referenced_functions.contains(name)
        {
            continue;
        }

        warnings.push(
            CompileWarning::new(WarningKind::UnusedFunction(name_str.to_string()), info.span)
                .with_help(format!(
                    "if this is intentional, prefix it with an underscore: `_{name_str}`"
                )),
        );
    }
}

fn collect_static_function_references(sema: &BodySema<'_>) -> HashSet<Spur> {
    let mut referenced = HashSet::new();

    for (_, inst) in sema.rir.iter() {
        if let InstData::Call { name, .. } = &inst.data
            && let Some(function_key) =
                resolve_static_call_reference(&sema.call_facts(), *name, inst.span.file_id)
        {
            referenced.insert(function_key);
        }
    }

    // Type-position call heads are selected by the canonical semantic type
    // driver. Project those exact observations into warning reachability
    // instead of reparsing RIR type strings with a peer syntax walker. This
    // includes value-returning comptime calls used only in array lengths.
    for event in &sema.declaration_type_call_head_dependencies {
        let Some(name) = sema.interner.get(&event.callable_name) else {
            continue;
        };
        if let Some(function) = sema
            .call_facts()
            .resolve_function_name_local(name, FileId::new(event.callable_file))
        {
            referenced.insert(function);
        }
    }

    referenced
}

/// Move newly referenced functions/methods onto the lazy-analysis work queues
/// in a deterministic order.
///
/// `analyze_single_function` (and its method/destructor siblings) collect
/// references as `HashSet`s, whose iteration order is randomized per process.
/// Pushing them unsorted made the whole lazy-analysis order — and with it the
/// order diagnostics are emitted in AND the function order handed to codegen —
/// differ between identical runs (RUE-513: two files with independent sema
/// errors reported them in a random relative order). Sorting by resolved name
/// (and struct id) restores run-to-run determinism at the only place the
/// nondeterminism enters.
fn enqueue_references_sorted(
    interner: &ThreadedRodeo,
    referenced_fns: HashSet<Spur>,
    referenced_meths: HashSet<(StructId, Spur)>,
    analyzed_functions: &HashSet<Spur>,
    analyzed_methods: &HashSet<(StructId, Spur)>,
    pending_functions: &mut Vec<Spur>,
    pending_methods: &mut Vec<(StructId, Spur)>,
) {
    let mut fns: Vec<Spur> = referenced_fns
        .into_iter()
        .filter(|f| !analyzed_functions.contains(f))
        .collect();
    fns.sort_by_key(|f| interner.resolve(f));
    pending_functions.extend(fns);

    let mut meths: Vec<(StructId, Spur)> = referenced_meths
        .into_iter()
        .filter(|m| !analyzed_methods.contains(m))
        .collect();
    meths.sort_by_key(|&(sid, name)| (sid.0, interner.resolve(&name)));
    pending_methods.extend(meths);
}

fn extend_owned_aggregate_types(
    sema: &BodySema<'_>,
    roots: impl IntoIterator<Item = Type>,
    active: &mut HashSet<Type>,
) {
    fn visit(sema: &BodySema<'_>, ty: Type, active: &mut HashSet<Type>) {
        match ty.kind() {
            TypeKind::Struct(id) => {
                if !active.insert(ty) {
                    return;
                }
                for field in &sema.type_pool.struct_def(id).fields {
                    visit(sema, field.ty, active);
                }
            }
            TypeKind::Enum(id) => {
                if !active.insert(ty) {
                    return;
                }
                for payload in &sema.type_pool.enum_def(id).variant_payloads {
                    for &field in payload {
                        visit(sema, field, active);
                    }
                }
            }
            TypeKind::Array(id) => {
                if active.insert(ty) {
                    visit(sema, sema.type_pool.array_def(id).0, active);
                }
            }
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
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_)
            | TypeKind::Module(_)
            | TypeKind::Error
            | TypeKind::Never
            | TypeKind::ComptimeType => {}
        }
    }

    for ty in roots {
        visit(sema, ty, active);
    }
}

fn extend_committed_air_aggregate_types(
    sema: &BodySema<'_>,
    functions_with_strings: &[(AnalyzedFunction, Vec<String>)],
    active: &mut HashSet<Type>,
) {
    let types = functions_with_strings.iter().flat_map(|(function, _)| {
        std::iter::once(function.air.return_type())
            .chain(function.air.instructions().iter().map(|inst| inst.ty))
            .chain(function.air.places().iter().map(|place| place.base_type))
            .chain(function.air.param_drops().iter().map(|&(_, ty)| ty))
    });
    extend_owned_aggregate_types(sema, types, active);
}

/// Enqueue reachable anonymous destructors registered by comptime evaluation.
///
/// Drop glue, rather than user-written AIR, is their caller, so these implicit
/// roots need an explicit deterministic scan whenever the ordinary reference
/// frontier drains. Declaration-time aggregates are eager roots; a type made
/// during body analysis is a root only when the current attempt's AIR owns it.
/// Persistent slots created only by an abandoned attempt therefore stay inert.
fn enqueue_anonymous_destructors(
    sema: &BodySema<'_>,
    drop_marker_sym: Spur,
    active_aggregate_types: &HashSet<Type>,
    analyzed_methods: &HashSet<(StructId, Spur)>,
    pending_methods: &mut Vec<(StructId, Spur)>,
) {
    let mut destructors: Vec<(StructId, Spur)> = sema
        .anonymous_methods
        .keys()
        .copied()
        .filter(|&method_key @ (struct_id, method_name)| {
            method_name == drop_marker_sym
                && !analyzed_methods.contains(&method_key)
                && active_aggregate_types.contains(&Type::new_struct(struct_id))
        })
        .collect();
    destructors.sort_by_key(|&(sid, name)| (sid.0, sema.interner.resolve(&name)));
    pending_methods.extend(destructors);
}

/// Demand-driven body-analysis path (ADR-0045).
///
/// Ordinary function and method bodies are analyzed only when reachable from
/// the entry point (`main`). Declaration gathering remains eager, and named
/// destructors are currently implicit roots because drop glue is synthesized
/// from the full type pool.
///
/// This is the same trade-off Zig makes for faster builds and smaller binaries.
fn analyze_function_bodies_lazy(sema: &mut BodySema<'_>) -> MultiErrorResult<SemaOutput> {
    // Register core `str` before freezing the shared inference context even
    // when source never spells it, because every unconstrained literal uses
    // this canonical identity.
    sema.get_or_create_str_struct(Span::default())
        .map_err(CompileErrors::from)?;

    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Find main() - the reference root for executable body analysis.
    let main_sym = match sema.interner.get("main") {
        Some(sym) if sema.call_facts().function_contains(sym) => sym,
        _ => {
            // No main function found - this is an error
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::NoMainFunction,
            )));
        }
    };

    // The runtime invokes `main` directly using a fixed zero-argument ABI.
    // Validate that boundary before body analysis so an invalid declaration
    // cannot be skipped as a generic specialization or reach codegen.
    let main_info = sema
        .functions
        .get(&main_sym)
        .expect("main was found in the function table");
    if !main_info.params.is_empty() {
        return Err(CompileErrors::from(CompileError::new(
            ErrorKind::InvalidMainSignature {
                reason: "`main` must not declare parameters",
            },
            main_info.span,
        )));
    }
    if !matches!(main_info.return_type, Type::I32 | Type::UNIT) {
        return Err(CompileErrors::from(CompileError::new(
            ErrorKind::InvalidMainSignature {
                reason: "`main` must return `i32` or `()`",
            },
            main_info.span,
        )));
    }

    // Declaration-time aggregates remain part of the eager semantic universe.
    // Body-created aggregates become active only when committed AIR owns them;
    // abandoned representative attempts may leave inert slots in the pool.
    let baseline_aggregate_types = sema
        .type_pool
        .all_struct_ids()
        .into_iter()
        .map(Type::new_struct)
        .chain(
            sema.type_pool
                .all_enum_ids()
                .into_iter()
                .map(Type::new_enum),
        )
        .chain(
            sema.type_pool
                .all_array_ids()
                .into_iter()
                .map(Type::new_array),
        )
        .collect::<HashSet<_>>();

    // Declaration-owned dependency records predate body analysis. Everything
    // appended after this snapshot is body-attempt state. Producer-nominal
    // identity is exact (RUE-1089), so there is no representative-change restart;
    // the snapshot is restored exactly once, immediately below, as the analysis
    // entry state.
    let baseline_analyzed_body_owners = sema.analyzed_body_owners.clone();
    let baseline_ordinary_body_exports = sema.ordinary_body_exports.clone();
    let baseline_specialized_body_exports = sema.specialized_body_exports.clone();
    let baseline_body_named_dependencies = sema.body_named_dependencies.clone();
    let baseline_declaration_type_dependencies = sema.declaration_type_dependencies.clone();
    let baseline_declaration_type_dependency_index = sema.declaration_type_dependency_index.clone();
    let baseline_declaration_type_call_head_dependencies =
        sema.declaration_type_call_head_dependencies.clone();
    let baseline_declaration_builtin_type_call_head_dependencies =
        sema.declaration_builtin_type_call_head_dependencies.clone();
    let baseline_named_const_dependencies = sema.named_const_dependencies.clone();
    let baseline_reusable_ordinary_bodies = sema.reusable_ordinary_bodies.clone();
    let baseline_reusable_specialized_bodies = sema.reusable_specialized_bodies.clone();

    // Producer-nominal identity is exact and stable (RUE-1089): no reached
    // member can change an anonymous representative, so body analysis is a
    // single attempt rather than a restart loop. The baseline snapshots are
    // restored once as the analysis entry state.
    {
        sema.analyzed_body_owners = baseline_analyzed_body_owners.clone();
        sema.ordinary_body_exports = baseline_ordinary_body_exports.clone();
        sema.specialized_body_exports = baseline_specialized_body_exports.clone();
        sema.body_named_dependencies = baseline_body_named_dependencies.clone();
        sema.declaration_type_dependencies = baseline_declaration_type_dependencies.clone();
        sema.declaration_type_dependency_index = baseline_declaration_type_dependency_index.clone();
        sema.declaration_type_call_head_dependencies =
            baseline_declaration_type_call_head_dependencies.clone();
        sema.declaration_builtin_type_call_head_dependencies =
            baseline_declaration_builtin_type_call_head_dependencies.clone();
        sema.named_const_dependencies = baseline_named_const_dependencies.clone();
        sema.reusable_ordinary_bodies = baseline_reusable_ordinary_bodies.clone();
        sema.reusable_specialized_bodies = baseline_reusable_specialized_bodies.clone();

        // Work queue: functions/methods to analyze
        // Start with main().
        let mut pending_functions: Vec<Spur> = vec![main_sym];
        // Rue-to-C exports (`pub extern "C" fn`, ADR-0064 P4) are additional
        // reachability roots: a separately compiled C caller may invoke an
        // exported function even when nothing in this program calls it, so its
        // body must be analyzed and code-generated exactly like `main`. Seeded
        // in a deterministic (interned-symbol) order so analysis order — and the
        // resulting object/link layout — stays reproducible.
        let mut export_roots: Vec<Spur> = sema
            .functions
            .iter()
            .filter(|(_, info)| info.is_c_export)
            .map(|(name, _)| *name)
            .collect();
        export_roots.sort_unstable();
        pending_functions.extend(export_roots);
        let mut analyzed_functions: HashSet<Spur> = HashSet::new();
        let mut pending_methods: Vec<(StructId, Spur)> = Vec::new();
        let mut analyzed_methods: HashSet<(StructId, Spur)> = HashSet::new();
        let drop_marker_sym = sema.interner.get_or_intern("__drop");
        let mut named_destructors_analyzed = false;
        let mut specializer = crate::specialize::Specializer::default();

        // Collect results. These values do not escape an invalidated attempt.
        let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)> = Vec::new();
        let mut active_aggregate_types = baseline_aggregate_types.clone();
        let mut active_function_count = 0usize;
        let mut errors = CompileErrors::new();
        let mut all_warnings = Vec::new();

        // Process the transitive reference frontier until neither source-level
        // calls nor implicit destructor roots discover more work.
        loop {
            // Process pending functions
            while let Some(fn_name) = pending_functions.pop() {
                if analyzed_functions.contains(&fn_name) {
                    continue;
                }
                analyzed_functions.insert(fn_name);

                // Look up the function info
                let fn_info = match sema.function_info(fn_name).copied() {
                    Some(info) => info,
                    None => continue, // Should not happen, but be defensive
                };
                let function_identity = match sema.function_identity(fn_name) {
                    Ok(token) => crate::FunctionInstanceKey::Definition(token),
                    Err(failure) => {
                        errors.push(CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to issue callable identity for '{}': {failure:?}",
                                sema.interner.resolve(&fn_name)
                            )),
                            fn_info.span,
                        ));
                        continue;
                    }
                };

                // Skip functions with comptime parameters - they are analyzed per specialization
                if fn_info.is_generic {
                    continue;
                }

                // Skip foreign `extern "C"` declarations (ADR-0064 C FFI): they
                // have no body and no CFG. A call to one lowers to an undefined
                // linker symbol resolved from a static archive, so the function
                // is never analyzed or code-generated here.
                if fn_info.is_extern {
                    continue;
                }

                let fn_name_str = sema.interner.resolve(&fn_name).to_string();

                // Bind the body through the exact free-function declaration that
                // declaration gathering indexed. A receiverless associated
                // function has the same FnDecl shape as a free function and must
                // never win this lookup merely because it occurs first in RIR.
                let source_name = sema.source_function_name(fn_name);
                sema.body_analysis_work.free_function_record_lookups += 1;
                let declaration = sema
                    .declaration_index
                    .first_free_function(source_name, Some(fn_info.file_id));

                let Some(declaration) = declaration else {
                    // This could be a builtin or otherwise non-existent function.
                    // Preserve the historical defensive behavior and skip it.
                    continue;
                };
                let inst = sema.rir.get(declaration);
                let (name, params, return_type, body, has_self, span) = if let InstData::FnDecl {
                    name,
                    params,
                    return_type,
                    body,
                    has_self,
                    ..
                } = &inst.data
                {
                    (*name, params, *return_type, *body, *has_self, inst.span)
                } else {
                    unreachable!("free-function index contains only FnDecl instructions");
                };

                debug_assert_eq!(name, source_name);
                debug_assert!(!has_self);
                debug_assert_eq!(params, fn_info.rir_params(sema.rir));
                debug_assert_eq!(return_type, fn_info.return_type_syntax);
                debug_assert_eq!(body, fn_info.body);
                debug_assert_eq!(span, fn_info.span);
                debug_assert_eq!(span.file_id, fn_info.file_id);

                let params = sema.rir.params(params);

                let ordinary_owner = sema.body_owner_token(
                    fn_info.file_id,
                    sema.interner.resolve(&source_name),
                    None,
                    super::BodyOwnerKind::FreeFunction,
                );
                if let Some(candidate) = sema.reusable_ordinary_bodies.remove(&ordinary_owner) {
                    sema.body_analysis_work.ordinary_body_import_attempts += 1;
                    let imported =
                        import_staged_body(sema, &candidate.body, sema.rir.get(body).span);
                    let import_failure = imported.as_ref().err().map(|reason| reason.kind());
                    if let Ok(imported) = imported {
                        all_warnings.extend(imported.warnings.iter().cloned());
                        sema.body_analysis_work.ordinary_body_import_successes += 1;
                        sema.body_analysis_work
                            .ordinary_body_import_instructions_installed += imported.air.len();
                        sema.body_analysis_work
                            .ordinary_body_import_places_installed += imported.air.places().len();
                        sema.body_analysis_work
                            .ordinary_body_import_strings_installed += imported.strings.len();
                        sema.body_analysis_work.ordinary_bodies_reused += 1;
                        sema.body_analysis_work.ordinary_body_analyses_skipped += 1;
                        let (referenced_fns, referenced_meths) =
                            imported_body_references(sema, &imported);
                        let mut ordered_referenced_fns =
                            referenced_fns.iter().copied().collect::<Vec<_>>();
                        ordered_referenced_fns
                            .sort_by_key(|name| sema.interner.resolve(name).to_owned());
                        let analyzed = AnalyzedFunction {
                            identity: function_identity.clone(),
                            callable_kind: crate::AnalyzedCallableKind::Ordinary,
                            name: fn_name_str,
                            ordinary_owner: Some(ordinary_owner),
                            implicit_drop_source: Some(
                                super::ImplicitDropDependencySourceEvent::FreeFunction {
                                    token: ordinary_owner,
                                    file: fn_info.file_id.index(),
                                    name: sema.interner.resolve(&source_name).to_string(),
                                },
                            ),
                            air: imported.air,
                            local_atoms: imported.local_atoms,
                            num_locals: imported.num_locals,
                            num_param_slots: imported.num_param_slots,
                            param_modes: imported.param_modes,
                            allow_unreachable_code: imported.allow_unreachable_code,
                        };
                        sema.analyzed_body_owners.push(
                            super::AnalyzedBodyOwnerEvent::FreeFunction {
                                token: ordinary_owner,
                                file: fn_info.file_id.index(),
                                name: sema.interner.resolve(&source_name).to_string(),
                            },
                        );
                        sema.body_analysis_work.ordinary_body_exports_attempted += 1;
                        match sema.export_ordinary_body(
                            ordinary_owner,
                            sema.rir.get(body).span,
                            &analyzed,
                            &imported.strings,
                            &[],
                            &referenced_meths,
                        ) {
                            Ok(export) => {
                                sema.body_analysis_work.ordinary_body_exports_succeeded += 1;
                                sema.body_analysis_work
                                    .ordinary_body_export_instructions_emitted +=
                                    export.body.instructions.len();
                                sema.body_analysis_work.ordinary_body_export_places_emitted +=
                                    export.body.places.len();
                                sema.body_analysis_work.ordinary_body_export_strings_emitted +=
                                    export.body.strings.len();
                                sema.ordinary_body_exports.push(export);
                            }
                            Err(reason) => {
                                sema.body_analysis_work.ordinary_body_exports_rejected += 1;
                                sema.body_analysis_work.last_ordinary_body_export_failure =
                                    Some(reason);
                            }
                        }
                        functions_with_strings.push((analyzed, imported.strings));
                        enqueue_references_sorted(
                            sema.interner,
                            referenced_fns,
                            referenced_meths,
                            &analyzed_functions,
                            &analyzed_methods,
                            &mut pending_functions,
                            &mut pending_methods,
                        );
                        continue;
                    } else {
                        sema.body_analysis_work.ordinary_body_import_failures += 1;
                        sema.body_analysis_work.last_ordinary_body_import_failure = import_failure;
                        sema.body_analysis_work.ordinary_body_import_atomic_discards += 1;
                    }
                }

                sema.body_analysis_work.bodies_attempted += 1;
                let previous_type_observer = sema.declaration_type_observer.replace((
                    fn_info.file_id,
                    sema.interner.resolve(&source_name).to_string(),
                    None,
                    super::DeclarationTypeDependencySourceKind::Function,
                    super::DeclarationTypeDependencyKind::Body,
                ));
                let previous_body_observer = sema.body_dependency_observer.replace(
                    super::AnalyzedBodyOwnerEvent::FreeFunction {
                        token: sema.body_owner_token(
                            fn_info.file_id,
                            sema.interner.resolve(&source_name),
                            None,
                            super::BodyOwnerKind::FreeFunction,
                        ),
                        file: fn_info.file_id.index(),
                        name: sema.interner.resolve(&source_name).to_string(),
                    },
                );
                let analysis = sema.analyze_single_function(
                    &infer_ctx,
                    &fn_name_str,
                    return_type,
                    params.values(),
                    body,
                    span,
                    fn_info.allow_unused_variable,
                    fn_info.allow_unreachable_code,
                );
                sema.declaration_type_observer = previous_type_observer;
                sema.body_dependency_observer = previous_body_observer;
                let ordinary_body_span = sema.rir.get(body).span;
                match analysis {
                    Ok((
                        mut analyzed,
                        warnings,
                        local_strings,
                        referenced_fns,
                        referenced_meths,
                    )) => {
                        sema.body_analysis_work.bodies_succeeded += 1;
                        sema.body_analysis_work.air_instructions_produced +=
                            analyzed.air.instructions().len();
                        sema.body_analysis_work.local_strings_produced += local_strings.len();
                        let ordinary_owner = sema.body_owner_token(
                            fn_info.file_id,
                            sema.interner.resolve(&source_name),
                            None,
                            super::BodyOwnerKind::FreeFunction,
                        );
                        sema.analyzed_body_owners.push(
                            super::AnalyzedBodyOwnerEvent::FreeFunction {
                                token: ordinary_owner,
                                file: fn_info.file_id.index(),
                                name: sema.interner.resolve(&source_name).to_string(),
                            },
                        );
                        analyzed.ordinary_owner = Some(ordinary_owner);
                        sema.body_analysis_work.ordinary_body_exports_attempted += 1;
                        match sema.export_ordinary_body(
                            ordinary_owner,
                            ordinary_body_span,
                            &analyzed,
                            &local_strings,
                            &warnings,
                            &referenced_meths,
                        ) {
                            Ok(export) => {
                                sema.body_analysis_work.ordinary_body_exports_succeeded += 1;
                                sema.body_analysis_work
                                    .ordinary_body_export_instructions_emitted +=
                                    export.body.instructions.len();
                                sema.body_analysis_work.ordinary_body_export_places_emitted +=
                                    export.body.places.len();
                                sema.body_analysis_work.ordinary_body_export_strings_emitted +=
                                    export.body.strings.len();
                                sema.ordinary_body_exports.push(export);
                            }
                            Err(reason) => {
                                sema.body_analysis_work.ordinary_body_exports_rejected += 1;
                                sema.body_analysis_work.last_ordinary_body_export_failure =
                                    Some(reason);
                            }
                        }
                        analyzed.implicit_drop_source =
                            Some(super::ImplicitDropDependencySourceEvent::FreeFunction {
                                token: sema.body_owner_token(
                                    fn_info.file_id,
                                    sema.interner.resolve(&source_name),
                                    None,
                                    super::BodyOwnerKind::FreeFunction,
                                ),
                                file: fn_info.file_id.index(),
                                name: sema.interner.resolve(&source_name).to_string(),
                            });
                        functions_with_strings.push((analyzed, local_strings));
                        all_warnings.extend(warnings);

                        // Add newly referenced functions to the work queue
                        enqueue_references_sorted(
                            sema.interner,
                            referenced_fns,
                            referenced_meths,
                            &analyzed_functions,
                            &analyzed_methods,
                            &mut pending_functions,
                            &mut pending_methods,
                        );
                    }
                    Err(e) => {
                        sema.body_analysis_work.bodies_failed += 1;
                        errors.push(e)
                    }
                }
            }

            // Process pending methods
            while let Some((struct_id, method_name)) = pending_methods.pop() {
                if analyzed_methods.contains(&(struct_id, method_name)) {
                    continue;
                }
                analyzed_methods.insert((struct_id, method_name));

                // Look up the method info
                let method_info = match sema.method_info((struct_id, method_name)).copied() {
                    Some(info) => info,
                    None => continue,
                };

                // Get the struct definition to find its name for impl block lookup
                let struct_def = sema.type_pool.struct_def(struct_id);
                let type_name_str = struct_def.name.to_string();
                let method_name_str = sema.interner.resolve(&method_name).to_string();

                // For anonymous structs, use the MethodInfo directly since there's no named StructDecl.
                // Membership, not the generated-name prefix: a source struct may
                // legally be called `__anon_struct_N` (RUE-1050).
                if sema.anonymous_struct_ids.contains(&struct_id) {
                    sema.body_analysis_work.anonymous_method_record_lookups += 1;
                    let full_name =
                        sema.method_symbol(struct_id, &method_name_str, method_info.has_self);

                    // Build param_info from MethodInfo's ParamRange
                    let param_names = sema.param_arena.names(method_info.params);
                    let param_types = sema.param_arena.types(method_info.params);
                    let param_modes = sema.param_arena.modes(method_info.params);
                    let param_comptime = sema.param_arena.comptime(method_info.params);

                    let mut param_info: Vec<(Spur, Type, RirParamMode, bool)> = Vec::new();

                    if method_info.has_self {
                        // Add self parameter in the receiver's declared mode
                        // (by-value `self`, or by-ref `borrow`/`inout self`; RUE-15).
                        let self_sym = sema.interner.get_or_intern("self");
                        param_info.push((
                            self_sym,
                            method_info.struct_type,
                            method_info.self_mode,
                            false,
                        ));
                    }

                    // Add regular parameters (convert from arena slices)
                    for i in 0..param_names.len() {
                        param_info.push((
                            param_names[i],
                            param_types[i],
                            param_modes[i],
                            param_comptime[i],
                        ));
                    }

                    // Retrieve captured comptime values from struct-level storage
                    // Clone the HashMap to avoid borrowing issues with mutable analyze_method_body call
                    let struct_id = method_info
                        .struct_type
                        .as_struct()
                        .expect("method must belong to struct");
                    let captured_values = sema
                        .anon_struct_captured_values
                        .get(&struct_id)
                        .cloned()
                        .unwrap_or_else(HashMap::new);
                    let enclosing_type_subst = sema
                        .anon_struct_type_subst
                        .get(&struct_id)
                        .cloned()
                        .unwrap_or_else(HashMap::new);

                    // A `drop fn(self)` in an anonymous struct body is carried
                    // under the reserved `__drop` method name (RUE-312). Analyze it
                    // as a destructor in the lazy pipeline too: drop glue adds the
                    // call implicitly, and destructor analysis has different
                    // self-move/drop semantics from an ordinary method.
                    let is_destructor = method_name == drop_marker_sym;
                    let member_kind = if is_destructor {
                        crate::AnonymousMemberKind::Destructor
                    } else if method_info.has_self {
                        crate::AnonymousMemberKind::Method
                    } else {
                        crate::AnonymousMemberKind::AssociatedFunction
                    };
                    let anonymous_identity = match sema.canonical_anonymous_member_producer(
                        method_info.struct_type,
                        method_name,
                        member_kind,
                    ) {
                        Ok(crate::StableProducerId::Function(identity)) => *identity,
                        Ok(crate::StableProducerId::Definition(_)) => unreachable!(),
                        Err(failure) => {
                            errors.push(CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to issue anonymous callable identity for '{full_name}': {failure:?}"
                            )),
                            method_info.span,
                        ));
                            continue;
                        }
                    };
                    let analysis_result = if is_destructor {
                        sema.analyze_anon_destructor_body(
                            &infer_ctx,
                            &param_info,
                            method_info.body,
                            method_info.struct_type,
                            &captured_values,
                            method_name,
                            &full_name,
                            &enclosing_type_subst,
                        )
                    } else {
                        sema.analyze_method_body(
                            &infer_ctx,
                            method_name,
                            method_info.has_self,
                            method_info.return_type,
                            &param_info,
                            method_info.body,
                            method_info.struct_type,
                            &captured_values,
                            &enclosing_type_subst,
                            method_info.self_is_mut,
                        )
                    };

                    sema.body_analysis_work.bodies_attempted += 1;
                    match analysis_result {
                        Ok((
                            air,
                            num_locals,
                            num_param_slots,
                            param_modes_result,
                            warnings,
                            local_strings,
                            local_atoms,
                            referenced_fns,
                            referenced_meths,
                        )) => {
                            sema.body_analysis_work.bodies_succeeded += 1;
                            sema.body_analysis_work.air_instructions_produced +=
                                air.instructions().len();
                            sema.body_analysis_work.local_strings_produced += local_strings.len();
                            sema.analyzed_body_owners
                                .push(super::AnalyzedBodyOwnerEvent::Anonymous);
                            let validated =
                                match crate::ValidatedAir::from_semantic_air_with_symbols(
                                    air,
                                    &sema.type_pool,
                                    sema.interner,
                                ) {
                                    Ok(air) => air,
                                    Err(error) => {
                                        sema.body_analysis_work.bodies_failed += 1;
                                        errors.push(error.into());
                                        continue;
                                    }
                                };
                            let analyzed = AnalyzedFunction {
                                identity: anonymous_identity,
                                callable_kind: if method_name_str == "__drop" {
                                    crate::AnalyzedCallableKind::Destructor
                                } else {
                                    crate::AnalyzedCallableKind::Ordinary
                                },
                                ordinary_owner: None,
                                name: full_name,
                                implicit_drop_source: Some(
                                    super::ImplicitDropDependencySourceEvent::Anonymous,
                                ),
                                air: validated,
                                local_atoms,
                                num_locals,
                                num_param_slots,
                                param_modes: param_modes_result,
                                allow_unreachable_code: false,
                            };
                            functions_with_strings.push((analyzed, local_strings));
                            all_warnings.extend(warnings);

                            enqueue_references_sorted(
                                sema.interner,
                                referenced_fns,
                                referenced_meths,
                                &analyzed_functions,
                                &analyzed_methods,
                                &mut pending_functions,
                                &mut pending_methods,
                            );
                        }
                        Err(e) => {
                            sema.body_analysis_work.bodies_failed += 1;
                            errors.push(e)
                        }
                    }
                    continue;
                }

                sema.body_analysis_work.named_method_record_lookups += 1;
                let owner_name = sema
                    .interner
                    .get(&type_name_str)
                    .expect("named struct definition must retain its interned source name");
                let Some(method_ref) = sema.call_facts().named_method_declaration(
                    struct_def.file_id,
                    owner_name,
                    method_name,
                ) else {
                    debug_assert!(
                        false,
                        "gathered named MethodInfo must have a private declaration record"
                    );
                    continue;
                };
                let method_inst = sema.rir.get(method_ref);
                let InstData::FnDecl {
                    name: m_name,
                    params,
                    return_type,
                    body,
                    has_self,
                    self_mode,
                    self_is_mut,
                    ..
                } = &method_inst.data
                else {
                    unreachable!("named method record pointed at a non-FnDecl");
                };
                debug_assert_eq!(*m_name, method_name);
                debug_assert_eq!(*body, method_info.body);
                debug_assert_eq!(method_inst.span, method_info.span);
                debug_assert_eq!(*has_self, method_info.has_self);
                debug_assert_eq!(*self_mode, method_info.self_mode);
                debug_assert_eq!(*self_is_mut, method_info.self_is_mut);

                let params = sema.rir.params(params);
                let full_name = sema.method_symbol(struct_id, &method_name_str, *has_self);
                let named_method_identity =
                    match sema.function_identity(sema.interner.get_or_intern(&full_name)) {
                        Ok(token) => crate::FunctionInstanceKey::Definition(token),
                        Err(failure) => {
                            errors.push(CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to issue callable identity for '{full_name}': {failure:?}"
                            )),
                            method_info.span,
                        ));
                            continue;
                        }
                    };
                let generic = sema
                    .param_arena
                    .comptime(method_info.params)
                    .iter()
                    .copied()
                    .any(|flag| flag);
                let owner_kind = if *has_self {
                    super::BodyOwnerKind::Method
                } else {
                    super::BodyOwnerKind::AssociatedFunction
                };
                let ordinary_owner = sema.body_owner_token(
                    method_info.span.file_id,
                    &method_name_str,
                    Some(&type_name_str),
                    owner_kind,
                );
                if !generic
                    && let Some(candidate) = sema.reusable_ordinary_bodies.remove(&ordinary_owner)
                {
                    sema.body_analysis_work.ordinary_body_import_attempts += 1;
                    match import_staged_body(sema, &candidate.body, sema.rir.get(*body).span) {
                        Ok(imported) => {
                            all_warnings.extend(imported.warnings.iter().cloned());
                            sema.body_analysis_work.ordinary_body_import_successes += 1;
                            sema.body_analysis_work
                                .ordinary_body_import_instructions_installed += imported.air.len();
                            sema.body_analysis_work
                                .ordinary_body_import_places_installed +=
                                imported.air.places().len();
                            sema.body_analysis_work
                                .ordinary_body_import_strings_installed += imported.strings.len();
                            sema.body_analysis_work.ordinary_bodies_reused += 1;
                            sema.body_analysis_work.ordinary_body_analyses_skipped += 1;
                            let (referenced_fns, referenced_meths) =
                                imported_body_references(sema, &imported);
                            let analyzed = AnalyzedFunction {
                                identity: named_method_identity.clone(),
                                callable_kind: crate::AnalyzedCallableKind::Ordinary,
                                name: full_name,
                                ordinary_owner: Some(ordinary_owner),
                                implicit_drop_source: Some(
                                    super::ImplicitDropDependencySourceEvent::NamedMethod {
                                        token: ordinary_owner,
                                        file: method_info.span.file_id.index(),
                                        owner_name: type_name_str.clone(),
                                        method_name: method_name_str.clone(),
                                    },
                                ),
                                air: imported.air,
                                local_atoms: imported.local_atoms,
                                num_locals: imported.num_locals,
                                num_param_slots: imported.num_param_slots,
                                param_modes: imported.param_modes,
                                allow_unreachable_code: imported.allow_unreachable_code,
                            };
                            sema.analyzed_body_owners.push(
                                super::AnalyzedBodyOwnerEvent::NamedMethod {
                                    token: ordinary_owner,
                                    file: method_info.span.file_id.index(),
                                    owner_name: type_name_str.clone(),
                                    method_name: method_name_str.clone(),
                                    generic: false,
                                },
                            );
                            sema.body_analysis_work.ordinary_body_exports_attempted += 1;
                            match sema.export_ordinary_body(
                                ordinary_owner,
                                sema.rir.get(*body).span,
                                &analyzed,
                                &imported.strings,
                                &[],
                                &referenced_meths,
                            ) {
                                Ok(export) => {
                                    sema.body_analysis_work.ordinary_body_exports_succeeded += 1;
                                    sema.body_analysis_work
                                        .ordinary_body_export_instructions_emitted +=
                                        export.body.instructions.len();
                                    sema.body_analysis_work.ordinary_body_export_places_emitted +=
                                        export.body.places.len();
                                    sema.body_analysis_work.ordinary_body_export_strings_emitted +=
                                        export.body.strings.len();
                                    sema.ordinary_body_exports.push(export);
                                }
                                Err(reason) => {
                                    sema.body_analysis_work.ordinary_body_exports_rejected += 1;
                                    sema.body_analysis_work.last_ordinary_body_export_failure =
                                        Some(reason);
                                }
                            }
                            functions_with_strings.push((analyzed, imported.strings));
                            enqueue_references_sorted(
                                sema.interner,
                                referenced_fns,
                                referenced_meths,
                                &analyzed_functions,
                                &analyzed_methods,
                                &mut pending_functions,
                                &mut pending_methods,
                            );
                            continue;
                        }
                        Err(reason) => {
                            sema.body_analysis_work.ordinary_body_import_failures += 1;
                            sema.body_analysis_work.last_ordinary_body_import_failure =
                                Some(reason.kind());
                            sema.body_analysis_work.ordinary_body_import_atomic_discards += 1;
                        }
                    }
                }
                sema.body_analysis_work.bodies_attempted += 1;
                let previous_type_observer = sema.declaration_type_observer.replace((
                    method_info.span.file_id,
                    method_name_str.clone(),
                    Some(type_name_str.clone()),
                    if *has_self {
                        super::DeclarationTypeDependencySourceKind::Method
                    } else {
                        super::DeclarationTypeDependencySourceKind::AssociatedFunction
                    },
                    super::DeclarationTypeDependencyKind::Body,
                ));
                let previous_body_observer = sema.body_dependency_observer.replace(
                    super::AnalyzedBodyOwnerEvent::NamedMethod {
                        token: sema.body_owner_token(
                            method_info.span.file_id,
                            &method_name_str,
                            Some(&type_name_str),
                            if *has_self {
                                super::BodyOwnerKind::Method
                            } else {
                                super::BodyOwnerKind::AssociatedFunction
                            },
                        ),
                        file: method_info.span.file_id.index(),
                        owner_name: type_name_str.clone(),
                        method_name: method_name_str.clone(),
                        generic,
                    },
                );
                let analysis = sema.analyze_method_function(
                    &infer_ctx,
                    &full_name,
                    *return_type,
                    params.values(),
                    *body,
                    method_inst.span,
                    method_info.struct_type,
                    *has_self,
                    *self_mode,
                    *self_is_mut,
                    method_info.returns_borrow,
                );
                sema.declaration_type_observer = previous_type_observer;
                sema.body_dependency_observer = previous_body_observer;
                match analysis {
                    Ok((
                        mut analyzed,
                        warnings,
                        local_strings,
                        referenced_fns,
                        referenced_meths,
                    )) => {
                        sema.body_analysis_work.bodies_succeeded += 1;
                        sema.body_analysis_work.air_instructions_produced +=
                            analyzed.air.instructions().len();
                        sema.body_analysis_work.local_strings_produced += local_strings.len();
                        sema.analyzed_body_owners.push(
                            super::AnalyzedBodyOwnerEvent::NamedMethod {
                                token: sema.body_owner_token(
                                    method_info.span.file_id,
                                    &method_name_str,
                                    Some(&type_name_str),
                                    if *has_self {
                                        super::BodyOwnerKind::Method
                                    } else {
                                        super::BodyOwnerKind::AssociatedFunction
                                    },
                                ),
                                file: method_info.span.file_id.index(),
                                owner_name: type_name_str.clone(),
                                method_name: method_name_str.clone(),
                                generic,
                            },
                        );
                        analyzed.ordinary_owner = Some(sema.body_owner_token(
                            method_info.span.file_id,
                            &method_name_str,
                            Some(&type_name_str),
                            if *has_self {
                                super::BodyOwnerKind::Method
                            } else {
                                super::BodyOwnerKind::AssociatedFunction
                            },
                        ));
                        if !generic {
                            sema.body_analysis_work.ordinary_body_exports_attempted += 1;
                            match sema.export_ordinary_body(
                                ordinary_owner,
                                sema.rir.get(*body).span,
                                &analyzed,
                                &local_strings,
                                &warnings,
                                &referenced_meths,
                            ) {
                                Ok(export) => {
                                    sema.body_analysis_work.ordinary_body_exports_succeeded += 1;
                                    sema.body_analysis_work
                                        .ordinary_body_export_instructions_emitted +=
                                        export.body.instructions.len();
                                    sema.body_analysis_work.ordinary_body_export_places_emitted +=
                                        export.body.places.len();
                                    sema.body_analysis_work.ordinary_body_export_strings_emitted +=
                                        export.body.strings.len();
                                    sema.ordinary_body_exports.push(export);
                                }
                                Err(reason) => {
                                    sema.body_analysis_work.ordinary_body_exports_rejected += 1;
                                    sema.body_analysis_work.last_ordinary_body_export_failure =
                                        Some(reason);
                                }
                            }
                        }
                        analyzed.implicit_drop_source =
                            Some(super::ImplicitDropDependencySourceEvent::NamedMethod {
                                token: sema.body_owner_token(
                                    method_info.span.file_id,
                                    &method_name_str,
                                    Some(&type_name_str),
                                    if *has_self {
                                        super::BodyOwnerKind::Method
                                    } else {
                                        super::BodyOwnerKind::AssociatedFunction
                                    },
                                ),
                                file: method_info.span.file_id.index(),
                                owner_name: type_name_str.clone(),
                                method_name: method_name_str.clone(),
                            });
                        functions_with_strings.push((analyzed, local_strings));
                        all_warnings.extend(warnings);
                        enqueue_references_sorted(
                            sema.interner,
                            referenced_fns,
                            referenced_meths,
                            &analyzed_functions,
                            &analyzed_methods,
                            &mut pending_functions,
                            &mut pending_methods,
                        );
                    }
                    Err(e) => {
                        sema.body_analysis_work.bodies_failed += 1;
                        errors.push(e)
                    }
                }
            }

            // Anonymous destructors are not referenced by user-written call AIR;
            // drop glue adds those calls later for instantiated anonymous types.
            // Once comptime evaluation has registered such a destructor, enqueue it
            // so lazy analysis emits `__anon_struct_N.__drop` before the backend
            // links drop glue.
            if pending_functions.is_empty() && pending_methods.is_empty() {
                extend_committed_air_aggregate_types(
                    sema,
                    &functions_with_strings[active_function_count..],
                    &mut active_aggregate_types,
                );
                active_function_count = functions_with_strings.len();
                enqueue_anonymous_destructors(
                    sema,
                    drop_marker_sym,
                    &active_aggregate_types,
                    &analyzed_methods,
                    &mut pending_methods,
                );
            }

            if !pending_functions.is_empty() || !pending_methods.is_empty() {
                continue;
            }

            // Named destructors are implicit roots: drop glue can call them without
            // a source-level reference. Analyze each once, then feed any calls made
            // by their bodies back through the same deterministic work queues.
            if !named_destructors_analyzed {
                named_destructors_analyzed = true;
                // Copy the request-local records before mutating `sema` below.
                // The loop finishes selecting every destructor before the queued
                // references are processed on the next fixed-point iteration.
                let destructor_records = sema.declaration_index.destructors().to_vec();
                for destructor in destructor_records {
                    sema.body_analysis_work
                        .named_destructor_declarations_visited += 1;
                    // File-aware first (RUE-558), matching the historical scan.
                    let struct_id = match sema
                        .structs_by_file_name
                        .get(&(destructor.span.file_id, destructor.type_name))
                    {
                        Some(id) => *id,
                        None => continue,
                    };
                    let struct_type = Type::new_struct(struct_id);
                    let full_name = sema.destructor_symbol(struct_id);

                    let owner_name = sema.type_pool.struct_def(struct_id).name.to_string();
                    let named_destructor_identity = match sema.stable_definition_token(
                        destructor.span.file_id.index(),
                        &owner_name,
                        Some(&owner_name),
                        crate::StableDefinitionKind::Destructor,
                    ) {
                        Ok(token) => crate::FunctionInstanceKey::Definition(token),
                        Err(failure) => {
                            errors.push(CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to issue callable identity for '{full_name}': {failure:?}"
                            )),
                            destructor.span,
                        ));
                            continue;
                        }
                    };
                    let ordinary_owner = sema.body_owner_token(
                        destructor.span.file_id,
                        &owner_name,
                        Some(&owner_name),
                        super::BodyOwnerKind::Destructor,
                    );
                    if let Some(candidate) = sema.reusable_ordinary_bodies.remove(&ordinary_owner) {
                        sema.body_analysis_work.ordinary_body_import_attempts += 1;
                        match import_staged_body(
                            sema,
                            &candidate.body,
                            sema.rir.get(destructor.body).span,
                        ) {
                            Ok(imported) => {
                                all_warnings.extend(imported.warnings.iter().cloned());
                                sema.body_analysis_work.ordinary_body_import_successes += 1;
                                sema.body_analysis_work
                                    .ordinary_body_import_instructions_installed +=
                                    imported.air.len();
                                sema.body_analysis_work
                                    .ordinary_body_import_places_installed +=
                                    imported.air.places().len();
                                sema.body_analysis_work
                                    .ordinary_body_import_strings_installed +=
                                    imported.strings.len();
                                sema.body_analysis_work.ordinary_bodies_reused += 1;
                                sema.body_analysis_work.ordinary_body_analyses_skipped += 1;
                                let (referenced_fns, referenced_meths) =
                                    imported_body_references(sema, &imported);
                                let analyzed = AnalyzedFunction {
                                    identity: named_destructor_identity.clone(),
                                    callable_kind: crate::AnalyzedCallableKind::Destructor,
                                    name: full_name,
                                    ordinary_owner: Some(ordinary_owner),
                                    implicit_drop_source: Some(
                                        super::ImplicitDropDependencySourceEvent::NamedDestructor {
                                            token: ordinary_owner,
                                            file: destructor.span.file_id.index(),
                                            owner_name: owner_name.clone(),
                                        },
                                    ),
                                    air: imported.air,
                                    local_atoms: imported.local_atoms,
                                    num_locals: imported.num_locals,
                                    num_param_slots: imported.num_param_slots,
                                    param_modes: imported.param_modes,
                                    allow_unreachable_code: imported.allow_unreachable_code,
                                };
                                sema.analyzed_body_owners.push(
                                    super::AnalyzedBodyOwnerEvent::NamedDestructor {
                                        token: ordinary_owner,
                                        file: destructor.span.file_id.index(),
                                        owner_name: owner_name.clone(),
                                    },
                                );
                                sema.body_analysis_work.ordinary_body_exports_attempted += 1;
                                match sema.export_ordinary_body(
                                    ordinary_owner,
                                    sema.rir.get(destructor.body).span,
                                    &analyzed,
                                    &imported.strings,
                                    &[],
                                    &referenced_meths,
                                ) {
                                    Ok(export) => {
                                        sema.body_analysis_work.ordinary_body_exports_succeeded +=
                                            1;
                                        sema.body_analysis_work
                                            .ordinary_body_export_instructions_emitted +=
                                            export.body.instructions.len();
                                        sema.body_analysis_work
                                            .ordinary_body_export_places_emitted +=
                                            export.body.places.len();
                                        sema.body_analysis_work
                                            .ordinary_body_export_strings_emitted +=
                                            export.body.strings.len();
                                        sema.ordinary_body_exports.push(export);
                                    }
                                    Err(reason) => {
                                        sema.body_analysis_work.ordinary_body_exports_rejected += 1;
                                        sema.body_analysis_work.last_ordinary_body_export_failure =
                                            Some(reason);
                                    }
                                }
                                functions_with_strings.push((analyzed, imported.strings));
                                enqueue_references_sorted(
                                    sema.interner,
                                    referenced_fns,
                                    referenced_meths,
                                    &analyzed_functions,
                                    &analyzed_methods,
                                    &mut pending_functions,
                                    &mut pending_methods,
                                );
                                continue;
                            }
                            Err(reason) => {
                                sema.body_analysis_work.ordinary_body_import_failures += 1;
                                sema.body_analysis_work.last_ordinary_body_import_failure =
                                    Some(reason.kind());
                                sema.body_analysis_work.ordinary_body_import_atomic_discards += 1;
                            }
                        }
                    }

                    sema.body_analysis_work.bodies_attempted += 1;
                    let previous_type_observer = sema.declaration_type_observer.replace((
                        destructor.span.file_id,
                        owner_name.clone(),
                        Some(owner_name.clone()),
                        super::DeclarationTypeDependencySourceKind::Destructor,
                        super::DeclarationTypeDependencyKind::Body,
                    ));
                    let previous_body_observer = sema.body_dependency_observer.replace(
                        super::AnalyzedBodyOwnerEvent::NamedDestructor {
                            token: sema.body_owner_token(
                                destructor.span.file_id,
                                &owner_name,
                                Some(&owner_name),
                                super::BodyOwnerKind::Destructor,
                            ),
                            file: destructor.span.file_id.index(),
                            owner_name: sema.type_pool.struct_def(struct_id).name.to_string(),
                        },
                    );
                    let analysis = sema.analyze_destructor_function(
                        &infer_ctx,
                        &full_name,
                        destructor.body,
                        destructor.span,
                        struct_type,
                    );
                    sema.declaration_type_observer = previous_type_observer;
                    sema.body_dependency_observer = previous_body_observer;
                    match analysis {
                        Ok((
                            mut analyzed,
                            warnings,
                            local_strings,
                            referenced_fns,
                            referenced_meths,
                        )) => {
                            sema.body_analysis_work.bodies_succeeded += 1;
                            sema.body_analysis_work.air_instructions_produced +=
                                analyzed.air.instructions().len();
                            sema.body_analysis_work.local_strings_produced += local_strings.len();
                            sema.analyzed_body_owners.push(
                                super::AnalyzedBodyOwnerEvent::NamedDestructor {
                                    token: sema.body_owner_token(
                                        destructor.span.file_id,
                                        &sema.type_pool.struct_def(struct_id).name,
                                        Some(&sema.type_pool.struct_def(struct_id).name),
                                        super::BodyOwnerKind::Destructor,
                                    ),
                                    file: destructor.span.file_id.index(),
                                    owner_name: sema
                                        .type_pool
                                        .struct_def(struct_id)
                                        .name
                                        .to_string(),
                                },
                            );
                            analyzed.ordinary_owner = Some(sema.body_owner_token(
                                destructor.span.file_id,
                                &sema.type_pool.struct_def(struct_id).name,
                                Some(&sema.type_pool.struct_def(struct_id).name),
                                super::BodyOwnerKind::Destructor,
                            ));
                            sema.body_analysis_work.ordinary_body_exports_attempted += 1;
                            match sema.export_ordinary_body(
                                ordinary_owner,
                                sema.rir.get(destructor.body).span,
                                &analyzed,
                                &local_strings,
                                &warnings,
                                &referenced_meths,
                            ) {
                                Ok(export) => {
                                    sema.body_analysis_work.ordinary_body_exports_succeeded += 1;
                                    sema.body_analysis_work
                                        .ordinary_body_export_instructions_emitted +=
                                        export.body.instructions.len();
                                    sema.body_analysis_work.ordinary_body_export_places_emitted +=
                                        export.body.places.len();
                                    sema.body_analysis_work.ordinary_body_export_strings_emitted +=
                                        export.body.strings.len();
                                    sema.ordinary_body_exports.push(export);
                                }
                                Err(reason) => {
                                    sema.body_analysis_work.ordinary_body_exports_rejected += 1;
                                    sema.body_analysis_work.last_ordinary_body_export_failure =
                                        Some(reason);
                                }
                            }
                            analyzed.implicit_drop_source =
                                Some(super::ImplicitDropDependencySourceEvent::NamedDestructor {
                                    token: sema.body_owner_token(
                                        destructor.span.file_id,
                                        &sema.type_pool.struct_def(struct_id).name,
                                        Some(&sema.type_pool.struct_def(struct_id).name),
                                        super::BodyOwnerKind::Destructor,
                                    ),
                                    file: destructor.span.file_id.index(),
                                    owner_name: sema
                                        .type_pool
                                        .struct_def(struct_id)
                                        .name
                                        .to_string(),
                                });
                            functions_with_strings.push((analyzed, local_strings));
                            all_warnings.extend(warnings);
                            enqueue_references_sorted(
                                sema.interner,
                                referenced_fns,
                                referenced_meths,
                                &analyzed_functions,
                                &analyzed_methods,
                                &mut pending_functions,
                                &mut pending_methods,
                            );
                        }
                        Err(e) => {
                            sema.body_analysis_work.bodies_failed += 1;
                            errors.push(e)
                        }
                    }
                }
                continue;
            }

            // Specialized generic bodies are part of the same reachability fixed
            // point as source-level bodies. Feed every ordinary function or method
            // they call back through the deterministic analysis queues, then scan
            // any later source bodies for further specialization requests.
            let specialization_failed = match specializer.run_to_fixpoint(
                &mut functions_with_strings,
                &mut all_warnings,
                sema,
                &infer_ctx,
                sema.interner,
            ) {
                Ok(discovered) => {
                    enqueue_references_sorted(
                        sema.interner,
                        discovered.functions,
                        discovered.methods,
                        &analyzed_functions,
                        &analyzed_methods,
                        &mut pending_functions,
                        &mut pending_methods,
                    );
                    false
                }
                Err(error) => {
                    errors.push(error);
                    true
                }
            };

            if specialization_failed {
                break;
            }

            // Comptime evaluation inside a specialization may register a new
            // anonymous destructor without an explicit call in its AIR. Keep it as
            // an implicit root, just like anonymous destructors registered by an
            // ordinary source body above.
            if pending_functions.is_empty() && pending_methods.is_empty() {
                extend_committed_air_aggregate_types(
                    sema,
                    &functions_with_strings[active_function_count..],
                    &mut active_aggregate_types,
                );
                active_function_count = functions_with_strings.len();
                enqueue_anonymous_destructors(
                    sema,
                    drop_marker_sym,
                    &active_aggregate_types,
                    &analyzed_methods,
                    &mut pending_methods,
                );
            }

            if !pending_functions.is_empty() || !pending_methods.is_empty() {
                continue;
            }

            break;
        }

        // Finish the invalid attempt before restarting so one traversal can
        // observe every lower representative discovered by a large program.
        // Its diagnostics and reachability remain transaction-local, while a
        // restart per individual discovery would make stabilization quadratic.
        finalize_function_body_analysis(
            sema,
            functions_with_strings,
            &active_aggregate_types,
            all_warnings,
            &analyzed_functions,
            errors,
        )
    }
}

/// Reject moving `self` out of a destructor body (RUE-139).
///
/// Dropping a value runs its destructor and then the drop glue; if the
/// destructor moves `self` to a new owner (`consume(self)`, `let x = self`,
/// a by-value method call, ...), that owner drops the value again at ITS
/// scope exit — re-entering the destructor in infinite recursion. This is
/// the spirit of Rust's E0509 (cannot move out of a type implementing Drop).
///
/// Detection: sema wraps every surviving whole-value move of a pass-by-value
/// parameter in an [`AirInstData::MarkMoved`] marker (uses that turn out to
/// be borrows are cancelled in place and leave no marker). A destructor's
/// only parameter is `self` at ABI slot 0, so any whole-value param marker
/// in the analyzed AIR is a move of `self`. Partial field moves
/// (`place: Some(_)`) are not rejected here: they don't re-enter the
/// destructor (the drop-glue double drop of such a field is a separate,
/// pre-existing issue).
pub(crate) fn reject_self_move_in_destructor(air: &Air, full_name: &str) -> CompileResult<()> {
    for (_, inst) in air.iter() {
        if let AirInstData::MarkMoved {
            slot: 0,
            is_param: true,
            place: None,
            ..
        } = inst.data
        {
            let type_name = full_name.strip_suffix(".__drop").unwrap_or(full_name);
            // Strip the RUE-571 file qualifier (`P$2` -> `P`) for display.
            let type_name = type_name.split('$').next().unwrap_or(type_name);
            return Err(CompileError::new(
                ErrorKind::MoveSelfOutOfDestructor {
                    type_name: type_name.to_string(),
                },
                inst.span,
            )
            .with_label("`self` is moved out here", inst.span));
        }
    }
    Ok(())
}

/// Build the diagnostic for a move out of an `inout` parameter.
///
/// Rule (RUE-127): moving out of an inout parameter is always rejected, even if
/// the parameter is reassigned afterwards — reinitialization-before-exit is not
/// tracked yet. Without this rule, the call would leave the caller's variable
/// moved-from while the caller still considers it live.
pub(crate) fn move_out_of_inout_error(name: &str, span: Span) -> CompileError {
    CompileError::new(
        ErrorKind::MoveOutOfInout {
            variable: name.to_string(),
        },
        span,
    )
    .with_note(
        "an `inout` parameter is a mutable borrow of the caller's variable; \
         moving its value out would leave the caller's variable uninitialized",
    )
    .with_help(
        "moves out of `inout` parameters are rejected even if the parameter is \
         reassigned before returning (reinitialization is not tracked yet)",
    )
}

/// Build the diagnostic for a non-exhaustive `match` (E0600), naming exactly
/// what is missing (RUE-133).
///
/// - enum scrutinee: lists the uncovered variants ("missing variants: Blue, Green")
/// - bool scrutinee: names the uncovered literal pattern(s)
/// - integer scrutinee: suggests the required wildcard arm
pub(crate) fn non_exhaustive_match_error(
    span: Span,
    scrutinee_type: Type,
    enum_def: Option<&crate::types::EnumDef>,
    variant_covered: impl Fn(u32) -> bool,
    bool_true_covered: bool,
    bool_false_covered: bool,
) -> CompileError {
    let err = CompileError::new(ErrorKind::NonExhaustiveMatch, span);
    if scrutinee_type == Type::BOOL {
        let missing = match (bool_true_covered, bool_false_covered) {
            (false, false) => "patterns `true` and `false` are",
            (false, true) => "pattern `true` is",
            (true, false) => "pattern `false` is",
            // Both covered means the match was exhaustive; we only get here
            // because callers check exhaustiveness first.
            (true, true) => return err,
        };
        err.with_help(format!("{missing} not covered"))
    } else if let Some(def) = enum_def {
        let missing: Vec<&str> = def
            .variants
            .iter()
            .enumerate()
            .filter(|(i, _)| !variant_covered(*i as u32))
            .map(|(_, v)| v.as_ref())
            .collect();
        if missing.is_empty() {
            return err;
        }
        err.with_help(format!("missing variants: {}", missing.join(", ")))
    } else {
        err.with_help("integer matches must include a wildcard arm: `_ => ...`")
    }
}

/// Validate that a by-ref (`inout`/`borrow`) call argument is a place — a
/// variable, or a field/index projection chain rooted at one — and return
/// the root variable symbol (RUE-143).
///
/// Codegen passes a by-ref argument by address: place-address formation
/// (frame slot + static field offsets + dynamic index offsets, or a received
/// by-ref pointer minus descending offsets) lives in `rue-codegen`'s shared
/// `byref_args` module. Anything that is not a place (a call result, literal,
/// struct-init expression, arithmetic, ...) has no caller-visible storage to
/// point at and is rejected as a non-lvalue.
fn require_byref_place_arg(rir: &Rir, arg: &RirCallArg) -> CompileResult<Spur> {
    root_variable_of(rir, arg.value).ok_or_else(|| {
        CompileError::new(
            if arg.is_inout() {
                ErrorKind::InoutNonLvalue
            } else {
                ErrorKind::BorrowNonLvalue
            },
            rir.get(arg.value).span,
        )
    })
}

/// Result of the element-wise linear array consumption check (RUE-186); see
/// [`Sema::check_array_elementwise_consumption`].
pub(crate) enum ElementwiseConsumption {
    /// Every element was moved out on every path: the array's must-consume
    /// obligation is satisfied.
    Complete,
    /// No element was ever consumed (or the type is not an array): the
    /// caller reports its usual whole-value diagnostic.
    NotElementwise,
}

/// Intern the move-path segment for a constant array index (RUE-186).
///
/// Element paths reuse the field-path representation: index K becomes the
/// interned decimal string of K. Identifiers can never be all digits, so
/// these segments cannot collide with field names (see
/// [`super::context::FieldPath`]).
pub(crate) fn index_path_segment(interner: &ThreadedRodeo, index: u64) -> Spur {
    interner.get_or_intern(index.to_string())
}

/// True when a move-path segment encodes a constant array index (all-digit
/// interned string; see [`index_path_segment`]).
pub(crate) fn is_index_segment(interner: &ThreadedRodeo, seg: Spur) -> bool {
    let s = interner.resolve(&seg);
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Format a move path for diagnostics: field segments as `.name`, constant
/// array index segments as `[K]` (e.g. `xs[0]`, `o.a`, `o.items[2].name`).
fn format_move_path(interner: &ThreadedRodeo, root_var: Spur, path: &[Spur]) -> String {
    let mut out = interner.resolve(&root_var).to_string();
    for seg in path {
        let s = interner.resolve(seg);
        if is_index_segment(interner, *seg) {
            out.push_str(&format!("[{s}]"));
        } else {
            out.push('.');
            out.push_str(s);
        }
    }
    out
}

/// The standard fix hint appended to every use-after-move (E0205) diagnostic:
/// Rue's mechanism for using a value without consuming it is to pass it by
/// `borrow`, so naming the moved value makes the suggestion copy-pasteable
/// (RUE-19 item 4). `name` is the value as it appears in the message (a bare
/// variable like `b`, or a path like `o.a`).
pub(crate) fn borrow_instead_of_move_help(name: &str) -> String {
    format!("to use `{name}` after the move, pass it by borrow instead: `borrow {name}`")
}

/// Build the use-after-move error for a field access whose path (or one of
/// its ancestor prefixes) was moved at `moved_span`.
pub(crate) fn use_after_move_path_error(
    interner: &lasso::ThreadedRodeo,
    root_var: Spur,
    field_path: &[Spur],
    span: Span,
    moved_span: Span,
) -> CompileError {
    let path_str = format_move_path(interner, root_var, field_path);
    let help = borrow_instead_of_move_help(&path_str);
    CompileError::new(ErrorKind::UseAfterMove(path_str), span)
        .with_label("value moved here", moved_span)
        .with_help(help)
}

/// Build the error for a linear value that goes out of scope without being
/// consumed on every path.
///
/// `consumed_on_some_path` is the span of a consumption that happened on only
/// SOME paths (if any); when present it selects the more precise "not
/// consumed on all paths" diagnostic over the plain "dropped" one.
pub(crate) fn linear_not_consumed_error(
    name: &str,
    decl_span: Span,
    consumed_on_some_path: Option<Span>,
) -> CompileError {
    match consumed_on_some_path {
        Some(consumed_span) => CompileError::new(
            ErrorKind::LinearValueNotConsumedOnAllPaths(name.to_string()),
            decl_span,
        )
        .with_label("consumed here, but not on every path", consumed_span)
        .with_help(
            "a linear value must be consumed on every path; \
             consume it in the other branches too (paths that diverge, \
             e.g. by returning, are exempt)",
        ),
        None => CompileError::new(
            ErrorKind::LinearValueNotConsumed(name.to_string()),
            decl_span,
        ),
    }
}

/// Extract the root variable symbol from an expression, if it refers to a
/// variable. Canonical, pipeline-agnostic implementation; see
/// [`Sema::extract_root_variable`] for the full contract.
pub(crate) fn root_variable_of(rir: &Rir, inst_ref: InstRef) -> Option<Spur> {
    let inst = rir.get(inst_ref);
    match &inst.data {
        InstData::VarRef { name, .. } => Some(*name),
        InstData::FieldGet { base, .. } => root_variable_of(rir, *base),
        InstData::IndexGet { base, .. } => root_variable_of(rir, *base),
        _ => None,
    }
}

pub(crate) fn const_use_anchor_of(
    rir: &Rir,
    inst_ref: InstRef,
) -> Option<rue_rir::RirStructuralAnchor> {
    match &rir.get(inst_ref).data {
        InstData::VarRef { anchor, .. } => anchor.clone(),
        InstData::FieldGet { base, .. } => const_use_anchor_of(rir, *base),
        _ => None,
    }
}

/// Check exclusivity rules for inout and borrow parameters in a call.
///
/// This is the shared implementation behind [`Sema::check_exclusive_access`].
/// It enforces three rules:
/// 1. Inout arguments must be lvalues (a variable, or a field/index
///    projection chain rooted at one — RUE-143). A `borrow` argument may
///    instead be elaborated into a place by argument analysis (RUE-953).
/// 2. Same ROOT variable cannot be passed to multiple inout parameters
///    (prevents aliasing; conservatively, even disjoint fields conflict)
/// 3. Same root variable cannot be passed to both inout and borrow (law of
///    exclusivity)
///
/// The law of exclusivity: either one mutable (inout) access OR any number of
/// immutable (borrow) accesses, never both simultaneously.
fn check_exclusive_access_in<A>(
    rir: &Rir,
    interner: &ThreadedRodeo,
    args: A,
    call_span: Span,
    resolve_borrow_root: &dyn Fn(InstRef) -> Option<Spur>,
) -> CompileResult<()>
where
    A: IntoIterator,
    A::Item: std::ops::Deref<Target = RirCallArg>,
{
    let mut inout_vars: HashSet<Spur> = HashSet::new();
    let mut borrow_vars: HashSet<Spur> = HashSet::new();

    for arg in args {
        let arg = &*arg;
        // A `-> borrow T` accessor call is a place for `borrow` arguments
        // (ADR-0062): it roots at its receiver's root and joins the shared
        // set. `inout` accessor results stay rejected as non-lvalues (the
        // exclusive form is the RUE-1016 phase).
        let maybe_var_symbol = root_variable_of(rir, arg.value).or_else(|| {
            arg.is_borrow()
                .then(|| resolve_borrow_root(arg.value))
                .flatten()
        });

        // Check that inout/borrow arguments are lvalues
        if arg.is_inout() && maybe_var_symbol.is_none() {
            return Err(CompileError::new(
                ErrorKind::InoutNonLvalue,
                rir.get(arg.value).span,
            ));
        }
        // A `borrow` operand that denotes no place is not an error: argument
        // analysis elaborates it into a promoted static or a hidden temporary
        // (spec 6.1:39, RUE-953). It has no root variable, so it takes part in
        // no exclusivity conflict — nothing else can name that storage.

        if let Some(var_symbol) = maybe_var_symbol {
            if arg.is_inout() {
                // Check for duplicate inout access
                if !inout_vars.insert(var_symbol) {
                    let var_name = interner.resolve(&var_symbol).to_string();
                    return Err(CompileError::new(
                        ErrorKind::InoutExclusiveAccess { variable: var_name },
                        call_span,
                    ));
                }
                // Check for borrow/inout conflict
                if borrow_vars.contains(&var_symbol) {
                    let var_name = interner.resolve(&var_symbol).to_string();
                    return Err(CompileError::new(
                        ErrorKind::BorrowInoutConflict { variable: var_name },
                        call_span,
                    ));
                }
            } else if arg.is_borrow() {
                borrow_vars.insert(var_symbol);
                // Check for borrow/inout conflict
                if inout_vars.contains(&var_symbol) {
                    let var_name = interner.resolve(&var_symbol).to_string();
                    return Err(CompileError::new(
                        ErrorKind::BorrowInoutConflict { variable: var_name },
                        call_span,
                    ));
                }
            }
        }
    }
    Ok(())
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Create a type mismatch error with safe type name resolution.
    ///
    /// This helper method safely resolves type names even for anonymous structs
    /// by using the type pool. This prevents panics when rendering error messages
    /// for anonymous struct types that might not be fully registered yet.
    ///
    /// # Arguments
    /// - `expected`: The expected type
    /// - `found`: The actual type found
    /// - `span`: The source location of the mismatch
    ///
    /// # Returns
    /// A CompileError with properly formatted type names
    #[inline]
    pub(crate) fn type_mismatch_error(
        &self,
        expected: Type,
        found: Type,
        span: Span,
    ) -> CompileError {
        CompileError::new(
            ErrorKind::TypeMismatch {
                expected: expected.safe_name_with_pool(Some(self.body_type_pool())),
                found: found.safe_name_with_pool(Some(self.body_type_pool())),
            },
            span,
        )
    }
}

mod anon_methods;
mod builtin_ops;
mod calls;
mod functions;
mod instructions;
mod intrinsics;
mod ownership;
pub(crate) use ownership::{AccessorEscapeSite, CallOperands, FirstClassStrSite};
mod pointers;
mod type_inference;

#[cfg(test)]
mod named_const_dependency_filter_tests {
    use super::*;

    fn dependency(
        source_file: u32,
        source_name: String,
    ) -> super::super::NamedConstDependencyEvent {
        super::super::NamedConstDependencyEvent {
            source_file,
            target: super::super::NamedConstDependencyTargetEvent::FreeFunction {
                file: source_file,
                name: format!("target_{source_name}"),
            },
            source_name,
        }
    }

    #[test]
    fn constant_heavy_dependency_filter_is_linear_and_preserves_order() {
        const CONSTANTS: usize = 4_096;
        const FILES: u32 = 16;
        let bound_constants = (0..CONSTANTS)
            .map(|index| (index as u32 % FILES, format!("CONST_{index}")))
            .collect::<Vec<_>>();
        let mut dependencies = Vec::with_capacity(CONSTANTS * 3);
        let mut expected = Vec::with_capacity(CONSTANTS * 2);
        for (index, (file, name)) in bound_constants.iter().enumerate() {
            dependencies.push(dependency(*file, format!("MISSING_{index}")));
            let kept = dependency(*file, name.clone());
            dependencies.push(kept.clone());
            dependencies.push(kept.clone());
            expected.push(kept.clone());
            expected.push(kept);
        }
        let event_count = dependencies.len();
        let mut constants_indexed = 0;
        let mut events_examined = 0;

        let bound_constants =
            index_bound_named_constants_with_observer(bound_constants, || constants_indexed += 1);
        retain_bound_named_const_dependencies_with_observer(
            &mut dependencies,
            &bound_constants,
            || events_examined += 1,
        );

        assert_eq!(constants_indexed, CONSTANTS);
        assert_eq!(events_examined, event_count);
        assert_eq!(constants_indexed + events_examined, CONSTANTS + event_count);
        assert_eq!(
            dependencies, expected,
            "retain preserves source event order"
        );

        dependencies.sort();
        dependencies.dedup();
        expected.sort();
        expected.dedup();
        assert_eq!(dependencies, expected, "sort/dedup output remains stable");
        assert_eq!(dependencies.len(), CONSTANTS);
    }
}

#[cfg(test)]
mod error_invariant_tests {
    use super::*;
    use crate::inst::Air;
    use crate::intern_pool::TypeInternPool;

    fn output_with(func: AnalyzedFunction) -> SemaOutput {
        SemaOutput {
            functions: vec![func],
            strings: Vec::new(),
            warnings: Vec::new(),
            anonymous_nominal_identities_by_type: HashMap::new(),
            aggregate_type_identities_by_type: HashMap::new(),
            aggregate_types_by_identity: HashMap::new(),
            type_pool: TypeInternPool::new().freeze(),
            body_analysis_work: crate::BodyAnalysisWork::default(),
            ordinary_body_exports: Vec::new(),
            specialized_body_exports: Vec::new(),
            analyzed_body_owners: Vec::new(),
            body_named_dependencies: Vec::new(),
            declaration_type_dependencies: Vec::new(),
            declaration_type_dependencies_complete: false,
            declaration_type_call_head_dependencies: Vec::new(),
            declaration_type_call_head_dependencies_complete: false,
            declaration_builtin_type_call_head_dependencies: Vec::new(),
            supported_type_call_heads_complete: false,
            named_const_dependencies: Vec::new(),
            named_value_const_dependencies_complete: false,
        }
    }

    fn func_named(name: &str, air: Air) -> AnalyzedFunction {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        AnalyzedFunction {
            identity: crate::FunctionInstanceKey::Definition(crate::SemanticDefinitionToken::new(
                0, 0,
            )),
            callable_kind: crate::AnalyzedCallableKind::Ordinary,
            name: name.to_string(),
            ordinary_owner: None,
            implicit_drop_source: None,
            air: crate::ValidatedAir::from_semantic_air_with_symbols(air, &pool, &interner)
                .expect("test AIR must validate"),
            local_atoms: Vec::new(),
            num_locals: 0,
            num_param_slots: 0,
            param_modes: ParamSlotModes::default(),
            allow_unreachable_code: false,
        }
    }

    /// A well-typed function must not trip the sema→CFG error invariant.
    #[test]
    fn no_error_type_is_clean() {
        let mut air = Air::new(Type::I32);
        air.add_inst(AirInst {
            data: AirInstData::Const(0),
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        let output = output_with(func_named("main", air));
        assert!(find_undiagnosed_error_type(&output).is_none());
    }

    /// An `<error>`-typed instruction on the success path is a compiler bug and
    /// must be reported as an internal error (RUE-153).
    #[test]
    fn error_typed_instruction_is_caught() {
        let mut air = Air::new(Type::I32);
        air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::ERROR,
            span: Span::new(0, 0),
        });
        let output = output_with(func_named("main", air));
        let err = find_undiagnosed_error_type(&output).expect("error type must be caught");
        assert!(matches!(err.kind, ErrorKind::InternalError(_)));
    }

    /// An `<error>` return type is likewise a bug and must be caught.
    #[test]
    fn error_return_type_is_caught() {
        let air = Air::new(Type::ERROR);
        let output = output_with(func_named("f", air));
        let err = find_undiagnosed_error_type(&output).expect("error return type must be caught");
        assert!(matches!(err.kind, ErrorKind::InternalError(_)));
    }
}
