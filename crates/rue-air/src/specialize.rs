//! Generic function specialization pass.
//!
//! This module provides the specialization pass that transforms `CallGeneric`
//! instructions into regular `Call` instructions by:
//!
//! 1. Collecting all `CallGeneric` instructions in the analyzed functions
//! 2. For each unique (func_name, type_args, value_args) combination, creating
//!    a specialized function
//! 3. Rewriting `CallGeneric` to `Call` with the specialized function name
//!
//! Specialization covers both comptime *type* parameters (`comptime T: type`)
//! and comptime *value* parameters (`comptime n: i32`, RUE-166): each distinct
//! combination of type and value arguments gets its own specialized body, in
//! which the value parameters are compile-time constants (available to
//! `comptime` blocks and forwardable to other comptime parameters).
//!
//! Specialized bodies can contain further `CallGeneric` instructions (a generic
//! function forwarding its type parameter to another generic, or a
//! comptime-recursive function like `fact(comptime n)` calling `fact(n - 1)`),
//! so these steps repeat until no new specializations are discovered.
//!
//! # Architecture
//!
//! The provider body host drives specialization one body at a time:
//! [`select_provider_body_specializations`] rewrites an analyzed body's
//! `CallGeneric` instructions and reports the demanded specialization keys,
//! and [`analyze_one_specialization_with_host`] analyzes exactly one demanded
//! specialized body. The compiler's query graph owns the cross-body fixed
//! point and its expansion budget ([`MAX_SPECIALIZATION_ROUNDS`]).

use ahash::{AHashMap, AHashSet};
use std::collections::hash_map::Entry;

use lasso::{Key, Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind};
use rue_span::Span;

use crate::inst::{Air, AirInstData};
use crate::sema::{
    AnalyzedFunction, ConstValue, InferenceContext, OrdinaryBodyAnalysisHost, OrdinaryBodyEngine,
    SemanticBodyExportHost,
};
use crate::types::{StructId, Type};

/// A key for a specialized function:
/// (base_function_name, type_arguments, value_arguments).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpecializationKey {
    /// Base function name (e.g., "identity")
    pub base_name: Spur,
    /// Type arguments (e.g., [Type::I32]), one per comptime type parameter
    pub type_args: Vec<Type>,
    /// Comptime value arguments (e.g., [Integer(5)]), one per comptime value
    /// parameter (RUE-166)
    pub value_args: Vec<ConstValue>,
}

/// Info about a specialization: the mangled name and the first call site span.
struct SpecializationInfo {
    /// The mangled name for the specialized function.
    mangled_name: Spur,
    /// The span of the first call site (for error reporting if the function doesn't exist).
    call_site_span: Span,
}

/// Maximum number of specialization rounds before giving up.
///
/// Each round creates the bodies for the specializations discovered in the
/// previous round. The budget persists when specialization temporarily yields
/// to sema's ordinary function/method worklists: otherwise a chain that
/// alternates between a specialization and a newly registered method can reset
/// the counter forever (RUE-580).
///
/// This is deliberately a conservative, program-wide expansion budget rather
/// than exact causal-depth tracking. Independent branches can share the budget,
/// but that tradeoff keeps every form of generic/comptime expansion bounded —
/// including comptime-value recursion (`fact(comptime n)` calling
/// `fact(n - 1)`, RUE-166) and ever-growing type instantiation such as
/// `f(Pair(T), ...)` inside `f` — without threading provenance through every
/// sema work queue.
pub const MAX_SPECIALIZATION_ROUNDS: usize = 64;

