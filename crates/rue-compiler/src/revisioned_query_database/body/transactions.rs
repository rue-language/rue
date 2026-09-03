//! Canonical body-transaction evaluation and publication.
//!
//! This module owns transaction control/failure classification, evaluation,
//! rollback handoffs, and the database methods that request exact body
//! projections. Closure and durable-comptime algorithms remain separate.

use super::super::*;

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum BodyTransactionRequestFailure {
    Query(QueryAbort),
    DeferredAnonymousProducers(Arc<[crate::FunctionInstanceKey]>),
    /// An anonymous producer this body depends on committed a deterministic
    /// semantic failure. The dependent body cannot be built and must fail
    /// closed; the failure is never retried or reclassified as cancellation.
    ProducerFailed(Box<crate::semantic_query_nucleus::SemanticNucleusFailure>),
    /// One exact trusted `Option(payload)` specialization failed before body
    /// analysis. No partial registry or body terminal is published.
    WellKnownOptionResolution(WellKnownOptionResolutionFailure),
}
pub(crate) use crate::body_query::WellKnownOptionResolutionFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::revisioned_query_database) enum WellKnownDependencyAbortClass {
    Incomplete,
    Propagate,
}

pub(in crate::revisioned_query_database) fn classify_well_known_dependency_abort(
    abort: &QueryAbort,
) -> WellKnownDependencyAbortClass {
    match abort {
        QueryAbort::Canceled | QueryAbort::MissingInput(_) => {
            WellKnownDependencyAbortClass::Incomplete
        }
        QueryAbort::Cycle(_) | QueryAbort::ForeignRuntime | QueryAbort::UnpublishedRevision(_) => {
            WellKnownDependencyAbortClass::Propagate
        }
    }
}

pub(in crate::revisioned_query_database) struct BodyTransactionEvaluator {
    pub(in crate::revisioned_query_database) parse_modules:
        QueryFamily<ModuleQueryKey, ParseModuleValue>,
    pub(crate) module_source_bases:
        QueryFamily<ModuleQueryKey, Option<rue_air::DurableBodySourceLocator>>,
    pub(in crate::revisioned_query_database) body_input: BodyInputResolver,
    pub(in crate::revisioned_query_database) body_toolchain_demands:
        QueryFamily<crate::body_query::BodyQueryKey, crate::BodyToolchainDemand>,
    pub(in crate::revisioned_query_database) body_produced_anonymous:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::ProducedAnonymous>,
    pub(in crate::revisioned_query_database) semantic_nucleus: SemanticNucleusFamily,
    pub(in crate::revisioned_query_database) stable_declaration_classifications: QueryFamily<
        StableDeclarationClassificationQueryKey,
        StableDeclarationClassificationQueryValue,
    >,
    pub(crate) declaration_shells:
        QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    pub(in crate::revisioned_query_database) lookup_names:
        QueryFamily<LookupNameKey, LookupNameValue>,
    pub(in crate::revisioned_query_database) lookup_imports:
        QueryFamily<LookupImportKey, LookupImportValue>,
    pub(in crate::revisioned_query_database) provider_observation_meter:
        Arc<ProviderObservationCounters>,
    pub(in crate::revisioned_query_database) lookup_root_lease:
        Arc<Mutex<PublishedRootLookupLease>>,
    pub(in crate::revisioned_query_database) runtime: QueryRuntime,
    pub(in crate::revisioned_query_database) shared_durable_payloads:
        Arc<SharedDurablePayloadCache>,
    /// The ADR-0076 revision-shared symbol space. Every body of one semantic
    /// revision decodes its RIR into, and analyzes against, one append-only
    /// interner, so the program's nominal closure is interned once per
    /// revision instead of once per body.
    pub(in crate::revisioned_query_database) symbol_space: RevisionSymbolSpace,
    #[cfg(test)]
    pub(in crate::revisioned_query_database) inject_body_transaction_failure:
        Arc<std::sync::atomic::AtomicBool>,
}

