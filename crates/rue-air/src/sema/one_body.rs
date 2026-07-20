//! Test-only extraction of an exact-one-body semantic transaction.
//!
//! Production continues to use the established whole-program driver.  This
//! module exists to prove the transaction contract needed by the compiler's
//! future body query without installing a second production authority.
//!
//! Module selections remain exact stable module-binding definition tokens:
//! that is the canonical compiler join key and retains both the local binding
//! and its module payload without guessing a target from a display path. Drop
//! glue is not selected by semantic body analysis. Its named-destructor edges
//! are emitted by CFG construction from the canonical type identities carried
//! by a successful body; a deterministic semantic failure has no CFG artifact,
//! while its type dependencies retain every input that can affect drop facts.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use lasso::Spur;
use rue_error::{CompileError, CompileErrors, ErrorKind};
use rue_rir::InstData;
use rue_span::{FileId, Span};

use super::{AnalyzedBodyOwnerEvent, AnalyzedFunction, BodyOwnerKind, BodySema};
use crate::{
    FunctionInstanceKey, NominalInstanceKey, SemanticBodyExport, SemanticDefinitionToken,
    SemanticModuleToken, SemanticSpecializationIdentity, SemanticSpecializedBodyExport,
    StableDefinitionKind, Type, TypeInstanceKey,
};

