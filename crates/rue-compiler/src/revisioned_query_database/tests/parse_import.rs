use super::*;

#[test]
fn incremental_pending_requests_stop_at_conclusive_vendored_std_failure() {
    let context = ImportDiscoveryContext::new(1, "/project", Some("/sdk"), "all").unwrap();
    let occurrence = crate::ImportOccurrenceKey::from_directive(&crate::ImportDirective::new(
        ModuleId::from_logical_path("main.rue").unwrap(),
        0,
        3,
        "std".into(),
    ));
    let groups = crate::import_discovery::discovery_groups_for_occurrence(
        &context,
        &occurrence,
        "/project/main.rue",
    )
    .unwrap();
    assert_eq!(groups.len(), 2);
    let mut ledger = ImportObservationLedger::default();
    ledger
        .record(
            ImportObservation::failure(
                groups[0][0].clone(),
                crate::ImportObservationStatus::PresentUnreadable("synthetic".into()),
            )
            .unwrap(),
        )
        .unwrap();
    let toolchain = crate::AcceptedImportSource::new(
        groups[1][0].requested_path(),
        groups[1][0].requested_path(),
        PhysicalFileIdentity::new(4, 1),
        FileMetadataFingerprint::new(1, 1, 1),
        Arc::new("pub fn answer() -> i32 { 7 }".into()),
    )
    .unwrap();
    ledger
        .record(ImportObservation::accepted(groups[1][0].clone(), toolchain).unwrap())
        .unwrap();

    assert!(pending_occurrence_requests(&groups, &ledger).is_empty());
}

#[test]
fn editing_one_demanded_module_reuses_other_module_terminals() {
    let first = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn a() {}"),
            (2, "/b.rue", "b.rue", "fn b() -> i32 { 1 }"),
        ],
        1,
    );
    let second = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn a() {}"),
            (2, "/b.rue", "b.rue", "fn b() -> i32 { 2 }"),
        ],
        1,
    );
    let a = ModuleId::from_logical_path("a.rue").unwrap();
    let b = ModuleId::from_logical_path("b.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let first_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&first), &first);
    let (first_a_parse, first_a_index) = database.module_terminals(first_revision, a.clone());
    let (first_b_parse, first_b_index) = database.module_terminals(first_revision, b.clone());

    let second_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&second), &second);
    let (second_a_parse, second_a_index) = database.module_terminals(second_revision, a);
    let (second_b_parse, second_b_index) = database.module_terminals(second_revision, b);

    assert!(Arc::ptr_eq(&first_a_parse, &second_a_parse));
    assert!(Arc::ptr_eq(&first_a_index, &second_a_index));
    assert!(!Arc::ptr_eq(&first_b_parse, &second_b_parse));
    assert!(!Arc::ptr_eq(&first_b_index, &second_b_index));
}

#[test]
fn file_id_renumbering_reuses_terminals_and_rebinds_current_projections() {
    let first = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            (2, "/b.rue", "b.rue", "fn b() -> i32 { 2 }"),
        ],
        1,
    );
    let second = source_snapshot(
        &[
            (1, "/inserted.rue", "inserted.rue", "fn inserted() {}"),
            (2, "/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            (3, "/b.rue", "b.rue", "fn b() -> i32 { 2 }"),
        ],
        2,
    );
    let a = ModuleId::from_logical_path("a.rue").unwrap();
    let b = ModuleId::from_logical_path("b.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let first_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&first), &first);
    let (first_parse, first_index) = database.module_terminals(first_revision, a.clone());
    let _ = database.module_terminals(first_revision, b.clone());

    let second_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&second), &second);
    let (second_parse, second_index) = database.module_terminals(second_revision, a.clone());
    assert!(Arc::ptr_eq(&first_parse, &second_parse));
    assert!(Arc::ptr_eq(&first_index, &second_index));
    assert_eq!(second_parse.file_id(), FileId::new(1));

    let (program, parse_work) = database.parse_program(second_revision, &a, [a.clone(), b.clone()]);
    let program = program.unwrap();
    assert_eq!(parse_work.syntax.parser_invocations, 0);
    assert_eq!(parse_work.modules_rebound, 2);
    let projected_a = program.module(&a).unwrap();
    assert_eq!(projected_a.file_id(), FileId::new(2));
    assert!(projected_a.transient_token_buffer_was_released());
    assert!(
        projected_a
            .presented_tokens_for_test()
            .iter()
            .all(|token| token.span.file_id == FileId::new(2))
    );
    assert!(projected_a.ast().items.iter().all(|item| match item {
        rue_parser::Item::Function(function) => {
            function.span.file_id == FileId::new(2)
                && function.body.span().file_id == FileId::new(2)
        }
        _ => false,
    }));
    assert!(
        projected_a
            .definitions()
            .candidates()
            .iter()
            .all(|definition| {
                definition.name_span().file_id == FileId::new(2)
                    && definition.declaration_span().file_id == FileId::new(2)
            })
    );

    let indexes = database
        .projected_module_indexes(second_revision, &program)
        .unwrap();
    let a_index = indexes
        .iter()
        .find(|index| index.revision.module == a)
        .unwrap();
    assert!(a_index.definitions.iter().all(|definition| {
        definition.name_span.file_id == FileId::new(2)
            && definition.declaration_span.file_id == FileId::new(2)
    }));
    let merged = crate::canonical_merge::merge_parsed_modules_reusing_indexes(
        &program, &indexes, None, None,
    )
    .unwrap();
    let ordered_modules = program
        .modules()
        .iter()
        .map(|module| module.module_id().clone())
        .collect::<Vec<_>>();
    let (module_rirs, query_work) =
        database.compose_candidate_module_rirs(second_revision, ordered_modules);
    let module_rirs = module_rirs.unwrap();
    assert_eq!(query_work.modules_visited, 2);
    assert!(query_work.items_visited > 0);
    let projected_rir = crate::canonical_lower::project_candidate_module_rirs_with_work(
        &merged,
        &module_rirs,
        query_work,
        rue_lexer::MAX_INTERNED_STRINGS,
    )
    .unwrap();
    assert_eq!(projected_rir.work().modules_visited, 2);
    assert!(projected_rir.work().items_visited > 0);
    assert_eq!(projected_rir.work().modules_projected, 2);
    assert_eq!(
        projected_rir.work().instructions_appended,
        projected_rir.rir().len()
    );
    assert_eq!(
        projected_rir.work().payload_words_appended,
        projected_rir.rir().extra_len()
    );
    assert!(
        projected_rir
            .rir()
            .iter()
            .any(|(_, instruction)| instruction.span.file_id == FileId::new(2))
    );
    let mut projected_files = BTreeSet::new();
    projected_rir
        .rir()
        .try_visit_span_slots(
            || Ok::<_, std::convert::Infallible>(()),
            |_slot, span| {
                projected_files.insert(span.file_id.index());
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(
        projected_files,
        BTreeSet::from([2, 3]),
        "canonical RIR presentation rebinds every span family to the current revision"
    );
}

#[test]
fn reused_parse_failures_are_rebound_to_the_current_file_id() {
    let first = source_snapshot(&[(1, "/broken.rue", "broken.rue", "fn broken( {")], 1);
    let second = source_snapshot(
        &[
            (1, "/inserted.rue", "inserted.rue", "fn inserted() {}"),
            (2, "/broken.rue", "broken.rue", "fn broken( {"),
        ],
        2,
    );
    let broken = ModuleId::from_logical_path("broken.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let first_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&first), &first);
    let (first_error, first_work) =
        database.parse_program(first_revision, &broken, std::iter::once(broken.clone()));
    assert_eq!(first_work.syntax.parser_invocations, 1);
    assert_eq!(
        first_error
            .unwrap_err()
            .first()
            .unwrap()
            .span()
            .unwrap()
            .file_id,
        FileId::new(1)
    );

    let second_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&second), &second);
    let (second_error, second_work) =
        database.parse_program(second_revision, &broken, std::iter::once(broken.clone()));
    assert_eq!(second_work.syntax.parser_invocations, 0);
    assert_eq!(second_work.modules_reused, 1);
    assert_eq!(
        second_error
            .unwrap_err()
            .first()
            .unwrap()
            .span()
            .unwrap()
            .file_id,
        FileId::new(2)
    );
}

#[test]
fn invalid_undemanded_module_is_neither_parsed_nor_lowered() {
    let base = source_snapshot(&[(1, "/a.rue", "a.rue", "fn a() {}")], 1);
    let snapshot = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn a() {}"),
            (2, "/broken.rue", "broken.rue", "fn broken( {"),
        ],
        1,
    );
    let a = ModuleId::from_logical_path("a.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let base_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&base), &base);
    let (base_parse, base_index) = database.module_terminals(base_revision, a.clone());
    assert_eq!(database.runtime.metrics().claims, 2);
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let (parsed, work) = database.parse_program(revision, &a, [a.clone()]);
    assert!(parsed.is_ok());
    assert_eq!(work.syntax.parser_invocations, 0);
    assert!(
        database
            .compose_candidate_module_rirs(revision, [a])
            .0
            .is_ok()
    );
    assert_eq!(database.runtime.metrics().claims, 4);
    let demanded = ModuleId::from_logical_path("a.rue").unwrap();
    let (next_parse, next_index) = database.module_terminals(revision, demanded);
    assert!(Arc::ptr_eq(&base_parse, &next_parse));
    assert!(Arc::ptr_eq(&base_index, &next_index));
}

#[test]
fn parse_module_frontier_parallelizes_and_reports_exact_work() {
    let texts = (0..8)
        .map(|index| format!("fn f{index}() -> i32 {{ {index} }}\n"))
        .collect::<Vec<_>>();
    let paths = (0..8)
        .map(|index| format!("/m{index}.rue"))
        .collect::<Vec<_>>();
    let logical = (0..8)
        .map(|index| format!("m{index}.rue"))
        .collect::<Vec<_>>();
    let entries = (0..8)
        .map(|index| {
            (
                index as u32 + 1,
                paths[index].as_str(),
                logical[index].as_str(),
                texts[index].as_str(),
            )
        })
        .collect::<Vec<_>>();
    let snapshot = source_snapshot(&entries, 1);
    let modules = logical
        .iter()
        .map(|path| ModuleId::from_logical_path(path).unwrap())
        .collect::<Vec<_>>();
    for _ in 0..16 {
        let root = modules[0].clone();
        let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
        let revision = revision_for(&mut database, &snapshot);
        let (program, work) = database.parse_program(revision, &root, modules.clone());
        assert!(program.is_ok());
        assert_eq!(work.frontier_items, 8);
        assert_eq!(work.frontier_batches, 1);
        assert_eq!(work.frontier_batch_overhead, 1);
        assert_eq!(work.modules_reparsed, 8);
        assert_eq!(work.modules_reused, 0);
        let scheduling = database.runtime_metrics_for_test();
        assert_eq!(scheduling.batch_worker_slots_requested, 7);
        assert_eq!(scheduling.batch_worker_slots_granted, 3);
        assert_eq!(scheduling.batch_worker_lanes_entered, 4);
    }
}

#[test]
fn parse_module_frontier_reuses_unedited_children_on_narrow_edit() {
    let first = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            (2, "/b.rue", "b.rue", "fn b() -> i32 { 2 }"),
            (3, "/c.rue", "c.rue", "fn c() -> i32 { 3 }"),
            (4, "/d.rue", "d.rue", "fn d() -> i32 { 4 }"),
        ],
        1,
    );
    let second = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            (2, "/b.rue", "b.rue", "fn b() -> i32 { 20 }"),
            (3, "/c.rue", "c.rue", "fn c() -> i32 { 3 }"),
            (4, "/d.rue", "d.rue", "fn d() -> i32 { 4 }"),
        ],
        1,
    );
    let modules =
        ["a.rue", "b.rue", "c.rue", "d.rue"].map(|path| ModuleId::from_logical_path(path).unwrap());
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
    let first_revision = revision_for(&mut database, &first);
    let (_, cold) = database.parse_program(first_revision, &modules[0], modules.clone());
    assert_eq!(cold.modules_reparsed, 4);
    assert_eq!(cold.previous_module_lookups, 4);

    let no_op_revision = revision_for(&mut database, &first);
    let (_, cross_revision_noop) =
        database.parse_program(no_op_revision, &modules[0], modules.clone());
    assert_eq!(cross_revision_noop.frontier_items, 4);
    assert_eq!(cross_revision_noop.frontier_batches, 1);
    assert_eq!(cross_revision_noop.frontier_batch_overhead, 0);
    assert_eq!(cross_revision_noop.previous_module_lookups, 4);
    assert_eq!(cross_revision_noop.modules_reparsed, 0);
    assert_eq!(cross_revision_noop.modules_reused, 4);

    let second_revision = revision_for(&mut database, &second);
    let (_, narrow) = database.parse_program(second_revision, &modules[0], modules.clone());
    assert_eq!(narrow.frontier_items, 4);
    assert_eq!(narrow.frontier_batches, 1);
    assert_eq!(narrow.frontier_batch_overhead, 1);
    assert_eq!(narrow.previous_module_lookups, 5);
    assert_eq!(narrow.modules_reparsed, 1);
    assert_eq!(narrow.modules_reused, 3);
    let noop_root = modules[0].clone();
    let (_, noop) = database.parse_program(second_revision, &noop_root, modules);
    assert_eq!(noop.modules_reparsed, 0);
    assert_eq!(noop.modules_reused, 4);
    assert_eq!(noop.frontier_batch_overhead, 0);
    assert_eq!(noop.previous_module_lookups, 0);
}

#[test]
fn parse_module_frontier_one_and_many_workers_preserve_error_order() {
    let snapshot = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn a( {}"),
            (2, "/b.rue", "b.rue", "fn b( {}"),
            (3, "/c.rue", "c.rue", "fn c( {}"),
        ],
        1,
    );
    let modules =
        ["c.rue", "a.rue", "b.rue"].map(|path| ModuleId::from_logical_path(path).unwrap());
    let run = |workers| {
        let mut database = RevisionedQueryDatabase::with_query_concurrency(workers);
        let revision = revision_for(&mut database, &snapshot);
        database
            .parse_program(revision, &modules[1], modules.clone())
            .0
            .unwrap_err()
    };
    for _ in 0..16 {
        assert_eq!(run(1), run(4));
    }
}

#[test]
fn warning_reference_frontier_parallelizes_and_reports_exact_work() {
    let text = (0..32)
        .map(|index| format!("fn f{index}() -> i32 {{ {index} }}\n"))
        .collect::<String>();
    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let keys: Arc<[crate::body_query::BodyQueryKey]> = (0..32)
        .map(|index| {
            crate::body_query::BodyQueryKey::new(
                free_function_instance(&module, &format!("f{index}")),
                semantic_configuration(),
            )
        })
        .collect::<Vec<_>>()
        .into();
    for _ in 0..16 {
        let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
        let revision = revision_for(&mut database, &snapshot);
        database
            .parse_program(revision, &module, [module.clone()])
            .0
            .unwrap();
        let (attempt, executions) = database.warning_body_reference_frontier(
            revision,
            keys.clone(),
            CancellationToken::new(),
        );
        assert!(attempt.terminal().is_some());
        assert_eq!(
            executions
                .iter()
                .flatten()
                .filter(|execution| execution.execution == RequestExecution::Computed)
                .count(),
            32
        );
        assert_eq!(
            attempt
                .work()
                .iter()
                .find(|(name, _)| name.as_ref() == "warning-reference.frontier.items")
                .map(|(_, amount)| *amount),
            Some(32)
        );
        assert_eq!(
            attempt
                .work()
                .iter()
                .find(|(name, _)| name.as_ref() == "warning-reference.frontier.batches")
                .map(|(_, amount)| *amount),
            Some(1)
        );
        assert_eq!(
            attempt
                .work()
                .iter()
                .find(|(name, _)| name.as_ref() == "warning-reference.frontier.overhead")
                .map(|(_, amount)| *amount),
            Some(1)
        );
        assert_eq!(
            attempt
                .nested_attempts()
                .iter()
                .filter(|attempt| attempt.node().family() == "compiler.warning-body-references")
                .count(),
            32
        );
        let scheduling = database.runtime_metrics_for_test();
        // The warning aggregate schedules three prerequisite projections and
        // the final body-reference frontier over the same 32 cold bodies.
        assert_eq!(scheduling.batch_worker_slots_requested, 4 * 31);
        assert_eq!(scheduling.batch_worker_slots_granted, 4 * 3);
        assert_eq!(scheduling.batch_worker_lanes_entered, 4 * 4);
    }
}

