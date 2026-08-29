macro_rules! register_semantic_stable_declaration_classifications {
    ($declaration_memo_retention:ident, $occurrences_for_stable_classification:ident, $runtime:ident, $shells_for_stable_classification:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.stable-declaration-classification",
                $declaration_memo_retention,
                |left: &StableDeclarationClassificationQueryValue,
                 right: &StableDeclarationClassificationQueryValue| left == right,
                move |context, _, key: &StableDeclarationClassificationQueryKey| {
                    use crate::declaration_candidate::{
                        DeclarationOccurrenceCapability, DeclarationShellFailure,
                    };

                    let Some(candidates) = stable_syntax_candidate_set(&key.0) else {
                        return Ok(QueryOutput::success(
                            StableDeclarationClassificationQueryValue::Invalid(
                                StableDeclarationClassificationFailure::MalformedStableKey(
                                    key.0.clone(),
                                ),
                            ),
                        )
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    };
                    let indexed = context.query_registered(
                        &$occurrences_for_stable_classification,
                        ModuleQueryKey(key.0.module().clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("DeclarationOccurrenceIndex publishes typed values")
                    };
                    let value = match indexed {
                        DeclarationOccurrenceIndexValue::Failure(failure) => {
                            StableDeclarationClassificationQueryValue::Invalid(
                                StableDeclarationClassificationFailure::OccurrencesUnavailable(
                                    failure.clone(),
                                ),
                            )
                        }
                        DeclarationOccurrenceIndexValue::Available(index) => {
                            let mut selected = None;
                            let mut invalid = None;
                            for candidate in candidates.into_iter().flatten() {
                                match index.capabilities.get(&candidate) {
                                    None => {}
                                    Some(DeclarationOccurrenceCapability::Ambiguous { .. }) => {
                                        invalid.get_or_insert(
                                            StableDeclarationClassificationFailure::Ambiguous(
                                                candidate,
                                            ),
                                        );
                                    }
                                    Some(DeclarationOccurrenceCapability::Exact {
                                        duplicate_multiplicity,
                                        ..
                                    }) if *duplicate_multiplicity != 1 => {
                                        invalid.get_or_insert(
                                            StableDeclarationClassificationFailure::DuplicateMultiplicity {
                                                key: candidate,
                                                multiplicity: *duplicate_multiplicity,
                                            },
                                        );
                                    }
                                    Some(DeclarationOccurrenceCapability::Exact { .. }) => {
                                        let shell = context.query_registered(
                                            &$shells_for_stable_classification,
                                            DeclarationShellQueryKey(candidate.clone()),
                                        )?;
                                        let rue_query::QueryOutcome::Success(shell) =
                                            shell.outcome()
                                        else {
                                            unreachable!(
                                                "DeclarationShell publishes typed values"
                                            )
                                        };
                                        match shell {
                                            DeclarationShellQueryValue::Available(fact)
                                                if fact.key == candidate =>
                                            {
                                                if let Some(first) =
                                                    selected.replace(candidate.clone())
                                                {
                                                    invalid.get_or_insert(
                                                        StableDeclarationClassificationFailure::MultipleAvailable {
                                                            first,
                                                            second: candidate,
                                                        },
                                                    );
                                                }
                                            }
                                            DeclarationShellQueryValue::Available(_) => {
                                                invalid.get_or_insert(
                                                    StableDeclarationClassificationFailure::ParserCapabilityMismatch(
                                                        candidate,
                                                    ),
                                                );
                                            }
                                            DeclarationShellQueryValue::Failure(
                                                DeclarationShellFailure::OccurrencesUnavailable(
                                                    failure,
                                                ),
                                            ) => {
                                                invalid.get_or_insert(
                                                    StableDeclarationClassificationFailure::OccurrencesUnavailable(
                                                        failure.clone(),
                                                    ),
                                                );
                                            }
                                            DeclarationShellQueryValue::Failure(
                                                DeclarationShellFailure::Ambiguous(_),
                                            ) => {
                                                invalid.get_or_insert(
                                                    StableDeclarationClassificationFailure::Ambiguous(
                                                        candidate,
                                                    ),
                                                );
                                            }
                                            DeclarationShellQueryValue::Failure(
                                                DeclarationShellFailure::Absent(_)
                                                | DeclarationShellFailure::ParserCapabilityMismatch(
                                                    _,
                                                ),
                                            ) => {
                                                invalid.get_or_insert(
                                                    StableDeclarationClassificationFailure::ParserCapabilityMismatch(
                                                        candidate,
                                                    ),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(failure) = invalid {
                                StableDeclarationClassificationQueryValue::Invalid(failure)
                            } else if let Some(candidate) = selected {
                                StableDeclarationClassificationQueryValue::Selected(candidate)
                            } else {
                                StableDeclarationClassificationQueryValue::Absent
                            }
                        }
                    };
                    let kind = if matches!(
                        value,
                        StableDeclarationClassificationQueryValue::Invalid(_)
                    ) {
                        QueryTerminalKind::Failure
                    } else {
                        QueryTerminalKind::Success
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the StableDeclarationClassification family has one canonical name")
    }};
}
