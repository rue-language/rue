use super::super::*;

/// ADR-0076 §1/§4: one append-only equality space per revision, shared by
/// every body of that revision, and retired once the revision leaves the
/// live window so a body carrying it fails its authority check.
#[test]
fn the_revision_symbol_space_is_one_generation_per_revision() {
    let space = RevisionSymbolSpace::default();
    let first = Revision::new(1, 0);

    let a = space.generation(first);
    let b = space.generation(first);
    assert!(Arc::ptr_eq(a.interner(), b.interner()));
    a.interner().get_or_intern("__anon_struct_00.member");
    assert_eq!(b.interner().len(), 1, "the space is shared, not copied");

    // A peer revision gets its own space and does not retire this one, so
    // two concurrently pinned revisions cannot abandon each other's bodies.
    let peer = space.generation(Revision::new(2, 0));
    assert!(!Arc::ptr_eq(a.interner(), peer.interner()));
    assert!(a.is_live());
    assert!(peer.is_live());
    assert_eq!(peer.interner().len(), 0);

    // Falling out of the live window retires it for every holder.
    for id in 3..=(RevisionSymbolSpace::WINDOW as u64 + 2) {
        space.generation(Revision::new(id, 0));
    }
    assert!(!a.is_live(), "the superseded generation fails closed");
    assert!(!b.is_live());
    assert_eq!(
        a.interner().len(),
        1,
        "a retired space still resolves the handles it issued"
    );
}

#[test]
fn canonical_rir_presentation_preserves_resource_limit_and_capacity_codes() {
    let cases = [
        (
            rue_rir::RirPayloadBuildError::ResourceLimitExceeded {
                family: "packed test",
            },
            rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT,
        ),
        (
            rue_rir::RirPayloadBuildError::CapacityFailure {
                family: "packed test",
            },
            rue_error::ErrorCode::COMPILER_RESOURCE_EXHAUSTION,
        ),
        (
            rue_rir::RirPayloadBuildError::InternerFailure {
                family: "packed test",
                kind: lasso::LassoErrorKind::FailedAllocation,
            },
            rue_error::ErrorCode::COMPILER_RESOURCE_EXHAUSTION,
        ),
    ];
    for (error, expected) in cases {
        let artifact_kind =
            crate::canonical_lower::rir_build_error_kind("packed candidate artifact", &error);
        let artifact = candidate_rir_artifact_failure_errors(&DeclarationBodyPlanFailure::Build(
            artifact_kind,
        ));
        assert_eq!(artifact.first().unwrap().kind.code(), expected);

        let composition = candidate_rir_composition_failure_error(
            &crate::canonical_lower::DeclarationBodyPlanBuildFailure::Build(error),
        );
        assert_eq!(composition.kind.code(), expected);
    }
}

#[test]
fn body_publication_three_callback_transaction_rolls_back_and_retries() {
    let closure_root = Arc::new(Mutex::new(PublishedBodyClosureRoot {
        additions: 7,
        deletions: 3,
        ..PublishedBodyClosureRoot::default()
    }));
    let reachability_root = Arc::new(Mutex::new(PublishedBodyReachabilityRoot::default()));
    let mut initial_lookup_lease = PublishedRootLookupLease {
        roots: BTreeMap::from([(
            "body:previous".to_owned(),
            RootLeaseEntry {
                observations: ObservedLookupRoot::new(),
                publication: 0,
            },
        )]),
        next_root_publication: 1,
        rederivations_after_eviction: 5,
        supersession_evictions: 2,
        ..PublishedRootLookupLease::default()
    };
    let existing = lookup_history_key("existing");
    initial_lookup_lease.record_incarnation(existing.clone(), 41);
    let lookup_lease = Arc::new(Mutex::new(initial_lookup_lease));
    let mut closure_handoff = PublishedBodyClosureTerminalHandoff {
        root: closure_root.clone(),
        pending: Some(Arc::new(rue_query::RetainedPinSet::new())),
        pending_reached: Some(BTreeSet::new()),
        previous: None,
        installed: false,
    };
    let mut reachability_handoff = PublishedBodyReachabilityTerminalHandoff {
        root: reachability_root,
        pending: Some(Arc::new(rue_query::RetainedPinSet::new())),
        previous: None,
        installed: false,
    };
    let mut lookup_handoff = PublishedBodyClosureLookupHandoff {
        lease: lookup_lease.clone(),
        runtime: QueryRuntime::new(1),
        observed: Some(BTreeMap::from([(
            "body:successor".to_owned(),
            ObservedLookupRoot {
                pins: rue_query::RetainedPinSet::new(),
                observed_keys: vec![(
                    LookupObservationKey::Name(LookupNameKey {
                        module: ModuleId::from_logical_path("main.rue").unwrap(),
                        namespace: DefinitionNamespace::ModuleItem,
                        name: "successor".into(),
                    }),
                    42,
                )],
            },
        )])),
        retire_absent: true,
        rollback: None,
    };

    rue_query::QueryAttemptHandoff::commit(&mut closure_handoff);
    rue_query::QueryAttemptHandoff::commit(&mut reachability_handoff);
    rue_query::QueryAttemptHandoff::commit(&mut lookup_handoff);
    {
        let lease = lookup_lease.lock().unwrap();
        assert!(lease.roots.contains_key("body:successor"));
        assert!(!lease.roots.contains_key("body:previous"));
    }

    // This is the callback order used when cancellation is observed after
    // the final publication callback, and equally models the attempted
    // prefix unwound after a later callback panic.
    rue_query::QueryAttemptHandoff::abort(&mut lookup_handoff);
    rue_query::QueryAttemptHandoff::abort(&mut reachability_handoff);
    rue_query::QueryAttemptHandoff::abort(&mut closure_handoff);
    {
        let closure = closure_root.lock().unwrap();
        assert_eq!(closure.additions, 7);
        assert_eq!(closure.deletions, 3);
        let lease = lookup_lease.lock().unwrap();
        assert!(lease.roots.contains_key("body:previous"));
        assert!(!lease.roots.contains_key("body:successor"));
        assert_eq!(lease.rederivations_after_eviction, 5);
        assert_eq!(lease.supersession_evictions, 2);
        assert_eq!(lease.next_root_publication, 1);
        assert_eq!(lease.next_incarnation_generation, 1);
        assert_eq!(lease.seen_incarnation(&existing), Some(41));
        assert_eq!(lease.incarnations.len(), 1);
    }

    rue_query::QueryAttemptHandoff::commit(&mut closure_handoff);
    rue_query::QueryAttemptHandoff::commit(&mut reachability_handoff);
    rue_query::QueryAttemptHandoff::commit(&mut lookup_handoff);
    assert!(closure_handoff.installed);
    assert!(reachability_handoff.installed);
    assert!(
        lookup_lease
            .lock()
            .unwrap()
            .roots
            .contains_key("body:successor")
    );
}

#[test]
fn compiler_provider_fatal_status_dominates_incomplete_status() {
    let incomplete =
        CompilerBodyProviderStatus::Incomplete(CompilerBodyProviderIncomplete::Canceled);
    let missing = CompilerBodyProviderStatus::Incomplete(
        CompilerBodyProviderIncomplete::MissingInput(InputIdentity::new("body", "signature")),
    );
    let fatal = CompilerBodyProviderStatus::Fatal(QueryAbort::ForeignRuntime);

    assert!(provider_status_should_replace(&incomplete, &fatal));
    assert!(!provider_status_should_replace(&fatal, &missing));
    assert!(!provider_status_should_replace(&incomplete, &missing));
}

#[test]
fn production_provider_boundary_uses_owned_handles_and_shared_rir_view() {
    let compiler = include_str!("../../provider.rs");
    let runtime = crate::revisioned_query_database::REVISIONED_DATABASE_SOURCE;
    let query_start = compiler
        .find("pub(crate) struct CompilerBodyProviderQueries")
        .unwrap();
    let query_end = compiler[query_start..]
        .find("\n}\n\n#[allow(dead_code)]\nimpl<'a> CompilerBodyProviderQueries")
        .map(|offset| query_start + offset + 2)
        .unwrap();
    let query_fields = &compiler[query_start..query_end];
    assert!(query_fields.contains("QueryFamily"));
    for banned in [
        "RevisionedQueryDatabase",
        "CanonicalMergedProgram",
        "CanonicalRirOutput",
        "declaration_manifest",
        "reachability",
        "InstRef",
        "Spur",
        "Span",
        "FileId",
        "ProviderIdentityContext::new",
    ] {
        assert!(
            !query_fields.contains(banned),
            "provider query bundle retains banned boundary artifact `{banned}`"
        );
    }

    let provider_start = compiler
        .find("pub(crate) struct CompilerBodyFactProvider")
        .unwrap();
    let provider_end = compiler[provider_start..]
        .find("\n}\n\n#[allow(dead_code)]\nimpl<'a> CompilerBodyFactProvider")
        .map(|offset| provider_start + offset + 2)
        .unwrap();
    assert!(!compiler[provider_start..provider_end].contains("RevisionedQueryDatabase"));

    let type_start = compiler
        .find("pub(crate) struct ProviderTypeFacts")
        .unwrap();
    let type_end = compiler[type_start..]
        .find("impl<'p, 'o, 'db> rue_air::SemanticModulePathProvider")
        .map(|offset| type_start + offset)
        .unwrap();
    let type_slice = &compiler[type_start..type_end];
    assert!(type_slice.contains("materialize_nominal"));
    assert!(!type_slice.contains("intern_definition"));
    assert!(!type_slice.contains("intern_module"));
    assert!(!type_slice.contains("ProviderIdentityContext::new"));

    let test_module = runtime
        .find("\n#[cfg(test)]\nmod tests")
        .expect("revisioned-query tests have a cfg boundary");
    assert!(
        !runtime[..test_module].contains("BodyRirView::from_parts"),
        "production provider construction must obtain its view from BodyRirBundle::view"
    );
    let lower = include_str!("../../../canonical_lower.rs");
    assert!(lower.contains("BodyRirBundle::new_with_index_attribution"));
    assert!(
        lower.contains("materialize_body_rir_bundle_with_declaration"),
        "the packed candidate materializer owns the request-local bundle"
    );
}

fn main_body_key() -> crate::body_query::BodyQueryKey {
    crate::body_query::BodyQueryKey::new(
        free_function_instance(&ModuleId::from_logical_path("main.rue").unwrap(), "main"),
        semantic_configuration(),
    )
}

#[test]
fn malformed_exact_option_fails_body_request_without_compute_or_publication() {
    let snapshot = trusted_option_body_snapshot(
        r#"
fn LocalOption(comptime T: type) -> type { enum { Some(T), None } }
fn main() -> i32 {
    let L = LocalOption(i32);
    let _lookalike: L = L.None;
    let _result = @parse_i32("1");
    0
}
"#,
        "pub fn Option(comptime T: type) -> type { missing }",
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let key = main_body_key();
    let result = database.body_transaction(revision, key.clone(), CancellationToken::new());

    assert!(matches!(
        result,
        Err(BodyTransactionRequestFailure::WellKnownOptionResolution(
            WellKnownOptionResolutionFailure::Semantic {
                payload: crate::well_known_option::FalliblePayload::I32,
                ..
            }
        ))
    ));
    assert!(database.has_retained_body_key(&key));
    assert!(database.any_body_transaction_terminal());
}

#[test]
fn missing_trusted_option_is_typed_incomplete_without_body_publication() {
    let snapshot = trusted_body_snapshot(
        r#"fn main() -> i32 { let _result = @parse_i32("1"); 0 }"#,
        None,
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let key = main_body_key();
    let result = database.body_transaction(revision, key.clone(), CancellationToken::new());

    assert!(matches!(
        result,
        Err(BodyTransactionRequestFailure::WellKnownOptionResolution(
            WellKnownOptionResolutionFailure::Incomplete {
                payload: crate::well_known_option::FalliblePayload::I32,
                prerequisite: Some(ref key),
                ..
            }
        )) if key.module().as_str() == crate::OPTION_MODULE_LOGICAL_PATH
    ));
    assert!(database.has_retained_body_key(&key));
    assert!(database.any_body_transaction_terminal());
}

#[test]
fn body_transaction_owns_candidate_artifact_through_canonical_projections() {
    let first = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 1 }")], 1);
    let second = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 2 }")], 1);
    let mut database = RevisionedQueryDatabase::default();
    let key = main_body_key();
    let first_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&first), &first);
    let first_transaction = database
        .runtime
        .request_registered(
            &database.body_transactions,
            first_revision,
            key.clone(),
            CancellationToken::new(),
        )
        .into_result()
        .expect("the first body transaction succeeds");
    let first_demand = database
        .runtime
        .request_registered(
            &database.body_toolchain_demands,
            first_revision,
            key.clone(),
            CancellationToken::new(),
        )
        .into_result()
        .expect("the first toolchain projection succeeds");
    let rue_query::QueryOutcome::Success(first_demand_value) = first_demand.outcome() else {
        unreachable!("BodyToolchainDemand publishes typed values")
    };
    assert!(first_demand_value.source_candidate_available());

    let transaction_dependencies = first_transaction
        .dependencies()
        .iter()
        .map(|observation| observation.node.family())
        .collect::<BTreeSet<_>>();
    assert!(
        transaction_dependencies.contains("compiler.body-toolchain-demands"),
        "the canonical demand projection owns source-candidate availability: {transaction_dependencies:?}"
    );
    assert!(
        transaction_dependencies.contains("compiler.declaration-body-plan-artifacts")
            && transaction_dependencies.contains("compiler.body-source-basis"),
        "the transaction directly observes the selected plan and current basis: {transaction_dependencies:?}"
    );
    assert!(!transaction_dependencies.contains(concat!("compiler.body-", "input")));
    assert!(
        !transaction_dependencies.contains("compiler.raw-declaration-body"),
        "the body transaction must not add a peer raw-body edge: {transaction_dependencies:?}"
    );
    assert!(
        first_demand
            .dependencies()
            .iter()
            .any(|observation| observation.node.family()
                == "compiler.declaration-body-plan-artifacts"),
        "the demand projection must retain its exact candidate-artifact edge"
    );
    assert!(
        !first_demand
            .dependencies()
            .iter()
            .any(|observation| observation.node.family() == "compiler.raw-declaration-body"),
        "the demand projection must not retain a duplicate raw-body edge"
    );

    let second_revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&second), &second);
    let second_demand = database
        .runtime
        .request_registered(
            &database.body_toolchain_demands,
            second_revision,
            key.clone(),
            CancellationToken::new(),
        )
        .into_result()
        .expect("the second toolchain projection succeeds");
    let second_transaction = database
        .runtime
        .request_registered(
            &database.body_transactions,
            second_revision,
            key,
            CancellationToken::new(),
        )
        .into_result()
        .expect("the second body transaction succeeds");
    assert_eq!(
        first_demand.stamp(),
        second_demand.stamp(),
        "an equal no-demand projection stays green across a body-only edit"
    );
    assert_ne!(
        first_transaction.stamp(),
        second_transaction.stamp(),
        "the canonical body-input edge still invalidates semantic analysis"
    );
}

#[test]
fn toolchain_demand_uses_typed_artifact_intrinsics_not_source_mentions() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            r#"
fn main() -> i32 {
    // @parse_i64 is only a comment.
    let _spelling = "@parse_i32";
    let _ordinary_intrinsic = @to_string(1);
    0
}
"#,
        )],
        1,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let terminal = database
        .body_toolchain_demands(revision, main_body_key(), CancellationToken::new())
        .expect("the typed artifact demand projection succeeds");
    let rue_query::QueryOutcome::Success(demand) = terminal.outcome() else {
        panic!("BodyToolchainDemand publishes a typed value")
    };

    assert!(demand.source_candidate_available());
    assert!(demand.payload_kinds().is_empty());
    assert!(demand.modules().is_empty());
    assert!(
        terminal
            .dependencies()
            .iter()
            .all(|dependency| dependency.node.family() != "compiler.raw-declaration-body")
    );
}

#[test]
fn toolchain_demand_maps_all_five_artifact_kinds_across_nested_method() {
    let body = r#"{
    let _i32 = @parse_i32("1");
    let _i64 = @parse_i64("1");
    let _u32 = @parse_u32("1");
    let _u64 = @parse_u64("1");
    let _duplicate = @parse_i32("2");
    struct {
        fn nested() -> i32 {
            let _nested_duplicate = @parse_i64("3");
            let _nested_only = @read_line();
            0
        }
    }
}"#;
    let source_text = format!("fn Box() -> type {body}");
    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", &source_text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, "Box"),
        semantic_configuration(),
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let terminal = database
        .body_toolchain_demands(revision, key, CancellationToken::new())
        .expect("the all-kind artifact demand projection succeeds");
    let rue_query::QueryOutcome::Success(demand) = terminal.outcome() else {
        panic!("BodyToolchainDemand publishes a typed value")
    };
    let artifact_kinds = demand
        .payload_kinds()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let lexical_oracle = crate::well_known_option::scan_body_payload_kinds(body);

    assert!(demand.source_candidate_available());
    assert_eq!(artifact_kinds, lexical_oracle);
    assert_eq!(artifact_kinds.len(), 5);
    assert!(artifact_kinds.contains(&crate::well_known_option::FalliblePayload::StrBuf));
    assert_eq!(demand.modules().len(), 2);
}

#[test]
fn rooted_runtime_and_comptime_use_candidate_artifacts() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::DurableType;
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, DeclarationSemanticQueryKey, SemanticNucleusKey,
    };

    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Make(comptime T: type) -> type { struct { value: T } }\n\
                 fn main() -> i32 { 0 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "main")]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("representative rooted runtime body closes");
    let candidate = declaration_candidate(&database, revision, &module, Category::Function, "Make");
    let _ = request_semantic_nucleus_observed(
        &database,
        revision,
        SemanticNucleusKey::ComptimeCall(ComptimeCallQueryKey {
            declaration: DeclarationSemanticQueryKey {
                declaration: candidate,
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([(Arc::from("T"), DurableType::I32)]),
            value_arguments: Arc::from([]),
        }),
    );
}

#[test]
fn cold_signature_only_demand_does_no_candidate_astgen_work() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn selected(value: i32) -> i32 { value }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let candidate = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
        name: Arc::from("selected"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let (requested, _) = request_semantic_nucleus_observed(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: candidate,
                configuration: semantic_configuration(),
            },
        ),
    );
    assert!(matches!(
        requested,
        crate::semantic_query_nucleus::SemanticNucleusValue::Signature(_)
    ));
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        database
            .declaration_body_plan_artifacts
            .retention()
            .terminals,
        0
    );
}

