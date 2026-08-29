macro_rules! register_backend_backend_root_publications {
    ($backend_root_for_publication:ident, $body_closure_root_for_backend_publication:ident, $body_reachability_root_for_backend_publication:ident, $cfg_collection_root_for_backend_publication:ident, $codegen_collection_root_for_backend_publication:ident, $codegen_units_for_backend_publication:ident, $object_projections_for_backend_publication:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.backend-root-publication",
                1,
                |left: &bool, right: &bool| left == right,
                move |context, _, key: &BackendRootPublicationKey| {
                    let fallbacks = backend_retention_fallbacks(
                        &$backend_root_for_publication,
                        &$body_closure_root_for_backend_publication,
                        &$body_reachability_root_for_backend_publication,
                        &$cfg_collection_root_for_backend_publication,
                        &$codegen_collection_root_for_backend_publication,
                    );
                    let _validated_registered = context
                        .endorse_registered_validations_from(&fallbacks)
                        .expect("backend retention roots belong to this query runtime");
                    let (pending, object_projection_terminals) = match &key.input {
                        BackendRootPublicationInput::Codegen(batch) => {
                            let terminals = context.query_registered_adaptive_batch_refs(
                                &$codegen_units_for_backend_publication,
                                batch.keys.iter(),
                            )?;
                            if terminals.iter().any(|terminal| {
                                matches!(
                                    terminal.outcome(),
                                    rue_query::QueryOutcome::Success(
                                        crate::codegen_query::CodegenUnitValue::Failure(_)
                                    )
                                )
                            }) {
                                return Ok(QueryOutput::success(false)
                                    .with_terminal_kind(QueryTerminalKind::Failure));
                            }
                            let pending = context
                                .retain_observed_terminal_cones_from(&terminals, &fallbacks)
                                .expect("backend-root validation observes CodegenUnit cones");
                            (pending, 0)
                        }
                        BackendRootPublicationInput::Objects(batch) => {
                            let terminals = context.query_registered_adaptive_batch_refs(
                                &$object_projections_for_backend_publication,
                                batch.keys.iter(),
                            )?;
                            if terminals.iter().any(|terminal| {
                                matches!(
                                    terminal.outcome(),
                                    rue_query::QueryOutcome::Success(
                                        crate::object_query::ObjectProjectionValue::Failure(_)
                                    )
                                )
                            }) {
                                return Ok(QueryOutput::success(false)
                                    .with_terminal_kind(QueryTerminalKind::Failure));
                            }
                            let pending = context
                                .retain_observed_terminal_cones_from(&terminals, &fallbacks)
                                .expect("backend-root validation observes ObjectProjection cones");
                            (pending, batch.keys.len())
                        }
                    };
                    context.register_attempt_handoff(PublishedBackendRootHandoff {
                        root: $backend_root_for_publication.clone(),
                        pending: Some(Arc::new(pending)),
                        functions: Some(key.functions.iter().cloned().collect()),
                        cfg_terminals: key.cfg_terminals,
                        optimized_cfg_terminals: key.optimized_cfg_terminals,
                        codegen_unit_terminals: key.codegen_unit_terminals,
                        object_projection_terminals,
                        previous: None,
                        installed: false,
                    });
                    Ok(QueryOutput::success(true))
                },
            )
            .expect("the backend-root publication family has one canonical name")
    }};
}
