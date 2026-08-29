macro_rules! register_semantic_drop_glues {
    ($runtime:ident, $type_facts_for_drop_glue:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.drop-glue",
                BODY_QUERY_MEMO_RETENTION,
                |left: &crate::type_queries::DropGlueValue,
                 right: &crate::type_queries::DropGlueValue| left == right,
                move |context, _, key: &crate::type_queries::TypeQueryKey| {
                    evaluate_drop_glue(context, &$type_facts_for_drop_glue, key)
                },
            )
            .expect("the DropGlue family has one canonical name")
    }};
}