#[derive(Debug, Clone)]
pub(crate) enum OneBodyRequest {
    Definition(SemanticDefinitionToken),
    Specialization {
        base: SemanticDefinitionToken,
        type_arguments: Vec<Type>,
        value_arguments: Vec<super::ConstValue>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OneBodyInterruption {
    Canceled,
    EngineAbort,
}

#[derive(Debug)]
pub(crate) enum OneBodyCanonicalArtifact {
    Ordinary(Box<SemanticBodyExport>),
    Specialization(Box<SemanticSpecializedBodyExport>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum OneBodyDependency {
    Callable(FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>),
    Definition(SemanticDefinitionToken),
    Type(TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>),
}

#[derive(Debug)]
pub(crate) enum OneBodyTransactionOutcome {
    Success {
        artifact: OneBodyCanonicalArtifact,
        references: Arc<[OneBodyDependency]>,
    },
    DeterministicFailure {
        errors: CompileErrors,
        references: Arc<[OneBodyDependency]>,
    },
    NonTerminal {
        reason: OneBodyNonTerminalReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OneBodyNonTerminalReason {
    Canceled,
    EngineAbort,
    TypedIncomplete(crate::SemanticBodyExportFailure),
}

impl OneBodyTransactionOutcome {
    pub(crate) fn references(&self) -> Option<&[OneBodyDependency]> {
        match self {
            Self::Success { references, .. } | Self::DeterministicFailure { references, .. } => {
                Some(references)
            }
            Self::NonTerminal { .. } => None,
        }
    }

    pub(crate) fn artifact_instruction_count(&self) -> Option<usize> {
        match self {
            Self::Success {
                artifact: OneBodyCanonicalArtifact::Ordinary(artifact),
                ..
            } => Some(artifact.body.instructions.len()),
            Self::Success {
                artifact: OneBodyCanonicalArtifact::Specialization(artifact),
                ..
            } => Some(artifact.body.instructions.len()),
            Self::DeterministicFailure { .. } | Self::NonTerminal { .. } => None,
        }
    }

    pub(crate) fn non_terminal_reason(&self) -> Option<&OneBodyNonTerminalReason> {
        match self {
            Self::NonTerminal { reason } => Some(reason),
            Self::Success { .. } | Self::DeterministicFailure { .. } => None,
        }
    }
}

fn interruption_outcome(interruption: OneBodyInterruption) -> OneBodyTransactionOutcome {
    OneBodyTransactionOutcome::NonTerminal {
        reason: match interruption {
            OneBodyInterruption::Canceled => OneBodyNonTerminalReason::Canceled,
            OneBodyInterruption::EngineAbort => OneBodyNonTerminalReason::EngineAbort,
        },
    }
}

fn stable_token(
    sema: &BodySema<'_>,
    file: u32,
    name: &str,
    owner: Option<&str>,
    kind: StableDefinitionKind,
) -> Result<SemanticDefinitionToken, crate::SemanticBodyExportFailure> {
    sema.stable_definition_token(file, name, owner, kind)
}

fn target_dependency(
    sema: &BodySema<'_>,
    target: &super::NamedConstDependencyTargetEvent,
) -> Result<OneBodyDependency, crate::SemanticBodyExportFailure> {
    use super::{DeclarationTypeDependencyTargetKind as TK, NamedConstDependencyTargetEvent as T};
    match target {
        T::ValueConst { file, name } => {
            stable_token(sema, *file, name, None, StableDefinitionKind::ValueConst)
                .map(OneBodyDependency::Definition)
        }
        T::FreeFunction { file, name } => {
            stable_token(sema, *file, name, None, StableDefinitionKind::Function)
                .map(|token| OneBodyDependency::Callable(FunctionInstanceKey::Definition(token)))
        }
        T::NamedType { file, name, kind } => {
            let kind = match kind {
                TK::Struct => StableDefinitionKind::Struct,
                TK::Enum => StableDefinitionKind::Enum,
                TK::ValueConst => StableDefinitionKind::ValueConst,
            };
            let token = stable_token(sema, *file, name, None, kind)?;
            if matches!(
                kind,
                StableDefinitionKind::Struct | StableDefinitionKind::Enum
            ) {
                Ok(OneBodyDependency::Type(TypeInstanceKey::Nominal(
                    NominalInstanceKey::Named(token),
                )))
            } else {
                Ok(OneBodyDependency::Definition(token))
            }
        }
        T::ModuleBinding { file, name } => {
            stable_token(sema, *file, name, None, StableDefinitionKind::ModuleBinding)
                .map(OneBodyDependency::Definition)
        }
    }
}

#[derive(Debug)]
enum ReferenceProjectionFailure {
    TypedIncomplete(crate::SemanticBodyExportFailure),
    EngineAbort,
}

impl From<crate::SemanticBodyExportFailure> for ReferenceProjectionFailure {
    fn from(reason: crate::SemanticBodyExportFailure) -> Self {
        Self::TypedIncomplete(reason)
    }
}

fn projection_failure_outcome(failure: ReferenceProjectionFailure) -> OneBodyTransactionOutcome {
    OneBodyTransactionOutcome::NonTerminal {
        reason: match failure {
            ReferenceProjectionFailure::TypedIncomplete(reason) => {
                OneBodyNonTerminalReason::TypedIncomplete(reason)
            }
            ReferenceProjectionFailure::EngineAbort => OneBodyNonTerminalReason::EngineAbort,
        },
    }
}

fn arguments_have_issuer(
    arguments: &crate::CanonicalArguments<SemanticDefinitionToken, SemanticModuleToken>,
    issuer: u64,
) -> bool {
    arguments.types.iter().all(|ty| type_has_issuer(ty, issuer))
        && arguments.values.iter().all(|value| match value {
            crate::CanonicalArgumentValue::Type(ty) => type_has_issuer(ty, issuer),
            crate::CanonicalArgumentValue::Function(function) => {
                function_has_issuer(function, issuer)
            }
            crate::CanonicalArgumentValue::Integer(_)
            | crate::CanonicalArgumentValue::Bool(_)
            | crate::CanonicalArgumentValue::Unit
            | crate::CanonicalArgumentValue::String(_) => true,
        })
}

fn anonymous_nominal_has_issuer(
    nominal: &crate::AnonymousNominalKey<SemanticDefinitionToken, SemanticModuleToken>,
    issuer: u64,
) -> bool {
    let producer = match &nominal.producer {
        crate::StableProducerId::Definition(definition) => definition.issuer() == issuer,
        crate::StableProducerId::Function(function) => function_has_issuer(function, issuer),
    };
    producer && arguments_have_issuer(&nominal.arguments, issuer)
}

fn type_has_issuer(
    ty: &TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    issuer: u64,
) -> bool {
    match ty {
        TypeInstanceKey::Nominal(NominalInstanceKey::Named(definition)) => {
            definition.issuer() == issuer
        }
        TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(nominal)) => {
            anonymous_nominal_has_issuer(nominal, issuer)
        }
        TypeInstanceKey::Array { element, .. }
        | TypeInstanceKey::Slice { element, .. }
        | TypeInstanceKey::PtrConst(element)
        | TypeInstanceKey::PtrMut(element) => type_has_issuer(element, issuer),
        TypeInstanceKey::Module(module) => module.issuer() == issuer,
        TypeInstanceKey::I8
        | TypeInstanceKey::I16
        | TypeInstanceKey::I32
        | TypeInstanceKey::I64
        | TypeInstanceKey::U8
        | TypeInstanceKey::U16
        | TypeInstanceKey::U32
        | TypeInstanceKey::U64
        | TypeInstanceKey::Bool
        | TypeInstanceKey::Unit
        | TypeInstanceKey::Never
        | TypeInstanceKey::ComptimeType
        | TypeInstanceKey::BuiltinNominal { .. }
        | TypeInstanceKey::GenericParameter(_) => true,
    }
}

fn function_has_issuer(
    function: &FunctionInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
    issuer: u64,
) -> bool {
    match function {
        FunctionInstanceKey::Definition(definition) => definition.issuer() == issuer,
        FunctionInstanceKey::Specialization { base, arguments } => {
            function_has_issuer(base, issuer) && arguments_have_issuer(arguments, issuer)
        }
        FunctionInstanceKey::AnonymousMember { owner, .. } => type_has_issuer(owner, issuer),
        FunctionInstanceKey::DropGlue(ty) => type_has_issuer(ty, issuer),
    }
}

fn dependency_has_issuer(dependency: &OneBodyDependency, issuer: u64) -> bool {
    match dependency {
        OneBodyDependency::Callable(function) => function_has_issuer(function, issuer),
        OneBodyDependency::Definition(definition) => definition.issuer() == issuer,
        OneBodyDependency::Type(ty) => type_has_issuer(ty, issuer),
    }
}

fn body_references(
    sema: &BodySema<'_>,
    owner: Option<super::BodyOwnerToken>,
    referenced_functions: &HashSet<Spur>,
    referenced_methods: &HashSet<(crate::StructId, Spur)>,
    specializations: &[(
        Spur,
        SemanticSpecializationIdentity<SemanticDefinitionToken, SemanticModuleToken>,
    )],
) -> Result<Arc<[OneBodyDependency]>, ReferenceProjectionFailure> {
    let mut references = BTreeSet::new();
    for function in referenced_functions {
        let identity = sema.body_function_identity(*function)?;
        references.insert(OneBodyDependency::Callable(identity));
    }
    for (structure, method) in referenced_methods {
        let Some(info) = sema.method_info((*structure, *method)) else {
            return Err(ReferenceProjectionFailure::EngineAbort);
        };
        let symbol = sema.method_symbol(*structure, sema.interner.resolve(method), info.has_self);
        let Some(symbol) = sema.interner.get(&symbol) else {
            return Err(ReferenceProjectionFailure::EngineAbort);
        };
        let identity = sema.body_function_identity(symbol)?;
        references.insert(OneBodyDependency::Callable(identity));
    }
    for (source, identity) in &sema.body_specialization_dependencies {
        if owner.is_some_and(|owner| source.token() != Some(owner)) {
            continue;
        }
        references.insert(OneBodyDependency::Callable(identity.clone()));
    }
    for (source, symbol) in &sema.body_callable_dependencies {
        if owner.is_some_and(|owner| source.token() != Some(owner)) {
            continue;
        }
        let identity = sema.body_function_identity(*symbol)?;
        references.insert(OneBodyDependency::Callable(identity));
    }
    for event in &sema.body_named_dependencies {
        if owner.is_some_and(|owner| event.source.token() != Some(owner)) {
            continue;
        }
        references.insert(target_dependency(sema, &event.target)?);
    }
    for event in &sema.declaration_type_dependencies {
        if event.dependency_kind != super::DeclarationTypeDependencyKind::Body
            || owner.is_some_and(|owner| event.source_token != Some(owner))
        {
            continue;
        }
        let kind = match event.target_kind {
            super::DeclarationTypeDependencyTargetKind::Struct => StableDefinitionKind::Struct,
            super::DeclarationTypeDependencyTargetKind::Enum => StableDefinitionKind::Enum,
            super::DeclarationTypeDependencyTargetKind::ValueConst => {
                StableDefinitionKind::ValueConst
            }
        };
        let token = stable_token(sema, event.target_file, &event.target_name, None, kind)?;
        if matches!(
            kind,
            StableDefinitionKind::Struct | StableDefinitionKind::Enum
        ) {
            references.insert(OneBodyDependency::Type(TypeInstanceKey::Nominal(
                NominalInstanceKey::Named(token),
            )));
        } else {
            references.insert(OneBodyDependency::Definition(token));
        }
    }
    for event in &sema.declaration_type_call_head_dependencies {
        if event.dependency_kind != super::DeclarationTypeDependencyKind::Body
            || owner.is_some_and(|owner| event.source_token != Some(owner))
        {
            continue;
        }
        let token = stable_token(
            sema,
            event.callable_file,
            &event.callable_name,
            None,
            StableDefinitionKind::Function,
        )?;
        references.insert(OneBodyDependency::Callable(
            FunctionInstanceKey::Definition(token),
        ));
    }
    for (_, identity) in specializations {
        references.remove(&OneBodyDependency::Callable(
            FunctionInstanceKey::Definition(identity.base),
        ));
    }
    for (source, identity) in &sema.body_specialization_dependencies {
        if owner.is_some_and(|owner| source.token() != Some(owner)) {
            continue;
        }
        if let FunctionInstanceKey::Specialization { base, .. } = identity {
            references.remove(&OneBodyDependency::Callable((**base).clone()));
        }
    }
    let references: Arc<[OneBodyDependency]> = references.into_iter().collect::<Vec<_>>().into();
    if let Some(owner) = owner
        && references
            .iter()
            .any(|dependency| !dependency_has_issuer(dependency, owner.issuer()))
    {
        return Err(ReferenceProjectionFailure::TypedIncomplete(
            crate::SemanticBodyExportFailure::ForeignStableIdentityIssuer,
        ));
    }
    Ok(references)
}

fn fail(
    sema: &BodySema<'_>,
    owner: Option<super::BodyOwnerToken>,
    error: CompileError,
) -> OneBodyTransactionOutcome {
    if sema.one_body_inference_failure_incomplete {
        return OneBodyTransactionOutcome::NonTerminal {
            reason: OneBodyNonTerminalReason::TypedIncomplete(
                crate::SemanticBodyExportFailure::MissingStableIdentity,
            ),
        };
    }
    if matches!(
        error.kind,
        ErrorKind::InvalidCompilerInput(_)
            | ErrorKind::CompilerProducerInvariant(_)
            | ErrorKind::CompilerResourceLimit(_)
            | ErrorKind::CompilerResourceExhaustion(_)
            | ErrorKind::OutputPublication(_)
            | ErrorKind::InternalError(_)
            | ErrorKind::InternalCodegenError(_)
    ) {
        return OneBodyTransactionOutcome::NonTerminal {
            reason: OneBodyNonTerminalReason::EngineAbort,
        };
    }
    let errors = if sema.one_body_recovered_errors.is_empty() {
        CompileErrors::from(error)
    } else {
        let mut errors = CompileErrors::new();
        for error in &sema.one_body_recovered_errors {
            errors.push(error.clone());
        }
        errors
    };
    match body_references(sema, owner, &HashSet::new(), &HashSet::new(), &[]) {
        Ok(references) => OneBodyTransactionOutcome::DeterministicFailure { errors, references },
        Err(failure) => projection_failure_outcome(failure),
    }
}

fn finalize_ordinary(
    sema: &BodySema<'_>,
    owner: super::BodyOwnerToken,
    body_span: Span,
    function: AnalyzedFunction,
    warnings: Vec<rue_error::CompileWarning>,
    strings: Vec<String>,
    referenced_functions: HashSet<Spur>,
    referenced_methods: HashSet<(crate::StructId, Spur)>,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    let (function, specializations) =
        match crate::specialize::select_one_body_specializations(sema, function) {
            Ok(selected) => selected,
            Err(error) => return fail(sema, Some(owner), error),
        };
    let references = match body_references(
        sema,
        Some(owner),
        &referenced_functions,
        &referenced_methods,
        &specializations,
    ) {
        Ok(references) => references,
        Err(failure) => return projection_failure_outcome(failure),
    };
    if let Some(interruption) = interruption {
        return interruption_outcome(interruption);
    }
    let specialized_calls = specializations.iter().cloned().collect::<HashMap<_, _>>();
    match sema.export_one_body_with_specializations(
        owner,
        body_span,
        &function,
        &strings,
        &warnings,
        &specialized_calls,
    ) {
        Ok(artifact) => OneBodyTransactionOutcome::Success {
            artifact: OneBodyCanonicalArtifact::Ordinary(Box::new(artifact)),
            references,
        },
        Err(reason) => OneBodyTransactionOutcome::NonTerminal {
            reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
        },
    }
}

pub(super) fn analyze_one_body(
    mut sema: BodySema<'_>,
    request: OneBodyRequest,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    // The request control state is authoritative and dominates setup,
    // semantic diagnostics, canonicalization, and publication alike.
    if let Some(interruption) = interruption {
        return interruption_outcome(interruption);
    }
    sema.one_body_error_recovery = true;
    sema.one_body_recovered_errors.clear();
    sema.one_body_inference_failure_incomplete = false;
    if let Err(error) = sema.get_or_create_str_struct(Span::default()) {
        return fail(&sema, None, error);
    }
    let infer_ctx = sema.build_inference_context();
    match request {
        OneBodyRequest::Specialization {
            base,
            type_arguments,
            value_arguments,
        } => {
            let Some(endpoint) = sema.stable_definition_endpoints.get(&base) else {
                return fail(
                    &sema,
                    None,
                    CompileError::without_span(ErrorKind::InvalidCompilerInput(
                        "specialization base has no current stable endpoint".into(),
                    )),
                );
            };
            let Some(source_name) = sema.interner.get(endpoint.name.as_ref()) else {
                return fail(
                    &sema,
                    None,
                    CompileError::without_span(ErrorKind::UndefinedFunction(
                        endpoint.name.to_string(),
                    )),
                );
            };
            let Some(&base_name) = sema
                .functions_by_file_name
                .get(&(FileId::new(endpoint.file), source_name))
            else {
                return fail(
                    &sema,
                    None,
                    CompileError::without_span(ErrorKind::UndefinedFunction(
                        endpoint.name.to_string(),
                    )),
                );
            };
            let key = crate::specialize::SpecializationKey {
                base_name,
                type_args: type_arguments,
                value_args: value_arguments,
            };
            let base_name = key.base_name;
            let base_info = sema.function_info(base_name).cloned();
            let owner = base_info.as_ref().map(|info| {
                sema.body_owner_token(
                    info.file_id,
                    sema.interner.resolve(&sema.source_function_name(base_name)),
                    None,
                    BodyOwnerKind::FreeFunction,
                )
            });
            let specialized =
                match crate::specialize::analyze_one_specialization(&mut sema, &infer_ctx, key) {
                    Ok(body) => body,
                    Err(error) => return fail(&sema, owner, error),
                };
            let (function, nested) = match crate::specialize::select_one_body_specializations(
                &sema,
                specialized.function,
            ) {
                Ok(value) => value,
                Err(error) => return fail(&sema, owner, error),
            };
            let references = match body_references(
                &sema,
                owner,
                &specialized.referenced_functions,
                &specialized.referenced_methods,
                &nested,
            ) {
                Ok(references) => references,
                Err(failure) => return projection_failure_outcome(failure),
            };
            let Some(base_info) = base_info else {
                return fail(
                    &sema,
                    owner,
                    CompileError::without_span(ErrorKind::UndefinedFunction(
                        sema.interner.resolve(&base_name).to_owned(),
                    )),
                );
            };
            let specialized_calls = nested.iter().cloned().collect::<HashMap<_, _>>();
            match sema.export_specialized_body(
                base_name,
                &base_info,
                &specialized.key.type_args,
                &specialized.key.value_args,
                &function,
                &specialized.local_strings,
                &specialized.warnings,
                &specialized_calls,
                &specialized.dependencies,
                specialized.dependency_boundary_complete,
            ) {
                Ok(artifact) if artifact.identity == specialized.identity => {
                    OneBodyTransactionOutcome::Success {
                        artifact: OneBodyCanonicalArtifact::Specialization(Box::new(artifact)),
                        references,
                    }
                }
                Ok(_) => OneBodyTransactionOutcome::NonTerminal {
                    reason: OneBodyNonTerminalReason::TypedIncomplete(
                        crate::SemanticBodyExportFailure::MissingStableIdentity,
                    ),
                },
                Err(reason) => OneBodyTransactionOutcome::NonTerminal {
                    reason: OneBodyNonTerminalReason::TypedIncomplete(reason),
                },
            }
        }
        OneBodyRequest::Definition(token) => {
            analyze_definition(&mut sema, &infer_ctx, token, interruption)
        }
    }
}

fn analyze_definition(
    sema: &mut BodySema<'_>,
    infer_ctx: &super::InferenceContext,
    token: SemanticDefinitionToken,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    let Some(endpoint) = sema.stable_definition_endpoints.get(&token).cloned() else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "one-body request has no current stable endpoint".into(),
            )),
        );
    };
    match endpoint.kind {
        StableDefinitionKind::Function => {
            let Some(source_name) = sema.interner.get(endpoint.name.as_ref()) else {
                return fail(
                    sema,
                    None,
                    CompileError::without_span(ErrorKind::UndefinedFunction(
                        endpoint.name.to_string(),
                    )),
                );
            };
            let Some(&name) = sema
                .functions_by_file_name
                .get(&(FileId::new(endpoint.file), source_name))
            else {
                return fail(
                    sema,
                    None,
                    CompileError::without_span(ErrorKind::UndefinedFunction(
                        endpoint.name.to_string(),
                    )),
                );
            };
            let info = sema.functions[&name];
            if info.is_generic || info.is_extern {
                return OneBodyTransactionOutcome::NonTerminal {
                    reason: OneBodyNonTerminalReason::TypedIncomplete(
                        crate::SemanticBodyExportFailure::UnsupportedGenericCall,
                    ),
                };
            }
            let source = sema.source_function_name(name);
            let Some(declaration) = sema
                .declaration_index
                .first_free_function(source, Some(info.file_id))
            else {
                return fail(
                    sema,
                    None,
                    CompileError::without_span(ErrorKind::UndefinedFunction(
                        endpoint.name.to_string(),
                    )),
                );
            };
            let InstData::FnDecl {
                params,
                return_type,
                body,
                ..
            } = &sema.rir.get(declaration).data
            else {
                unreachable!()
            };
            let owner = sema.body_owner_token(
                info.file_id,
                endpoint.name.as_ref(),
                None,
                BodyOwnerKind::FreeFunction,
            );
            let previous_body =
                sema.body_dependency_observer
                    .replace(AnalyzedBodyOwnerEvent::FreeFunction {
                        token: owner,
                        file: endpoint.file,
                        name: endpoint.name.to_string(),
                    });
            let previous_type = sema.declaration_type_observer.replace((
                info.file_id,
                endpoint.name.to_string(),
                None,
                super::DeclarationTypeDependencySourceKind::Function,
                super::DeclarationTypeDependencyKind::Body,
            ));
            let result = sema.analyze_single_function(
                infer_ctx,
                sema.interner.resolve(&name),
                *return_type,
                sema.rir.params(params).values(),
                *body,
                info.span,
                info.allow_unused_variable,
                info.allow_unreachable_code,
            );
            sema.body_dependency_observer = previous_body;
            sema.declaration_type_observer = previous_type;
            match result {
                Ok((mut function, warnings, strings, functions, methods)) => {
                    function.ordinary_owner = Some(owner);
                    finalize_ordinary(
                        sema,
                        owner,
                        sema.rir.get(*body).span,
                        function,
                        warnings,
                        strings,
                        functions,
                        methods,
                        interruption,
                    )
                }
                Err(error) => fail(sema, Some(owner), error),
            }
        }
        StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction => {
            analyze_named_method(sema, infer_ctx, &endpoint, interruption)
        }
        StableDefinitionKind::Destructor => {
            analyze_named_destructor(sema, infer_ctx, &endpoint, interruption)
        }
        _ => fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "one-body request does not identify a callable body".into(),
            )),
        ),
    }
}

