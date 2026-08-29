macro_rules! register_parse_import_declaration_orders {
    ($parse_for_declaration_orders:ident, $runtime:ident) => {{
$runtime
            .family_with_equality_and_evaluator(
                "compiler.declaration-order",
                MODULE_QUERY_MEMO_RETENTION,
                |left: &DeclarationOrderValue, right: &DeclarationOrderValue| left == right,
                move |context, _, key: &ModuleQueryKey| {
                    let parsed = context
                        .query_registered(&$parse_for_declaration_orders, key.clone())?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let value = match &parsed.result {
                        Ok(module) => DeclarationOrderValue::Available(
                            module
                                .definitions()
                                .declaration_keys_in_source_order()
                                .cloned()
                                .collect::<Vec<_>>()
                                .into(),
                        ),
                        Err(_) => DeclarationOrderValue::Failure(
                            crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                module: key.0.clone(),
                            },
                        ),
                    };
                    let kind = if matches!(value, DeclarationOrderValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the DeclarationOrder family has one canonical name")
    }};
}
