macro_rules! register_backend_object_projection_batches {
    ($backend_root_for_object_projection_batch:ident, $body_closure_root_for_object_projection_batch:ident, $body_reachability_root_for_object_projection_batch:ident, $cfg_collection_root_for_object_projection_batch:ident, $codegen_collection_root_for_object_projection_batch:ident, $object_projections_for_batch:ident, $runtime:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.object-projection-batch",
                0,
                |left: &ObjectProjectionBatchOutput, right: &ObjectProjectionBatchOutput| {
                    left.values.len() == right.values.len()
                        && left.values.iter().zip(right.values.iter()).all(
                            |(left, right)| {
                                crate::object_query::object_projection_value_equal(left, right)
                            },
                        )
                },
                move |context, _, key: &ObjectProjectionBatchKey| {
                    let fallbacks = backend_retention_fallbacks(
                        &$backend_root_for_object_projection_batch,
                        &$body_closure_root_for_object_projection_batch,
                        &$body_reachability_root_for_object_projection_batch,
                        &$cfg_collection_root_for_object_projection_batch,
                        &$codegen_collection_root_for_object_projection_batch,
                    );
                    let _validated_registered = context
                        .endorse_registered_validations_from(&fallbacks)
                        .expect("backend retention roots belong to this query runtime");
                    let _attempts =
                        context.retain_nested_attempts_for(&["compiler.object-projection"]);
                    let terminals = context.query_registered_adaptive_batch_refs(
                        &$object_projections_for_batch,
                        key.keys.iter(),
                    )?;
                    let kind = if terminals
                        .iter()
                        .all(|terminal| terminal.kind() == QueryTerminalKind::Success)
                    {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    let retained_children = Arc::new(
                        context
                            .retain_observed_terminal_cones_from(&terminals, &fallbacks)
                            .expect(
                                "the registered ObjectProjection batch observes every selected child cone",
                            ),
                    );
                    let values = terminals
                        .iter()
                        .map(|terminal| {
                            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                                unreachable!("ObjectProjection publishes typed values")
                            };
                            value.clone()
                        })
                        .collect::<Vec<_>>()
                        .into();
                    Ok(QueryOutput::success(ObjectProjectionBatchOutput {
                        values,
                        _retained_children: retained_children,
                    })
                    .with_terminal_kind(kind))
                },
            )
            .expect("the ObjectProjectionBatch family has one canonical name")
    }};
}
