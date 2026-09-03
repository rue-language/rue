//! Durable-comptime adapters for revisioned semantic queries.
//!
//! These adapters translate stable instance/value identities and expose only
//! the exact semantic/import query authorities required by durable comptime.
//! Body-transaction state and mutation remain owned by `transactions`.

use super::super::*;

pub(crate) fn durable_type_from_instance_key(
    value: &crate::TypeInstanceKey,
) -> Option<crate::durable_semantics::DurableType> {
    crate::durable_comptime::durable_type_from_instance_key(value)
}

pub(crate) fn durable_value_from_argument(
    value: &crate::CanonicalArgumentValue,
) -> Option<crate::durable_semantics::DurableConstValue> {
    use crate::CanonicalArgumentValue as V;
    use crate::durable_semantics::DurableConstValue as D;
    Some(match value {
        V::Integer(value) => D::Integer(*value),
        V::Bool(value) => D::Bool(*value),
        V::Type(value) => D::Type(durable_type_from_instance_key(value)?),
        V::Function(value) => {
            let crate::FunctionInstanceKey::Definition(key) = value.as_ref() else {
                return None;
            };
            D::Function(key.clone())
        }
        V::Unit => D::Unit,
        V::String(value) => D::String(value.clone()),
        V::Float(value) => D::Float(value.clone()),
    })
}

pub(in crate::revisioned_query_database) fn comptime_call_for_anonymous_function(
    producer: &crate::semantic_query_nucleus::DeclarationSemanticQueryKey,
    function: &crate::FunctionInstanceKey,
    shell: &crate::declaration_candidate::DeclarationShellFact,
    signature: &crate::semantic_query_nucleus::ResolvedDeclarationSignature,
    exact_type_syntax: &rue_air::DurableCallableTypeSyntax,
) -> Option<crate::semantic_query_nucleus::ComptimeCallQueryKey> {
    // A dependent runtime result also projects to `ComptimeType` until its
    // arguments are known (for example `[i32; N]`). Only a function whose
    // declared result is literally `type` is an anonymous type constructor.
    let Some(rue_rir::RirTypeSyntaxNode::Named(symbol)) =
        exact_type_syntax.syntax.node(exact_type_syntax.result)
    else {
        return None;
    };
    if exact_type_syntax
        .syntax
        .symbol(*symbol)
        .is_none_or(|name| name.as_ref() != "type")
    {
        return None;
    }
    let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
        parameters,
        result: crate::durable_semantics::DurableType::ComptimeType,
        is_extern: false,
        ..
    } = &signature.signature
    else {
        return None;
    };
    let expected = crate::semantic_query_nucleus::direct_identity(shell)?.key;
    let arguments = match function {
        crate::FunctionInstanceKey::Definition(definition) if *definition == expected => {
            crate::CanonicalArguments::default()
        }
        crate::FunctionInstanceKey::Specialization { base, arguments }
            if matches!(
                base.as_ref(),
                crate::FunctionInstanceKey::Definition(definition) if *definition == expected
            ) =>
        {
            arguments.clone()
        }
        _ => return None,
    };
    if shell.parameters.len() != parameters.len()
        || shell
            .parameters
            .iter()
            .any(|parameter| !parameter.is_comptime)
    {
        return None;
    }
    let mut type_arguments = arguments.types.iter();
    let mut value_arguments = arguments.values.iter();
    let mut types = Vec::new();
    let mut values = Vec::new();
    for (header, parameter) in shell.parameters.iter().zip(parameters.iter()) {
        if parameter.ty == crate::durable_semantics::DurableType::ComptimeType
            && let Some(value) = type_arguments.next()
        {
            types.push((header.name.clone(), durable_type_from_instance_key(value)?));
        } else {
            values.push((
                header.name.clone(),
                durable_value_from_argument(value_arguments.next()?)?,
            ));
        }
    }
    if type_arguments.next().is_some() || value_arguments.next().is_some() {
        return None;
    }
    Some(crate::semantic_query_nucleus::ComptimeCallQueryKey {
        declaration: producer.clone(),
        type_arguments: types.into(),
        value_arguments: values.into(),
    })
}

