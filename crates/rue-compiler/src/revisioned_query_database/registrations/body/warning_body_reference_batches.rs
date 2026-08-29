macro_rules! register_body_warning_body_reference_batches {
    ($call_heads_for_warning_batch:ident, $classifications_for_warning_batch:ident, $closure_root_for_warning_batch:ident, $declaration_root_for_warning_batch:ident, $reachability_root_for_warning_batch:ident, $runtime:ident, $shells_for_warning_batch:ident, $warning_body_references_for_batch:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.warning-body-reference-frontier",
                1,
                |left: &WarningBodyReferencesBatchValue,
                 right: &WarningBodyReferencesBatchValue| {
                    left.values == right.values
                },
                move |context, _, key: &WarningBodyReferencesBatchKey| {
                    let declaration_fallback = $declaration_root_for_warning_batch
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .lease
                        .clone();
                    let closure_fallback = $closure_root_for_warning_batch
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .lease
                        .clone();
                    let reachability_fallback = $reachability_root_for_warning_batch
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .lease
                        .clone();
                    let semantic_fallbacks = [
                        declaration_fallback,
                        closure_fallback,
                        reachability_fallback,
                    ];
                    let _validated_registered = context
                        .endorse_registered_validations_from(&semantic_fallbacks)
                        .expect("warning frontier uses this query runtime");
                    // Warning call-head projections are warning-only and are
                    // not members of the semantic publication roots. Observe
                    // their exact currently-demanded frontier first, then lend
                    // that complete cone to the warning-body children. This is
                    // still the canonical projection family; no facts are
                    // recomputed or copied into a peer authority.
                    let classification_keys = key
                        .bodies
                        .iter()
                        .filter_map(|key| body_source_definition_key(&key.instance).cloned())
                        .map(StableDeclarationClassificationQueryKey)
                        .collect::<Vec<_>>();
                    let classifications = context.query_registered_adaptive_batch_refs(
                        &$classifications_for_warning_batch,
                        classification_keys.iter(),
                    )?;
                    let shell_keys = classifications
                        .iter()
                        .filter_map(|terminal| {
                            let rue_query::QueryOutcome::Success(classification) =
                                terminal.outcome()
                            else {
                                unreachable!(
                                    "StableDeclarationClassification publishes typed values"
                                )
                            };
                            match classification {
                                StableDeclarationClassificationQueryValue::Selected(candidate) => {
                                    Some(DeclarationShellQueryKey(candidate.clone()))
                                }
                                StableDeclarationClassificationQueryValue::Absent
                                | StableDeclarationClassificationQueryValue::Invalid(_) => None,
                            }
                        })
                        .collect::<Vec<_>>();
                    let shells = context.query_registered_adaptive_batch_refs(
                        &$shells_for_warning_batch,
                        shell_keys.iter(),
                    )?;
                    let call_head_keys = shell_keys
                        .iter()
                        .zip(shells.iter())
                        .filter_map(|(key, terminal)| {
                            let rue_query::QueryOutcome::Success(shell) = terminal.outcome() else {
                                unreachable!("DeclarationShell publishes typed values")
                            };
                            match shell {
                                DeclarationShellQueryValue::Available(shell)
                                    if !shell.is_extern =>
                                {
                                    Some(WarningCallHeadProjectionQueryKey(key.0.clone()))
                                }
                                DeclarationShellQueryValue::Available(_)
                                | DeclarationShellQueryValue::Failure(_) => None,
                            }
                        })
                        .collect::<Vec<_>>();
                    let call_heads = context.query_registered_adaptive_batch_refs(
                        &$call_heads_for_warning_batch,
                        call_head_keys.iter(),
                    )?;
                    let call_head_fallback = Arc::new(
                        context
                            .retain_observed_terminal_cones_from(&call_heads, &semantic_fallbacks)
                            .expect("warning frontier observes exact call-head cones"),
                    );
                    let fallbacks = [
                        semantic_fallbacks[0].clone(),
                        semantic_fallbacks[1].clone(),
                        semantic_fallbacks[2].clone(),
                        call_head_fallback,
                    ];
                    let _validated_children = context
                        .endorse_registered_validations_from(&fallbacks)
                        .expect("warning child frontier uses this query runtime");
                    context.record_work(rue_query::WorkItem::new(
                        "warning-reference.frontier.items",
                        key.bodies.len() as u64,
                    ));
                    context.record_work(rue_query::WorkItem::new(
                        "warning-reference.frontier.batches",
                        1,
                    ));
                    context.record_work(rue_query::WorkItem::new(
                        "warning-reference.frontier.overhead",
                        1,
                    ));
                    let _attempts =
                        context.retain_nested_attempts_for(&["compiler.warning-body-references"]);
                    let terminals = context.query_registered_adaptive_batch_refs(
                        &$warning_body_references_for_batch,
                        key.bodies.iter(),
                    )?;
                    let values = terminals
                        .iter()
                        .map(|terminal| {
                            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                                unreachable!("WarningBodyReferences publishes typed values")
                            };
                            value.clone()
                        })
                        .collect::<Vec<_>>();
                    let retained_children = Arc::new(
                        context
                            .retain_observed_terminal_cones_from(&terminals, &fallbacks)
                            .expect("warning frontier observes every child cone"),
                    );
                    let kind = if values
                        .iter()
                        .all(|value| matches!(value, WarningBodyReferencesValue::Available(_)))
                    {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(WarningBodyReferencesBatchValue {
                        values: values.into(),
                        _retained_children: retained_children,
                    })
                    .with_terminal_kind(kind))
                },
            )
            .expect("the WarningBodyReferenceFrontier family has one canonical name")
    }};
}