fn collect_specializations(
    air: &Air,
    interner: &ThreadedRodeo,
    specializations: &mut AHashMap<SpecializationKey, SpecializationInfo>,
    pending: &mut Vec<SpecializationKey>,
    work: &mut crate::BodyAnalysisWork,
) -> bool {
    let mut has_call_generic = false;
    for inst in air.instructions() {
        work.specialization_air_instructions_scanned += 1;
        if let AirInstData::CallGeneric {
            name,
            type_args,
            value_args,
            ..
        } = &inst.data
        {
            has_call_generic = true;
            work.generic_calls_observed += 1;
            let type_args: Vec<Type> = air.get_type_args(type_args).collect();
            let value_args = air.get_const_values(value_args).collect();

            let key = SpecializationKey {
                base_name: *name,
                type_args,
                value_args,
            };

            if let Entry::Vacant(entry) = specializations.entry(key) {
                work.specialization_requests_unique += 1;
                let base_name = interner.resolve(name);
                let mangled = mangle_specialized_name(
                    base_name,
                    &entry.key().type_args,
                    &entry.key().value_args,
                );
                let mangled_sym = interner.get_or_intern(&mangled);
                pending.push(entry.key().clone());
                entry.insert(SpecializationInfo {
                    mangled_name: mangled_sym,
                    call_site_span: inst.span,
                });
            } else {
                work.specialization_requests_duplicate += 1;
            }
        }
    }
    has_call_generic
}

fn rewrite_call_generic(
    air: &mut crate::AirEditor,
    specializations: &AHashMap<SpecializationKey, SpecializationInfo>,
    work: &mut crate::BodyAnalysisWork,
) {
    let mut rewrites: Vec<(usize, Spur)> = Vec::new();

    for (i, inst) in air.instructions().iter().enumerate() {
        if let AirInstData::CallGeneric {
            name,
            type_args,
            value_args,
            args: _,
        } = &inst.data
        {
            let type_args: Vec<Type> = air.get_type_args(type_args).collect();
            let value_args = air.get_const_values(value_args).collect();

            let key = SpecializationKey {
                base_name: *name,
                type_args,
                value_args,
            };

            if let Some(info) = specializations.get(&key) {
                rewrites.push((i, info.mangled_name));
            }
        }
    }

    for (index, name) in rewrites {
        work.specialization_rewrites += 1;
        air.rewrite_generic_call_to_call(index, name);
    }
}

fn host_stable_identity<H>(
    key: &SpecializationKey,
    host: &H,
) -> Result<
    crate::SemanticSpecializationIdentity<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >,
    crate::SemanticBodyExportFailure,
>
where
    H: OrdinaryBodyAnalysisHost + SemanticBodyExportHost,
{
    let base = host.function_identity(key.base_name)?;
    let type_arguments = key
        .type_args
        .iter()
        .map(|value| host.export_body_type(*value))
        .collect::<Result<Vec<_>, _>>()?;
    let value_arguments = key
        .value_args
        .iter()
        .map(|value| {
            Ok(match value {
                ConstValue::Integer(value) => crate::SemanticImportConstValue::Integer(*value),
                ConstValue::Bool(value) => crate::SemanticImportConstValue::Bool(*value),
                ConstValue::Type(value) => {
                    crate::SemanticImportConstValue::Type(host.export_body_type(*value)?)
                }
                ConstValue::Function(value) => {
                    crate::SemanticImportConstValue::Function(host.function_identity(*value)?)
                }
                ConstValue::Unit => crate::SemanticImportConstValue::Unit,
                ConstValue::String(content) => crate::SemanticImportConstValue::String(
                    std::sync::Arc::from(host.resolve_publication_symbol(content)),
                ),
            })
        })
        .collect::<Result<Vec<_>, crate::SemanticBodyExportFailure>>()?;
    Ok(crate::SemanticSpecializationIdentity {
        base,
        type_arguments: type_arguments.into(),
        value_arguments: value_arguments.into(),
    })
}

