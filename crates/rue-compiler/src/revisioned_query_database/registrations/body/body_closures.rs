macro_rules! register_body_body_closures {
    ($anonymous_digest_forcing_for_closure_aggregation:ident, $bundles_for_closure:ident, $declarations_for_closure_aggregation:ident, $reachability_for_closure:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.body-closure",
                BODY_CLOSURE_MEMO_RETENTION,
                crate::body_query::body_closure_output_equal,
                move |context, _, key: &crate::body_query::BodyClosureQueryKey| {
                    #[cfg(test)]
                    {
                        $anonymous_digest_forcing_for_closure_aggregation
                            .lock()
                            .expect("body-closure forced-digest state is not poisoned")
                            .sealed = true;
                    }
                    let reachability =
                        context.query_registered(&$reachability_for_closure, key.clone())?;
                    let rue_query::QueryOutcome::Success(reachability) = reachability.outcome()
                    else {
                        unreachable!("BodyReachability publishes typed values")
                    };
                    let declarations = context.query_registered(
                        &$declarations_for_closure_aggregation,
                        SemanticNucleusProjectionKey {
                            modules: key.modules.clone(),
                            configuration: key.configuration.clone(),
                        },
                    )?;
                    let rue_query::QueryOutcome::Success(declarations) = declarations.outcome()
                    else {
                        unreachable!("DeclarationSemanticsProjection publishes typed values")
                    };
                    let mut anonymous_digest_owners = BTreeMap::new();
                    let mut anonymous_digest_collision = None;
                    let body_closure_anonymous_digest =
                        |nominal: &crate::durable_semantics::DurableAnonymousNominal| {
                            #[cfg(test)]
                            {
                                let canonical = nominal.identity.with_canonical_producer();
                                if let Some(digest) =
                                    $anonymous_digest_forcing_for_closure_aggregation
                                        .lock()
                                        .expect("body-closure forced-digest state is not poisoned")
                                        .digests
                                        .get(canonical.as_ref())
                                        .copied()
                                {
                                    return digest;
                                }
                                compiler_anonymous_identity_digest(&nominal.identity)
                            }
                            #[cfg(not(test))]
                            {
                                nominal.anonymous_identity_digest()
                            }
                        };
                    if let SemanticNucleusProjectionValue::Available { projection, .. } =
                        declarations
                    {
                        for nominal in projection.anonymous_nominals.iter() {
                            register_body_closure_anonymous_digest(
                                &mut anonymous_digest_owners,
                                &mut anonymous_digest_collision,
                                body_closure_anonymous_digest(nominal),
                                &nominal.identity,
                            );
                        }
                    }
                    let body_keys = reachability
                        .reached
                        .iter()
                        .cloned()
                        .map(|instance| {
                            crate::body_query::BodyQueryKey::new(
                                instance,
                                key.configuration.clone(),
                            )
                        })
                        .collect::<Vec<_>>();
                    // Reachability has already produced these deep registered
                    // cones through BodyTransaction. Consume the final bundles
                    // in this one endorsed task so validation certificates are
                    // shared across bodies; a second batch would isolate each
                    // proof and recursively revalidate the same semantic cone.
                    let mut bodies = Vec::with_capacity(body_keys.len());
                    let mut has_deterministic_failure = false;
                    let mut fatal = reachability.fatal.clone();
                    for body_key in body_keys {
                        let bundle =
                            context.query_registered(&$bundles_for_closure, body_key.clone())?;
                        let rue_query::QueryOutcome::Success(bundle_value) = bundle.outcome()
                        else {
                            unreachable!("BodyAnalysisBundle publishes typed values")
                        };
                        has_deterministic_failure |= matches!(
                            bundle_value.transaction,
                            crate::body_query::BodyTransaction::DeterministicFailure { .. }
                        );
                        match bundle_value.produced_anonymous.as_ref() {
                            Some(crate::body_query::ProducedAnonymous::ProducerFailed(failure)) => {
                                fatal.get_or_insert_with(|| {
                                    crate::body_query::BodyClosureFatal::ProducerFailed {
                                        instance: body_key.instance.clone(),
                                        failure: failure.clone(),
                                    }
                                });
                            }
                            Some(crate::body_query::ProducedAnonymous::Produced(produced)) => {
                                for nominal in produced.0.iter() {
                                    register_body_closure_anonymous_digest(
                                        &mut anonymous_digest_owners,
                                        &mut anonymous_digest_collision,
                                        body_closure_anonymous_digest(nominal),
                                        &nominal.identity,
                                    );
                                }
                            }
                            None => {}
                        }
                        bodies.push(crate::body_query::BodyClosureBody {
                            key: body_key,
                            bundle,
                        });
                    }
                    if fatal.is_none()
                        && reachability.scheduling_errors.is_empty()
                        && reachability.parked_toolchain.is_none()
                        && let Some((digest, first, second)) = anonymous_digest_collision
                    {
                        fatal = Some(
                            crate::body_query::BodyClosureFatal::AnonymousDigestCollision {
                                digest,
                                first,
                                second,
                            },
                        );
                    }
                    let output = crate::body_query::BodyClosureOutput {
                        reached: reachability.reached.clone(),
                        demanded_drop_glue: reachability.demanded_drop_glue.clone(),
                        demanded_drop_glue_plans: reachability.demanded_drop_glue_plans.clone(),
                        demanded_error_printers: reachability.demanded_error_printers.clone(),
                        bodies: bodies.into(),
                        scheduling_errors: reachability.scheduling_errors.clone(),
                        fatal,
                        parked_toolchain: reachability.parked_toolchain.clone(),
                    };
                    let terminal_kind = if output.scheduling_errors.is_empty()
                        && output.fatal.is_none()
                        && output.parked_toolchain.is_none()
                        && !has_deterministic_failure
                    {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(output).with_terminal_kind(terminal_kind))
                },
            )
            .expect("the BodyClosure family has one canonical name")
    }};
}
