macro_rules! register_body_body_source_bases {
    ($classifications_for_body_source_bases:ident, $module_store_for_body_source_bases:ident, $parse_for_body_source_bases:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.body-source-basis",
                BODY_QUERY_MEMO_RETENTION,
                crate::body_query::body_source_basis_equal,
                move |context, _, key: &crate::body_query::BodyQueryKey| {
                    let Some(definition) = body_source_definition_key(&key.instance).cloned()
                    else {
                        return Ok(QueryOutput::success(None));
                    };
                    let classification = context.query_registered(
                        &$classifications_for_body_source_bases,
                        StableDeclarationClassificationQueryKey(definition),
                    )?;
                    let rue_query::QueryOutcome::Success(
                        StableDeclarationClassificationQueryValue::Selected(candidate),
                    ) = classification.outcome()
                    else {
                        return Ok(QueryOutput::success(None));
                    };
                    // ParseModule deliberately has semantic equality and may
                    // retain across trivia-only relocation. This locator
                    // projection must also observe the exact module-source leaf
                    // so presentation coordinates refresh independently.
                    context.input(module_source_input(&candidate.module))?;
                    context.input(module_metadata_input(&candidate.module))?;
                    let parsed = context.query_registered(
                        &$parse_for_body_source_bases,
                        ModuleQueryKey(candidate.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let Ok(module) = &parsed.result else {
                        return Ok(QueryOutput::success(None));
                    };
                    let spans = module.body_source_spans(candidate).or_else(|| {
                        Some((
                            module
                                .definitions()
                                .declaration_locator(candidate)?
                                .declaration_span,
                            module.definitions().producer_fragment_span(candidate)?,
                        ))
                    });
                    let Some((declaration_span, body_span)) = spans else {
                        return Ok(QueryOutput::success(None));
                    };
                    let view = module_input_view(
                        &$module_store_for_body_source_bases,
                        context.revision(),
                    )?;
                    let current = view
                        .metadata
                        .find_by(|leaf| leaf.module.cmp(&candidate.module))
                        .ok_or(QueryAbort::Canceled)?;
                    let source_length = view
                        .snapshot
                        .source(current.file_id)
                        .and_then(|source| u32::try_from(source.source.len()).ok())
                        .ok_or(QueryAbort::Canceled)?;
                    let physical_path = view
                        .snapshot
                        .metadata()
                        .physical_path(current.file_id)
                        .ok_or(QueryAbort::Canceled)?;
                    Ok(QueryOutput::success(Some(
                        crate::body_query::BodySourceLocator {
                            file_id: current.file_id,
                            physical_path: Arc::from(physical_path),
                            source_length,
                            declaration_start: declaration_span.start,
                            declaration_end: declaration_span.end,
                            body_start: body_span.start,
                            body_end: body_span.end,
                        },
                    )))
                },
            )
            .expect("the BodySourceBasis family has one canonical name")
    }};
}