pub(in crate::revisioned_query_database) fn collect_anonymous_nominal_type_dependencies(
    ty: &crate::durable_semantics::DurableType,
    output: &mut BTreeSet<crate::AnonymousNominalKey>,
) {
    use crate::durable_semantics::DurableType as T;
    match ty {
        T::AnonymousNominal(identity) => {
            output.insert(identity.clone());
        }
        T::Array { element, .. }
        | T::Slice { element, .. }
        | T::PtrConst(element)
        | T::PtrMut(element) => collect_anonymous_nominal_type_dependencies(element, output),
        _ => {}
    }
}

pub(in crate::revisioned_query_database) fn collect_anonymous_nominal_value_dependencies(
    value: &crate::durable_semantics::DurableConstValue,
    output: &mut BTreeSet<crate::AnonymousNominalKey>,
) {
    if let crate::durable_semantics::DurableConstValue::Type(ty) = value {
        collect_anonymous_nominal_type_dependencies(ty, output);
    }
}

pub(in crate::revisioned_query_database) fn collect_durable_anonymous_nominal_dependencies(
    nominal: &crate::durable_semantics::DurableAnonymousNominal,
    output: &mut BTreeSet<crate::AnonymousNominalKey>,
) {
    use crate::durable_semantics::{
        DurableAnonymousMethodType as M, DurableAnonymousNominalShape as S,
    };
    for (_, ty) in nominal.type_captures.iter() {
        collect_anonymous_nominal_type_dependencies(ty, output);
    }
    for (_, value) in nominal.value_captures.iter() {
        collect_anonymous_nominal_value_dependencies(value, output);
    }
    match &nominal.shape {
        S::Struct { fields, methods } => {
            for (_, ty) in fields.iter() {
                collect_anonymous_nominal_type_dependencies(ty, output);
            }
            for method in methods.iter() {
                for (ty, _, _) in method.parameters.iter() {
                    if let M::Concrete(ty) = ty {
                        collect_anonymous_nominal_type_dependencies(ty, output);
                    }
                }
                if let M::Concrete(ty) = &method.result {
                    collect_anonymous_nominal_type_dependencies(ty, output);
                }
            }
        }
        S::Enum { variants, .. } => {
            for (_, fields) in variants.iter() {
                for ty in fields.iter() {
                    collect_anonymous_nominal_type_dependencies(ty, output);
                }
            }
        }
    }
}

pub(in crate::revisioned_query_database) fn enqueue_unselected_anonymous_dependencies(
    selected: &BTreeMap<
        crate::AnonymousNominalKey,
        crate::durable_semantics::DurableAnonymousNominal,
    >,
    pending: &mut BTreeSet<crate::AnonymousNominalKey>,
    dependencies: impl IntoIterator<Item = crate::AnonymousNominalKey>,
) {
    pending.extend(
        dependencies
            .into_iter()
            .map(|dependency| dependency.with_canonical_producer().into_owned())
            .filter(|dependency| !selected.contains_key(dependency)),
    );
}

pub(in crate::revisioned_query_database) fn with_restored_state<
    S,
    O,
    R,
    Install,
    Operation,
    Restore,
>(
    state: &mut S,
    install: Install,
    operation: Operation,
    restore: Restore,
) -> R
where
    Install: FnOnce(&mut S) -> O,
    Operation: FnOnce(&mut S) -> R,
    Restore: FnOnce(&mut S, O),
{
    let old = install(state);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(state)));
    restore(state, old);
    match result {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Exact query authorities used by durable comptime services. Keeping this
/// adapter separate from the evaluator makes cancellation and import-site
/// resolution reusable by the AIR host without allowing it to inspect RIR.
pub(in crate::revisioned_query_database) struct DurableComptimeRootAuthority<'db> {
    pub(in crate::revisioned_query_database) provider: SemanticNucleusTypeProvider<'db>,
    pub(in crate::revisioned_query_database) imports:
        QueryFamily<DeclarationImportQueryKey, DeclarationImportQueryValue>,
    pub(in crate::revisioned_query_database) session:
        crate::durable_comptime::DurableComptimeSession,
    pub(in crate::revisioned_query_database) foreign: DurableComptimeForeignQueryAuthority<'db>,
}

