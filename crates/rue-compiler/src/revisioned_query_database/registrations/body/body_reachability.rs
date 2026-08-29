macro_rules! register_body_body_reachability {
    ($call_abis_for_body_reachability:ident, $declarations_for_body_closure:ident, $drop_glues_for_body_reachability:ident, $input_for_body_closure:ident, $produced_for_body_reachability:ident, $runtime:ident, $toolchain_for_body_closure:ident, $transactions_for_body_reachability:ident, $type_facts_for_body_reachability:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.body-reachability",
                BODY_CLOSURE_MEMO_RETENTION,
                crate::body_query::body_reachability_output_equal,
                move |context, _, key: &crate::body_query::BodyClosureQueryKey| {
                    assert!(
                        key.modules.windows(2).all(|pair| pair[0] < pair[1])
                            && key.roots.windows(2).all(|pair| pair[0] < pair[1])
                    );
                    let declarations = context.query_registered(
                        &$declarations_for_body_closure,
                        SemanticNucleusProjectionKey {
                            modules: key.modules.clone(),
                            configuration: key.configuration.clone(),
                        },
                    )?;
                    let rue_query::QueryOutcome::Success(declarations) = declarations.outcome()
                    else {
                        unreachable!("DeclarationSemanticsProjection publishes typed values")
                    };
                    let projection = match declarations {
                        SemanticNucleusProjectionValue::Available(projection) => projection,
                        SemanticNucleusProjectionValue::Failure {
                            declaration,
                            failure,
                        } => {
                            return Ok(QueryOutput::success(
                                crate::body_query::BodyReachabilityOutput {
                                    reached: Arc::from([]),
                                    demanded_drop_glue: Arc::from([]),
                                    demanded_drop_glue_plans: Arc::from([]),
                                    scheduling_errors: Arc::from([]),
                                    fatal: Some(
                                        crate::body_query::BodyClosureFatal::DeclarationFailed {
                                            declaration: declaration.clone(),
                                            failure: failure.clone(),
                                        },
                                    ),
                                    parked_toolchain: None,
                                },
                            )
                            .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                    };
                    let present_trusted_modules = key
                        .modules
                        .iter()
                        .filter(|module| module.is_trusted_standard_library())
                        .map(|module| Arc::<str>::from(module.as_str()))
                        .collect::<BTreeSet<_>>();
                    let roots = key
                        .roots
                        .iter()
                        .cloned()
                        .map(Arc::new)
                        .collect::<BTreeSet<_>>();
                    let mut pending = roots
                        .iter()
                        .cloned()
                        .map(|root| (root, 0usize))
                        .collect::<BTreeMap<_, _>>();
                    let mut blocked_on_anonymous = BTreeMap::<
                        Arc<crate::FunctionInstanceKey>,
                        BTreeSet<Arc<crate::FunctionInstanceKey>>,
                    >::new();
                    let mut visited = BTreeSet::new();
                    let mut failed_instances = BTreeSet::new();
                    let mut anonymous_dependency_pending = roots
                        .iter()
                        .cloned()
                        .map(|root| (root, 0usize))
                        .collect::<Vec<_>>();
                    let mut reached_body_keys = Vec::new();
                    let mut demanded_drop_glue = BTreeSet::new();
                    let mut demanded_drop_glue_plans = BTreeMap::new();
                    let mut pending_drop_glue = BTreeSet::new();
                    let mut visited_drop_glue = BTreeSet::new();
                    let mut scheduling_errors = BTreeMap::new();
                    let mut fatal = None;
                    let mut parked_toolchain = None;
                    let mut produced_anonymous = BTreeMap::new();
                    let mut ready_frontier = BTreeMap::new();
                    let mut pending_frontier_metrics = None;
                    let mut prefetched_transactions = VecDeque::<(
                        Arc<crate::FunctionInstanceKey>,
                        usize,
                        Option<
                            Arc<
                                rue_query::QueryTerminal<crate::body_query::BodyTransaction>,
                            >,
                        >,
                    )>::new();
                    let mut anonymous_visit_seen = AHashSet::new();
                    // Producer closures already derived in this evaluation (RUE-1557).
                    let mut anonymous_producer_closures: AHashMap<
                        Arc<crate::FunctionInstanceKey>,
                        Arc<[Arc<crate::FunctionInstanceKey>]>,
                    > = AHashMap::new();
                    // Toolchain demands observed so far in this evaluation.
                    //
                    // The parking path unions the missing modules of every body
                    // still ready or pending, and it can run more than once per
                    // request, so the same body's demand was re-queried on each
                    // sweep — a deep `BodyQueryKey` rebuilt and a registration
                    // performed per body per parking event (RUE-1562). The
                    // demand is a pure function of the key at this revision, so
                    // one evaluation-scoped cache answers every repeat.
                    //
                    // Only repeats are skipped: a body whose demand has not
                    // been observed yet is still queried here, so the set of
                    // dependency edges this evaluation records is unchanged.
                    let mut observed_toolchain_demands: AHashMap<
                        Arc<crate::FunctionInstanceKey>,
                        crate::BodyToolchainDemand,
                    > = AHashMap::new();
                    let concurrency = context.max_concurrency();
                    let body_query_prefetch_window = if concurrency == 1 {
                        1
                    } else {
                        concurrency.saturating_mul(2).min(64)
                    };
                    'schedule: loop {
                        // Every insertion flows through `schedule_body_instance`, which
                        // rejects visited bodies. The deferred-producer retry is the sole
                        // direct reinsertion and removes its body from `visited` first.
                        // Keep that ownership invariant explicit without sweeping the
                        // complete pending frontier before every scheduled body.
                        debug_assert!(pending.keys().all(|instance| !visited.contains(instance)));
                        if ready_frontier.is_empty() && prefetched_transactions.is_empty() {
                            // Close the statically encoded anonymous-producer
                            // graph before selecting a ready frontier. A body
                            // can name a producer through its instance key
                            // before BodyTransaction has a chance to publish a
                            // dynamic DeferredAnonymousProducers edge. The old
                            // serial stack discovered those producers while
                            // popping one body; the frontier scheduler makes
                            // the same dependency graph explicit first.
                            while let Some((instance, current_depth)) =
                                anonymous_dependency_pending.pop()
                            {
                                context.check_canceled()?;
                                let producers = instance_producer_closure(
                                    context,
                                    &mut anonymous_producer_closures,
                                    &mut anonymous_visit_seen,
                                    &instance,
                                );
                                for producer in producers.iter() {
                                    let depth = current_depth
                                        + usize::from(matches!(
                                            producer.as_ref(),
                                            crate::FunctionInstanceKey::Specialization { .. }
                                        ));
                                    if let Some((producer, true)) = schedule_body_instance(
                                        &mut pending,
                                        &mut ready_frontier,
                                        &mut prefetched_transactions,
                                        &visited,
                                        producer.as_ref(),
                                        depth,
                                    ) {
                                        anonymous_dependency_pending.push((producer, depth));
                                    }
                                }
                            }
                            context.record_work(rue_query::WorkItem::new(
                                "reachability.frontier.scans",
                                1,
                            ));
                            context.record_work(rue_query::WorkItem::new(
                                "reachability.frontier.scan-keys",
                                pending.len() as u64,
                            ));
                            let mut frontier = Vec::new();
                            let mut blocked_pending = BTreeMap::new();
                            for (instance, depth) in std::mem::take(&mut pending) {
                                let producers = instance_producer_closure(
                                    context,
                                    &mut anonymous_producer_closures,
                                    &mut anonymous_visit_seen,
                                    &instance,
                                );
                                let all_producers_visited =
                                    producers.iter().all(|producer| visited.contains(producer));
                                let ready = !visited.contains(&instance)
                                    && blocked_on_anonymous
                                        .get(&instance)
                                        .is_none_or(|producers| {
                                            producers
                                                .iter()
                                                .all(|producer| visited.contains(producer))
                                        })
                                    && all_producers_visited
                                    && (!matches!(
                                        instance.as_ref(),
                                        crate::FunctionInstanceKey::Specialization { .. }
                                    ) || !rue_air::comptime_depth_over_limit(
                                        comptime_specialization_depth(depth),
                                    ));
                                if ready {
                                    frontier.push((instance, depth));
                                } else {
                                    blocked_pending.insert(instance, depth);
                                }
                            }
                            pending = blocked_pending;
                            if !frontier.is_empty() {
                                let frontier_len = frontier.len();
                                ready_frontier.extend(frontier);
                                let width_bucket = match frontier_len {
                                    1 => "reachability.frontier.width-1",
                                    2..=3 => "reachability.frontier.width-2-3",
                                    4..=7 => "reachability.frontier.width-4-7",
                                    _ => "reachability.frontier.width-8-plus",
                                };
                                pending_frontier_metrics = Some((frontier_len, width_bucket));
                            }
                        }

                        if prefetched_transactions.is_empty() && !ready_frontier.is_empty() {
                            if concurrency == 1 {
                                // Preserve the logical frontier, but avoid
                                // manufacturing a second deep BodyQueryKey and
                                // one-element result vectors merely to execute
                                // its bounded window inline. The coordinator
                                // performs the same demand-before-transaction
                                // sequence below.
                                let (instance, depth) = ready_frontier
                                    .pop_first()
                                    .expect("a non-empty ready frontier has a first instance");
                                prefetched_transactions.push_back((instance, depth, None));
                                continue;
                            }
                            // Keep the retained result window bounded. A wide
                            // ready frontier can contain thousands of bodies;
                            // scheduling it all at once retains every completed
                            // transaction until the coordinator reaches it.
                            // Two tasks per worker leave scheduling headroom;
                            // the fixed ceiling bounds transient memory
                            // independently of source size.
                            let instances = ready_frontier
                                .keys()
                                .take(body_query_prefetch_window)
                                .cloned()
                                .collect::<Vec<_>>();
                            let frontier_keys = instances
                                .iter()
                                .map(|instance| {
                                    crate::body_query::BodyQueryKey::new(
                                        instance.as_ref().clone(),
                                        key.configuration.clone(),
                                    )
                                })
                                .collect::<Vec<_>>();
                            let demands = context.query_registered_batch(
                                &$toolchain_for_body_closure,
                                frontier_keys.clone(),
                            )?;
                            let mut batch_modules = BTreeSet::new();
                            let mut batch_requesters = BTreeSet::new();
                            for (instance, demand) in instances.iter().zip(demands) {
                                let rue_query::QueryOutcome::Success(demand) = demand.outcome()
                                else {
                                    unreachable!("BodyToolchainDemands publishes typed values")
                                };
                                // The parking sweep below walks the whole ready
                                // frontier, which contains every instance in
                                // this window, so record what the batch already
                                // answered rather than asking again (RUE-1562).
                                if observed_toolchain_demands
                                    .insert(instance.clone(), demand.clone())
                                    .is_none()
                                {
                                    context.record_work(rue_query::WorkItem::new(
                                        "reachability.toolchain-demand.queries",
                                        1,
                                    ));
                                }
                                let mut any_absent = false;
                                for module in demand.modules() {
                                    if !present_trusted_modules.contains(module.logical_path()) {
                                        batch_modules.insert(module.clone());
                                        any_absent = true;
                                    }
                                }
                                if any_absent
                                    && let Some(requester) = demand.requester()
                                {
                                    batch_requesters.insert(requester.clone());
                                }
                            }
                            if !batch_modules.is_empty() {
                                let remaining: Vec<Arc<crate::FunctionInstanceKey>> =
                                    ready_frontier.keys().chain(pending.keys()).cloned().collect();
                                for remaining_instance in remaining {
                                    // A cached demand skips the query that used
                                    // to carry this loop's cancellation check.
                                    context.check_canceled()?;
                                    let remaining_demand = observe_body_toolchain_demand(
                                        context,
                                        &$toolchain_for_body_closure,
                                        &mut observed_toolchain_demands,
                                        &key.configuration,
                                        &remaining_instance,
                                    )?;
                                    let remaining_demand = &remaining_demand;
                                    let mut any_absent = false;
                                    for module in remaining_demand.modules() {
                                        if !present_trusted_modules
                                            .contains(module.logical_path())
                                        {
                                            batch_modules.insert(module.clone());
                                            any_absent = true;
                                        }
                                    }
                                    if any_absent
                                        && let Some(requester) = remaining_demand.requester()
                                    {
                                        batch_requesters.insert(requester.clone());
                                    }
                                }
                                parked_toolchain = Some(crate::ParkedToolchainModules::new(
                                    batch_modules,
                                    batch_requesters,
                                ));
                                break 'schedule;
                            }
                            if let Some((frontier_len, width_bucket)) =
                                pending_frontier_metrics.take()
                            {
                                context.record_work(rue_query::WorkItem::new(
                                    "reachability.frontier.batches",
                                    1,
                                ));
                                context.record_work(rue_query::WorkItem::new(
                                    "reachability.frontier.keys",
                                    frontier_len as u64,
                                ));
                                context.record_work(rue_query::WorkItem::new(width_bucket, 1));
                            }
                            let transactions = context.query_registered_batch(
                                &$transactions_for_body_reachability,
                                frontier_keys,
                            )?;
                            for (instance, transaction) in instances.into_iter().zip(transactions) {
                                let depth = ready_frontier
                                    .remove(&instance)
                                    .expect("prefetched ready instances retain their depth");
                                prefetched_transactions.push_back((
                                    instance,
                                    depth,
                                    Some(transaction),
                                ));
                            }
                        }

                        let Some(instance) = prefetched_transactions
                            .front()
                            .map(|(instance, _, _)| instance.clone())
                        else {
                            let Some((instance, current_depth)) = pending.pop_first() else {
                                break;
                            };
                            if matches!(
                                instance.as_ref(),
                                crate::FunctionInstanceKey::Specialization { .. }
                            ) && rue_air::comptime_depth_over_limit(
                                comptime_specialization_depth(current_depth),
                            )
                            {
                                let name = function_definition_key(&instance)
                                    .map(crate::StableDefinitionKey::name)
                                    .unwrap_or("<anonymous>")
                                    .to_owned();
                                scheduling_errors.insert(
                                    instance.as_ref().clone(),
                                    crate::CompileErrors::from(
                                        crate::CompileError::without_span(
                                            rue_error::ErrorKind::ComptimeEvaluationFailed {
                                                reason: format!(
                                                    "specialization of '{name}' exceeded the maximum nesting depth ({}); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?",
                                                    rue_air::MAX_COMPTIME_CALL_DEPTH
                                                ),
                                            },
                                        ),
                                    ),
                                );
                            } else {
                                scheduling_errors.insert(
                                    instance.as_ref().clone(),
                                    crate::CompileErrors::from(
                                        crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "anonymous producer dependency cycle while scheduling {instance:?}"
                                            )),
                                        ),
                                    ),
                                );
                            }
                            break;
                        };
                        context.check_canceled()?;
                        let current_depth = prefetched_transactions
                            .front()
                            .map(|(_, depth, _)| *depth)
                            .expect("selected prefetched instances retain their depth");
                        if !visited.insert(instance.clone()) {
                            continue;
                        }
                        if matches!(
                            instance.as_ref(),
                            crate::FunctionInstanceKey::Specialization { .. }
                        ) && rue_air::comptime_depth_over_limit(
                            comptime_specialization_depth(current_depth),
                        )
                        {
                            let name = function_definition_key(&instance)
                                .map(crate::StableDefinitionKey::name)
                                .unwrap_or("<anonymous>");
                            scheduling_errors.insert(
                                instance.as_ref().clone(),
                                crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::ComptimeEvaluationFailed {
                                            reason: format!(
                                                "specialization of '{name}' exceeded the maximum nesting depth ({}); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?",
                                                rue_air::MAX_COMPTIME_CALL_DEPTH
                                            ),
                                        },
                                    ),
                                ),
                            );
                            break;
                        }

                        let body_key = crate::body_query::BodyQueryKey::new(
                            instance.as_ref().clone(),
                            key.configuration.clone(),
                        );
                        let demand = observe_body_toolchain_demand(
                            context,
                            &$toolchain_for_body_closure,
                            &mut observed_toolchain_demands,
                            &key.configuration,
                            &instance,
                        )?;
                        let demand = &demand;
                        let mut batch_modules = demand
                            .modules()
                            .iter()
                            .filter(|module| {
                                !present_trusted_modules.contains(module.logical_path())
                            })
                            .cloned()
                            .collect::<BTreeSet<_>>();
                        if !batch_modules.is_empty() {
                            let mut batch_requesters = demand
                                .requester()
                                .cloned()
                                .into_iter()
                                .collect::<BTreeSet<_>>();
                            let sweep: Vec<Arc<crate::FunctionInstanceKey>> = ready_frontier
                                .keys()
                                .chain(pending.keys())
                                .filter(|pending_instance| !visited.contains(*pending_instance))
                                .cloned()
                                .collect();
                            for pending_instance in sweep {
                                // A cached demand skips the query that used to
                                // carry this loop's cancellation check.
                                context.check_canceled()?;
                                let pending_demand = observe_body_toolchain_demand(
                                    context,
                                    &$toolchain_for_body_closure,
                                    &mut observed_toolchain_demands,
                                    &key.configuration,
                                    &pending_instance,
                                )?;
                                let pending_demand = &pending_demand;
                                let mut any_absent = false;
                                for module in pending_demand.modules() {
                                    if !present_trusted_modules.contains(module.logical_path()) {
                                        batch_modules.insert(module.clone());
                                        any_absent = true;
                                    }
                                }
                                if any_absent
                                    && let Some(requester) = pending_demand.requester()
                                {
                                    batch_requesters.insert(requester.clone());
                                }
                            }
                            parked_toolchain = Some(crate::ParkedToolchainModules::new(
                                batch_modules,
                                batch_requesters,
                            ));
                            break;
                        }
                        if let Some((frontier_len, width_bucket)) =
                            pending_frontier_metrics.take()
                        {
                            context.record_work(rue_query::WorkItem::new(
                                "reachability.frontier.batches",
                                1,
                            ));
                            context.record_work(rue_query::WorkItem::new(
                                "reachability.frontier.keys",
                                frontier_len as u64,
                            ));
                            context.record_work(rue_query::WorkItem::new(width_bucket, 1));
                        }

                        // The ordinary frontier prefetches the expensive
                        // BodyTransaction computations. Control outcomes must
                        // remain transaction-only (they publish no
                        // BodyReferences terminal), so interpret the exact
                        // transaction before projecting schedulable references.
                        let (prefetched_instance, _, transaction) = prefetched_transactions
                            .pop_front()
                            .expect("the selected prefetched transaction remains queued");
                        assert_eq!(prefetched_instance, instance);
                        context.record_work(rue_query::WorkItem::new(
                            "reachability.transactions.prefetched",
                            1,
                        ));
                        let transaction_terminal = match transaction {
                            Some(transaction) => transaction,
                            None => context.query_registered(
                                &$transactions_for_body_reachability,
                                body_key.clone(),
                            )?,
                        };
                        let rue_query::QueryOutcome::Success(transaction) =
                            transaction_terminal.outcome()
                        else {
                            unreachable!("BodyTransaction publishes typed values")
                        };
                        let deterministic_failure = matches!(
                            transaction,
                            crate::body_query::BodyTransaction::DeterministicFailure { .. }
                        );
                        match transaction {
                            crate::body_query::BodyTransaction::Control(
                                crate::body_query::BodyTransactionControl::DeferredAnonymousProducers(
                                    producers,
                                ),
                            ) => {
                                // A producer already diagnosed in this closure
                                // makes the dependent body unreachable for this
                                // compile. Its typed producer diagnostic is
                                // already in `bodies`; suppress only this
                                // dependent retry while continuing unrelated
                                // roots so ordinary multi-error collection is
                                // preserved.
                                if producers
                                    .iter()
                                    .any(|producer| failed_instances.contains(producer))
                                {
                                    continue;
                                }
                                let mut blockers = BTreeSet::new();
                                for producer in producers.iter() {
                                    let depth = current_depth
                                        + usize::from(matches!(
                                            producer,
                                            crate::FunctionInstanceKey::Specialization { .. }
                                    ));
                                    if let Some((producer, depth_changed)) = schedule_body_instance(
                                        &mut pending,
                                        &mut ready_frontier,
                                        &mut prefetched_transactions,
                                        &visited,
                                        producer,
                                        depth,
                                    ) {
                                        if depth_changed {
                                            anonymous_dependency_pending
                                                .push((producer.clone(), depth));
                                        }
                                        blockers.insert(producer.clone());
                                    }
                                }
                                if !blockers.is_empty() {
                                    visited.remove(&instance);
                                    pending.insert(instance.clone(), current_depth);
                                    blocked_on_anonymous.insert(instance, blockers);
                                    continue;
                                }
                                scheduling_errors.insert(
                                    instance.as_ref().clone(),
                                    crate::CompileErrors::from(
                                        crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "reached body query {instance:?} could not observe an already-reached anonymous producer"
                                            )),
                                        ),
                                    ),
                                );
                                break;
                            }
                            crate::body_query::BodyTransaction::Control(
                                crate::body_query::BodyTransactionControl::ProducerFailed(
                                    failure,
                                ),
                            ) => {
                                fatal = Some(
                                    crate::body_query::BodyClosureFatal::ProducerFailed {
                                        instance: instance.as_ref().clone(),
                                        failure: failure.clone(),
                                    },
                                );
                                break;
                            }
                            crate::body_query::BodyTransaction::Control(
                                crate::body_query::BodyTransactionControl::WellKnownOptionResolution(
                                    failure,
                                ),
                            ) => {
                                fatal = Some(
                                    crate::body_query::BodyClosureFatal::WellKnownOptionResolution {
                                        instance: instance.as_ref().clone(),
                                        failure: failure.clone(),
                                    },
                                );
                                break;
                            }
                            crate::body_query::BodyTransaction::Success { .. }
                            | crate::body_query::BodyTransaction::DeterministicFailure { .. } => {}
                        }
                        blocked_on_anonymous.remove(&instance);

                        // The scheduler has already requested and inspected
                        // this exact transaction. Its immutable reference Arc
                        // is the canonical reachability input; routing it
                        // through a second registered projection would repeat
                        // one memo claim and dependency validation per reached
                        // body without adding an independent invalidation edge.
                        let references = transaction.references();
                        reached_body_keys.push(body_key.clone());
                        // A deterministic body diagnostic is terminal for this
                        // body's dependents. Keep scheduling references that
                        // were successfully discovered before the diagnostic so
                        // unrelated reached bodies still publish their terminals
                        // and ordinary multi-error collection remains intact.
                        if deterministic_failure {
                            failed_instances.insert(instance.clone());
                        }
                        if matches!(
                            transaction,
                            crate::body_query::BodyTransaction::Success { .. }
                        ) {
                            let produced_terminal = context.query_registered(
                                &$produced_for_body_reachability,
                                body_key.clone(),
                            )?;
                            let rue_query::QueryOutcome::Success(produced) =
                                produced_terminal.outcome()
                            else {
                                unreachable!("BodyProducedAnonymous publishes typed values")
                            };
                            let crate::body_query::ProducedAnonymous::Produced(produced) = produced
                            else {
                                let crate::body_query::ProducedAnonymous::ProducerFailed(failure) =
                                    produced
                                else {
                                    unreachable!()
                                };
                                fatal =
                                    Some(crate::body_query::BodyClosureFatal::ProducerFailed {
                                        instance: instance.as_ref().clone(),
                                        failure: failure.clone(),
                                    });
                                break;
                            };
                            produced_anonymous.extend(
                                produced
                                    .0
                                    .iter()
                                    .cloned()
                                    .map(|nominal| (nominal.identity.clone(), nominal)),
                            );
                        }
                        for reference in references.0.iter() {
                            match reference {
                                crate::body_query::BodyReference::Callable(callable) => {
                                    let abi = context.query_registered(
                                        &$call_abis_for_body_reachability,
                                        crate::type_queries::CallAbiQueryKey {
                                            callable: callable.clone(),
                                            configuration: key.configuration.clone(),
                                        },
                                    )?;
                                    let rue_query::QueryOutcome::Success(abi) = abi.outcome()
                                    else {
                                        unreachable!("CallAbi publishes typed values")
                                    };
                                    // A typed unavailable ABI result is an
                                    // honest query terminal. The legacy CFG
                                    // consumer still owns materialization until
                                    // RUE-1030 switches it to these facts, so it
                                    // must not become a new source diagnostic.
                                    let _ = abi;
                                    match closure_callable_has_body(
                                        context,
                                        &$input_for_body_closure,
                                        &projection.declarations,
                                        &projection.declaration_index,
                                        callable,
                                        &key.configuration,
                                    )? {
                                        Ok(true) => {
                                            let depth = current_depth
                                                + usize::from(matches!(
                                                    callable,
                                                    crate::FunctionInstanceKey::Specialization { .. }
                                            ));
                                            if let Some((callable, true)) = schedule_body_instance(
                                                &mut pending,
                                                &mut ready_frontier,
                                                &mut prefetched_transactions,
                                                &visited,
                                                callable,
                                                depth,
                                            ) {
                                                anonymous_dependency_pending
                                                    .push((callable.clone(), depth));
                                            }
                                        }
                                        Ok(false) => {}
                                        Err(detail) => {
                                            fatal = Some(
                                                crate::body_query::BodyClosureFatal::BodyAvailability {
                                                    instance: instance.as_ref().clone(),
                                                    detail,
                                                },
                                            );
                                            break;
                                        }
                                    }
                                }
                                crate::body_query::BodyReference::Type(ty) => {
                                    let facts = context.query_registered(
                                        &$type_facts_for_body_reachability,
                                        crate::type_queries::TypeQueryKey {
                                            ty: ty.clone(),
                                            configuration: key.configuration.clone(),
                                        },
                                    )?;
                                    let rue_query::QueryOutcome::Success(facts) = facts.outcome()
                                    else {
                                        unreachable!("TypeFacts publishes typed values")
                                    };
                                    // TypeFacts publishes typed incomplete
                                    // outcomes for non-materializable semantic
                                    // types. Until RUE-1030 makes downstream
                                    // consumers query-native, observing that
                                    // terminal must preserve existing behavior.
                                    let _ = facts;
                                }
                                crate::body_query::BodyReference::DropGlue(ty) => {
                                    pending_drop_glue.insert(ty.clone());
                                }
                                crate::body_query::BodyReference::Definition(_) => {}
                            }
                        }
                        while fatal.is_none() && !pending_drop_glue.is_empty() {
                            let frontier = std::mem::take(&mut pending_drop_glue)
                                .into_iter()
                                .filter(|ty| !visited_drop_glue.contains(ty))
                                .collect::<Vec<_>>();
                            if frontier.is_empty() {
                                break;
                            }
                            let terminals = context.query_registered_adaptive_batch(
                                &$drop_glues_for_body_reachability,
                                frontier.iter().cloned().map(|ty| {
                                    crate::type_queries::TypeQueryKey {
                                        ty,
                                        configuration: key.configuration.clone(),
                                    }
                                }),
                            )?;
                            for (ty, terminal) in frontier.into_iter().zip(terminals) {
                                visited_drop_glue.insert(ty.clone());
                                let rue_query::QueryOutcome::Success(value) = terminal.outcome()
                                else {
                                    unreachable!("DropGlue publishes typed values")
                                };
                                match value {
                                    crate::type_queries::DropGlueValue::Available(glue) => {
                                        if !glue.required {
                                            continue;
                                        }
                                        demanded_drop_glue_plans
                                            .insert(ty.clone(), glue.as_ref().clone());
                                        demanded_drop_glue.insert(ty);
                                        pending_drop_glue.extend(glue.nested.iter().cloned());
                                        if let Some(destructor) = &glue.destructor {
                                            if let Some((destructor, true)) = schedule_body_instance(
                                                &mut pending,
                                                &mut ready_frontier,
                                                &mut prefetched_transactions,
                                                &visited,
                                                destructor,
                                                current_depth,
                                            ) {
                                                anonymous_dependency_pending.push((
                                                    destructor.clone(),
                                                    current_depth,
                                                ));
                                            }
                                        }
                                    }
                                    crate::type_queries::DropGlueValue::Failure(failure) => {
                                        fatal = Some(
                                            crate::body_query::BodyClosureFatal::TypeQuery {
                                                ty: Some(ty),
                                                detail: Arc::from(format!(
                                                    "drop glue: {failure:?}"
                                                )),
                                            },
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    reached_body_keys
                        .sort_by(|left, right| left.instance.cmp(&right.instance));
                    let reached = reached_body_keys
                        .iter()
                        .map(|body| body.instance.clone())
                        .collect::<Vec<_>>();
                    let scheduling_errors = scheduling_errors.into_iter().collect::<Vec<_>>();
                    let is_failure = !scheduling_errors.is_empty()
                        || fatal.is_some()
                        || parked_toolchain.is_some();
                    let output = crate::body_query::BodyReachabilityOutput {
                        reached: reached.into(),
                        demanded_drop_glue: demanded_drop_glue.into_iter().collect::<Vec<_>>().into(),
                        demanded_drop_glue_plans: demanded_drop_glue_plans
                            .into_iter()
                            .collect::<Vec<_>>()
                            .into(),
                        scheduling_errors: scheduling_errors.into(),
                        fatal,
                        parked_toolchain,
                    };
                    Ok(QueryOutput::success(output).with_terminal_kind(if is_failure {
                        QueryTerminalKind::Failure
                    } else {
                        QueryTerminalKind::Success
                    }))
                },
            )
            .expect("the BodyReachability family has one canonical name")
    }};
}