#[test]
fn artifact_failure_is_candidate_available_and_reaches_exact_body_failure() {
    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 1 }")], 1);
    let key = main_body_key();
    let crate::FunctionInstanceKey::Definition(definition) = &key.instance else {
        unreachable!()
    };
    let errors = crate::CompileErrors::from(crate::CompileError::without_span(
        rue_error::ErrorKind::InvalidCompilerInput("injected packed artifact failure".into()),
    ));
    let mut database = RevisionedQueryDatabase::default();
    database.inject_declaration_body_plan_failure_for_test(definition, errors.clone());
    let revision = revision_for(&mut database, &snapshot);
    let demand = database
        .body_toolchain_demands(revision, key.clone(), CancellationToken::new())
        .expect("artifact failure still publishes an empty demand projection");
    let rue_query::QueryOutcome::Success(demand) = demand.outcome() else {
        panic!("BodyToolchainDemand publishes a typed value")
    };
    assert!(demand.source_candidate_available());
    assert!(demand.payload_kinds().is_empty());
    assert!(demand.modules().is_empty());

    let transaction = database
        .body_transaction(revision, key, CancellationToken::new())
        .expect("artifact failure publishes a deterministic body transaction");
    let rue_query::QueryOutcome::Success(
        crate::body_query::BodyTransaction::DeterministicFailure {
            errors: published, ..
        },
    ) = transaction.outcome()
    else {
        panic!("artifact failure reaches the exact typed body-plan failure")
    };
    assert_eq!(published, &errors);
}

#[test]
fn concurrent_body_deferral_classification_is_atomic_and_not_cancellation() {
    let snapshot = trusted_body_snapshot(
        r#"fn main() -> i32 { let _result = @parse_i32("1"); 0 }"#,
        None,
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let database = Arc::new(database);
    let key = main_body_key();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let outcomes = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                let database = database.clone();
                let key = key.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    database.body_transaction(revision, key, CancellationToken::new())
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("body request thread did not panic"))
            .collect::<Vec<_>>()
    });
    assert!(
        outcomes.iter().all(|outcome| matches!(
            outcome,
            Err(BodyTransactionRequestFailure::WellKnownOptionResolution(
                WellKnownOptionResolutionFailure::Incomplete {
                    payload: crate::well_known_option::FalliblePayload::I32,
                    ..
                }
            ))
        )),
        "{outcomes:?}"
    );
}

#[test]
fn canceled_production_body_attempt_commits_no_lookup_handoff() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }\n",
        )],
        1,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        database.body_transaction(revision, main_body_key(), cancellation),
        Err(BodyTransactionRequestFailure::Query(QueryAbort::Canceled))
    ));
    let metrics = database.lookup_pressure_metrics();
    assert_eq!(metrics.published_roots, 0, "{metrics:?}");
    assert_eq!(metrics.leased_terminals, 0, "{metrics:?}");
}

#[test]
fn green_body_refreshes_equal_lookup_to_fresh_incarnation() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::with_declaration_memo_retention(4);
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let key = main_body_key();
    let first = database
        .body_transaction(revision, key.clone(), CancellationToken::new())
        .unwrap();
    let rue_query::QueryOutcome::Success(first_value) = first.outcome() else {
        unreachable!()
    };
    let descriptors = first_value.lookup_observations().unwrap();
    let helper_incarnation = descriptors
        .terminals
        .iter()
        .find_map(|(descriptor, incarnation)| match descriptor {
            LookupObservationKey::Name(name) if name.name.as_ref() == "helper" => {
                Some(*incarnation)
            }
            _ => None,
        })
        .expect("main observes helper lookup");

    // Release the body root, then exceed the lookup family's historical
    // floor so the old helper node can retire while the body memo remains.
    database
        .promote_published_lookup_root(body_lookup_root_identity(&key), ObservedLookupRoot::new());
    for slot in 0..32 {
        let _ = lookup_incarnation(&database, revision, &module, &format!("pressure_{slot}"));
    }
    let fresh_incarnation = lookup_incarnation(&database, revision, &module, "helper");
    assert_ne!(
        fresh_incarnation, helper_incarnation,
        "pressure must produce a fresh logical lookup incarnation"
    );

    let reused = database
        .body_transaction(revision, key.clone(), CancellationToken::new())
        .unwrap();
    assert_eq!(
        reused.stamp(),
        first.stamp(),
        "equal lookup output keeps semantic body equality green"
    );
    let lease = database
        .lookup_root_lease
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = lease
        .roots
        .get(&body_lookup_root_identity(&key))
        .expect("green publication restores the body root");
    assert!(
        root.observations
            .observed_keys
            .iter()
            .any(|(descriptor, incarnation)| matches!(
                descriptor,
                LookupObservationKey::Name(name)
                    if name.name.as_ref() == "helper"
                        && *incarnation == fresh_incarnation
            )),
        "green publication must own the current helper terminal"
    );
}

#[test]
fn deterministic_failure_references_keep_selected_and_exclude_rejected_candidate() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 }\n\
                 struct Wrong {}\n\
                 fn main() -> i32 { let _value = helper(); Wrong() }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let helper = free_function_instance(&module, "helper");
    let wrong = crate::StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Type,
        crate::StableDefinitionKind::Struct,
        Arc::from("Wrong"),
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let terminal = database
        .body_transaction(revision, main_body_key(), CancellationToken::new())
        .unwrap();
    let rue_query::QueryOutcome::Success(
        crate::body_query::BodyTransaction::DeterministicFailure { references, .. },
    ) = terminal.outcome()
    else {
        panic!("wrong-kind call must publish a deterministic body failure");
    };
    assert!(
        references
            .0
            .contains(&crate::body_query::BodyReference::Callable(helper)),
        "the semantically selected helper call remains a positive reference"
    );
    assert!(
        !references.0.iter().any(|reference| matches!(
            reference,
            crate::body_query::BodyReference::Definition(key) if key == &wrong
        ) || matches!(
            reference,
            crate::body_query::BodyReference::Type(crate::TypeInstanceKey::Nominal(
                crate::NominalInstanceKey::Named(key)
            )) if key == &wrong
        )),
        "the wrong-kind lookup candidate is a dependency, not a reachability reference"
    );
}

#[test]
fn missing_trusted_strbuf_is_typed_incomplete_without_body_publication() {
    let snapshot = trusted_body_snapshot(
        r#"fn main() -> i32 { let _result = @read_line(); 0 }"#,
        Some((
            FileId::new(2),
            "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }",
        )),
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let (_, unchecked_call) = crate::well_known_option::exact_option_query(
        crate::well_known_option::FalliblePayload::StrBuf,
        &semantic_configuration(),
    );
    let unchecked = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(unchecked_call),
        CancellationToken::new(),
    );
    assert!(
        matches!(
            unchecked.terminal().map(|terminal| terminal.outcome()),
            Some(rue_query::QueryOutcome::Success(
                crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(_)
            ))
        ),
        "the raw comptime call currently accepts an unverified durable StrBuf key; the exact \
             stable-classification preflight must remain authoritative: {:?}",
        unchecked.abort()
    );
    let key = main_body_key();
    let result = database.body_transaction(revision, key.clone(), CancellationToken::new());

    assert!(matches!(
        result,
        Err(BodyTransactionRequestFailure::WellKnownOptionResolution(
            WellKnownOptionResolutionFailure::Incomplete {
                payload: crate::well_known_option::FalliblePayload::StrBuf,
                prerequisite: Some(ref key),
                ..
            }
        )) if key.module().as_str() == crate::STRBUF_MODULE_LOGICAL_PATH
    ));
    assert!(database.has_retained_body_key(&key));
    assert!(database.any_body_transaction_terminal());
}

#[test]
fn wrong_exact_option_projection_fails_body_request_atomically() {
    let snapshot = trusted_option_body_snapshot(
        r#"fn main() -> i32 { let _result = @parse_u32("1"); 0 }"#,
        "pub fn Option(comptime T: type) -> i32 { 0 }",
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&snapshot), &snapshot);
    let key = main_body_key();
    let result = database.body_transaction(revision, key.clone(), CancellationToken::new());

    assert!(matches!(
        result,
        Err(BodyTransactionRequestFailure::WellKnownOptionResolution(
            WellKnownOptionResolutionFailure::WrongProjection {
                payload: crate::well_known_option::FalliblePayload::U32,
                ..
            }
        ))
    ));
    assert!(database.has_retained_body_key(&key));
    assert!(database.any_body_transaction_terminal());
}

#[test]
fn well_known_dependency_abort_classification_is_exhaustive() {
    assert_eq!(
        classify_well_known_dependency_abort(&QueryAbort::Canceled),
        WellKnownDependencyAbortClass::Incomplete
    );
    assert_eq!(
        classify_well_known_dependency_abort(&QueryAbort::MissingInput(InputIdentity::new(
            "well-known-test",
            "missing",
        ))),
        WellKnownDependencyAbortClass::Incomplete
    );
    assert_eq!(
        classify_well_known_dependency_abort(&QueryAbort::ForeignRuntime),
        WellKnownDependencyAbortClass::Propagate
    );
    assert_eq!(
        classify_well_known_dependency_abort(&QueryAbort::Cycle(Arc::from([]))),
        WellKnownDependencyAbortClass::Propagate
    );
    assert_eq!(
        classify_well_known_dependency_abort(&QueryAbort::UnpublishedRevision(Revision::new(
            42, 1,
        ))),
        WellKnownDependencyAbortClass::Propagate
    );
}

#[test]
fn warning_body_projection_is_candidate_exact_and_fail_closed() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let mut text = String::new();
    for index in 0..128 {
        text.push_str(&format!("fn unrelated_{index}() -> i32 {{ 0 }}\n"));
    }
    text.push_str(
        "fn target_free() -> i32 {\n\
                 let local = 0;\n\
                 target_direct();\n\
                 local();\n\
                 let helper = @import(\"helper.rue\");\n\
                 helper.target_imported()\n\
             }\n",
    );
    text.push_str(
        "fn target_nested() -> type {\n\
                 struct { fn hidden() -> i32 { nested_direct() } }\n\
             }\n",
    );
    text.push_str("fn target_type(value: Factory(i32)) -> Result(i32) { value }\n");
    text.push_str("@copy struct Bag { value: i32,\n");
    for index in 0..128 {
        text.push_str(&format!("fn unrelated_member_{index}() -> i32 {{ 0 }}\n"));
    }
    text.push_str("fn target_associated() -> i32 { target_associated_direct() }\n");
    text.push_str(
        "fn target_method(borrow self) -> i32 { target_method_direct(); self.value }\n}\n",
    );
    text.push_str("drop fn Bag(self) { target_drop_direct(); }\n");

    let source = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let parsed = database.runtime.request_registered(
        &database.parse_modules,
        revision,
        ModuleQueryKey(module.clone()),
        CancellationToken::new(),
    );
    let parsed = match parsed.terminal().unwrap().outcome() {
        rue_query::QueryOutcome::Success(ParseModuleValue {
            result: Ok(parsed), ..
        }) => parsed,
        other => panic!("expected parsed module, got {other:?}"),
    };
    for (category, name, expected) in [
        (
            Category::Function,
            "target_free",
            vec![
                (None, vec!["target_direct"]),
                (Some("helper.rue"), vec!["target_imported"]),
            ],
        ),
        (
            Category::AssociatedFunction,
            "target_associated",
            vec![(None, vec!["target_associated_direct"])],
        ),
        (
            Category::Method,
            "target_method",
            vec![(None, vec!["target_method_direct"])],
        ),
        (
            Category::Destructor,
            "Bag",
            vec![(None, vec!["target_drop_direct"])],
        ),
        (
            Category::Function,
            "target_nested",
            vec![(None, vec!["nested_direct"])],
        ),
        (
            Category::Function,
            "target_type",
            vec![(None, vec!["Factory"]), (None, vec!["Result"])],
        ),
    ] {
        let candidate = declaration_candidate(&database, revision, &module, category, name);
        let projected = parsed
            .declaration_warning_call_heads(&candidate)
            .unwrap_or_else(|| panic!("{category:?} has no parser-owned warning projection"));
        let actual = projected
            .iter()
            .map(|head| {
                (
                    head.import.as_ref().map(|import| import.specifier.as_ref()),
                    head.components
                        .iter()
                        .map(|component| component.as_ref())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{category:?} projected wrong call heads");
    }

    let mut mismatched = declaration_candidate(
        &database,
        revision,
        &module,
        Category::Function,
        "target_free",
    );
    mismatched.name = Arc::from("not-target-free");
    assert!(
        parsed.declaration_warning_call_heads(&mismatched).is_none(),
        "a mismatched exact key must fail closed"
    );
}

#[test]
fn transient_body_resolver_uses_canonical_plan_and_current_basis_without_reparse() {
    let source = source_snapshot(
        &[(7, "/main.rue", "main.rue", "fn selected() -> i32 { 7 }\n")],
        7,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, "selected"),
        semantic_configuration(),
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let terminal = database
        .body_input(revision, key, CancellationToken::new())
        .expect("body input request completes");
    let rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Available(input)) =
        terminal.outcome()
    else {
        panic!("ordinary body plan is available: {terminal:?}");
    };
    assert!(input.artifacts.plan.instruction_count() > 0);
    assert_eq!(input.source.file_id, FileId::new(7));
    let bundle = input
        .artifacts
        .plan
        .materialize_body_rir_bundle(
            &rue_rir::SharedSymbolSpace::private(),
            input.source.file_id,
            input.source.declaration_start,
            input.source.source_length,
            || Ok(()),
        )
        .expect("canonical body plan projects to current source");
    assert_eq!(bundle.source_file_id(), Some(FileId::new(7)));

    let runtime = crate::revisioned_query_database::REVISIONED_DATABASE_SOURCE;
    assert!(!runtime.contains(concat!("fn lower_owned_", "body_input(")));
    assert!(!runtime.contains(concat!("struct OwnedBody", "Lowering")));
    assert!(!runtime.contains(concat!("\"compiler.", "body-input\"")));
    assert!(!runtime.contains("\"compiler.body-source-locator\""));
    let body_source_basis = include_str!("../../registrations/body/body_source_bases.rs");
    assert_eq!(
        body_source_basis
            .matches("\"compiler.body-source-basis\",\n")
            .count(),
        1
    );
    let input_start = runtime.find("impl BodyInputResolver").unwrap();
    let input_end = runtime[input_start..]
        .find("struct BodyTransactionEvaluator")
        .map(|offset| input_start + offset)
        .unwrap();
    let input_source = &runtime[input_start..input_end];
    for forbidden in [
        "parse_source_snapshot_module",
        "lower_module_rir_with_work",
        "RawDeclarationBodyQueryKey",
        "RawDeclarationBodyQueryValue",
        "AstGen",
    ] {
        assert!(
            !input_source.contains(forbidden),
            "body-input cutover retained old frontend work: {forbidden}"
        );
    }
    for required in [
        "DeclarationBodyPlanArtifactsValue",
        "self.declaration_body_plan_artifacts",
        "self.body_source_bases",
    ] {
        assert!(
            input_source.contains(required),
            "body-input cutover lost canonical plan edge: {required}"
        );
    }

    let transaction_start = runtime
        .find("fn evaluate(\n        &self,\n        context: &rue_query::QueryContext")
        .unwrap();
    let transaction_end = runtime[transaction_start..]
        .find("\n}\n\nimpl RevisionedQueryDatabase")
        .map(|offset| transaction_start + offset)
        .unwrap();
    let transaction = &runtime[transaction_start..transaction_end];
    for forbidden in [
        "parse_source_snapshot_module",
        "lower_module_rir",
        "SourceSnapshot::new",
        "SourceSnapshot::single",
        "AstGen",
    ] {
        assert!(
            !transaction.contains(forbidden),
            "body transaction retained a peer frontend path: {forbidden}"
        );
    }
    assert_eq!(
        transaction
            .match_indices("materialize_body_rir_bundle_with_attribution")
            .count(),
        2,
        "ordinary and specialization arms must be the only plan materializers",
    );
}

#[test]
fn test_probe_specializations_share_one_candidate_plan_arc() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn selected(comptime N: i32) -> i32 { N }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let crate::FunctionInstanceKey::Definition(base) = free_function_instance(&module, "selected")
    else {
        unreachable!()
    };
    let specialization = |value| crate::FunctionInstanceKey::Specialization {
        base: Node::new(crate::FunctionInstanceKey::Definition(base.clone())),
        arguments: crate::CanonicalArguments {
            types: Arc::from([]),
            values: Arc::from([crate::CanonicalArgumentValue::Integer(value)]),
        },
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let input = |database: &RevisionedQueryDatabase, value| {
        let terminal = database
            .body_input(
                revision,
                crate::body_query::BodyQueryKey::new(
                    specialization(value),
                    semantic_configuration(),
                ),
                CancellationToken::new(),
            )
            .unwrap();
        let rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Available(input)) =
            terminal.outcome()
        else {
            panic!("specialization body plan unavailable: {terminal:?}");
        };
        input.clone()
    };
    let first = input(&database, 1);
    let second = input(&database, 2);
    assert!(Arc::ptr_eq(&first.artifacts, &second.artifacts));
}

