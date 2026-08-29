macro_rules! register_body_warning_call_head_projections {
    ($declaration_memo_retention:ident, $parse_for_warning_call_heads:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.warning-call-head-projection",
                $declaration_memo_retention,
                |left: &WarningCallHeadProjectionValue, right: &WarningCallHeadProjectionValue| {
                    left == right
                },
                move |context, _, key: &WarningCallHeadProjectionQueryKey| {
                    let candidate = &key.0;
                    let parsed = context.query_registered(
                        &$parse_for_warning_call_heads,
                        ModuleQueryKey(candidate.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let module = match &parsed.result {
                        Ok(module) => module,
                        Err(_) => {
                            return Ok(QueryOutput::success(
                                WarningCallHeadProjectionValue::Failure(
                                    WarningBodyReferencesFailure::ParseRejected(
                                        candidate.module.clone(),
                                    ),
                                ),
                            )
                            .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                    };
                    let value = module
                        .declaration_warning_call_heads(candidate)
                        .map_or_else(
                            || {
                                WarningCallHeadProjectionValue::Failure(
                                    WarningBodyReferencesFailure::ParserCapabilityMismatch(
                                        candidate.clone(),
                                    ),
                                )
                            },
                            |heads| WarningCallHeadProjectionValue::Available(heads.clone()),
                        );
                    let kind = if matches!(value, WarningCallHeadProjectionValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the WarningCallHeadProjection family has one canonical name")
    }};
}
