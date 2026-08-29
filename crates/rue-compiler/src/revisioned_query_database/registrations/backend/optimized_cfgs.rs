macro_rules! register_backend_optimized_cfgs {
    ($cfgs_for_optimization:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.optimized-cfg",
                BODY_QUERY_MEMO_RETENTION,
                crate::cfg_query::cfg_value_equal,
                move |context, _, key: &crate::cfg_query::OptimizedCfgQueryKey| {
                    crate::cfg_query::evaluate_optimized_cfg(context, &$cfgs_for_optimization, key)
                },
            )
            .expect("the OptimizedCfg family has one canonical name")
    }};
}