#[test]
fn rooted_specializations_observe_one_candidate_artifact_incarnation_and_astgen() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn selected(comptime N: i32) -> i32 { N }\n\
                 fn main() -> i32 { selected(1) + selected(2) }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let closure = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "main")]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("rooted specialization closure completes");
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        panic!("body closure publishes a typed value")
    };
    let specializations = output
        .reached
        .iter()
        .filter(|instance| {
            matches!(
                instance,
                crate::FunctionInstanceKey::Specialization { base, .. }
                    if matches!(base.as_ref(),
                        crate::FunctionInstanceKey::Definition(definition)
                            if definition.name() == "selected")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(specializations.len(), 2);

    let artifact_dependencies = specializations
        .into_iter()
        .map(|instance| {
            let transaction = database
                .body_transaction(
                    revision,
                    crate::body_query::BodyQueryKey::new(instance, semantic_configuration()),
                    CancellationToken::new(),
                )
                .expect("specialization transaction remains retained");
            transaction
                .dependencies()
                .iter()
                .find(|dependency| {
                    dependency.node.family() == "compiler.declaration-body-plan-artifacts"
                })
                .map(|dependency| (dependency.incarnation, dependency.stamp))
                .expect("transaction directly observes its candidate artifact")
        })
        .collect::<Vec<_>>();
    assert_eq!(artifact_dependencies[0], artifact_dependencies[1]);
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        2,
        "main and selected each lower once; the second specialization reuses selected"
    );
}

#[test]
fn sibling_only_edit_keeps_artifact_transaction_and_downstream_green() {
    let source = |sibling| {
        let text = format!("fn sibling() -> i32 {{ {sibling} }}\nfn chosen() -> i32 {{ 7 }}\n");
        source_snapshot(&[(1, "/main.rue", "main.rue", text.as_str())], 1)
    };
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, "chosen"),
        semantic_configuration(),
    );
    let closure_key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "chosen")]),
        configuration: semantic_configuration(),
    };
    let downstream_stamps =
        |database: &RevisionedQueryDatabase,
         revision,
         key: &crate::body_query::BodyQueryKey,
         input: &Arc<rue_query::QueryTerminal<crate::body_query::BodyInputValue>>,
         transaction: &Arc<rue_query::QueryTerminal<crate::body_query::BodyTransaction>>| {
            let rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Available(
                input,
            )) = input.outcome()
            else {
                panic!("selected body input is available")
            };
            let rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success {
                body,
                ..
            }) = transaction.outcome()
            else {
                panic!("selected body transaction succeeds")
            };
            let function = key.instance.clone();
            let owner = match body.as_ref() {
                crate::body_query::CanonicalBody::Ordinary { owner, .. } => owner,
                _ => panic!("selected free function has an ordinary canonical body"),
            };
            let facts = crate::local_semantic_materialization::LocalMaterializationFacts {
                declarations: Arc::from([]),
                anonymous_nominals: Arc::from([]),
                callables: Arc::from([crate::local_semantic_materialization::LocalCallableFact {
                    identity: function.clone(),
                    symbol: Arc::from(owner.name()),
                }]),
                nominal_metadata: Arc::from([]),
                modules: Arc::from([owner.module().clone()]),
                builtin_nominals: Arc::from([]),
                required_types: Arc::from([]),
                indexes: Arc::new(
                    crate::local_semantic_materialization::LocalMaterializationIndexes::default(),
                ),
            };
            let body_span = rue_span::Span::with_file(
                input.source.file_id,
                input.source.body_start,
                input.source.body_end,
            );
            let cfg_key = crate::cfg_query::CfgQueryKey::new(
                function.clone(),
                semantic_configuration(),
                crate::cfg_query::CfgSemanticInput::Body {
                    input: Arc::new(crate::cfg_query::CfgBodyInput {
                        function,
                        canonical: body.clone(),
                        body_span,
                        #[cfg(test)]
                        interner_limit: None,
                        #[cfg(test)]
                        force_failure: false,
                    }),
                    materialization: Arc::new(facts),
                },
            );
            let cfg = database
                .runtime
                .request_registered(
                    &database.cfgs,
                    revision,
                    cfg_key.clone(),
                    CancellationToken::new(),
                )
                .into_result()
                .expect("CFG completes");
            let (optimized_key, optimized_attempt) = database
                .optimized_cfg(
                    revision,
                    cfg_key,
                    rue_cfg::OptLevel::O1,
                    Arc::from([]),
                    CancellationToken::new(),
                )
                .expect("optimized CFG request is accepted");
            let optimized = optimized_attempt
                .into_result()
                .expect("optimized CFG completes");
            let codegen = database
                .codegen_unit(
                    revision,
                    optimized_key,
                    rue_target::Target::X86_64Linux,
                    rue_codegen::BackendArtifactRequest::default(),
                    rue_cfg::OptLevel::O1,
                    CancellationToken::new(),
                )
                .expect("codegen request is accepted")
                .into_result()
                .expect("codegen completes");
            (cfg.stamp(), optimized.stamp(), codegen.stamp())
        };
    let mut database = RevisionedQueryDatabase::default();
    let first_source = source("1");
    let first_revision = revision_for(&mut database, &first_source);
    database
        .body_closure(
            first_revision,
            closure_key.clone(),
            CancellationToken::new(),
        )
        .expect("first selected closure publishes");
    let candidate = declaration_candidate(
        &database,
        first_revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::Function,
        "chosen",
    );
    let first_artifact = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        first_revision,
        DeclarationBodyPlanQueryKey(candidate.clone()),
        CancellationToken::new(),
    );
    let first_artifact_stamp = first_artifact
        .terminal()
        .expect("first candidate artifact publishes")
        .stamp();
    let first_input = database
        .body_input(first_revision, key.clone(), CancellationToken::new())
        .unwrap();
    let first_transaction = database
        .body_transaction(first_revision, key.clone(), CancellationToken::new())
        .expect("first chosen body transaction completes");
    let first_downstream = downstream_stamps(
        &database,
        first_revision,
        &key,
        &first_input,
        &first_transaction,
    );

    let second_source = source("123456");
    let second_revision = revision_for(&mut database, &second_source);
    database
        .body_closure(
            second_revision,
            closure_key.clone(),
            CancellationToken::new(),
        )
        .expect("second selected closure publishes");
    let second_artifact = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        second_revision,
        DeclarationBodyPlanQueryKey(candidate),
        CancellationToken::new(),
    );
    let second_artifact_stamp = second_artifact
        .terminal()
        .expect("second candidate artifact publishes")
        .stamp();
    let second_input = database
        .body_input(second_revision, key.clone(), CancellationToken::new())
        .unwrap();
    let second_transaction = database
        .body_transaction(second_revision, key.clone(), CancellationToken::new())
        .expect("shifted chosen body transaction completes");
    let second_downstream = downstream_stamps(
        &database,
        second_revision,
        &key,
        &second_input,
        &second_transaction,
    );

    assert_eq!(first_artifact_stamp, second_artifact_stamp);
    assert_eq!(first_transaction.stamp(), second_transaction.stamp());
    assert_eq!(first_downstream, second_downstream);

    for sibling in [
        "2000000", "3000000", "4000000", "5000000", "6000000", "7000000", "8000000", "9000000",
        "10000000", "11000000", "12000000", "13000000", "14000000", "15000000", "16000000",
        "17000000", "18000000", "19000000",
    ] {
        let source = source(sibling);
        let revision = revision_for(&mut database, &source);
        database
            .body_closure(revision, closure_key.clone(), CancellationToken::new())
            .expect("successive selected closure publishes");
        let artifact = database.runtime.request_registered(
            &database.declaration_body_plan_artifacts,
            revision,
            DeclarationBodyPlanQueryKey(declaration_candidate(
                &database,
                revision,
                &module,
                crate::declaration_candidate::DeclarationCandidateCategory::Function,
                "chosen",
            )),
            CancellationToken::new(),
        );
        assert_eq!(artifact.terminal().unwrap().stamp(), first_artifact_stamp);
        let transaction = database
            .body_transaction(revision, key.clone(), CancellationToken::new())
            .expect("successive chosen transaction remains reusable");
        assert_eq!(transaction.stamp(), first_transaction.stamp());
        assert!(
            database
                .declaration_body_plan_artifacts
                .retention()
                .terminals
                <= BODY_QUERY_MEMO_RETENTION + 1
        );
    }
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        20,
        "each revision lowers the reached candidate once, never once per consumer"
    );
}

#[test]
fn candidate_artifact_retention_bounds_history_and_rederives_evicted_values() {
    let mut text = (0..(BODY_QUERY_MEMO_RETENTION + 6))
        .map(|index| format!("fn f{index}() -> i32 {{ {index} }}\n"))
        .collect::<String>();
    text.push_str("fn chosen() -> i32 { 7 }\n");
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

    database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "chosen")]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("current chosen body closure is rooted");
    let chosen = declaration_candidate(
        &database,
        revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::Function,
        "chosen",
    );
    let chosen_terminal = database
        .runtime
        .request_registered(
            &database.declaration_body_plan_artifacts,
            revision,
            DeclarationBodyPlanQueryKey(chosen),
            CancellationToken::new(),
        )
        .into_result()
        .unwrap();
    let rue_query::QueryOutcome::Success(DeclarationBodyPlanArtifactsValue::Available(
        chosen_artifact,
    )) = chosen_terminal.outcome()
    else {
        panic!("chosen artifact is available")
    };
    let chosen_weak = Arc::downgrade(chosen_artifact);
    drop(chosen_terminal);

    let first_candidate = declaration_candidate(
        &database,
        revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::Function,
        "f0",
    );
    let first_attempt = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        revision,
        DeclarationBodyPlanQueryKey(first_candidate.clone()),
        CancellationToken::new(),
    );
    let first_terminal = first_attempt.terminal().unwrap().clone();
    let rue_query::QueryOutcome::Success(DeclarationBodyPlanArtifactsValue::Available(
        first_artifact,
    )) = first_terminal.outcome()
    else {
        panic!("first artifact is available")
    };
    let first_weak = Arc::downgrade(first_artifact);
    let render = |artifact: &crate::canonical_lower::DeclarationBodyPlanArtifacts| {
        let declaration_start = u32::try_from(text.find("fn f0").unwrap()).unwrap();
        let space = rue_rir::SharedSymbolSpace::private();
        let rir = artifact
            .plan
            .materialize_candidate_rir(
                &space,
                rue_span::FileId::new(1),
                declaration_start,
                u32::try_from(text.len()).unwrap(),
                || Ok(()),
            )
            .unwrap();
        rue_rir::RirPrinter::new(&rir, space.interner()).to_string()
    };
    let first_render = render(first_artifact);
    drop(first_terminal);
    drop(first_attempt);

    for index in 1..(BODY_QUERY_MEMO_RETENTION + 6) {
        let candidate = declaration_candidate(
            &database,
            revision,
            &module,
            crate::declaration_candidate::DeclarationCandidateCategory::Function,
            &format!("f{index}"),
        );
        let terminal = database
            .runtime
            .request_registered(
                &database.declaration_body_plan_artifacts,
                revision,
                DeclarationBodyPlanQueryKey(candidate),
                CancellationToken::new(),
            )
            .into_result()
            .unwrap();
        drop(terminal);
    }

    let retention = database.declaration_body_plan_artifacts.retention();
    assert!(
        retention.terminals <= BODY_QUERY_MEMO_RETENTION + 1,
        "only the current rooted artifact may exceed the bounded history: {retention:?}"
    );
    assert!(
        chosen_weak.upgrade().is_some(),
        "the current closure root stays live"
    );
    assert!(
        first_weak.upgrade().is_none(),
        "the stale unrooted artifact is released"
    );

    let rederived = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        revision,
        DeclarationBodyPlanQueryKey(first_candidate),
        CancellationToken::new(),
    );
    assert_eq!(rederived.execution(), rue_query::RequestExecution::Computed);
    let rue_query::QueryOutcome::Success(DeclarationBodyPlanArtifactsValue::Available(
        rederived_artifact,
    )) = rederived.terminal().unwrap().outcome()
    else {
        panic!("evicted artifact rederives successfully")
    };
    assert_eq!(render(rederived_artifact), first_render);
}

#[test]
fn physical_path_change_invalidates_named_body_input() {
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, "chosen"),
        semantic_configuration(),
    );
    let mut database = RevisionedQueryDatabase::default();
    let first = source_snapshot(
        &[(
            1,
            "/first/main.rue",
            "main.rue",
            "fn chosen() -> i32 { 7 }\n",
        )],
        1,
    );
    let first_revision = revision_for(&mut database, &first);
    let first_input = database
        .body_input(first_revision, key.clone(), CancellationToken::new())
        .unwrap();
    let second = source_snapshot(
        &[(
            1,
            "/second/main.rue",
            "main.rue",
            "fn chosen() -> i32 { 7 }\n",
        )],
        1,
    );
    let second_revision = revision_for(&mut database, &second);
    let second_input = database
        .body_input(second_revision, key, CancellationToken::new())
        .unwrap();

    assert_ne!(first_input.stamp(), second_input.stamp());
}

#[test]
fn file_id_reassignment_refreshes_current_basis_without_dirtying_body_input() {
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, "chosen"),
        semantic_configuration(),
    );
    let mut database = RevisionedQueryDatabase::default();
    let text = "fn chosen() -> i32 { missing }\n";
    let first = source_snapshot(&[(1, "/main.rue", "main.rue", text)], 1);
    let first_revision = revision_for(&mut database, &first);
    let first_input = database
        .body_input(first_revision, key.clone(), CancellationToken::new())
        .unwrap();
    let second = source_snapshot(&[(7, "/main.rue", "main.rue", text)], 7);
    let second_revision = revision_for(&mut database, &second);
    let second_input = database
        .body_input(second_revision, key.clone(), CancellationToken::new())
        .unwrap();
    let second_transaction = database
        .body_transaction(second_revision, key, CancellationToken::new())
        .expect("reassigned failure transaction publishes");

    assert_eq!(first_input.stamp(), second_input.stamp());
    let rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Available(second)) =
        second_input.outcome()
    else {
        panic!("reassigned body input remains available")
    };
    assert_eq!(second.source.file_id, rue_span::FileId::new(7));
    assert!(second.artifacts.plan.instruction_count() > 0);
    let rue_query::QueryOutcome::Success(transaction) = second_transaction.outcome() else {
        panic!("transaction publishes a typed value")
    };
    let projected = project_transaction_diagnostics(transaction.clone(), Some(&second.source));
    let crate::body_query::BodyTransaction::DeterministicFailure { errors, .. } = projected else {
        panic!("undefined name remains a deterministic failure")
    };
    assert!(errors.iter().all(|error| {
        error
            .span()
            .is_none_or(|span| span.file_id == rue_span::FileId::new(7))
    }));
}

#[test]
fn body_plan_failure_preserves_compile_errors_and_referenced_body_reachability() {
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let bad = free_function_instance(&module, "bad");
    let crate::FunctionInstanceKey::Definition(definition) = &bad else {
        unreachable!()
    };
    let errors = crate::CompileErrors::from(crate::CompileError::without_span(
        rue_error::ErrorKind::InvalidCompilerInput("broken canonical module RIR".into()),
    ));
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn bad() -> i32 { 1 }\nfn main() -> i32 { bad() }\n",
        )],
        1,
    );
    let mut database = RevisionedQueryDatabase::default();
    database.inject_declaration_body_plan_failure_for_test(definition, errors.clone());
    let revision = revision_for(&mut database, &source);
    let main = free_function_instance(&module, "main");
    let closure = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module]),
                roots: Arc::from([main.clone()]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("referenced plan failure publishes a closure");
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        panic!("body closure publishes a typed value")
    };
    assert_eq!(output.reached.as_ref(), &[bad.clone(), main]);
    assert!(output.scheduling_errors.is_empty());
    assert!(output.fatal.is_none());
    let bad_bundle = output
        .bodies
        .iter()
        .find(|body| body.key.instance == bad)
        .expect("referenced failing body remains scheduled");
    let rue_query::QueryOutcome::Success(bundle) = bad_bundle.bundle.outcome() else {
        panic!("body-analysis bundle publishes a typed value")
    };
    let crate::body_query::BodyTransaction::DeterministicFailure {
        errors: closure_errors,
        ..
    } = &bundle.transaction
    else {
        panic!("closure retains the referenced body's exact failure")
    };
    assert_eq!(closure_errors, &errors);

    let transaction = database
        .body_transaction(
            revision,
            crate::body_query::BodyQueryKey::new(bad, semantic_configuration()),
            CancellationToken::new(),
        )
        .expect("referenced body failure publishes its transaction");
    let rue_query::QueryOutcome::Success(transaction) = transaction.outcome() else {
        panic!("body transaction publishes a typed value")
    };
    let crate::body_query::BodyTransaction::DeterministicFailure {
        errors: published, ..
    } = transaction
    else {
        panic!("body-plan failure publishes a deterministic transaction")
    };
    assert_eq!(published, &errors);
}

#[test]
fn internal_body_trivia_recomputes_failure_at_the_current_span() {
    let source = |body: &str| source_snapshot(&[(1, "/main.rue", "main.rue", body)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, "chosen"),
        semantic_configuration(),
    );
    let mut database = RevisionedQueryDatabase::default();
    let first_source = source("fn chosen() -> i32 { missing }\n");
    let first_revision = revision_for(&mut database, &first_source);
    let first_input = database
        .body_input(first_revision, key.clone(), CancellationToken::new())
        .unwrap();
    let first_transaction = database
        .body_transaction(first_revision, key.clone(), CancellationToken::new())
        .expect("first failing transaction publishes");

    let second_text = "fn chosen() -> i32 {       missing }\n";
    let second_source = source(second_text);
    let second_revision = revision_for(&mut database, &second_source);
    let second_input = database
        .body_input(second_revision, key.clone(), CancellationToken::new())
        .unwrap();
    let second_transaction = database
        .body_transaction(second_revision, key, CancellationToken::new())
        .expect("shifted failing transaction publishes");

    assert_ne!(first_input.stamp(), second_input.stamp());
    assert_ne!(first_transaction.stamp(), second_transaction.stamp());
    let rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Available(input)) =
        second_input.outcome()
    else {
        panic!("second body input is available")
    };
    let rue_query::QueryOutcome::Success(transaction) = second_transaction.outcome() else {
        panic!("second transaction publishes a typed failure")
    };
    let projected = project_transaction_diagnostics(transaction.clone(), Some(&input.source));
    let crate::body_query::BodyTransaction::DeterministicFailure { errors, .. } = projected else {
        panic!("undefined name remains a deterministic body failure")
    };
    let missing = u32::try_from(second_text.find("missing").unwrap()).unwrap();
    assert!(errors.iter().any(|error| {
        error.span().is_some_and(|span| {
            span.file_id == input.source.file_id && span.start <= missing && missing < span.end
        })
    }));
}

#[test]
fn anonymous_member_reuses_its_candidate_artifact_and_current_source_basis() {
    let first_text = "// unrelated leading source\nfn Box() -> type {\n    struct { fn get(self) -> i32 { 7 } }\n}";
    let shifted_text = "// another position-only line\n// unrelated leading source\nfn Box() -> type {\n    struct { fn get(self) -> i32 { 7 } }\n}";
    let source =
        |file_id, text| source_snapshot(&[(file_id, "/main.rue", "main.rue", text)], file_id);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let producer = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Box")),
        arguments: crate::CanonicalArguments::default(),
    };
    let producer_key =
        crate::body_query::BodyQueryKey::new(producer.clone(), semantic_configuration());
    let member_key = |terminal: &rue_query::QueryTerminal<crate::body_query::ProducedAnonymous>| {
        let rue_query::QueryOutcome::Success(crate::body_query::ProducedAnonymous::Produced(
            produced,
        )) = terminal.outcome()
        else {
            panic!("anonymous producer did not publish its member facts")
        };
        let owner = produced
            .0
            .iter()
            .find(|nominal| {
                let crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                    methods, ..
                } = &nominal.shape
                else {
                    return false;
                };
                methods
                    .iter()
                    .any(|method| method.name.as_ref() == "get" && method.has_body)
            })
            .expect("producer publishes a body-bearing anonymous get method");
        crate::body_query::BodyQueryKey::new(
            crate::FunctionInstanceKey::AnonymousMember {
                owner: Node::new(crate::TypeInstanceKey::Nominal(
                    crate::NominalInstanceKey::Anonymous(Node::new(owner.identity.clone())),
                )),
                member: crate::AnonymousMemberKey {
                    kind: crate::AnonymousMemberKind::Method,
                    name: Arc::from("get"),
                },
            },
            semantic_configuration(),
        )
    };

    let mut database = RevisionedQueryDatabase::default();
    let first_source = source(1, first_text);
    let first_revision = revision_for(&mut database, &first_source);
    let first_produced = database.runtime.request_registered(
        &database.body_produced_anonymous,
        first_revision,
        producer_key.clone(),
        CancellationToken::new(),
    );
    let first_member = member_key(first_produced.terminal().unwrap());
    let first_candidate = declaration_candidate(
        &database,
        first_revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::Function,
        "Box",
    );
    let first_artifact = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        first_revision,
        DeclarationBodyPlanQueryKey(first_candidate),
        CancellationToken::new(),
    );
    let first_artifact_stamp = first_artifact.terminal().unwrap().stamp();
    let astgen_before_member = database
        .declaration_body_plan_astgen_evaluations
        .load(std::sync::atomic::Ordering::Relaxed);
    let first_transaction = database
        .body_transaction(
            first_revision,
            first_member.clone(),
            CancellationToken::new(),
        )
        .expect("anonymous get transaction succeeds from its producer artifact");
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        astgen_before_member,
        "the anonymous member must reuse its producer candidate AstGen result",
    );
    assert!(first_transaction.dependencies().iter().any(|dependency| {
        dependency.node.family() == "compiler.declaration-body-plan-artifacts"
            && dependency.stamp == first_artifact_stamp
    }));
    let rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success {
        body, ..
    }) = first_transaction.outcome()
    else {
        panic!("anonymous get transaction did not publish a body")
    };
    let crate::body_query::CanonicalBody::Anonymous {
        body_anchor: first_anchor,
        ..
    } = body.as_ref()
    else {
        panic!("anonymous get transaction published the wrong body kind")
    };
    let first_locator = database
        .body_source_basis_projection(
            first_revision,
            first_member.clone(),
            CancellationToken::new(),
        )
        .unwrap();
    let rue_query::QueryOutcome::Success(Some(first_locator)) = first_locator.outcome() else {
        panic!("anonymous member has its producer's current source basis")
    };
    assert_eq!(
        first_locator.body_start + first_anchor.start,
        u32::try_from(first_text.find("7 }").unwrap()).unwrap(),
    );
    assert_eq!(
        first_locator.body_start + first_anchor.end,
        u32::try_from(first_text.find("7 }").unwrap() + "7".len()).unwrap(),
    );

    let shifted_source = source(7, shifted_text);
    let shifted_revision = revision_for(&mut database, &shifted_source);
    let shifted_produced = database.runtime.request_registered(
        &database.body_produced_anonymous,
        shifted_revision,
        producer_key,
        CancellationToken::new(),
    );
    let shifted_member = member_key(shifted_produced.terminal().unwrap());
    assert_eq!(shifted_member, first_member);
    let shifted_candidate = declaration_candidate(
        &database,
        shifted_revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::Function,
        "Box",
    );
    let shifted_artifact = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        shifted_revision,
        DeclarationBodyPlanQueryKey(shifted_candidate),
        CancellationToken::new(),
    );
    assert_eq!(
        shifted_artifact.terminal().unwrap().stamp(),
        first_artifact_stamp,
        "moving the producer must not restamp its candidate artifact",
    );
    let astgen_before_shifted_member = database
        .declaration_body_plan_astgen_evaluations
        .load(std::sync::atomic::Ordering::Relaxed);
    let shifted_transaction = database
        .body_transaction(
            shifted_revision,
            shifted_member.clone(),
            CancellationToken::new(),
        )
        .expect("shifted anonymous get transaction succeeds");
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        astgen_before_shifted_member,
    );
    assert_eq!(shifted_transaction.stamp(), first_transaction.stamp());
    let rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success {
        body, ..
    }) = shifted_transaction.outcome()
    else {
        panic!("shifted anonymous get transaction did not publish a body")
    };
    let crate::body_query::CanonicalBody::Anonymous {
        body_anchor: shifted_anchor,
        ..
    } = body.as_ref()
    else {
        panic!("shifted anonymous get transaction published the wrong body kind")
    };
    assert_eq!(shifted_anchor, first_anchor);
    let shifted_locator = database
        .body_source_basis_projection(shifted_revision, shifted_member, CancellationToken::new())
        .unwrap();
    let rue_query::QueryOutcome::Success(Some(shifted_locator)) = shifted_locator.outcome() else {
        panic!("shifted anonymous member has its current producer basis")
    };
    assert_eq!(shifted_locator.file_id, FileId::new(7));
    assert_eq!(
        shifted_locator.body_start + shifted_anchor.start,
        u32::try_from(shifted_text.find("7 }").unwrap()).unwrap(),
    );
    assert_eq!(
        shifted_locator.body_start + shifted_anchor.end,
        u32::try_from(shifted_text.find("7 }").unwrap() + "7".len()).unwrap(),
    );
}