impl<'db> DurableComptimeRootAuthority<'db> {
    pub(in crate::revisioned_query_database) fn finish_root(
        mut self,
    ) -> SemanticNucleusTypeProvider<'db> {
        let session_effects = self
            .session
            .drain_root_effects()
            .expect("durable AIR root must unwind lifecycle edges");
        self.provider.merge_comptime_effects(
            session_effects,
            &crate::durable_comptime::DurableComptimeApplicationPolicy::preserve(),
        );
        self.provider
    }
}

pub(in crate::revisioned_query_database) fn evaluate_durable_comptime_root(
    authority: &mut DurableComptimeRootAuthority<'_>,
    frame: crate::durable_comptime::DurableComptimeConstFrame,
    mut env: rue_air::ComptimeEnv<
        crate::durable_comptime::EvaluatedSemanticConst,
        crate::durable_comptime::DurableComptimeType,
        crate::durable_comptime::DurableComptimeName,
        crate::durable_comptime::DurableComptimeFile,
        crate::durable_comptime::DurableComptimeIdentity,
    >,
) -> rue_air::ComptimeOutcome<
    crate::durable_comptime::EvaluatedSemanticConst,
    crate::durable_comptime::DurableComptimeHostFailure,
> {
    env.defining_file = frame.context.clone();
    env.expected_result = frame.expected_result.clone();
    let mut host = crate::durable_comptime::DurableComptimeHost::new(authority);
    rue_air::ComptimeEngine::new(&mut host).evaluate(frame, &mut env)
}

/// Classify one canonical AIR root terminal into the query family's two
/// result channels. Semantic failures remain values, retained query failures
/// remain query failures, and aborts retain the AIR abort channel.
pub(in crate::revisioned_query_database) fn durable_comptime_root_result(
    outcome: rue_air::ComptimeOutcome<
        crate::durable_comptime::EvaluatedSemanticConst,
        crate::durable_comptime::DurableComptimeHostFailure,
    >,
) -> Result<
    Result<crate::durable_comptime::EvaluatedSemanticConst, EvaluateSemanticConstError>,
    rue_query::QueryFailure,
> {
    match outcome {
        rue_air::ComptimeOutcome::Known(value) => Ok(Ok(value)),
        rue_air::ComptimeOutcome::HostFailure(error) => match error.into_root_host_failure() {
            Ok(failure) => Ok(Err(EvaluateSemanticConstError::Failure(failure))),
            Err(failure) => Err(failure),
        },
        rue_air::ComptimeOutcome::Abort(error) => Ok(Err(EvaluateSemanticConstError::Abort(
            error.into_root_abort(),
        ))),
        rue_air::ComptimeOutcome::Trap(trap) => Ok(Err(
            crate::durable_comptime::DurableComptimeFailure::comptime_failure(format!(
                "{} (this operation would panic at runtime)",
                trap.operation
            )),
        )),
        rue_air::ComptimeOutcome::RuntimeDependent
        | rue_air::ComptimeOutcome::NotReady
        | rue_air::ComptimeOutcome::UnsupportedContext => Ok(Err(
            crate::durable_comptime::DurableComptimeFailure::resolution(
                "declaration-time comptime did not reduce to a value",
            ),
        )),
    }
}

impl crate::durable_comptime::DurableComptimeForeignCallAuthority
    for DurableComptimeRootAuthority<'_>
{
    fn probe_comptime_call(
        &self,
        producer: &crate::StableDefinitionKey,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> Result<crate::body_query::ForeignComptimeCallLookup, QueryAbort> {
        self.foreign
            .probe_comptime_call(producer, type_arguments, value_arguments)
    }
}

impl crate::durable_comptime::DurableComptimeHostAuthority for DurableComptimeRootAuthority<'_> {
    fn durable_session(&self) -> &crate::durable_comptime::DurableComptimeSession {
        &self.session
    }

    fn durable_session_mut(&mut self) -> &mut crate::durable_comptime::DurableComptimeSession {
        &mut self.session
    }

    #[cfg(test)]
    fn test_array_length_override(&self) -> Option<i128> {
        TEST_ARRAY_LENGTH_OVERRIDE.with(std::cell::Cell::get)
    }
}

