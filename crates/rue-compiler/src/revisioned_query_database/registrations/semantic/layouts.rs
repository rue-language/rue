macro_rules! register_semantic_layouts {
    ($layout_family_for_evaluator:ident, $runtime:ident, $type_shapes_for_layout:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.layout",
                BODY_QUERY_MEMO_RETENTION,
                |left: &crate::type_queries::LayoutValue,
                 right: &crate::type_queries::LayoutValue| left == right,
                move |context, _, key: &crate::type_queries::TypeQueryKey| {
                    let layouts = $layout_family_for_evaluator
                        .get()
                        .ok_or(QueryAbort::ForeignRuntime)?;
                    evaluate_layout(context, &layouts, &$type_shapes_for_layout, key)
                },
            )
            .expect("the Layout family has one canonical name")
    }};
}
