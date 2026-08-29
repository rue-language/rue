macro_rules! register_parse_import_parse_modules {
    ($parse_identity_resolution:ident, $parse_stage_for_parse_modules:ident, $parse_store:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.parse-module",
                MODULE_QUERY_MEMO_RETENTION,
                parse_module_value_equal,
                move |context, _, key: &ModuleQueryKey| {
                    context.input(module_source_input(&key.0))?;
                    let view = module_input_view(&$parse_store, context.revision())?;
                    // Taking the staged wave parse is a pure work handoff: the
                    // consumer verifies exact SourceId identity against the
                    // snapshot this query's declared input pinned, so the value
                    // remains a function of that input alone.
                    let staged = $parse_stage_for_parse_modules
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&key.0);
                    let (result, work) =
                        crate::parsed_modules::parse_source_snapshot_module_with_stage(
                            &view.snapshot,
                            &key.0,
                            staged,
                            &$parse_identity_resolution,
                        );
                    Ok(QueryOutput::success(ParseModuleValue { result, work }))
                },
            )
            .expect("the ParseModule family has one canonical name")
    }};
}
