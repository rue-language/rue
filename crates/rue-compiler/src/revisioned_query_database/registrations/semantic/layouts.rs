macro_rules! register_semantic_layouts {
    ($layout_family_for_evaluator:ident, $runtime:ident, $type_shapes_for_layout:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.layout",
                BODY_QUERY_MEMO_RETENTION,
                |left: &crate::type_queries::LayoutValue,
                 right: &crate::type_queries::LayoutValue| left == right,
                move |context, _, key: &crate::type_queries::TypeQueryKey| {
                    evaluate_layout(
                        context,
                        $layout_family_for_evaluator
                            .get()
                            .expect("Layout family is installed before requests"),
                        &$type_shapes_for_layout,
                        key,
                    )
                },
            )
            .expect("the Layout family has one canonical name")
    }};
}
