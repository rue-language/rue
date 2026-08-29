macro_rules! register_backend_cfgs {
    ($call_abis_for_cfg:ident, $drop_glues_for_cfg:ident, $layouts_for_cfg:ident, $runtime:ident, $type_facts_for_cfg:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.cfg",
                BODY_QUERY_MEMO_RETENTION,
                crate::cfg_query::cfg_value_equal,
                move |context, _, key: &crate::cfg_query::CfgQueryKey| {
                    crate::cfg_query::evaluate_cfg(
                        context,
                        &$layouts_for_cfg,
                        &$type_facts_for_cfg,
                        &$drop_glues_for_cfg,
                        &$call_abis_for_cfg,
                        key,
                    )
                },
            )
            .expect("the Cfg family has one canonical name")
    }};
}