#[test]
fn warning_reference_frontier_retains_large_cross_revision_narrow_reuse() {
    const FUNCTIONS: usize = 40;
    let source = |changed: bool| {
        (0..FUNCTIONS)
            .map(|index| {
                if changed && index == 17 {
                    format!("fn f{index}() -> i32 {{ f0() }}\n")
                } else {
                    format!("fn f{index}() -> i32 {{ {index} }}\n")
                }
            })
            .collect::<String>()
    };
    let before_text = source(false);
    let after_text = source(true);
    let before = source_snapshot(&[(1, "/main.rue", "main.rue", &before_text)], 1);
    let after = source_snapshot(&[(1, "/main.rue", "main.rue", &after_text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let keys: Arc<[crate::body_query::BodyQueryKey]> = (0..FUNCTIONS)
        .map(|index| {
            crate::body_query::BodyQueryKey::new(
                free_function_instance(&module, &format!("f{index}")),
                semantic_configuration(),
            )
        })
        .collect::<Vec<_>>()
        .into();

    let run = |workers| {
        let mut database = RevisionedQueryDatabase::with_query_concurrency(workers);
        let aggregate_key = WarningBodyReferencesBatchKey {
            bodies: keys.clone(),
        };
        let first_revision = revision_for(&mut database, &before);
        database
            .parse_program(first_revision, &module, [module.clone()])
            .0
            .unwrap();
        let (cold, cold_executions) = database.warning_body_reference_frontier(
            first_revision,
            keys.clone(),
            CancellationToken::new(),
        );
        assert!(cold.terminal().is_some());
        assert_eq!(
            cold_executions
                .iter()
                .flatten()
                .filter(|execution| execution.execution == RequestExecution::Computed)
                .count(),
            FUNCTIONS
        );
        drop(cold);
        assert!(
            database
                .warning_body_reference_batches
                .contains_retained_key(&aggregate_key),
            "the aggregate family, not the completed request lease, retains the frontier"
        );
        assert!(
            keys.iter()
                .all(|key| database.warning_body_references.contains_retained_key(key))
        );

        let no_op_revision = revision_for(&mut database, &before);
        database
            .parse_program(no_op_revision, &module, [module.clone()])
            .0
            .unwrap();
        let (no_op, _) = database.warning_body_reference_frontier(
            no_op_revision,
            keys.clone(),
            CancellationToken::new(),
        );
        assert_eq!(no_op.execution(), RequestExecution::Reused);
        assert_eq!(
            no_op
                .work()
                .iter()
                .find_map(|(name, amount)| {
                    (name.as_ref() == "warning-reference.frontier.overhead").then_some(*amount)
                })
                .unwrap_or(0),
            0
        );
        drop(no_op);
        assert!(
            database
                .warning_body_reference_batches
                .contains_retained_key(&aggregate_key),
            "cross-revision green reuse remains family-owned after request release"
        );

        let narrow_revision = revision_for(&mut database, &after);
        database
            .parse_program(narrow_revision, &module, [module.clone()])
            .0
            .unwrap();
        let (narrow, narrow_executions) = database.warning_body_reference_frontier(
            narrow_revision,
            keys.clone(),
            CancellationToken::new(),
        );
        assert!(narrow.terminal().is_some());
        let executions = narrow_executions
            .into_iter()
            .map(|execution| {
                execution
                    .expect("red aggregate requests every child")
                    .execution
            })
            .collect::<Vec<_>>();
        assert_eq!(
            executions
                .iter()
                .filter(|execution| **execution == RequestExecution::Computed)
                .count(),
            1
        );
        assert_eq!(
            executions
                .iter()
                .filter(|execution| **execution == RequestExecution::Reused)
                .count(),
            FUNCTIONS - 1
        );
        drop(narrow);
        assert!(
            keys.iter()
                .all(|key| database.warning_body_references.contains_retained_key(key))
        );
        executions
    };

    assert_eq!(run(1), run(4));
}

/// Reproducible final-code latency witness for the parse and warning
/// frontiers. Run with:
/// `scripts/rue unit compiler rue_1667_frontier_latency_witness --ignored --nocapture`.
/// Timing remains observational; exact work and overlap are assertions.
#[test]
#[ignore]
fn rue_1667_frontier_latency_witness() {
    const SAMPLES: usize = 5;
    const PARSE_MODULES: usize = 16;
    const PARSE_COMMENT_BYTES: usize = 256_000;
    const WARNING_FUNCTIONS: usize = 512;
    const WARNING_CALLEES_PER_BODY: usize = 128;

    let parse_texts = (0..PARSE_MODULES)
        .map(|index| {
            format!(
                "// {}\nfn f{index}() -> i32 {{ {index} }}\n",
                "x".repeat(PARSE_COMMENT_BYTES)
            )
        })
        .collect::<Vec<_>>();
    let parse_paths = (0..PARSE_MODULES)
        .map(|index| format!("/m{index}.rue"))
        .collect::<Vec<_>>();
    let parse_logical = (0..PARSE_MODULES)
        .map(|index| format!("m{index}.rue"))
        .collect::<Vec<_>>();
    let parse_entries = (0..PARSE_MODULES)
        .map(|index| {
            (
                index as u32 + 1,
                parse_paths[index].as_str(),
                parse_logical[index].as_str(),
                parse_texts[index].as_str(),
            )
        })
        .collect::<Vec<_>>();
    let parse_snapshot = source_snapshot(&parse_entries, 1);
    let parse_modules = parse_logical
        .iter()
        .map(|path| ModuleId::from_logical_path(path).unwrap())
        .collect::<Vec<_>>();

    let warning_calls = (0..WARNING_CALLEES_PER_BODY)
        .map(|index| format!("f{index}()"))
        .collect::<Vec<_>>()
        .join(" + ");
    let warning_text = (0..WARNING_FUNCTIONS)
        .map(|index| format!("fn f{index}() -> i32 {{ {warning_calls} }}\n"))
        .collect::<String>();
    let warning_snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", &warning_text)], 1);
    let warning_module = ModuleId::from_logical_path("main.rue").unwrap();
    let warning_keys: Arc<[crate::body_query::BodyQueryKey]> = (0..WARNING_FUNCTIONS)
        .map(|index| {
            crate::body_query::BodyQueryKey::new(
                free_function_instance(&warning_module, &format!("f{index}")),
                semantic_configuration(),
            )
        })
        .collect::<Vec<_>>()
        .into();

    let measure = |workers| {
        let mut parse_cold = Vec::with_capacity(SAMPLES);
        let mut parse_warm = Vec::with_capacity(SAMPLES);
        let mut warning_cold = Vec::with_capacity(SAMPLES);
        let mut warning_warm = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let mut parse_database = RevisionedQueryDatabase::with_query_concurrency(workers);
            let revision = revision_for(&mut parse_database, &parse_snapshot);
            let start = std::time::Instant::now();
            let (_, cold_work) =
                parse_database.parse_program(revision, &parse_modules[0], parse_modules.clone());
            parse_cold.push(start.elapsed().as_micros());
            assert_eq!(cold_work.modules_reparsed, PARSE_MODULES);
            assert_eq!(cold_work.frontier_batch_overhead, 1);
            let start = std::time::Instant::now();
            let (_, warm_work) =
                parse_database.parse_program(revision, &parse_modules[0], parse_modules.clone());
            parse_warm.push(start.elapsed().as_micros());
            assert_eq!(warm_work.modules_reused, PARSE_MODULES);
            assert_eq!(warm_work.previous_module_lookups, 0);
            assert_eq!(warm_work.frontier_batch_overhead, 0);
            assert_eq!(
                parse_database.runtime_metrics_for_test().peak_query_workers > 1,
                workers > 1
            );

            let mut warning_database = RevisionedQueryDatabase::with_query_concurrency(workers);
            let revision = revision_for(&mut warning_database, &warning_snapshot);
            warning_database
                .parse_program(revision, &warning_module, [warning_module.clone()])
                .0
                .unwrap();
            let start = std::time::Instant::now();
            let (cold, cold_executions) = warning_database.warning_body_reference_frontier(
                revision,
                warning_keys.clone(),
                CancellationToken::new(),
            );
            warning_cold.push(start.elapsed().as_micros());
            assert!(cold.terminal().is_some());
            assert_eq!(
                cold_executions
                    .iter()
                    .flatten()
                    .filter(|execution| execution.execution == RequestExecution::Computed)
                    .count(),
                WARNING_FUNCTIONS
            );
            let no_op_revision = revision_for(&mut warning_database, &warning_snapshot);
            warning_database
                .parse_program(no_op_revision, &warning_module, [warning_module.clone()])
                .0
                .unwrap();
            let start = std::time::Instant::now();
            let (warm, _) = warning_database.warning_body_reference_frontier(
                no_op_revision,
                warning_keys.clone(),
                CancellationToken::new(),
            );
            warning_warm.push(start.elapsed().as_micros());
            assert_eq!(warm.execution(), RequestExecution::Reused);
            assert_eq!(
                warm.work()
                    .iter()
                    .find_map(|(name, amount)| {
                        (name.as_ref() == "warning-reference.frontier.overhead").then_some(*amount)
                    })
                    .unwrap_or(0),
                0
            );
            assert_eq!(
                warning_database
                    .runtime_metrics_for_test()
                    .peak_query_workers
                    > 1,
                workers > 1
            );
        }
        for samples in [
            &mut parse_cold,
            &mut parse_warm,
            &mut warning_cold,
            &mut warning_warm,
        ] {
            samples.sort_unstable();
        }
        (parse_cold, parse_warm, warning_cold, warning_warm)
    };

    for workers in [1, 4] {
        let (parse_cold, parse_warm, warning_cold, warning_warm) = measure(workers);
        eprintln!(
            "RUE-1667 workers={workers} samples={SAMPLES} parse_modules={PARSE_MODULES} \
                 parse_bytes_per_module={PARSE_COMMENT_BYTES} parse_cold_us={parse_cold:?} \
                 parse_cold_median_us={} parse_warm_us={parse_warm:?} \
                 parse_warm_median_us={} warning_functions={WARNING_FUNCTIONS} \
                 warning_callees_per_body={WARNING_CALLEES_PER_BODY} \
                 warning_cold_us={warning_cold:?} warning_cold_median_us={} \
                 warning_warm_us={warning_warm:?} warning_warm_median_us={}",
            parse_cold[SAMPLES / 2],
            parse_warm[SAMPLES / 2],
            warning_cold[SAMPLES / 2],
            warning_warm[SAMPLES / 2],
        );
    }
}

#[test]
fn warning_reference_frontier_inflight_cancellation_publishes_no_aggregate() {
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct GatedChildKey(usize);
    impl QueryKey for GatedChildKey {
        fn stable_identity(&self) -> String {
            format!("gated-warning-child-{}", self.0)
        }
        fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
            self.0.hash(hasher);
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct GatedBatchKey(Arc<[GatedChildKey]>);
    impl QueryKey for GatedBatchKey {
        fn stable_identity(&self) -> String {
            format!("gated-warning-frontier-{}", self.0.len())
        }
        fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
            self.0.hash(hasher);
        }
    }

    #[derive(Debug, Clone)]
    struct GatedBatchValue {
        values: Arc<[usize]>,
        _retained_children: Arc<rue_query::RetainedPinSet>,
    }
    impl RetainedCharge for GatedBatchValue {
        fn retained_charge(&self) -> u64 {
            self.values.retained_charge()
        }
    }

    #[derive(Default)]
    struct EvaluatorGate {
        state: Mutex<(usize, bool)>,
        changed: std::sync::Condvar,
    }

    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }")], 1);
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
    let revision = revision_for(&mut database, &snapshot);
    let gate = Arc::new(EvaluatorGate::default());
    let child_gate = gate.clone();
    let children = database
        .runtime
        .family_with_evaluator(
            "test.warning-body-reference-gated-child",
            8,
            move |context, _, key: &GatedChildKey| {
                let mut state = child_gate
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.0 += 1;
                child_gate.changed.notify_all();
                while !state.1 {
                    context.check_canceled()?;
                    state = child_gate
                        .changed
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    let children_for_batch = children.clone();
    let batches = database
        .runtime
        .family_with_equality_and_evaluator(
            "test.warning-body-reference-gated-frontier",
            1,
            |left: &GatedBatchValue, right: &GatedBatchValue| left.values == right.values,
            move |context, _, key: &GatedBatchKey| {
                let _validated_registered = context
                    .endorse_registered_validations_from(&[])
                    .expect("gated warning frontier uses this runtime");
                let _attempts = context
                    .retain_nested_attempts_for(&["test.warning-body-reference-gated-child"]);
                let terminals = context
                    .query_registered_adaptive_batch_refs(&children_for_batch, key.0.iter())?;
                let values = terminals
                    .iter()
                    .map(|terminal| {
                        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                            unreachable!("gated children publish successes")
                        };
                        *value
                    })
                    .collect::<Vec<_>>();
                let retained_children = Arc::new(
                    context
                        .retain_observed_terminal_cones_from(&terminals, &[])
                        .expect("gated warning frontier observes every child"),
                );
                Ok(QueryOutput::success(GatedBatchValue {
                    values: values.into(),
                    _retained_children: retained_children,
                }))
            },
        )
        .unwrap();
    let keys: Arc<[GatedChildKey]> = (0..32).map(GatedChildKey).collect::<Vec<_>>().into();
    let database = Arc::new(database);
    let aggregate_key = GatedBatchKey(keys);
    let cancellation = CancellationToken::new();
    let request_database = database.clone();
    let request_batches = batches.clone();
    let request_key = aggregate_key.clone();
    let request_cancellation = cancellation.clone();
    let request = std::thread::spawn(move || {
        request_database.runtime.request_registered(
            &request_batches,
            revision,
            request_key,
            request_cancellation,
        )
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut state = gate
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while state.0 < 4 {
        let now = std::time::Instant::now();
        assert!(
            now < deadline,
            "only {} of four warning children reached the evaluator rendezvous",
            state.0
        );
        let (next, timed_out) = gate
            .changed
            .wait_timeout(state, deadline.saturating_duration_since(now))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = next;
        assert!(
            !timed_out.timed_out() || state.0 >= 4,
            "only {} of four warning children reached the evaluator rendezvous",
            state.0
        );
    }
    drop(state);
    let scheduling = database.runtime_metrics_for_test();
    assert_eq!(scheduling.batch_worker_slots_requested, 31);
    assert_eq!(scheduling.batch_worker_slots_granted, 3);
    assert_eq!(scheduling.batch_worker_lanes_entered, 4);
    assert_eq!(
        scheduling.peak_query_workers, 4,
        "four gated child evaluators guarantee exact worker overlap"
    );
    cancellation.cancel();
    gate.changed.notify_all();
    let canceled = request
        .join()
        .expect("warning frontier request thread joins");
    assert_eq!(canceled.abort(), Some(&QueryAbort::Canceled));
    assert!(canceled.terminal().is_none());
    assert!(
        canceled.nested_attempts().iter().any(|attempt| {
            attempt.node().family() == "test.warning-body-reference-gated-child"
        })
    );
    assert!(!batches.contains_retained_key(&aggregate_key));

    let mut state = gate
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.1 = true;
    gate.changed.notify_all();
    drop(state);
    let retry = database.runtime.request_registered(
        &batches,
        revision,
        aggregate_key.clone(),
        CancellationToken::new(),
    );
    assert!(retry.terminal().is_some());
    assert!(batches.contains_retained_key(&aggregate_key));
}

#[test]
fn module_index_projection_requests_and_reuses_lookup_name_terminals() {
    let source = source_snapshot(
        &[(1, "/main.rue", "main.rue", "fn alpha() {} fn beta() {}")],
        1,
    );
    let main = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let (program, _) = database.parse_program(revision, &main, [main.clone()]);
    let program = program.unwrap();
    let after_parse = database.runtime.metrics().claims;
    let first = database
        .projected_module_indexes(revision, &program)
        .unwrap();
    assert_eq!(first[0].definitions.len(), 2);
    assert_eq!(
        database.runtime.metrics().claims - after_parse,
        3,
        "one ModuleIndex plus two production LookupName terminals"
    );
    let after_first_projection = database.runtime.metrics().claims;
    let second = database
        .projected_module_indexes(revision, &program)
        .unwrap();
    assert_eq!(first[0].definitions, second[0].definitions);
    assert_eq!(database.runtime.metrics().claims, after_first_projection);
}

#[test]
fn module_index_exact_name_partition_ignores_irrelevant_definitions() {
    let mut source = String::from("fn target() {}\n");
    for index in 0..128 {
        source.push_str(&format!("fn irrelevant_{index}() {{}}\n"));
    }
    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", source.as_str())], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let (_, index) = database.module_terminals(revision, module.clone());

    assert_eq!(index.definitions.len(), 129);
    assert_eq!(
        index.definition_indices(DefinitionNamespace::ModuleItem, "target"),
        &[0],
        "the exact-key partition visits only the matching candidate"
    );
    assert_eq!(
        index
            .definitions_for(DefinitionNamespace::ModuleItem, "target")
            .count(),
        1
    );
    assert_eq!(
        index
            .definitions_for(DefinitionNamespace::ModuleItem, "absent")
            .count(),
        0
    );

    let lookup = request_lookup_name(
        &database,
        revision,
        &module,
        DefinitionNamespace::ModuleItem,
        "target",
    );
    assert!(matches!(
        canonical_of(&lookup),
        CanonicalNameResolution::Unique(fact) if fact.name.as_ref() == "target"
    ));
}

#[test]
fn module_index_exact_import_partition_ignores_irrelevant_directives() {
    let mut source = String::from("const target = @import(\"./target.rue\");\n");
    for index in 0..128 {
        source.push_str(&format!(
            "const irrelevant_{index} = @import(\"irrelevant_{index}.rue\");\n"
        ));
    }
    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", source.as_str())], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let (_, index) = database.module_terminals(revision, module);

    assert_eq!(index.imports.len(), 129);
    assert_eq!(
        index.import_partitions.get("target.rue"),
        Some(&0),
        "the exact-key partition recovers one source locator regardless of unrelated imports"
    );
    let (normalized, directive) = index.normalized_import("target.rue");
    assert_eq!(normalized, "target.rue");
    assert_eq!(
        directive.map(crate::ImportDirective::specifier),
        Some("./target.rue")
    );
    let target = directive.expect("target import retains its exact source locator");
    let occurrence = crate::ImportOccurrenceKey::from_directive(target);
    assert_eq!(
        index
            .import_occurrence(&occurrence)
            .map(crate::ImportDirective::specifier),
        Some("./target.rue")
    );
    let stale = crate::ImportOccurrenceKey::from_directive(&crate::ImportDirective::new(
        target.importer().clone(),
        target.source_offset().saturating_add(1),
        target.source_end(),
        Arc::from(target.specifier()),
    ));
    assert!(index.import_occurrence(&stale).is_none());
    let (_, absent) = index.normalized_import("absent.rue");
    assert!(absent.is_none());

    let compiler = include_str!("../parse_import.rs");
    let method_start = compiler
        .find("fn import_occurrence(")
        .expect("exact occurrence lookup remains explicit");
    let method_end = compiler[method_start..]
        .find("\n    }\n}\n\npub(super) fn new_module_index")
        .map(|offset| method_start + offset)
        .expect("exact occurrence lookup remains on ModuleIndex");
    let method = &compiler[method_start..method_end];
    assert!(method.contains(".binary_search_by("));
    assert!(!method.contains(".iter().find("));

    let runtime = super::REVISIONED_DATABASE_SOURCE;
    let evaluator_start = runtime
        .find("\"compiler.resolve-import\"")
        .expect("ResolveImport evaluator remains registered");
    let evaluator_end = runtime[evaluator_start..]
        .find(".expect(\"the ResolveImport family has one canonical name\")")
        .map(|offset| evaluator_start + offset)
        .expect("ResolveImport evaluator remains bounded");
    let evaluator = &runtime[evaluator_start..evaluator_end];
    assert!(evaluator.contains("index.import_occurrence(&key.occurrence)"));
    assert!(!evaluator.contains("module.imports().iter().find("));
}

#[test]
fn compiler_body_anonymous_registry_is_canonical_and_indexed() {
    let compiler = include_str!("../provider.rs");
    let registry_start = compiler
        .find("struct CanonicalAnonymousNominalRegistry")
        .expect("body provider retains one anonymous registry");
    let registry_end = compiler[registry_start..]
        .find("\n#[derive(Clone)]\npub(crate) struct CompilerBodyDurableSource")
        .map(|offset| registry_start + offset)
        .expect("anonymous registry remains separate from its consumer");
    let registry = &compiler[registry_start..registry_end];
    assert!(registry.contains("with_canonical_identity()"));
    assert!(registry.contains("self.by_identity.get(identity.as_ref()).cloned()"));

    let lookup_start = compiler
        .find("fn try_anonymous_nominal(")
        .expect("body provider retains anonymous lookup");
    let lookup_end = compiler[lookup_start..]
        .find("\n    pub(super) fn signature(")
        .map(|offset| lookup_start + offset)
        .expect("anonymous lookup stays bounded");
    let lookup = &compiler[lookup_start..lookup_end];
    assert!(lookup.contains("dynamic.get(key)"));
    assert!(lookup.contains("try_anonymous_nominal(key)"));
    assert!(lookup.contains("producer_transport_failure"));
    assert!(!lookup.contains(".or(cached)"));
    assert!(!lookup.contains(".values()"));
    assert!(!lookup.contains(".iter().find("));
}

#[test]
fn compiler_body_anonymous_registry_unifies_producers_and_keeps_rich_facts() {
    let canonical = anonymous_identity_for_digest_test(
        "RegistryProducer",
        rue_air::AnonymousNominalKind::Struct,
    );
    let crate::StableProducerId::Function(base) = canonical.producer.clone() else {
        unreachable!("digest-test helper uses a function producer")
    };
    let mut wrapped = canonical.clone();
    wrapped.producer =
        crate::StableProducerId::Function(Node::new(crate::FunctionInstanceKey::Specialization {
            base,
            arguments: crate::CanonicalArguments::default(),
        }));
    let thin = crate::durable_semantics::DurableAnonymousNominal::new(
        wrapped.clone(),
        crate::durable_semantics::DurableAnonymousNominalShape::Struct {
            fields: Arc::from([]),
            methods: Arc::from([]),
        },
        Arc::from([]),
        Arc::from([]),
    );
    let rich = crate::durable_semantics::DurableAnonymousNominal::new(
        canonical.clone(),
        crate::durable_semantics::DurableAnonymousNominalShape::Struct {
            fields: Arc::from([]),
            methods: Arc::from([crate::durable_semantics::DurableAnonymousMethodSignature {
                name: Arc::from("method"),
                has_self: false,
                self_mode: crate::durable_semantics::DurableParameterMode::Value,
                returns_borrow: false,
                returns_inout: false,
                parameters: Arc::from([]),
                result: crate::durable_semantics::DurableAnonymousMethodType::Concrete(
                    rue_air::SemanticImportType::I32,
                ),
                has_body: false,
            }]),
        },
        Arc::from([(Arc::from("T"), rue_air::SemanticImportType::I32)]),
        Arc::from([]),
    );

    let mut registry = CanonicalAnonymousNominalRegistry::default();
    registry.extend([&thin]);
    let canonical_thin = registry.get(&canonical).unwrap().unwrap();
    let wrapped_thin = registry.get(&wrapped).unwrap().unwrap();
    assert_eq!(canonical_thin.as_ref(), &thin.with_canonical_identity());
    assert!(Rc::ptr_eq(&canonical_thin, &wrapped_thin));
    registry.extend([&rich]);
    registry.extend([&thin]);

    assert_eq!(registry.by_identity.len(), 1);
    let canonical_rich = registry.get(&canonical).unwrap().unwrap();
    let wrapped_rich = registry.get(&wrapped).unwrap().unwrap();
    assert_eq!(canonical_rich.as_ref(), &rich.with_canonical_identity());
    assert!(Rc::ptr_eq(&canonical_rich, &wrapped_rich));

    let counterfeit = rich.with_shape(
        crate::durable_semantics::DurableAnonymousNominalShape::Struct {
            fields: Arc::from([(Arc::from("counterfeit"), rue_air::SemanticImportType::I64)]),
            methods: Arc::from([]),
        },
    );
    registry.extend([&counterfeit]);
    assert_eq!(registry.by_identity.len(), 0);
    assert_eq!(registry.get(&canonical), Err(canonical.clone()));
    assert_eq!(registry.get(&wrapped), Err(canonical.clone()));

    let conflicting_captures = crate::durable_semantics::DurableAnonymousNominal::new(
        canonical.clone(),
        rich.shape.clone(),
        Arc::from([(Arc::from("T"), rue_air::SemanticImportType::I64)]),
        Arc::from([]),
    );
    let mut capture_registry = CanonicalAnonymousNominalRegistry::default();
    capture_registry.extend([&rich]);
    capture_registry.extend([&conflicting_captures]);
    assert_eq!(capture_registry.by_identity.len(), 0);
    assert_eq!(capture_registry.get(&canonical), Err(canonical));
}

#[test]
fn anonymous_dependency_frontier_canonicalizes_aliases_before_deduplication() {
    let canonical = anonymous_identity_for_digest_test(
        "DependencyProducer",
        rue_air::AnonymousNominalKind::Struct,
    );
    let crate::StableProducerId::Function(base) = canonical.producer.clone() else {
        unreachable!("digest-test helper uses a function producer")
    };
    let mut wrapped = canonical.clone();
    wrapped.producer =
        crate::StableProducerId::Function(Node::new(crate::FunctionInstanceKey::Specialization {
            base,
            arguments: crate::CanonicalArguments::default(),
        }));
    let fact = crate::durable_semantics::DurableAnonymousNominal::new(
        canonical.clone(),
        crate::durable_semantics::DurableAnonymousNominalShape::Struct {
            fields: Arc::from([]),
            methods: Arc::from([]),
        },
        Arc::from([]),
        Arc::from([]),
    );
    let selected = BTreeMap::from([(canonical.clone(), fact)]);
    let mut pending = BTreeSet::new();
    enqueue_unselected_anonymous_dependencies(
        &selected,
        &mut pending,
        [wrapped.clone(), canonical.clone()],
    );
    assert!(
        pending.is_empty(),
        "an alias of a selected identity must not re-enter the frontier"
    );

    enqueue_unselected_anonymous_dependencies(
        &BTreeMap::new(),
        &mut pending,
        [wrapped, canonical.clone()],
    );
    assert_eq!(pending, BTreeSet::from([canonical]));
}

#[test]
fn provider_produced_anonymous_projection_rejects_conflicting_duplicate_identity() {
    let token = rue_air::SemanticDefinitionToken::new(4, 2);
    let base = rue_air::FunctionInstanceKey::Definition(token);
    let direct = rue_air::AnonymousNominalKey {
        kind: rue_air::AnonymousNominalKind::Struct,
        producer: rue_air::StableProducerId::Function(rue_air::Node::new(base.clone())),
        anchor: rue_rir::RirStructuralAnchor::new(vec![
            rue_rir::RirStructuralPathSegment::Body,
            rue_rir::RirStructuralPathSegment::AnonymousType(0),
        ]),
    };
    let mut alias = direct.clone();
    alias.producer = rue_air::StableProducerId::Function(rue_air::Node::new(
        rue_air::FunctionInstanceKey::Specialization {
            base: rue_air::Node::new(base),
            arguments: rue_air::CanonicalArguments::default(),
        },
    ));
    let produced = |identity, field, ty| rue_air::SemanticProducedAnonymousNominal {
        identity,
        shape: rue_air::SemanticProducedAnonymousNominalShape::Struct {
            fields: Arc::from([(Arc::from(field), ty)]),
            methods: Arc::from([]),
        },
        type_captures: Arc::from([]),
        value_captures: Arc::from([]),
    };
    let definitions = AHashMap::from([(
        token,
        crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_validated_canonical("main.rue"),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "producer",
            None,
        ),
    )]);
    let result = project_provider_produced_anonymous_nominals(
        &[
            produced(direct, "a", rue_air::TypeInstanceKey::I32),
            produced(alias, "b", rue_air::TypeInstanceKey::I64),
        ],
        &definitions,
        &AHashMap::new(),
    );
    assert_eq!(
        result,
        Err(rue_air::SemanticStableResolutionFailure::Ambiguous)
    );
}

#[test]
fn provider_produced_anonymous_projection_rejects_relocated_thin_rich_duplicate() {
    let first_token = rue_air::SemanticDefinitionToken::new(4, 2);
    let second_token = rue_air::SemanticDefinitionToken::new(5, 2);
    let identity = |token| rue_air::AnonymousNominalKey {
        kind: rue_air::AnonymousNominalKind::Struct,
        producer: rue_air::StableProducerId::Function(rue_air::Node::new(
            rue_air::FunctionInstanceKey::Definition(token),
        )),
        anchor: rue_rir::RirStructuralAnchor::new(vec![
            rue_rir::RirStructuralPathSegment::Body,
            rue_rir::RirStructuralPathSegment::AnonymousType(0),
        ]),
    };
    let produced = |token, type_captures| rue_air::SemanticProducedAnonymousNominal {
        identity: identity(token),
        shape: rue_air::SemanticProducedAnonymousNominalShape::Struct {
            fields: Arc::from([(Arc::from("value"), rue_air::TypeInstanceKey::I32)]),
            methods: Arc::from([]),
        },
        type_captures,
        value_captures: Arc::from([]),
    };
    let thin = produced(first_token, Arc::from([]));
    let rich = produced(
        second_token,
        Arc::from([(Arc::from("T"), rue_air::TypeInstanceKey::I32)]),
    );
    let stable_producer = crate::StableDefinitionKey::from_stable_parts(
        ModuleId::from_validated_canonical("main.rue"),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "producer",
        None,
    );
    let definitions = AHashMap::from([
        (first_token, stable_producer.clone()),
        (second_token, stable_producer),
    ]);

    for produced in [[thin.clone(), rich.clone()], [rich.clone(), thin.clone()]] {
        assert_eq!(
            project_provider_produced_anonymous_nominals(&produced, &definitions, &AHashMap::new(),),
            Err(rue_air::SemanticStableResolutionFailure::Ambiguous),
            "complete provider publications must conflict after token relocation in either order"
        );
    }
}

#[test]
fn lookup_name_retains_position_free_facts_across_trivia_shifts() {
    let first = source_snapshot(
        &[(1, "/main.rue", "main.rue", "pub struct Base { value: i32 }")],
        1,
    );
    let shifted = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "// leading trivia moves every current locator\npub struct Base { value: i32 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = LookupNameKey {
        module: module.clone(),
        namespace: DefinitionNamespace::ModuleItem,
        name: Arc::from("Base"),
    };
    let mut database = RevisionedQueryDatabase::default();
    let first_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&first), &first);
    let first_lookup = database.runtime.request_registered(
        &database.lookup_names,
        first_revision,
        key.clone(),
        CancellationToken::new(),
    );
    let first_stamp = first_lookup.terminal().unwrap().stamp();
    let (first_program, _) =
        database.parse_program(first_revision, &module, std::iter::once(module.clone()));
    let first_locator = database
        .projected_module_indexes(first_revision, &first_program.unwrap())
        .unwrap()[0]
        .definitions[0]
        .name_span;

    let shifted_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&shifted), &shifted);
    let shifted_lookup = database.runtime.request_registered(
        &database.lookup_names,
        shifted_revision,
        key,
        CancellationToken::new(),
    );
    assert_eq!(
        shifted_lookup.terminal().unwrap().stamp(),
        first_stamp,
        "trivia-only locator changes must not invalidate the retained name fact"
    );
    let (shifted_program, _) =
        database.parse_program(shifted_revision, &module, std::iter::once(module.clone()));
    let shifted_locator = database
        .projected_module_indexes(shifted_revision, &shifted_program.unwrap())
        .unwrap()[0]
        .definitions[0]
        .name_span;
    assert!(shifted_locator.start > first_locator.start);
}