fn project_named_value_candidate(
    provider: &SemanticNucleusTypeProvider<'_>,
    accessing_source: &crate::StableDefinitionKey,
    module: &ModuleId,
    name: &str,
    kind: crate::durable_comptime::DurableComptimeNamedValueKind,
) -> Result<
    Option<crate::durable_comptime::DurableComptimeNamedValueProjection>,
    rue_air::SemanticProviderError<
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    >,
> {
    let dependency = |key: crate::StableDefinitionKey| {
        crate::semantic_query_nucleus::SemanticDeclarationDependency {
            source: accessing_source.clone(),
            kind: rue_air::DeclarationTypeDependencyKind::Body,
            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                key,
            ),
        }
    };
    match kind {
        crate::durable_comptime::DurableComptimeNamedValueKind::Const => {
            let Some(candidate) =
                provider.candidate_from(accessing_source, module, name, DefinitionKind::Const)?
            else {
                return Ok(None);
            };
            let resolution = provider.const_resolution(candidate)?;
            let (value, key, anonymous_nominals) = match resolution {
                crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                    key,
                    ty,
                    value,
                    anonymous_nominals,
                    ..
                } => (
                    crate::durable_comptime::EvaluatedSemanticConst::Value(
                        crate::durable_comptime::TypedSemanticConst::typed(*value, ty),
                    ),
                    key,
                    anonymous_nominals,
                ),
                crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                    key,
                    target,
                } => (
                    crate::durable_comptime::EvaluatedSemanticConst::Module(target),
                    key,
                    Arc::from([]),
                ),
            };
            Ok(Some(
                crate::durable_comptime::DurableComptimeNamedValueProjection::new(
                    value,
                    dependency(key),
                )
                .with_anonymous_nominals(anonymous_nominals),
            ))
        }
        crate::durable_comptime::DurableComptimeNamedValueKind::Function => {
            let Some(candidate) = provider.candidate_from(
                accessing_source,
                module,
                name,
                DefinitionKind::Function,
            )?
            else {
                return Ok(None);
            };
            let identity = provider.identity(candidate)?;
            let key = identity.key.clone();
            Ok(Some(
                crate::durable_comptime::DurableComptimeNamedValueProjection::new(
                    crate::durable_comptime::EvaluatedSemanticConst::Value(
                        crate::durable_comptime::TypedSemanticConst::typed(
                            crate::durable_semantics::DurableConstValue::Function(key),
                            crate::durable_semantics::DurableType::ComptimeType,
                        ),
                    ),
                    dependency(identity.key),
                ),
            ))
        }
        crate::durable_comptime::DurableComptimeNamedValueKind::Struct
        | crate::durable_comptime::DurableComptimeNamedValueKind::Enum => {
            let definition_kind = match kind {
                crate::durable_comptime::DurableComptimeNamedValueKind::Struct => {
                    DefinitionKind::Struct
                }
                crate::durable_comptime::DurableComptimeNamedValueKind::Enum => {
                    DefinitionKind::Enum
                }
                crate::durable_comptime::DurableComptimeNamedValueKind::Const
                | crate::durable_comptime::DurableComptimeNamedValueKind::Function => {
                    unreachable!("scalar named-value kinds handled above")
                }
            };
            let Some(candidate) =
                provider.candidate_from(accessing_source, module, name, definition_kind)?
            else {
                return Ok(None);
            };
            let identity = provider.identity(candidate)?;
            let key = identity.key.clone();
            Ok(Some(
                crate::durable_comptime::DurableComptimeNamedValueProjection::new(
                    crate::durable_comptime::EvaluatedSemanticConst::Value(
                        crate::durable_comptime::TypedSemanticConst::typed(
                            crate::durable_semantics::DurableConstValue::Type(
                                crate::durable_semantics::DurableType::Nominal(key),
                            ),
                            crate::durable_semantics::DurableType::ComptimeType,
                        ),
                    ),
                    dependency(identity.key),
                ),
            ))
        }
    }
}

