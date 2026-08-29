macro_rules! register_parse_import_declaration_occurrence_indexes {
    ($parse_for_declaration_occurrences:ident, $runtime:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.declaration-occurrence-index",
                MODULE_QUERY_MEMO_RETENTION,
                declaration_occurrence_index_value_equal,
                move |context, _, key: &ModuleQueryKey| {
                    let parsed = context
                        .query_registered(&$parse_for_declaration_occurrences, key.clone())?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let value = match &parsed.result {
                        Ok(module) => DeclarationOccurrenceIndexValue::Available(Arc::new(
                            DeclarationOccurrenceIndex {
                                capabilities: module
                                    .definitions()
                                    .declaration_capabilities()
                                    .iter()
                                    .cloned()
                                    .map(|capability| (capability.key().clone(), capability))
                                    .collect(),
                            },
                        )),
                        Err(_) => DeclarationOccurrenceIndexValue::Failure(
                            crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                module: key.0.clone(),
                            },
                        ),
                    };
                    let kind = if matches!(value, DeclarationOccurrenceIndexValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the DeclarationOccurrenceIndex family has one canonical name")
    }};
}