fn import_fixture(
    epoch: u64,
    source: &str,
) -> (
    CompilerSession,
    DiscoverySourceAssembler,
    ImportDiscoveryContext,
) {
    let context =
        ImportDiscoveryContext::new(epoch, "/project", Some("/sdk"), "test-policy").unwrap();
    let assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/project/main.rue",
        "/physical/main.rue",
        PhysicalFileIdentity::new(1, 1),
        FileMetadataFingerprint::new(1, 2, 3),
        Arc::new(source.to_owned()),
    )
    .unwrap();
    (CompilerSession::new(), assembler, context)
}

fn begin_and_plan(
    session: &mut CompilerSession,
    assembler: &mut DiscoverySourceAssembler,
    context: ImportDiscoveryContext,
) -> (ImportInputRevision, ImportDiscoveryPlan) {
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let revision = session
        .begin_import_input_request(&snapshot, context.clone(), reads.clone())
        .unwrap();
    let plan = session
        .stage_import_discovery(
            &snapshot,
            context,
            reads.shared_slice(),
            ImportObservationLedger::default(),
        )
        .unwrap();
    (revision, plan)
}

#[test]
fn import_frontier_rejects_roots_outside_the_pinned_plan() {
    let (_, mut assembler, context) =
        import_fixture(313, "const selected = @import(\"dep\"); fn main() {}");
    let mut database = RevisionedQueryDatabase::default();
    let (_, _, revision, plan) = begin_database_plan(&mut database, &mut assembler, context);

    // An occurrence from an unrelated program is definitionally outside the
    // pinned plan: its specifier and spans exist in no plan group.
    let (_, mut foreign_assembler, foreign_context) =
        import_fixture(314, "const other = @import(\"elsewhere\"); fn main() {}");
    let mut foreign_database = RevisionedQueryDatabase::default();
    let (_, _, _, foreign_plan) = begin_database_plan(
        &mut foreign_database,
        &mut foreign_assembler,
        foreign_context,
    );
    let foreign_occurrence = foreign_plan.demand_roots().occurrences()[0].clone();

    let roots = ImportDemandRoots::new(
        plan.demand_roots()
            .occurrences()
            .iter()
            .cloned()
            .chain([foreign_occurrence]),
    );
    assert!(
        database
            .import_frontier(revision, &plan, ImportDemandMode::Rooted, &roots)
            .unwrap_err()
            .to_string()
            .contains("outside the pinned plan"),
        "a demand root absent from the pinned plan must be refused"
    );

    // The exact plan-derived roots remain accepted after the refusal.
    database
        .import_frontier(
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .unwrap();
}

#[test]
fn ordinary_and_rooted_publication_share_one_compatibility_namespace() {
    let context =
        ImportDiscoveryContext::new(401, "/project", Some("/sdk"), "test-policy").unwrap();
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/project/main.rue",
        "/physical/main.rue",
        PhysicalFileIdentity::new(1, 1),
        FileMetadataFingerprint::new(1, 2, 3),
        Arc::new("fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }".to_owned()),
    )
    .unwrap();
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let closure_key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "main")]),
        configuration: semantic_configuration(),
    };
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);

    // Ordinary staged update: establish and pin both reached bodies.
    let ordinary = revision_for(&mut database, &snapshot);
    let cold = database
        .body_closure(ordinary, closure_key.clone(), CancellationToken::new())
        .unwrap();
    assert_eq!(cold.body_executions.len(), 2);
    let modules = snapshot
        .source_revision()
        .modules()
        .iter()
        .map(|module| module.module.clone())
        .collect::<Vec<_>>();
    let (program, _) = database.parse_program(ordinary, &module, modules);
    let plan = ImportDiscoveryPlan::new(&program.unwrap(), context.clone()).unwrap();

    // Rooted request: bind the existing ordinary lineage to this exact
    // observation context and validate both body terminals.
    let rooted_input = database
        .begin_import_inputs(&snapshot, context, reads)
        .unwrap();
    let rooted = Revision::new(rooted_input.revision_id, rooted_input.compatibility_token);
    assert!(ordinary.is_compatible_with(rooted));
    database
        .import_frontier(
            rooted_input,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .unwrap();
    let rooted_close = database
        .body_closure(rooted, closure_key.clone(), CancellationToken::new())
        .unwrap();
    assert!(
        rooted_close
            .body_executions
            .values()
            .all(|execution| *execution == RequestExecution::Reused),
        "rooted publication must reuse the ordinary-update body terminals"
    );

    // A subsequent ordinary staged update stays in the bound namespace,
    // rather than returning to the old constant-token namespace.
    let ordinary_again = revision_for(&mut database, &snapshot);
    assert!(rooted.is_compatible_with(ordinary_again));
    let ordinary_close = database
        .body_closure(ordinary_again, closure_key, CancellationToken::new())
        .unwrap();
    assert!(
        ordinary_close
            .body_executions
            .values()
            .all(|execution| *execution == RequestExecution::Reused),
        "ordinary publication after a rooted request must retain body reuse"
    );
}