impl crate::durable_comptime::DurableComptimeSemanticAuthority
    for DurableComptimeRootAuthority<'_>
{
    fn check_canceled(&self) -> Result<(), QueryAbort> {
        self.provider.context.check_canceled()
    }

    fn resolve_type_syntax(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Result<
        crate::durable_semantics::DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
            StableDefinitionKey,
            Arc<str>,
        >,
    > {
        let Some(registered) = self.session.registered_program(program) else {
            return Err(rue_air::SemanticResolutionError::ProviderFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                    "durable comptime type syntax references an unregistered program",
                )),
            ));
        };
        let module = program.declaration.module();
        let source = program.declaration.clone();
        self.provider.with_dependency_source(&source, |provider| {
            rue_air::resolve_structured_semantic_type_syntax_with(
                provider,
                module,
                registered.rir.type_syntax(),
                syntax,
                |symbol| registered.symbols[symbol.into_usize()].as_ref(),
            )
        })
    }

    fn resolve_type_syntax_with_substitutions(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_substitutions: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> Result<
        crate::durable_semantics::DurableType,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
            StableDefinitionKey,
            Arc<str>,
        >,
    > {
        let (provider, session) = (&mut self.provider, &self.session);
        let Some(registered) = session.registered_program(program) else {
            return Err(rue_air::SemanticResolutionError::ProviderFailure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                    "durable comptime type syntax references an unregistered program",
                )),
            ));
        };
        let module = program.declaration.module();
        let source = program.declaration.clone();
        provider.with_dependency_source(&source, |provider| {
            provider.with_comptime_substitutions(
                type_substitutions,
                value_substitutions,
                |provider| {
                    rue_air::resolve_structured_semantic_type_syntax_with(
                        provider,
                        module,
                        registered.rir.type_syntax(),
                        syntax,
                        |symbol| registered.symbols[symbol.into_usize()].as_ref(),
                    )
                },
            )
        })
    }

    fn begin_structured_type(
        &mut self,
        program: &crate::body_query::DurableComptimeProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        type_substitutions: Vec<(Arc<str>, crate::durable_semantics::DurableType)>,
        value_substitutions: Vec<(Arc<str>, crate::durable_semantics::DurableConstValue)>,
    ) -> Result<
        crate::durable_comptime::DurableStructuredTypePoll,
        crate::durable_comptime::DurableStructuredTypeBeginError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let source = program.declaration.clone();
        self.provider.with_dependency_source(&source, |provider| {
            crate::durable_comptime::begin_durable_structured_type(
                &self.session,
                program,
                syntax,
                type_substitutions,
                value_substitutions,
                provider,
            )
        })
    }

    fn resume_structured_type(
        &mut self,
        job: crate::durable_comptime::DurableStructuredTypeJob,
        reduced: rue_air::SemanticProviderResult<
            Option<
                rue_air::SemanticComptimeCallResult<
                    crate::durable_semantics::DurableType,
                    crate::durable_semantics::DurableConstValue,
                >,
            >,
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    ) -> Result<
        crate::durable_comptime::DurableStructuredTypePoll,
        rue_air::SemanticTypeSyntaxError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
            StableDefinitionKey,
            Arc<str>,
        >,
    > {
        let source = job.program().key().declaration.clone();
        self.provider.with_dependency_source(&source, |provider| {
            crate::durable_comptime::resume_durable_structured_type(job, provider, reduced)
        })
    }

    fn begin_comptime_call_admission(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        crate::durable_comptime::DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        type Failure = crate::semantic_query_nucleus::SemanticNucleusFailure;

        let candidate = self.provider.candidate_from(
            accessing_source,
            module,
            name,
            DefinitionKind::Function,
        )?;
        let Some(candidate) = candidate else {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(format!("undefined comptime function `{name}`"))),
            ));
        };
        let identity = self.provider.identity(candidate.clone())?;
        Ok(crate::durable_comptime::DurableComptimeCallableAdmissionStart {
            candidate,
            identity: identity.clone(),
            configuration: self.provider.configuration.clone(),
            name: Arc::from(name),
            dependency: crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: accessing_source.clone(),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target:
                    crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        identity.key,
                    ),
            },
        })
    }

    fn begin_comptime_call_admission_for_key(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        head: &crate::StableDefinitionKey,
    ) -> Result<
        crate::durable_comptime::DurableComptimeCallableAdmissionStart,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        type Failure = crate::semantic_query_nucleus::SemanticNucleusFailure;

        let Some(candidate) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(head)
        else {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(format!(
                    "undefined comptime function `{}`",
                    head.name()
                ))),
            ));
        };
        let identity = self.provider.identity(candidate.clone())?;
        if identity.key != *head {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(
                    "comptime function identity does not match requested key",
                )),
            ));
        }
        Ok(crate::durable_comptime::DurableComptimeCallableAdmissionStart {
            candidate,
            identity: identity.clone(),
            configuration: self.provider.configuration.clone(),
            name: Arc::from(head.name()),
            dependency: crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: accessing_source.clone(),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target:
                    crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        identity.key,
                    ),
            },
        })
    }

    fn finish_comptime_call_admission(
        &self,
        start: crate::durable_comptime::DurableComptimeCallableAdmissionStart,
        argument_modes: &[crate::durable_semantics::DurableParameterMode],
    ) -> Result<
        crate::durable_comptime::DurableComptimeCallableAdmission,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        type Failure = crate::semantic_query_nucleus::SemanticNucleusFailure;
        let crate::durable_comptime::DurableComptimeCallableAdmissionStart {
            candidate,
            identity,
            configuration,
            name,
            dependency: _,
        } = start;
        let signature = self.provider.signature(candidate.clone())?;
        let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
            parameters,
            result,
            ..
        } = signature
        else {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(format!("`{name}` is not callable"))),
            ));
        };
        let shell = self
            .provider
            .context
            .query_registered(
                self.provider.shells,
                DeclarationShellQueryKey(candidate.clone()),
            )
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(shell)) =
            shell.outcome()
        else {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from("comptime call shell became unavailable")),
            ));
        };
        if shell.parameters.len() != argument_modes.len()
            || parameters.len() != argument_modes.len()
        {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Resolution(Arc::from(format!(
                    "comptime call `{name}` has the wrong arity"
                ))),
            ));
        }
        for (parameter, argument_mode) in parameters.iter().zip(argument_modes.iter().copied()) {
            use crate::durable_semantics::DurableParameterMode as ParameterMode;
            let failure = match (parameter.mode, argument_mode) {
                (ParameterMode::Value, ParameterMode::Value)
                | (ParameterMode::Borrow, ParameterMode::Borrow)
                | (ParameterMode::Inout, ParameterMode::Inout) => None,
                (ParameterMode::Inout, _) => Some(rue_error::ErrorKind::InoutKeywordMissing),
                (ParameterMode::Borrow, _) => Some(rue_error::ErrorKind::BorrowKeywordMissing),
                (ParameterMode::Value, ParameterMode::Borrow) => {
                    Some(rue_error::ErrorKind::UnexpectedCallArgumentMode { mode: "borrow" })
                }
                (ParameterMode::Value, ParameterMode::Inout) => {
                    Some(rue_error::ErrorKind::UnexpectedCallArgumentMode { mode: "inout" })
                }
            };
            if let Some(kind) = failure {
                return Err(rue_air::SemanticProviderError::Failure(
                    Failure::Diagnostic(kind),
                ));
            }
        }
        let all_parameters_comptime =
            !parameters.is_empty() && parameters.iter().all(|parameter| parameter.is_comptime);
        let is_type_function = result == crate::durable_semantics::DurableType::ComptimeType;
        let eligible = if is_type_function {
            parameters.is_empty() || all_parameters_comptime
        } else {
            all_parameters_comptime
        };
        if !eligible {
            return Err(rue_air::SemanticProviderError::Failure(
                Failure::Diagnostic(rue_error::ErrorKind::ConstExprNotSupported {
                    expr_kind: format!("call to `{name}`"),
                }),
            ));
        }
        Ok(crate::durable_comptime::DurableComptimeCallableAdmission {
            candidate,
            identity,
            configuration,
            parameters,
            result,
            shell_parameters: shell.parameters.clone(),
        })
    }

    fn resolve_named_value(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<crate::durable_comptime::DurableComptimeNamedValueProjection>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        #[cfg(test)]
        {
            TEST_NAMED_VALUE_CHECKS.with(|checks| {
                checks.set(checks.get() + 1);
            });
            if TEST_NAMED_VALUE_CANCEL.with(std::cell::Cell::get) {
                return Err(rue_air::SemanticProviderError::Abort(QueryAbort::Canceled));
            }
        }
        crate::durable_comptime::resolve_named_value_in_order(|kind| {
            project_named_value_candidate(&self.provider, accessing_source, module, name, kind)
        })
    }

    fn resolve_module_member(
        &self,
        accessing_source: &crate::StableDefinitionKey,
        module: &ModuleId,
        member: &str,
    ) -> Result<
        crate::durable_comptime::DurableComptimeNamedValueProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(projection) = crate::durable_comptime::resolve_module_member_in_order(|kind| {
            project_named_value_candidate(&self.provider, accessing_source, module, member, kind)
        })?
        else {
            return Err(rue_air::SemanticProviderError::Failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::UnknownModuleMember {
                        module_name: module.to_string(),
                        member_name: member.to_owned(),
                    },
                ),
            ));
        };
        Ok(projection)
    }

    fn resolve_import(
        &self,
        site: &crate::durable_comptime::DurableImportSite,
    ) -> Result<crate::durable_comptime::DurableImportResolution, QueryAbort> {
        let key = crate::declaration_candidate::DeclarationImportSiteKey {
            declaration: site.declaration.clone(),
            occurrence: site.occurrence,
            specifier: site.specifier.clone(),
        };
        let terminal = self
            .provider
            .context
            .query_registered(&self.imports, DeclarationImportQueryKey(key))?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("DeclarationImport publishes typed values")
        };
        Ok(match value {
            DeclarationImportQueryValue::Available(crate::CanonicalImportResolution::Resolved(
                module,
            )) => crate::durable_comptime::DurableImportResolution::Resolved(module.clone()),
            DeclarationImportQueryValue::Available(crate::CanonicalImportResolution::Missing) => {
                crate::durable_comptime::DurableImportResolution::Missing
            }
            DeclarationImportQueryValue::Failure(failure) => {
                crate::durable_comptime::DurableImportResolution::Failure(failure.clone())
            }
        })
    }

    fn resolve_keyed_import(
        &self,
        site: &rue_air::ComptimeSite<crate::body_query::DurableComptimeProgramKey>,
        specifier: &str,
    ) -> Result<
        crate::durable_comptime::DurableImportResolution,
        crate::durable_comptime::DurableComptimeKeyedImportError,
    > {
        if site.kind() != rue_air::ComptimeSiteKind::Import {
            return Err(crate::durable_comptime::DurableComptimeKeyedImportError::WrongSiteKind);
        }
        let program = site.program();
        let Some(registered) = self.session.registered_program(program) else {
            return Err(crate::durable_comptime::DurableComptimeKeyedImportError::UnknownProgram);
        };
        let Some(occurrence) = registered
            .imports
            .imports
            .iter()
            .find(|occurrence| occurrence.occurrence == site.occurrence())
        else {
            return Err(
                crate::durable_comptime::DurableComptimeKeyedImportError::UnknownInstruction,
            );
        };
        if occurrence.specifier.as_ref() != specifier {
            return Err(
                crate::durable_comptime::DurableComptimeKeyedImportError::SpecifierMismatch,
            );
        }
        let Some(declaration) =
            crate::revisioned_query_database::declaration_candidate_for_stable_key(
                &program.declaration,
            )
        else {
            return Err(
                crate::durable_comptime::DurableComptimeKeyedImportError::UnknownDeclaration,
            );
        };
        let durable_site = crate::durable_comptime::DurableImportSite {
            declaration,
            occurrence: occurrence.occurrence,
            specifier: occurrence.specifier.clone(),
        };
        self.resolve_import(&durable_site)
            .map_err(crate::durable_comptime::DurableComptimeKeyedImportError::ProviderAbort)
    }

    fn resolve_target_intrinsic(
        &self,
        intrinsic: rue_air::ComptimeTargetIntrinsic,
        argument_count: usize,
    ) -> Result<
        crate::durable_comptime::TargetEnumValue,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        crate::durable_comptime::resolve_target_intrinsic_facts(
            intrinsic,
            argument_count,
            self.provider.configuration.target.arch(),
            self.provider.configuration.target.os(),
            self.provider.configuration.target.data_model(),
        )
        .map_err(rue_air::SemanticProviderError::Failure)
    }

    fn resolve_target_enum_variant(
        &self,
        type_name: &str,
        variant: &str,
    ) -> Result<
        crate::durable_comptime::TargetEnumValue,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        crate::durable_comptime::resolve_target_enum_variant_facts(type_name, variant)
            .map_err(rue_air::SemanticProviderError::Failure)
    }
}