pub(crate) fn select_provider_body_specializations<H>(
    host: &mut H,
    mut function: AnalyzedFunction,
) -> CompileResult<(
    AnalyzedFunction,
    Vec<(
        Spur,
        crate::SemanticSpecializationIdentity<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    )>,
    Vec<crate::FunctionInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
)>
where
    H: OrdinaryBodyAnalysisHost + SemanticBodyExportHost,
{
    let mut specializations = AHashMap::new();
    let mut pending = Vec::new();
    let mut work = crate::BodyAnalysisWork::default();
    if collect_specializations(
        &function.air,
        host.body_interner(),
        &mut specializations,
        &mut pending,
        &mut work,
    ) {
        let mut editor = function.air.into_editor();
        rewrite_call_generic(&mut editor, &specializations, &mut work);
        function.air = editor.finish(crate::AirValidationContext::SemanticWithSymbols(
            host.body_type_pool(),
            host.body_interner(),
        ))?;
    }
    let mut selected = pending
        .iter()
        .map(|key| {
            let info = &specializations[key];
            let identity = host_stable_identity(key, host).map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to select a canonical specialization identity: {failure:?}"
                    )),
                    info.call_site_span,
                )
            })?;
            let base = crate::FunctionInstanceKey::Definition(
                host.function_identity(key.base_name).map_err(|failure| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "failed to select a canonical specialization base: {failure:?}"
                        )),
                        info.call_site_span,
                    )
                })?,
            );
            let arguments = crate::CanonicalArguments {
                types: key
                    .type_args
                    .iter()
                    .map(|ty| host.canonical_type_instance(*ty))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|failure| {
                        CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to select canonical type arguments: {failure:?}"
                            )),
                            info.call_site_span,
                        )
                    })?
                    .into(),
                values: key
                    .value_args
                    .iter()
                    .copied()
                    .map(|value| host.canonical_argument_value(value))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|failure| {
                        CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to select canonical value arguments: {failure:?}"
                            )),
                            info.call_site_span,
                        )
                    })?
                    .into(),
            };
            let instance = crate::FunctionInstanceKey::Specialization {
                base: Box::new(base),
                arguments,
            };
            Ok((info.mangled_name, identity, instance))
        })
        .collect::<CompileResult<Vec<_>>>()?;
    selected.sort_by(|left, right| left.1.cmp(&right.1));
    selected.dedup_by(|left, right| left.1 == right.1);
    let instances = selected
        .iter()
        .map(|(_, _, instance)| instance.clone())
        .collect();
    let identities = selected
        .into_iter()
        .map(|(name, identity, _)| (name, identity))
        .collect();
    Ok((function, identities, instances))
}

pub(crate) struct OneSpecializedBody {
    pub(crate) function: AnalyzedFunction,
    pub(crate) warnings: Vec<CompileWarning>,
    pub(crate) local_strings: Vec<String>,
    pub(crate) referenced_functions: AHashSet<Spur>,
    pub(crate) referenced_methods: AHashSet<(StructId, Spur)>,
    pub(crate) dependencies: Vec<crate::SemanticDefinitionToken>,
    pub(crate) dependency_boundary_complete: bool,
    pub(crate) identity: crate::SemanticSpecializationIdentity<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >,
}