fn analyze_named_method(
    sema: &mut BodySema<'_>,
    infer_ctx: &super::InferenceContext,
    endpoint: &crate::SemanticDefinitionEndpoint,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    let Some(owner_name) = endpoint.owner.as_deref() else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "method endpoint has no owner".into(),
            )),
        );
    };
    let Some(owner_symbol) = sema.interner.get(owner_name) else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "method owner is absent".into(),
            )),
        );
    };
    let Some(&struct_id) = sema
        .structs_by_file_name
        .get(&(FileId::new(endpoint.file), owner_symbol))
    else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "method owner is absent".into(),
            )),
        );
    };
    let Some(method_name) = sema.interner.get(endpoint.name.as_ref()) else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(endpoint.name.to_string())),
        );
    };
    let Some(info) = sema.method_info((struct_id, method_name)).cloned() else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(endpoint.name.to_string())),
        );
    };
    let Some(&declaration) = sema
        .named_method_declarations
        .get(&(struct_id, method_name))
    else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(endpoint.name.to_string())),
        );
    };
    let InstData::FnDecl {
        params,
        return_type,
        body,
        has_self,
        self_mode,
        self_is_mut,
        ..
    } = &sema.rir.get(declaration).data
    else {
        unreachable!()
    };
    let kind = if *has_self {
        BodyOwnerKind::Method
    } else {
        BodyOwnerKind::AssociatedFunction
    };
    let owner = sema.body_owner_token(
        info.span.file_id,
        endpoint.name.as_ref(),
        Some(owner_name),
        kind,
    );
    let generic = sema
        .param_arena
        .comptime(info.params)
        .iter()
        .any(|value| *value);
    let owner_event = AnalyzedBodyOwnerEvent::NamedMethod {
        token: owner,
        file: endpoint.file,
        owner_name: owner_name.to_owned(),
        method_name: endpoint.name.to_string(),
        generic,
    };
    let previous_body = sema.body_dependency_observer.replace(owner_event);
    let previous_type = sema.declaration_type_observer.replace((
        info.span.file_id,
        endpoint.name.to_string(),
        Some(owner_name.to_owned()),
        if *has_self {
            super::DeclarationTypeDependencySourceKind::Method
        } else {
            super::DeclarationTypeDependencySourceKind::AssociatedFunction
        },
        super::DeclarationTypeDependencyKind::Body,
    ));
    let full_name = sema.method_symbol(struct_id, endpoint.name.as_ref(), *has_self);
    let result = sema.analyze_method_function(
        infer_ctx,
        &full_name,
        *return_type,
        sema.rir.params(params).values(),
        *body,
        info.span,
        info.struct_type,
        *has_self,
        *self_mode,
        *self_is_mut,
    );
    sema.body_dependency_observer = previous_body;
    sema.declaration_type_observer = previous_type;
    match result {
        Ok((mut function, warnings, strings, functions, methods)) => {
            function.ordinary_owner = Some(owner);
            finalize_ordinary(
                sema,
                owner,
                sema.rir.get(*body).span,
                function,
                warnings,
                strings,
                functions,
                methods,
                interruption,
            )
        }
        Err(error) => fail(sema, Some(owner), error),
    }
}