#[test]
fn anonymous_member_diagnostics_relocate_and_internal_trivia_invalidates() {
    let text = |prefix: &str, trivia: &str| {
        format!(
            "{prefix}fn Box() -> type {{\n    struct {{ fn get(self) -> i32 {{ {trivia}missing }} }}\n}}"
        )
    };
    let source = |text: &str| source_snapshot(&[(1, "/main.rue", "main.rue", text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let producer = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Box")),
        arguments: crate::CanonicalArguments::default(),
    };
    let member_key = |terminal: &rue_query::QueryTerminal<crate::body_query::ProducedAnonymous>| {
        let rue_query::QueryOutcome::Success(crate::body_query::ProducedAnonymous::Produced(
            produced,
        )) = terminal.outcome()
        else {
            panic!("Box did not publish get's owner")
        };
        let owner = produced
            .0
            .iter()
            .find(|nominal| {
                matches!(
                    &nominal.shape,
                    crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                        methods,
                        ..
                    } if methods.iter().any(|method| method.name.as_ref() == "get")
                )
            })
            .unwrap();
        crate::body_query::BodyQueryKey::new(
            crate::FunctionInstanceKey::AnonymousMember {
                owner: Node::new(crate::TypeInstanceKey::Nominal(
                    crate::NominalInstanceKey::Anonymous(Node::new(owner.identity.clone())),
                )),
                member: crate::AnonymousMemberKey {
                    kind: crate::AnonymousMemberKind::Method,
                    name: Arc::from("get"),
                },
            },
            semantic_configuration(),
        )
    };
    let request = |database: &RevisionedQueryDatabase,
                   revision,
                   producer_key: crate::body_query::BodyQueryKey| {
        let produced = database.runtime.request_registered(
            &database.body_produced_anonymous,
            revision,
            producer_key,
            CancellationToken::new(),
        );
        let member = member_key(produced.terminal().unwrap());
        let transaction = database
            .body_transaction(revision, member.clone(), CancellationToken::new())
            .expect("anonymous diagnostic publishes deterministically");
        let locator = database
            .body_source_basis_projection(revision, member, CancellationToken::new())
            .unwrap();
        (transaction, locator)
    };

    let mut database = RevisionedQueryDatabase::default();
    let first_text = text("", "");
    let first_source = source(&first_text);
    let first_revision = revision_for(&mut database, &first_source);
    let first_candidate = declaration_candidate(
        &database,
        first_revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::Function,
        "Box",
    );
    let first_artifact = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        first_revision,
        DeclarationBodyPlanQueryKey(first_candidate),
        CancellationToken::new(),
    );
    let (first_transaction, _) = request(
        &database,
        first_revision,
        crate::body_query::BodyQueryKey::new(producer.clone(), semantic_configuration()),
    );

    let shifted_text = text("// position-only prefix\n", "");
    let shifted_source = source(&shifted_text);
    let shifted_revision = revision_for(&mut database, &shifted_source);
    let shifted_candidate = declaration_candidate(
        &database,
        shifted_revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::Function,
        "Box",
    );
    let shifted_artifact = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        shifted_revision,
        DeclarationBodyPlanQueryKey(shifted_candidate),
        CancellationToken::new(),
    );
    let (shifted_transaction, shifted_locator) = request(
        &database,
        shifted_revision,
        crate::body_query::BodyQueryKey::new(producer.clone(), semantic_configuration()),
    );
    assert_eq!(
        shifted_artifact.terminal().unwrap().stamp(),
        first_artifact.terminal().unwrap().stamp(),
    );
    assert_eq!(shifted_transaction.stamp(), first_transaction.stamp());
    let rue_query::QueryOutcome::Success(Some(shifted_locator)) = shifted_locator.outcome() else {
        panic!("shifted anonymous diagnostic has a current source basis")
    };
    let rue_query::QueryOutcome::Success(transaction) = shifted_transaction.outcome() else {
        panic!("shifted anonymous diagnostic has a typed transaction")
    };
    let projected = project_transaction_diagnostics(transaction.clone(), Some(shifted_locator));
    let crate::body_query::BodyTransaction::DeterministicFailure { errors, .. } = projected else {
        panic!("undefined anonymous name remains a deterministic failure")
    };
    let shifted_missing = u32::try_from(shifted_text.find("missing").unwrap()).unwrap();
    assert!(errors.iter().any(|error| {
        error.span().is_some_and(|span| {
            span.file_id == shifted_locator.file_id
                && span.start <= shifted_missing
                && shifted_missing < span.end
        })
    }));

    let internal_text = text("// position-only prefix\n", "       ");
    let internal_source = source(&internal_text);
    let internal_revision = revision_for(&mut database, &internal_source);
    let internal_candidate = declaration_candidate(
        &database,
        internal_revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::Function,
        "Box",
    );
    let internal_artifact = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        internal_revision,
        DeclarationBodyPlanQueryKey(internal_candidate),
        CancellationToken::new(),
    );
    let (internal_transaction, internal_locator) = request(
        &database,
        internal_revision,
        crate::body_query::BodyQueryKey::new(producer, semantic_configuration()),
    );
    assert_ne!(
        internal_artifact.terminal().unwrap().stamp(),
        shifted_artifact.terminal().unwrap().stamp(),
    );
    assert_ne!(internal_transaction.stamp(), shifted_transaction.stamp());
    let rue_query::QueryOutcome::Success(Some(internal_locator)) = internal_locator.outcome()
    else {
        panic!("internally shifted anonymous diagnostic has a current source basis")
    };
    let rue_query::QueryOutcome::Success(transaction) = internal_transaction.outcome() else {
        panic!("internally shifted anonymous diagnostic has a typed transaction")
    };
    let projected = project_transaction_diagnostics(transaction.clone(), Some(internal_locator));
    let crate::body_query::BodyTransaction::DeterministicFailure { errors, .. } = projected else {
        panic!("internally shifted undefined name remains deterministic")
    };
    let internal_missing = u32::try_from(internal_text.find("missing").unwrap()).unwrap();
    assert!(errors.iter().any(|error| {
        error.span().is_some_and(|span| {
            span.file_id == internal_locator.file_id
                && span.start <= internal_missing
                && internal_missing < span.end
        })
    }));
}

#[test]
fn nested_anonymous_members_share_the_ultimate_candidate_artifact() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Outer() -> type {\n\
                     struct {\n\
                         fn make(self) -> type {\n\
                             struct { fn value(self) -> i32 { 11 } }\n\
                         }\n\
                     }\n\
                 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let producer = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Outer")),
        arguments: crate::CanonicalArguments::default(),
    };
    let member_key = |terminal: &rue_query::QueryTerminal<crate::body_query::ProducedAnonymous>,
                      name: &str| {
        let rue_query::QueryOutcome::Success(crate::body_query::ProducedAnonymous::Produced(
            produced,
        )) = terminal.outcome()
        else {
            panic!("anonymous producer did not publish {name}")
        };
        let owner = produced
            .0
            .iter()
            .find(|nominal| {
                let crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                    methods, ..
                } = &nominal.shape
                else {
                    return false;
                };
                methods
                    .iter()
                    .any(|method| method.name.as_ref() == name && method.has_body)
            })
            .unwrap_or_else(|| panic!("anonymous producer has no body-bearing {name}"));
        crate::body_query::BodyQueryKey::new(
            crate::FunctionInstanceKey::AnonymousMember {
                owner: Node::new(crate::TypeInstanceKey::Nominal(
                    crate::NominalInstanceKey::Anonymous(Node::new(owner.identity.clone())),
                )),
                member: crate::AnonymousMemberKey {
                    kind: crate::AnonymousMemberKind::Method,
                    name: Arc::from(name),
                },
            },
            semantic_configuration(),
        )
    };

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let outer = database.runtime.request_registered(
        &database.body_produced_anonymous,
        revision,
        crate::body_query::BodyQueryKey::new(producer, semantic_configuration()),
        CancellationToken::new(),
    );
    let make = member_key(outer.terminal().unwrap(), "make");
    let after_outer = database
        .declaration_body_plan_astgen_evaluations
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        after_outer, 1,
        "discovering Outer evaluates its canonical candidate artifact exactly once",
    );

    let inner = database.runtime.request_registered(
        &database.body_produced_anonymous,
        revision,
        make.clone(),
        CancellationToken::new(),
    );
    let value = member_key(inner.terminal().unwrap(), "value");
    let after_make = database
        .declaration_body_plan_astgen_evaluations
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(after_make, 1, "make demands Outer's candidate exactly once");
    let value_transaction = database
        .body_transaction(revision, value, CancellationToken::new())
        .expect("nested value transaction succeeds");
    assert!(matches!(
        value_transaction.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
    ));
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        after_make,
        "make and value must both select nested declarations from Outer",
    );

    let make_transaction = database
        .body_transaction(revision, make, CancellationToken::new())
        .expect("make transaction remains retained");
    let artifact_dependency = |transaction: &rue_query::QueryTerminal<_>| {
        transaction
            .dependencies()
            .iter()
            .find(|dependency| {
                dependency.node.family() == "compiler.declaration-body-plan-artifacts"
            })
            .map(|dependency| (dependency.incarnation, dependency.stamp))
            .expect("anonymous transaction directly observes its producer artifact")
    };
    assert_eq!(
        artifact_dependency(&make_transaction),
        artifact_dependency(&value_transaction),
    );
}

#[test]
fn const_produced_anonymous_member_uses_the_const_candidate_artifact() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const T: type = struct {\n\
                     value: i32,\n\
                     fn get(self) -> i32 { self.value }\n\
                 };\n\
                 fn main() -> i32 {\n\
                     let value: T = T { value: 5 };\n\
                     value.get()\n\
                 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let configuration = semantic_configuration();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let closure = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "main")]),
                configuration: configuration.clone(),
            },
            CancellationToken::new(),
        )
        .expect("const-produced anonymous member closure succeeds");
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        panic!("const-produced closure publishes a typed value")
    };
    let get = output
        .reached
        .iter()
        .find(|instance| {
            matches!(
                instance,
                crate::FunctionInstanceKey::AnonymousMember { member, .. }
                    if member.name.as_ref() == "get"
            )
        })
        .cloned()
        .expect("main reaches T.get");
    let transaction = database
        .body_transaction(
            revision,
            crate::body_query::BodyQueryKey::new(get, configuration),
            CancellationToken::new(),
        )
        .expect("T.get transaction remains retained");
    assert!(matches!(
        transaction.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
    ));

    let candidate = declaration_candidate(
        &database,
        revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
        "T",
    );
    let artifact = database.runtime.request_registered(
        &database.declaration_body_plan_artifacts,
        revision,
        DeclarationBodyPlanQueryKey(candidate),
        CancellationToken::new(),
    );
    let artifact_stamp = artifact.terminal().unwrap().stamp();
    assert!(transaction.dependencies().iter().any(|dependency| {
        dependency.node.family() == "compiler.declaration-body-plan-artifacts"
            && dependency.stamp == artifact_stamp
    }));
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        2,
        "main and const T each lower once; T.get adds no AstGen work",
    );
}

#[test]
fn declaration_root_handoff_prevents_const_and_comptime_artifact_rederivation() {
    let source = source_snapshot(
        &[((
            1,
            "/main.rue",
            "main.rue",
            "fn selected(comptime seed: i32) -> i32 { seed + 2 }\n\
                 const VALUE: i32 = selected(40);\n\
                 fn main() -> i32 { VALUE }",
        ))],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let configuration = semantic_configuration();
    let preview_features = crate::PreviewFeatures::default();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

    database
        .projected_declaration_semantics_for_modules(
            revision,
            [module.clone()],
            configuration.target,
            &preview_features,
            CancellationToken::new(),
        )
        .expect("declaration discovery succeeds");
    let after_declarations = database
        .declaration_body_plan_astgen_evaluations
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        after_declarations, 2,
        "the const and its comptime callee each lower once during declaration discovery",
    );

    let closure = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "main")]),
                configuration,
            },
            CancellationToken::new(),
        )
        .expect("rooted body closure succeeds");
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        panic!("rooted body closure publishes a typed value")
    };
    assert!(output.fatal.is_none());
    assert!(output.scheduling_errors.is_empty());
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        after_declarations + 1,
        "body closure lowers only main; the exact artifact leases carry the const and comptime candidates across the root boundary",
    );
}

#[test]
fn anonymous_producer_preserves_its_candidate_artifact_failure_before_member_publication() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Box() -> type { struct { fn get(self) -> i32 { 7 } } }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let crate::FunctionInstanceKey::Definition(definition) = free_function_instance(&module, "Box")
    else {
        unreachable!()
    };
    let errors = crate::CompileErrors::from(crate::CompileError::without_span(
        rue_error::ErrorKind::InvalidCompilerInput(
            "injected anonymous producer artifact failure".into(),
        ),
    ));
    let mut database = RevisionedQueryDatabase::default();
    database.inject_declaration_body_plan_failure_for_test(&definition, errors.clone());
    let revision = revision_for(&mut database, &source);
    let value = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(
            crate::semantic_query_nucleus::ComptimeCallQueryKey {
                declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration: declaration_candidate(
                        &database,
                        revision,
                        &module,
                        crate::declaration_candidate::DeclarationCandidateCategory::Function,
                        "Box",
                    ),
                    configuration: semantic_configuration(),
                },
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            },
        ),
    );
    let crate::semantic_query_nucleus::SemanticNucleusValue::Failure(
        crate::semantic_query_nucleus::SemanticNucleusFailure::Syntax(published),
    ) = value
    else {
        panic!("candidate artifact failure was downgraded or published an owner: {value:?}")
    };
    assert_eq!(published.as_ref(), errors.to_string());
}

#[test]
fn anonymous_member_kind_mismatch_is_deterministic_not_cancellation() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Box() -> type { struct { fn get(self) -> i32 { 7 } } }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let producer = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Box")),
        arguments: crate::CanonicalArguments::default(),
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let produced = database.runtime.request_registered(
        &database.body_produced_anonymous,
        revision,
        crate::body_query::BodyQueryKey::new(producer, semantic_configuration()),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(crate::body_query::ProducedAnonymous::Produced(produced)) =
        produced.terminal().unwrap().outcome()
    else {
        panic!("Box publishes get's owner")
    };
    let owner = produced
        .0
        .first()
        .expect("Box publishes one anonymous struct");
    let mismatched = crate::body_query::BodyQueryKey::new(
        crate::FunctionInstanceKey::AnonymousMember {
            owner: Node::new(crate::TypeInstanceKey::Nominal(
                crate::NominalInstanceKey::Anonymous(Node::new(owner.identity.clone())),
            )),
            member: crate::AnonymousMemberKey {
                kind: crate::AnonymousMemberKind::AssociatedFunction,
                name: Arc::from("get"),
            },
        },
        semantic_configuration(),
    );
    let attempt = database.runtime.request_registered(
        &database.body_transactions,
        revision,
        mismatched.clone(),
        CancellationToken::new(),
    );
    assert!(attempt.abort().is_none());
    let terminal = attempt
        .terminal()
        .expect("mismatched anonymous member publishes a stable failure");
    assert!(matches!(
        terminal.outcome(),
        rue_query::QueryOutcome::Success(
            crate::body_query::BodyTransaction::DeterministicFailure { .. }
        )
    ));
    assert!(
        database
            .body_transactions
            .contains_retained_key(&mismatched)
    );
}

