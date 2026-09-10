macro_rules! register_body_body_transactions {
    ($body_transaction_evaluator_for_family:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.body-transaction",
                BODY_QUERY_MEMO_RETENTION,
                crate::body_query::transaction_equal,
                move |context, _, key: &crate::body_query::BodyQueryKey| {
                    // `None` before installation is a registration defect and
                    // after teardown is a request against a database that is
                    // gone; neither may proceed on a released evaluator.
                    let evaluator = $body_transaction_evaluator_for_family
                        .get()
                        .ok_or(QueryAbort::ForeignRuntime)?;
                    evaluator.evaluate(context, key)
                },
            )
            .expect("the BodyTransaction family has one canonical name")
    }};
}