fn publish_remapped_observations(
    database: &mut RevisionedQueryDatabase,
    snapshot: &SourceSnapshot,
    reads: AcceptedReadManifest,
    plan: &ImportDiscoveryPlan,
    mut revision: ImportInputRevision,
    remaps: &[(&str, PhysicalFileIdentity)],
) -> ImportInputRevision {
    let roots = ImportDemandRoots::whole_plan(plan);
    loop {
        let frontier = database
            .import_frontier(revision, plan, ImportDemandMode::Rooted, &roots)
            .unwrap();
        if frontier.requests().is_empty() {
            return revision;
        }
        let observations = frontier
            .requests()
            .iter()
            .cloned()
            .map(|request| {
                let Some((_, identity)) = remaps
                    .iter()
                    .find(|(path, _)| *path == request.requested_path())
                else {
                    return ImportObservation::absent(request);
                };
                let entry = reads
                    .iter()
                    .find(|entry| entry.metadata_identity() == *identity)
                    .unwrap();
                let file_id = snapshot
                    .files()
                    .find(|source| snapshot.module_id(source.file_id) == Some(entry.module()))
                    .unwrap()
                    .file_id;
                let accepted = crate::AcceptedImportSource::new(
                    request.requested_path(),
                    entry.canonical_path(),
                    entry.metadata_identity(),
                    entry.metadata_fingerprint(),
                    snapshot.shared_source_text(file_id).unwrap(),
                )
                .unwrap();
                ImportObservation::accepted(request, accepted).unwrap()
            })
            .collect();
        revision = database
            .publish_import_batch(&frontier, snapshot, reads.clone(), observations)
            .unwrap();
    }
}

#[test]
fn declaration_imports_are_exact_lazy_and_distinguish_duplicate_specifiers() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source = "const selected = if true { @import(\"same\") } else { @import(\"same\") }; const untouched = @import(\"other\"); fn main() {}";
    let (_, mut assembler, context) = import_fixture(301, source);
    let mut database = RevisionedQueryDatabase::default();
    let (snapshot, reads, revision, plan) =
        begin_database_plan(&mut database, &mut assembler, context);
    let revision = publish_manifest_observations(&mut database, &snapshot, reads, &plan, revision);
    let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let parsed = database.runtime.request_registered(
        &database.parse_modules,
        runtime_revision,
        ModuleQueryKey(module.clone()),
        CancellationToken::new(),
    );
    let parsed_module = match parsed.terminal().unwrap().outcome() {
        rue_query::QueryOutcome::Success(value) => value.result.clone().unwrap(),
        rue_query::QueryOutcome::Failure(_) => unreachable!(),
    };
    assert_eq!(
        parsed_module.declaration_import_locator_materialization_count(),
        0,
        "indexing and import discovery must retain only fixed parser locators"
    );

    let first_key = declaration_import_key(
        &module,
        Category::ConstCandidate,
        "selected",
        None,
        0,
        "same",
    );
    let second_key = declaration_import_key(
        &module,
        Category::ConstCandidate,
        "selected",
        None,
        1,
        "same",
    );
    assert_ne!(first_key.stable_identity(), second_key.stable_identity());
    for key in [first_key.clone(), second_key] {
        let requested = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            key,
            CancellationToken::new(),
        );
        assert_eq!(execution(&requested), RequestExecution::Computed);
        assert_eq!(
            requested
                .dependencies()
                .iter()
                .map(|dependency| dependency.node.family())
                .collect::<Vec<_>>(),
            vec![
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.parse-module",
                "compiler.resolve-import",
            ]
        );
        let terminal = requested.terminal().unwrap();
        assert_eq!(terminal.kind(), QueryTerminalKind::Success);
        assert!(matches!(
            terminal.outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                crate::CanonicalImportResolution::Missing
            ))
        ));
    }
    assert_eq!(
        parsed_module.declaration_import_locator_materialization_count(),
        2,
        "only the two demanded sites in selected may materialize"
    );
    let warm = database.runtime.request_registered(
        &database.declaration_imports,
        runtime_revision,
        first_key,
        CancellationToken::new(),
    );
    assert_eq!(execution(&warm), RequestExecution::Reused);
    assert_eq!(
        parsed_module.declaration_import_locator_materialization_count(),
        2
    );
    let out_of_range = database.runtime.request_registered(
        &database.declaration_imports,
        runtime_revision,
        declaration_import_key(
            &module,
            Category::ConstCandidate,
            "selected",
            None,
            2,
            "same",
        ),
        CancellationToken::new(),
    );
    assert!(matches!(
        out_of_range.terminal().unwrap().outcome(),
        rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
            crate::declaration_candidate::DeclarationImportFailure::SiteOutOfRange {
                available: 2,
                ..
            }
        ))
    ));
    let wrong_specifier = database.runtime.request_registered(
        &database.declaration_imports,
        runtime_revision,
        declaration_import_key(
            &module,
            Category::ConstCandidate,
            "selected",
            None,
            0,
            "different",
        ),
        CancellationToken::new(),
    );
    assert!(matches!(
        wrong_specifier.terminal().unwrap().outcome(),
        rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
            crate::declaration_candidate::DeclarationImportFailure::SpecifierMismatch {
                actual,
                ..
            }
        )) if actual.as_ref() == "same"
    ));
}

#[test]
fn declaration_import_relocation_reuses_and_stale_absolute_site_fails_typed() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let first_source = "const selected = @import(\"missing\"); fn main() {}";
    let (_, mut first_assembler, first_context) = import_fixture(302, first_source);
    let mut database = RevisionedQueryDatabase::default();
    let (first_snapshot, first_reads, first_revision, first_plan) =
        begin_database_plan(&mut database, &mut first_assembler, first_context);
    let old_occurrence = first_plan.groups()[0][0].occurrence().clone();
    assert_ne!(
        ResolveImportKey {
            occurrence: old_occurrence.clone(),
            mode: ImportDemandMode::Rooted,
        }
        .stable_identity(),
        ResolveImportKey {
            occurrence: old_occurrence.clone(),
            mode: ImportDemandMode::Speculative,
        }
        .stable_identity(),
        "resolve-import stable identities must include demand mode"
    );
    let first_revision = publish_manifest_observations(
        &mut database,
        &first_snapshot,
        first_reads,
        &first_plan,
        first_revision,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = declaration_import_key(
        &module,
        Category::ConstCandidate,
        "selected",
        None,
        0,
        "missing",
    );
    let first = database.runtime.request_registered(
        &database.declaration_imports,
        Revision::new(
            first_revision.revision_id,
            first_revision.compatibility_token,
        ),
        key.clone(),
        CancellationToken::new(),
    );
    let first_stamp = first.terminal().unwrap().stamp();

    let shifted_source =
        "// position-only relocation\n\nconst selected = @import(\"missing\"); fn main() {}";
    let (_, mut shifted_assembler, shifted_context) = import_fixture(303, shifted_source);
    let (shifted_snapshot, shifted_reads, shifted_revision, shifted_plan) =
        begin_database_plan(&mut database, &mut shifted_assembler, shifted_context);
    let shifted_revision = publish_manifest_observations(
        &mut database,
        &shifted_snapshot,
        shifted_reads,
        &shifted_plan,
        shifted_revision,
    );
    let shifted_runtime = Revision::new(
        shifted_revision.revision_id,
        shifted_revision.compatibility_token,
    );
    let relocated = database.runtime.request_registered(
        &database.declaration_imports,
        shifted_runtime,
        key,
        CancellationToken::new(),
    );
    assert_eq!(
        relocated.terminal().unwrap().stamp(),
        first_stamp,
        "position-free declaration import results must stay green across trivia relocation"
    );

    let stale = database.runtime.request_registered(
        &database.resolve_imports,
        shifted_runtime,
        ResolveImportKey {
            occurrence: old_occurrence,
            mode: ImportDemandMode::Rooted,
        },
        CancellationToken::new(),
    );
    assert!(matches!(
        stale.terminal().unwrap().outcome(),
        rue_query::QueryOutcome::Success(ResolveImportValue {
            site_found: false,
            ..
        })
    ));
}

#[test]
fn declaration_import_recovers_when_resolution_observations_arrive() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let (_, mut assembler, context) =
        import_fixture(306, "const selected = @import(\"missing\"); fn main() {}");
    let mut database = RevisionedQueryDatabase::default();
    let (snapshot, reads, revision, plan) =
        begin_database_plan(&mut database, &mut assembler, context);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = declaration_import_key(
        &module,
        Category::ConstCandidate,
        "selected",
        None,
        0,
        "missing",
    );
    let pending = database.runtime.request_registered(
        &database.declaration_imports,
        Revision::new(revision.revision_id, revision.compatibility_token),
        key.clone(),
        CancellationToken::new(),
    );
    assert!(matches!(
        pending.terminal().unwrap().outcome(),
        rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
            crate::declaration_candidate::DeclarationImportFailure::ResolutionUnavailable(_)
        ))
    ));

    let completed = publish_manifest_observations(&mut database, &snapshot, reads, &plan, revision);
    let recovered = database.runtime.request_registered(
        &database.declaration_imports,
        Revision::new(completed.revision_id, completed.compatibility_token),
        key,
        CancellationToken::new(),
    );
    assert_eq!(execution(&recovered), RequestExecution::Computed);
    assert!(matches!(
        recovered.terminal().unwrap().outcome(),
        rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
            crate::CanonicalImportResolution::Missing
        ))
    ));
}

#[test]
fn semantic_import_is_typed_missing_input_and_recovers_on_successor_revision() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let (_, mut assembler, context) =
        import_fixture(307, "const selected = @import(\"missing\"); fn main() {}");
    let mut database = RevisionedQueryDatabase::default();
    let (snapshot, reads, revision, plan) =
        begin_database_plan(&mut database, &mut assembler, context);
    let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let query = Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: declaration_candidate(
            &database,
            runtime_revision,
            &module,
            Category::ConstCandidate,
            "selected",
        ),
        configuration: semantic_configuration(),
    });

    let pending = database.runtime.request_registered(
        &database.semantic_nucleus,
        runtime_revision,
        query.clone(),
        CancellationToken::new(),
    );
    assert_eq!(execution(&pending), RequestExecution::Aborted);
    assert!(matches!(pending.abort(), Some(QueryAbort::MissingInput(_))));
    assert!(pending.terminal().is_none());

    let completed = publish_manifest_observations(&mut database, &snapshot, reads, &plan, revision);
    let recovered = database.runtime.request_registered(
        &database.semantic_nucleus,
        Revision::new(completed.revision_id, completed.compatibility_token),
        query,
        CancellationToken::new(),
    );
    assert_eq!(execution(&recovered), RequestExecution::Computed);
    assert!(matches!(
        recovered.terminal().unwrap().outcome(),
        rue_query::QueryOutcome::Success(Value::Failure(Failure::Resolution(message)))
            if message.as_ref() == "cannot find module `missing`"
    ));
}

