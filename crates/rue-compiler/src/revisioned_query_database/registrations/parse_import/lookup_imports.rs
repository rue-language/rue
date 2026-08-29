macro_rules! register_parse_import_lookup_imports {
    ($declaration_memo_retention:ident, $index_for_import_lookup:ident, $lookup_import_eval_probe:ident, $resolve_import_for_lookup_evaluator:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.lookup-import",
                $declaration_memo_retention,
                |left: &LookupImportValue, right: &LookupImportValue| left == right,
                move |context, _, key: &LookupImportKey| {
                    #[cfg(test)]
                    $lookup_import_eval_probe
                        .lock()
                        .expect("lookup-import probe is not poisoned")
                        .push(key.clone());
                    let indexed = context.query_registered(
                        &$index_for_import_lookup,
                        ModuleQueryKey(key.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("ModuleIndex publishes typed values")
                    };
                    let (mut value, directive) = match &indexed.0 {
                        Ok(index) => {
                            let (normalized, directive) =
                                index.normalized_import(key.specifier.as_ref());
                            (
                                LookupImportValue::classify(normalized, directive),
                                directive,
                            )
                        }
                        // An unavailable index carries no consultable import
                        // directives, so the consulted path is a first-class
                        // absent binding.
                        Err(_) => (LookupImportValue(Err(ImportBindingFailure::Absent)), None),
                    };
                    if let LookupImportValue(Ok(binding)) = &mut value {
                        let directive = directive.ok_or(QueryAbort::Canceled)?;
                        let resolved = context.query_registered(
                            $resolve_import_for_lookup_evaluator
                                .get()
                                .expect("ResolveImport is installed before requests begin"),
                            ResolveImportKey {
                                occurrence: crate::ImportOccurrenceKey::from_directive(directive),
                                mode: ImportDemandMode::Rooted,
                            },
                        )?;
                        let rue_query::QueryOutcome::Success(resolved) = resolved.outcome() else {
                            unreachable!("ResolveImport publishes typed values")
                        };
                        match &resolved.resolution {
                            Some(crate::CanonicalImportResolution::Resolved(target)) => {
                                binding.target = Some(target.clone());
                            }
                            Some(crate::CanonicalImportResolution::Missing) => {
                                value = LookupImportValue(Err(ImportBindingFailure::Absent));
                            }
                            None => return Err(QueryAbort::Canceled),
                        }
                    }
                    let kind = if value.0.is_ok() {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the LookupImport family has one canonical name")
    }};
}
