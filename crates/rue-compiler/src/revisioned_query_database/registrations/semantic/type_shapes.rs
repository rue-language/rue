macro_rules! register_semantic_type_shapes {
    ($produced_anonymous_for_type_shape:ident, $runtime:ident, $semantic_nucleus_for_type_shape:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.type-shape",
                BODY_QUERY_MEMO_RETENTION,
                |left: &crate::type_queries::TypeShapeValue,
                 right: &crate::type_queries::TypeShapeValue| left == right,
                move |context, _, key: &crate::type_queries::TypeQueryKey| {
                    evaluate_type_shape(
                        context,
                        &$semantic_nucleus_for_type_shape,
                        &$produced_anonymous_for_type_shape,
                        key,
                    )
                },
            )
            .expect("the TypeShape family has one canonical name")
    }};
}