thread_local! {
    pub(in crate::revisioned_query_database) static SEMANTIC_COMPTIME_CALL_DEPTH: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    #[cfg(test)]
    static TEST_NAMED_VALUE_CANCEL: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    #[cfg(test)]
    pub(in crate::revisioned_query_database) static TEST_NAMED_VALUE_CHECKS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    #[cfg(test)]
    static TEST_ARRAY_LENGTH_OVERRIDE: std::cell::Cell<Option<i128>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(in crate::revisioned_query_database) struct TestSemanticComptimeNamedValueCancelGuard {
    previous: bool,
}
#[cfg(test)]
impl TestSemanticComptimeNamedValueCancelGuard {
    pub(in crate::revisioned_query_database) fn set(value: bool) -> Self {
        let previous = TEST_NAMED_VALUE_CANCEL.with(|slot| {
            let previous = slot.get();
            slot.set(value);
            previous
        });
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for TestSemanticComptimeNamedValueCancelGuard {
    fn drop(&mut self) {
        TEST_NAMED_VALUE_CANCEL.with(|slot| slot.set(self.previous));
    }
}

#[cfg(test)]
pub(in crate::revisioned_query_database) struct TestSemanticComptimeArrayLengthOverrideGuard {
    previous: Option<i128>,
}
#[cfg(test)]
impl TestSemanticComptimeArrayLengthOverrideGuard {
    pub(in crate::revisioned_query_database) fn set(value: Option<i128>) -> Self {
        let previous = TEST_ARRAY_LENGTH_OVERRIDE.with(|slot| {
            let previous = slot.get();
            slot.set(value);
            previous
        });
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for TestSemanticComptimeArrayLengthOverrideGuard {
    fn drop(&mut self) {
        TEST_ARRAY_LENGTH_OVERRIDE.with(|slot| slot.set(self.previous));
    }
}

/// Query-stack ticket for a durable comptime call.
///
/// The query boundary owns this ticket so cancellation and unwinding restore
/// the caller's depth; the limit and diagnostic authority remain in AIR.
pub(in crate::revisioned_query_database) struct SemanticComptimeCallDepthGuard(usize);

impl SemanticComptimeCallDepthGuard {
    pub(in crate::revisioned_query_database) fn enter(
        name: &str,
    ) -> Result<Self, EvaluateSemanticConstError> {
        SEMANTIC_COMPTIME_CALL_DEPTH.with(|depth| {
            let current = depth.get();
            // This guard wraps child query entries rather than the root AIR
            // frame, so the first active query is propagated call depth one.
            let propagated_depth = rue_air::next_comptime_depth(current);
            if rue_air::comptime_depth_over_limit(propagated_depth) {
                return Err(
                    crate::durable_comptime::DurableComptimeFailure::maximum_depth(
                        name,
                        rue_air::MAX_COMPTIME_CALL_DEPTH,
                    ),
                );
            }
            depth.set(current + 1);
            Ok(Self(current))
        })
    }
}

impl Drop for SemanticComptimeCallDepthGuard {
    fn drop(&mut self) {
        SEMANTIC_COMPTIME_CALL_DEPTH.with(|depth| depth.set(self.0));
    }
}