#[test]
fn declaration_imports_preserve_canonical_resolution_and_category_boundaries() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source = "const selected = @import(\"dep\"); struct Box { value: i32, fn get(borrow self) { @import(\"method\"); } fn make() -> Box { @import(\"associated\"); Box { value: 0 } } } drop fn Box(self) { @import(\"drop\"); } fn free() { @import(\"free\"); } enum Choice { A } extern \"C\" { fn foreign() -> i32; }";
    let (_, mut assembler, context) = import_fixture(304, source);
    assembler
        .add_explicit(
            "/project/dep.rue",
            "/physical/dep-file.rue",
            PhysicalFileIdentity::new(2, 1),
            FileMetadataFingerprint::new(2, 2, 3),
            Arc::new("const value = 1;".to_owned()),
        )
        .unwrap();
    assembler
        .add_explicit(
            "/project/dep/_dep.rue",
            "/physical/dep-dir.rue",
            PhysicalFileIdentity::new(3, 1),
            FileMetadataFingerprint::new(3, 2, 3),
            Arc::new("const value = 2;".to_owned()),
        )
        .unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let (snapshot, reads, revision, plan) =
        begin_database_plan(&mut database, &mut assembler, context);
    let revision = publish_manifest_observations(&mut database, &snapshot, reads, &plan, revision);
    let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let selected = database.runtime.request_registered(
        &database.declaration_imports,
        runtime_revision,
        declaration_import_key(
            &module,
            Category::ConstCandidate,
            "selected",
            None,
            0,
            "dep",
        ),
        CancellationToken::new(),
    );
    let selected_terminal = selected.terminal().unwrap();
    // Policy v2: both module forms on disk are not ambiguous — the
    // extensionless specifier names the facade alone; the sibling
    // `dep.rue` is never probed.
    assert!(
        matches!(
            selected_terminal.outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                crate::CanonicalImportResolution::Resolved(module)
            )) if module.as_str() == "dep/_dep.rue"
        ),
        "unexpected declaration import outcome: {:#?}",
        selected_terminal.outcome()
    );

    let owner = crate::declaration_candidate::DeclarationCandidateOwner {
        category: Category::Struct,
        name: Arc::from("Box"),
    };
    for (category, name, owner, specifier) in [
        (Category::Method, "get", Some(owner.clone()), "method"),
        (
            Category::AssociatedFunction,
            "make",
            Some(owner.clone()),
            "associated",
        ),
        (Category::Destructor, "Box", Some(owner), "drop"),
        (Category::Function, "free", None, "free"),
    ] {
        let requested = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            declaration_import_key(&module, category, name, owner, 0, specifier),
            CancellationToken::new(),
        );
        assert!(matches!(
            requested.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                crate::CanonicalImportResolution::Missing
            ))
        ));
    }

    for (category, name) in [
        (Category::Struct, "Box"),
        (Category::Enum, "Choice"),
        (Category::ExternFunction, "foreign"),
    ] {
        let key = declaration_import_key(&module, category, name, None, 0, "none");
        let requested = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            key.clone(),
            CancellationToken::new(),
        );
        assert!(matches!(
            requested.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
                crate::declaration_candidate::DeclarationImportFailure::CategoryMismatch(
                    actual
                )
            )) if actual == &key.0
        ));
    }
}

#[test]
fn resolved_declaration_import_observes_only_winning_physical_provenance() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source = "const selected = @import(\"dep.rue\"); fn main() {}";
    let (_, mut first_assembler, first_context) = import_fixture(307, source);
    first_assembler
        .add_explicit(
            "/project/dep.rue",
            "/physical/dep.rue",
            PhysicalFileIdentity::new(2, 1),
            FileMetadataFingerprint::new(4, 5, 6),
            Arc::new("const value = 1;".to_owned()),
        )
        .unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let (first_snapshot, first_reads, first_revision, first_plan) =
        begin_database_plan(&mut database, &mut first_assembler, first_context);
    let first_revision = publish_manifest_observations(
        &mut database,
        &first_snapshot,
        first_reads,
        &first_plan,
        first_revision,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = declaration_import_key(
        &module,
        Category::ConstCandidate,
        "selected",
        None,
        0,
        "dep.rue",
    );
    let first = database.runtime.request_registered(
        &database.declaration_imports,
        Revision::new(
            first_revision.revision_id,
            first_revision.compatibility_token,
        ),
        key.clone(),
        CancellationToken::new(),
    );
    assert!(matches!(
        first.terminal().unwrap().outcome(),
        rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
            crate::CanonicalImportResolution::Resolved(target)
        )) if target.as_str() == "dep.rue"
    ));
    let first_stamp = first.terminal().unwrap().stamp();

    let (_, mut remapped_assembler, remapped_context) = import_fixture(307, source);
    remapped_assembler
        .add_explicit(
            "/project/other.rue",
            "/physical/other.rue",
            PhysicalFileIdentity::new(2, 1),
            FileMetadataFingerprint::new(4, 5, 6),
            Arc::new("const value = 1;".to_owned()),
        )
        .unwrap();
    let (remapped_snapshot, remapped_reads, remapped_revision, remapped_plan) =
        begin_database_plan(&mut database, &mut remapped_assembler, remapped_context);
    let remapped_revision = publish_remapped_observations(
        &mut database,
        &remapped_snapshot,
        remapped_reads,
        &remapped_plan,
        remapped_revision,
        &[("/project/dep.rue", PhysicalFileIdentity::new(2, 1))],
    );
    let remapped = database.runtime.request_registered(
        &database.declaration_imports,
        Revision::new(
            remapped_revision.revision_id,
            remapped_revision.compatibility_token,
        ),
        key.clone(),
        CancellationToken::new(),
    );
    let remapped_terminal = remapped.terminal().unwrap();
    assert_ne!(remapped_terminal.stamp(), first_stamp);
    assert!(matches!(
        remapped_terminal.outcome(),
        rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
            crate::CanonicalImportResolution::Resolved(target)
        )) if target.as_str() == "other.rue"
    ));
    let remapped_stamp = remapped_terminal.stamp();

    let mut green_assembler = remapped_assembler.clone();
    green_assembler
        .add_explicit(
            "/project/unrelated.rue",
            "/physical/unrelated.rue",
            PhysicalFileIdentity::new(9, 1),
            FileMetadataFingerprint::new(9, 2, 3),
            Arc::new("const unrelated = 9;".to_owned()),
        )
        .unwrap();
    let green_context =
        ImportDiscoveryContext::new(307, "/project", Some("/sdk"), "test-policy").unwrap();
    let (green_snapshot, green_reads, green_revision, green_plan) =
        begin_database_plan(&mut database, &mut green_assembler, green_context);
    let green_revision = publish_remapped_observations(
        &mut database,
        &green_snapshot,
        green_reads,
        &green_plan,
        green_revision,
        &[("/project/dep.rue", PhysicalFileIdentity::new(2, 1))],
    );
    let green = database.runtime.request_registered(
        &database.declaration_imports,
        Revision::new(
            green_revision.revision_id,
            green_revision.compatibility_token,
        ),
        key,
        CancellationToken::new(),
    );
    assert_eq!(green.terminal().unwrap().stamp(), remapped_stamp);
}

#[test]
fn facade_declaration_import_observes_its_provenance_leaf_only() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    // Policy v2: the extensionless specifier owns exactly one candidate,
    // the facade. The contract pinned here is provenance tracking at that
    // single leaf: a physical remap of the winner re-stamps the terminal,
    // while adding files the policy never probes — including the sibling
    // `dep.rue` file-module spelling — validates green.
    let source = "const selected = @import(\"dep\"); fn main() {}";
    let (_, mut first_assembler, first_context) = import_fixture(308, source);
    first_assembler
        .add_explicit(
            "/project/dep/_dep.rue",
            "/physical/dep-dir.rue",
            PhysicalFileIdentity::new(3, 1),
            FileMetadataFingerprint::new(2, 5, 6),
            Arc::new("const value = 2;".to_owned()),
        )
        .unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let (first_snapshot, first_reads, first_revision, first_plan) =
        begin_database_plan(&mut database, &mut first_assembler, first_context);
    let first_revision = publish_manifest_observations(
        &mut database,
        &first_snapshot,
        first_reads,
        &first_plan,
        first_revision,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = declaration_import_key(
        &module,
        Category::ConstCandidate,
        "selected",
        None,
        0,
        "dep",
    );
    let first = database.runtime.request_registered(
        &database.declaration_imports,
        Revision::new(
            first_revision.revision_id,
            first_revision.compatibility_token,
        ),
        key.clone(),
        CancellationToken::new(),
    );
    assert!(matches!(
        first.terminal().unwrap().outcome(),
        rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
            crate::CanonicalImportResolution::Resolved(module)
        )) if module.as_str() == "dep/_dep.rue"
    ));
    let first_stamp = first.terminal().unwrap().stamp();

    let (_, mut remapped_assembler, remapped_context) = import_fixture(308, source);
    remapped_assembler
        .add_explicit(
            "/project/facade.rue",
            "/physical/facade.rue",
            PhysicalFileIdentity::new(3, 1),
            FileMetadataFingerprint::new(2, 5, 6),
            Arc::new("const value = 2;".to_owned()),
        )
        .unwrap();
    let (remapped_snapshot, remapped_reads, remapped_revision, remapped_plan) =
        begin_database_plan(&mut database, &mut remapped_assembler, remapped_context);
    let remaps = [("/project/dep/_dep.rue", PhysicalFileIdentity::new(3, 1))];
    let remapped_revision = publish_remapped_observations(
        &mut database,
        &remapped_snapshot,
        remapped_reads,
        &remapped_plan,
        remapped_revision,
        &remaps,
    );
    let remapped = database.runtime.request_registered(
        &database.declaration_imports,
        Revision::new(
            remapped_revision.revision_id,
            remapped_revision.compatibility_token,
        ),
        key.clone(),
        CancellationToken::new(),
    );
    let remapped_terminal = remapped.terminal().unwrap();
    assert_ne!(remapped_terminal.stamp(), first_stamp);
    assert!(matches!(
        remapped_terminal.outcome(),
        rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
            crate::CanonicalImportResolution::Resolved(module)
        )) if module.as_str() == "facade.rue"
    ));
    let remapped_stamp = remapped_terminal.stamp();

    let mut green_assembler = remapped_assembler.clone();
    green_assembler
        .add_explicit(
            "/project/dep.rue",
            "/physical/dep-file.rue",
            PhysicalFileIdentity::new(9, 1),
            FileMetadataFingerprint::new(9, 2, 3),
            Arc::new("const value = 1;".to_owned()),
        )
        .unwrap();
    let green_context =
        ImportDiscoveryContext::new(308, "/project", Some("/sdk"), "test-policy").unwrap();
    let (green_snapshot, green_reads, green_revision, green_plan) =
        begin_database_plan(&mut database, &mut green_assembler, green_context);
    let green_revision = publish_remapped_observations(
        &mut database,
        &green_snapshot,
        green_reads,
        &green_plan,
        green_revision,
        &remaps,
    );
    let green = database.runtime.request_registered(
        &database.declaration_imports,
        Revision::new(
            green_revision.revision_id,
            green_revision.compatibility_token,
        ),
        key,
        CancellationToken::new(),
    );
    assert_eq!(green.terminal().unwrap().stamp(), remapped_stamp);
}

/// A hard-link alias with a lexicographically smaller canonical path that is
/// observed only AFTER the entry was carried by a produced snapshot/manifest
/// must not rewrite the retained representative: the published state is
/// immutable, so the assembler keeps reproducing exactly the snapshot,
/// manifest, and published view it already handed out. (Observed before the
/// first production, the smaller alias still wins — see the hard-link
/// order-independence tests.)
#[test]
fn alias_observed_after_publication_keeps_snapshot_manifest_and_view_in_agreement() {
    let (_, mut assembler, context) = import_fixture(311, "fn main() {}");
    assembler
        .add_explicit(
            "/project/a.rue",
            "/real/z-hard-a",
            PhysicalFileIdentity::new(9, 9),
            FileMetadataFingerprint::new(1, 2, 3),
            Arc::new("same inode".into()),
        )
        .unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let snapshot = assembler.snapshot().unwrap();
    let manifest = assembler.accepted_read_manifest();
    let revision = database
        .begin_import_inputs(&snapshot, context, manifest.clone())
        .unwrap();

    // The smaller alias of the SAME physical identity arrives only now.
    assembler
        .add_explicit(
            "/project/a.rue",
            "/real/a-hard-a",
            PhysicalFileIdentity::new(9, 9),
            FileMetadataFingerprint::new(1, 2, 3),
            Arc::new("same inode".into()),
        )
        .unwrap();
    let successor_snapshot = assembler.snapshot().unwrap();
    let successor_manifest = assembler.accepted_read_manifest();

    // The retained representative is frozen, so snapshot and manifest stay
    // in exact agreement with each other...
    assert_eq!(successor_snapshot.metadata(), snapshot.metadata());
    assert_eq!(
        successor_snapshot.source_revision(),
        snapshot.source_revision()
    );
    assert_eq!(successor_manifest, manifest);
    assert!(
        successor_manifest
            .iter()
            .any(|entry| entry.canonical_path() == "/real/z-hard-a")
    );

    // ...and with the published view, byte for byte, so a later successor
    // publication extends this state rather than rejecting it as mutated.
    let (current, view_snapshot, _, view_manifest, _, _) =
        database.current_import_view_state().unwrap();
    assert_eq!(current, revision);
    assert_eq!(
        view_snapshot.source_revision(),
        successor_snapshot.source_revision()
    );
    assert_eq!(view_manifest, successor_manifest);
}

#[test]
fn import_publication_rejects_duplicate_and_unmatched_physical_provenance() {
    let source = "const selected = @import(\"dep\"); fn main() {}";
    let (_, mut assembler, context) = import_fixture(309, source);
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let duplicated = reads
        .iter()
        .cloned()
        .chain(std::iter::once(reads.iter().next().unwrap().clone()))
        .collect::<Vec<_>>();
    let mut database = RevisionedQueryDatabase::default();
    assert!(
        database
            .begin_import_inputs(
                &snapshot,
                context.clone(),
                AcceptedReadManifest::from_entries(duplicated),
            )
            .is_err(),
        "duplicate physical provenance must fail before revision publication"
    );

    let (snapshot, reads, revision, plan) =
        begin_database_plan(&mut database, &mut assembler, context);
    let roots = ImportDemandRoots::whole_plan(&plan);
    let frontier = database
        .import_frontier(revision, &plan, ImportDemandMode::Rooted, &roots)
        .unwrap();
    let observations = frontier
        .requests()
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, request)| {
            if index == 0 {
                let accepted = crate::AcceptedImportSource::new(
                    request.requested_path(),
                    request.requested_path(),
                    PhysicalFileIdentity::new(99, 99),
                    FileMetadataFingerprint::new(1, 2, 3),
                    Arc::new("const value = 1;".to_owned()),
                )
                .unwrap();
                ImportObservation::accepted(request, accepted).unwrap()
            } else {
                ImportObservation::absent(request)
            }
        })
        .collect();
    assert!(
        database
            .publish_import_batch(&frontier, &snapshot, reads, observations)
            .is_err(),
        "accepted observations without exact manifest provenance must not publish"
    );
    assert_eq!(database.current_import_revision(), Some(revision));
}

#[test]
fn canceled_and_evicted_declaration_import_requests_recover() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source_text = (0..=MODULE_QUERY_MEMO_RETENTION)
        .map(|index| format!("const c{index} = @import(\"x{index}\");"))
        .collect::<Vec<_>>()
        .join("\n");
    let (_, mut assembler, context) = import_fixture(305, &source_text);
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let mut database =
        RevisionedQueryDatabase::with_declaration_memo_retention(MODULE_QUERY_MEMO_RETENTION);
    let revision = database
        .begin_import_inputs(&snapshot, context, reads)
        .unwrap();
    let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = |index| {
        declaration_import_key(
            &module,
            Category::ConstCandidate,
            format!("c{index}"),
            None,
            0,
            &format!("x{index}"),
        )
    };

    let canceled = CancellationToken::new();
    canceled.cancel();
    let aborted = database.runtime.request_registered(
        &database.declaration_imports,
        runtime_revision,
        key(0),
        canceled,
    );
    assert_eq!(execution(&aborted), RequestExecution::Aborted);
    assert!(aborted.terminal().is_none());

    for index in 0..=MODULE_QUERY_MEMO_RETENTION {
        let requested = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            key(index),
            CancellationToken::new(),
        );
        assert!(matches!(
            requested.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
                crate::declaration_candidate::DeclarationImportFailure::ResolutionUnavailable(_)
            ))
        ));
    }
    assert_eq!(
        database.declaration_imports.retention().terminals,
        MODULE_QUERY_MEMO_RETENTION
    );
    let recovered = database.runtime.request_registered(
        &database.declaration_imports,
        runtime_revision,
        key(0),
        CancellationToken::new(),
    );
    assert_eq!(execution(&recovered), RequestExecution::Computed);
}

