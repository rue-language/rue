macro_rules! register_parse_import_lookup_names {
    ($declaration_memo_retention:ident, $index_for_lookup:ident, $lookup_name_eval_probe:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.lookup-name",
                $declaration_memo_retention,
                |left: &LookupNameValue, right: &LookupNameValue| left == right,
                move |context, _, key: &LookupNameKey| {
                    #[cfg(test)]
                    $lookup_name_eval_probe
                        .lock()
                        .expect("lookup-name probe is not poisoned")
                        .push(key.clone());
                    let indexed = context
                        .query_registered(&$index_for_lookup, ModuleQueryKey(key.module.clone()))?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("ModuleIndex publishes typed values")
                    };
                    let result = match &indexed.0 {
                        Ok(index) => Ok(index
                            .definitions_for(key.namespace, key.name.as_ref())
                            .map(ModuleIndexEntry::lookup_fact)
                            .collect::<Vec<_>>()
                            .into()),
                        Err(_) => Err(LookupNameFailure::ModuleIndexUnavailable(
                            key.module.clone(),
                        )),
                    };
                    Ok(QueryOutput::success(LookupNameValue(result)))
                },
            )
            .expect("the LookupName family has one canonical name")
    }};
}