fn analyze_named_destructor(
    sema: &mut BodySema<'_>,
    infer_ctx: &super::InferenceContext,
    endpoint: &crate::SemanticDefinitionEndpoint,
    interruption: Option<OneBodyInterruption>,
) -> OneBodyTransactionOutcome {
    let Some(owner_name) = endpoint.owner.as_deref() else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "destructor endpoint has no owner".into(),
            )),
        );
    };
    let Some(owner_symbol) = sema.interner.get(owner_name) else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "destructor owner is absent".into(),
            )),
        );
    };
    let Some(&struct_id) = sema
        .structs_by_file_name
        .get(&(FileId::new(endpoint.file), owner_symbol))
    else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "destructor owner is absent".into(),
            )),
        );
    };
    let Some(record) = sema
        .declaration_index
        .destructors()
        .iter()
        .find(|record| {
            record.span.file_id.index() == endpoint.file && record.type_name == owner_symbol
        })
        .cloned()
    else {
        return fail(
            sema,
            None,
            CompileError::without_span(ErrorKind::UndefinedFunction(format!(
                "{owner_name}.__drop"
            ))),
        );
    };
    let owner = sema.body_owner_token(
        record.span.file_id,
        owner_name,
        Some(owner_name),
        BodyOwnerKind::Destructor,
    );
    let previous_body =
        sema.body_dependency_observer
            .replace(AnalyzedBodyOwnerEvent::NamedDestructor {
                token: owner,
                file: endpoint.file,
                owner_name: owner_name.to_owned(),
            });
    let previous_type = sema.declaration_type_observer.replace((
        record.span.file_id,
        owner_name.to_owned(),
        Some(owner_name.to_owned()),
        super::DeclarationTypeDependencySourceKind::Destructor,
        super::DeclarationTypeDependencyKind::Body,
    ));
    let result = sema.analyze_destructor_function(
        infer_ctx,
        &sema.destructor_symbol(struct_id),
        record.body,
        record.span,
        Type::new_struct(struct_id),
    );
    sema.body_dependency_observer = previous_body;
    sema.declaration_type_observer = previous_type;
    match result {
        Ok((mut function, warnings, strings, functions, methods)) => {
            function.ordinary_owner = Some(owner);
            finalize_ordinary(
                sema,
                owner,
                sema.rir.get(record.body).span,
                function,
                warnings,
                strings,
                functions,
                methods,
                interruption,
            )
        }
        Err(error) => fail(sema, Some(owner), error),
    }
}