#[test]
fn wide_root_imports_form_one_exact_compiler_frontier() {
    let source = r#"
            const a = @import("a");
            const b = @import("b");
            const c = @import("c");
            const d = @import("d");
            fn main() -> i32 { 0 }
        "#;
    let (mut session, mut assembler, context) = import_fixture(1, source);
    let (revision, plan) = begin_and_plan(&mut session, &mut assembler, context);
    let frontier = session
        .import_demand_frontier_for_roots(
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .unwrap();
    assert_eq!(frontier.revision(), revision);
    assert_eq!(frontier.revision().frontier_round(), 0);
    assert!(!frontier.requests().is_empty());
    assert_eq!(
        frontier
            .requests()
            .iter()
            .map(|request| request.occurrence())
            .collect::<BTreeSet<_>>()
            .len(),
        4,
        "all same-depth roots must be returned in one host batch"
    );

    let mut reversed = frontier
        .requests()
        .iter()
        .cloned()
        .map(ImportObservation::absent)
        .collect::<Vec<_>>();
    reversed.reverse();
    assert!(
        session
            .publish_import_observation_batch(
                &frontier,
                &assembler.snapshot().unwrap(),
                assembler.accepted_read_manifest(),
                reversed,
            )
            .unwrap_err()
            .to_string()
            .contains("exactly preserve")
    );

    let observations = frontier
        .requests()
        .iter()
        .cloned()
        .map(ImportObservation::absent)
        .collect();
    let successor = session
        .publish_import_observation_batch(
            &frontier,
            &assembler.snapshot().unwrap(),
            assembler.accepted_read_manifest(),
            observations,
        )
        .unwrap();
    assert_eq!(successor.frontier_round(), 1);
    assert_eq!(
        session
            .import_observation_ledger(successor)
            .unwrap()
            .iter()
            .count(),
        frontier.requests().len()
    );
}

#[test]
fn speculative_frontiers_are_effect_free_and_cannot_publish_host_results() {
    let (mut session, mut assembler, context) = import_fixture(
        2,
        r#"const helper = @import("helper"); fn main() -> i32 { 0 }"#,
    );
    let (revision, plan) = begin_and_plan(&mut session, &mut assembler, context);
    let speculative = session
        .import_demand_frontier_for_roots(
            revision,
            &plan,
            ImportDemandMode::Speculative,
            &plan.demand_roots(),
        )
        .unwrap();
    assert!(speculative.requests().is_empty());
    assert!(speculative.speculative_blocked());
    assert_eq!(
        session
            .import_observation_ledger(revision)
            .unwrap()
            .iter()
            .count(),
        0
    );
    assert!(
        session
            .publish_import_observation_batch(
                &speculative,
                &assembler.snapshot().unwrap(),
                assembler.accepted_read_manifest(),
                Vec::new(),
            )
            .unwrap_err()
            .to_string()
            .contains("speculative")
    );

    let rooted = session
        .import_demand_frontier_for_roots(
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .unwrap();
    assert!(!rooted.requests().is_empty());
    assert_eq!(rooted.revision(), revision);
}

#[test]
fn resolve_import_recomputes_when_only_discovery_context_changes() {
    let (mut session, mut assembler, first_context) = import_fixture(
        24,
        r#"const standard = @import("std"); fn main() -> i32 { 0 }"#,
    );
    let (first_revision, first_plan) =
        begin_and_plan(&mut session, &mut assembler, first_context.clone());
    let first = session
        .import_demand_frontier_for_roots(
            first_revision,
            &first_plan,
            ImportDemandMode::Rooted,
            &first_plan.demand_roots(),
        )
        .unwrap();
    // Policy v2 probes the vendored {root}/std/_std.rue before the
    // toolchain root, so the first frontier round holds the vendored
    // candidate; the captured std root still appears in the plan's
    // later group.
    assert!(
        first
            .requests()
            .iter()
            .any(|request| request.requested_path() == "/project/std/_std.rue")
    );
    assert!(
        first_plan
            .groups()
            .iter()
            .flat_map(|group| group.iter())
            .any(|request| request.requested_path().starts_with("/sdk/"))
    );

    let second_context =
        ImportDiscoveryContext::new(24, "/project", Some("/other-sdk"), "other-policy").unwrap();
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let second_revision = session
        .begin_import_input_request(&snapshot, second_context.clone(), reads.clone())
        .unwrap();
    let first_runtime_revision = Revision::new(
        first_revision.revision_id,
        first_revision.compatibility_token,
    );
    let second_runtime_revision = Revision::new(
        second_revision.revision_id,
        second_revision.compatibility_token,
    );
    assert!(
        !first_runtime_revision.is_compatible_with(second_runtime_revision),
        "a discovery-context change must start a new compatibility namespace"
    );
    let second_plan = session
        .stage_import_discovery(
            &snapshot,
            second_context.clone(),
            reads.shared_slice(),
            ImportObservationLedger::default(),
        )
        .unwrap();
    let second = session
        .import_demand_frontier_for_roots(
            second_revision,
            &second_plan,
            ImportDemandMode::Rooted,
            &second_plan.demand_roots(),
        )
        .unwrap();
    assert!(
        second
            .requests()
            .iter()
            .all(|request| request.context() == &second_context)
    );
    assert!(
        second_plan
            .groups()
            .iter()
            .flat_map(|group| group.iter())
            .any(|request| request.requested_path().starts_with("/other-sdk/"))
    );
    assert!(
        !second_plan
            .groups()
            .iter()
            .flat_map(|group| group.iter())
            .any(|request| request.requested_path().starts_with("/sdk/"))
    );
}

#[test]
fn explicit_occurrence_roots_select_one_of_twenty_seven_without_speculative_io() {
    let mut source = String::new();
    for index in 0..27 {
        source.push_str(&format!(
            "pub const m{index} = @import(\"m{index}.rue\");\n"
        ));
    }
    source.push_str("fn main() -> i32 { 0 }\n");
    let (mut session, mut assembler, context) = import_fixture(21, &source);
    let (revision, plan) = begin_and_plan(&mut session, &mut assembler, context);
    let selected = plan
        .groups()
        .iter()
        .find(|group| group[0].exact_specifier() == "m7.rue")
        .unwrap()[0]
        .occurrence()
        .clone();
    let roots = ImportDemandRoots::new([selected.clone()]);

    let speculative = session
        .import_demand_frontier_for_roots(revision, &plan, ImportDemandMode::Speculative, &roots)
        .unwrap();
    assert!(speculative.requests().is_empty());
    assert!(speculative.speculative_blocked());
    assert!(
        session
            .import_observation_ledger(revision)
            .unwrap()
            .is_empty()
    );

    let rooted = session
        .import_demand_frontier_for_roots(revision, &plan, ImportDemandMode::Rooted, &roots)
        .unwrap();
    assert!(!rooted.requests().is_empty());
    assert!(
        rooted
            .requests()
            .iter()
            .all(|request| request.occurrence() == &selected)
    );
}

#[test]
fn resolve_import_retains_more_than_module_cap_without_recomputation() {
    const OCCURRENCES: u32 = 4_100;

    let (_, mut assembler, context) = import_fixture(211, "fn main() -> i32 { 0 }");
    let mut database = RevisionedQueryDatabase::default();
    let (_, _, revision, _) = begin_database_plan(&mut database, &mut assembler, context);
    let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
    let importer = ModuleId::from_logical_path("main.rue").unwrap();
    let key = |index: u32| ResolveImportKey {
        occurrence: crate::ImportOccurrenceKey::from_directive(&crate::ImportDirective::new(
            importer.clone(),
            index.saturating_mul(2),
            index.saturating_mul(2).saturating_add(1),
            format!("missing-{index}.rue").into(),
        )),
        mode: ImportDemandMode::Rooted,
    };

    for index in 0..OCCURRENCES {
        let attempt = database.runtime.request_registered(
            &database.resolve_imports,
            runtime_revision,
            key(index),
            CancellationToken::new(),
        );
        assert_eq!(attempt.execution(), RequestExecution::Computed);
        assert!(attempt.terminal().is_some());
    }
    let retention = database.resolve_imports.retention();
    assert_eq!(
        retention.terminal_limit,
        IMPORT_OCCURRENCE_QUERY_MEMO_RETENTION
    );
    assert_eq!(retention.memo_nodes, OCCURRENCES as usize);
    assert_eq!(retention.terminals, OCCURRENCES as usize);
    let mut speculative_key = key(0);
    speculative_key.mode = ImportDemandMode::Speculative;
    let speculative = database.runtime.request_registered(
        &database.resolve_imports,
        runtime_revision,
        speculative_key.clone(),
        CancellationToken::new(),
    );
    assert_eq!(speculative.execution(), RequestExecution::Computed);
    let retention = database.resolve_imports.retention();
    assert_eq!(retention.memo_nodes, OCCURRENCES as usize + 1);
    assert_eq!(
        retention.terminals,
        OCCURRENCES as usize + 1,
        "rooted and speculative occurrence variants have distinct identities"
    );
    assert_eq!(database.runtime.metrics().retention_growth, 0);
    let claims = database.runtime.metrics().claims;

    for index in 0..OCCURRENCES {
        let attempt = database.runtime.request_registered(
            &database.resolve_imports,
            runtime_revision,
            key(index),
            CancellationToken::new(),
        );
        assert_eq!(
            attempt.execution(),
            RequestExecution::Reused,
            "occurrence {index} was evicted at the old module-scaled cap"
        );
    }
    let speculative = database.runtime.request_registered(
        &database.resolve_imports,
        runtime_revision,
        speculative_key,
        CancellationToken::new(),
    );
    assert_eq!(speculative.execution(), RequestExecution::Reused);
    assert_eq!(
        database.runtime.metrics().claims,
        claims,
        "re-reading a >4,096 occurrence universe must not recompute"
    );
}

#[test]
fn new_request_generation_has_no_carried_ledger_authority() {
    let (mut session, mut assembler, context) = import_fixture(
        22,
        r#"const missing = @import("missing.rue"); fn main() -> i32 { 0 }"#,
    );
    let (first_revision, first_plan) =
        begin_and_plan(&mut session, &mut assembler, context.clone());
    let first = session
        .import_demand_frontier_for_roots(
            first_revision,
            &first_plan,
            ImportDemandMode::Rooted,
            &first_plan.demand_roots(),
        )
        .unwrap();
    let successor = session
        .publish_import_observation_batch(
            &first,
            &assembler.snapshot().unwrap(),
            assembler.accepted_read_manifest(),
            first
                .requests()
                .iter()
                .cloned()
                .map(ImportObservation::absent)
                .collect(),
        )
        .unwrap();
    let stale = session.import_observation_ledger(successor).unwrap();
    assert!(!stale.is_empty());

    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let fresh_revision = session
        .begin_import_input_request(&snapshot, context.clone(), reads.clone())
        .unwrap();
    let fresh_ledger = session.import_observation_ledger(fresh_revision).unwrap();
    assert!(fresh_ledger.is_empty());
    let fresh_plan = session
        .stage_import_discovery(&snapshot, context, reads.shared_slice(), fresh_ledger)
        .unwrap();
    let reread = session
        .import_demand_frontier_for_roots(
            fresh_revision,
            &fresh_plan,
            ImportDemandMode::Rooted,
            &fresh_plan.demand_roots(),
        )
        .unwrap();
    assert_eq!(
        reread
            .requests()
            .iter()
            .map(ImportDiscoveryRequest::requested_path)
            .collect::<Vec<_>>(),
        first
            .requests()
            .iter()
            .map(ImportDiscoveryRequest::requested_path)
            .collect::<Vec<_>>()
    );
}

#[test]
fn duplicate_occurrences_share_one_host_operation_and_fan_out_typed_results() {
    let (mut session, mut assembler, context) = import_fixture(
        23,
        r#"
                const first = @import("shared.rue");
                const second = @import("shared.rue");
                fn main() -> i32 { 0 }
            "#,
    );
    let (revision, plan) = begin_and_plan(&mut session, &mut assembler, context);
    let frontier = session
        .import_demand_frontier_for_roots(
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .unwrap();
    assert_eq!(frontier.requests().len(), 1, "one host candidate operation");

    let successor = session
        .publish_import_observation_batch(
            &frontier,
            &assembler.snapshot().unwrap(),
            assembler.accepted_read_manifest(),
            vec![ImportObservation::absent(frontier.requests()[0].clone())],
        )
        .unwrap();
    let ledger = session.import_observation_ledger(successor).unwrap();
    assert_eq!(
        ledger.len(),
        2,
        "result fans out to both source occurrences"
    );
    assert_eq!(
        ledger
            .iter()
            .map(|observation| observation.request().occurrence())
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
}

#[test]
fn successor_revisions_carry_observations_but_new_epochs_reread() {
    let source = r#"const helper = @import("helper"); fn main() -> i32 { 0 }"#;
    let (mut session, mut assembler, context) = import_fixture(3, source);
    let (revision, plan) = begin_and_plan(&mut session, &mut assembler, context);
    let first = session
        .import_demand_frontier_for_roots(
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .unwrap();
    let first_paths = first
        .requests()
        .iter()
        .map(|request| request.requested_path().to_owned())
        .collect::<BTreeSet<_>>();
    let successor = session
        .publish_import_observation_batch(
            &first,
            &assembler.snapshot().unwrap(),
            assembler.accepted_read_manifest(),
            first
                .requests()
                .iter()
                .cloned()
                .map(ImportObservation::absent)
                .collect(),
        )
        .unwrap();
    let carried = session.import_observation_ledger(successor).unwrap();
    assert_eq!(carried.iter().count(), first.requests().len());
    let successor_plan = session
        .stage_import_discovery(
            &assembler.snapshot().unwrap(),
            plan.context().clone(),
            assembler.accepted_read_manifest().shared_slice(),
            carried,
        )
        .unwrap();
    let next = session
        .import_demand_frontier_for_roots(
            successor,
            &successor_plan,
            ImportDemandMode::Rooted,
            &successor_plan.demand_roots(),
        )
        .unwrap();
    assert!(
        next.requests()
            .iter()
            .all(|request| !first_paths.contains(request.requested_path()))
    );

    let new_context =
        ImportDiscoveryContext::new(4, "/project", Some("/sdk"), "test-policy").unwrap();
    let new_snapshot = assembler.snapshot().unwrap();
    let new_revision = session
        .begin_import_input_request(
            &new_snapshot,
            new_context.clone(),
            assembler.accepted_read_manifest(),
        )
        .unwrap();
    let new_plan = session
        .stage_import_discovery(
            &new_snapshot,
            new_context,
            assembler.accepted_read_manifest().shared_slice(),
            ImportObservationLedger::default(),
        )
        .unwrap();
    let reread = session
        .import_demand_frontier_for_roots(
            new_revision,
            &new_plan,
            ImportDemandMode::Rooted,
            &new_plan.demand_roots(),
        )
        .unwrap();
    assert_eq!(
        reread
            .requests()
            .iter()
            .map(|request| request.requested_path().to_owned())
            .collect::<BTreeSet<_>>(),
        first_paths
    );
}

#[test]
fn input_stamp_tables_follow_exact_retained_full_and_overlay_views() {
    const GENERATIONS: u64 = IMPORT_INPUT_REVISION_RETENTION as u64 + 32;
    fn assert_exact_values<T: Eq + Hash + std::fmt::Debug>(
        actual: &AHashMap<T, RetainedValueStamp>,
        expected: &AHashMap<T, usize>,
    ) {
        assert_eq!(actual.len(), expected.len());
        for value in expected.keys() {
            assert!(actual[value].retained_views > 0);
        }
    }

    let mut database = RevisionedQueryDatabase::default();
    let first_context =
        ImportDiscoveryContext::new(10_000, "/project", Some("/sdk"), "retention-stress").unwrap();
    let mut latest_context = first_context.clone();

    for generation in 0..GENERATIONS {
        let context = ImportDiscoveryContext::new(
            10_000 + generation,
            "/project",
            Some("/sdk"),
            "retention-stress",
        )
        .unwrap();
        latest_context = context.clone();
        let mut assembler = DiscoverySourceAssembler::new(
            context.clone(),
            "/project/main.rue",
            "/physical/main.rue",
            PhysicalFileIdentity::new(1, 1),
            FileMetadataFingerprint::new(1, 2, 3),
            Arc::new("const dependency = @import(\"dep.rue\"); fn main() -> i32 { 0 }".to_owned()),
        )
        .unwrap();
        let (initial_snapshot, initial_reads, revision, plan) =
            begin_database_plan(&mut database, &mut assembler, context);
        let frontier = database
            .import_frontier(
                revision,
                &plan,
                ImportDemandMode::Rooted,
                &plan.demand_roots(),
            )
            .unwrap();

        let recover = generation % 2 == 1;
        let (successor_snapshot, successor_reads) = if recover {
            let accepted_request = frontier
                .requests()
                .iter()
                .find(|request| request.requested_path() == "/project/dep.rue")
                .expect("the project-relative candidate is in the compiler frontier");
            let canonical_path = format!("/physical/dep-{generation}.rue");
            assembler
                .add_explicit(
                    accepted_request.requested_path(),
                    &canonical_path,
                    PhysicalFileIdentity::new(2, generation + 1),
                    FileMetadataFingerprint::new(generation + 1, 5, 6),
                    Arc::new(format!("const value = {generation};")),
                )
                .unwrap();
            (
                assembler.snapshot().unwrap(),
                assembler.accepted_read_manifest(),
            )
        } else {
            (initial_snapshot, initial_reads)
        };
        let observations = frontier
            .requests()
            .iter()
            .cloned()
            .map(|request| {
                let Some(entry) = successor_reads
                    .iter()
                    .find(|entry| entry.requested_path() == request.requested_path())
                else {
                    return ImportObservation::absent(request);
                };
                let file_id = successor_snapshot
                    .files()
                    .find(|source| {
                        successor_snapshot.module_id(source.file_id) == Some(entry.module())
                    })
                    .unwrap()
                    .file_id;
                let accepted = crate::AcceptedImportSource::new(
                    entry.requested_path(),
                    entry.canonical_path(),
                    entry.metadata_identity(),
                    entry.metadata_fingerprint(),
                    successor_snapshot.shared_source_text(file_id).unwrap(),
                )
                .unwrap();
                ImportObservation::accepted(request, accepted).unwrap()
            })
            .collect();
        database
            .publish_import_batch(
                &frontier,
                &successor_snapshot,
                successor_reads,
                observations,
            )
            .unwrap();

        let metrics = database.input_stamp_retention_metrics();
        assert!(metrics.module_views <= MODULE_INPUT_REVISION_RETENTION);
        assert!(metrics.import_views <= IMPORT_INPUT_REVISION_RETENTION);
        assert!(metrics.module_source_stamps <= metrics.module_views.saturating_mul(2));
        assert!(metrics.import_context_stamps <= metrics.import_views);
        assert!(metrics.accepted_topology_stamps <= metrics.import_views);
        assert!(metrics.accepted_read_provenance_stamps <= metrics.import_views * 2);
        assert!(metrics.import_observation_stamps <= metrics.import_views * 4);
    }

    let import_store = lock_import_store(&database.import_store);
    assert_eq!(
        import_store.revisions.len(),
        IMPORT_INPUT_REVISION_RETENTION
    );
    assert!(
        !import_store.context_stamps.contains_key(&first_context),
        "a context used only by evicted views must not keep its stamp"
    );
    assert!(
        import_store.context_stamps.contains_key(&latest_context),
        "the current view must keep its context stamp"
    );

    let mut context_refs = AHashMap::new();
    let mut topology_refs = AHashMap::new();
    let mut provenance_refs = AHashMap::new();
    let mut observation_refs = AHashMap::new();
    for view in &import_store.revisions {
        *context_refs.entry(view.context.clone()).or_insert(0) += 1;
        *topology_refs
            .entry(view.accepted_topology.clone())
            .or_insert(0) += 1;
        for read in view.accepted_reads.iter() {
            *provenance_refs.entry(read.clone()).or_insert(0) += 1;
        }
        for observation in view.ledger.iter() {
            *observation_refs.entry(observation.clone()).or_insert(0) += 1;
        }
    }
    assert_exact_values(&import_store.context_stamps, &context_refs);
    assert_exact_values(&import_store.topology_stamps, &topology_refs);
    assert_exact_values(&import_store.provenance_stamps, &provenance_refs);
    assert_exact_values(&import_store.observation_stamps, &observation_refs);
    drop(import_store);

    let module_store = database
        .module_store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(module_store.revisions.len(), GENERATIONS as usize * 2);
    let mut module_refs = AHashMap::new();
    let mut metadata_refs = AHashMap::new();
    for view in &module_store.revisions {
        for source in view.snapshot.source_revision().modules() {
            *module_refs
                .entry(ModuleInputLeaf {
                    revision: source.clone(),
                })
                .or_insert(0) += 1;
        }
        for metadata in view.metadata.iter() {
            *metadata_refs.entry(metadata.clone()).or_insert(0) += 1;
        }
    }
    assert_exact_values(&module_store.stamps, &module_refs);
    assert_exact_values(&module_store.metadata_stamps, &metadata_refs);
    drop(module_store);

    // Exercise the same centralized module-view trimming primitive at a
    // small limit so this stress test proves eviction without making the
    // compiler parse thousands of otherwise identical generations.
    const TEST_MODULE_RETENTION: usize = 8;
    let mut bounded_module_store = ModuleInputStore {
        retention_limit: TEST_MODULE_RETENTION,
        ..ModuleInputStore::default()
    };
    bounded_module_store
        .protected_revisions
        .insert(Revision::new(1, 0));
    for generation in 0..TEST_MODULE_RETENTION * 2 {
        let source_text = format!("fn main() -> i32 {{ {generation} }}");
        let snapshot = source_snapshot(
            &[(
                (generation + 1) as u32,
                "/main.rue",
                "main.rue",
                &source_text,
            )],
            (generation + 1) as u32,
        );
        let sources = snapshot.source_revision().modules().to_vec();
        let mut metadata = module_metadata_leaves(&snapshot)
            .into_values()
            .collect::<Vec<_>>();
        metadata.sort_by(module_metadata_order);
        let metadata =
            crate::shared_segments::SharedSegments::flat(metadata.into(), module_metadata_order);
        for source in snapshot.source_revision().modules() {
            let leaf = ModuleInputLeaf {
                revision: source.clone(),
            };
            let ModuleInputStore {
                next_stamp, stamps, ..
            } = &mut bounded_module_store;
            exact_value_stamp(next_stamp, stamps, &leaf);
            retain_stamp_value(stamps, &leaf);
        }
        retain_module_input_view(
            &mut bounded_module_store,
            Arc::new(ModuleInputView {
                revision: Revision::new(generation as u64 + 1, 0),
                snapshot,
                metadata,
                stamp_lease: Arc::new(ModuleInputStampLease {
                    parent: None,
                    sources: sources.into(),
                    metadata: Arc::from([]),
                }),
            }),
        );
    }
    assert_eq!(
        bounded_module_store.revisions.len(),
        TEST_MODULE_RETENTION + 1,
        "one old selection root is the documented constant allowance"
    );
    assert_eq!(bounded_module_store.stamps.len(), TEST_MODULE_RETENTION + 1);
    assert_eq!(
        bounded_module_store
            .revisions
            .front()
            .unwrap()
            .revision
            .id(),
        1
    );
    assert!(
        bounded_module_store
            .stamps
            .values()
            .all(|retained| retained.retained_views == 1)
    );
}

#[test]
fn failed_runtime_publication_releases_pending_input_stamp_leases() {
    let (_, mut assembler, context) = import_fixture(12_000, "fn main() -> i32 { 0 }");
    let mut database = RevisionedQueryDatabase::default();
    database.set_module_input_retention_for_test(1);
    let (snapshot, reads, published, _) =
        begin_database_plan(&mut database, &mut assembler, context.clone());
    let before = database.input_stamp_retention_metrics();
    let published_leaves_before = database.import_view_full_leaves_published();

    let mut changed = DiscoverySourceAssembler::new(
        context.clone(),
        "/project/main.rue",
        "/physical/main.rue",
        PhysicalFileIdentity::new(1, 1),
        FileMetadataFingerprint::new(4, 5, 6),
        Arc::new("fn main() -> i32 { 1 }".to_owned()),
    )
    .unwrap();
    let changed_snapshot = changed.snapshot().unwrap();
    let changed_reads = changed.accepted_read_manifest();
    database.next_revision = published.revision_id;
    assert!(
        database
            .begin_import_inputs(&changed_snapshot, context, changed_reads)
            .is_err(),
        "reusing an immutable runtime revision must reject publication"
    );
    assert_eq!(database.input_stamp_retention_metrics(), before);
    assert_eq!(
        database.import_view_full_leaves_published(),
        published_leaves_before,
        "a rejected runtime publication is not counted as published work"
    );
    let module_store = database
        .module_store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(module_store.by_revision.len(), module_store.revisions.len());
    for view in &module_store.revisions {
        assert!(Arc::ptr_eq(
            view,
            module_store
                .by_revision
                .get(&view.revision)
                .expect("every committed module view keeps its exact index entry")
        ));
    }
    assert_eq!(
        module_store
            .revisions
            .back()
            .unwrap()
            .snapshot
            .source_revision(),
        snapshot.source_revision()
    );
    drop(module_store);
    assert_eq!(reads, assembler.accepted_read_manifest());
}

#[test]
fn last_good_module_stamp_survives_beyond_the_revision_window_and_recovers_green() {
    const RETENTION: usize = 8;
    let good = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
        )],
        1,
    );
    let good_source = good.source_revision().modules()[0].clone();
    let mut session = CompilerSession::new();
    session.set_module_input_retention_for_test(RETENTION);
    assert!(session.update(&good).result().is_ok());
    let good_stamp = session
        .module_source_stamp_for_test(&good_source)
        .expect("the selected successful source owns a stamp");

    for generation in 0..RETENTION + 4 {
        let text = format!("fn main() -> i32 {{ missing_{generation} }} @");
        let failed = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
        assert!(session.update(&failed).result().is_err());
    }
    assert_eq!(
        session.module_source_stamp_for_test(&good_source),
        Some(good_stamp),
        "last-good selection keeps its exact content-to-stamp mapping past the recency window"
    );
    let retained = session.unstable_metrics().retention();
    assert_eq!(retained.retained_module_input_views, RETENTION + 1);

    let recovered = session.update(&good);
    assert!(recovered.result().is_ok());
    assert_eq!(recovered.work().syntax.parser_invocations, 0);
    assert_eq!(
        session.module_source_stamp_for_test(&good_source),
        Some(good_stamp),
        "recovery reuses the protected last-good stamp"
    );
}

// -----------------------------------------------------------------------
// RUE-1091 slice 3a — widened module name index + lookup families.
//
// These exercise the registered query machinery for the ADR-0066 §4 exact
// provider boundary. Production body analysis consumes these exact
// terminals; the focused tests below pin their independent query behavior.
// -----------------------------------------------------------------------

#[test]
fn module_index_carries_candidate_columns_and_stays_in_module() {
    // Requesting one module's index must build that module's index alone,
    // carrying kind and visibility for every namespace, without enumerating
    // any other module (ADR-0066 §4: O(module declarations), no cross reach).
    let snapshot = source_snapshot(
        &[
            (
                1,
                "/a.rue",
                "a.rue",
                "pub struct Public {}\nstruct Private {}\ndrop fn Public(self) {}\n",
            ),
            (2, "/b.rue", "b.rue", "fn b() -> i32 { 1 }\n"),
        ],
        1,
    );
    let a = ModuleId::from_logical_path("a.rue").unwrap();
    let b = ModuleId::from_logical_path("b.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let attempt = database.runtime.request_registered(
        &database.module_indexes,
        revision,
        ModuleQueryKey(a.clone()),
        CancellationToken::new(),
    );
    let terminal = attempt.terminal().unwrap();
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!()
    };
    let index = value.0.clone().unwrap();

    let public = index
        .definitions
        .iter()
        .find(|entry| {
            entry.namespace == DefinitionNamespace::ModuleItem && entry.name.as_ref() == "Public"
        })
        .expect("public struct candidate");
    assert_eq!(public.kind, DefinitionKind::Struct);
    assert_eq!(
        public.visibility,
        Some(rue_parser::ast::Visibility::Public),
        "the candidate set carries visibility"
    );
    let private = index
        .definitions
        .iter()
        .find(|entry| entry.name.as_ref() == "Private")
        .expect("private struct candidate");
    assert_eq!(private.kind, DefinitionKind::Struct);
    assert_ne!(
        private.visibility,
        Some(rue_parser::ast::Visibility::Public)
    );
    let destructor = index
        .definitions
        .iter()
        .find(|entry| entry.namespace == DefinitionNamespace::Destructor)
        .expect("destructor namespace candidate");
    assert_eq!(
        destructor.kind,
        DefinitionKind::Destructor,
        "the index carries a candidate for every namespace"
    );

    let built = database.module_index_build_log.lock().unwrap().clone();
    assert!(
        built.contains(&a),
        "a's index must have been built: {built:?}"
    );
    assert!(
        !built.contains(&b),
        "building a's index must not enumerate module b: {built:?}"
    );
}

#[test]
fn lookup_records_distinguish_every_canonical_outcome() {
    // Positive, negative, ambiguous, visibility-filtered, and
    // kind-distinguished results are each a distinct, correct canonical
    // record derived from the same module index.
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "pub struct Uniq {}\nfn dup() {}\nfn dup() {}\nstruct Hidden {}\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    let positive = canonical_of(&request_lookup_name(
        &database,
        revision,
        &m,
        DefinitionNamespace::ModuleItem,
        "Uniq",
    ));
    assert!(matches!(positive, CanonicalNameResolution::Unique(_)));

    let absent = canonical_of(&request_lookup_name(
        &database,
        revision,
        &m,
        DefinitionNamespace::ModuleItem,
        "missing",
    ));
    assert_eq!(absent, CanonicalNameResolution::Absent);

    let ambiguous = canonical_of(&request_lookup_name(
        &database,
        revision,
        &m,
        DefinitionNamespace::ModuleItem,
        "dup",
    ));
    assert!(matches!(ambiguous, CanonicalNameResolution::Ambiguous(_)));
    assert_eq!(ambiguous.candidates().len(), 2);

    // Success, absence, and ambiguity are mutually distinct records.
    assert_ne!(positive, absent);
    assert_ne!(positive, ambiguous);
    assert_ne!(absent, ambiguous);

    // Visibility filtering yields a distinct record: a private candidate is
    // dropped when consulted across a visibility boundary.
    let hidden = canonical_of(&request_lookup_name(
        &database,
        revision,
        &m,
        DefinitionNamespace::ModuleItem,
        "Hidden",
    ));
    assert!(matches!(hidden, CanonicalNameResolution::Unique(_)));
    assert_eq!(
        hidden.visible(false),
        hidden,
        "same-domain access retains it"
    );
    assert_eq!(
        hidden.visible(true),
        CanonicalNameResolution::Absent,
        "a private candidate is filtered out across the boundary"
    );
    assert_ne!(hidden, hidden.visible(true));

    // Kind distinguishes the same candidate set: the ambiguous function pair
    // survives `of_kind(Function)` but vanishes under `of_kind(Struct)`, and
    // the unique struct survives only under `of_kind(Struct)`.
    assert_eq!(ambiguous.of_kind(DefinitionKind::Function), ambiguous);
    assert_eq!(
        ambiguous.of_kind(DefinitionKind::Struct),
        CanonicalNameResolution::Absent
    );
    assert_ne!(
        positive.of_kind(DefinitionKind::Struct),
        positive.of_kind(DefinitionKind::Function)
    );
    assert_eq!(positive.of_kind(DefinitionKind::Struct), positive);
}

