macro_rules! register_body_body_analysis_bundles {
    ($produced_for_analysis_bundle:ident, $runtime:ident, $transactions_for_analysis_bundle:ident) => {{
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
                        produced_anonymous,
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
