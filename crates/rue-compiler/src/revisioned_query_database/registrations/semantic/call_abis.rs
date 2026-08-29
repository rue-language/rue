macro_rules! register_semantic_call_abis {
    ($declaration_shells_for_call_abi:ident, $layouts_for_call_abi:ident, $lookup_names_for_call_abi:ident, $produced_anonymous_for_call_abi:ident, $runtime:ident, $semantic_nucleus_for_call_abi:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.call-abi",
                BODY_QUERY_MEMO_RETENTION,
                |left: &crate::type_queries::CallAbiValue,
                 right: &crate::type_queries::CallAbiValue| left == right,
                move |context, _, key: &crate::type_queries::CallAbiQueryKey| {
                    evaluate_call_abi(
                        context,
                        &$semantic_nucleus_for_call_abi,
                        &$declaration_shells_for_call_abi,
                        &$lookup_names_for_call_abi,
                        &$produced_anonymous_for_call_abi,
                        &$layouts_for_call_abi,
                        key,
                    )
                },
            )
            .expect("the CallAbi family has one canonical name")
    }};
}
