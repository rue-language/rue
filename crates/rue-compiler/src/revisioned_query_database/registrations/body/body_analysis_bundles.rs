macro_rules! register_body_body_analysis_bundles {
    ($produced_for_analysis_bundle:ident, $runtime:ident, $transactions_for_analysis_bundle:ident, $type_facts_for_analysis_bundle:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.body-analysis-bundle",
                BODY_QUERY_MEMO_RETENTION,
                crate::body_query::analysis_bundle_equal,
                move |context, _, key: &crate::body_query::BodyQueryKey| {
                    let transaction_terminal = context
                        .query_registered(&$transactions_for_analysis_bundle, key.clone())?;
                    let rue_query::QueryOutcome::Success(transaction) =
                        transaction_terminal.outcome()
                    else {
                        unreachable!("BodyTransaction publishes typed values")
                    };
                    let produced_anonymous = match transaction {
                        crate::body_query::BodyTransaction::Success { .. } => {
                            let produced = context
                                .query_registered(&$produced_for_analysis_bundle, key.clone())?;
                            let rue_query::QueryOutcome::Success(produced) = produced.outcome()
                            else {
                                unreachable!("BodyProducedAnonymous publishes typed values")
                            };
                            Some(produced.clone())
                        }
                        crate::body_query::BodyTransaction::DeterministicFailure { .. }
                        | crate::body_query::BodyTransaction::Control(_) => None,
                    };
                    let transaction = if let crate::body_query::BodyTransaction::Success {
                        transfer_requirements,
                        references,
                        lookup_observations,
                        ..
                    } = transaction
                    {
                        let mut failure = None;
                        let mut failure_coordinate = None;
                        for requirement in transfer_requirements.iter() {
                            let terminal = context.query_registered(
                                &$type_facts_for_analysis_bundle,
                                crate::type_queries::TypeQueryKey {
                                    ty: crate::semantic_identity::type_instance_from_semantic(
                                        &requirement.ty,
                                    ),
                                    configuration: key.configuration.clone(),
                                },
                            )?;
                            match crate::revisioned_query_database::semantic::type_facts_from_terminal(terminal.as_ref()) {
                                Ok(facts) if !facts.transferable => {
                                    failure = Some(crate::CompileError::new(
                                        rue_error::ErrorKind::ComptimeEvaluationFailed {
                                            reason: facts.transfer_failure.as_deref().unwrap_or(
                                                "type is not transferable across a thread boundary",
                                            ).to_owned(),
                                        },
                                        rue_span::Span::new(0, 0),
                                    ));
                                    failure_coordinate = Some(requirement.coordinate);
                                    break;
                                }
                                Ok(_) => {}
                                Err(fact_failure) => {
                                    failure = Some(crate::CompileError::new(
                                        rue_error::ErrorKind::ComptimeEvaluationFailed {
                                            reason: format!(
                                                "transferability facts unavailable: {fact_failure:?}"
                                            ),
                                        },
                                        rue_span::Span::new(0, 0),
                                    ));
                                    failure_coordinate = Some(requirement.coordinate);
                                    break;
                                }
                            }
                        }
                        if let Some(error) = failure {
                            crate::body_query::BodyTransaction::DeterministicFailure {
                                errors: crate::CompileErrors::from(error),
                                diagnostic_basis: failure_coordinate.map(|coordinate| {
                                    crate::body_query::BodyDiagnosticBasis {
                                        coordinates: std::sync::Arc::from([coordinate]),
                                    }
                                }),
                                references: references.clone(),
                                lookup_observations: lookup_observations.clone(),
                            }
                        } else {
                            transaction.clone()
                        }
                    } else {
                        transaction.clone()
                    };
                    let terminal_kind = if matches!(
                        transaction,
                        crate::body_query::BodyTransaction::Success { .. }
                    ) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    let output = QueryOutput::success(crate::body_query::BodyAnalysisBundle {
                        transaction: transaction.clone(),
                        produced_anonymous: if terminal_kind == QueryTerminalKind::Success {
                            produced_anonymous
                        } else {
                            None
                        },
                    })
                    .with_terminal_kind(terminal_kind);
                    if terminal_kind == QueryTerminalKind::Success {
                        Ok(output.with_work(
                            transaction_terminal
                                .work()
                                .iter()
                                .map(|(identity, amount)| WorkItem::new(identity.clone(), *amount))
                                .collect(),
                        ))
                    } else {
                        Ok(output)
                    }
                },
            )
            .expect("the BodyAnalysisBundle family has one canonical name")
    }};
}
