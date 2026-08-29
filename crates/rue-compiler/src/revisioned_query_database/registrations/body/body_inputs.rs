#[allow(unused_macros)]
macro_rules! register_body_body_inputs {
    ($body_input_resolver:ident, $runtime:ident) => {{
        {
            let resolver = $body_input_resolver.clone();
            $runtime
                .family_with_equality_and_evaluator(
                    "compiler.test-body-input-probe",
                    BODY_QUERY_MEMO_RETENTION,
                    crate::body_query::body_input_equal,
                    move |context, _, key: &crate::body_query::BodyQueryKey| {
                        Ok(QueryOutput::success(resolver.resolve(context, key)?))
                    },
                )
                .expect("the test BodyInput probe family has one canonical name")
        }
    }};
}