#[test]
fn anonymous_member_materialization_cancellation_publishes_nothing_and_retries() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Box() -> type {\n\
                     struct { fn get(self) -> i32 { let x = 7; x } }\n\
                 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let producer = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Box")),
        arguments: crate::CanonicalArguments::default(),
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let produced = database.runtime.request_registered(
        &database.body_produced_anonymous,
        revision,
        crate::body_query::BodyQueryKey::new(producer, semantic_configuration()),
        CancellationToken::new(),
    );
    let terminal = produced.terminal().unwrap();
    let rue_query::QueryOutcome::Success(crate::body_query::ProducedAnonymous::Produced(produced)) =
        terminal.outcome()
    else {
        panic!("Box publishes its anonymous get owner")
    };
    let owner = produced
        .0
        .iter()
        .find(|nominal| {
            matches!(
                &nominal.shape,
                crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                    methods,
                    ..
                } if methods.iter().any(|method| method.name.as_ref() == "get")
            )
        })
        .unwrap();
    let get = crate::body_query::BodyQueryKey::new(
        crate::FunctionInstanceKey::AnonymousMember {
            owner: Node::new(crate::TypeInstanceKey::Nominal(
                crate::NominalInstanceKey::Anonymous(Node::new(owner.identity.clone())),
            )),
            member: crate::AnonymousMemberKey {
                kind: crate::AnonymousMemberKind::Method,
                name: Arc::from("get"),
            },
        },
        semantic_configuration(),
    );
    let candidate = declaration_candidate(
        &database,
        revision,
        &module,
        crate::declaration_candidate::DeclarationCandidateCategory::Function,
        "Box",
    );
    database
        .runtime
        .request_registered(
            &database.declaration_body_plan_artifacts,
            revision,
            DeclarationBodyPlanQueryKey(candidate),
            CancellationToken::new(),
        )
        .into_result()
        .expect("producer artifact is warm before transaction materialization");

    {
        let _injection = database.cancel_constraint_generation_after_nodes_for_test(12);
        let attempt = database.runtime.request_registered(
            &database.body_transactions,
            revision,
            get.clone(),
            CancellationToken::new(),
        );
        assert!(matches!(attempt.abort(), Some(QueryAbort::Canceled)));
        assert!(attempt.terminal().is_none());
        assert!(!database.body_transactions.contains_retained_key(&get));
    }

    let retry = database
        .body_transaction(revision, get.clone(), CancellationToken::new())
        .expect("uncanceled anonymous-member retry completes");
    assert!(matches!(
        retry.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
    ));
    assert!(database.body_transactions.contains_retained_key(&get));
}

#[test]
fn body_diagnostic_projection_preserves_nonlocal_and_unknown_spans() {
    let source = crate::body_query::BodySourceLocator {
        file_id: FileId::new(7),
        physical_path: Arc::from("/old/main.rue"),
        source_length: 180,
        source_text: Arc::new(String::new()),
        declaration_start: 100,
        declaration_end: 150,
        body_start: 120,
        body_end: 145,
    };
    let local = crate::Span::with_file(source.file_id, 125, 128);
    let absolute = crate::Span::with_file(FileId::new(8), 4, 6);
    let unknown = crate::Span::new(2, 3);
    let empty_unknown = crate::Span::default();
    let mut errors = crate::CompileErrors::new();
    for span in [local, absolute, unknown, empty_unknown] {
        errors.push(crate::CompileError::new(
            crate::ErrorKind::InvalidInteger,
            span,
        ));
    }
    let (errors, diagnostic_basis) = crate::body_query::relative_body_diagnostics(errors, &source);
    let transaction = crate::body_query::BodyTransaction::DeterministicFailure {
        errors,
        diagnostic_basis: Some(diagnostic_basis),
        references: crate::body_query::BodyReferences(Arc::from([])),
        lookup_observations: crate::body_query::BodyLookupObservations::default(),
    };

    let current = crate::body_query::BodySourceLocator {
        file_id: FileId::new(9),
        physical_path: Arc::from("/current/main.rue"),
        source_length: 280,
        source_text: Arc::new(String::new()),
        declaration_start: 200,
        declaration_end: 250,
        body_start: 220,
        body_end: 245,
    };
    let crate::body_query::BodyTransaction::DeterministicFailure { errors, .. } =
        project_transaction_diagnostics(transaction, Some(&current))
    else {
        panic!("diagnostic projection preserves the transaction variant")
    };
    let projected: Vec<_> = errors
        .iter()
        .map(|error| error.span().expect("test diagnostics have primary spans"))
        .collect();
    assert_eq!(
        projected,
        [
            crate::Span::with_file(current.file_id, 225, 228),
            absolute,
            unknown,
            empty_unknown,
        ]
    );
}

#[test]
fn body_input_registered_evaluator_classifies_unsupported_and_missing_inputs() {
    let generic = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn selected(comptime T: type) -> T { 7 }\nstruct NotABody {}\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let instance = free_function_instance(&module, "selected");
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&generic), &generic);
    let key = crate::body_query::BodyQueryKey::new(instance, semantic_configuration());
    let terminal = database
        .body_input(revision, key, CancellationToken::new())
        .unwrap();
    assert!(matches!(
        terminal.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Incomplete(
            crate::body_query::BodyInputIncomplete::Generic
        ))
    ));

    let unknown = crate::body_query::BodyQueryKey::new(
        free_function_instance(
            &ModuleId::from_logical_path("other.rue").unwrap(),
            "missing",
        ),
        semantic_configuration(),
    );
    let missing = database
        .body_input(revision, unknown, CancellationToken::new())
        .unwrap();
    assert!(matches!(
        missing.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Incomplete(
            crate::body_query::BodyInputIncomplete::MissingPrerequisite(_)
        ))
    ));

    let unsupported = crate::body_query::BodyQueryKey::new(
        crate::FunctionInstanceKey::Specialization {
            base: Node::new(free_function_instance(
                &ModuleId::from_logical_path("main.rue").unwrap(),
                "selected",
            )),
            arguments: crate::CanonicalArguments::default(),
        },
        semantic_configuration(),
    );
    let specialization = database
        .body_input(revision, unsupported, CancellationToken::new())
        .unwrap();
    assert!(matches!(
        specialization.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Available(_))
    ));

    let unsupported_kind = crate::StableDefinitionKind::Struct;
    let unsupported = crate::body_query::BodyQueryKey::new(
        crate::FunctionInstanceKey::Definition(crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("main.rue").unwrap(),
            crate::StableDefinitionNamespace::Type,
            unsupported_kind,
            Arc::from("NotABody"),
            None,
        )),
        semantic_configuration(),
    );
    let terminal = database
        .body_input(revision, unsupported, CancellationToken::new())
        .unwrap();
    assert!(matches!(
        terminal.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Incomplete(
            crate::body_query::BodyInputIncomplete::UnsupportedKind(actual)
        )) if *actual == unsupported_kind
    ));

    let extern_source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "extern \"C\" { fn selected() -> i32; }\n",
        )],
        1,
    );
    let mut extern_database = RevisionedQueryDatabase::default();
    let extern_revision = extern_database.source_revision(
        &crate::session::ExactSourceInput::new(&extern_source),
        &extern_source,
    );
    let extern_terminal = extern_database
        .body_input(
            extern_revision,
            crate::body_query::BodyQueryKey::new(
                free_function_instance(
                    &ModuleId::from_logical_path("main.rue").unwrap(),
                    "selected",
                ),
                semantic_configuration(),
            ),
            CancellationToken::new(),
        )
        .unwrap();
    assert!(matches!(
        extern_terminal.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Incomplete(
            crate::body_query::BodyInputIncomplete::Extern
        ))
    ));

    let malformed = source_snapshot(
        &[(1, "/main.rue", "main.rue", "fn selected() -> i32 {\n")],
        1,
    );
    let mut malformed_database = RevisionedQueryDatabase::default();
    let malformed_revision = malformed_database.source_revision(
        &crate::session::ExactSourceInput::new(&malformed),
        &malformed,
    );
    let malformed_terminal = malformed_database
        .body_input(
            malformed_revision,
            crate::body_query::BodyQueryKey::new(
                free_function_instance(
                    &ModuleId::from_logical_path("main.rue").unwrap(),
                    "selected",
                ),
                semantic_configuration(),
            ),
            CancellationToken::new(),
        )
        .unwrap();
    assert!(matches!(
        malformed_terminal.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Incomplete(
            crate::body_query::BodyInputIncomplete::MissingPrerequisite(_)
        ))
    ));
}

#[test]
fn body_input_cancellation_aborts_without_publishing_a_terminal() {
    let source = source_snapshot(
        &[(1, "/main.rue", "main.rue", "fn selected() -> i32 { 7 }\n")],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, "selected"),
        semantic_configuration(),
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let attempt = database.runtime.request_registered(
        &database.body_inputs,
        revision,
        key.clone(),
        cancellation,
    );
    assert!(matches!(attempt.abort(), Some(QueryAbort::Canceled)));
    assert!(attempt.terminal().is_none());
    assert!(!database.body_inputs.contains_retained_key(&key));
}

#[test]
fn cancellation_mid_body_materialization_publishes_no_terminal_and_retry_succeeds() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn selected() -> i32 { let a = 1; let b = 2; a + b }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&crate::session::ExactSourceInput::new(&source), &source);
    let key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, "selected"),
        semantic_configuration(),
    );
    database
        .body_input(revision, key.clone(), CancellationToken::new())
        .expect("body input is warm before transaction materialization");

    {
        let _injection = database.cancel_constraint_generation_after_nodes_for_test(12);
        let attempt = database.runtime.request_registered(
            &database.body_transactions,
            revision,
            key.clone(),
            CancellationToken::new(),
        );
        assert!(matches!(attempt.abort(), Some(QueryAbort::Canceled)));
        assert_eq!(database.constraint_generation_phase_for_test(), 1);
        assert!(database.constraint_generation_visits_for_test() >= 12);
        assert!(
            database.constraint_generation_visits_for_test() <= 24,
            "frontier cancellation must unwind promptly rather than visit the tail"
        );
        assert!(attempt.terminal().is_none());
        assert!(!database.body_transactions.contains_retained_key(&key));
        assert!(
            database.constraint_generation_visits_for_test() >= 12,
            "the dedicated cancellation injector entered constraint generation"
        );
    }

    let retry = database
        .body_transaction(revision, key.clone(), CancellationToken::new())
        .expect("uncanceled retry completes");
    assert!(matches!(
        retry.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
    ));
    assert!(database.body_transactions.contains_retained_key(&key));
}

#[test]
fn staged_comptime_facts_are_repeated_and_parallel_deterministic() {
    for iteration in 0..20 {
        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn choose(comptime n: i8) -> i32 { if n == 1 { if n < 2 { 9 } else { 0 } } else { 0 } }\nfn main() -> i32 { choose(1) }\n",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let crate::FunctionInstanceKey::Definition(base) =
            free_function_instance(&module, "choose")
        else {
            unreachable!("free function helper returns a definition");
        };
        let key = crate::body_query::BodyQueryKey::new(
            crate::FunctionInstanceKey::Specialization {
                base: Node::new(crate::FunctionInstanceKey::Definition(base)),
                arguments: crate::CanonicalArguments {
                    types: Arc::from([]),
                    values: Arc::from([crate::CanonicalArgumentValue::Integer(1)]),
                },
            },
            semantic_configuration(),
        );

        // Establish the sequential production baseline from a specialized
        // transaction; this exercises staged selector facts and the emitted
        // canonical body rather than only warming the input query.
        let mut baseline_database = RevisionedQueryDatabase::default();
        let baseline_revision = revision_for(&mut baseline_database, &source);
        let baseline = baseline_database
            .body_transaction(baseline_revision, key.clone(), CancellationToken::new())
            .expect("sequential specialized baseline succeeds");
        let baseline_metrics = baseline_database.provider_observation_metrics();

        // Run two independent production databases through the in-evaluation
        // rendezvous. Equal requests on one database would merely test query
        // joining; this gate proves both providers reach staged evaluation.
        let mut left_database = RevisionedQueryDatabase::with_query_concurrency(8);
        let left_revision = revision_for(&mut left_database, &source);
        let mut right_database = RevisionedQueryDatabase::with_query_concurrency(8);
        let right_revision = revision_for(&mut right_database, &source);
        let left_database = Arc::new(left_database);
        let right_database = Arc::new(right_database);
        let frontier_rendezvous = FrontierRendezvous::new();
        let _frontier_gate =
            left_database.arm_frontier_rendezvous_for_test(frontier_rendezvous.clone());
        let left_key = key.clone();
        let left_database_for_thread = left_database.clone();
        let left_thread = std::thread::spawn(move || {
            left_database_for_thread
                .body_transaction(left_revision, left_key, CancellationToken::new())
                .expect("left concurrent specialized query succeeds")
        });
        let right_database_for_thread = right_database.clone();
        let right_thread = std::thread::spawn(move || {
            right_database_for_thread
                .body_transaction(right_revision, key, CancellationToken::new())
                .expect("right concurrent specialized query succeeds")
        });
        assert!(
            frontier_rendezvous.wait_for_arrivals(2),
            "both providers must reach in-evaluation staged rendezvous (arrivals={}, timed_out={})",
            frontier_rendezvous.arrivals(),
            frontier_rendezvous.timed_out()
        );
        assert_eq!(frontier_rendezvous.arrivals(), 2);
        assert_eq!(frontier_rendezvous.frontier_arrivals(), 2);
        assert!(!frontier_rendezvous.timed_out());
        frontier_rendezvous.release();
        let left = left_thread.join().expect("left query thread joins");
        let right = right_thread.join().expect("right query thread joins");
        let left_metrics = left_database.provider_observation_metrics();
        let right_metrics = right_database.provider_observation_metrics();

        let rue_query::QueryOutcome::Success(baseline_transaction) = baseline.outcome() else {
            panic!("sequential specialized baseline did not succeed");
        };
        let rue_query::QueryOutcome::Success(left_transaction) = left.outcome() else {
            panic!("left concurrent specialized query did not succeed");
        };
        let rue_query::QueryOutcome::Success(right_transaction) = right.outcome() else {
            panic!("right concurrent specialized query did not succeed");
        };
        assert!(crate::body_query::transaction_equal(
            baseline_transaction,
            left_transaction,
        ));
        assert!(crate::body_query::transaction_equal(
            baseline_transaction,
            right_transaction,
        ));
        assert_eq!(baseline_metrics, left_metrics);
        assert_eq!(baseline_metrics, right_metrics);
        assert_eq!(
            frontier_rendezvous.arrivals(),
            2,
            "rendezvous iteration {iteration}"
        );
    }
}

#[test]
fn non_selector_benchmark_has_zero_staged_work() {
    let source = source_snapshot(
        &[(1, "/main.rue", "main.rue", "fn main() -> i32 { 7 }\n")],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, "main"),
        semantic_configuration(),
    );
    let result = database
        .body_transaction(revision, key, CancellationToken::new())
        .unwrap();
    assert!(matches!(
        result.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
    ));
    let metrics = database.provider_observation_metrics();
    assert_eq!(metrics.staged_probe_nodes, 0);
    assert_eq!(metrics.staged_frontier_bodies, 0);
    assert_eq!(metrics.staged_resolved_instructions, 0);
    assert_eq!(metrics.staged_fact_nodes, 0);
    assert_eq!(metrics.staged_canonical_evaluations, 0);
    assert_eq!(metrics.staged_constraints_generated, 0);
    assert_eq!(metrics.staged_binding_scope_nodes, 0);
    assert_eq!(metrics.staged_binding_materializations, 0);
    assert_eq!(metrics.staged_binding_trie_updates, 0);
    assert_eq!(metrics.staged_binding_trie_lookups, 0);
    assert_eq!(metrics.staged_precompute_nodes, 0);
}

#[test]
fn staged_local_and_selector_work_scales_linearly() {
    let make_source = |locals: usize| {
        let lets = (0..locals)
            .map(|index| format!("let v{index}: i32 = {index};"))
            .collect::<String>();
        source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                &format!(
                    "fn choose(comptime n: i32) -> i32 {{ {lets} if n == 0 {{ 1 }} else {{ 0 }} }}\nfn main() -> i32 {{ choose(0) }}\n"
                ),
            )],
            1,
        )
    };
    let measure = |source: SourceSnapshot| {
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = revision_for(&mut database, &source);
        let crate::FunctionInstanceKey::Definition(base) =
            free_function_instance(&module, "choose")
        else {
            unreachable!("free function helper returns a definition");
        };
        database
            .body_transaction(
                revision,
                crate::body_query::BodyQueryKey::new(
                    crate::FunctionInstanceKey::Specialization {
                        base: Node::new(crate::FunctionInstanceKey::Definition(base)),
                        arguments: crate::CanonicalArguments {
                            types: Arc::from([]),
                            values: Arc::from([crate::CanonicalArgumentValue::Integer(0)]),
                        },
                    },
                    semantic_configuration(),
                ),
                CancellationToken::new(),
            )
            .unwrap();
        database.provider_observation_metrics()
    };
    let small = measure(make_source(8));
    let large = measure(make_source(16));
    let work = |metrics: crate::unstable::ProviderObservationMetrics| {
        metrics
            .staged_probe_nodes
            .saturating_add(metrics.staged_frontier_bodies)
            .saturating_add(metrics.staged_resolved_instructions)
            .saturating_add(metrics.staged_fact_nodes)
            .saturating_add(metrics.staged_canonical_evaluations)
            .saturating_add(metrics.staged_constraints_generated)
            .saturating_add(metrics.staged_binding_scope_nodes)
            .saturating_add(metrics.staged_binding_materializations)
            .saturating_add(metrics.staged_binding_trie_updates)
            .saturating_add(metrics.staged_binding_trie_lookups)
            .saturating_add(metrics.staged_precompute_nodes)
    };
    assert_eq!(small.staged_frontier_bodies, 1);
    assert_eq!(large.staged_frontier_bodies, 1);
    assert_eq!(small.staged_binding_scope_nodes, 8);
    assert_eq!(large.staged_binding_scope_nodes, 16);
    assert_eq!(
        large.staged_probe_nodes - small.staged_probe_nodes,
        16,
        "probe work charges each newly reachable local exactly once"
    );
    assert_eq!(
        large.staged_constraints_generated - small.staged_constraints_generated,
        8,
        "frontier constraint work grows with the selected segment only"
    );
    assert_eq!(
        work(small),
        401,
        "the eight-local production specialization has a stable staged-work total"
    );
    assert_eq!(
        work(large),
        713,
        "the sixteen-local production specialization has a stable staged-work total"
    );
    assert!(
        work(large) >= work(small),
        "larger staged source must not do less work"
    );
    assert!(
        work(large) <= work(small).saturating_mul(2),
        "staged work must be linear: small={} large={}",
        work(small),
        work(large)
    );
}

