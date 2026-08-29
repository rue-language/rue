macro_rules! register_parse_import_resolve_imports {
    ($evaluator_store:ident, $index_for_import_resolution:ident, $resolve_identity_resolution:ident, $runtime:ident) => {{
        $runtime
            .family_with_evaluator(
                "compiler.resolve-import",
                IMPORT_OCCURRENCE_QUERY_MEMO_RETENTION,
                move |context, _, key: &ResolveImportKey| {
                    let view = {
                        let store = lock_import_store(&$evaluator_store);
                        store
                            .revisions
                            .iter()
                            .find(|view| view.revision == context.revision())
                            .cloned()
                    }
                    .ok_or_else(|| QueryAbort::UnpublishedRevision(context.revision()))?;
                    context.input(import_context_input())?;
                    let indexed = context.query_registered(
                        &$index_for_import_resolution,
                        ModuleQueryKey(key.occurrence.importer().clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("ModuleIndex publishes typed values")
                    };
                    let site = indexed
                        .0
                        .as_ref()
                        .ok()
                        .and_then(|index| index.import_occurrence(&key.occurrence));
                    let Some(site) = site else {
                        return Ok(QueryOutput::success(ResolveImportValue {
                            site_found: false,
                            groups: Arc::from([]),
                            requests: Arc::from([]),
                            speculative_blocked: false,
                            resolution: None,
                        }));
                    };
                    context.input(accepted_read_input(key.occurrence.importer()))?;
                    let importer = view
                        .accepted_reads
                        .find_module(key.occurrence.importer())
                        .expect("indexed importer retains accepted-read provenance");
                    let occurrence = crate::ImportOccurrenceKey::from_directive(site);
                    let groups = crate::import_discovery::discovery_groups_for_occurrence(
                        &view.context,
                        &occurrence,
                        importer.requested_path(),
                    )
                    .expect("accepted import provenance and captured context are canonical");
                    if groups.is_empty() {
                        // The occurrence's candidate escapes its project or
                        // captured standard-library root (ADR-0078): no
                        // filesystem request exists and the rejection is
                        // deterministic, so the binding is a first-class
                        // Missing terminal. The E0713 diagnostic is owned by
                        // the diagnostic projection.
                        return Ok(QueryOutput::success(ResolveImportValue {
                            site_found: true,
                            groups: Arc::from([]),
                            requests: Arc::from([]),
                            speculative_blocked: false,
                            resolution: Some(crate::CanonicalImportResolution::Missing),
                        }));
                    }
                    for request in groups.iter().flat_map(|group| group.iter()) {
                        let present = context
                            .optional_input(import_observation_input(request))
                            .is_some();
                        assert_eq!(present, view.ledger.get(request).is_some());
                    }
                    let pending = pending_occurrence_requests(&groups, &view.ledger);
                    let speculative_blocked =
                        key.mode == ImportDemandMode::Speculative && !pending.is_empty();
                    let resolution = if pending.is_empty()
                        && !crate::import_discovery::exact_import_has_failures(
                            &groups,
                            &view.ledger,
                        ) {
                        pub(super) enum ProvenanceLookupFailure {
                            Query(QueryAbort),
                            Invalid,
                        }

                        if crate::import_discovery::validate_exact_import_occurrence(
                            &groups,
                            &view.ledger,
                        )
                        .is_err()
                        {
                            None
                        } else {
                            let winner = crate::import_discovery::exact_import_winner(
                                groups.iter(),
                                &view.ledger,
                            );
                            match crate::import_discovery::resolve_exact_import_winner(
                                winner,
                                |source| {
                                    context
                                        .input(accepted_import_provenance_input(
                                            source.metadata_identity(),
                                        ))
                                        .map_err(ProvenanceLookupFailure::Query)?;
                                    crate::import_discovery::accepted_import_module(
                                        source,
                                        &view.accepted_reads,
                                        &$resolve_identity_resolution,
                                    )
                                    .map_err(|_| ProvenanceLookupFailure::Invalid)
                                },
                            ) {
                                Ok(resolution) => Some(resolution),
                                Err(ProvenanceLookupFailure::Query(abort)) => return Err(abort),
                                Err(ProvenanceLookupFailure::Invalid) => None,
                            }
                        }
                    } else {
                        None
                    };
                    Ok(QueryOutput::success(ResolveImportValue {
                        site_found: true,
                        groups: groups.into(),
                        requests: if speculative_blocked {
                            Arc::from([])
                        } else {
                            pending.into()
                        },
                        speculative_blocked,
                        resolution,
                    }))
                },
            )
            .expect("the ResolveImport family has one canonical name")
    }};
}
