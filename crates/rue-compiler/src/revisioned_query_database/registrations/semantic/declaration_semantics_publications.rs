macro_rules! register_semantic_declaration_semantics_publications {
    ($artifacts_for_declaration_publication:ident, $closure_root_for_declaration_publication:ident, $cone_retention_failures_for_declaration_publication:ident, $projection_for_declaration_publication:ident, $reachability_root_for_declaration_publication:ident, $root_for_declaration_publication:ident, $runtime:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.declaration-semantics-publication",
                0,
                |left: &Arc<rue_query::QueryTerminal<SemanticNucleusProjectionValue>>,
                 right: &Arc<rue_query::QueryTerminal<SemanticNucleusProjectionValue>>| {
                    match (left.outcome(), right.outcome()) {
                        (
                            rue_query::QueryOutcome::Success(left),
                            rue_query::QueryOutcome::Success(right),
                        ) => left == right,
                        (
                            rue_query::QueryOutcome::Failure(left),
                            rue_query::QueryOutcome::Failure(right),
                        ) => left == right,
                        _ => false,
                    }
                },
                move |context, _, key: &SemanticNucleusProjectionKey| {
                    // The preceding successful body graph already retains this
                    // projection's exact dependency cone. Borrow it while
                    // discovering the next root set, then retain only the
                    // observed candidate artifacts for the gap before body
                    // closure starts.
                    let declaration_fallback = $root_for_declaration_publication
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .lease
                        .clone();
                    let closure_fallback = $closure_root_for_declaration_publication
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .lease
                        .clone();
                    let reachability_fallback = $reachability_root_for_declaration_publication
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .lease
                        .clone();
                    let validation_fallbacks = [
                        declaration_fallback,
                        closure_fallback,
                        reachability_fallback,
                    ];
                    let _validated_registered = context
                        .endorse_registered_validations_from(&validation_fallbacks)
                        .expect("semantic publication roots belong to this query runtime");
                    let projection = context.query_registered(
                        &$projection_for_declaration_publication,
                        key.clone(),
                    )?;
                    let mut pending = context
                        .retain_observed_family(&$artifacts_for_declaration_publication)
                        .expect("candidate artifacts belong to this query runtime");
                    // Best-effort: an unretained cone leaves the lease exactly
                    // as it is today — the successor scope re-leases through
                    // demand cascades instead of borrowing. That degradation is
                    // counted (and asserted in debug builds) so it can never
                    // read as the stronger run: the session gate pins the
                    // counter and the miss count to zero on maintained
                    // workloads.
                    match context
                        .retain_observed_terminal_cone_from(&projection, &validation_fallbacks)
                    {
                        Ok(cone) => pending.absorb(cone),
                        Err(error) => {
                            debug_assert!(
                                false,
                                "declaration publication could not retain its projection cone: {error:?}"
                            );
                            $cone_retention_failures_for_declaration_publication
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    context.register_attempt_handoff(
                        PublishedDeclarationSemanticsTerminalHandoff {
                            root: $root_for_declaration_publication.clone(),
                            pending: Some(Arc::new(pending)),
                            previous: None,
                            installed: false,
                        },
                    );
                    Ok(
                        QueryOutput::success(projection.clone())
                            .with_terminal_kind(projection.kind()),
                    )
                },
            )
            .expect("the DeclarationSemanticsPublication family has one canonical name")
    }};
}