#[test]
fn staged_non_generic_calls_do_not_materialize_scope() {
    let make_source = |locals_count: usize, calls_count: usize| {
        let locals = (0..locals_count)
            .map(|index| format!("let local_{index}: i32 = {index};"))
            .collect::<String>();
        let calls = (0..calls_count).map(|_| "leaf();").collect::<String>();
        source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                &format!(
                    "fn leaf() -> i32 {{ 1 }}\nfn choose(comptime n: i32) -> i32 {{ if n == 0 {{ {locals}{calls} 1 }} else {{ 0 }} }}\nfn main() -> i32 {{ choose(0) }}\n"
                ),
            )],
            1,
        )
    };
    let measure = |source: SourceSnapshot| {
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = revision_for(&mut database, &source);
        let crate::FunctionInstanceKey::Definition(base) =
            free_function_instance(&module, "choose")
        else {
            unreachable!("free function helper returns a definition");
        };
        let key = crate::body_query::BodyQueryKey::new(
            crate::FunctionInstanceKey::Specialization {
                base: Node::new(crate::FunctionInstanceKey::Definition(base)),
                arguments: crate::CanonicalArguments {
                    types: Arc::from([]),
                    values: Arc::from([crate::CanonicalArgumentValue::Integer(0)]),
                },
            },
            semantic_configuration(),
        );
        let result = database
            .body_transaction(revision, key, CancellationToken::new())
            .unwrap();
        assert!(matches!(
            result.outcome(),
            rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
        ));
        database.provider_observation_metrics()
    };
    let small = measure(make_source(8, 8));
    let more_calls = measure(make_source(8, 16));
    let large = measure(make_source(16, 16));
    assert_eq!(small.staged_binding_materializations, 2);
    assert_eq!(more_calls.staged_binding_materializations, 2);
    assert_eq!(large.staged_binding_materializations, 2);
    assert_eq!(
        small.staged_binding_trie_lookups, more_calls.staged_binding_trie_lookups,
        "ordinary calls must not perform additional runtime-name membership lookups"
    );
    assert_eq!(
        small.staged_binding_trie_updates, more_calls.staged_binding_trie_updates,
        "ordinary calls must not update the persistent runtime-name scope"
    );
    assert_eq!(small.staged_binding_scope_nodes, 8);
    assert_eq!(more_calls.staged_binding_scope_nodes, 8);
    assert_eq!(large.staged_binding_scope_nodes, 16);
    assert_eq!(small.staged_binding_trie_lookups, 66);
    assert_eq!(more_calls.staged_binding_trie_lookups, 66);
    assert_eq!(large.staged_binding_trie_lookups, 66);
    assert_eq!(
        more_calls.staged_probe_nodes - small.staged_probe_nodes,
        8,
        "adding eight ordinary calls charges only their own source nodes"
    );
    assert_eq!(
        more_calls.staged_fact_nodes - small.staged_fact_nodes,
        8,
        "ordinary calls do not add a scope walk to staged fact collection"
    );
    assert_eq!(
        large.staged_probe_nodes - more_calls.staged_probe_nodes,
        16,
        "doubling visible locals remains linear"
    );
    assert_eq!(
        large.staged_binding_trie_updates - more_calls.staged_binding_trie_updates,
        264,
        "only the eight additional locals update the persistent scope"
    );
    assert!(
        large.staged_probe_nodes > small.staged_probe_nodes,
        "the extra locals are charged as actual source work"
    );
}

#[test]
fn staged_nested_selector_prefix_work_scales_linearly() {
    let make_source = |depth: usize| {
        let mut expression = String::from("1");
        for level in (0..depth).rev() {
            expression = format!(
                "if n == 0 {{ let visible_{level}: i32 = {level}; {expression} }} else {{ 0 }}"
            );
        }
        source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                &format!(
                    "fn choose(comptime n: i32) -> i32 {{ {expression} }}\nfn main() -> i32 {{ choose(0) }}\n"
                ),
            )],
            1,
        )
    };
    let measure = |source: SourceSnapshot| {
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = revision_for(&mut database, &source);
        let crate::FunctionInstanceKey::Definition(base) =
            free_function_instance(&module, "choose")
        else {
            unreachable!("free function helper returns a definition");
        };
        let key = crate::body_query::BodyQueryKey::new(
            crate::FunctionInstanceKey::Specialization {
                base: Node::new(crate::FunctionInstanceKey::Definition(base)),
                arguments: crate::CanonicalArguments {
                    types: Arc::from([]),
                    values: Arc::from([crate::CanonicalArgumentValue::Integer(0)]),
                },
            },
            semantic_configuration(),
        );
        let result = database
            .body_transaction(revision, key, CancellationToken::new())
            .unwrap();
        assert!(matches!(
            result.outcome(),
            rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
        ));
        database.provider_observation_metrics()
    };
    let small = measure(make_source(4));
    let large = measure(make_source(8));
    let work = |metrics: crate::unstable::ProviderObservationMetrics| {
        metrics
            .staged_probe_nodes
            .saturating_add(metrics.staged_frontier_bodies)
            .saturating_add(metrics.staged_resolved_instructions)
            .saturating_add(metrics.staged_fact_nodes)
            .saturating_add(metrics.staged_canonical_evaluations)
            .saturating_add(metrics.staged_constraints_generated)
            .saturating_add(metrics.staged_binding_scope_nodes)
            .saturating_add(metrics.staged_binding_materializations)
            .saturating_add(metrics.staged_binding_trie_updates)
            .saturating_add(metrics.staged_binding_trie_lookups)
            .saturating_add(metrics.staged_precompute_nodes)
    };
    let small_work = work(small);
    let large_work = work(large);
    assert_eq!(small.staged_frontier_bodies, 4);
    assert_eq!(large.staged_frontier_bodies, 8);
    assert_eq!(small.staged_binding_scope_nodes, 4);
    assert_eq!(large.staged_binding_scope_nodes, 8);
    assert_eq!(small_work, 524);
    assert_eq!(large_work, 1048);
    assert_eq!(large_work, small_work.saturating_mul(2));
    assert!(
        large_work <= small_work.saturating_mul(2),
        "nested selector staging must be bounded-linear: small={small_work} large={large_work}"
    );
}

#[test]
fn staged_runtime_parameter_scope_is_inserted_once() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn choose(comptime n: i32, value: i32) -> i32 { if n == 0 { value } else { 0 } }\nfn main() -> i32 { choose(0, 7) }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let crate::FunctionInstanceKey::Definition(base) = free_function_instance(&module, "choose")
    else {
        unreachable!("free function helper returns a definition");
    };
    let key = crate::body_query::BodyQueryKey::new(
        crate::FunctionInstanceKey::Specialization {
            base: Node::new(crate::FunctionInstanceKey::Definition(base)),
            arguments: crate::CanonicalArguments {
                types: Arc::from([]),
                values: Arc::from([crate::CanonicalArgumentValue::Integer(0)]),
            },
        },
        semantic_configuration(),
    );
    let result = database
        .body_transaction(revision, key, CancellationToken::new())
        .unwrap();
    assert!(matches!(
        result.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
    ));
    let metrics = database.provider_observation_metrics();
    assert_eq!(metrics.staged_binding_scope_nodes, 1);
    assert_eq!(metrics.staged_binding_trie_updates, 33);
    assert_eq!(metrics.staged_binding_materializations, 2);
    assert_eq!(metrics.staged_binding_trie_lookups, 66);
}

#[test]
fn staged_frontier_constraint_cancellation_publishes_nothing_and_retry_is_identical() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn choose(comptime n: i8) -> i32 { if n == 1 { let a = 1; let b = 2; let c = 3; let d1 = 4; let d2 = 5; let d3 = 6; let d4 = 7; let d5 = 8; let d6 = 9; let d7 = 10; let d8 = 11; let d9 = 12; let d10 = 13; let d11 = 14; let d12 = 15; let d13 = 16; let d14 = 17; let d15 = 18; let d16 = 19; let d17 = 20; let d18 = 21; let d19 = 22; let d20 = 23; if n << 8 == n { a + b + c } else { 0 } } else { 0 } }\nfn main() -> i32 { choose(1) }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let crate::FunctionInstanceKey::Definition(base) = free_function_instance(&module, "choose")
    else {
        unreachable!("free function helper returns a definition")
    };
    let specialized = crate::FunctionInstanceKey::Specialization {
        base: Node::new(crate::FunctionInstanceKey::Definition(base)),
        arguments: crate::CanonicalArguments {
            types: Arc::from([]),
            values: Arc::from([crate::CanonicalArgumentValue::Integer(1)]),
        },
    };
    let key = crate::body_query::BodyQueryKey::new(specialized, semantic_configuration());
    database
        .body_input(revision, key.clone(), CancellationToken::new())
        .expect("the selector body input is available");
    let short_visits;
    let short_post_cancel_attempts;
    {
        let _injection = database.cancel_frontier_constraint_generation_after_nodes_for_test(12);
        let attempt = database.runtime.request_registered(
            &database.body_transactions,
            revision,
            key.clone(),
            CancellationToken::new(),
        );
        assert!(matches!(attempt.abort(), Some(QueryAbort::Canceled)));
        assert_eq!(database.constraint_generation_phase_for_test(), 1);
        assert!(database.constraint_generation_visits_for_test() >= 12);
        assert!(
            database.constraint_generation_visits_for_test() <= 24,
            "frontier cancellation must unwind promptly rather than visit the tail"
        );
        assert!(attempt.terminal().is_none());
        assert!(!database.body_transactions.contains_retained_key(&key));
        short_visits = database.constraint_generation_visits_for_test();
        short_post_cancel_attempts = database.constraint_generation_post_cancel_attempts_for_test();
    }
    let retry = database
        .body_transaction(revision, key.clone(), CancellationToken::new())
        .expect("uncanceled staged retry succeeds");
    assert!(matches!(
        retry.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
    ));

    // Compare the recovered transaction with a clean specialized query,
    // rather than merely checking that the retry reached a green terminal.
    let mut clean_database = RevisionedQueryDatabase::default();
    let clean_revision = revision_for(&mut clean_database, &source);
    let clean = clean_database
        .body_transaction(clean_revision, key, CancellationToken::new())
        .expect("clean specialized query succeeds");
    let rue_query::QueryOutcome::Success(retry_transaction) = retry.outcome() else {
        unreachable!("retry was checked above");
    };
    let rue_query::QueryOutcome::Success(clean_transaction) = clean.outcome() else {
        unreachable!("clean query succeeds");
    };
    assert!(crate::body_query::transaction_equal(
        retry_transaction,
        clean_transaction,
    ));

    // A canceled frontier must unwind its enclosing sibling loop rather
    // than continuing through an arbitrarily long unreachable tail.  The
    // same production specialized transaction with a thousand locals is
    // the adversarial source-sized witness for that bound.
    let long_tail = (0..1000)
        .map(|index| format!("let tail_{index}: i32 = {index};"))
        .collect::<String>();
    let long_source_text = format!(
        "fn choose(comptime n: i8) -> i32 {{ if n == 1 {{ {long_tail} if n << 8 == n {{ 1 }} else {{ 0 }} }} else {{ 0 }} }}\nfn main() -> i32 {{ choose(1) }}\n"
    );
    let long_source = source_snapshot(&[(1, "/main.rue", "main.rue", &long_source_text)], 1);
    let mut long_database = RevisionedQueryDatabase::default();
    let long_revision = revision_for(&mut long_database, &long_source);
    let crate::FunctionInstanceKey::Definition(long_base) =
        free_function_instance(&module, "choose")
    else {
        unreachable!("free function helper returns a definition")
    };
    let long_key = crate::body_query::BodyQueryKey::new(
        crate::FunctionInstanceKey::Specialization {
            base: Node::new(crate::FunctionInstanceKey::Definition(long_base)),
            arguments: crate::CanonicalArguments {
                types: Arc::from([]),
                values: Arc::from([crate::CanonicalArgumentValue::Integer(1)]),
            },
        },
        semantic_configuration(),
    );
    long_database
        .body_input(long_revision, long_key.clone(), CancellationToken::new())
        .expect("the long-tail selector body input is available");
    let long_visits;
    let long_post_cancel_attempts;
    {
        let _injection =
            long_database.cancel_frontier_constraint_generation_after_nodes_for_test(12);
        let attempt = long_database.runtime.request_registered(
            &long_database.body_transactions,
            long_revision,
            long_key,
            CancellationToken::new(),
        );
        assert!(matches!(attempt.abort(), Some(QueryAbort::Canceled)));
        assert!(attempt.terminal().is_none());
        long_visits = long_database.constraint_generation_visits_for_test();
        long_post_cancel_attempts =
            long_database.constraint_generation_post_cancel_attempts_for_test();
    }
    assert!(
        long_visits <= short_visits.saturating_add(2),
        "frontier cancellation visits must be tail-independent: short={short_visits} long={long_visits}"
    );
    assert!(
        long_post_cancel_attempts <= short_post_cancel_attempts.saturating_add(1),
        "canceled block tails must not be attempted: short={short_post_cancel_attempts} long={long_post_cancel_attempts}"
    );

    // Exercise the same production frontier transaction with the other
    // sibling-heavy generator paths.  Each case is canceled by the
    // frontier cgen injector, then retried and compared with an entirely
    // clean specialized transaction so cancellation cannot leave a tail
    // diagnostic, fact, or cached partial result behind.
    let run_frontier_case = |source_text: &str| -> (usize, usize) {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let mut database = RevisionedQueryDatabase::default();
        let revision = revision_for(&mut database, &source);
        let crate::FunctionInstanceKey::Definition(base) =
            free_function_instance(&module, "choose")
        else {
            unreachable!("free function helper returns a definition")
        };
        let key = crate::body_query::BodyQueryKey::new(
            crate::FunctionInstanceKey::Specialization {
                base: Node::new(crate::FunctionInstanceKey::Definition(base)),
                arguments: crate::CanonicalArguments {
                    types: Arc::from([]),
                    values: Arc::from([crate::CanonicalArgumentValue::Integer(1)]),
                },
            },
            semantic_configuration(),
        );
        database
            .body_input(revision, key.clone(), CancellationToken::new())
            .expect("frontier case body input is available");
        let (visits, post_cancel_attempts) = {
            let _injection =
                database.cancel_frontier_constraint_generation_after_nodes_for_test(12);
            let attempt = database.runtime.request_registered(
                &database.body_transactions,
                revision,
                key.clone(),
                CancellationToken::new(),
            );
            assert!(matches!(attempt.abort(), Some(QueryAbort::Canceled)));
            assert_eq!(database.constraint_generation_phase_for_test(), 1);
            assert!(database.constraint_generation_visits_for_test() >= 12);
            assert!(
                database.constraint_generation_visits_for_test() <= 24,
                "frontier cgen cancellation must unwind sibling tails"
            );
            assert!(attempt.terminal().is_none());
            assert!(!database.body_transactions.contains_retained_key(&key));
            let _total_attempts = database.constraint_generation_attempted_siblings_for_test();
            (
                database.constraint_generation_visits_for_test(),
                database.constraint_generation_post_cancel_attempts_for_test(),
            )
        };
        let retry = database
            .body_transaction(revision, key.clone(), CancellationToken::new())
            .expect("frontier cancellation retry succeeds");

        let mut clean_database = RevisionedQueryDatabase::default();
        let clean_revision = revision_for(&mut clean_database, &source);
        let clean = clean_database
            .body_transaction(clean_revision, key, CancellationToken::new())
            .expect("clean frontier case succeeds");
        let rue_query::QueryOutcome::Success(retry_transaction) = retry.outcome() else {
            unreachable!("frontier retry succeeds")
        };
        let rue_query::QueryOutcome::Success(clean_transaction) = clean.outcome() else {
            unreachable!("clean frontier case succeeds")
        };
        assert!(crate::body_query::transaction_equal(
            retry_transaction,
            clean_transaction,
        ));
        (visits, post_cancel_attempts)
    };

    // A function call with a thousand well-typed arguments exercises the
    // ordinary call argument loop (and keeps the source valid on retry).
    let short_call_params = "a0: i32, a1: i32";
    let short_call_args = "0, 0";
    let short_call_source = format!(
        "fn leaf({short_call_params}) -> i32 {{ 0 }}\nfn choose(comptime n: i8) -> i32 {{ if n == 1 {{ leaf({short_call_args}); 1 }} else {{ 0 }} }}\nfn main() -> i32 {{ choose(1) }}\n"
    );
    let long_call_params = (0..1000)
        .map(|index| format!("a{index}: i32"))
        .collect::<Vec<_>>()
        .join(", ");
    let long_call_args = (0..1000).map(|_| "0").collect::<Vec<_>>().join(", ");
    let long_call_source = format!(
        "fn leaf({long_call_params}) -> i32 {{ 0 }}\nfn choose(comptime n: i8) -> i32 {{ if n == 1 {{ leaf({long_call_args}); 1 }} else {{ 0 }} }}\nfn main() -> i32 {{ choose(1) }}\n"
    );
    let (short_call_visits, short_call_post_cancel) = run_frontier_case(&short_call_source);
    let (long_call_visits, long_call_post_cancel) = run_frontier_case(&long_call_source);
    assert!(
        long_call_visits <= short_call_visits.saturating_add(2),
        "frontier call cancellation visits must be tail-independent: short={short_call_visits} long={long_call_visits}"
    );
    assert!(
        long_call_post_cancel <= short_call_post_cancel.saturating_add(1),
        "canceled call argument tails must not be attempted: short={short_call_post_cancel} long={long_call_post_cancel}"
    );

    // Array literals use the aggregate element loop.  The first selected
    // element is enough to enter the frontier; later elements must not be
    // visited after the cgen cancellation token is set.
    let short_elements = "0, 0";
    let short_array_source = format!(
        "fn choose(comptime n: i8) -> i32 {{ if n == 1 {{ let values = [{short_elements}]; 1 }} else {{ 0 }} }}\nfn main() -> i32 {{ choose(1) }}\n"
    );
    let long_elements = (0..1000).map(|_| "0").collect::<Vec<_>>().join(", ");
    let long_array_source = format!(
        "fn choose(comptime n: i8) -> i32 {{ if n == 1 {{ let values = [{long_elements}]; 1 }} else {{ 0 }} }}\nfn main() -> i32 {{ choose(1) }}\n"
    );
    let (short_array_visits, short_array_post_cancel) = run_frontier_case(&short_array_source);
    let (long_array_visits, long_array_post_cancel) = run_frontier_case(&long_array_source);
    assert!(
        long_array_visits <= short_array_visits.saturating_add(2),
        "frontier array cancellation visits must be tail-independent: short={short_array_visits} long={long_array_visits}"
    );
    assert!(
        long_array_post_cancel <= short_array_post_cancel.saturating_add(1),
        "canceled array tails must not be attempted: short={short_array_post_cancel} long={long_array_post_cancel}"
    );
}

#[test]
fn body_closure_edge_addition_and_deletion_publish_exact_reached_sets() {
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let source = |main_body: &str| {
        source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                &format!(
                    "fn leaf() -> i32 {{ 1 }}\n\
                         fn stable() -> i32 {{ 2 }}\n\
                         fn added() -> i32 {{ 3 }}\n\
                         fn main() -> i32 {{ {main_body} }}\n"
                ),
            )],
            1,
        )
    };
    let first_source = source("leaf() + stable()");
    let added_source = source("leaf() + stable() + added()");
    let deleted_source = source("stable() + added()");
    let configuration = semantic_configuration();
    let closure_key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "main")]),
        configuration,
    };
    let leaf = free_function_instance(&module, "leaf");
    let stable = free_function_instance(&module, "stable");
    let added = free_function_instance(&module, "added");
    let mut database = RevisionedQueryDatabase::default();

    let first_revision = revision_for(&mut database, &first_source);
    let first = database
        .body_closure(
            first_revision,
            closure_key.clone(),
            CancellationToken::new(),
        )
        .unwrap();
    let first_root_metrics = database.body_closure_root_metrics();

    let added_revision = revision_for(&mut database, &added_source);
    let added_closure = database
        .body_closure(
            added_revision,
            closure_key.clone(),
            CancellationToken::new(),
        )
        .unwrap();
    let added_root_metrics = database.body_closure_root_metrics();

    let deleted_revision = revision_for(&mut database, &deleted_source);
    let deleted_closure = database
        .body_closure(
            deleted_revision,
            closure_key.clone(),
            CancellationToken::new(),
        )
        .unwrap();
    let deleted_root_metrics = database.body_closure_root_metrics();

    // Exact membership is the published contract, which is what this
    // compares. The sequence `reached` happens to arrive in is the identity
    // total order, an implementation detail of the key comparator; pinning
    // it here made this test fail for a comparator change that published
    // the same closure.
    let reached = |request: &BodyClosureRequest| {
        let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        output.reached.iter().cloned().collect::<BTreeSet<_>>()
    };
    assert_eq!(
        reached(&first),
        BTreeSet::from([
            leaf.clone(),
            free_function_instance(&module, "main"),
            stable.clone(),
        ])
    );
    assert_eq!(
        reached(&added_closure),
        BTreeSet::from([
            added.clone(),
            leaf.clone(),
            free_function_instance(&module, "main"),
            stable.clone(),
        ])
    );
    assert_eq!(
        reached(&deleted_closure),
        BTreeSet::from([
            added.clone(),
            free_function_instance(&module, "main"),
            stable.clone(),
        ])
    );
    assert_eq!(
        (first_root_metrics.1, first_root_metrics.2),
        (3, 0),
        "the cold root accounts its exact reached membership as additions"
    );
    assert_eq!(
        (added_root_metrics.1, added_root_metrics.2),
        (4, 0),
        "adding one call edge accounts one independent membership addition"
    );
    assert_eq!(
        (deleted_root_metrics.1, deleted_root_metrics.2),
        (4, 1),
        "deleting one call edge accounts one independent membership deletion"
    );
}