#[test]
fn equal_lookup_output_preserves_stamp_across_unrelated_module_edit() {
    let first = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn keep() -> i32 { 1 }\n"),
            (2, "/b.rue", "b.rue", "fn other() -> i32 { 1 }\n"),
        ],
        1,
    );
    let second = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn keep() -> i32 { 1 }\n"),
            // Only module b changes.
            (2, "/b.rue", "b.rue", "fn other() -> i32 { 2 }\n"),
        ],
        1,
    );
    let a = ModuleId::from_logical_path("a.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let first_revision = revision_for(&mut database, &first);
    let first_stamp = request_lookup_name(
        &database,
        first_revision,
        &a,
        DefinitionNamespace::ModuleItem,
        "keep",
    )
    .terminal()
    .unwrap()
    .stamp();

    let second_revision = revision_for(&mut database, &second);
    let warm = request_lookup_name(
        &database,
        second_revision,
        &a,
        DefinitionNamespace::ModuleItem,
        "keep",
    );
    assert_eq!(
        execution(&warm),
        RequestExecution::Reused,
        "a's lookup must not recompute when only b changed"
    );
    assert_eq!(
        warm.terminal().unwrap().stamp(),
        first_stamp,
        "equal lookup output preserves its stamp (consumer-green precondition)"
    );
}

