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
                    match Self::evaluate_declaration_semantics_projection(
                        context,
                        &$occurrences_for_declaration_projection,
                        &$orders_for_declaration_projection,
                        &$nucleus_for_declaration_projection,
                        &$shells_for_declaration_projection,
                        key,
                    ) {
                        Ok(projection) => Ok(QueryOutput::success(
                            SemanticNucleusProjectionValue::Available(projection),
                        )),
                        Err(SemanticNucleusBatchFailure::Stable {
                            declaration,
                            failure,
                        }) => Ok(
                            QueryOutput::success(SemanticNucleusProjectionValue::Failure {
                                declaration,
                                failure,
                            })
                            .with_terminal_kind(QueryTerminalKind::Failure),
                        ),
                        Err(SemanticNucleusBatchFailure::Query(abort)) => Err(abort),
                    }
                },
            )
            .expect("the DeclarationSemanticsProjection family has one canonical name")
    }};
}
