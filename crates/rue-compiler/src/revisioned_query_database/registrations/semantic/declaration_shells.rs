macro_rules! register_semantic_declaration_shells {
    ($declaration_memo_retention:ident, $occurrences_for_shells:ident, $parse_for_declaration_shells:ident, $runtime:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.declaration-shell",
                $declaration_memo_retention,
                |left: &DeclarationShellQueryValue, right: &DeclarationShellQueryValue| {
                    left == right
                },
                move |context, _, key: &DeclarationShellQueryKey| {
                    let indexed = context.query_registered(
                        &$occurrences_for_shells,
                        ModuleQueryKey(key.0.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("DeclarationOccurrenceIndex publishes typed values")
                    };
                    let value = match indexed {
                        DeclarationOccurrenceIndexValue::Failure(failure) => {
                            DeclarationShellQueryValue::Failure(
                                crate::declaration_candidate::DeclarationShellFailure::OccurrencesUnavailable(
                                    failure.clone(),
                                ),
                            )
                        }
                        DeclarationOccurrenceIndexValue::Available(index) => {
                            match index.capabilities.get(&key.0) {
                                None => DeclarationShellQueryValue::Failure(
                                    crate::declaration_candidate::DeclarationShellFailure::Absent(
                                        key.0.clone(),
                                    ),
                                ),
                                Some(crate::declaration_candidate::DeclarationOccurrenceCapability::Ambiguous { .. }) => {
                                    DeclarationShellQueryValue::Failure(
                                        crate::declaration_candidate::DeclarationShellFailure::Ambiguous(
                                            key.0.clone(),
                                        ),
                                    )
                                }
                                Some(crate::declaration_candidate::DeclarationOccurrenceCapability::Exact { .. }) => {
                                    let parsed = context.query_registered(
                                        &$parse_for_declaration_shells,
                                        ModuleQueryKey(key.0.module.clone()),
                                    )?;
                                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                                        unreachable!("ParseModule publishes typed values")
                                    };
                                    match &parsed.result {
                                        Ok(module) => match module
                                            .definitions()
                                            .evaluate_declaration_shell(&key.0)
                                        {
                                            Ok(fact) => DeclarationShellQueryValue::Available(fact),
                                            Err(failure) => DeclarationShellQueryValue::Failure(failure),
                                        },
                                        Err(_) => DeclarationShellQueryValue::Failure(
                                            crate::declaration_candidate::DeclarationShellFailure::OccurrencesUnavailable(
                                                crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                                    module: key.0.module.clone(),
                                                },
                                            ),
                                        ),
                                    }
                                }
                            }
                        }
                    };
                    let kind = if matches!(value, DeclarationShellQueryValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the DeclarationShell family has one canonical name")
    }};
}