impl BodyTransactionEvaluator {
    fn body_plan_failure(
        definition: &crate::StableDefinitionKey,
        failure: &DeclarationBodyPlanFailure,
    ) -> crate::body_query::BodyTransaction {
        let errors = match failure {
            DeclarationBodyPlanFailure::CandidateRirRejected(errors) => errors.clone(),
            DeclarationBodyPlanFailure::Build(kind) => {
                crate::CompileErrors::from(crate::CompileError::without_span(kind.clone()))
            }
            failure => crate::CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InternalError(format!(
                    "canonical body plan failed for {definition:?}: {failure:?}"
                )),
            )),
        };
        crate::body_query::BodyTransaction::DeterministicFailure {
            diagnostic_basis: None,
            errors,
            references: crate::body_query::BodyReferences(Arc::from([])),
            lookup_observations: crate::body_query::BodyLookupObservations::default(),
        }
    }

    fn lowering_failure(
        definition: &crate::StableDefinitionKey,
        detail: impl std::fmt::Display,
    ) -> crate::body_query::BodyTransaction {
        crate::body_query::BodyTransaction::DeterministicFailure {
            diagnostic_basis: None,
            errors: crate::CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InternalError(format!(
                    "owned body input lowering failed for {definition:?}: {detail}"
                )),
            )),
            references: crate::body_query::BodyReferences(Arc::from([])),
            lookup_observations: crate::body_query::BodyLookupObservations::default(),
        }
    }

    fn lowering_build_failure(
        error: &rue_rir::RirPayloadBuildError,
    ) -> crate::body_query::BodyTransaction {
        crate::body_query::BodyTransaction::DeterministicFailure {
            diagnostic_basis: None,
            errors: crate::CompileErrors::from(crate::CompileError::without_span(
                crate::canonical_lower::rir_build_error_kind("packed body materialization", error),
            )),
            references: crate::body_query::BodyReferences(Arc::from([])),
            lookup_observations: crate::body_query::BodyLookupObservations::default(),
        }
    }

    fn compiler_body_provider_queries<'a>(
        &self,
        context: &'a rue_query::QueryContext,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
        observed: std::rc::Rc<std::cell::RefCell<ObservedLookupRoot>>,
        positive_references: std::rc::Rc<
            std::cell::RefCell<BTreeSet<crate::body_query::BodyReference>>,
        >,
    ) -> CompilerBodyProviderQueries<'a> {
        CompilerBodyProviderQueries {
            context,
            parse_modules: self.parse_modules.clone(),
            module_source_bases: self.module_source_bases.clone(),
            lookup_names: self.lookup_names.clone(),
            lookup_imports: self.lookup_imports.clone(),
            declaration_body_plan_artifacts: self
                .body_input
                .declaration_body_plan_artifacts
                .clone(),
            semantic_nucleus: self.semantic_nucleus.clone(),
            body_produced_anonymous: self.body_produced_anonymous.clone(),
            body_toolchain_demands: self.body_toolchain_demands.clone(),
            configuration,
            status: std::rc::Rc::new(std::cell::RefCell::new(CompilerBodyProviderStatus::Ready)),
            deferred_anonymous_producers: std::rc::Rc::new(
                std::cell::RefCell::new(BTreeSet::new()),
            ),
            producer_transport_failure: std::rc::Rc::new(std::cell::RefCell::new(None)),
            observed,
            positive_references,
            meter: self.provider_observation_meter.clone(),
            shared_durable_payloads: self.shared_durable_payloads.clone(),
        }
    }

    pub(in crate::revisioned_query_database) fn evaluate(
        &self,
        context: &rue_query::QueryContext,
        key: &crate::body_query::BodyQueryKey,
    ) -> Result<QueryOutput<crate::body_query::BodyTransaction>, QueryAbort> {
        #[cfg(test)]
        if self
            .inject_body_transaction_failure
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(QueryOutput::success(
                crate::body_query::BodyTransaction::DeterministicFailure {
                    diagnostic_basis: None,
                    errors: crate::CompileErrors::from(crate::CompileError::without_span(
                        rue_error::ErrorKind::InternalError(format!(
                            "injected_body_transaction_failure for body instance {:?}",
                            key.instance
                        )),
                    )),
                    references: crate::body_query::BodyReferences(Arc::from([])),
                    lookup_observations: crate::body_query::BodyLookupObservations::default(),
                },
            )
            .with_terminal_kind(QueryTerminalKind::Failure));
        }
        let definition = body_source_definition_key(&key.instance)
            .cloned()
            .ok_or(QueryAbort::Canceled)?;
        let candidate =
            declaration_candidate_for_stable_key(&definition).ok_or(QueryAbort::Canceled)?;
        let deferred_anonymous_producers =
            std::rc::Rc::new(std::cell::RefCell::new(BTreeSet::new()));
        // Set when a depended-on anonymous producer committed a deterministic
        // semantic failure. It is carried out of the query closure — which can
        // only signal a bare `QueryAbort` — and mapped to typed `ProducerFailed`
        // control at the request boundary.
        let producer_transport_failure = std::rc::Rc::new(std::cell::RefCell::new(None));
        let well_known_resolution_failure: std::cell::RefCell<
            Option<WellKnownOptionResolutionFailure>,
        > = std::cell::RefCell::new(None);
        let observed = std::rc::Rc::new(std::cell::RefCell::new(ObservedLookupRoot::new()));
        let positive_references = std::rc::Rc::new(std::cell::RefCell::new(BTreeSet::new()));
        // Materialization work is accumulated only until the body transaction
        // reaches a successful terminal. Failed and canceled body attempts
        // therefore never publish structural lowering work.
        let materialization_work =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::<WorkItem>::new()));
        let result = (|| {
            // The transaction's prerequisite fan-out (toolchain demand/artifact
            // authority, transitive anonymous nominals, well-known `Option`
            // resolution) and its trailing lookup-edge recording both run
            // per reached body around the analysis itself. They are timed
            // separately so prerequisite work remains visible beside the
            // body-analysis computation (RUE-786).
            let prerequisites_span =
                tracing::info_span!("body_query_prerequisites", phase = "semantic_analysis")
                    .entered();
            // Observe THIS body's exact fallible-intrinsic payload set from the
            // registered `body-toolchain-demands` node — the ONE canonical
            // typed intrinsic-set and source-candidate authority (RUE-1112 C1)
            // — instead of rescanning or adding a duplicate source edge.
            // A body roots only the payloads it uses, so an unrelated body gains
            // no query edge to payloads it does not use.
            let toolchain_demand =
                context.query_registered(&self.body_toolchain_demands, key.clone())?;
            let rue_query::QueryOutcome::Success(toolchain_demand) = toolchain_demand.outcome()
            else {
                unreachable!("BodyToolchainDemands publishes typed values")
            };
            if !matches!(
                key.instance,
                crate::FunctionInstanceKey::AnonymousMember { .. }
            ) && !toolchain_demand.source_candidate_available()
            {
                return Err(QueryAbort::Canceled);
            }
            let body_payload_kinds = toolchain_demand.payload_kinds();
            let mut selected_anonymous = BTreeMap::new();
            let mut pending_anonymous = collect_instance_anonymous_nominals(&key.instance);
            let canonical_instance = key.instance.with_collapsed_empty_specializations();
            while let Some(identity) = pending_anonymous.pop_first() {
                let identity = identity.with_canonical_producer().into_owned();
                if let crate::StableProducerId::Function(function) = &identity.producer
                    && function.as_ref() != canonical_instance.as_ref()
                {
                    let produced = match context.query_registered(
                        &self.body_produced_anonymous,
                        crate::body_query::BodyQueryKey::new(
                            (**function).clone(),
                            key.configuration.clone(),
                        ),
                    ) {
                        Ok(produced) => produced,
                        Err(QueryAbort::Canceled) => {
                            deferred_anonymous_producers
                                .borrow_mut()
                                .insert((**function).clone());
                            return Err(QueryAbort::Canceled);
                        }
                        Err(abort) => return Err(abort),
                    };
                    let rue_query::QueryOutcome::Success(produced) = produced.outcome() else {
                        unreachable!("BodyProducedAnonymous publishes typed values")
                    };
                    let produced = match produced {
                        crate::body_query::ProducedAnonymous::Produced(produced) => produced,
                        crate::body_query::ProducedAnonymous::ProducerFailed(failure) => {
                            *producer_transport_failure.borrow_mut() = Some(failure.clone());
                            return Err(QueryAbort::Canceled);
                        }
                    };
                    for nominal in produced.0.iter() {
                        if let Err(identity) = crate::durable_semantics::merge_anonymous_nominal(
                            &mut selected_anonymous,
                            nominal,
                        ) {
                            *producer_transport_failure.borrow_mut() = Some(Box::new(
                                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                                    Arc::from(format!(
                                        "conflicting durable anonymous facts for {identity:?}"
                                    )),
                                ),
                            ));
                            return Err(QueryAbort::Canceled);
                        }
                    }
                }
                if !selected_anonymous.contains_key(&identity) {
                    let query = anonymous_nominal_query_key(&identity, &key.configuration)
                        .ok_or(QueryAbort::Canceled)?;
                    let nominal = context.query_registered(
                        &self.semantic_nucleus,
                        crate::semantic_query_nucleus::SemanticNucleusKey::AnonymousNominal(query),
                    )?;
                    let rue_query::QueryOutcome::Success(nominal) = nominal.outcome() else {
                        unreachable!("SemanticNucleus publishes typed values")
                    };
                    let crate::semantic_query_nucleus::SemanticNucleusValue::AnonymousNominal(
                        nominal,
                    ) = nominal
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    if let Err(identity) = crate::durable_semantics::merge_anonymous_nominal(
                        &mut selected_anonymous,
                        nominal,
                    ) {
                        *producer_transport_failure.borrow_mut() = Some(Box::new(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                                Arc::from(format!(
                                    "conflicting durable anonymous facts for {identity:?}"
                                )),
                            ),
                        ));
                        return Err(QueryAbort::Canceled);
                    }
                }
                if let Some(nominal) = selected_anonymous.get(&identity) {
                    let mut dependencies = BTreeSet::new();
                    collect_durable_anonymous_nominal_dependencies(nominal, &mut dependencies);
                    enqueue_unselected_anonymous_dependencies(
                        &selected_anonymous,
                        &mut pending_anonymous,
                        dependencies,
                    );
                }
            }
            // Resolve THIS body's exact trusted-std `Option(payload)` set
            // atomically (RUE-1112). Every key is derived directly from the
            // registered body's canonical payload kinds. The session already
            // parked any missing trusted module before entering this task, so
            // a committed semantic failure or wrong projection is fatal:
            // publish neither a partial registry nor a body terminal.
            let well_known = {
                let mut option_by_payload = Vec::new();
                let mut nominals = BTreeMap::new();
                for &payload_kind in body_payload_kinds {
                    for prerequisite in
                        crate::well_known_option::exact_option_prerequisites(payload_kind)
                    {
                        let classified = match context.query_registered(
                            &self.stable_declaration_classifications,
                            StableDeclarationClassificationQueryKey(prerequisite.stable.clone()),
                        ) {
                            Ok(classified) => classified,
                            Err(abort) => {
                                if matches!(
                                    classify_well_known_dependency_abort(&abort),
                                    WellKnownDependencyAbortClass::Propagate
                                ) {
                                    return Err(abort);
                                }
                                *well_known_resolution_failure.borrow_mut() =
                                    Some(WellKnownOptionResolutionFailure::Incomplete {
                                        payload: payload_kind,
                                        prerequisite: Some(prerequisite.stable),
                                        detail: Arc::from(format!(
                                            "exact trusted declaration prerequisite is \
                                                 unavailable: {abort:?}"
                                        )),
                                    });
                                return Err(QueryAbort::Canceled);
                            }
                        };
                        match classified.outcome() {
                            rue_query::QueryOutcome::Success(
                                StableDeclarationClassificationQueryValue::Selected(candidate),
                            ) if candidate == &prerequisite.candidate => {}
                            rue_query::QueryOutcome::Success(value) => {
                                *well_known_resolution_failure.borrow_mut() =
                                    Some(WellKnownOptionResolutionFailure::Incomplete {
                                        payload: payload_kind,
                                        prerequisite: Some(prerequisite.stable),
                                        detail: Arc::from(format!(
                                            "exact trusted declaration prerequisite did not \
                                                 select its required candidate: {value:?}"
                                        )),
                                    });
                                return Err(QueryAbort::Canceled);
                            }
                            rue_query::QueryOutcome::Failure(failure) => {
                                *well_known_resolution_failure.borrow_mut() =
                                    Some(WellKnownOptionResolutionFailure::Incomplete {
                                        payload: payload_kind,
                                        prerequisite: Some(prerequisite.stable),
                                        detail: Arc::from(format!(
                                            "exact trusted declaration prerequisite query \
                                                 failed: {failure:?}"
                                        )),
                                    });
                                return Err(QueryAbort::Canceled);
                            }
                        }
                    }
                    let (payload, call) = crate::well_known_option::exact_option_query(
                        payload_kind,
                        &key.configuration,
                    );
                    let projected = match context.query_registered(
                        &self.semantic_nucleus,
                        crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(call),
                    ) {
                        Ok(projected) => projected,
                        Err(abort) => {
                            if matches!(
                                classify_well_known_dependency_abort(&abort),
                                WellKnownDependencyAbortClass::Propagate
                            ) {
                                return Err(abort);
                            }
                            *well_known_resolution_failure.borrow_mut() =
                                Some(WellKnownOptionResolutionFailure::Incomplete {
                                    payload: payload_kind,
                                    prerequisite: None,
                                    detail: Arc::from(format!(
                                        "exact trusted Option specialization is unavailable: \
                                             {abort:?}"
                                    )),
                                });
                            return Err(QueryAbort::Canceled);
                        }
                    };
                    let projection = match projected.outcome() {
                        rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(
                                projection,
                            ),
                        ) => projection,
                        rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(failure),
                        ) => {
                            *well_known_resolution_failure.borrow_mut() =
                                Some(WellKnownOptionResolutionFailure::Semantic {
                                    payload: payload_kind,
                                    failure: Box::new(failure.clone()),
                                });
                            return Err(QueryAbort::Canceled);
                        }
                        rue_query::QueryOutcome::Success(other) => {
                            *well_known_resolution_failure.borrow_mut() =
                                Some(WellKnownOptionResolutionFailure::WrongProjection {
                                    payload: payload_kind,
                                    detail: Arc::from(format!(
                                        "expected ComptimeCall(Type), found {other:?}"
                                    )),
                                });
                            return Err(QueryAbort::Canceled);
                        }
                        rue_query::QueryOutcome::Failure(failure) => {
                            *well_known_resolution_failure.borrow_mut() = Some(
                                WellKnownOptionResolutionFailure::WrongProjection {
                                    payload: payload_kind,
                                    detail: Arc::from(format!(
                                        "expected a semantic nucleus value, found query failure {failure:?}"
                                    )),
                                },
                            );
                            return Err(QueryAbort::Canceled);
                        }
                    };
                    let crate::semantic_query_nucleus::ComptimeCallResultProjection::Type(
                        option_type,
                    ) = &projection.result
                    else {
                        *well_known_resolution_failure.borrow_mut() =
                            Some(WellKnownOptionResolutionFailure::WrongProjection {
                                payload: payload_kind,
                                detail: Arc::from(format!(
                                    "expected ComptimeCall(Type), found ComptimeCall({:?})",
                                    projection.result
                                )),
                            });
                        return Err(QueryAbort::Canceled);
                    };
                    option_by_payload.push((payload, option_type.clone()));
                    for nominal in projection.anonymous_nominals.iter() {
                        if let Err(identity) = crate::durable_semantics::merge_anonymous_nominal(
                            &mut nominals,
                            nominal,
                        ) {
                            *well_known_resolution_failure.borrow_mut() =
                                Some(WellKnownOptionResolutionFailure::WrongProjection {
                                    payload: payload_kind,
                                    detail: Arc::from(format!(
                                        "conflicting durable anonymous facts for {identity:?}"
                                    )),
                                });
                            return Err(QueryAbort::Canceled);
                        }
                    }
                }
                crate::body_query::WellKnownOptionResolution {
                    option_by_payload: Arc::from(option_by_payload),
                    anonymous_nominals: Arc::from(nominals.into_values().collect::<Vec<_>>()),
                }
            };
            let well_known_facts = rue_air::ProviderWellKnownOptionFacts {
                nominals: well_known
                    .anonymous_nominals
                    .iter()
                    .map(|nominal| nominal.identity.clone())
                    .collect(),
                option_by_payload: well_known.option_by_payload.to_vec(),
            };
            let analysis_anonymous = selected_anonymous
                .values()
                .chain(well_known.anonymous_nominals.iter())
                .cloned()
                .collect::<Vec<_>>();
            let consulted_anonymous_nominals = crate::body_query::BodyConsultedAnonymousNominals(
                analysis_anonymous.clone().into(),
            );
            drop(prerequisites_span);
            let _analysis_span =
                tracing::info_span!("body_analysis", phase = "semantic_analysis").entered();
            let transaction = if matches!(key.instance, crate::FunctionInstanceKey::Definition(_))
                && matches!(
                    definition.kind(),
                    crate::StableDefinitionKind::Function
                        | crate::StableDefinitionKind::Method
                        | crate::StableDefinitionKind::AssociatedFunction
                        | crate::StableDefinitionKind::Destructor
                        // A test declaration owns an ordinary `()`-typed body
                        // and reaches the same analyzer (ADR-0083 §1).
                        | crate::StableDefinitionKind::Test
                ) {
                let input = self.body_input.resolve(context, key)?;
                let input = match input {
                    crate::body_query::BodyInputValue::Available(input) => input,
                    crate::body_query::BodyInputValue::Incomplete(
                        crate::body_query::BodyInputIncomplete::BodyPlanFailure(failure),
                    ) => {
                        return Ok(QueryOutput::success(Self::body_plan_failure(
                            &definition,
                            &failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    crate::body_query::BodyInputValue::Incomplete(_) => {
                        return Err(QueryAbort::Canceled);
                    }
                };
                let attribution_enabled =
                    tracing::enabled!(target: "rue::timing", tracing::Level::INFO);
                let _body_input_lowering_span =
                    tracing::info_span!("body_input_lowering", phase = "semantic_analysis")
                        .entered();
                let symbol_space = self.symbol_space.generation(context.revision());
                let materialized = if attribution_enabled {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle_with_attribution(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|(bundle, attribution)| (bundle, Some(attribution)))
                } else {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|bundle| (bundle, None))
                };
                let bundle = match materialized {
                    Ok((bundle, attribution)) if input.artifacts.plan.instruction_count() > 0 => {
                        if let Some(attribution) = attribution {
                            publish_body_plan_materialization_attribution(attribution);
                        }
                        materialization_work.borrow_mut().extend([
                            WorkItem::new("candidate_body_plan.materialization.plans", 1),
                            WorkItem::new(
                                "candidate_body_plan.materialization.instructions",
                                bundle.instruction_count() as u64,
                            ),
                            WorkItem::new(
                                "candidate_body_plan.materialization.payload_words",
                                bundle.payload_word_count() as u64,
                            ),
                        ]);
                        bundle
                    }
                    Ok(_) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            "owned body input lowered to no local instructions",
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Query(abort)) => {
                        return Err(abort);
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Build(error)) => {
                        return Ok(QueryOutput::success(Self::lowering_build_failure(&error))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Invalid(
                        failure,
                    )) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                };
                drop(_body_input_lowering_span);
                // ADR-0076 §4: a body materialized against a superseded
                // equality space is never reused. Abandoning the attempt here
                // re-runs the body against the live generation, which is the
                // fail-closed half of `require_rir_authority`.
                if !bundle.symbol_space().is_live() {
                    return Err(QueryAbort::Canceled);
                }
                let provider = CompilerBodyFactProvider::new(
                    self.compiler_body_provider_queries(
                        context,
                        key.configuration.clone(),
                        observed.clone(),
                        positive_references.clone(),
                    )
                    .with_deferred_anonymous_producers(deferred_anonymous_producers.clone())
                    .with_producer_transport_failure(producer_transport_failure.clone()),
                );
                let source = CompilerBodyDurableSource::with_anonymous(
                    &provider,
                    &analysis_anonymous,
                    Some((
                        definition.module().clone(),
                        rue_air::DurableBodySourceLocator {
                            file_id: input.source.file_id,
                            physical_path: input.source.physical_path.clone(),
                            source_length: input.source.source_length,
                            source_text: input.source.source_text.clone(),
                        },
                    )),
                );
                let preview = key
                    .configuration
                    .preview_features
                    .names()
                    .iter()
                    .filter_map(|name| name.parse().ok())
                    .collect();
                let analyzed = {
                    let _provider_analysis_span = tracing::info_span!(
                        "semantic_provider_analysis",
                        phase = "semantic_analysis"
                    )
                    .entered();
                    rue_air::analyze_provider_ordinary_body(
                        &provider,
                        source,
                        &bundle,
                        definition.clone(),
                        definition.name(),
                        definition.kind(),
                        definition.owner().map(|owner| owner.name()),
                        key.configuration.target,
                        preview,
                        &well_known_facts,
                    )
                };
                match provider.finish_status() {
                    Ok(()) => {}
                    Err(CompilerBodyProviderStatus::Fatal(abort)) => return Err(abort),
                    Err(CompilerBodyProviderStatus::Incomplete(_)) => {
                        return Err(QueryAbort::Canceled);
                    }
                    Err(CompilerBodyProviderStatus::Ready) => unreachable!(),
                }
                match analyzed {
                    Ok(analyzed) => {
                        self.provider_observation_meter
                            .accrue_provider_body_work(analyzed.work);
                        let mut references = analyzed
                            .referenced_definitions
                            .iter()
                            .cloned()
                            .map(|definition| {
                                crate::body_query::BodyReference::Callable(
                                    crate::FunctionInstanceKey::Definition(definition),
                                )
                            })
                            .collect::<BTreeSet<_>>();
                        references.extend(
                            analyzed
                                .referenced_values
                                .iter()
                                .cloned()
                                .map(crate::body_query::BodyReference::Definition),
                        );
                        let definition_tokens = analyzed
                            .definition_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let module_tokens = analyzed
                            .module_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let nested = analyzed
                            .referenced_specializations
                            .iter()
                            .map(|instance| {
                                instance.try_map_identities(
                                    &|token| {
                                        definition_tokens.get(token).cloned().ok_or(
                                            rue_air::SemanticStableResolutionFailure::Missing,
                                        )
                                    },
                                    &|token| {
                                        module_tokens.get(token).cloned().ok_or(
                                            rue_air::SemanticStableResolutionFailure::Missing,
                                        )
                                    },
                                )
                            })
                            .collect::<Result<Vec<_>, _>>();
                        if let Ok(nested) = &nested {
                            references.extend(
                                nested
                                    .iter()
                                    .cloned()
                                    .map(crate::body_query::BodyReference::Callable),
                            );
                        }
                        let body = analyzed.export.body.try_map_keys(
                            &|token| {
                                definition_tokens
                                    .get(token)
                                    .cloned()
                                    .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                            },
                            &|token| {
                                module_tokens
                                    .get(token)
                                    .cloned()
                                    .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                            },
                        );
                        let produced_anonymous_nominals =
                            project_provider_produced_anonymous_nominals(
                                &analyzed.produced_anonymous_nominals,
                                &definition_tokens,
                                &module_tokens,
                            );
                        match (body, nested, produced_anonymous_nominals) {
                            (Ok(body), Ok(_), Ok(produced_anonymous_nominals)) => {
                                collect_published_body_references(&body, &mut references);
                                crate::body_query::BodyTransaction::Success {
                                    body: Arc::new(crate::body_query::CanonicalBody::Ordinary {
                                        owner: definition.clone(),
                                        body,
                                    }),
                                    references: crate::body_query::BodyReferences(
                                        references.into_iter().collect::<Vec<_>>().into(),
                                    ),
                                    produced_anonymous_nominals,
                                    consulted_anonymous_nominals: consulted_anonymous_nominals
                                        .clone(),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                            (Err(failure), _, _) => {
                                crate::body_query::BodyTransaction::DeterministicFailure {
                                    diagnostic_basis: None,
                                    errors: crate::CompileErrors::from(
                                        crate::CompileError::without_span(
                                            rue_error::ErrorKind::OutputPublication(format!(
                                                "provider body key relocation failed: \
                                                         {failure:?}"
                                            )),
                                        ),
                                    ),
                                    references: crate::body_query::BodyReferences(Arc::from([])),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                            (_, Err(failure), _) => {
                                crate::body_query::BodyTransaction::DeterministicFailure {
                                    diagnostic_basis: None,
                                    errors: crate::CompileErrors::from(
                                        crate::CompileError::without_span(
                                            rue_error::ErrorKind::OutputPublication(format!(
                                                "provider specialization reference relocation \
                                                     failed: {failure:?}"
                                            )),
                                        ),
                                    ),
                                    references: crate::body_query::BodyReferences(Arc::from([])),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                            (_, _, Err(failure)) => {
                                crate::body_query::BodyTransaction::DeterministicFailure {
                                    diagnostic_basis: None,
                                    errors: crate::CompileErrors::from(
                                        crate::CompileError::without_span(
                                            rue_error::ErrorKind::OutputPublication(format!(
                                                "provider produced anonymous relocation failed: \
                                                 {failure:?}"
                                            )),
                                        ),
                                    ),
                                    references: crate::body_query::BodyReferences(Arc::from([])),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                        }
                    }
                    Err(error) => body_failure_with_source(error, &input.source),
                }
            } else if let crate::FunctionInstanceKey::Specialization { base: _, arguments } =
                &key.instance
                && matches!(definition.kind(), crate::StableDefinitionKind::Function)
            {
                let input = self.body_input.resolve(context, key)?;
                let input = match input {
                    crate::body_query::BodyInputValue::Available(input) => input,
                    crate::body_query::BodyInputValue::Incomplete(
                        crate::body_query::BodyInputIncomplete::BodyPlanFailure(failure),
                    ) => {
                        return Ok(QueryOutput::success(Self::body_plan_failure(
                            &definition,
                            &failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    crate::body_query::BodyInputValue::Incomplete(_) => {
                        return Err(QueryAbort::Canceled);
                    }
                };
                let attribution_enabled =
                    tracing::enabled!(target: "rue::timing", tracing::Level::INFO);
                let _body_input_lowering_span =
                    tracing::info_span!("body_input_lowering", phase = "semantic_analysis")
                        .entered();
                let symbol_space = self.symbol_space.generation(context.revision());
                let materialized = if attribution_enabled {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle_with_attribution(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|(bundle, attribution)| (bundle, Some(attribution)))
                } else {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|bundle| (bundle, None))
                };
                let bundle = match materialized {
                    Ok((bundle, attribution)) if input.artifacts.plan.instruction_count() > 0 => {
                        if let Some(attribution) = attribution {
                            publish_body_plan_materialization_attribution(attribution);
                        }
                        materialization_work.borrow_mut().extend([
                            WorkItem::new("candidate_body_plan.materialization.plans", 1),
                            WorkItem::new(
                                "candidate_body_plan.materialization.instructions",
                                bundle.instruction_count() as u64,
                            ),
                            WorkItem::new(
                                "candidate_body_plan.materialization.payload_words",
                                bundle.payload_word_count() as u64,
                            ),
                        ]);
                        bundle
                    }
                    Ok(_) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            "owned body input lowered to no local instructions",
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Query(abort)) => {
                        return Err(abort);
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Build(error)) => {
                        return Ok(QueryOutput::success(Self::lowering_build_failure(&error))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Invalid(
                        failure,
                    )) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                };
                drop(_body_input_lowering_span);
                // ADR-0076 §4: a body materialized against a superseded
                // equality space is never reused. Abandoning the attempt here
                // re-runs the body against the live generation, which is the
                // fail-closed half of `require_rir_authority`.
                if !bundle.symbol_space().is_live() {
                    return Err(QueryAbort::Canceled);
                }
                let provider = CompilerBodyFactProvider::new(
                    self.compiler_body_provider_queries(
                        context,
                        key.configuration.clone(),
                        observed.clone(),
                        positive_references.clone(),
                    )
                    .with_deferred_anonymous_producers(deferred_anonymous_producers.clone())
                    .with_producer_transport_failure(producer_transport_failure.clone()),
                );
                let source = CompilerBodyDurableSource::with_anonymous(
                    &provider,
                    &analysis_anonymous,
                    Some((
                        definition.module().clone(),
                        rue_air::DurableBodySourceLocator {
                            file_id: input.source.file_id,
                            physical_path: input.source.physical_path.clone(),
                            source_length: input.source.source_length,
                            source_text: input.source.source_text.clone(),
                        },
                    )),
                );
                let preview = key
                    .configuration
                    .preview_features
                    .names()
                    .iter()
                    .filter_map(|name| name.parse().ok())
                    .collect();
                let analyzed = {
                    let _provider_analysis_span = tracing::info_span!(
                        "semantic_provider_analysis",
                        phase = "semantic_analysis"
                    )
                    .entered();
                    rue_air::analyze_provider_specialized_body(
                        &provider,
                        source,
                        &bundle,
                        definition.clone(),
                        definition.name(),
                        arguments,
                        key.configuration.target,
                        preview,
                        &well_known_facts,
                    )
                };
                match provider.finish_status() {
                    Ok(()) => {}
                    Err(CompilerBodyProviderStatus::Fatal(abort)) => return Err(abort),
                    Err(CompilerBodyProviderStatus::Incomplete(_)) => {
                        return Err(QueryAbort::Canceled);
                    }
                    Err(CompilerBodyProviderStatus::Ready) => unreachable!(),
                }
                // A comptime type constructor owns the anonymous nominals in
                // its result. Observe that exact semantic projection here so
                // the body-produced projection remains a thin view of this
                // transaction instead of recomputing producer facts.
                let produced_anonymous_nominals = {
                    let shell = context.query_registered(
                        &self.declaration_shells,
                        DeclarationShellQueryKey(candidate.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(
                        shell,
                    )) = shell.outcome()
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                        declaration: candidate.clone(),
                        configuration: key.configuration.clone(),
                    };
                    let signature = context.query_registered(
                        &self.semantic_nucleus,
                        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                            producer.clone(),
                        ),
                    )?;
                    let rue_query::QueryOutcome::Success(
                        crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature),
                    ) = signature.outcome()
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    let Some(exact_type_syntax) = signature.callable_type_syntax.as_ref() else {
                        return Err(QueryAbort::Canceled);
                    };
                    if let Some(call) = comptime_call_for_anonymous_function(
                        &producer,
                        &key.instance,
                        shell,
                        signature,
                        exact_type_syntax,
                    ) {
                        let projected = context.query_registered(
                            &self.semantic_nucleus,
                            crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(call),
                        )?;
                        let rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(
                                projected,
                            ),
                        ) = projected.outcome()
                        else {
                            return Err(QueryAbort::Canceled);
                        };
                        crate::body_query::BodyProducedAnonymousNominals(
                            projected.anonymous_nominals.clone(),
                        )
                    } else {
                        crate::body_query::BodyProducedAnonymousNominals(Arc::from([]))
                    }
                };
                match analyzed {
                    Ok(analyzed) => {
                        self.provider_observation_meter
                            .accrue_provider_body_work(analyzed.work);
                        let definition_tokens = analyzed
                            .definition_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let module_tokens = analyzed
                            .module_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let definition = |token: &rue_air::SemanticDefinitionToken| {
                            definition_tokens
                                .get(token)
                                .cloned()
                                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                        };
                        let module = |token: &rue_air::SemanticModuleToken| {
                            module_tokens
                                .get(token)
                                .cloned()
                                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                        };
                        let identity = analyzed.export.identity.try_map_keys(&definition, &module);
                        let body = analyzed.export.body.try_map_keys(&definition, &module);
                        let dependencies = analyzed
                            .export
                            .dependencies
                            .iter()
                            .map(&definition)
                            .collect::<Result<Vec<_>, _>>();
                        let nested = analyzed
                            .referenced_specializations
                            .iter()
                            .map(|instance| instance.try_map_identities(&definition, &module))
                            .collect::<Result<Vec<_>, _>>();
                        let locally_produced = project_provider_produced_anonymous_nominals(
                            &analyzed.produced_anonymous_nominals,
                            &definition_tokens,
                            &module_tokens,
                        );
                        match (identity, body, dependencies, nested, locally_produced) {
                            (
                                Ok(identity),
                                Ok(body),
                                Ok(dependencies),
                                Ok(nested),
                                Ok(locally_produced),
                            ) => {
                                let mut references = analyzed
                                    .referenced_definitions
                                    .into_iter()
                                    .map(|definition| {
                                        crate::body_query::BodyReference::Callable(
                                            crate::FunctionInstanceKey::Definition(definition),
                                        )
                                    })
                                    .collect::<BTreeSet<_>>();
                                references.extend(
                                    analyzed
                                        .referenced_values
                                        .into_iter()
                                        .map(crate::body_query::BodyReference::Definition),
                                );
                                references.extend(
                                    nested
                                        .into_iter()
                                        .map(crate::body_query::BodyReference::Callable),
                                );
                                collect_published_body_references(&body, &mut references);
                                let mut produced = BTreeMap::new();
                                let conflict = produced_anonymous_nominals
                                    .0
                                    .iter()
                                    .chain(locally_produced.0.iter())
                                    .find_map(|nominal| {
                                        crate::durable_semantics::merge_anonymous_nominal(
                                            &mut produced,
                                            nominal,
                                        )
                                        .err()
                                    });
                                if let Some(identity) = conflict {
                                    crate::body_query::BodyTransaction::DeterministicFailure {
                                        diagnostic_basis: None,
                                        errors: crate::CompileErrors::from(
                                            crate::CompileError::without_span(
                                                rue_error::ErrorKind::OutputPublication(format!(
                                                    "provider specialization produced conflicting anonymous facts for {identity:?}"
                                                )),
                                            ),
                                        ),
                                        references: crate::body_query::BodyReferences(
                                            Arc::from([]),
                                        ),
                                        lookup_observations:
                                            crate::body_query::BodyLookupObservations::default(),
                                    }
                                } else {
                                    crate::body_query::BodyTransaction::Success {
                                        body: Arc::new(
                                            crate::body_query::CanonicalBody::Specialization {
                                                identity,
                                                body,
                                                dependencies: dependencies.into(),
                                                dependency_boundary_complete: analyzed
                                                    .export
                                                    .dependency_boundary_complete,
                                            },
                                        ),
                                        references: crate::body_query::BodyReferences(
                                            references.into_iter().collect::<Vec<_>>().into(),
                                        ),
                                        produced_anonymous_nominals:
                                            crate::body_query::BodyProducedAnonymousNominals(
                                                produced.into_values().collect::<Vec<_>>().into(),
                                            ),
                                        consulted_anonymous_nominals: consulted_anonymous_nominals
                                            .clone(),
                                        lookup_observations:
                                            crate::body_query::BodyLookupObservations::default(),
                                    }
                                }
                            }
                            _ => crate::body_query::BodyTransaction::DeterministicFailure {
                                diagnostic_basis: None,
                                errors: crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::OutputPublication(
                                            "provider specialization key relocation failed"
                                                .to_owned(),
                                        ),
                                    ),
                                ),
                                references: crate::body_query::BodyReferences(Arc::from([])),
                                lookup_observations:
                                    crate::body_query::BodyLookupObservations::default(),
                            },
                        }
                    }
                    Err(error) => body_failure_with_source(error, &input.source),
                }
            } else if let crate::FunctionInstanceKey::AnonymousMember { owner, member } =
                &key.instance
            {
                let crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(
                    owner_identity,
                )) = owner.as_ref()
                else {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous member has a non-anonymous owner",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                };
                let canonical_owner = owner_identity.with_canonical_producer();
                let Some(owner_fact) = selected_anonymous.get(canonical_owner.as_ref()) else {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous member owner fact is unavailable",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                };
                let crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                    methods, ..
                } = &owner_fact.shape
                else {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous enum cannot own a callable member",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                };
                let Some(method) = methods.iter().find(|candidate| {
                    let kind = if candidate.name.as_ref() == "__drop" {
                        crate::AnonymousMemberKind::Destructor
                    } else if candidate.has_self {
                        crate::AnonymousMemberKind::Method
                    } else {
                        crate::AnonymousMemberKind::AssociatedFunction
                    };
                    candidate.name == member.name && kind == member.kind
                }) else {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous member identity is absent from its producer facts",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                };
                if !method.has_body {
                    return Ok(QueryOutput::success(Self::lowering_failure(
                        &definition,
                        "anonymous member producer facts do not admit a body",
                    ))
                    .with_terminal_kind(QueryTerminalKind::Failure));
                }
                let input = self.body_input.resolve_producer_artifact(context, key)?;
                let input = match input {
                    crate::body_query::BodyInputValue::Available(input) => input,
                    crate::body_query::BodyInputValue::Incomplete(
                        crate::body_query::BodyInputIncomplete::BodyPlanFailure(failure),
                    ) => {
                        return Ok(QueryOutput::success(Self::body_plan_failure(
                            &definition,
                            &failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    crate::body_query::BodyInputValue::Incomplete(incomplete) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            format!("anonymous producer artifact is unavailable: {incomplete:?}"),
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                };
                let attribution_enabled =
                    tracing::enabled!(target: "rue::timing", tracing::Level::INFO);
                let _body_input_lowering_span =
                    tracing::info_span!("body_input_lowering", phase = "semantic_analysis")
                        .entered();
                let symbol_space = self.symbol_space.generation(context.revision());
                let materialized = if attribution_enabled {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle_with_declaration_and_attribution(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|(bundle, declaration, attribution)| {
                            (bundle, declaration, Some(attribution))
                        })
                } else {
                    input
                        .artifacts
                        .plan
                        .materialize_body_rir_bundle_with_declaration(
                            &symbol_space,
                            input.source.file_id,
                            input.source.declaration_start,
                            input.source.source_length,
                            || context.check_canceled(),
                        )
                        .map(|(bundle, declaration)| (bundle, declaration, None))
                };
                let (bundle, candidate_root) = match materialized {
                    Ok((bundle, declaration, attribution))
                        if input.artifacts.plan.instruction_count() > 0 =>
                    {
                        if let Some(attribution) = attribution {
                            publish_body_plan_materialization_attribution(attribution);
                        }
                        materialization_work.borrow_mut().extend([
                            WorkItem::new("candidate_body_plan.materialization.plans", 1),
                            WorkItem::new(
                                "candidate_body_plan.materialization.instructions",
                                bundle.instruction_count() as u64,
                            ),
                            WorkItem::new(
                                "candidate_body_plan.materialization.payload_words",
                                bundle.payload_word_count() as u64,
                            ),
                        ]);
                        (bundle, declaration)
                    }
                    Ok(_) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            "anonymous producer artifact contains no local instructions",
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Query(abort)) => {
                        return Err(abort);
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Build(error)) => {
                        return Ok(QueryOutput::success(Self::lowering_build_failure(&error))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    Err(crate::canonical_lower::BodyPlanMaterializationFailure::Invalid(
                        failure,
                    )) => {
                        return Ok(QueryOutput::success(Self::lowering_failure(
                            &definition,
                            failure,
                        ))
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                };
                drop(_body_input_lowering_span);
                // ADR-0076 §4: a body materialized against a superseded
                // equality space is never reused. Abandoning the attempt here
                // re-runs the body against the live generation, which is the
                // fail-closed half of `require_rir_authority`.
                if !bundle.symbol_space().is_live() {
                    return Err(QueryAbort::Canceled);
                }
                let provider = CompilerBodyFactProvider::new(
                    self.compiler_body_provider_queries(
                        context,
                        key.configuration.clone(),
                        observed.clone(),
                        positive_references.clone(),
                    )
                    .with_deferred_anonymous_producers(deferred_anonymous_producers.clone())
                    .with_producer_transport_failure(producer_transport_failure.clone()),
                );
                let source_locator = rue_air::DurableBodySourceLocator {
                    file_id: input.source.file_id,
                    physical_path: input.source.physical_path.clone(),
                    source_length: input.source.source_length,
                    source_text: input.source.source_text.clone(),
                };
                let source = CompilerBodyDurableSource::with_anonymous(
                    &provider,
                    &analysis_anonymous,
                    Some((definition.module().clone(), source_locator.clone())),
                );
                let preview = key
                    .configuration
                    .preview_features
                    .names()
                    .iter()
                    .filter_map(|name| name.parse().ok())
                    .collect();
                let analyzed = {
                    let _provider_analysis_span = tracing::info_span!(
                        "semantic_provider_analysis",
                        phase = "semantic_analysis"
                    )
                    .entered();
                    rue_air::analyze_provider_anonymous_body(
                        &provider,
                        source,
                        &bundle,
                        candidate_root,
                        definition.clone(),
                        owner.as_ref(),
                        member,
                        key.configuration.target,
                        preview,
                        &well_known_facts,
                    )
                };
                match provider.finish_status() {
                    Ok(()) => {}
                    Err(CompilerBodyProviderStatus::Fatal(abort)) => return Err(abort),
                    Err(CompilerBodyProviderStatus::Incomplete(_)) => {
                        return Err(QueryAbort::Canceled);
                    }
                    Err(CompilerBodyProviderStatus::Ready) => unreachable!(),
                }
                match analyzed {
                    Ok(analyzed) => {
                        self.provider_observation_meter
                            .accrue_provider_body_work(analyzed.work);
                        let body_anchor = analyzed
                            .body_span
                            .start
                            .checked_sub(input.source.body_start)
                            .zip(analyzed.body_span.end.checked_sub(input.source.body_start))
                            .filter(|(start, end)| {
                                analyzed.body_span.file_id == input.source.file_id
                                    && start <= end
                                    && analyzed.body_span.end <= input.source.body_end
                            })
                            .map(|(start, end)| crate::body_query::BodyRelativeRange {
                                start,
                                end,
                            });
                        let definition_tokens = analyzed
                            .definition_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let module_tokens = analyzed
                            .module_tokens
                            .into_iter()
                            .collect::<AHashMap<_, _>>();
                        let definition = |token: &rue_air::SemanticDefinitionToken| {
                            definition_tokens
                                .get(token)
                                .cloned()
                                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                        };
                        let module = |token: &rue_air::SemanticModuleToken| {
                            module_tokens
                                .get(token)
                                .cloned()
                                .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
                        };
                        let identity = analyzed
                            .export
                            .identity
                            .try_map_identities(&definition, &module);
                        let body = analyzed.export.body.try_map_keys(&definition, &module);
                        let nested = analyzed
                            .referenced_specializations
                            .iter()
                            .map(|instance| instance.try_map_identities(&definition, &module))
                            .collect::<Result<Vec<_>, _>>();
                        let produced_anonymous_nominals =
                            project_provider_produced_anonymous_nominals(
                                &analyzed.produced_anonymous_nominals,
                                &definition_tokens,
                                &module_tokens,
                            );
                        match (identity, body, nested, produced_anonymous_nominals) {
                            (
                                Ok(identity),
                                Ok(body),
                                Ok(nested),
                                Ok(produced_anonymous_nominals),
                            ) if identity == key.instance && body_anchor.is_some() => {
                                let mut references = analyzed
                                    .referenced_definitions
                                    .into_iter()
                                    .map(|definition| {
                                        crate::body_query::BodyReference::Callable(
                                            crate::FunctionInstanceKey::Definition(definition),
                                        )
                                    })
                                    .collect::<BTreeSet<_>>();
                                references.extend(
                                    analyzed
                                        .referenced_values
                                        .into_iter()
                                        .map(crate::body_query::BodyReference::Definition),
                                );
                                references.extend(
                                    nested
                                        .into_iter()
                                        .map(crate::body_query::BodyReference::Callable),
                                );
                                collect_published_body_references(&body, &mut references);
                                crate::body_query::BodyTransaction::Success {
                                    body: Arc::new(crate::body_query::CanonicalBody::Anonymous {
                                        identity,
                                        body_anchor: body_anchor.expect(
                                            "successful anonymous body projection has an anchor",
                                        ),
                                        body,
                                    }),
                                    references: crate::body_query::BodyReferences(
                                        references.into_iter().collect::<Vec<_>>().into(),
                                    ),
                                    produced_anonymous_nominals,
                                    consulted_anonymous_nominals: consulted_anonymous_nominals
                                        .clone(),
                                    lookup_observations:
                                        crate::body_query::BodyLookupObservations::default(),
                                }
                            }
                            _ => crate::body_query::BodyTransaction::DeterministicFailure {
                                diagnostic_basis: None,
                                errors: crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::OutputPublication(
                                            "provider anonymous body key relocation failed"
                                                .to_owned(),
                                        ),
                                    ),
                                ),
                                references: crate::body_query::BodyReferences(Arc::from([])),
                                lookup_observations:
                                    crate::body_query::BodyLookupObservations::default(),
                            },
                        }
                    }
                    Err(error) => body_failure_with_source(error, &input.source),
                }
            } else {
                crate::body_query::BodyTransaction::DeterministicFailure {
                    diagnostic_basis: None,
                    errors: crate::CompileErrors::from(crate::CompileError::without_span(
                        rue_error::ErrorKind::InvalidCompilerInput(
                            "body query instance has no provider-native analyzer".into(),
                        ),
                    )),
                    references: crate::body_query::BodyReferences(Arc::from([])),
                    lookup_observations: crate::body_query::BodyLookupObservations::default(),
                }
            };
            // Provider operations above record every semantic, lookup, and
            // producer edge while the analysis consumes it. Replaying the
            // exported reference summary through semantic queries here would
            // be both redundant and cyclic (notably through
            // body-produced-anonymous -> body-transaction).
            let observed = observed.replace(ObservedLookupRoot::new());
            let descriptors = observed.descriptors();
            context.register_attempt_handoff(PublishedLookupRootHandoff {
                lease: self.lookup_root_lease.clone(),
                runtime: self.runtime.clone(),
                root: body_lookup_root_identity(key),
                observed: Some(observed),
                rollback: None,
            });
            let transaction = transaction.attach_provider_observations(
                descriptors,
                positive_references.replace(BTreeSet::new()),
            );
            let kind = if matches!(
                transaction,
                crate::body_query::BodyTransaction::Success { .. }
            ) {
                QueryTerminalKind::Success
            } else {
                QueryTerminalKind::Failure
            };
            let output = QueryOutput::success(transaction).with_terminal_kind(kind);
            if kind == QueryTerminalKind::Success {
                Ok(output.with_work(materialization_work.borrow_mut().drain(..).collect()))
            } else {
                Ok(output)
            }
        })();
        match result {
            Ok(output) => Ok(output),
            Err(abort) => {
                // Cancellation is query control and always wins. Domain-specific
                // deferrals are typed query values, atomically published with the
                // attempt that classified them; no revision/key side channel can
                // race a joiner or a successor request.
                if context.check_canceled().is_err() || !matches!(abort, QueryAbort::Canceled) {
                    return Err(abort);
                }
                let control = if let Some(failure) = producer_transport_failure.borrow_mut().take()
                {
                    crate::body_query::BodyTransactionControl::ProducerFailed(failure)
                } else if !deferred_anonymous_producers.borrow().is_empty() {
                    crate::body_query::BodyTransactionControl::DeferredAnonymousProducers(
                        deferred_anonymous_producers
                            .borrow()
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .into(),
                    )
                } else if let Some(failure) = well_known_resolution_failure.into_inner() {
                    crate::body_query::BodyTransactionControl::WellKnownOptionResolution(failure)
                } else {
                    return Err(abort);
                };
                Ok(
                    QueryOutput::success(crate::body_query::BodyTransaction::Control(control))
                        .with_terminal_kind(QueryTerminalKind::Failure),
                )
            }
        }
    }
}
impl RevisionedQueryDatabase {
    /// Request the canonical registered body evaluator. Domain-specific
    /// deferrals are typed terminal values published atomically with the
    /// attempt that produced them; cancellation remains a runtime abort.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn body_transaction(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<
        Arc<rue_query::QueryTerminal<crate::body_query::BodyTransaction>>,
        BodyTransactionRequestFailure,
    > {
        let attempt = self.runtime.request_registered(
            &self.body_transactions,
            revision,
            key.clone(),
            cancellation.clone(),
        );
        match attempt.into_result() {
            Ok(terminal) => match terminal.outcome() {
                rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Control(
                    crate::body_query::BodyTransactionControl::DeferredAnonymousProducers(
                        producers,
                    ),
                )) => Err(BodyTransactionRequestFailure::DeferredAnonymousProducers(
                    producers.clone(),
                )),
                rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Control(
                    crate::body_query::BodyTransactionControl::ProducerFailed(failure),
                )) => Err(BodyTransactionRequestFailure::ProducerFailed(
                    failure.clone(),
                )),
                rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Control(
                    crate::body_query::BodyTransactionControl::WellKnownOptionResolution(failure),
                )) => Err(BodyTransactionRequestFailure::WellKnownOptionResolution(
                    failure.clone(),
                )),
                rue_query::QueryOutcome::Success(transaction) => {
                    if let Some(observations) = transaction.lookup_observations() {
                        self.refresh_published_body_lookup_root(
                            revision,
                            &key,
                            observations,
                            cancellation.clone(),
                        )
                        .map_err(BodyTransactionRequestFailure::Query)?;
                    }
                    Ok(terminal)
                }
                _ => Ok(terminal),
            },
            Err(abort) if cancellation.is_canceled() => {
                Err(BodyTransactionRequestFailure::Query(QueryAbort::Canceled))
            }
            Err(abort) => Err(BodyTransactionRequestFailure::Query(abort)),
        }
    }
    /// Request the current presentation locator for one body independently of
    /// its retained semantic transaction and aggregate body closure.
    pub(crate) fn body_source_basis_projection(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<
        Arc<rue_query::QueryTerminal<Option<crate::body_query::BodySourceLocator>>>,
        QueryAbort,
    > {
        self.runtime
            .request_registered(&self.body_source_bases, revision, key, cancellation)
            .into_result()
    }

    /// Request one stable-ordered frontier of independent warning-only body
    /// projections. The registered root joins atomically, so cancellation or a
    /// child abort cannot publish a partial aggregate.
    pub(crate) fn warning_body_reference_frontier(
        &self,
        revision: Revision,
        keys: Arc<[crate::body_query::BodyQueryKey]>,
        cancellation: CancellationToken,
    ) -> (
        QueryRequestAttempt<WarningBodyReferencesBatchValue>,
        Vec<Option<FrontierChildExecution>>,
    ) {
        let attempt = self.runtime.request_registered(
            &self.warning_body_reference_batches,
            revision,
            WarningBodyReferencesBatchKey {
                bodies: keys.clone(),
            },
            cancellation,
        );
        let executions =
            frontier_child_executions(&attempt, "compiler.warning-body-references", keys.as_ref());
        (attempt, executions)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn body_produced_anonymous_projection(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<Arc<rue_query::QueryTerminal<crate::body_query::ProducedAnonymous>>, QueryAbort>
    {
        self.runtime
            .request_registered(&self.body_produced_anonymous, revision, key, cancellation)
            .into_result()
    }

    #[cfg(test)]
    pub(crate) fn body_input(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<Arc<rue_query::QueryTerminal<crate::body_query::BodyInputValue>>, QueryAbort> {
        self.runtime
            .request_registered(&self.body_inputs, revision, key, cancellation)
            .into_result()
    }

    /// Project one reached body's trusted-toolchain-module demand set (RUE-1112)
    /// from the registered `body-toolchain-demands` node. This is the rooted
    /// semantic attempt's park prerequisite: it observes the body's canonical
    /// declaration artifact, is pure and I/O-free, and never itself parks. The rooted attempt
    /// checks the projected modules against the satisfied catalogue and decides
    /// whether to park BEFORE the body transaction runs.
    #[cfg(test)]
    pub(crate) fn body_toolchain_demands(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<Arc<rue_query::QueryTerminal<crate::BodyToolchainDemand>>, QueryAbort> {
        self.runtime
            .request_registered(&self.body_toolchain_demands, revision, key, cancellation)
            .into_result()
    }

    /// Whether the body-transaction family has published any terminal. Used by
    /// the RUE-1112 park test to prove a park precedes any body transaction.
    #[cfg(test)]
    pub(crate) fn any_body_transaction_terminal(&self) -> bool {
        self.body_transactions.any_retained_key(|_| true)
    }

    /// Whether a retained body transaction has a successful value. Cached
    /// deterministic failures are intentionally excluded: retention of a
    /// failure is not evidence of published candidate work or reusable work.
    #[cfg(test)]
    pub(crate) fn any_successful_body_transaction_for_test(&self) -> bool {
        let Some(revision) = self.current_semantic_revision() else {
            return false;
        };
        let mut keys = Vec::new();
        self.body_transactions.any_retained_key(|key| {
            keys.push(key.clone());
            false
        });
        keys.into_iter().any(|key| {
            self.body_transaction(revision, key, CancellationToken::new())
                .is_ok_and(|terminal| {
                    matches!(
                        terminal.outcome(),
                        rue_query::QueryOutcome::Success(
                            crate::body_query::BodyTransaction::Success { .. }
                        )
                    )
                })
        })
    }

    /// Whether the exact body key already has a retained memo node.
    ///
    /// This is deliberately narrower than provenance: a newly reached body has
    /// no prior key to invalidate, even though its first terminal changes the
    /// provenance-derived body set. The query runtime remains authoritative for
    /// whether the current request actually computed or reused that key. The
    /// exact retained-key index probe is O(1) average-case; it does not
    /// enumerate the retained body family once per reached body.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn has_retained_body_key(&self, key: &crate::body_query::BodyQueryKey) -> bool {
        self.body_transactions.contains_retained_key(key)
    }

    /// Test-only snapshot of every retained body-query identity and whether its
    /// terminal is observable at `revision`.
    ///
    /// The key scan is intentionally rooted in the production body-transaction
    /// family. It includes stale keys left behind by invalidation, while the
    /// read-only terminal request records whether that key still has a current
    /// success or deterministic failure. The supplied computation is never
    /// entered, so this hook cannot become a second semantic computation path.
    #[allow(dead_code)]
    pub(crate) fn retained_body_identity_states_for_test(
        &self,
        revision: Revision,
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    ) -> BTreeMap<String, Option<crate::BodyTransaction>> {
        let mut keys = Vec::new();
        self.body_transactions.any_retained_key(|candidate| {
            keys.push(candidate.clone());
            false
        });
        keys.retain(|key| key.configuration == configuration);
        keys.sort_by_key(|key| key.stable_identity());
        keys.into_iter()
            .map(|key| {
                let identity = key.stable_identity();
                let transaction = self.retained_body_transaction_for_test(revision, key);
                (identity, transaction)
            })
            .collect()
    }

    /// Test-only exact provenance for retained ordinary body terminals.
    ///
    /// Looking up retained keys directly keeps failed-revision acceptance tests
    /// independent of the aggregate stable-definition request, which may itself
    /// fail after a negative name lookup even though unrelated body terminals
    /// remain reusable.
    #[cfg(test)]
    pub(crate) fn retained_body_transaction_origins_for_test(
        &self,
        revision: Revision,
        names: &[String],
    ) -> BTreeMap<String, u64> {
        let mut origins = BTreeMap::new();
        for name in names {
            let mut retained_key = None;
            self.body_transactions.any_retained_key(|candidate| {
                let crate::FunctionInstanceKey::Definition(definition) = &candidate.instance else {
                    return false;
                };
                if definition.name() != name.as_str() {
                    return false;
                }
                retained_key = Some(candidate.clone());
                true
            });
            let Some(key) = retained_key else {
                continue;
            };
            let terminal = self.body_transaction(revision, key, CancellationToken::new());
            if let Ok(terminal) = terminal {
                origins.insert(name.clone(), terminal.origin_request_id());
            }
        }
        origins
    }

    /// Test-only lookup of one retained body transaction by its complete
    /// function-instance identity, including specializations and anonymous
    /// members.
    pub(crate) fn retained_body_transaction_for_test(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
    ) -> Option<crate::BodyTransaction> {
        let terminal = self
            .body_transaction(revision, key, CancellationToken::new())
            .ok()?;
        let rue_query::QueryOutcome::Success(transaction) = terminal.outcome() else {
            unreachable!("BodyTransaction publishes typed values")
        };
        Some(transaction.clone())
    }
}
