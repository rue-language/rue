macro_rules! register_backend_optimized_cfg_batches {
    ($backend_root_for_optimized_cfg_batch:ident, $body_closure_root_for_optimized_cfg_batch:ident, $body_reachability_root_for_optimized_cfg_batch:ident, $cfg_collection_root_for_optimized_cfg_batch:ident, $codegen_collection_root_for_optimized_cfg_batch:ident, $optimized_cfgs_for_batch:ident, $runtime:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.optimized-cfg-batch",
                0,
                |left: &OptimizedCfgBatchOutput, right: &OptimizedCfgBatchOutput| {
                    left.values.len() == right.values.len()
                        && left
                            .values
                            .iter()
                            .zip(right.values.iter())
                            .all(|(left, right)| crate::cfg_query::cfg_value_equal(left, right))
                        && left.non_reusable_functions == right.non_reusable_functions
                        && left.unreachable_functions == right.unreachable_functions
                },
                move |context, _, key: &OptimizedCfgBatchKey| {
                    let fallbacks = backend_retention_fallbacks(
                        &$backend_root_for_optimized_cfg_batch,
                        &$body_closure_root_for_optimized_cfg_batch,
                        &$body_reachability_root_for_optimized_cfg_batch,
                        &$cfg_collection_root_for_optimized_cfg_batch,
                        &$codegen_collection_root_for_optimized_cfg_batch,
                    );
                    let _validated_registered = context
                        .endorse_registered_validations_from(&fallbacks)
                        .expect("backend retention roots belong to this query runtime");
                    let _attempts = context.retain_nested_attempts_for(&[
                        "compiler.cfg",
                        "compiler.optimized-cfg",
                    ]);
                    let terminals = context.query_registered_adaptive_batch_refs(
                        &$optimized_cfgs_for_batch,
                        key.keys.iter(),
                    )?;
                    let values = terminals
                        .iter()
                        .map(|terminal| {
                            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                                unreachable!("OptimizedCfg publishes typed values")
                            };
                            value.clone()
                        })
                        .collect::<Vec<_>>();
                    let (values, non_reusable_functions, unreachable_functions) =
                        crate::cfg_query::apply_general_inlining(
                            context,
                            key.keys.as_ref(),
                            &values,
                            key.roots.as_ref(),
                        )?;
                    let kind = if values.iter().all(|value| matches!(value, crate::cfg_query::CfgValue::Available(_))) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    let retained_terminals = terminals
                        .iter()
                        .zip(key.keys.iter())
                        .zip(values.iter())
                        .filter(|((_, key), value)| {
                            !unreachable_functions.contains(&key.cfg.function)
                                && matches!(value, crate::cfg_query::CfgValue::Available(record) if record.durable_reuse_allowed)
                        })
                        .map(|((terminal, _), _)| terminal.clone())
                        .collect::<Vec<_>>();
                    let retained_children = Arc::new(
                        context
                            .retain_observed_terminal_cones_from(&retained_terminals, &fallbacks)
                            .expect("general inlining batch observes every retained child cone"),
                    );
                    let values = values.into();
                    Ok(QueryOutput::success(OptimizedCfgBatchOutput {
                        values,
                        non_reusable_functions: non_reusable_functions.into_iter().collect::<Vec<_>>().into(),
                        unreachable_functions: unreachable_functions.into_iter().collect::<Vec<_>>().into(),
                        _retained_children: retained_children,
                    })
                    .with_terminal_kind(kind))
                },
            )
            .expect("the OptimizedCfgBatch family has one canonical name")
    }};
}
