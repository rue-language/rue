macro_rules! register_body_body_produced_anonymous {
    ($runtime:ident, $semantic_nucleus_for_produced_anonymous_evaluator:ident, $shells_for_produced_anonymous:ident, $transactions_for_produced_anonymous:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.body-produced-anonymous",
                BODY_QUERY_MEMO_RETENTION,
                crate::body_query::produced_anonymous_equal,
                move |context, _, key: &crate::body_query::BodyQueryKey| {
                    // Only free-function definitions and their exact
                    // specializations can be compile-time type constructors.
                    // Every other body kind publishes its locally produced
                    // anonymous facts directly from its successful transaction.
                    // In particular, an anonymous member has no declaration
                    // signature from which to synthesize a comptime-call query.
                    let transaction_only = match &key.instance {
                        crate::FunctionInstanceKey::Definition(definition) => {
                            definition.kind() != crate::StableDefinitionKind::Function
                        }
                        crate::FunctionInstanceKey::Specialization { .. } => false,
                        crate::FunctionInstanceKey::AnonymousMember { .. }
                        | crate::FunctionInstanceKey::DropGlue(_)
                        | crate::FunctionInstanceKey::ErrorPrinter(_)
                        | crate::FunctionInstanceKey::TestDispatcher => true,
                    };
                    if transaction_only {
                        let transaction = context
                            .query_registered(&$transactions_for_produced_anonymous, key.clone())?;
                        let rue_query::QueryOutcome::Success(
                            crate::body_query::BodyTransaction::Success {
                                produced_anonymous_nominals,
                                ..
                            },
                        ) = transaction.outcome()
                        else {
                            return Err(QueryAbort::Canceled);
                        };
                        return Ok(QueryOutput::success(
                            crate::body_query::ProducedAnonymous::Produced(
                                produced_anonymous_nominals.clone(),
                            ),
                        ));
                    }
                    if matches!(
                        &key.instance,
                        crate::FunctionInstanceKey::Definition(definition)
                            if definition.kind() == crate::StableDefinitionKind::Function
                    ) {
                        match context
                            .query_registered(&$transactions_for_produced_anonymous, key.clone())
                        {
                            Ok(transaction) => {
                                let rue_query::QueryOutcome::Success(transaction) =
                                    transaction.outcome()
                                else {
                                    unreachable!("BodyTransaction publishes typed values")
                                };
                                if let crate::body_query::BodyTransaction::Success {
                                    produced_anonymous_nominals,
                                    ..
                                } = transaction
                                    && produced_anonymous_nominals.0.is_empty()
                                {
                                    return Ok(QueryOutput::success(
                                        crate::body_query::ProducedAnonymous::Produced(
                                            produced_anonymous_nominals.clone(),
                                        ),
                                    ));
                                }
                                // A deterministic producer diagnostic is a
                                // stable semantic fact, not cancellation. Let
                                // the comptime projection below recover its
                                // typed `ProducerFailed` value so anonymous
                                // type consumers cannot abort the rooted
                                // request while collecting that diagnostic.
                                // Control outcomes remain unavailable until
                                // their exact prerequisite is scheduled.
                                if matches!(
                                    transaction,
                                    crate::body_query::BodyTransaction::Control(_)
                                ) {
                                    return Err(QueryAbort::Canceled);
                                }
                            }
                            Err(QueryAbort::Canceled) => {}
                            Err(abort) => return Err(abort),
                        }
                    }

                    // A declaration signature can name the result of a
                    // compile-time type constructor before body reachability
                    // has supplied that constructor's body transaction. Keep
                    // the fact producer-owned by publishing the constructor's
                    // exact semantic projection through this family; the
                    // AnonymousNominal consumer still has one canonical body-
                    // produced dependency path.
                    let Some(definition) = function_definition_key(&key.instance).cloned() else {
                        return Err(QueryAbort::Canceled);
                    };
                    let Some(declaration) = declaration_candidate_for_stable_key(&definition)
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                        declaration: declaration.clone(),
                        configuration: key.configuration.clone(),
                    };
                    let shell = context.query_registered(
                        &$shells_for_produced_anonymous,
                        DeclarationShellQueryKey(declaration.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(
                        shell,
                    )) = shell.outcome()
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    let semantic_nucleus = $semantic_nucleus_for_produced_anonymous_evaluator
                        .get()
                        .expect("SemanticNucleus is installed before requests begin");
                    let signature = context.query_registered(
                        semantic_nucleus,
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
                    let Some(call) = comptime_call_for_anonymous_function(
                        &producer,
                        &key.instance,
                        shell,
                        signature,
                        exact_type_syntax,
                    ) else {
                        let transaction = context
                            .query_registered(&$transactions_for_produced_anonymous, key.clone())?;
                        let rue_query::QueryOutcome::Success(
                            crate::body_query::BodyTransaction::Success {
                                produced_anonymous_nominals,
                                ..
                            },
                        ) = transaction.outcome()
                        else {
                            return Err(QueryAbort::Canceled);
                        };
                        return Ok(QueryOutput::success(
                            crate::body_query::ProducedAnonymous::Produced(
                                produced_anonymous_nominals.clone(),
                            ),
                        ));
                    };
                    let projected = context.query_registered(
                        semantic_nucleus,
                        crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(call),
                    )?;
                    let projected = match projected.outcome() {
                        rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(
                                projected,
                            ),
                        ) => projected,
                        rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(failure),
                        ) => {
                            // A committed semantic failure is a typed producer
                            // fact, whether it is an ordinary source diagnostic
                            // (for example, an empty anonymous struct) or an
                            // internal anchor-transport invariant violation.
                            // Preserve it through the producer projection so a
                            // dependent body reports the deterministic failure
                            // instead of aborting an uncanceled request.
                            return Ok(QueryOutput::success(
                                crate::body_query::ProducedAnonymous::ProducerFailed(Box::new(
                                    failure.clone(),
                                )),
                            ));
                        }
                        // A genuinely unavailable or wrong-kind producer remains
                        // query control rather than a fabricated semantic fact.
                        _ => return Err(QueryAbort::Canceled),
                    };
                    let owner = crate::StableProducerId::Function(Node::new(
                        key.instance
                            .with_collapsed_empty_specializations()
                            .into_owned(),
                    ));
                    let owned = projected
                        .anonymous_nominals
                        .iter()
                        .filter(|nominal| nominal.identity.producer == owner)
                        .cloned()
                        .collect::<Vec<_>>();
                    Ok(QueryOutput::success(
                        crate::body_query::ProducedAnonymous::Produced(
                            crate::body_query::BodyProducedAnonymousNominals(owned.into()),
                        ),
                    ))
                },
            )
            .expect("the BodyProducedAnonymous family has one canonical name")
    }};
}
