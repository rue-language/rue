macro_rules! register_body_warning_body_references {
    ($call_heads_for_warning_references:ident, $classifications_for_warning_references:ident, $imports_for_warning_references:ident, $runtime:ident, $shells_for_warning_references:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.warning-body-references",
                BODY_QUERY_MEMO_RETENTION,
                |left: &WarningBodyReferencesValue, right: &WarningBodyReferencesValue| {
                    left == right
                },
                move |context, _, key: &crate::body_query::BodyQueryKey| {
                    let Some(definition) = body_source_definition_key(&key.instance).cloned()
                    else {
                        return Ok(QueryOutput::success(WarningBodyReferencesValue::Available(
                            Arc::from([]),
                        )));
                    };
                    let classification = context.query_registered(
                        &$classifications_for_warning_references,
                        StableDeclarationClassificationQueryKey(definition.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(classification) = classification.outcome()
                    else {
                        unreachable!("StableDeclarationClassification publishes typed values")
                    };
                    let candidate = match classification {
                        StableDeclarationClassificationQueryValue::Selected(candidate) => candidate,
                        StableDeclarationClassificationQueryValue::Absent => {
                            return Ok(QueryOutput::success(WarningBodyReferencesValue::Failure(
                                WarningBodyReferencesFailure::ClassificationAbsent(definition),
                            ))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                        StableDeclarationClassificationQueryValue::Invalid(failure) => {
                            return Ok(QueryOutput::success(WarningBodyReferencesValue::Failure(
                                WarningBodyReferencesFailure::ClassificationInvalid(
                                    failure.clone(),
                                ),
                            ))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                    };
                    let shell = context.query_registered(
                        &$shells_for_warning_references,
                        DeclarationShellQueryKey(candidate.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(shell) = shell.outcome() else {
                        unreachable!("DeclarationShell publishes typed values")
                    };
                    let shell = match shell {
                        DeclarationShellQueryValue::Available(shell) => shell,
                        DeclarationShellQueryValue::Failure(failure) => {
                            return Ok(QueryOutput::success(WarningBodyReferencesValue::Failure(
                                WarningBodyReferencesFailure::Shell(failure.clone()),
                            ))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                    };
                    if shell.is_extern {
                        return Ok(QueryOutput::success(WarningBodyReferencesValue::Available(
                            Arc::from([]),
                        )));
                    }
                    let projection = context.query_registered(
                        &$call_heads_for_warning_references,
                        WarningCallHeadProjectionQueryKey(candidate.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(projection) = projection.outcome() else {
                        unreachable!("WarningCallHeadProjection publishes typed values")
                    };
                    let heads = match projection {
                        WarningCallHeadProjectionValue::Available(heads) => heads,
                        WarningCallHeadProjectionValue::Failure(failure) => {
                            return Ok(QueryOutput::success(WarningBodyReferencesValue::Failure(
                                failure.clone(),
                            ))
                            .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                    };
                    let mut resolved_heads = BTreeSet::new();
                    for head in heads.iter() {
                        let module = match &head.import {
                            None => None,
                            Some(import) => {
                                let import_key =
                                    crate::declaration_candidate::DeclarationImportSiteKey {
                                        declaration: candidate.clone(),
                                        occurrence: import.occurrence,
                                        specifier: import.specifier.clone(),
                                    };
                                let resolved = context.query_registered(
                                    &$imports_for_warning_references,
                                    DeclarationImportQueryKey(import_key.clone()),
                                )?;
                                let rue_query::QueryOutcome::Success(resolved) = resolved.outcome()
                                else {
                                    unreachable!("DeclarationImport publishes typed values")
                                };
                                match resolved {
                                    DeclarationImportQueryValue::Available(
                                        crate::CanonicalImportResolution::Resolved(target),
                                    ) => Some(target.clone()),
                                    DeclarationImportQueryValue::Available(resolution) => {
                                        return Ok(QueryOutput::success(
                                            WarningBodyReferencesValue::Failure(
                                                WarningBodyReferencesFailure::ImportResolution {
                                                    key: import_key.clone(),
                                                    resolution: resolution.clone(),
                                                },
                                            ),
                                        )
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                    DeclarationImportQueryValue::Failure(failure) => {
                                        return Ok(QueryOutput::success(
                                            WarningBodyReferencesValue::Failure(
                                                WarningBodyReferencesFailure::Import(
                                                    failure.clone(),
                                                ),
                                            ),
                                        )
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                }
                            }
                        };
                        resolved_heads.insert(WarningStaticCallHead {
                            module,
                            components: head.components.clone(),
                        });
                    }
                    Ok(QueryOutput::success(WarningBodyReferencesValue::Available(
                        resolved_heads.into_iter().collect::<Vec<_>>().into(),
                    )))
                },
            )
            .expect("the WarningBodyReferences family has one canonical name")
    }};
}
