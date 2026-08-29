macro_rules! register_backend_codegen_unit_batches {
    ($backend_root_for_codegen_batch:ident, $body_closure_root_for_codegen_batch:ident, $body_reachability_root_for_codegen_batch:ident, $cfg_collection_root_for_codegen_batch:ident, $codegen_collection_root_for_codegen_batch:ident, $codegen_units_for_batch:ident, $runtime:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.codegen-unit-batch",
                0,
                |left: &CodegenUnitBatchOutput, right: &CodegenUnitBatchOutput| {
                    left.values.len() == right.values.len()
                        && left
                            .values
                            .iter()
                            .zip(right.values.iter())
                            .all(|(left, right)| {
                                crate::codegen_query::codegen_unit_value_equal(left, right)
                            })
                },
                move |context, _, key: &CodegenUnitBatchKey| {
                    let fallbacks = backend_retention_fallbacks(
                        &$backend_root_for_codegen_batch,
                        &$body_closure_root_for_codegen_batch,
                        &$body_reachability_root_for_codegen_batch,
                        &$cfg_collection_root_for_codegen_batch,
                        &$codegen_collection_root_for_codegen_batch,
                    );
                    let _validated_registered = context
                        .endorse_registered_validations_from(&fallbacks)
                        .expect("backend retention roots belong to this query runtime");
                    let _attempts = context.retain_nested_attempts_for(&["compiler.codegen-unit"]);
                    let terminals = context.query_registered_adaptive_batch_refs(
                        &$codegen_units_for_batch,
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
                                "the registered CodegenUnit batch observes every selected child cone",
                            ),
                    );
                    let values = terminals
                        .iter()
                        .map(|terminal| {
                            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                                unreachable!("CodegenUnit publishes typed values")
                            };
                            value.clone()
                        })
                        .collect::<Vec<_>>()
                        .into();
                    Ok(QueryOutput::success(CodegenUnitBatchOutput {
                        values,
                        _retained_children: retained_children,
                    })
                    .with_terminal_kind(kind))
                },
            )
            .expect("the CodegenUnitBatch family has one canonical name")
    }};
}
