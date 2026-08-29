macro_rules! register_parse_import_module_source_bases {
    ($module_store_for_module_source_bases:ident, $parse_for_module_source_bases:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.module-source-basis",
                BODY_QUERY_MEMO_RETENTION,
                |left: &Option<rue_air::DurableBodySourceLocator>,
                 right: &Option<rue_air::DurableBodySourceLocator>| {
                    match (left, right) {
                        (Some(left), Some(right)) => {
                            left.file_id == right.file_id
                                && left.physical_path == right.physical_path
                        }
                        (None, None) => true,
                        _ => false,
                    }
                },
                move |context, _, key: &ModuleQueryKey| {
                    context.input(module_metadata_input(&key.0))?;
                    let parsed =
                        context.query_registered(&$parse_for_module_source_bases, key.clone())?;
                    let rue_query::QueryOutcome::Success(ParseModuleValue {
                        result: Ok(parsed),
                        ..
                    }) = parsed.outcome()
                    else {
                        return Ok(QueryOutput::success(None));
                    };
                    let view = module_input_view(
                        &$module_store_for_module_source_bases,
                        context.revision(),
                    )?;
                    let current = view
                        .metadata
                        .find_by(|leaf| leaf.module.cmp(&key.0))
                        .ok_or(QueryAbort::Canceled)?;
                    Ok(QueryOutput::success(Some(
                        rue_air::DurableBodySourceLocator {
                            file_id: current.file_id,
                            physical_path: current.physical_path.clone(),
                            source_length: u32::try_from(parsed.source_text().len())
                                .map_err(|_| QueryAbort::Canceled)?,
                        },
                    )))
                },
            )
            .expect("the ModuleSourceBasis family has one canonical name")
    }};
}