#[test]
fn negative_to_positive_flips_lookup_while_unrelated_name_keeps_stamp() {
    let first = source_snapshot(&[(1, "/m.rue", "m.rue", "fn stable() -> i32 { 1 }\n")], 1);
    let second = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn stable() -> i32 { 1 }\nfn extra() -> i32 { 2 }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let parse_revision = revision_for(&mut database, &first);
    let graph = crate::test_support::test_import_graph(&first).unwrap();
    database.adopt_test_import_graph_for_revision(parse_revision, graph);
    let first_revision = database.current_semantic_revision().unwrap();

    let extra_first = request_lookup_name(
        &database,
        first_revision,
        &m,
        DefinitionNamespace::ModuleItem,
        "extra",
    );
    assert_eq!(canonical_of(&extra_first), CanonicalNameResolution::Absent);
    let extra_first_stamp = extra_first.terminal().unwrap().stamp();
    let stable_first_stamp = request_lookup_name(
        &database,
        first_revision,
        &m,
        DefinitionNamespace::ModuleItem,
        "stable",
    )
    .terminal()
    .unwrap()
    .stamp();

    let parse_revision = revision_for(&mut database, &second);
    let graph = crate::test_support::test_import_graph(&second).unwrap();
    database.adopt_test_import_graph_for_revision(parse_revision, graph);
    let second_revision = database.current_semantic_revision().unwrap();
    // The queried name gains a declaration: negative -> positive flips it.
    let extra_second = request_lookup_name(
        &database,
        second_revision,
        &m,
        DefinitionNamespace::ModuleItem,
        "extra",
    );
    assert!(matches!(
        canonical_of(&extra_second),
        CanonicalNameResolution::Unique(_)
    ));
    assert_ne!(
        extra_second.terminal().unwrap().stamp(),
        extra_first_stamp,
        "adding the queried name must change its lookup stamp"
    );

    // An unrelated name in the same edited module recomputes but its result
    // is equal, so its stamp is preserved — the firewall the design rests on.
    // Assert recompute-then-preserve on the one key: the sibling is genuinely
    // re-evaluated (its module index changed) yet keeps its stamp, not merely
    // reused without evaluation.
    let stable_second = request_lookup_name(
        &database,
        second_revision,
        &m,
        DefinitionNamespace::ModuleItem,
        "stable",
    );
    assert_eq!(
        execution(&stable_second),
        RequestExecution::Computed,
        "the edited module's sibling lookup must actually re-evaluate"
    );
    assert_eq!(
        stable_second.terminal().unwrap().stamp(),
        stable_first_stamp,
        "adding an unrelated name must leave a sibling lookup's stamp equal"
    );
}

#[test]
fn absent_import_bindings_are_first_class_records_with_stamp_discipline() {
    let first = source_snapshot(&[(1, "/m.rue", "m.rue", "fn main() -> i32 { 0 }\n")], 1);
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let parse_revision = revision_for(&mut database, &first);
    let graph = crate::test_support::test_import_graph(&first).unwrap();
    database.adopt_test_import_graph_for_revision(parse_revision, graph);
    let first_revision = database.current_semantic_revision().unwrap();

    let absent = request_lookup_import(&database, first_revision, &m, "missing.rue");
    assert_eq!(
        import_binding(&absent),
        LookupImportValue(Err(ImportBindingFailure::Absent))
    );
    let absent_stamp = absent.terminal().unwrap().stamp();

    // Editing unrelated declarations recomputes the module index without
    // changing the retained failed lookup.
    let second = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn unrelated() -> i32 { 1 }\n\
                     fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let parse_revision = revision_for(&mut database, &second);
    let graph = crate::test_support::test_import_graph(&second).unwrap();
    database.adopt_test_import_graph_for_revision(parse_revision, graph);
    let second_revision = database.current_semantic_revision().unwrap();
    let absent_again = request_lookup_import(&database, second_revision, &m, "missing.rue");
    assert_eq!(
        absent_again.terminal().unwrap().stamp(),
        absent_stamp,
        "an unrelated edit preserves the absent lookup stamp"
    );

    // Every import-binding evaluation consulted only the consulting module's
    // own index — the lookup never reaches into another module.
    let evaluated = database.lookup_import_eval_log.lock().unwrap().clone();
    assert!(!evaluated.is_empty());
    assert!(
        evaluated.iter().all(|key| key.module == m),
        "import lookups must consult only their own module: {evaluated:?}"
    );
}

#[test]
fn import_binding_classifier_covers_absent_rejected_and_repeated_sites() {
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let classify = |specifier: &str, directives: &[crate::ImportDirective]| {
        let normalized = rue_air::normalize_module_path(specifier);
        let directive = directives
            .iter()
            .find(|directive| rue_air::normalize_module_path(directive.specifier()) == normalized);
        LookupImportValue::classify(normalized, directive)
    };
    // A specifier that normalizes to an empty module path is a first-class
    // rejected binding.
    let dot = crate::ImportDirective::new(m.clone(), 0, 1, Arc::from("."));
    assert_eq!(
        classify(".", std::slice::from_ref(&dot)),
        LookupImportValue(Err(ImportBindingFailure::Rejected))
    );
    assert_eq!(
        classify("other.rue", std::slice::from_ref(&dot)),
        LookupImportValue(Err(ImportBindingFailure::Absent))
    );
    let duplicated = [
        crate::ImportDirective::new(m.clone(), 0, 1, Arc::from("dep.rue")),
        crate::ImportDirective::new(m.clone(), 2, 3, Arc::from("dep.rue")),
    ];
    assert_eq!(
        classify("dep.rue", &duplicated),
        LookupImportValue(Ok(ResolvedImportBinding {
            normalized_specifier: Arc::from("dep.rue"),
            target: None,
        }))
    );
    assert_eq!(
        classify("dep.rue", std::slice::from_ref(&duplicated[0])),
        LookupImportValue(Ok(ResolvedImportBinding {
            normalized_specifier: Arc::from("dep.rue"),
            target: None,
        }))
    );

    // RUE-1091 slice 3b regression (carried from the 3a review): both the
    // requested specifier and every directive specifier normalize through
    // the one `normalize_module_path` authority before matching.
    //
    // Case 1 — `./dep.rue` and `dep.rue` are the same physical target, so
    // two sites spelled the two ways share one resolved binding. A raw
    // string match would give the sites distinct lookup identities.
    let mixed_spellings = [
        crate::ImportDirective::new(m.clone(), 0, 1, Arc::from("./dep.rue")),
        crate::ImportDirective::new(m.clone(), 2, 3, Arc::from("dep.rue")),
    ];
    assert_eq!(
        classify("dep.rue", &mixed_spellings),
        LookupImportValue(Ok(ResolvedImportBinding {
            normalized_specifier: Arc::from("dep.rue"),
            target: None,
        })),
        "`./dep.rue` and `dep.rue` are one target and one binding"
    );

    // Case 2 — a normalized request against a `./`-spelled directive must
    // resolve, never fall through to a false `Absent`. A raw match of
    // `dep.rue` against a lone `./dep.rue` directive would be `Absent`.
    let dot_slash = crate::ImportDirective::new(m.clone(), 0, 1, Arc::from("./dep.rue"));
    assert_eq!(
        classify("dep.rue", std::slice::from_ref(&dot_slash)),
        LookupImportValue(Ok(ResolvedImportBinding {
            normalized_specifier: Arc::from("dep.rue"),
            target: None,
        })),
        "a normalized request against a `./`-spelled directive must resolve, \
             not be a false Absent"
    );
}

#[test]
fn editing_module_revalidates_only_its_own_retained_lookups() {
    let first = source_snapshot(
        &[
            (1, "/a.rue", "a.rue", "fn a_fn() -> i32 { 1 }\n"),
            (2, "/b.rue", "b.rue", "fn b_fn() -> i32 { 1 }\n"),
        ],
        1,
    );
    // Editing a's declarations changes a's name index; b is untouched.
    let second = source_snapshot(
        &[
            (
                1,
                "/a.rue",
                "a.rue",
                "fn a_fn() -> i32 { 1 }\nfn a_added() -> i32 { 2 }\n",
            ),
            (2, "/b.rue", "b.rue", "fn b_fn() -> i32 { 1 }\n"),
        ],
        1,
    );
    let a = ModuleId::from_logical_path("a.rue").unwrap();
    let b = ModuleId::from_logical_path("b.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let first_revision = revision_for(&mut database, &first);
    let _ = request_lookup_name(
        &database,
        first_revision,
        &a,
        DefinitionNamespace::ModuleItem,
        "a_fn",
    )
    .terminal();
    let _ = request_lookup_name(
        &database,
        first_revision,
        &b,
        DefinitionNamespace::ModuleItem,
        "b_fn",
    )
    .terminal();
    database.lookup_name_eval_log.lock().unwrap().clear();

    let second_revision = revision_for(&mut database, &second);
    let _ = request_lookup_name(
        &database,
        second_revision,
        &a,
        DefinitionNamespace::ModuleItem,
        "a_fn",
    )
    .terminal();
    let b_second = request_lookup_name(
        &database,
        second_revision,
        &b,
        DefinitionNamespace::ModuleItem,
        "b_fn",
    );
    assert_eq!(
        execution(&b_second),
        RequestExecution::Reused,
        "b's lookup must reuse its terminal when only a changed"
    );

    let evaluated = database.lookup_name_eval_log.lock().unwrap().clone();
    let a_key = LookupNameKey {
        module: a.clone(),
        namespace: DefinitionNamespace::ModuleItem,
        name: Arc::from("a_fn"),
    };
    let b_key = LookupNameKey {
        module: b.clone(),
        namespace: DefinitionNamespace::ModuleItem,
        name: Arc::from("b_fn"),
    };
    assert!(
        evaluated.contains(&a_key),
        "the edited module's retained lookup must revalidate: {evaluated:?}"
    );
    assert!(
        !evaluated.contains(&b_key),
        "an unedited module's lookup must not recompute: {evaluated:?}"
    );
}

// -----------------------------------------------------------------------
// RUE-1091 slice 3b — the exact body-fact provider + differential adapter.
//
// Each op requests its exact backing terminal through the query context, so
// the returned owned fact is proven fact-for-fact against the production
// epoch's equivalent terminal, and the probe's recorded edges prove each op
// observed exactly that terminal.
// -----------------------------------------------------------------------

fn epoch_name_resolution(
    attempt: &QueryRequestAttempt<LookupNameValue>,
) -> rue_air::NameResolution {
    let terminal = attempt.terminal().unwrap();
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!("LookupName publishes typed values")
    };
    name_resolution_from_value(value)
}

fn recorded_family<'a>(
    outcome_dependencies: &'a [rue_query::NodeIdentity],
    family: &str,
) -> Vec<&'a rue_query::NodeIdentity> {
    outcome_dependencies
        .iter()
        .filter(|node| node.family() == family)
        .collect()
}

#[test]
fn provider_name_lookup_matches_epoch_and_records_lookup_name_edge() {
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "pub struct Uniq {}\nfn dup() {}\nfn dup() {}\nstruct Hidden {}\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let parse_revision = revision_for(&mut database, &snapshot);
    let graph = crate::test_support::test_import_graph(&snapshot).unwrap();
    database.adopt_test_import_graph_for_revision(parse_revision, graph);
    let revision = database.current_semantic_revision().unwrap();
    let config = semantic_configuration();

    let outcome = database.probe_ready_body_facts(revision, config, "name-lookup", |provider| {
        (
            provider.lookup_unqualified(&m, rue_air::ProviderNamespace::ModuleItem, "Uniq"),
            provider.lookup_unqualified(&m, rue_air::ProviderNamespace::ModuleItem, "dup"),
            provider.lookup_unqualified(&m, rue_air::ProviderNamespace::ModuleItem, "absent"),
        )
    });
    let (uniq, dup, absent) = &outcome.result;

    // Positive / negative / ambiguous are distinct candidate-set outcomes.
    assert!(matches!(uniq, rue_air::NameResolution::Unique(_)));
    assert!(matches!(dup, rue_air::NameResolution::Ambiguous(candidates) if candidates.len() == 2));
    assert_eq!(*absent, rue_air::NameResolution::Absent);

    // Differential: each provider result equals the production epoch's
    // canonical classification of the same lookup terminal.
    assert_eq!(
        *uniq,
        epoch_name_resolution(&request_lookup_name(
            &database,
            revision,
            &m,
            DefinitionNamespace::ModuleItem,
            "Uniq",
        ))
    );
    assert_eq!(
        *dup,
        epoch_name_resolution(&request_lookup_name(
            &database,
            revision,
            &m,
            DefinitionNamespace::ModuleItem,
            "dup",
        ))
    );

    // Visibility- and kind-filtered views are candidate SETS the caller
    // narrows locally: `Uniq` is public, `Hidden` is not.
    let hidden =
        database.probe_ready_body_facts(revision, semantic_configuration(), "vis", |provider| {
            provider.lookup_unqualified(&m, rue_air::ProviderNamespace::ModuleItem, "Hidden")
        });
    assert!(matches!(hidden.result, rue_air::NameResolution::Unique(_)));
    assert_eq!(hidden.result.visible(true), rue_air::NameResolution::Absent);
    assert_eq!(uniq.visible(true), *uniq);
    assert_eq!(
        uniq.of_kind(rue_air::ProviderDefinitionKind::Function),
        rue_air::NameResolution::Absent,
        "Uniq is a struct, not a function"
    );

    // Edge-recording proof: the provider recorded exactly a lookup-name edge
    // per consulted key.
    let names = recorded_family(&outcome.dependencies, "compiler.lookup-name");
    assert!(
        names.iter().any(|node| node.key().contains("Uniq")),
        "the Uniq lookup recorded its terminal edge: {:?}",
        outcome.dependencies
    );
    assert!(names.iter().any(|node| node.key().contains("dup")));
    assert!(names.iter().any(|node| node.key().contains("absent")));
    assert!(
        outcome
            .dependencies
            .iter()
            .all(|node| node.family() == "compiler.lookup-name"),
        "a name lookup observes only its lookup-name terminal: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_import_absence_matches_epoch_and_records_lookup_edge() {
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", "fn main() -> i32 { 0 }\n")], 1);
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let parse_revision = revision_for(&mut database, &snapshot);
    let graph = crate::test_support::test_import_graph(&snapshot).unwrap();
    database.adopt_test_import_graph_for_revision(parse_revision, graph);
    let revision = database.current_semantic_revision().unwrap();

    let outcome =
        database.probe_ready_body_facts(revision, semantic_configuration(), "import", |provider| {
            provider.resolve_import(&m, "missing.rue")
        });
    assert_eq!(outcome.result, rue_air::ImportResolution::Absent);

    // Edge-recording proof: only lookup-import edges, one per consulted path.
    assert!(
        outcome
            .dependencies
            .iter()
            .all(|node| node.family() == "compiler.lookup-import"),
        "import resolution observes only its lookup-import terminal: {:?}",
        outcome.dependencies
    );
    assert!(
        recorded_family(&outcome.dependencies, "compiler.lookup-import")
            .iter()
            .any(|node| node.key().contains("missing.rue")),
    );
}