#[test]
fn body_closure_call_scc_is_a_finite_graph_not_a_query_cycle() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn a(value: i32) -> i32 { if value == 0 { 0 } else { b(value - 1) } }\n\
                 fn b(value: i32) -> i32 { if value == 0 { 0 } else { a(value - 1) } }\n\
                 fn main() -> i32 { a(2) }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::with_query_concurrency(1);
    let revision = revision_for(&mut database, &snapshot);
    let closure = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "main")]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("ordinary call SCCs terminate through visited graph membership");
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert_eq!(output.reached.len(), 3);
    assert!(output.fatal.is_none());
    assert!(output.scheduling_errors.is_empty());
}

#[test]
fn single_worker_toolchain_park_aggregates_the_complete_ready_frontier() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn parse() -> i32 { let _value = @parse_u32(\"1\"); 0 }\n\
                 fn read() -> i32 { let _value = @read_line(); 0 }\n\
                 fn main() -> i32 { parse() + read() }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::with_query_concurrency(1);
    let revision = revision_for(&mut database, &snapshot);
    let closure = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "main")]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("the closure publishes one aggregate toolchain park");
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    let parked = output
        .parked_toolchain
        .as_ref()
        .expect("both ready siblings require absent trusted modules");
    let paths = parked
        .demands()
        .iter()
        .map(crate::TrustedToolchainModuleDemand::logical_path)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        paths,
        BTreeSet::from([
            crate::OPTION_MODULE_LOGICAL_PATH,
            crate::STRBUF_MODULE_LOGICAL_PATH,
        ]),
        "the one-worker path must inspect siblings already promoted out of pending"
    );
    assert_eq!(
        parked.requesters().len(),
        2,
        "one park retains both exact requesting bodies"
    );
}

#[test]
fn body_closure_one_and_many_workers_publish_identical_reached_work_and_diagnostics() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn a() -> i32 { 1 }\n\
                 fn b() -> i32 { 2 }\n\
                 fn c() -> i32 { 3 }\n\
                 fn d() -> i32 { 4 }\n\
                 struct Dead { value: i64 }\n\
                 drop fn Dead(self) { missing_named(); }\n\
                 fn DeadAnonymous() -> type { struct { value: i64, drop fn(self) { missing_anonymous(); } } }\n\
                 fn main() -> i32 { a() + b() + c() + d() }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "main")]),
        configuration: semantic_configuration(),
    };
    let run = |workers| {
        let mut database = RevisionedQueryDatabase::with_query_concurrency(workers);
        let revision = revision_for(&mut database, &snapshot);
        let request = database
            .body_closure(revision, key.clone(), CancellationToken::new())
            .unwrap();
        let work = request
            .body_executions
            .values()
            .filter(|execution| **execution == rue_query::RequestExecution::Computed)
            .count();
        let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        assert_eq!(
            output.reached.len(),
            5,
            "invalid unreachable destructors must not become roots"
        );
        assert!(
            output.demanded_drop_glue_plans.is_empty(),
            "unowned unreachable types must demand no glue"
        );
        assert!(output.scheduling_errors.is_empty());
        assert!(output.fatal.is_none());
        (
            output.reached.to_vec(),
            output.scheduling_errors.clone(),
            output.fatal.clone(),
            output.demanded_drop_glue_plans.clone(),
            work,
        )
    };
    assert_eq!(run(1), run(4));
}

#[test]
fn body_reachability_scans_each_prefetched_frontier_once() {
    const CALLEES: usize = 16;
    let mut text = (0..CALLEES)
        .map(|index| format!("fn f{index}() -> i32 {{ {index} }}\n"))
        .collect::<String>();
    let expression = (0..CALLEES)
        .map(|index| format!("f{index}()"))
        .collect::<Vec<_>>()
        .join(" + ");
    text.push_str(&format!("fn main() -> i32 {{ {expression} }}\n"));
    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
    let revision = revision_for(&mut database, &snapshot);
    let request = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "main")]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("body closure publishes");
    let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert_eq!(output.reached.len(), CALLEES + 1);
    let work = |expected: &str| {
        request
            .work
            .iter()
            .find_map(|(label, amount)| (label.as_ref() == expected).then_some(*amount))
            .unwrap_or(0)
    };
    assert!(
        work("reachability.frontier.scans") < output.reached.len() as u64
            && work("reachability.frontier.scan-keys") <= output.reached.len() as u64 + 8,
        "wide prefetched bodies must be consumed without a per-body pending-set rescan; \
             got {} scans and {} scanned keys for {} reached bodies",
        work("reachability.frontier.scans"),
        work("reachability.frontier.scan-keys"),
        output.reached.len(),
    );
    assert_eq!(work("reachability.frontier.batches"), 2);
    assert_eq!(work("reachability.frontier.keys"), (CALLEES + 1) as u64);
    assert_eq!(work("reachability.frontier.width-1"), 1);
    assert_eq!(work("reachability.frontier.width-2-3"), 0);
    assert_eq!(work("reachability.frontier.width-4-7"), 0);
    assert_eq!(work("reachability.frontier.width-8-plus"), 1);
    assert_eq!(
        work("reachability.transactions.prefetched"),
        (CALLEES + 1) as u64
    );
    assert_eq!(work("reachability.transactions.serial"), 0);
}

#[test]
fn anonymous_producer_closures_are_derived_once_per_instance() {
    // RUE-1557: reachability walks a key's anonymous-nominal graph to
    // schedule the producers it names, and walks it again on every frontier
    // scan to ask whether those producers are visited. The graph is
    // statically encoded and cannot move within a request, so the second
    // use re-derived a closure the first had already computed — once per
    // pending body per scan round.
    //
    // Several producers keep the frontier alive for more than one round,
    // which is what made the per-round re-walk visible.
    const PRODUCERS: usize = 8;
    let mut text = (0..PRODUCERS)
        .map(|index| format!("fn P{index}() -> type {{ struct {{ x{index}: i32 }} }}\n"))
        .collect::<String>();
    text.push_str("fn main() -> i32 {\n");
    for index in 0..PRODUCERS {
        text.push_str(&format!("    let T{index} = P{index}();\n"));
        text.push_str(&format!(
            "    let v{index}: T{index} = T{index} {{ x{index}: {index} }};\n"
        ));
    }
    let sum = (0..PRODUCERS)
        .map(|index| format!("v{index}.x{index}"))
        .collect::<Vec<_>>()
        .join(" + ");
    text.push_str(&format!("    {sum}\n}}\n"));
    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "main")]),
        configuration: semantic_configuration(),
    };

    let run = |workers| {
        let mut database = RevisionedQueryDatabase::with_query_concurrency(workers);
        let revision = revision_for(&mut database, &snapshot);
        let request = database
            .body_closure(revision, key.clone(), CancellationToken::new())
            .expect("anonymous producers publish one body closure");
        let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        assert!(output.fatal.is_none());
        assert!(output.scheduling_errors.is_empty());
        let work = |expected: &str| {
            request
                .work
                .iter()
                .find_map(|(label, amount)| (label.as_ref() == expected).then_some(*amount))
                .unwrap_or(0)
        };
        (
            output.reached.len(),
            work("reachability.anonymous.closure-walks"),
            work("reachability.frontier.scans"),
        )
    };

    for workers in [1, 4] {
        let (reached, walks, scans) = run(workers);
        assert_eq!(reached, PRODUCERS + 1, "every producer body is reached");
        assert!(
            scans > 1,
            "the fixture must take more than one frontier scan to be a regression test \
                 for per-scan re-walking, got {scans}"
        );
        assert!(
            walks <= reached as u64,
            "with {workers} worker(s) each of the {reached} instances must have its \
                 anonymous graph walked at most once, got {walks} walks across {scans} scans"
        );
    }

    // With the closure derived once per instance the count no longer tracks
    // how many scan rounds a schedule happened to take, so it is identical
    // whatever the worker count.
    assert_eq!(
        run(1),
        run(4),
        "closure accounting must be identical across worker counts"
    );
}

#[test]
fn each_body_toolchain_demand_is_queried_once_per_reachability_request() {
    // RUE-1562: reachability reads a body's trusted-toolchain demand from
    // more than one place — the prefetch batch reads the frontier window,
    // and the parking sweep walks the whole frontier, which contains that
    // window. Each body's demand is a pure function of its key at this
    // revision, so one evaluation-scoped cache answers every repeat and no
    // body is queried twice.
    //
    // A wide frontier is the case that made the duplication visible: the
    // batch and the sweep overlap by the whole prefetch window.
    const CALLEES: usize = 24;
    let mut text = (0..CALLEES)
        .map(|index| format!("fn f{index}() -> i32 {{ {index} }}\n"))
        .collect::<String>();
    let expression = (0..CALLEES)
        .map(|index| format!("f{index}()"))
        .collect::<Vec<_>>()
        .join(" + ");
    text.push_str(&format!("fn main() -> i32 {{ {expression} }}\n"));
    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "main")]),
        configuration: semantic_configuration(),
    };

    let run = |workers| {
        let mut database = RevisionedQueryDatabase::with_query_concurrency(workers);
        let revision = revision_for(&mut database, &snapshot);
        let request = database
            .body_closure(revision, key.clone(), CancellationToken::new())
            .expect("a wide call fan-out publishes one body closure");
        let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        assert!(output.fatal.is_none());
        let queries = request
            .work
            .iter()
            .find_map(|(label, amount)| {
                (label.as_ref() == "reachability.toolchain-demand.queries").then_some(*amount)
            })
            .unwrap_or(0);
        (output.reached.len(), queries)
    };

    for workers in [1, 4] {
        let (reached, queries) = run(workers);
        assert_eq!(reached, CALLEES + 1, "every callee is reached");
        assert_eq!(
            queries, reached as u64,
            "with {workers} worker(s) each of the {reached} reached bodies must have its \
                 toolchain demand queried exactly once, got {queries} queries"
        );
    }

    // The counter must not depend on how the work was scheduled: the serial
    // and parallel paths reach the demand through different call sites, and
    // a published work ledger that disagreed across worker counts would be
    // a determinism break rather than a measurement.
    assert_eq!(
        run(1),
        run(4),
        "demand accounting must be identical across worker counts"
    );
}

#[test]
fn ready_anonymous_producers_share_one_structured_frontier() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn L() -> type { struct { x: i32 } }\n\
                 fn R() -> type { struct { x: i32 } }\n\
                 fn main() -> i32 {\n\
                     let TL = L();\n\
                     let TR = R();\n\
                     let a: TL = TL { x: 40 };\n\
                     let b: TR = TR { x: 2 };\n\
                     a.x + b.x\n\
                 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "main")]),
        configuration: semantic_configuration(),
    };
    let run = |workers| {
        let mut database = RevisionedQueryDatabase::with_query_concurrency(workers);
        let revision = revision_for(&mut database, &snapshot);
        let request = database
            .body_closure(revision, key.clone(), CancellationToken::new())
            .expect("branching anonymous producers publish one body closure");
        let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        assert!(output.fatal.is_none());
        assert!(output.scheduling_errors.is_empty());
        (output.reached.to_vec(), request.work)
    };
    let one_worker = run(1);
    let many_workers = run(4);
    assert_eq!(one_worker, many_workers);
    assert_eq!(one_worker.0.len(), 3);
    let work = |expected: &str| {
        one_worker
            .1
            .iter()
            .find_map(|(label, amount)| (label.as_ref() == expected).then_some(*amount))
            .unwrap_or(0)
    };
    assert_eq!(work("reachability.frontier.batches"), 2);
    assert_eq!(work("reachability.frontier.keys"), 3);
    assert_eq!(work("reachability.frontier.width-1"), 1);
    assert_eq!(work("reachability.frontier.width-2-3"), 1);
    assert_eq!(work("reachability.transactions.prefetched"), 3);
    assert_eq!(work("reachability.transactions.serial"), 0);
}

#[test]
fn injected_body_transaction_failure_runs_in_the_structured_frontier() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn first() -> i32 { 0 }\nfn second() -> i32 { 1 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    // `RevisionedQueryDatabase::body_closure` sorts and dedups roots before
    // dispatch, and the evaluator asserts it received them that way. This
    // fixture goes straight at the family, so it canonicalises the same way
    // rather than relying on the spelling order happening to be ascending.
    let mut roots = [
        free_function_instance(&module, "first"),
        free_function_instance(&module, "second"),
    ];
    roots.sort();
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
    let revision = revision_for(&mut database, &snapshot);
    let _injection = database.inject_body_transaction_failure_for_test();
    let attempt = database.runtime.request_registered(
        &database.body_reachability,
        revision,
        crate::body_query::BodyClosureQueryKey {
            modules: Arc::from([module.clone()]),
            roots: Arc::from(roots.clone()),
            configuration: semantic_configuration(),
        },
        CancellationToken::new(),
    );
    let terminal = attempt
        .terminal()
        .expect("injected failure publishes reachability");
    let rue_query::QueryOutcome::Success(output) = terminal.outcome() else {
        unreachable!("BodyReachability publishes typed values")
    };
    assert_eq!(output.reached.len(), 2);
    let work = |expected: &str| {
        attempt
            .work()
            .iter()
            .find_map(|(label, amount)| (label.as_ref() == expected).then_some(*amount))
            .unwrap_or(0)
    };
    assert_eq!(
        work("reachability.frontier.keys"),
        2,
        "failure injection remains visible in the production structured child"
    );
    assert!(
        work("reachability.frontier.scans") >= 1,
        "the injected attempt enters the ordinary frontier scanner"
    );
    for instance in roots {
        let transaction = database.runtime.request_registered(
            &database.body_transactions,
            revision,
            crate::body_query::BodyQueryKey::new(instance, semantic_configuration()),
            CancellationToken::new(),
        );
        assert_eq!(
            transaction.execution(),
            rue_query::RequestExecution::Reused,
            "each structured child publishes its injected transaction"
        );
        let terminal = transaction.terminal().expect("transaction publishes");
        assert!(matches!(
            terminal.outcome(),
            rue_query::QueryOutcome::Success(
                crate::body_query::BodyTransaction::DeterministicFailure { .. }
            )
        ));
    }
}

#[test]
fn body_closure_root_pins_reached_programs_past_the_history_floor_and_releases_deletions() {
    const CALLEES: usize = 16;
    let source = |reached_callees: usize| {
        let mut text = (0..CALLEES)
            .map(|index| format!("fn f{index}() -> i32 {{ {index} }}\n"))
            .collect::<String>();
        let expression = (0..reached_callees)
            .map(|index| format!("f{index}()"))
            .collect::<Vec<_>>()
            .join(" + ");
        text.push_str(&format!("fn main() -> i32 {{ {expression} }}\n"));
        source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1)
    };
    let full = source(CALLEES);
    let reduced = source(CALLEES / 2);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let closure_key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "main")]),
        configuration: semantic_configuration(),
    };
    let deleted_body_key = crate::body_query::BodyQueryKey::new(
        free_function_instance(&module, &format!("f{}", CALLEES - 1)),
        semantic_configuration(),
    );
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);

    let full_revision = revision_for(&mut database, &full);
    let cold = database
        .body_closure(full_revision, closure_key.clone(), CancellationToken::new())
        .unwrap();
    let rue_query::QueryOutcome::Success(cold_output) = cold.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert_eq!(cold_output.reached.len(), CALLEES + 1);
    assert!(
        database.body_transactions.retention().terminals > CALLEES,
        "the exact published root must grow body retention past its {}-terminal history floor",
        BODY_QUERY_MEMO_RETENTION
    );
    assert!(
        database
            .body_transactions
            .contains_retained_key(&deleted_body_key)
    );
    let cold_root = database.body_closure_root_metrics();
    assert_eq!((cold_root.1, cold_root.2), ((CALLEES + 1) as u64, 0));

    let warm = database
        .body_closure(full_revision, closure_key.clone(), CancellationToken::new())
        .unwrap();
    assert_eq!(database.body_closure_root_metrics(), cold_root);
    assert!(
        warm.body_executions.is_empty(),
        "the revision-scoped validation memo skips the already verified body closure"
    );

    let reduced_revision = revision_for(&mut database, &reduced);
    let canceled = CancellationToken::new();
    canceled.cancel();
    assert!(matches!(
        database.body_closure(reduced_revision, closure_key.clone(), canceled),
        Err(QueryAbort::Canceled)
    ));
    assert_eq!(
        database.body_closure_root_metrics(),
        cold_root,
        "an aborted successor must roll back without replacing the published root"
    );
    let reduced_request = database
        .body_closure(reduced_revision, closure_key, CancellationToken::new())
        .unwrap();
    let rue_query::QueryOutcome::Success(reduced_output) = reduced_request.terminal.outcome()
    else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert_eq!(reduced_output.reached.len(), CALLEES / 2 + 1);
    let reduced_root = database.body_closure_root_metrics();
    assert_eq!(
        (reduced_root.1, reduced_root.2),
        ((CALLEES + 1) as u64, (CALLEES / 2) as u64)
    );
    assert!(
        reduced_root.0 < cold_root.0,
        "replacing the root must release body-specific leases for deleted membership"
    );
    assert!(
        !database
            .body_transactions
            .contains_retained_key(&deleted_body_key),
        "the deleted predecessor body terminal must become unpinned and evict, \
             proving validation observations outside the successor cone are not rooted"
    );
}

