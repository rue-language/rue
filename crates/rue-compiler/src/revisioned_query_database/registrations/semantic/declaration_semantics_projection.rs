macro_rules! register_semantic_declaration_semantics_projection {
    ($nucleus_for_declaration_projection:ident, $occurrences_for_declaration_projection:ident, $orders_for_declaration_projection:ident, $runtime:ident, $shells_for_declaration_projection:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.declaration-semantics-projection",
                64,
                |left: &SemanticNucleusProjectionValue, right: &SemanticNucleusProjectionValue| {
                    left == right
                },
                move |context, _, key: &SemanticNucleusProjectionKey| {
                    let result = Self::evaluate_declaration_semantics_projection(
                        context,
                        &$occurrences_for_declaration_projection,
                        &$orders_for_declaration_projection,
                        &$nucleus_for_declaration_projection,
                        &$shells_for_declaration_projection,
                        key,
                    );
                    match result {
                        Ok(projection) => {
                            // This aggregate is the last point at which every
                            // semantic-nucleus observation is still leased by
                            // the evaluating request. A wide or repeatedly
                            // validated projection can exceed the child
                            // families' memo history before the outer
                            // publication requests its terminal, so carry
                            // those exact leases with the aggregate value for
                            // fail-closed cone promotion at that boundary.
                            let retained_dependencies =
                                Arc::new(context.retain_observed_terminals());
                            Ok(QueryOutput::success(
                                SemanticNucleusProjectionValue::Available {
                                    projection,
                                    _retained_dependencies: retained_dependencies,
                                },
                            ))
                        }
                        Err(SemanticNucleusBatchFailure::Stable {
                            declaration,
                            failure,
                        }) => {
                            let retained_dependencies =
                                Arc::new(context.retain_observed_terminals());
                            Ok(
                                QueryOutput::success(SemanticNucleusProjectionValue::Failure {
                                    declaration,
                                    failure,
                                    _retained_dependencies: retained_dependencies,
                                })
                                .with_terminal_kind(QueryTerminalKind::Failure),
                            )
                        }
                        Err(SemanticNucleusBatchFailure::Query(abort)) => Err(abort),
                    }
                },
            )
            .expect("the DeclarationSemanticsProjection family has one canonical name")
    }};
}
