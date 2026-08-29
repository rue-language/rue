macro_rules! register_parse_import_module_indexes {
    ($module_index_build_probe:ident, $parse_for_index:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.module-index",
                MODULE_QUERY_MEMO_RETENTION,
                module_index_value_equal,
                move |context, _, key: &ModuleQueryKey| {
                    #[cfg(test)]
                    $module_index_build_probe
                        .lock()
                        .expect("module-index probe is not poisoned")
                        .push(key.0.clone());
                    let parsed = context.query_registered(&$parse_for_index, key.clone())?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let result = match &parsed.result {
                        Ok(module) => Ok(Arc::new(new_module_index(
                            module.revision().clone(),
                            module
                                .definitions()
                                .candidates()
                                .iter()
                                .map(|candidate| ModuleIndexEntry {
                                    namespace: candidate.namespace(),
                                    kind: candidate.kind(),
                                    visibility: candidate.visibility(),
                                    name: Arc::from(candidate.name()),
                                    language_item: module_index_entry_language_item(
                                        &key.0,
                                        candidate.kind(),
                                        candidate.name(),
                                    ),
                                    name_span: candidate.name_span(),
                                    declaration_span: candidate.declaration_span(),
                                })
                                .collect::<Vec<_>>()
                                .into(),
                            module.imports().to_vec().into(),
                        ))),
                        Err(errors) => Err(errors.clone()),
                    };
                    Ok(QueryOutput::success(ModuleIndexValue(result)))
                },
            )
            .expect("the ModuleIndex family has one canonical name")
    }};
}
