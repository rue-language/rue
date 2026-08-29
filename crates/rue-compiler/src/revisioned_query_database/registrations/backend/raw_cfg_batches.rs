macro_rules! register_backend_raw_cfg_batches {
    ($backend_root_for_raw_cfg_batch:ident, $body_closure_root_for_raw_cfg_batch:ident, $body_reachability_root_for_raw_cfg_batch:ident, $cfg_collection_root_for_raw_cfg_batch:ident, $cfgs_for_raw_batch:ident, $codegen_collection_root_for_raw_cfg_batch:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.raw-cfg-batch",
                0,
                |left: &RawCfgBatchOutput, right: &RawCfgBatchOutput| {
                    left.values.len() == right.values.len()
                        && left
                            .values
                            .iter()
                            .zip(right.values.iter())
                            .all(|(left, right)| crate::cfg_query::cfg_value_equal(left, right))
                },
                move |context, _, key: &RawCfgBatchKey| {
                    let fallbacks = backend_retention_fallbacks(
                        &$backend_root_for_raw_cfg_batch,
                        &$body_closure_root_for_raw_cfg_batch,
                        &$body_reachability_root_for_raw_cfg_batch,
                        &$cfg_collection_root_for_raw_cfg_batch,
                        &$codegen_collection_root_for_raw_cfg_batch,
                    );
                    let _validated_registered = context
                        .endorse_registered_validations_from(&fallbacks)
                        .expect("raw CFG retention roots belong to this query runtime");
                    let _attempts = context.retain_nested_attempts_for(&["compiler.cfg"]);
                    let terminals = context.query_registered_adaptive_batch_refs(
                        &$cfgs_for_raw_batch,
                        key.keys.iter(),
                    )?;
                    let values = terminals
                        .iter()
                        .map(|terminal| {
                            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                                unreachable!("Cfg publishes typed values")
                            };
                            value.clone()
                        })
                        .collect::<Vec<_>>();
                    let retained_children = Arc::new(
                        context
                            .retain_observed_terminal_cones_from(&terminals, &fallbacks)
                            .expect("raw CFG batch observes every child cone"),
                    );
                    let kind = if values
                        .iter()
                        .all(|value| matches!(value, crate::cfg_query::CfgValue::Available(_)))
                    {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(RawCfgBatchOutput {
                        values: values.into(),
                        _retained_children: retained_children,
                    })
                    .with_terminal_kind(kind))
                },
            )
            .expect("the RawCfgBatch family has one canonical name")
    }};
}
