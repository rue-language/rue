macro_rules! register_semantic_type_facts {
    ($lookup_names_for_type_facts:ident, $produced_anonymous_for_type_facts:ident, $runtime:ident, $semantic_nucleus_for_type_facts:ident, $type_facts_family_for_evaluator:ident, $type_shapes_for_type_facts:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.type-facts",
                BODY_QUERY_MEMO_RETENTION,
                |left: &crate::type_queries::TypeFactsValue,
                 right: &crate::type_queries::TypeFactsValue| left == right,
                move |context, _, key: &crate::type_queries::TypeQueryKey| {
                    evaluate_type_facts(
                        context,
                        $type_facts_family_for_evaluator
                            .get()
                            .expect("TypeFacts family is installed before requests"),
                        &$type_shapes_for_type_facts,
                        &$semantic_nucleus_for_type_facts,
                        &$lookup_names_for_type_facts,
                        &$produced_anonymous_for_type_facts,
                        key,
                    )
                },
            )
            .expect("the TypeFacts family has one canonical name")
    }};
}
