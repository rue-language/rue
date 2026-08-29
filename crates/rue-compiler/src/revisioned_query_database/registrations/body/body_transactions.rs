macro_rules! register_body_body_transactions {
    ($body_transaction_evaluator_for_family:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.body-transaction",
                BODY_QUERY_MEMO_RETENTION,
                crate::body_query::transaction_equal,
                move |context, _, key: &crate::body_query::BodyQueryKey| {
                    $body_transaction_evaluator_for_family
                        .get()
                        .expect("BodyTransaction evaluator is installed before requests begin")
                        .evaluate(context, key)
                },
            )
            .expect("the BodyTransaction family has one canonical name")
    }};
}
