macro_rules! register_parse_import_declaration_imports {
    ($declaration_memo_retention:ident, $occurrences_for_declaration_import:ident, $parse_for_declaration_import:ident, $resolve_for_declaration_import:ident, $runtime:ident, $shells_for_declaration_import:ident, $test_imports_for_declaration_import:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.declaration-import",
                $declaration_memo_retention,
                |left: &DeclarationImportQueryValue, right: &DeclarationImportQueryValue| {
                    left == right
                },
                move |context, _, key: &DeclarationImportQueryKey| {
                    use crate::declaration_candidate::{
                        DeclarationCandidateCategory, DeclarationImportFailure,
                        DeclarationOccurrenceCapability, DeclarationShellFailure,
                    };
                    use crate::parsed_modules::ParsedDeclarationImportFailure;

                    let indexed = context.query_registered(
                        &$occurrences_for_declaration_import,
                        ModuleQueryKey(key.0.declaration.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("DeclarationOccurrenceIndex publishes typed values")
                    };
                    let value = match indexed {
                        DeclarationOccurrenceIndexValue::Failure(failure) => {
                            DeclarationImportQueryValue::Failure(
                                DeclarationImportFailure::OccurrencesUnavailable(failure.clone()),
                            )
                        }
                        DeclarationOccurrenceIndexValue::Available(index) => {
                            match index.capabilities.get(&key.0.declaration) {
                                None => DeclarationImportQueryValue::Failure(
                                    DeclarationImportFailure::AbsentDeclaration(key.0.clone()),
                                ),
                                Some(DeclarationOccurrenceCapability::Ambiguous { .. }) => {
                                    DeclarationImportQueryValue::Failure(
                                        DeclarationImportFailure::AmbiguousDeclaration(
                                            key.0.clone(),
                                        ),
                                    )
                                }
                                Some(DeclarationOccurrenceCapability::Exact {
                                    duplicate_multiplicity: 0,
                                    ..
                                }) => DeclarationImportQueryValue::Failure(
                                    DeclarationImportFailure::ParserCapabilityMismatch(
                                        key.0.clone(),
                                    ),
                                ),
                                Some(DeclarationOccurrenceCapability::Exact { .. }) => {
                                    let shell = context.query_registered(
                                        &$shells_for_declaration_import,
                                        DeclarationShellQueryKey(key.0.declaration.clone()),
                                    )?;
                                    let rue_query::QueryOutcome::Success(shell) = shell.outcome()
                                    else {
                                        unreachable!("DeclarationShell publishes typed values")
                                    };
                                    match shell {
                                        DeclarationShellQueryValue::Failure(failure) => {
                                            let failure = match failure {
                                                DeclarationShellFailure::OccurrencesUnavailable(
                                                    failure,
                                                ) => DeclarationImportFailure::OccurrencesUnavailable(
                                                    failure.clone(),
                                                ),
                                                DeclarationShellFailure::Absent(_) => {
                                                    DeclarationImportFailure::AbsentDeclaration(
                                                        key.0.clone(),
                                                    )
                                                }
                                                DeclarationShellFailure::Ambiguous(_) => {
                                                    DeclarationImportFailure::AmbiguousDeclaration(
                                                        key.0.clone(),
                                                    )
                                                }
                                                DeclarationShellFailure::ParserCapabilityMismatch(
                                                    _,
                                                ) => DeclarationImportFailure::ParserCapabilityMismatch(
                                                    key.0.clone(),
                                                ),
                                            };
                                            DeclarationImportQueryValue::Failure(failure)
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if !matches!(
                                                fact.key.category,
                                                DeclarationCandidateCategory::ConstCandidate
                                                    | DeclarationCandidateCategory::Function
                                                    | DeclarationCandidateCategory::Method
                                                    | DeclarationCandidateCategory::AssociatedFunction
                                                    | DeclarationCandidateCategory::Destructor
                                            ) =>
                                        {
                                            DeclarationImportQueryValue::Failure(
                                                DeclarationImportFailure::CategoryMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if fact.key != key.0.declaration =>
                                        {
                                            DeclarationImportQueryValue::Failure(
                                                DeclarationImportFailure::ParserCapabilityMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(_) => {
                                            let parsed = context.query_registered(
                                                &$parse_for_declaration_import,
                                                ModuleQueryKey(key.0.declaration.module.clone()),
                                            )?;
                                            let rue_query::QueryOutcome::Success(parsed) =
                                                parsed.outcome()
                                            else {
                                                unreachable!("ParseModule publishes typed values")
                                            };
                                            match &parsed.result {
                                                Err(_) => DeclarationImportQueryValue::Failure(
                                                    DeclarationImportFailure::OccurrencesUnavailable(
                                                        crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                                            module: key.0.declaration.module.clone(),
                                                        },
                                                    ),
                                                ),
                                                Ok(module) => match module.declaration_import(&key.0)
                                                {
                                                    Err(ParsedDeclarationImportFailure::SiteOutOfRange {
                                                        available,
                                                    }) => DeclarationImportQueryValue::Failure(
                                                        DeclarationImportFailure::SiteOutOfRange {
                                                            key: key.0.clone(),
                                                            available,
                                                        },
                                                    ),
                                                    Err(ParsedDeclarationImportFailure::SpecifierMismatch {
                                                        actual,
                                                    }) => DeclarationImportQueryValue::Failure(
                                                        DeclarationImportFailure::SpecifierMismatch {
                                                            key: key.0.clone(),
                                                            actual,
                                                        },
                                                    ),
                                                    Err(ParsedDeclarationImportFailure::CapabilityMismatch) => {
                                                        DeclarationImportQueryValue::Failure(
                                                            DeclarationImportFailure::ParserCapabilityMismatch(
                                                                key.0.clone(),
                                                            ),
                                                        )
                                                    }
                                                    Ok(site) => {
                                                        #[cfg(test)]
                                                        {
                                                            let view = $test_imports_for_declaration_import
                                                                .lock()
                                                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                                                .revisions
                                                                .iter()
                                                                .find(|view| view.revision == context.revision())
                                                                .cloned();
                                                            if let Some(view) = view {
                                                                context.input(test_import_graph_input())?;
                                                                let normalized = rue_air::normalize_module_path(
                                                                    site.specifier(),
                                                                );
                                                                let value = view
                                                                    .graph
                                                                    .records()
                                                                    .iter()
                                                                    .find(|record| {
                                                                        record.importer() == site.importer()
                                                                            && record.normalized_specifier()
                                                                                == normalized
                                                                    })
                                                                    .map(|record| {
                                                                        DeclarationImportQueryValue::Available(
                                                                            record.resolution().clone(),
                                                                        )
                                                                    })
                                                                    .unwrap_or_else(|| {
                                                                        DeclarationImportQueryValue::Failure(
                                                                            DeclarationImportFailure::ResolutionUnavailable(
                                                                                key.0.clone(),
                                                                            ),
                                                                        )
                                                                });
                                                                let kind = if matches!(
                                                                    value,
                                                                    DeclarationImportQueryValue::Available(_)
                                                                ) {
                                                                    QueryTerminalKind::Success
                                                                } else {
                                                                    QueryTerminalKind::Failure
                                                                };
                                                                return Ok(QueryOutput::success(value)
                                                                    .with_terminal_kind(kind));
                                                            }
                                                        }
                                                        let resolved = context.query_registered(
                                                            &$resolve_for_declaration_import,
                                                            ResolveImportKey {
                                                                occurrence: crate::ImportOccurrenceKey::from_directive(&site),
                                                                mode: ImportDemandMode::Rooted,
                                                            },
                                                        )?;
                                                        let rue_query::QueryOutcome::Success(resolved) =
                                                            resolved.outcome()
                                                        else {
                                                            unreachable!("ResolveImport publishes typed values")
                                                        };
                                                        if !resolved.site_found {
                                                            DeclarationImportQueryValue::Failure(
                                                                DeclarationImportFailure::ParserCapabilityMismatch(
                                                                    key.0.clone(),
                                                                ),
                                                            )
                                                        } else if let Some(resolution) =
                                                            &resolved.resolution
                                                        {
                                                            DeclarationImportQueryValue::Available(
                                                                resolution.clone(),
                                                            )
                                                        } else {
                                                            DeclarationImportQueryValue::Failure(
                                                                DeclarationImportFailure::ResolutionUnavailable(
                                                                    key.0.clone(),
                                                                ),
                                                            )
                                                        }
                                                    }
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    };
                    let kind = if matches!(value, DeclarationImportQueryValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the DeclarationImport family has one canonical name")
    }};
}
