macro_rules! register_body_body_closure_publications {
    ($closures_for_publication:ident, $imports_for_closure_publication:ident, $lease_for_closure_publication:ident, $names_for_closure_publication:ident, $reachability_for_publication:ident, $runtime:ident, $runtime_for_closure_publication:ident, $terminal_root_for_closure_publication:ident, $terminal_root_for_declaration_publication:ident, $terminal_root_for_reachability_publication:ident) => {{
        $runtime
                .family_with_equality_and_evaluator(
                    "compiler.body-closure-publication",
                    1,
                    |left: &Arc<rue_query::QueryTerminal<crate::body_query::BodyClosureOutput>>,
                     right: &Arc<
                        rue_query::QueryTerminal<crate::body_query::BodyClosureOutput>,
                    >| match (left.outcome(), right.outcome()) {
                        (
                            rue_query::QueryOutcome::Success(left),
                            rue_query::QueryOutcome::Success(right),
                        ) => crate::body_query::body_closure_output_equal(left, right),
                        (
                            rue_query::QueryOutcome::Failure(left),
                            rue_query::QueryOutcome::Failure(right),
                        ) => left == right,
                        _ => false,
                    },
                    move |context, _, key: &crate::body_query::BodyClosurePublicationKey| {
                        // The publication request's operational sidecar consumes
                        // body-transaction lifecycle rows plus the one
                        // reachability row whose deterministic work describes
                        // frontier scheduling. Scope the closure request itself
                        // so both warm validation and cold evaluation suppress
                        // the much larger descendant ledger while preserving
                        // query semantics.
                        let _nested_attempts = context.retain_nested_attempts_for(&[
                            "compiler.body-transaction",
                            "compiler.body-reachability",
                            "compiler.declaration-body-plan-artifacts",
                        ]);
                        let closure_fallback = $terminal_root_for_closure_publication
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .lease
                            .clone();
                        let reachability_fallback = $terminal_root_for_reachability_publication
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .lease
                            .clone();
                        let declaration_fallback = $terminal_root_for_declaration_publication
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .lease
                            .clone();
                        let validation_fallbacks = [
                            closure_fallback.clone(),
                            reachability_fallback.clone(),
                            declaration_fallback.clone(),
                        ];
                        let _validated_registered = context
                            .endorse_registered_validations_from(&validation_fallbacks)
                            .expect("body retention roots belong to this query runtime");
                        let closure = context
                            .query_registered(&$closures_for_publication, key.closure.clone())?;
                        let rue_query::QueryOutcome::Success(output) = closure.outcome() else {
                            unreachable!("BodyClosure publishes typed values")
                        };
                        let well_formed_toolchain_park = output.parked_toolchain.is_some()
                            && output.scheduling_errors.is_empty()
                            && output.fatal.is_none()
                            && output.bodies.iter().all(|body| {
                                let rue_query::QueryOutcome::Success(bundle) =
                                    body.bundle.outcome()
                                else {
                                    unreachable!("BodyAnalysisBundle publishes typed values")
                                };
                                !matches!(
                                    &bundle.transaction,
                                    crate::body_query::BodyTransaction::DeterministicFailure { .. }
                                )
                        });
                        if closure.kind() == QueryTerminalKind::Success {
                            let pending = Arc::new(
                                context
                                    .retain_observed_terminal_cone_from(
                                        &closure,
                                        &validation_fallbacks,
                                    )
                                    .expect(
                                        "registered closure validation retains its exact dependency cone",
                                    ),
                            );
                            // RUE-1584: sibling windows racing in this batch
                            // captured the publication roots before this cone
                            // was installed; hand it to them directly so they
                            // borrow instead of re-leasing the shared leaves.
                            context.publish_batch_retention_fallback(&pending);
                            context.register_attempt_handoff(PublishedBodyClosureTerminalHandoff {
                                root: $terminal_root_for_closure_publication.clone(),
                                pending: Some(pending),
                                pending_reached: Some(output.reached.iter().cloned().collect()),
                                previous: None,
                                installed: false,
                            });
                            // Install the final exact closure cone before
                            // releasing the acquisition-round reachability
                            // root, so body protection transfers without a gap.
                            context.register_attempt_handoff(
                                PublishedBodyReachabilityTerminalHandoff {
                                    root: $terminal_root_for_reachability_publication.clone(),
                                    pending: Some(Arc::new(rue_query::RetainedPinSet::new())),
                                    previous: None,
                                    installed: false,
                                },
                            );
                            // The complete closure cone now owns every
                            // declaration dependency. Release the temporary
                            // discovery bridge only after that replacement has
                            // been installed, mirroring the reachability
                            // handoff above.
                            context.register_attempt_handoff(
                                PublishedDeclarationSemanticsTerminalHandoff {
                                    root: $terminal_root_for_declaration_publication.clone(),
                                    pending: Some(Arc::new(rue_query::RetainedPinSet::new())),
                                    previous: None,
                                    installed: false,
                                },
                            );
                            let mut observed_lookup_roots = BTreeMap::new();
                            for body in output.bodies.iter() {
                                let rue_query::QueryOutcome::Success(bundle) =
                                    body.bundle.outcome()
                                else {
                                    unreachable!("BodyAnalysisBundle publishes typed values")
                                };
                                let Some(descriptors) = bundle.transaction.lookup_observations()
                                else {
                                    continue;
                                };
                                let mut observed = ObservedLookupRoot::new();
                                for (descriptor, _) in descriptors.terminals.iter() {
                                    match descriptor {
                                        LookupObservationKey::Name(lookup) => {
                                            let terminal = context.query_registered(
                                                &$names_for_closure_publication,
                                                lookup.clone(),
                                            )?;
                                            observed.record(
                                                &$names_for_closure_publication,
                                                &terminal,
                                                LookupObservationKey::Name(lookup.clone()),
                                            );
                                        }
                                        LookupObservationKey::Import(lookup) => {
                                            let terminal = context.query_registered(
                                                &$imports_for_closure_publication,
                                                lookup.clone(),
                                            )?;
                                            observed.record(
                                                &$imports_for_closure_publication,
                                                &terminal,
                                                LookupObservationKey::Import(lookup.clone()),
                                            );
                                        }
                                    }
                                }
                                observed_lookup_roots
                                    .insert(body_lookup_root_identity(&body.key), observed);
                            }
                            context.register_attempt_handoff(PublishedBodyClosureLookupHandoff {
                                lease: $lease_for_closure_publication.clone(),
                                $runtime: $runtime_for_closure_publication.clone(),
                                observed: Some(observed_lookup_roots),
                                retire_absent: true,
                                rollback: None,
                            });
                        } else if well_formed_toolchain_park {
                            let reachability = context.query_registered(
                                &$reachability_for_publication,
                                key.closure.clone(),
                            )?;
                            let rue_query::QueryOutcome::Success(reachability_output) =
                                reachability.outcome()
                            else {
                                unreachable!("BodyReachability publishes typed values")
                            };
                            assert!(
                                reachability_output.parked_toolchain.is_some()
                                    && reachability_output.scheduling_errors.is_empty()
                                    && reachability_output.fatal.is_none(),
                                "a well-formed parked closure retains its exact reachability cone"
                            );
                            let pending = Arc::new(
                                context
                                    .retain_observed_terminal_cone_from(
                                        &reachability,
                                        &validation_fallbacks,
                                    )
                                    .expect(
                                        "registered reachability validation retains its exact dependency cone",
                                    ),
                            );
                            // RUE-1584: same sibling handoff as the complete
                            // closure arm above.
                            context.publish_batch_retention_fallback(&pending);
                            context.register_attempt_handoff(
                                PublishedBodyReachabilityTerminalHandoff {
                                    root: $terminal_root_for_reachability_publication.clone(),
                                    pending: Some(pending),
                                    previous: None,
                                    installed: false,
                                },
                            );
                            context.register_attempt_handoff(
                                PublishedDeclarationSemanticsTerminalHandoff {
                                    root: $terminal_root_for_declaration_publication.clone(),
                                    pending: Some(Arc::new(rue_query::RetainedPinSet::new())),
                                    previous: None,
                                    installed: false,
                                },
                            );
                        }
                        Ok(
                            QueryOutput::success(closure.clone())
                                .with_terminal_kind(closure.kind()),
                        )
                    },
                )
                .expect("the BodyClosurePublication family has one canonical name")
    }};
}