/// Run the substitution-aware ordinary expression engine for one provider
/// specialization without consulting or mutating a declaration epoch.
pub(crate) fn analyze_one_specialization_with_host<H>(
    host: &mut H,
    infer_ctx: &InferenceContext,
    key: SpecializationKey,
) -> CompileResult<OneSpecializedBody>
where
    H: OrdinaryBodyAnalysisHost + SemanticBodyExportHost,
{
    let base_info = host.function_body_info(key.base_name).ok_or_else(|| {
        CompileError::without_span(ErrorKind::UndefinedFunction(
            host.body_interner().resolve(&key.base_name).to_owned(),
        ))
    })?;
    let base_name = host.body_interner().resolve(&key.base_name).to_owned();
    let specialized_name_text =
        mangle_specialized_name(&base_name, &key.type_args, &key.value_args);
    let base_call_info = crate::sema::FunctionCallInfo::from_body(base_info);
    let param_comptime_type = host.comptime_type_param_flags(&base_call_info);
    let base_params = host
        .body_param_data(base_info.params)
        .iter()
        .map(|(name, ty, mode, is_comptime)| (*name, *ty, *mode, *is_comptime))
        .collect::<Vec<_>>();

    let mut type_subst = AHashMap::new();
    let mut value_subst = AHashMap::new();
    let mut type_arg_idx = 0;
    let mut value_arg_idx = 0;
    for (param_index, (name, _, _, is_comptime)) in base_params.iter().enumerate() {
        if !*is_comptime {
            continue;
        }
        if param_comptime_type[param_index] {
            let ty = key.type_args.get(type_arg_idx).copied().ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "specialization is missing type argument {type_arg_idx} for '{}'",
                        host.body_interner().resolve(name)
                    )),
                    base_info.span,
                )
            })?;
            type_subst.insert(*name, ty);
            type_arg_idx += 1;
        } else {
            let value = key.value_args.get(value_arg_idx).copied().ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "specialization is missing value argument {value_arg_idx} for '{}'",
                        host.body_interner().resolve(name)
                    )),
                    base_info.span,
                )
            })?;
            value_subst.insert(*name, value);
            value_arg_idx += 1;
        }
    }
    if type_arg_idx != key.type_args.len() || value_arg_idx != key.value_args.len() {
        return Err(CompileError::new(
            ErrorKind::InternalError(format!(
                "specialization argument streams do not match declaration: consumed \
                 {type_arg_idx} type/{value_arg_idx} value, received {}/{}",
                key.type_args.len(),
                key.value_args.len()
            )),
            base_info.span,
        ));
    }

    let canonical_arguments = crate::CanonicalArguments {
        types: key
            .type_args
            .iter()
            .map(|ty| host.canonical_type_instance(*ty))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue provider specialization arguments: {failure:?}"
                    )),
                    base_info.span,
                )
            })?
            .into(),
        values: key
            .value_args
            .iter()
            .copied()
            .map(|value| host.canonical_argument_value(value))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue provider specialization arguments: {failure:?}"
                    )),
                    base_info.span,
                )
            })?
            .into(),
    };
    let identity = OrdinaryBodyEngine::new(host)
        .canonical_specialization_instance(key.base_name, &key.type_args, &key.value_args)
        .map_err(|failure| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "failed to issue provider specialization identity: {failure:?}"
                )),
                base_info.span,
            )
        })?;
    // The requested callable remains an exact specialization identity even
    // when its argument streams are empty. Anonymous producer identity has a
    // narrower canonical form: an empty specialization is the base function.
    // Normalize at the producer boundary so the body, its produced facts, and
    // final composition all name the same anonymous nominal.
    let producer = (
        crate::StableProducerId::Function(Box::new(
            identity.with_collapsed_empty_specializations().into_owned(),
        )),
        canonical_arguments,
    );
    let previous = host.replace_active_anonymous_producer(Some(producer));
    let analysis = {
        let mut engine = OrdinaryBodyEngine::new(host);
        let return_type = engine.resolve_substituted_return_type(
            &base_call_info,
            &type_subst,
            &value_subst,
            base_info.span,
        )?;
        let mut specialized_params = Vec::with_capacity(base_params.len());
        for (param_index, (name, ty, mode, is_comptime)) in base_params.into_iter().enumerate() {
            if param_comptime_type[param_index] {
                continue;
            }
            let concrete_ty = engine.resolve_substituted_param_type(
                &base_call_info,
                param_index,
                ty,
                &type_subst,
                &value_subst,
                base_info.span,
            )?;
            specialized_params.push((name, concrete_ty, mode, is_comptime));
        }
        engine.analyze_specialized_function(
            infer_ctx,
            return_type,
            &specialized_params,
            base_info.body,
            &type_subst,
            &value_subst,
        )
    };
    host.replace_active_anonymous_producer(previous);
    let (
        air,
        num_locals,
        num_param_slots,
        param_modes,
        warnings,
        local_strings,
        local_atoms,
        referenced_functions,
        referenced_methods,
    ) = analysis?;
    let stable_identity = host_stable_identity(&key, host).map_err(|failure| {
        CompileError::new(
            ErrorKind::InternalError(format!(
                "failed to export provider specialization identity: {failure:?}"
            )),
            base_info.span,
        )
    })?;
    let mut dependencies = referenced_functions
        .iter()
        .filter_map(|symbol| host.function_identity(*symbol).ok())
        .collect::<Vec<_>>();
    dependencies.sort();
    dependencies.dedup();
    let dependency_boundary_complete =
        referenced_methods.is_empty() && dependencies.len() == referenced_functions.len();
    Ok(OneSpecializedBody {
        function: AnalyzedFunction {
            identity,
            callable_kind: crate::AnalyzedCallableKind::Ordinary,
            ordinary_owner: None,
            name: specialized_name_text,
            implicit_drop_source: Some(
                crate::sema::ImplicitDropDependencySourceEvent::Specialization {
                    identity: stable_identity.clone(),
                },
            ),
            air: crate::ValidatedAir::from_semantic_air_with_symbols(
                air,
                host.body_type_pool(),
                host.body_interner(),
            )?,
            local_atoms,
            num_locals,
            num_param_slots,
            param_modes,
            allow_unreachable_code: base_info.allow_unreachable_code,
        },
        warnings,
        local_strings,
        referenced_functions,
        referenced_methods,
        dependencies,
        dependency_boundary_complete,
        identity: stable_identity,
    })
}