/// Focused, opt-in latency witness for RUE-1028. This is deliberately not a
/// threshold test: it prints independently timed cold, warm-validation, and
/// edge-deletion closure requests so release engineering can compare the
/// same corpus across commits without mixing setup or source construction
/// into the measured interval.
#[test]
#[ignore]
fn body_closure_cold_warm_deletion_latency_benchmark() {
    const CALLEES: usize = 128;
    let source = |reached_callees: usize| {
        let mut text = (0..CALLEES)
            .map(|index| format!("fn f{index}() -> i32 {{ {index} }}\n"))
            .collect::<String>();
        let expression = (0..reached_callees)
            .map(|index| format!("f{index}()"))
            .collect::<Vec<_>>()
            .join(" + ");
        text.push_str(&format!("fn main() -> i32 {{ {expression} }}\n"));
        source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1)
    };
    let full = source(CALLEES);
    let reduced = source(CALLEES / 2);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let closure_key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "main")]),
        configuration: semantic_configuration(),
    };
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
    let full_revision = revision_for(&mut database, &full);

    let cold_start = std::time::Instant::now();
    let cold = database
        .body_closure(full_revision, closure_key.clone(), CancellationToken::new())
        .unwrap();
    let cold_micros = cold_start.elapsed().as_micros();

    let warm_start = std::time::Instant::now();
    let warm = database
        .body_closure(full_revision, closure_key.clone(), CancellationToken::new())
        .unwrap();
    let warm_micros = warm_start.elapsed().as_micros();

    let reduced_revision = revision_for(&mut database, &reduced);
    let deletion_start = std::time::Instant::now();
    let deletion = database
        .body_closure(reduced_revision, closure_key, CancellationToken::new())
        .unwrap();
    let deletion_micros = deletion_start.elapsed().as_micros();

    let reached = |request: &BodyClosureRequest| {
        let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        output.reached.len()
    };
    assert_eq!(reached(&cold), CALLEES + 1);
    assert_eq!(reached(&warm), CALLEES + 1);
    assert_eq!(reached(&deletion), CALLEES / 2 + 1);
    eprintln!(
        "RUE-1028 body-closure latency: bodies={} workers=4 cold_us={} warm_us={} deletion_us={}",
        CALLEES + 1,
        cold_micros,
        warm_micros,
        deletion_micros
    );
}

fn assert_forced_body_closure_digest_collision(first_body: &str, second_body: &str) {
    let source = format!(
        "fn First() -> type {{ {first_body} }}\n\
             fn Second() -> type {{ {second_body} }}\n"
    );
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", &source)], 1);
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let configuration = semantic_configuration();
    let instance = |name| crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, name)),
        arguments: crate::CanonicalArguments::default(),
    };
    let first_instance = instance("First");
    let second_instance = instance("Second");
    let body_key = |instance| crate::body_query::BodyQueryKey::new(instance, configuration.clone());
    let first_key = body_key(first_instance.clone());
    let second_key = body_key(second_instance.clone());

    // Identity discovery is deliberately isolated from the system under
    // test. Stable identities are request-independent, so a probe database
    // can discover the exact forcing keys while the real database remains
    // completely cold until its closure request.
    let mut probe = RevisionedQueryDatabase::default();
    let probe_revision = revision_for(&mut probe, &snapshot);
    let produced_identity = |key: crate::body_query::BodyQueryKey| {
        let terminal = probe
            .body_produced_anonymous_projection(probe_revision, key, CancellationToken::new())
            .expect("registered producer body publishes its anonymous projection");
        let rue_query::QueryOutcome::Success(crate::body_query::ProducedAnonymous::Produced(
            produced,
        )) = terminal.outcome()
        else {
            panic!("producer body must publish anonymous nominals")
        };
        assert_eq!(produced.0.len(), 1);
        produced.0[0].identity.clone()
    };
    let first_identity = produced_identity(first_key.clone());
    let second_identity = produced_identity(second_key.clone());
    assert_ne!(first_identity, second_identity);

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let forced_digest = 0x1191;
    database.force_body_closure_anonymous_digest_for_test(first_identity.clone(), forced_digest);
    database.force_body_closure_anonymous_digest_for_test(second_identity.clone(), forced_digest);

    let closure_key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module]),
        roots: Arc::from([second_instance, first_instance]),
        configuration: configuration.clone(),
    };
    let cold = database
        .body_closure(revision, closure_key.clone(), CancellationToken::new())
        .expect("body closure publishes a typed forced-collision failure");
    for key in [&first_key, &second_key] {
        assert_eq!(
            cold.execution_for(key),
            rue_query::RequestExecution::Computed,
            "the cold closure must compute each producer transaction"
        );
        assert!(!cold.was_retained(key));
    }
    let rue_query::QueryOutcome::Success(output) = cold.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert_collision_fatal(
        output,
        forced_digest,
        first_identity.clone(),
        second_identity.clone(),
    );
    assert_eq!(
        output.bodies.len(),
        2,
        "the collision must be reconciled across two separate registered body transactions"
    );
    assert_eq!(
        output
            .bodies
            .iter()
            .map(|body| body.key.stable_identity())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first_key.stable_identity(), second_key.stable_identity()])
    );
    let warm = database
        .body_closure(revision, closure_key.clone(), CancellationToken::new())
        .expect("warm body closure reuses the typed forced-collision failure");
    assert!(Arc::ptr_eq(&cold.terminal, &warm.terminal));
    for key in [&first_key, &second_key] {
        assert_eq!(warm.execution_for(key), rue_query::RequestExecution::Reused);
        assert!(warm.was_retained(key));
    }

    let unchanged_revision = revision_for(&mut database, &snapshot);
    assert_ne!(unchanged_revision, revision);
    let unchanged = database
        .body_closure(unchanged_revision, closure_key, CancellationToken::new())
        .expect("unchanged successor revision retains the collision failure");
    let rue_query::QueryOutcome::Success(unchanged_output) = unchanged.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert_eq!(unchanged_output.fatal, output.fatal);
    for key in [&first_key, &second_key] {
        assert_eq!(
            unchanged.execution_for(key),
            rue_query::RequestExecution::Reused,
            "unchanged successor revision must reuse each producer transaction"
        );
        assert!(unchanged.was_retained(key));
    }
}

#[test]
fn body_closure_rejects_forced_struct_struct_digest_collision() {
    assert_forced_body_closure_digest_collision("struct { first: i32 }", "struct { second: bool }");
}

#[test]
fn body_closure_rejects_forced_enum_enum_digest_collision() {
    assert_forced_body_closure_digest_collision(
        "enum { First(i32), Empty }",
        "enum { Second(bool), Empty }",
    );
}

#[test]
fn body_closure_rejects_forced_struct_enum_digest_collision() {
    assert_forced_body_closure_digest_collision(
        "struct { first: i32 }",
        "enum { Second(bool), Empty }",
    );
}

fn projected_anonymous_nominals(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    snapshot: &SourceSnapshot,
) -> Arc<[crate::durable_semantics::DurableAnonymousNominal]> {
    let merged = crate::test_support::test_merged_program(snapshot).unwrap();
    database
        .projected_declaration_semantics(
            revision,
            merged.ast(),
            rue_target::Target::X86_64Linux,
            &crate::PreviewFeatures::default(),
            CancellationToken::new(),
        )
        .expect("declaration semantics project")
        .anonymous_nominals
}

fn assert_collision_fatal(
    output: &crate::body_query::BodyClosureOutput,
    digest: u128,
    left: crate::AnonymousNominalKey,
    right: crate::AnonymousNominalKey,
) {
    let left = left.with_canonical_producer().into_owned();
    let right = right.with_canonical_producer().into_owned();
    let (first, second) = if left < right {
        (left, right)
    } else {
        (right, left)
    };
    assert_eq!(
        output.fatal,
        Some(
            crate::body_query::BodyClosureFatal::AnonymousDigestCollision {
                digest,
                first,
                second,
            }
        )
    );
}

#[test]
fn body_closure_rejects_forced_declaration_declaration_digest_collision() {
    let source = "fn First() -> type { struct { first: i32 } }\n\
                      fn Second() -> type { enum { Second(bool), Empty } }\n\
                      struct FirstHolder { value: First() }\n\
                      struct SecondHolder { value: Second() }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let projected = projected_anonymous_nominals(&database, revision, &snapshot);
    assert_eq!(projected.len(), 2);
    let first = projected[0].identity.clone();
    let second = projected[1].identity.clone();
    let digest = 0xdede;
    database.force_body_closure_anonymous_digest_for_test(first.clone(), digest);
    database.force_body_closure_anonymous_digest_for_test(second.clone(), digest);

    let closure = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module]),
                roots: Arc::from([]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("declaration-only closure publishes typed collision failure");
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert_collision_fatal(output, digest, first, second);
    assert!(output.bodies.is_empty());
}

#[test]
fn body_closure_rejects_forced_declaration_body_digest_collision() {
    let source = "fn Declared() -> type { struct { declared: i32 } }\n\
                      fn Produced() -> type { enum { Produced(bool), Empty } }\n\
                      struct Holder { value: Declared() }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let projected = projected_anonymous_nominals(&database, revision, &snapshot);
    assert_eq!(projected.len(), 1);
    let declaration_identity = projected[0].identity.clone();
    let produced_instance = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Produced")),
        arguments: crate::CanonicalArguments::default(),
    };
    let produced_key =
        crate::body_query::BodyQueryKey::new(produced_instance.clone(), semantic_configuration());
    let produced = database
        .body_produced_anonymous_projection(
            revision,
            produced_key.clone(),
            CancellationToken::new(),
        )
        .expect("body producer publishes anonymous facts");
    let rue_query::QueryOutcome::Success(crate::body_query::ProducedAnonymous::Produced(produced)) =
        produced.outcome()
    else {
        panic!("body producer must succeed")
    };
    assert_eq!(produced.0.len(), 1);
    let body_identity = produced.0[0].identity.clone();
    let digest = 0xdbdb;
    database.force_body_closure_anonymous_digest_for_test(declaration_identity.clone(), digest);
    database.force_body_closure_anonymous_digest_for_test(body_identity.clone(), digest);

    let closure = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module]),
                roots: Arc::from([produced_instance]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("mixed declaration/body closure publishes typed collision failure");
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert_collision_fatal(output, digest, declaration_identity, body_identity);
    assert_eq!(output.bodies.len(), 1);
    assert_eq!(
        closure.execution_for(&produced_key),
        rue_query::RequestExecution::Computed
    );
}

#[test]
fn body_closure_parked_outcome_precedes_an_already_observed_collision() {
    let source = "fn First() -> type { struct { first: i32 } }\n\
                      fn Second() -> type { enum { Second(bool), Empty } }\n\
                      struct FirstHolder { value: First() }\n\
                      struct SecondHolder { value: Second() }\n\
                      fn main() -> i32 { let _value = @parse_u32(\"1\"); 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let projected = projected_anonymous_nominals(&database, revision, &snapshot);
    assert_eq!(projected.len(), 2);
    database.force_body_closure_anonymous_digest_for_test(projected[0].identity.clone(), 0xfeed);
    database.force_body_closure_anonymous_digest_for_test(projected[1].identity.clone(), 0xfeed);
    let closure = database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module.clone()]),
                roots: Arc::from([free_function_instance(&module, "main")]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .expect("closure parks for absent trusted toolchain modules");
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert!(output.parked_toolchain.is_some());
    assert!(
        output.fatal.is_none(),
        "an already observed collision must not leak past the higher-precedence park"
    );
}

#[test]
fn parking_unions_pending_demands_without_re_querying_them() {
    // RUE-1562: when a body demands an absent trusted toolchain module the
    // park has to union what every still-ready and still-pending body needs
    // too, which walks the whole frontier. That sweep re-asked the demand
    // family for bodies the prefetch batch had just read. It now reads the
    // evaluation's cache, so a body is queried at most once even though it
    // is visited by both the batch and the sweep.
    //
    // A wide fan-out where one leaf parks makes the overlap maximal: the
    // frontier is still full when the sweep runs.
    const CALLEES: usize = 24;
    let mut text = (0..CALLEES)
        .map(|index| format!("fn f{index}() -> i32 {{ {index} }}\n"))
        .collect::<String>();
    text.push_str("fn parked() -> i32 { let _value = @parse_u32(\"1\"); 0 }\n");
    let calls = (0..CALLEES)
        .map(|index| format!("f{index}()"))
        .collect::<Vec<_>>()
        .join(" + ");
    text.push_str(&format!("fn main() -> i32 {{ {calls} + parked() }}\n"));
    let snapshot = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "main")]),
        configuration: semantic_configuration(),
    };

    for workers in [1, 4] {
        let mut database = RevisionedQueryDatabase::with_query_concurrency(workers);
        let revision = revision_for(&mut database, &snapshot);
        let request = database
            .body_closure(revision, key.clone(), CancellationToken::new())
            .expect("the acquisition round publishes a typed park");
        let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        assert!(
            output.parked_toolchain.is_some(),
            "with {workers} worker(s) the absent trusted module must park the closure"
        );
        let queries = request
            .work
            .iter()
            .find_map(|(label, amount)| {
                (label.as_ref() == "reachability.toolchain-demand.queries").then_some(*amount)
            })
            .unwrap_or(0);
        // Every body the round touched is bounded by the whole program, so
        // one query per distinct body is the ceiling the sweep must respect.
        assert!(
            queries <= (CALLEES + 2) as u64,
            "with {workers} worker(s) the parking sweep re-queried demands: {queries} \
                 queries for at most {} distinct bodies",
            CALLEES + 2
        );
    }
}

#[test]
fn parked_toolchain_rounds_retain_and_reuse_the_exact_reachability_cone() {
    const CALLEES: usize = 16;
    let source = |parked: bool| {
        let mut text = (0..CALLEES)
            .map(|index| {
                let next = if index + 1 == CALLEES {
                    "parked()".to_owned()
                } else {
                    format!("f{}()", index + 1)
                };
                format!("fn f{index}() -> i32 {{ {next} }}\n")
            })
            .collect::<String>();
        if parked {
            text.push_str("fn parked() -> i32 { let _value = @parse_u32(\"1\"); 0 }\n");
        } else {
            text.push_str("fn parked() -> i32 { 0 }\n");
        }
        text.push_str("fn root() -> i32 { f0() }\n");
        source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1)
    };
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let body_keys = (0..CALLEES)
        .map(|index| free_function_instance(&module, &format!("f{index}")))
        .collect::<Vec<_>>();
    let mut reached_instances = body_keys.clone();
    reached_instances.push(free_function_instance(&module, "root"));
    let closure_key = crate::body_query::BodyClosureQueryKey {
        modules: Arc::from([module.clone()]),
        roots: Arc::from([free_function_instance(&module, "root")]),
        configuration: semantic_configuration(),
    };
    let body_keys = reached_instances
        .into_iter()
        .map(|instance| crate::body_query::BodyQueryKey::new(instance, semantic_configuration()))
        .collect::<Vec<_>>();
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
    let parked_revision = revision_for(&mut database, &source(true));

    let cold = database
        .body_closure(
            parked_revision,
            closure_key.clone(),
            CancellationToken::new(),
        )
        .expect("the first acquisition round publishes a typed park");
    let rue_query::QueryOutcome::Success(cold_output) = cold.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert!(cold_output.parked_toolchain.is_some());
    assert_eq!(cold_output.bodies.len(), CALLEES + 1);
    assert!(
        database.body_reachability_root_len() > BODY_QUERY_MEMO_RETENTION,
        "the parked exact cone must replace the bounded body history"
    );

    for round in 0..2 {
        let warm = database
            .body_closure(
                parked_revision,
                closure_key.clone(),
                CancellationToken::new(),
            )
            .expect("a later acquisition round reuses the parked cone");
        for key in &body_keys {
            assert_eq!(
                warm.execution_for(key),
                rue_query::RequestExecution::Reused,
                "parked acquisition round {round} must reuse {}",
                key.stable_identity()
            );
            assert!(warm.was_retained(key));
        }
    }

    let success_revision = revision_for(&mut database, &source(false));
    let success = database
        .body_closure(success_revision, closure_key, CancellationToken::new())
        .expect("the completed acquisition publishes the final closure");
    assert_eq!(success.terminal.kind(), QueryTerminalKind::Success);
    assert_eq!(
        database.body_reachability_root_len(),
        0,
        "the final closure root takes over before the parked root releases"
    );
    assert!(database.body_closure_root_metrics().0 > 0);
}

#[test]
fn body_closure_digest_registrar_is_permutation_independent_across_collisions() {
    let entries = [
        (
            7,
            anonymous_identity_for_digest_test("A", rue_air::AnonymousNominalKind::Struct),
        ),
        (
            7,
            anonymous_identity_for_digest_test("B", rue_air::AnonymousNominalKind::Struct),
        ),
        (
            7,
            anonymous_identity_for_digest_test("C", rue_air::AnonymousNominalKind::Struct),
        ),
        (
            8,
            anonymous_identity_for_digest_test("D", rue_air::AnonymousNominalKind::Struct),
        ),
        (
            8,
            anonymous_identity_for_digest_test("E", rue_air::AnonymousNominalKind::Struct),
        ),
    ];
    let register = |entries: Vec<(u128, crate::AnonymousNominalKey)>| {
        let mut owners = BTreeMap::new();
        let mut collision = None;
        for (digest, identity) in entries {
            register_body_closure_anonymous_digest(&mut owners, &mut collision, digest, &identity);
        }
        collision
    };
    let forward = register(entries.to_vec());
    let reverse = register(entries.iter().cloned().rev().collect());

    // The property is in the name: whichever order the registrar sees the
    // entries in, it reports the same collision.
    assert_eq!(forward, reverse);

    // And it reports the lowest colliding digest, naming its two smallest
    // identities in ascending order. Deriving the expectation from the same
    // total order the registrar uses keeps this about the selection rule
    // rather than about which spelling a particular comparator ranks first.
    let mut digest_seven = entries
        .iter()
        .filter(|(digest, _)| *digest == 7)
        .map(|(_, identity)| identity.clone())
        .collect::<Vec<_>>();
    digest_seven.sort();
    assert_eq!(
        forward,
        Some((7, digest_seven[0].clone(), digest_seven[1].clone()))
    );
}

#[test]
fn compiler_anonymous_digest_canonicalizes_empty_specialization_producers() {
    let canonical =
        anonymous_identity_for_digest_test("Canonical", rue_air::AnonymousNominalKind::Struct);
    let mut wrapped = canonical.clone();
    let crate::StableProducerId::Function(producer) = &canonical.producer else {
        unreachable!()
    };
    wrapped.producer =
        crate::StableProducerId::Function(Node::new(crate::FunctionInstanceKey::Specialization {
            base: producer.clone(),
            arguments: crate::CanonicalArguments::default(),
        }));
    assert_ne!(canonical, wrapped);
    assert_eq!(
        compiler_anonymous_identity_digest(&canonical),
        compiler_anonymous_identity_digest(&wrapped)
    );
    for identities in [
        [canonical.clone(), wrapped.clone()],
        [wrapped, canonical.clone()],
    ] {
        let mut owners = BTreeMap::new();
        let mut collision = None;
        for identity in identities {
            register_body_closure_anonymous_digest(&mut owners, &mut collision, 0xcafe, &identity);
        }
        assert!(
            collision.is_none(),
            "canonical and empty-specialization producer forms are one logical exact owner"
        );
        assert_eq!(owners, BTreeMap::from([(0xcafe, canonical.clone())]));
    }
}

#[test]
#[should_panic(
    expected = "body-closure digest forcing must be configured before the first closure evaluation"
)]
fn body_closure_digest_forcing_rejects_mutation_after_publication() {
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", "fn main() -> i32 { 0 }\n")], 1);
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    database
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([module]),
                roots: Arc::from([]),
                configuration: semantic_configuration(),
            },
            CancellationToken::new(),
        )
        .unwrap();
    database.force_body_closure_anonymous_digest_for_test(
        anonymous_identity_for_digest_test("TooLate", rue_air::AnonymousNominalKind::Struct),
        1,
    );
}
