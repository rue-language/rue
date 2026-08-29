macro_rules! register_backend_object_projections {
    ($codegen_units_for_object_projection:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.object-projection",
                BODY_QUERY_MEMO_RETENTION,
                crate::object_query::object_projection_value_equal,
                move |context, _, key: &crate::object_query::ObjectProjectionQueryKey| {
                    crate::object_query::evaluate_object_projection(
                        context,
                        &$codegen_units_for_object_projection,
                        key,
                    )
                },
            )
            .expect("the ObjectProjection family has one canonical name")
    }};
}