/// Separator between mangled segments in a specialized symbol name.
///
/// A `.` is deliberately *illegal* in a Rue identifier (which is
/// `[a-zA-Z_][a-zA-Z0-9_]*`), so a specialized symbol like `identity.i32`
/// lives in a namespace disjoint from every name a user can spell. This is
/// the targeted fix for RUE-41: previously the separator was `__`, so
/// `identity<i32>` mangled to `identity__i32` — a name a user could legally
/// define as a plain function, producing a duplicate-symbol link error for a
/// valid program. Switching to an identifier-illegal separator makes the
/// collision impossible by construction, without a full user-symbol mangling
/// overhaul (that is RUE-178). The full user-symbol mangling scheme (RUE-178)
/// is still future work; this only guarantees specialized names can't clash
/// with user names.
const SPEC_SEP: char = '.';

/// Generate a mangled name for a specialized function.
///
/// Type arguments and value arguments each contribute a [`SPEC_SEP`]-separated
/// segment, so distinct comptime values yield distinct symbols
/// (`fact.v5`, `fact.v4`, ...; RUE-166) exactly like distinct types do
/// (RUE-100's lesson: colliding mangles mean duplicate symbols at link time).
/// The `.` separator is illegal in Rue identifiers, so these names cannot
/// collide with a user-spellable function name (RUE-41).
pub(crate) fn mangle_specialized_name(
    base_name: &str,
    type_args: &[Type],
    value_args: &[ConstValue],
) -> String {
    let mut mangled = base_name.to_string();
    for ty in type_args {
        mangled.push(SPEC_SEP);
        mangled.push_str(&mangle_type(*ty));
    }
    for value in value_args {
        mangled.push(SPEC_SEP);
        mangled.push_str(&mangle_const_value(value));
    }
    mangled
}

/// Mangle a single comptime value argument into a unique symbol fragment.
///
/// Negative integers use an `m` (minus) prefix on the magnitude because `-`
/// is not a safe symbol character (`-3` -> `vm3`).
fn mangle_const_value(value: &ConstValue) -> String {
    match value {
        ConstValue::Integer(n) if *n < 0 => format!("vm{}", n.unsigned_abs()),
        ConstValue::Integer(n) => format!("v{}", n),
        ConstValue::Bool(b) => format!("v{}", b),
        ConstValue::Unit => "vunit".to_string(),
        ConstValue::Type(ty) => format!("v{}", mangle_type(*ty)),
        ConstValue::Function(name) => format!("vfn{}", name.into_usize()),
        // Comptime parameters have no string type, so strings never appear
        // in a specialization key (RUE-957). The interner index still gives
        // a unique, symbol-safe fragment should that ever change.
        ConstValue::String(content) => format!("vstr{}", content.into_usize()),
    }
}

/// Mangle a single type argument into a unique string.
///
/// Scalar types use their source-level name. Aggregate types must encode their
/// identity (the struct/enum/array/pointer ID): `Type::name()` returns a generic
/// placeholder like `"<struct>"` for them, which made every struct instantiation
/// of a generic function collide on a single specialized symbol (RUE-100).
fn mangle_type(ty: Type) -> String {
    use crate::types::TypeKind;
    match ty.kind() {
        TypeKind::Struct(id) => format!("struct{}", id.0),
        TypeKind::Enum(id) => format!("enum{}", id.0),
        TypeKind::Array(id) => format!("array{}", id.0),
        TypeKind::PtrConst(id) => format!("ptr_const{}", id.0),
        TypeKind::PtrMut(id) => format!("ptr_mut{}", id.0),
        TypeKind::Module(id) => format!("module{}", id.0),
        _ => ty.name().to_string(),
    }
}
