macro_rules! register_semantic_semantic_nucleus {
    ($artifacts_for_semantic_nucleus:ident, $declaration_memo_retention:ident, $imports_for_semantic_nucleus:ident, $names_for_semantic_nucleus:ident, $parse_for_semantic_nucleus:ident, $produced_anonymous_for_semantic_nucleus:ident, $runtime:ident, $shells_for_semantic_nucleus:ident, $type_facts_for_semantic_nucleus:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.semantic-nucleus",
                $declaration_memo_retention,
                |left: &crate::semantic_query_nucleus::SemanticNucleusValue,
                 right: &crate::semantic_query_nucleus::SemanticNucleusValue| left == right,
                move |context, family, key: &crate::semantic_query_nucleus::SemanticNucleusKey| {
                    use crate::semantic_query_nucleus::{
                        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
                        SemanticNucleusValue as Value,
                    };

                    let shell = context.query_registered(
                        &$shells_for_semantic_nucleus,
                        DeclarationShellQueryKey(key.declaration().clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(shell) = shell.outcome() else {
                        unreachable!("DeclarationShell publishes typed values")
                    };
                    let shell = match shell {
                        DeclarationShellQueryValue::Available(shell) => shell,
                        DeclarationShellQueryValue::Failure(failure) => {
                            let value = Value::Failure(Failure::Shell(Arc::from(format!(
                                "{failure:?}"
                            ))));
                            return Ok(QueryOutput::success(value)
                                .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                    };
                    let value = match key {
                        #[cfg(test)]
                        Key::EngineCycleProbe(_) => {
                            let _ = context.query_registered(family, key.clone())?;
                            unreachable!("engine cycle probe must abort before publication")
                        }
                        Key::Identity(query) => {
                            if query.declaration.category
                                == crate::declaration_candidate::DeclarationCandidateCategory::Destructor
                            {
                                let checked = context.query_registered(
                                    family,
                                    Key::Signature(query.clone()),
                                )?;
                                let rue_query::QueryOutcome::Success(checked) = checked.outcome()
                                else {
                                    unreachable!("SemanticNucleus publishes typed values")
                                };
                                match checked {
                                    Value::Failure(failure) => {
                                        return Ok(QueryOutput::success(Value::Failure(
                                            failure.clone(),
                                        ))
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                    Value::Signature(_) => {}
                                    _ => {
                                        let value = Value::Failure(Failure::Resolution(Arc::from(
                                            "destructor validity returned the wrong projection",
                                        )));
                                        return Ok(QueryOutput::success(value)
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                }
                            }
                            if let Some(identity) =
                                crate::semantic_query_nucleus::direct_identity(shell)
                            {
                                Value::Identity(identity)
                            } else {
                                let resolved = context.query_registered(
                                    family,
                                    Key::ConstResolution(query.clone()),
                                )?;
                                let rue_query::QueryOutcome::Success(resolved) =
                                    resolved.outcome()
                                else {
                                    unreachable!("SemanticNucleus publishes typed values")
                                };
                                match resolved {
                                    Value::ConstResolution(
                                        crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                                            key,
                                            ..
                                        }
                                        | crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                                            key,
                                            ..
                                        },
                                    ) => Value::Identity(
                                        crate::semantic_query_nucleus::DeclarationIdentityProjection {
                                            key: key.clone(),
                                            is_public: shell.is_public,
                                        },
                                    ),
                                    Value::Failure(failure) => Value::Failure(failure.clone()),
                                    _ => Value::Failure(Failure::Resolution(Arc::from(
                                        "const identity dependency returned the wrong projection",
                                    ))),
                                }
                            }
                        }
                        Key::Signature(query) => {
                            if query.declaration.category
                                == crate::declaration_candidate::DeclarationCandidateCategory::Destructor
                            {
                                let named_types = context.query_registered(
                                    &$names_for_semantic_nucleus,
                                    LookupNameKey {
                                        module: query.declaration.module.clone(),
                                        namespace: DefinitionNamespace::ModuleItem,
                                        name: query.declaration.name.clone(),
                                    },
                                )?;
                                let rue_query::QueryOutcome::Success(LookupNameValue(named_types)) =
                                    named_types.outcome()
                                else {
                                    unreachable!("LookupName publishes typed values")
                                };
                                let named_types = match named_types {
                                    Ok(named_types) => named_types,
                                    Err(failure) => {
                                        let value = Value::Failure(Failure::Resolution(Arc::from(
                                            format!("{failure:?}"),
                                        )));
                                        return Ok(QueryOutput::success(value)
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                };
                                if !named_types
                                    .iter()
                                    .any(|fact| fact.kind == DefinitionKind::Struct)
                                {
                                    let value = Value::Failure(Failure::Diagnostic(
                                        rue_air::declaration_validation::destructor_unknown_type(
                                            &query.declaration.name,
                                        ),
                                    ));
                                    return Ok(QueryOutput::success(value)
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                }

                                let destructors = context.query_registered(
                                    &$names_for_semantic_nucleus,
                                    LookupNameKey {
                                        module: query.declaration.module.clone(),
                                        namespace: DefinitionNamespace::Destructor,
                                        name: query.declaration.name.clone(),
                                    },
                                )?;
                                let rue_query::QueryOutcome::Success(LookupNameValue(destructors)) =
                                    destructors.outcome()
                                else {
                                    unreachable!("LookupName publishes typed values")
                                };
                                let destructors = match destructors {
                                    Ok(destructors) => destructors,
                                    Err(failure) => {
                                        let value = Value::Failure(Failure::Resolution(Arc::from(
                                            format!("{failure:?}"),
                                        )));
                                        return Ok(QueryOutput::success(value)
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                };
                                if destructors
                                    .iter()
                                    .filter(|fact| fact.kind == DefinitionKind::Destructor)
                                    .nth(1)
                                    .is_some()
                                {
                                    let mut duplicate = query.declaration.clone();
                                    duplicate.duplicate_discriminator = 1;
                                    let value = Value::Failure(
                                        Failure::DiagnosticAtDeclaration {
                                            kind: rue_air::declaration_validation::duplicate_destructor(
                                                &query.declaration.name,
                                            ),
                                            declaration: duplicate,
                                        },
                                    );
                                    return Ok(QueryOutput::success(value)
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                }
                            }
                            context.check_canceled()?;
                            let parsed_module = context.query_registered(
                                &$parse_for_semantic_nucleus,
                                ModuleQueryKey(query.declaration.module.clone()),
                            )?;
                            let rue_query::QueryOutcome::Success(parsed_module) =
                                parsed_module.outcome()
                            else {
                                unreachable!("ParseModule publishes typed values")
                            };
                            let parsed = match &parsed_module.result {
                                Ok(module) => crate::semantic_query_nucleus::project_semantic_signature(
                                    module,
                                    &query.declaration,
                                ),
                                Err(_) => Err(Arc::from(
                                    "declaration signature module failed to parse",
                                )),
                            };
                            context.check_canceled()?;
                            match parsed {
                                Err(failure) => Value::Failure(Failure::Syntax(failure)),
                                Ok(parsed) => {
                                            if let crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
                                                parameters,
                                                ..
                                            } = &parsed
                                                && let Some((kind, ordinal)) = rue_air::declaration_validation::duplicate_parameter_with_ordinal(
                                                    parameters.iter().map(|parameter| parsed.symbol(parameter.name)),
                                                )
                                            {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::DiagnosticAtParameter {
                                                        kind,
                                                        ordinal: ordinal as u32,
                                                    },
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                            if matches!(
                                                query.declaration.category,
                                                crate::declaration_candidate::DeclarationCandidateCategory::Function
                                                    | crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction
                                            ) && let Some(kind) = rue_air::declaration_validation::reserved_function_name(
                                                &query.declaration.name,
                                            ) {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::Diagnostic(kind),
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                            let mut substitutions = BTreeMap::new();
                                            if let Some(owner) = &query.declaration.owner {
                                                let owner_candidate = crate::declaration_candidate::DeclarationCandidateKey {
                                                    module: query.declaration.module.clone(),
                                                    category: owner.category,
                                                    name: owner.name.clone(),
                                                    owner: None,
                                                    duplicate_discriminator: 0,
                                                };
                                                let owner = context.query_registered(
                                                    family,
                                                    Key::Identity(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                                                        declaration: owner_candidate,
                                                        configuration: query.configuration.clone(),
                                                    }),
                                                )?;
                                                let rue_query::QueryOutcome::Success(owner) = owner.outcome() else {
                                                    unreachable!("SemanticNucleus publishes typed values")
                                                };
                                                if let Value::Identity(owner) = owner {
                                                    substitutions.insert(
                                                        Arc::from("Self"),
                                                        crate::durable_semantics::DurableType::Nominal(owner.key.clone()),
                                                    );
                                                }
                                            }
                                            let dependency_source = crate::semantic_query_nucleus::direct_identity(shell)
                                                .expect("signature shell has a direct identity")
                                                .key;
                                            let mut provider = SemanticNucleusTypeProvider {
                                                context,
                                                family,
                                                shells: &$shells_for_semantic_nucleus,
                                                names: &$names_for_semantic_nucleus,
                                                configuration: query.configuration.clone(),
                                                substitutions,
                                                value_substitutions: BTreeMap::new(),
                                                deferred_value_parameters: BTreeMap::new(),
                                                anonymous_nominals: BTreeMap::new(),
                                                dependency_source,
                                                dependency_kind: rue_air::DeclarationTypeDependencyKind::Signature,
                                                dependencies: BTreeSet::new(),
                                                deferred_ownership: BTreeSet::new(),
                                                ownership_properties: BTreeMap::new(),
                                            };
                                            match resolve_parsed_semantic_signature(
                                                &mut provider,
                                                &query.declaration.module,
                                                &parsed,
                                            ) {
                                                Ok(signature) => Value::Signature(
                                                    crate::semantic_query_nucleus::ResolvedDeclarationSignature {
                                                        definition: provider.dependency_source.clone(),
                                                        signature,
                                                        callable_type_syntax: parsed
                                                            .callable_type_syntax(),
                                                        anonymous_nominals: provider
                                                            .anonymous_nominals
                                                            .values()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                        dependencies: provider
                                                            .dependencies
                                                            .iter()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                        deferred_ownership: provider
                                                            .deferred_ownership
                                                            .iter()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                    },
                                                ),
                                                Err(ResolveSemanticSignatureError::Abort(
                                                    QueryAbort::Cycle(nodes),
                                                )) => Value::Failure(Failure::SignatureReentry {
                                                    signature: provider.dependency_source.clone(),
                                                    cycle: semantic_nucleus_cycle_names(&nodes),
                                                }),
                                                Err(ResolveSemanticSignatureError::Abort(abort)) => {
                                                    return Err(abort)
                                                }
                                                Err(ResolveSemanticSignatureError::Failure(failure)) => {
                                                    Value::Failure(*failure)
                                                }
                                            }
                                }
                            }
                        }
                        Key::NominalWellFormedness(query) => {
                            let identity = crate::semantic_query_nucleus::direct_identity(shell)
                                .expect("nominal well-formedness has a direct identity");
                            let mut provider = SemanticNucleusTypeProvider {
                                context,
                                family,
                                shells: &$shells_for_semantic_nucleus,
                                names: &$names_for_semantic_nucleus,
                                configuration: query.configuration.clone(),
                                substitutions: BTreeMap::new(),
                                value_substitutions: BTreeMap::new(),
                                deferred_value_parameters: BTreeMap::new(),
                                anonymous_nominals: BTreeMap::new(),
                                dependency_source: identity.key.clone(),
                                dependency_kind:
                                    rue_air::DeclarationTypeDependencyKind::Signature,
                                dependencies: BTreeSet::new(),
                                deferred_ownership: BTreeSet::new(),
                                ownership_properties: BTreeMap::new(),
                            };
                            match provider
                                .validate_nominal_well_formedness(query.declaration.clone())
                            {
                                Ok(()) => Value::NominalWellFormedness,
                                Err(rue_air::SemanticProviderError::Failure(failure)) => {
                                    Value::Failure(failure)
                                }
                                Err(rue_air::SemanticProviderError::Abort(abort)) => {
                                    return Err(abort);
                                }
                            }
                        }
                        Key::DeferredOwnership(query) => {
                            let (dependency_source, anonymous_nominals) = if query
                                .producer
                                .declaration
                                .category
                                == crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate
                            {
                                let resolution = context.query_registered(
                                    family,
                                    Key::ConstResolution(query.producer.clone()),
                                )?;
                                let rue_query::QueryOutcome::Success(resolution) =
                                    resolution.outcome()
                                else {
                                    unreachable!("SemanticNucleus publishes typed values")
                                };
                                match resolution {
                                    Value::ConstResolution(
                                        crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                                            key,
                                            anonymous_nominals,
                                            ..
                                        },
                                    ) => (key.clone(), anonymous_nominals.clone()),
                                    Value::Failure(failure) => {
                                        return Ok(QueryOutput::success(Value::Failure(
                                            failure.clone(),
                                        ))
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                    _ => unreachable!(
                                        "const deferred ownership producer returned the wrong projection"
                                    ),
                                }
                            } else {
                                let signature = context.query_registered(
                                    family,
                                    Key::Signature(query.producer.clone()),
                                )?;
                                let rue_query::QueryOutcome::Success(signature) =
                                    signature.outcome()
                                else {
                                    unreachable!("SemanticNucleus publishes typed values")
                                };
                                match signature {
                                    Value::Signature(signature) => (
                                        crate::semantic_query_nucleus::direct_identity(shell)
                                            .expect(
                                                "deferred ownership producer has a direct identity",
                                            )
                                            .key,
                                        signature.anonymous_nominals.clone(),
                                    ),
                                    Value::Failure(failure) => {
                                        return Ok(QueryOutput::success(Value::Failure(
                                            failure.clone(),
                                        ))
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                    _ => unreachable!(
                                        "signature deferred ownership producer returned the wrong projection"
                                    ),
                                }
                            };
                            let mut provider = SemanticNucleusTypeProvider {
                                context,
                                family,
                                shells: &$shells_for_semantic_nucleus,
                                names: &$names_for_semantic_nucleus,
                                configuration: query.producer.configuration.clone(),
                                substitutions: BTreeMap::new(),
                                value_substitutions: BTreeMap::new(),
                                deferred_value_parameters: BTreeMap::new(),
                                anonymous_nominals: BTreeMap::new(),
                                dependency_source,
                                dependency_kind:
                                    rue_air::DeclarationTypeDependencyKind::Signature,
                                dependencies: BTreeSet::new(),
                                deferred_ownership: BTreeSet::new(),
                                ownership_properties: BTreeMap::new(),
                            };
                            if let Err(error) = provider
                                .merge_anonymous_projections(anonymous_nominals.as_ref())
                            {
                                match error {
                                    rue_air::SemanticProviderError::Failure(failure) => {
                                        return Ok(QueryOutput::success(Value::Failure(failure))
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                    rue_air::SemanticProviderError::Abort(abort) => {
                                        return Err(abort);
                                    }
                                }
                            }
                            let gate_type_name = deferred_gate_type_diagnostic_name(
                                context,
                                family,
                                &query.gate.ty,
                                &query.producer.configuration,
                            )?;
                            let result = match query.gate.kind {
                                crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireDroppable => provider
                                    .type_carries_linear(&query.gate.ty)
                                    .map(|rejected| rejected.then(|| {
                                        rue_error::ErrorKind::ContainerElementIsLinear {
                                            ty: gate_type_name.clone(),
                                        }
                                    })),
                                crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireTriviallyDroppable => provider
                                    .type_has_drop_glue(
                                        $type_facts_for_semantic_nucleus
                                            .get()
                                            .expect("TypeFacts family is installed before requests"),
                                        &query.gate.ty,
                                    )
                                    .map(|rejected| rejected.then(|| {
                                        rue_error::ErrorKind::ContainerElementNotTriviallyDroppable {
                                            ty: gate_type_name.clone(),
                                        }
                                    })),
                            };
                            match result {
                                Ok(Some(kind)) => Value::Failure(Failure::OwnershipGate {
                                    kind,
                                    gate: query.gate.clone(),
                                }),
                                Ok(None) => Value::DeferredOwnership,
                                Err(rue_air::SemanticProviderError::Failure(failure)) => {
                                    Value::Failure(failure)
                                }
                                Err(rue_air::SemanticProviderError::Abort(abort)) => {
                                    return Err(abort)
                                }
                            }
                        }
                        Key::ConstResolution(query) => {
                            let named = context.query_registered(
                                &$names_for_semantic_nucleus,
                                LookupNameKey {
                                    module: query.declaration.module.clone(),
                                    namespace: DefinitionNamespace::ModuleItem,
                                    name: query.declaration.name.clone(),
                                },
                            )?;
                            let rue_query::QueryOutcome::Success(LookupNameValue(named)) =
                                named.outcome()
                            else {
                                unreachable!("LookupName publishes typed values")
                            };
                            let named = match named {
                                Ok(named) => named,
                                Err(failure) => {
                                    let value = Value::Failure(Failure::Resolution(Arc::from(
                                        format!("{failure:?}"),
                                    )));
                                    return Ok(QueryOutput::success(value)
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                }
                            };
                            let const_count = named
                                .iter()
                                .filter(|fact| fact.kind == DefinitionKind::Const)
                                .count();
                            if const_count > 1 {
                                let value = Value::Failure(Failure::Diagnostic(
                                    rue_air::declaration_validation::duplicate_constant(
                                        &query.declaration.name,
                                    ),
                                ));
                                return Ok(QueryOutput::success(value)
                                    .with_terminal_kind(QueryTerminalKind::Failure));
                            }
                            if let Some(kind) =
                                rue_air::declaration_validation::const_cross_kind_collision(
                                    &query.declaration.name,
                                    const_count == 1,
                                    named.iter().any(|fact| fact.kind != DefinitionKind::Const),
                                )
                            {
                                let value = Value::Failure(Failure::Diagnostic(kind));
                                return Ok(QueryOutput::success(value)
                                    .with_terminal_kind(QueryTerminalKind::Failure));
                            }
                            let artifact = context.query_registered(
                                &$artifacts_for_semantic_nucleus,
                                DeclarationBodyPlanQueryKey(query.declaration.clone()),
                            )?;
                            let rue_query::QueryOutcome::Success(artifact) = artifact.outcome()
                            else {
                                unreachable!("DeclarationBodyPlanArtifacts publishes typed values")
                            };
                            match artifact {
                                DeclarationBodyPlanArtifactsValue::Failure(failure) => {
                                    Value::Failure(candidate_rir_semantic_failure(failure))
                                }
                                DeclarationBodyPlanArtifactsValue::Available(artifact) => {
                                    let const_identity = crate::semantic_query_nucleus::classified_const_identity(shell, false);
                                    let program_key = crate::body_query::DurableComptimeProgramKey {
                                        declaration: const_identity.key.clone(),
                                        configuration: query.configuration.clone(),
                                    };
                                    let core = match crate::body_query::OwnedComptimeProgramCore::from_const_body_plan_without_imports(
                                        crate::body_query::DurableComptimeProgramPlan {
                                            key: program_key.clone(),
                                            candidate: query.declaration.clone(),
                                        },
                                        artifact,
                                        || context.check_canceled(),
                                    ) {
                                        Ok(core) => core,
                                        Err(crate::body_query::ComptimeProgramProjectionFailure::NotConst { .. }) => {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::Resolution(Arc::from(
                                                        "constant candidate artifact has a non-constant root",
                                                    )),
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                        }
                                        Err(crate::body_query::ComptimeProgramProjectionFailure::Materialization(
                                            failure,
                                        )) => match semantic_materialization_failure(failure) {
                                            Ok(failure) => {
                                                return Ok(QueryOutput::success(
                                                    Value::Failure(failure),
                                                )
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                            Err(abort) => return Err(abort),
                                        },
                                        Err(failure) => {
                                            return Ok(QueryOutput::success(Value::Failure(
                                                Failure::Resolution(Arc::from(format!(
                                                    "{failure:?}"
                                                ))),
                                            ))
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                        }
                                            };
                                    let Some((_, declared_type, _root)) =
                                        core.const_root()
                                    else {
                                        unreachable!("const core validated its root kind");
                                    };
                                    let provider = SemanticNucleusTypeProvider {
                                                context,
                                                family,
                                                shells: &$shells_for_semantic_nucleus,
                                                names: &$names_for_semantic_nucleus,
                                                configuration: query.configuration.clone(),
                                                substitutions: BTreeMap::new(),
                                                value_substitutions: BTreeMap::new(),
                                                deferred_value_parameters: BTreeMap::new(),
                                                anonymous_nominals: BTreeMap::new(),
                                                dependency_source: const_identity.key.clone(),
                                                dependency_kind: rue_air::DeclarationTypeDependencyKind::DeclaredType,
                                                dependencies: BTreeSet::new(),
                                                deferred_ownership: BTreeSet::new(),
                                                ownership_properties: BTreeMap::new(),
                                            };
                                    let session = crate::durable_comptime::DurableComptimeSession::new(
                                        const_identity.key.clone(),
                                        query.declaration.clone(),
                                                    )
                                    .expect("validated durable const session identity");
                                    let mut authority = DurableComptimeRootAuthority {
                                        provider,
                                        imports: $imports_for_semantic_nucleus.clone(),
                                        session,
                                        foreign: DurableComptimeForeignQueryAuthority {
                                            context,
                                            semantic_nucleus: family,
                                            declaration_body_plan_artifacts:
                                                &$artifacts_for_semantic_nucleus,
                                            configuration: &query.configuration,
                                        },
                                    };
                                    let mut frame = authority
                                        .session
                                        .admit_const_root(core.clone(), None)
                                        .expect("const root program must register once");
                                    // Resolve the declaration annotation once through the keyed
                                    // registered program.  The same owned result is used both as
                                    // AIR's expected-result hint and for the post-evaluation
                                    // declaration check; this avoids re-pairing a dense syntax
                                    // reference with a separate evaluation arena.
                                    let declared_type_resolution = declared_type.as_ref().map(|syntax| {
                                        let mut services =
                                            crate::durable_comptime::DurableComptimeServices::new(
                                                &mut authority,
                                            );
                                        services.resolve_type_syntax(&program_key, *syntax)
                                    });
                                    let expected_type = declared_type_resolution
                                        .as_ref()
                                        .and_then(|result| result.as_ref().ok())
                                        .cloned();
                                            if matches!(
                                                expected_type,
                                                Some(crate::durable_semantics::DurableType::Slice { .. })
                                            ) {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::Diagnostic(
                                                        rue_error::ErrorKind::SliceEscapesScope,
                                                    ),
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                    let core = match crate::body_query::OwnedComptimeProgramCore::finalize_imports(
                                        core,
                                        || context.check_canceled(),
                                    ) {
                                        Ok(core) => core,
                                        Err(crate::body_query::ComptimeProgramProjectionFailure::Materialization(
                                            failure,
                                        )) => match semantic_materialization_failure(failure) {
                                            Ok(failure) => {
                                                return Ok(QueryOutput::success(
                                                    Value::Failure(failure),
                                                )
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                            Err(abort) => return Err(abort),
                                        },
                                        Err(failure) => {
                                            return Ok(QueryOutput::success(Value::Failure(
                                                Failure::Resolution(Arc::from(format!(
                                                    "{failure:?}"
                                                ))),
                                            ))
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                        }
                                    };
                                    authority
                                        .session
                                        .finalize_registered_imports(&core)
                                        .expect("const root registry must retain the finalized core authority");
                                    frame.expected_result = expected_type.clone().map(Into::into);
                                    let mut env = rue_air::ComptimeEnv::<
                                        EvaluatedSemanticConst,
                                        crate::durable_comptime::DurableComptimeType,
                                        crate::durable_comptime::DurableComptimeName,
                                        crate::durable_comptime::DurableComptimeFile,
                                        crate::durable_comptime::DurableComptimeIdentity,
                                    >::new();
                                    env.canonical_identity = Some(
                                        crate::durable_comptime::DurableComptimeIdentity::from(
                                            crate::StableProducerId::Definition(
                                                const_identity.key.clone(),
                                            ),
                                        ),
                                    );
                                    let (result, provider) = {
                                        let outcome = evaluate_durable_comptime_root(
                                            &mut authority,
                                            frame,
                                            env,
                                        );
                                        let provider = authority.finish_root();
                                        let result = match durable_comptime_root_result(outcome) {
                                            Ok(result) => result,
                                            Err(failure) => return Ok(QueryOutput::failure(failure)),
                                        };
                                        (result, provider)
                                    };
                                            match result {
                                                Ok(EvaluatedSemanticConst::Module(target)) => {
                                                    if declared_type.is_some() {
                                                        Value::Failure(Failure::Resolution(
                                                            Arc::from(
                                                                "module binding cannot have a type annotation",
                                                            ),
                                                        ))
                                                    } else {
                                                        let identity = crate::semantic_query_nucleus::classified_const_identity(shell, true);
                                                        Value::ConstResolution(
                                                            crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                                                                key: identity.key,
                                                                target,
                                                            },
                                                        )
                                                    }
                                                }
                                                Ok(EvaluatedSemanticConst::Value(typed)) => {
                                                    let typed = Arc::unwrap_or_clone(typed);
                                                    let value = typed.value;
                                                    let resolved_type = match declared_type_resolution {
                                                        Some(Ok(ty)) => Ok(ty),
                                                        Some(Err(error)) => Err(error),
                                                        None if matches!(
                                                            value,
                                                            crate::durable_semantics::DurableConstValue::Type(_)
                                                                | crate::durable_semantics::DurableConstValue::Function(_)
                                                        ) => Ok(crate::durable_semantics::DurableType::ComptimeType),
                                                        None => {
                                                            let inferred = inferred_const_type_name(&value);
                                                            return Ok(QueryOutput::success(Value::Failure(
                                                                Failure::DiagnosticWithHelp {
                                                                    kind: rue_error::ErrorKind::ConstMissingTypeAnnotation {
                                                                        name: query.declaration.name.to_string(),
                                                                    },
                                                                    help: Arc::from(format!(
                                                                        "add a type annotation: `const {}: {} = ...;`",
                                                                        query.declaration.name,
                                                                        inferred,
                                                                    )),
                                                                },
                                                            )).with_terminal_kind(QueryTerminalKind::Failure));
                                                        }
                                                    };
                                                    match resolved_type {
                                                        Err(rue_air::SemanticResolutionError::ProviderAbort(abort)) => return Err(abort),
                                                        Err(error) => match semantic_type_query_failure(error) {
                                                            ResolveSemanticSignatureError::Abort(abort) => return Err(abort),
                                                            ResolveSemanticSignatureError::Failure(failure) => Value::Failure(*failure),
                                                        },
                                                        Ok(ty) => {
                                                            let compatible = typed.ty.as_ref().is_none_or(|found| {
                                                                found == &ty
                                                                    || (matches!(found, crate::durable_semantics::DurableType::ComptimeFloat)
                                                                        && matches!(ty, crate::durable_semantics::DurableType::F32 | crate::durable_semantics::DurableType::F64))
                                                            })
                                                                && match (&ty, &value) {
                                                                (crate::durable_semantics::DurableType::I8, crate::durable_semantics::DurableConstValue::Integer(value)) => i8::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::I16, crate::durable_semantics::DurableConstValue::Integer(value)) => i16::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::I32, crate::durable_semantics::DurableConstValue::Integer(value)) => i32::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::I64, crate::durable_semantics::DurableConstValue::Integer(value)) => i64::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::U8, crate::durable_semantics::DurableConstValue::Integer(value)) => u8::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::U16, crate::durable_semantics::DurableConstValue::Integer(value)) => u16::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::U32, crate::durable_semantics::DurableConstValue::Integer(value)) => u32::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::U64, crate::durable_semantics::DurableConstValue::Integer(value)) => u64::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::Bool, crate::durable_semantics::DurableConstValue::Bool(_))
                                                                | (crate::durable_semantics::DurableType::Unit, crate::durable_semantics::DurableConstValue::Unit)
                                                                | (crate::durable_semantics::DurableType::ComptimeFloat, crate::durable_semantics::DurableConstValue::Float(_))
                                                                | (crate::durable_semantics::DurableType::ComptimeType, crate::durable_semantics::DurableConstValue::Type(_) | crate::durable_semantics::DurableConstValue::Function(_)) => true,
                                                                (crate::durable_semantics::DurableType::F32, crate::durable_semantics::DurableConstValue::Float(value)) => rue_air::finite_float_literal_bits(value, rue_air::Type::F32).is_some(),
                                                                (crate::durable_semantics::DurableType::F64, crate::durable_semantics::DurableConstValue::Float(value)) => rue_air::finite_float_literal_bits(value, rue_air::Type::F64).is_some(),
                                                                (crate::durable_semantics::DurableType::BuiltinNominal { name, .. }, crate::durable_semantics::DurableConstValue::String(_)) if name.as_ref() == "str" => true,
                                                                _ => false,
                                                            };
                                                            if compatible {
                                                                let identity = crate::semantic_query_nucleus::classified_const_identity(shell, false);
                                                                Value::ConstResolution(crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                                                                    key: identity.key,
                                                                    ty,
                                                                    value: Box::new(value),
                                                                    anonymous_nominals: provider
                                                                        .anonymous_nominals
                                                                        .values()
                                                                        .cloned()
                                                                        .collect::<Vec<_>>()
                                                                        .into(),
                                                                    dependencies: provider
                                                                        .dependencies
                                                                        .iter()
                                                                        .cloned()
                                                                        .collect::<Vec<_>>()
                                                                        .into(),
                                                                    deferred_ownership: provider
                                                                        .deferred_ownership
                                                                        .iter()
                                                                        .cloned()
                                                                        .collect::<Vec<_>>()
                                                                        .into(),
                                                                })
                                                            } else {
                                                                let kind = match (&ty, &value) {
                                                                    (crate::durable_semantics::DurableType::I8
                                                                    | crate::durable_semantics::DurableType::I16
                                                                    | crate::durable_semantics::DurableType::I32
                                                                    | crate::durable_semantics::DurableType::I64
                                                                    | crate::durable_semantics::DurableType::U8
                                                                    | crate::durable_semantics::DurableType::U16
                                                                    | crate::durable_semantics::DurableType::U32
                                                                    | crate::durable_semantics::DurableType::U64,
                                                                    crate::durable_semantics::DurableConstValue::Integer(value)) if *value >= 0 => {
                                                                        rue_error::ErrorKind::LiteralOutOfRange {
                                                                            value: *value as u64,
                                                                            ty: durable_type_diagnostic_name(&ty),
                                                                        }
                                                                    }
                                                                    (crate::durable_semantics::DurableType::I8
                                                                    | crate::durable_semantics::DurableType::I16
                                                                    | crate::durable_semantics::DurableType::I32
                                                                    | crate::durable_semantics::DurableType::I64
                                                                    | crate::durable_semantics::DurableType::U8
                                                                    | crate::durable_semantics::DurableType::U16
                                                                    | crate::durable_semantics::DurableType::U32
                                                                    | crate::durable_semantics::DurableType::U64,
                                                                    crate::durable_semantics::DurableConstValue::Integer(value)) => {
                                                                        rue_error::ErrorKind::ComptimeEvaluationFailed {
                                                                            reason: format!(
                                                                                "value {value} is out of range for type {}",
                                                                                durable_type_diagnostic_name(&ty),
                                                                            ),
                                                                        }
                                                                    }
                                                                    _ => rue_error::ErrorKind::TypeMismatch {
                                                                        expected: durable_type_diagnostic_name(&ty),
                                                                        found: inferred_const_type_name(&value).to_owned(),
                                                                    },
                                                                };
                                                                Value::Failure(Failure::Diagnostic(kind))
                                                            }
                                                        }
                                                    }
                                                }
                                                Ok(EvaluatedSemanticConst::TargetEnum(_)) => {
                                                    Value::Failure(Failure::Resolution(Arc::from(
                                                        "target descriptor must be reduced by a declaration-time branch",
                                                    )))
                                                }
                                                Err(EvaluateSemanticConstError::Failure(failure)) => Value::Failure(*failure),
                                                Err(EvaluateSemanticConstError::Abort(QueryAbort::Cycle(nodes))) => {
                                                    Value::Failure(Failure::Cycle(
                                                        semantic_nucleus_cycle_names(&nodes),
                                                    ))
                                                }
                                                Err(EvaluateSemanticConstError::Abort(abort)) => return Err(abort),
                                            }
                                        }
                                    }
                                }
                        Key::AnonymousNominal(query) => {
                            let projected: Result<
                                Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
                                Failure,
                            > = match &query.identity.producer {
                                crate::StableProducerId::Definition(key) => {
                                    if declaration_candidate_for_stable_key(key).as_ref()
                                        != Some(&query.producer.declaration)
                                    {
                                        Err(Failure::Resolution(Arc::from(
                                            "anonymous nominal producer identity mismatch",
                                        )))
                                    } else {
                                        let resolved = context.query_registered(
                                            family,
                                            Key::ConstResolution(query.producer.clone()),
                                        )?;
                                        let rue_query::QueryOutcome::Success(resolved) =
                                            resolved.outcome()
                                        else {
                                            unreachable!("SemanticNucleus publishes typed values")
                                        };
                                        match resolved {
                                            Value::ConstResolution(
                                                crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                                                    anonymous_nominals,
                                                    ..
                                                },
                                            ) => Ok(anonymous_nominals.clone()),
                                            Value::Failure(failure) => Err(failure.clone()),
                                            _ => Err(Failure::Resolution(Arc::from(
                                                "anonymous nominal const producer returned the wrong projection",
                                            ))),
                                        }
                                    }
                                }
                                crate::StableProducerId::Function(function) => {
                                    let Some(key) = function_definition_key(function) else {
                                        let value = Value::Failure(Failure::Resolution(Arc::from(
                                            "anonymous nominal has an unsupported function producer",
                                        )));
                                        return Ok(QueryOutput::success(value)
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                    };
                                    if declaration_candidate_for_stable_key(key).as_ref()
                                        != Some(&query.producer.declaration)
                                    {
                                        Err(Failure::Resolution(Arc::from(
                                            "anonymous nominal producer identity mismatch",
                                        )))
                                    } else {
                                        let producer = context.query_registered(
                                            &$produced_anonymous_for_semantic_nucleus,
                                            crate::body_query::BodyQueryKey::new(
                                                (**function).clone(),
                                                query.producer.configuration.clone(),
                                            ),
                                        )?;
                                        let rue_query::QueryOutcome::Success(producer) =
                                            producer.outcome()
                                        else {
                                            unreachable!(
                                                "BodyProducedAnonymous publishes typed values"
                                            )
                                        };
                                        match producer {
                                            crate::body_query::ProducedAnonymous::Produced(
                                                produced,
                                            ) => Ok(produced.0.clone()),
                                            // The producer committed an
                                            // anchor-transport internal error;
                                            // fail closed rather than rescue the
                                            // identity (RUE-1089).
                                            crate::body_query::ProducedAnonymous::ProducerFailed(
                                                failure,
                                            ) => Err((**failure).clone()),
                                        }
                                    }
                                }
                            };
                            match projected {
                                Ok(projected) => {
                                    // Producer-nominal identity is exact: the
                                    // producer publishes this precise anchor
                                    // (transported from the frontend), so an exact
                                    // identity match is the only resolution.
                                    projected
                                        .iter()
                                        .find(|nominal| nominal.identity == query.identity)
                                        .cloned()
                                        .map(Value::AnonymousNominal)
                                        .unwrap_or_else(|| {
                                            Value::Failure(Failure::Resolution(Arc::from(
                                                "anonymous nominal producer did not publish the requested identity",
                                            )))
                                        })
                                }
                                Err(failure) => Value::Failure(failure),
                            }
                        }
                        Key::ComptimeCall(call) => {
                            let artifact = context.query_registered(
                                &$artifacts_for_semantic_nucleus,
                                DeclarationBodyPlanQueryKey(
                                    call.declaration.declaration.clone(),
                                ),
                            )?;
                            let rue_query::QueryOutcome::Success(artifact) = artifact.outcome()
                            else {
                                unreachable!("DeclarationBodyPlanArtifacts publishes typed values")
                            };
                            match artifact {
                                DeclarationBodyPlanArtifactsValue::Failure(failure) => {
                                    Value::Failure(candidate_rir_semantic_failure(failure))
                                }
                                DeclarationBodyPlanArtifactsValue::Available(artifact) => {
                                    let producer_key = crate::semantic_query_nucleus::direct_identity(shell)
                                        .expect("comptime call shell is callable")
                                        .key;
                                    let program_key = crate::body_query::DurableComptimeProgramKey {
                                        declaration: producer_key.clone(),
                                        configuration: call.declaration.configuration.clone(),
                                    };
                                    let core = match crate::body_query::OwnedComptimeProgramCore::from_callable_body_plan_without_imports(
                                        crate::body_query::DurableComptimeProgramPlan {
                                            key: program_key.clone(),
                                            candidate: call.declaration.declaration.clone(),
                                        },
                                        artifact,
                                        || context.check_canceled(),
                                    ) {
                                        Ok(core) => core,
                                        Err(crate::body_query::ComptimeProgramProjectionFailure::NotFunction { .. }) => {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::Resolution(Arc::from(
                                                        "comptime candidate artifact has a non-function root",
                                                    )),
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                        }
                                        Err(crate::body_query::ComptimeProgramProjectionFailure::Materialization(
                                            failure,
                                        )) => match semantic_materialization_failure(failure) {
                                            Ok(failure) => {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    failure,
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                            Err(abort) => return Err(abort),
                                        },
                                        Err(failure) => {
                                            return Ok(QueryOutput::success(Value::Failure(
                                                Failure::Resolution(Arc::from(format!("{failure:?}"))),
                                            ))
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                        }
                                    };
                                    let Some(body) = core.callable().map(|callable| callable.body) else {
                                        unreachable!("callable core validated its root kind");
                                            };
                                            let signature = context.query_registered(
                                                family,
                                                Key::Signature(call.declaration.clone()),
                                            )?;
                                            let rue_query::QueryOutcome::Success(signature) =
                                                signature.outcome()
                                            else {
                                                unreachable!("SemanticNucleus publishes typed values")
                                            };
                                            let Value::Signature(signature) = signature else {
                                                let Value::Failure(failure) = signature else {
                                                    unreachable!("signature query returned the wrong projection")
                                                };
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    failure.clone(),
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            };
                                            let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                                                parameters: callable_parameters,
                                                result: callable_result,
                                                ..
                                            } = &signature.signature else {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::Resolution(Arc::from(
                                                        "comptime call target is not callable",
                                                    )),
                                                )).with_terminal_kind(QueryTerminalKind::Failure));
                                            };
                                            let concrete_type_arguments = call
                                                .type_arguments
                                                .iter()
                                                .map(|(_, ty)| ty.clone())
                                                .collect::<Vec<_>>();
                                            let value_parameter_types = callable_parameters
                                                .iter()
                                                .filter(|parameter| {
                                                    parameter.ty
                                                        != crate::durable_semantics::DurableType::ComptimeType
                                                })
                                                .map(|parameter| {
                                                    substitute_durable_generics(
                                                        &parameter.ty,
                                                        &concrete_type_arguments,
                                                    )
                                                })
                                                .collect::<Vec<_>>();
                                            let expected_type = substitute_durable_generics(
                                                callable_result,
                                                &concrete_type_arguments,
                                            );
                                            let substitutions = call
                                                .type_arguments
                                                .iter()
                                                .cloned()
                                                .collect::<BTreeMap<_, _>>();
                                            let value_substitutions = call
                                                .value_arguments
                                                .iter()
                                                .cloned()
                                                .collect::<BTreeMap<_, _>>();
                                            let producer = crate::durable_comptime::canonical_specialized_function_producer(
                                                &producer_key,
                                                &call.type_arguments,
                                                &call.value_arguments,
                                            )
                                            .expect("durable type/value arguments have canonical identities");
                                            let mut anonymous_dependencies = BTreeSet::new();
                                            for (_, ty) in call.type_arguments.iter() {
                                                collect_anonymous_nominal_type_dependencies(
                                                    ty,
                                                    &mut anonymous_dependencies,
                                                );
                                            }
                                            for (_, value) in call.value_arguments.iter() {
                                                collect_anonymous_nominal_value_dependencies(
                                                    value,
                                                    &mut anonymous_dependencies,
                                                );
                                            }
                                            let mut anonymous_nominals = BTreeMap::new();
                                            for identity in anonymous_dependencies {
                                                let Some(dependency) = anonymous_nominal_query_key(
                                                    &identity,
                                                    &call.declaration.configuration,
                                                ) else {
                                                    return Ok(QueryOutput::success(Value::Failure(
                                                        Failure::Resolution(Arc::from(
                                                            "anonymous nominal argument has an unsupported producer",
                                                        )),
                                                    ))
                                                    .with_terminal_kind(QueryTerminalKind::Failure));
                                                };
                                                let dependency = context.query_registered(
                                                    family,
                                                    Key::AnonymousNominal(dependency),
                                                )?;
                                                let rue_query::QueryOutcome::Success(dependency) =
                                                    dependency.outcome()
                                                else {
                                                    unreachable!("SemanticNucleus publishes typed values")
                                                };
                                                match dependency {
                                                    Value::AnonymousNominal(value) => {
                                                        if let Err(identity) =
                                                            crate::durable_semantics::merge_anonymous_nominal(
                                                                &mut anonymous_nominals,
                                                                value,
                                                            )
                                                        {
                                                            return Ok(QueryOutput::success(
                                                                Value::Failure(Failure::Resolution(
                                                                    Arc::from(format!(
                                                                        "conflicting durable anonymous facts for {identity:?}"
                                                                    )),
                                                                )),
                                                            )
                                                            .with_terminal_kind(
                                                                QueryTerminalKind::Failure,
                                                            ));
                                                        }
                                                    }
                                                    Value::Failure(failure) => {
                                                        return Ok(QueryOutput::success(
                                                            Value::Failure(failure.clone()),
                                                        )
                                                        .with_terminal_kind(
                                                            QueryTerminalKind::Failure,
                                                        ));
                                                    }
                                                    _ => {
                                                        return Ok(QueryOutput::success(
                                                            Value::Failure(Failure::Resolution(
                                                                Arc::from(
                                                                    "anonymous nominal dependency returned the wrong projection",
                                                                ),
                                                            )),
                                                        )
                                                        .with_terminal_kind(
                                                            QueryTerminalKind::Failure,
                                                        ));
                                                    }
                                                }
                                            }
                                            let provider = SemanticNucleusTypeProvider {
                                                context,
                                                family,
                                                shells: &$shells_for_semantic_nucleus,
                                                names: &$names_for_semantic_nucleus,
                                                configuration: call
                                                    .declaration
                                                    .configuration
                                                    .clone(),
                                                substitutions: substitutions.clone(),
                                                value_substitutions: value_substitutions.clone(),
                                                deferred_value_parameters: BTreeMap::new(),
                                                anonymous_nominals,
                                                dependency_source: producer_key.clone(),
                                                dependency_kind: rue_air::DeclarationTypeDependencyKind::Body,
                                                dependencies: BTreeSet::new(),
                                                deferred_ownership: BTreeSet::new(),
                                                ownership_properties: BTreeMap::new(),
                                            };
                                            let session = crate::durable_comptime::DurableComptimeSession::new(
                                                producer_key.clone(),
                                                call.declaration.declaration.clone(),
                                            )
                                            .expect("validated durable call session identity");
                                            let mut locals = BTreeMap::new();
                                            locals.extend(call.type_arguments.iter().map(
                                                |(name, value)| {
                                                    (
                                                        name.clone(),
                                                        EvaluatedSemanticConst::Value(
                                                            TypedSemanticConst::typed(
                                                                crate::durable_semantics::DurableConstValue::Type(value.clone()),
                                                                crate::durable_semantics::DurableType::ComptimeType,
                                                            ),
                                                        ),
                                                    )
                                                },
                                            ));
                                            locals.extend(call.value_arguments.iter().zip(value_parameter_types.iter()).map(
                                                |((name, value), ty)| {
                                                    (
                                                        name.clone(),
                                                        EvaluatedSemanticConst::Value(
                                                            TypedSemanticConst::typed(value.clone(), ty.clone()),
                                                        ),
                                                    )
                                                },
                                            ));
                                    let core = match crate::body_query::OwnedComptimeProgramCore::finalize_imports(
                                        core,
                                        || context.check_canceled(),
                                    ) {
                                        Ok(core) => core,
                                        Err(crate::body_query::ComptimeProgramProjectionFailure::Materialization(
                                            failure,
                                        )) => match semantic_materialization_failure(failure) {
                                            Ok(failure) => {
                                                return Ok(QueryOutput::success(
                                                    Value::Failure(failure),
                                                )
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                            Err(abort) => return Err(abort),
                                        },
                                        Err(failure) => {
                                            return Ok(QueryOutput::success(Value::Failure(
                                                Failure::Resolution(Arc::from(format!(
                                                    "{failure:?}"
                                                ))),
                                            ))
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                        }
                                    };
                                            let (result, provider) = {
                                                let mut authority = DurableComptimeRootAuthority {
                                                    provider,
                                                    imports: $imports_for_semantic_nucleus.clone(),
                                                    session,
                                                    foreign: DurableComptimeForeignQueryAuthority {
                                                        context,
                                                        semantic_nucleus: family,
                                                        declaration_body_plan_artifacts:
                                                            &$artifacts_for_semantic_nucleus,
                                                        configuration: &call.declaration.configuration,
                                                    },
                                                };
                                        authority
                                            .session
                                            .register_program(&core)
                                            .expect("callable program must register once");
                                                let root_identity = crate::durable_comptime::DurableComptimeIdentity::from(
                                                    producer,
                                                );
                                                let mut frame = rue_air::ComptimeFrame::callable_body(
                                                    program_key.clone(),
                                                    body,
                                                    root_identity.clone(),
                                                );
                                                frame.context = authority
                                                    .session
                                                    .file_for_program(&program_key)
                                                    .ok();
                                                frame.expected_result =
                                                    Some(expected_type.clone().into());
                                                let mut env = rue_air::ComptimeEnv::<
                                                    EvaluatedSemanticConst,
                                                    crate::durable_comptime::DurableComptimeType,
                                                    crate::durable_comptime::DurableComptimeName,
                                                    crate::durable_comptime::DurableComptimeFile,
                                                    crate::durable_comptime::DurableComptimeIdentity,
                                                >::new();
                                                env.type_subst = substitutions
                                                    .iter()
                                                    .map(|(name, ty)| {
                                                        (
                                                            crate::durable_comptime::DurableComptimeName::from(
                                                                name.clone(),
                                                            ),
                                                            crate::durable_comptime::DurableComptimeType(
                                                                ty.clone(),
                                                            ),
                                                        )
                                                    })
                                                    .collect();
                                                env.value_subst = value_substitutions
                                                    .iter()
                                                    .zip(value_parameter_types.iter())
                                                    .map(|((name, value), ty)| {
                                                        (
                                                            crate::durable_comptime::DurableComptimeName::from(
                                                                name.clone(),
                                                            ),
                                                            EvaluatedSemanticConst::Value(
                                                                TypedSemanticConst::typed(
                                                                    value.clone(),
                                                                    ty.clone(),
                                                                ),
                                                            ),
                                                        )
                                                    })
                                                    .collect();
                                                env.locals = locals
                                                    .into_iter()
                                                    .map(|(name, value)| {
                                                        (
                                                            crate::durable_comptime::DurableComptimeName::from(
                                                                name,
                                                            ),
                                                            value,
                                                        )
                                                    })
                                                    .collect();
                                                env.canonical_identity = Some(root_identity);
                                                let outcome = evaluate_durable_comptime_root(
                                                    &mut authority,
                                                    frame,
                                                    env,
                                                );
                                                let provider = authority.finish_root();
                                                let result = match durable_comptime_root_result(outcome) {
                                                    Ok(result) => result,
                                                    Err(failure) => return Ok(QueryOutput::failure(failure)),
                                                };
                                                (result, provider)
                                            };
                                            match result {
                                                Ok(EvaluatedSemanticConst::Value(value))
                                                    if matches!(value.value, crate::durable_semantics::DurableConstValue::Type(_)) =>
                                                {
                                                    let crate::durable_semantics::DurableConstValue::Type(ty) = &value.value else {
                                                        unreachable!()
                                                    };
                                                    Value::ComptimeCall(
                                                    crate::semantic_query_nucleus::ComptimeCallProjection {
                                                        result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Type(ty.clone()),
                                                        anonymous_nominals: provider
                                                            .anonymous_nominals
                                                            .values()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                        dependencies: provider
                                                            .dependencies
                                                            .iter()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                        deferred_ownership: provider
                                                            .deferred_ownership
                                                            .iter()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                    },
                                                )
                                                }
                                                Ok(EvaluatedSemanticConst::Value(value)) => {
                                                    let value = Arc::unwrap_or_clone(value);
                                                    Value::ComptimeCall(
                                                        crate::semantic_query_nucleus::ComptimeCallProjection {
                                                            result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(value.value),
                                                            anonymous_nominals: provider
                                                                .anonymous_nominals
                                                                .values()
                                                                .cloned()
                                                                .collect::<Vec<_>>()
                                                                .into(),
                                                            dependencies: provider
                                                                .dependencies
                                                                .iter()
                                                                .cloned()
                                                                .collect::<Vec<_>>()
                                                                .into(),
                                                            deferred_ownership: provider
                                                                .deferred_ownership
                                                                .iter()
                                                                .cloned()
                                                                .collect::<Vec<_>>()
                                                                .into(),
                                                        },
                                                    )
                                                }
                                                Ok(EvaluatedSemanticConst::Module(_)) => {
                                                    Value::Failure(Failure::Resolution(Arc::from(
                                                        "comptime function returned a module",
                                                    )))
                                                }
                                                Ok(EvaluatedSemanticConst::TargetEnum(_)) => {
                                                    Value::Failure(Failure::Resolution(Arc::from(
                                                        "comptime function returned an unreduced target descriptor",
                                                    )))
                                                }
                                                Err(EvaluateSemanticConstError::Failure(failure)) => {
                                                    Value::Failure(*failure)
                                                }
                                                Err(EvaluateSemanticConstError::Abort(
                                                    QueryAbort::Cycle(nodes),
                                                )) => Value::Failure(Failure::Cycle(
                                                    semantic_nucleus_cycle_names(&nodes),
                                                )),
                                                Err(EvaluateSemanticConstError::Abort(abort)) => {
                                                    return Err(abort)
                                                }
                                            }
                                        }
                                    }
                                }
                    };
                    let kind = if matches!(value, Value::Failure(_)) {
                        QueryTerminalKind::Failure
                    } else {
                        QueryTerminalKind::Success
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the SemanticNucleus family has one canonical name")
    }};
}
