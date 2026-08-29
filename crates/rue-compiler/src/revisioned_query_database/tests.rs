use super::body::RevisionSymbolSpace;
use super::test_support::*;
use super::*;
use crate::{
    CompilerSession, DiscoverySourceAssembler, FileMetadataFingerprint, ImportDiscoveryContext,
    ImportObservation, PhysicalFileIdentity, SourceMetadata,
};
use rue_span::FileId;
use std::collections::BTreeSet;

#[test]
fn semantic_comptime_call_depth_guard_restores_after_every_exit() {
    SEMANTIC_COMPTIME_CALL_DEPTH.with(|depth| assert_eq!(depth.get(), 0));

    let mut guards = Vec::new();
    for _ in 0..rue_air::MAX_COMPTIME_CALL_DEPTH {
        guards.push(
            SemanticComptimeCallDepthGuard::enter("count")
                .expect("propagated depths one through 64 must be admitted"),
        );
    }
    SEMANTIC_COMPTIME_CALL_DEPTH
        .with(|depth| assert_eq!(depth.get(), rue_air::MAX_COMPTIME_CALL_DEPTH));
    assert!(
        SemanticComptimeCallDepthGuard::enter("count").is_err(),
        "the exact next depth must be rejected"
    );
    while guards.pop().is_some() {}
    SEMANTIC_COMPTIME_CALL_DEPTH.with(|depth| assert_eq!(depth.get(), 0));

    let aborted: Result<(), QueryAbort> = (|| {
        let _guard = SemanticComptimeCallDepthGuard::enter("count")
            .expect("the root depth must be admitted");
        Err(QueryAbort::Canceled)
    })();
    assert!(matches!(aborted, Err(QueryAbort::Canceled)));
    SEMANTIC_COMPTIME_CALL_DEPTH.with(|depth| assert_eq!(depth.get(), 0));
}

struct AssertSemanticComptimeDepthZero;

impl Drop for AssertSemanticComptimeDepthZero {
    fn drop(&mut self) {
        SEMANTIC_COMPTIME_CALL_DEPTH.with(|depth| assert_eq!(depth.get(), 0));
    }
}

#[test]
#[should_panic(expected = "synthetic comptime query unwind")]
fn semantic_comptime_call_depth_guard_restores_while_unwinding() {
    let _assert_after_guard = AssertSemanticComptimeDepthZero;
    let _guard =
        SemanticComptimeCallDepthGuard::enter("count").expect("the root depth must be admitted");
    panic!("synthetic comptime query unwind");
}

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

fn lookup_history_key(name: impl Into<Arc<str>>) -> LookupObservationKey {
    LookupObservationKey::Name(LookupNameKey {
        module: ModuleId::from_logical_path("history.rue").unwrap(),
        namespace: DefinitionNamespace::ModuleItem,
        name: name.into(),
    })
}

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
fn backend_root_publication_gate_serializes_distinct_epochs() {
    let gate = Arc::new(BackendRootPublicationGate::default());
    let first_epoch = gate.enter();
    let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let second_gate = gate.clone();
    let second_attempted = attempted.clone();
    let second_entered = entered.clone();
    let second_epoch = std::thread::spawn(move || {
        second_attempted.store(true, std::sync::atomic::Ordering::Release);
        let _publication = second_gate.enter();
        second_entered.store(true, std::sync::atomic::Ordering::Release);
    });
    while !attempted.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::yield_now();
    }
    assert!(
        !entered.load(std::sync::atomic::Ordering::Acquire),
        "a distinct backend-root epoch cannot enter while its predecessor may roll back"
    );
    drop(first_epoch);
    second_epoch.join().unwrap();
    assert!(entered.load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn backend_root_publication_handoff_restores_last_good_root_on_rollback() {
    let root = Arc::new(Mutex::new(PublishedBackendRoot {
        publications: 7,
        additions: 11,
        deletions: 3,
        ..PublishedBackendRoot::default()
    }));
    let mut handoff = PublishedBackendRootHandoff {
        root: root.clone(),
        pending: Some(Arc::new(rue_query::RetainedPinSet::new())),
        functions: Some(BTreeSet::new()),
        cfg_terminals: 2,
        optimized_cfg_terminals: 1,
        codegen_unit_terminals: 1,
        object_projection_terminals: 1,
        previous: None,
        installed: false,
    };
    rue_query::QueryAttemptHandoff::commit(&mut handoff);
    assert_eq!(
        root.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .publications,
        8
    );
    rue_query::QueryAttemptHandoff::abort(&mut handoff);
    let restored = root
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(restored.publications, 7);
    assert_eq!(restored.additions, 11);
    assert_eq!(restored.deletions, 3);
}

#[test]
fn foreign_signature_agreement_uses_resolved_identity_mode_and_comptime_not_names() {
    use crate::durable_semantics::{
        DurableParameterMode as Mode, DurableSemanticParameter as Parameter, DurableType as Type,
    };

    let parameter = |name: &str, ty: Type, mode: Mode, is_comptime: bool| Parameter {
        name: Arc::from(name),
        ty,
        mode,
        is_comptime,
    };
    let left = [parameter("left", Type::I64, Mode::Value, false)];
    let renamed = [parameter("right", Type::I64, Mode::Value, false)];
    assert!(foreign_signatures_agree(
        &left,
        &Type::I64,
        &renamed,
        &Type::I64
    ));

    let borrowed = [parameter("left", Type::I64, Mode::Borrow, false)];
    assert_eq!(
        foreign_signature_display(&borrowed, &Type::I64),
        "fn(borrow i64) -> i64"
    );
    assert!(!foreign_signatures_agree(
        &left,
        &Type::I64,
        &borrowed,
        &Type::I64
    ));
    let comptime = [parameter("left", Type::I64, Mode::Value, true)];
    assert_eq!(
        foreign_signature_display(&comptime, &Type::I64),
        "fn(comptime i64) -> i64"
    );
    assert!(!foreign_signatures_agree(
        &left,
        &Type::I64,
        &comptime,
        &Type::I64
    ));

    let nominal = |module: &str| {
        Type::Nominal(crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path(module).unwrap(),
            crate::StableDefinitionNamespace::Type,
            crate::StableDefinitionKind::Struct,
            Arc::from("Point"),
            None,
        ))
    };
    let first_nominal = [parameter("point", nominal("left.rue"), Mode::Value, false)];
    let second_nominal = [parameter("point", nominal("right.rue"), Mode::Value, false)];
    assert!(!foreign_signatures_agree(
        &first_nominal,
        &Type::I32,
        &second_nominal,
        &Type::I32
    ));
}

#[test]
fn lookup_incarnation_history_refreshes_recency_without_duplicate_order_entries() {
    let mut lease = PublishedRootLookupLease::default();
    for index in 0..LOOKUP_INCARNATION_HISTORY_BOUND {
        lease.record_incarnation(lookup_history_key(format!("key-{index}")), index as u64);
    }
    lease.record_incarnation(lookup_history_key("key-0"), 10_000);
    lease.record_incarnation(lookup_history_key("newest"), 20_000);

    assert_eq!(lease.incarnations.len(), LOOKUP_INCARNATION_HISTORY_BOUND);
    assert_eq!(
        lease.incarnation_order.len(),
        LOOKUP_INCARNATION_HISTORY_BOUND
    );
    assert_eq!(
        lease.seen_incarnation(&lookup_history_key("key-0")),
        Some(10_000)
    );
    assert_eq!(lease.seen_incarnation(&lookup_history_key("key-1")), None);
    assert_eq!(
        lease.seen_incarnation(&lookup_history_key("newest")),
        Some(20_000)
    );
}

#[test]
fn lookup_incarnation_history_keeps_name_and_import_families_distinct() {
    let module = ModuleId::from_logical_path("history.rue").unwrap();
    let name = LookupObservationKey::Name(LookupNameKey {
        module: module.clone(),
        namespace: DefinitionNamespace::ModuleItem,
        name: "shared".into(),
    });
    let import = LookupObservationKey::Import(LookupImportKey {
        module,
        specifier: "shared".into(),
    });
    let mut lease = PublishedRootLookupLease::default();
    lease.record_incarnation(name.clone(), 41);
    lease.record_incarnation(import.clone(), 42);

    assert_eq!(lease.seen_incarnation(&name), Some(41));
    assert_eq!(lease.seen_incarnation(&import), Some(42));
    assert_eq!(lease.incarnations.len(), 2);
}

#[test]
fn lookup_incarnation_history_mutations_roll_back_exactly() {
    let mut lease = PublishedRootLookupLease::default();
    for index in 0..LOOKUP_INCARNATION_HISTORY_BOUND {
        lease.record_incarnation(lookup_history_key(format!("key-{index}")), index as u64);
    }
    let previous_incarnations = lease.incarnations.clone();
    let previous_incarnation_order = lease.incarnation_order.clone();
    let previous_generation = lease.next_incarnation_generation;

    let mutations = vec![
        lease.record_incarnation(lookup_history_key("key-0"), 10_000),
        lease.record_incarnation(lookup_history_key("newest"), 20_000),
        lease.record_incarnation(lookup_history_key("newest"), 30_000),
    ];
    assert_eq!(
        lease.seen_incarnation(&lookup_history_key("key-0")),
        Some(10_000)
    );
    assert_eq!(lease.seen_incarnation(&lookup_history_key("key-1")), None);
    assert_eq!(
        lease.seen_incarnation(&lookup_history_key("newest")),
        Some(30_000)
    );

    lease.rollback_incarnation_mutations(mutations);
    lease.next_incarnation_generation = previous_generation;
    assert_eq!(lease.incarnations, previous_incarnations);
    assert_eq!(lease.incarnation_order, previous_incarnation_order);
}

#[test]
fn lookup_root_handoff_journal_rolls_back_and_retries() {
    let mut initial = PublishedRootLookupLease {
        roots: BTreeMap::from([(
            "root".to_owned(),
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
    initial.record_incarnation(existing.clone(), 41);
    let lease = Arc::new(Mutex::new(initial));
    let observed_key = LookupObservationKey::Name(LookupNameKey {
        module: ModuleId::from_logical_path("main.rue").unwrap(),
        namespace: DefinitionNamespace::ModuleItem,
        name: "successor".into(),
    });
    let mut handoff = PublishedLookupRootHandoff {
        lease: lease.clone(),
        runtime: QueryRuntime::new(1),
        root: "root".to_owned(),
        observed: Some(ObservedLookupRoot {
            pins: rue_query::RetainedPinSet::new(),
            observed_keys: vec![(observed_key.clone(), 42)],
        }),
        rollback: None,
    };

    rue_query::QueryAttemptHandoff::commit(&mut handoff);
    {
        let lease = lease.lock().unwrap();
        assert_eq!(lease.seen_incarnation(&observed_key), Some(42));
        assert_eq!(lease.roots["root"].publication, 1);
    }
    rue_query::QueryAttemptHandoff::abort(&mut handoff);
    {
        let lease = lease.lock().unwrap();
        assert_eq!(lease.seen_incarnation(&existing), Some(41));
        assert_eq!(lease.seen_incarnation(&observed_key), None);
        assert_eq!(lease.incarnations.len(), 1);
        assert_eq!(lease.next_incarnation_generation, 1);
        assert_eq!(lease.next_root_publication, 1);
        assert_eq!(lease.rederivations_after_eviction, 5);
        assert_eq!(lease.supersession_evictions, 2);
        assert_eq!(lease.roots["root"].publication, 0);
    }

    rue_query::QueryAttemptHandoff::commit(&mut handoff);
    let lease = lease.lock().unwrap();
    assert_eq!(lease.seen_incarnation(&observed_key), Some(42));
    assert_eq!(lease.roots["root"].publication, 1);
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
    let compiler = include_str!("provider.rs");
    let runtime = super::REVISIONED_DATABASE_SOURCE;
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
    let lower = include_str!("../canonical_lower.rs");
    assert!(lower.contains("BodyRirBundle::new_with_index_attribution"));
    assert!(
        lower.contains("materialize_body_rir_bundle_with_declaration"),
        "the packed candidate materializer owns the request-local bundle"
    );
}

#[test]
fn stable_definition_kinds_have_fixed_syntax_candidate_sets() {
    use crate::StableDefinitionKind as K;
    use crate::declaration_candidate::DeclarationCandidateCategory as C;

    let module = ModuleId::from_logical_path("main.rue").unwrap();
    for (kind, owner, expected) in [
        (K::Function, None, &[C::Function, C::ExternFunction][..]),
        (K::Struct, None, &[C::Struct][..]),
        (K::Enum, None, &[C::Enum][..]),
        (K::ValueConst, None, &[C::ConstCandidate][..]),
        (K::ModuleBinding, None, &[C::ConstCandidate][..]),
        (
            K::Method,
            Some((K::Struct, Arc::from("Owner"))),
            &[C::Method][..],
        ),
        (
            K::AssociatedFunction,
            Some((K::Struct, Arc::from("Owner"))),
            &[C::AssociatedFunction][..],
        ),
        (
            K::Destructor,
            Some((K::Struct, Arc::from("Owner"))),
            &[C::Destructor][..],
        ),
    ] {
        let key = StableDefinitionKey::from_stable_parts(
            module.clone(),
            crate::StableDefinitionNamespace::Value,
            kind,
            "item",
            owner.clone(),
        );
        let candidates = stable_syntax_candidate_set(&key)
            .expect("every well-formed stable definition kind has a syntax candidate set")
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.category)
                .collect::<Vec<_>>(),
            expected,
            "{kind:?}"
        );
        for candidate in candidates {
            assert_eq!(candidate.module, module);
            assert_eq!(candidate.name.as_ref(), "item");
            assert!(Arc::ptr_eq(&candidate.name, key.shared_name()));
            assert_eq!(candidate.duplicate_discriminator, 0);
            assert_eq!(
                candidate.owner.as_ref().map(|owner| owner.name.as_ref()),
                owner.as_ref().map(|(_, name)| name.as_ref())
            );
            if let Some(candidate_owner) = &candidate.owner {
                assert!(Arc::ptr_eq(
                    &candidate_owner.name,
                    key.owner().unwrap().shared_name()
                ));
            }
        }
    }
}

#[test]
fn stable_declaration_classification_is_narrow_green_and_multiplicity_sensitive() {
    let first = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper(value: i32) -> i32 { value + 1 }\nfn main() -> i32 { helper(1) }",
        )],
        1,
    );
    let unrelated = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper(value: i32) -> i32 { value + 1 }\nfn extra() -> i32 { 9 }\nfn main() -> i32 { helper(1) }",
        )],
        1,
    );
    let duplicate = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper(value: i32) -> i32 { value + 1 }\nfn helper(value: i32) -> i32 { value + 2 }\nfn main() -> i32 { helper(1) }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = StableDeclarationClassificationQueryKey(StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "helper",
        None,
    ));
    let mut database = RevisionedQueryDatabase::default();
    let first_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&first),
        &first,
    );
    let first = database.runtime.request_registered(
        &database.stable_declaration_classifications,
        first_revision,
        key.clone(),
        CancellationToken::new(),
    );
    let first_terminal = first.terminal().unwrap();
    let first_stamp = first_terminal.stamp();
    assert!(matches!(
        first_terminal.outcome(),
        rue_query::QueryOutcome::Success(
            StableDeclarationClassificationQueryValue::Selected(candidate)
        ) if candidate.category
            == crate::declaration_candidate::DeclarationCandidateCategory::Function
    ));
    assert_eq!(
        first
            .dependencies()
            .iter()
            .map(|dependency| dependency.node.family())
            .collect::<Vec<_>>(),
        vec![
            "compiler.declaration-occurrence-index",
            "compiler.declaration-shell"
        ]
    );

    let unrelated_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&unrelated),
        &unrelated,
    );
    let unrelated = database.runtime.request_registered(
        &database.stable_declaration_classifications,
        unrelated_revision,
        key.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        unrelated.terminal().unwrap().stamp(),
        first_stamp,
        "an unrelated declaration may rebuild the module occurrence index but must keep the \
             narrow classification green"
    );

    let duplicate_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&duplicate),
        &duplicate,
    );
    let duplicate = database.runtime.request_registered(
        &database.stable_declaration_classifications,
        duplicate_revision,
        key,
        CancellationToken::new(),
    );
    let duplicate_terminal = duplicate.terminal().unwrap();
    assert_ne!(duplicate_terminal.stamp(), first_stamp);
    assert!(matches!(
        duplicate_terminal.outcome(),
        rue_query::QueryOutcome::Success(StableDeclarationClassificationQueryValue::Invalid(
            StableDeclarationClassificationFailure::DuplicateMultiplicity {
                multiplicity: 2,
                ..
            }
        ))
    ));
}

fn source_snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
    let physical = entries
        .iter()
        .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
        .collect::<AHashMap<_, _>>();
    let logical = entries
        .iter()
        .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
        .collect::<AHashMap<_, _>>();
    let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
    SourceSnapshot::new(
        metadata,
        entries
            .iter()
            .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
            .collect(),
    )
    .unwrap()
}

fn semantic_configuration() -> crate::semantic_query_nucleus::SemanticQueryConfiguration {
    crate::semantic_query_nucleus::SemanticQueryConfiguration {
        target: rue_target::Target::X86_64Linux,
        preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
    }
}

fn free_function_instance(module: &ModuleId, name: &str) -> crate::FunctionInstanceKey {
    crate::FunctionInstanceKey::Definition(crate::StableDefinitionKey::from_stable_parts(
        module.clone(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        Arc::from(name),
        None,
    ))
}

#[test]
fn anonymous_nominal_traversal_visits_each_shared_identity_exactly_once() {
    // RUE-1555: canonical argument slices are shared through `Arc`, so one
    // instance key reaches the same nested identity through many paths and
    // the visited set is consulted far more often than it grows. It used
    // to be a `Vec` scanned linearly, which made the traversal quadratic
    // in the identities a key reaches; membership is now constant-time.
    //
    // What must not change is the traversal: every reachable identity
    // produced exactly once, by pointer identity rather than structural
    // equality, with the scratch buffer reused across calls.
    const LEAVES: u32 = 256;
    const SHARERS: u32 = 64;

    let module = ModuleId::from_logical_path("anon.rue").unwrap();
    let definition = |name: &str| {
        crate::StableDefinitionKey::from_stable_parts(
            module.clone(),
            crate::StableDefinitionNamespace::Type,
            crate::StableDefinitionKind::Struct,
            Arc::from(name),
            None,
        )
    };
    let leaf = |ordinal: u32| crate::AnonymousNominalKey {
        kind: crate::semantic_identity::AnonymousNominalKind::Struct,
        producer: crate::StableProducerId::Definition(definition("make")),
        anchor: crate::semantic_identity::StructuralAnchor::new(vec![
            crate::semantic_identity::StructuralPathSegment::AnonymousType(ordinal),
        ]),
    };

    // One slice, cloned into every specialization below, so each of the
    // SHARERS levels re-walks the very same LEAVES addresses. That is the
    // adversarial shape: LEAVES * SHARERS visit attempts against a visited
    // set that only ever holds LEAVES entries.
    let shared: Arc<[crate::TypeInstanceKey]> = (0..LEAVES)
        .map(|ordinal| {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(Node::new(leaf(
                ordinal,
            ))))
        })
        .collect::<Vec<_>>()
        .into();
    let wide = |base: crate::FunctionInstanceKey| crate::FunctionInstanceKey::Specialization {
        base: Node::new(base),
        arguments: crate::CanonicalArguments {
            types: shared.clone(),
            values: Arc::new([]),
        },
    };

    let mut key = free_function_instance(&module, "root");
    for _ in 0..SHARERS {
        key = wide(key);
    }
    // Deep nesting on top of the fan-out: an identity whose producer is
    // itself a specialization carrying the same slice, so the whole leaf
    // set is reachable a second time through a different kind of edge.
    let nested = crate::AnonymousNominalKey {
        kind: crate::semantic_identity::AnonymousNominalKind::Struct,
        producer: crate::StableProducerId::Function(Node::new(wide(free_function_instance(
            &module, "nested",
        )))),
        anchor: crate::semantic_identity::StructuralAnchor::new(vec![
            crate::semantic_identity::StructuralPathSegment::AnonymousType(LEAVES),
        ]),
    };
    let key = crate::FunctionInstanceKey::Specialization {
        base: Node::new(key),
        arguments: crate::CanonicalArguments {
            types: Arc::from([crate::TypeInstanceKey::Nominal(
                crate::NominalInstanceKey::Anonymous(Node::new(nested.clone())),
            )]),
            values: Arc::new([]),
        },
    };

    let mut scratch = AHashSet::new();
    let mut visited = Vec::new();
    visit_instance_anonymous_nominals(&key, &mut scratch, |identity| {
        visited.push(identity.clone());
    });

    assert_eq!(
        visited.len(),
        LEAVES as usize + 1,
        "every leaf plus the nested identity is produced exactly once, \
             however many paths reach it"
    );
    let distinct: BTreeSet<crate::AnonymousNominalKey> = visited.iter().cloned().collect();
    assert_eq!(
        distinct.len(),
        visited.len(),
        "a repeat visit must be suppressed, not merely deduplicated later"
    );
    assert!(
        distinct.contains(&nested),
        "the identity reached through a Function producer is still visited"
    );
    assert_eq!(
        collect_instance_anonymous_nominals(&key),
        distinct,
        "the collecting wrapper agrees with the raw traversal"
    );

    // Scratch reuse: the buffer is cleared at entry, so a second traversal
    // through the same one produces the same result rather than a
    // truncated one.
    let mut reused = Vec::new();
    visit_instance_anonymous_nominals(&key, &mut scratch, |identity| {
        reused.push(identity.clone());
    });
    assert_eq!(
        reused, visited,
        "the scratch set must be cleared and reused between traversals"
    );
}

fn trusted_option_body_snapshot(root_source: &str, option_source: &str) -> SourceSnapshot {
    let option = FileId::new(2);
    trusted_body_snapshot(root_source, Some((option, option_source)), None)
}

fn trusted_body_snapshot(
    root_source: &str,
    option_source: Option<(FileId, &str)>,
    strbuf_source: Option<(FileId, &str)>,
) -> SourceSnapshot {
    let root = FileId::new(1);
    let mut physical = AHashMap::from([(root, "/project/main.rue".to_owned())]);
    let mut logical = AHashMap::from([(root, "main.rue".to_owned())]);
    let mut trusted = AHashSet::new();
    let mut sources = vec![(root, Arc::new(root_source.to_owned()))];
    if let Some((option, source)) = option_source {
        physical.insert(option, "/sdk/option.rue".to_owned());
        logical.insert(option, crate::OPTION_MODULE_LOGICAL_PATH.to_owned());
        trusted.insert(option);
        sources.push((option, Arc::new(source.to_owned())));
    }
    if let Some((strbuf, source)) = strbuf_source {
        physical.insert(strbuf, "/sdk/strbuf.rue".to_owned());
        logical.insert(strbuf, crate::STRBUF_MODULE_LOGICAL_PATH.to_owned());
        trusted.insert(strbuf);
        sources.push((strbuf, Arc::new(source.to_owned())));
    }
    let metadata =
        SourceMetadata::new_with_trusted_standard_library(root, physical, logical, trusted)
            .expect("trusted Option metadata is valid");
    SourceSnapshot::new(metadata, sources).expect("trusted body snapshot is valid")
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
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
    let first_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&first),
        &first,
    );
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

    let second_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&second),
        &second,
    );
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
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

fn declaration_candidate(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    module: &ModuleId,
    category: crate::declaration_candidate::DeclarationCandidateCategory,
    name: &str,
) -> crate::declaration_candidate::DeclarationCandidateKey {
    let attempt = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        revision,
        ModuleQueryKey(module.clone()),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(value) = attempt.terminal().unwrap().outcome() else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(index) = value else {
        panic!("declaration occurrence index unavailable")
    };
    index
        .capabilities
        .keys()
        .find(|candidate| candidate.category == category && candidate.name.as_ref() == name)
        .cloned()
        .unwrap_or_else(|| panic!("missing {category:?} candidate `{name}`"))
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
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

fn request_semantic_nucleus(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    key: crate::semantic_query_nucleus::SemanticNucleusKey,
) -> crate::semantic_query_nucleus::SemanticNucleusValue {
    request_semantic_nucleus_observed(database, revision, key).0
}

fn request_semantic_nucleus_observed(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    key: crate::semantic_query_nucleus::SemanticNucleusKey,
) -> (
    crate::semantic_query_nucleus::SemanticNucleusValue,
    QueryRequestAttempt<crate::semantic_query_nucleus::SemanticNucleusValue>,
) {
    let attempt = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        key,
        CancellationToken::new(),
    );
    let terminal = attempt
        .terminal()
        .unwrap_or_else(|| panic!("semantic nucleus aborted: {:?}", attempt.abort()));
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!()
    };
    (value.clone(), attempt)
}

fn assert_direct_semantic_observation(
    label: &str,
    attempt: &QueryRequestAttempt<crate::semantic_query_nucleus::SemanticNucleusValue>,
    required_families: &[&str],
    allowed_families: &[&str],
    maximum_dependencies: usize,
) {
    let actual = attempt
        .dependencies()
        .iter()
        .map(|dependency| dependency.node.family())
        .collect::<BTreeSet<_>>();
    let required = required_families.iter().copied().collect::<BTreeSet<_>>();
    let allowed = allowed_families.iter().copied().collect::<BTreeSet<_>>();
    assert!(
        required.is_subset(&actual),
        "{label} omitted a required direct dependency family: required={required:?}, actual={actual:?}"
    );
    assert!(
        actual.is_subset(&allowed),
        "{label} observed an unexpected dependency family: actual={actual:?}, allowed={allowed:?}; batch, root, full-plan, and unrelated discovery dependencies are forbidden"
    );
    assert!(
        attempt.dependencies().len() <= maximum_dependencies,
        "{label} observed broad same-family discovery: dependencies={:?}",
        attempt.dependencies()
    );
    assert!(
        attempt.inputs().is_empty(),
        "{label} read inputs directly instead of through its precise query dependencies: {:?}",
        attempt.inputs()
    );
}

fn nucleus_failure_message(
    value: &crate::semantic_query_nucleus::SemanticNucleusValue,
) -> Option<String> {
    use crate::semantic_query_nucleus::{SemanticNucleusFailure as F, SemanticNucleusValue as V};
    match value {
        V::Failure(
            F::Diagnostic(kind)
            | F::DiagnosticAtParameter { kind, .. }
            | F::DiagnosticAtDeclaration { kind, .. }
            | F::OwnershipGate { kind, .. }
            | F::DiagnosticWithHelp { kind, .. }
            | F::DiagnosticWithNote { kind, .. },
        ) => Some(kind.to_string()),
        _ => None,
    }
}

#[test]
fn direct_identity_and_signature_families_are_complete_per_declaration() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::{DurableParameterMode, DurableType};
    use crate::semantic_query_nucleus::{
        DeclarationSignatureProjection as Sig, SemanticNucleusKey as Key, SemanticNucleusValue as V,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct S { value: i32, fn get(borrow self, delta: i32) -> i32 { self.value + delta } fn make(value: i32) -> S { S { value } } } enum E { A, B } drop fn S(self) {} fn free(value: i32) -> i32 { value } fn main() {}",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );

    for (category, kind, name, owner) in [
        (
            Category::Function,
            crate::StableDefinitionKind::Function,
            "free",
            None,
        ),
        (
            Category::Struct,
            crate::StableDefinitionKind::Struct,
            "S",
            None,
        ),
        (Category::Enum, crate::StableDefinitionKind::Enum, "E", None),
        (
            Category::Method,
            crate::StableDefinitionKind::Method,
            "get",
            Some("S"),
        ),
        (
            Category::AssociatedFunction,
            crate::StableDefinitionKind::AssociatedFunction,
            "make",
            Some("S"),
        ),
        (
            Category::Destructor,
            crate::StableDefinitionKind::Destructor,
            "S",
            Some("S"),
        ),
    ] {
        let declaration = declaration_candidate(&database, revision, &module, category, name);
        let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        };
        let (identity, identity_attempt) =
            request_semantic_nucleus_observed(&database, revision, Key::Identity(query.clone()));
        if category == Category::Destructor {
            assert_direct_semantic_observation(
                "destructor identity",
                &identity_attempt,
                &["compiler.declaration-shell", "compiler.semantic-nucleus"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                7,
            );
        } else {
            assert_direct_semantic_observation(
                "direct identity",
                &identity_attempt,
                &["compiler.declaration-shell"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.parse-module",
                ],
                3,
            );
        }
        let V::Identity(identity) = identity else {
            panic!("direct identity failed for {kind:?} {name}: {identity:?}")
        };
        assert_eq!(identity.key.namespace(), kind.namespace());
        assert_eq!(identity.key.kind(), kind);
        assert_eq!(identity.key.name(), name);
        assert_eq!(identity.key.module(), &module);
        assert_eq!(identity.key.owner().map(|owner| owner.name()), owner);
        assert!(
            !identity.is_public,
            "no declaration in this fixture is `pub`"
        );

        let (signature, signature_attempt) =
            request_semantic_nucleus_observed(&database, revision, Key::Signature(query));
        match category {
            Category::Destructor => assert_direct_semantic_observation(
                "destructor signature",
                &signature_attempt,
                &[
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.parse-module",
                ],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                10,
            ),
            Category::Method | Category::AssociatedFunction => assert_direct_semantic_observation(
                "owned callable signature",
                &signature_attempt,
                &[
                    "compiler.declaration-shell",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                9,
            ),
            _ => assert_direct_semantic_observation(
                "direct signature",
                &signature_attempt,
                &["compiler.declaration-shell", "compiler.parse-module"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.parse-module",
                ],
                4,
            ),
        }
        let V::Signature(signature) = signature else {
            panic!("direct signature failed for {kind:?} {name}: {signature:?}")
        };
        let signature = &signature.signature;
        match (kind, name) {
            (crate::StableDefinitionKind::Function, "free") => {
                let Sig::Callable {
                    parameters,
                    result,
                    has_self,
                    self_mode,
                    is_accessor,
                    is_unchecked,
                    is_extern,
                    is_c_export,
                    ..
                } = signature
                else {
                    panic!("free must project a callable signature: {signature:?}")
                };
                let [value] = parameters.as_ref() else {
                    panic!("free has one parameter: {parameters:?}")
                };
                assert_eq!(value.name.as_ref(), "value");
                assert_eq!(value.ty, DurableType::I32);
                assert_eq!(value.mode, DurableParameterMode::Value);
                assert!(!value.is_comptime);
                assert_eq!(result, &DurableType::I32);
                assert!(!has_self);
                assert_eq!(self_mode, &DurableParameterMode::Value);
                assert!(!is_accessor && !is_unchecked && !is_extern && !is_c_export);
            }
            (crate::StableDefinitionKind::Struct, "S") => {
                let Sig::Struct {
                    fields,
                    is_copy,
                    is_linear,
                    is_repr_c,
                } = signature
                else {
                    panic!("S must project a struct signature: {signature:?}")
                };
                let [(field, ty)] = fields.as_ref() else {
                    panic!("S has one field: {fields:?}")
                };
                assert_eq!(field.as_ref(), "value");
                assert_eq!(ty, &DurableType::I32);
                assert!(!is_copy, "a destructor-bearing struct is not copyable");
                assert!(!is_linear && !is_repr_c);
            }
            (crate::StableDefinitionKind::Enum, "E") => {
                let Sig::Enum { variants, .. } = signature else {
                    panic!("E must project an enum signature: {signature:?}")
                };
                let rendered = variants
                    .iter()
                    .map(|(name, payload)| (name.as_ref(), payload.len()))
                    .collect::<Vec<_>>();
                assert_eq!(rendered, [("A", 0), ("B", 0)]);
            }
            (crate::StableDefinitionKind::Method, "get") => {
                let Sig::Callable {
                    parameters,
                    result,
                    has_self,
                    self_mode,
                    ..
                } = signature
                else {
                    panic!("get must project a callable signature: {signature:?}")
                };
                let [delta] = parameters.as_ref() else {
                    panic!("get has one explicit parameter: {parameters:?}")
                };
                assert_eq!(delta.name.as_ref(), "delta");
                assert_eq!(delta.ty, DurableType::I32);
                assert_eq!(result, &DurableType::I32);
                assert!(has_self);
                assert_eq!(self_mode, &DurableParameterMode::Borrow);
            }
            (crate::StableDefinitionKind::AssociatedFunction, "make") => {
                let Sig::Callable {
                    parameters,
                    result,
                    has_self,
                    ..
                } = signature
                else {
                    panic!("make must project a callable signature: {signature:?}")
                };
                assert_eq!(parameters.len(), 1);
                assert!(!has_self);
                let DurableType::Nominal(owner_key) = result else {
                    panic!("make returns the owning nominal: {result:?}")
                };
                assert_eq!(owner_key.name(), "S");
                assert_eq!(owner_key.kind(), crate::StableDefinitionKind::Struct);
            }
            (crate::StableDefinitionKind::Destructor, "S") => {
                assert_eq!(signature, &Sig::Destructor);
            }
            other => panic!("unexpected fixture declaration {other:?}"),
        }
    }
}

#[test]
fn direct_const_family_evaluates_the_annotated_initializer() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusKey as Key,
        SemanticNucleusValue as V,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const SELECTED: i32 = 40 + 2; fn main() -> i32 { SELECTED }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let declaration = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "SELECTED",
    );
    let configuration = semantic_configuration();
    let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
        &database,
        revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration,
        }),
    );
    assert_direct_semantic_observation(
        "const evaluation",
        &keyed_attempt,
        &[
            "compiler.declaration-body-plan-artifacts",
            "compiler.declaration-shell",
            "compiler.lookup-name",
        ],
        &[
            "compiler.declaration-body-plan-artifacts",
            "compiler.declaration-occurrence-index",
            "compiler.declaration-shell",
            "compiler.lookup-name",
            "compiler.module-index",
            "compiler.parse-module",
        ],
        6,
    );
    let V::ConstResolution(Resolution::Value {
        ty: keyed_ty,
        value,
        ..
    }) = keyed
    else {
        panic!("direct const terminal failed: {keyed:?}")
    };
    let crate::durable_semantics::DurableConstValue::Integer(keyed_value) = *value else {
        panic!("direct const terminal returned a non-integer value")
    };
    assert_eq!(keyed_ty, crate::durable_semantics::DurableType::I32);
    assert_eq!(keyed_value, 42, "`40 + 2` evaluates at declaration time");
}

#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_direct_target_selected_comptime_evaluates_under_the_host_arch() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, ComptimeCallResultProjection as ResultProjection,
        SemanticNucleusKey as Key, SemanticNucleusValue as V,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn selected(comptime seed: i32) -> i32 { match @target_arch() { Arch.X86_64 => seed + 64, Arch.Aarch64 => seed + 32 } } fn main() -> i32 { selected(0) }",
        )],
        1,
    );
    let target = rue_target::Target::host().expect("test host is a supported target");
    // `@target_arch()` selects the match arm from the configured target, so
    // the expected value follows directly from the host architecture.
    let expected = match target.arch() {
        rue_target::Arch::X86_64 => 64,
        rue_target::Arch::Aarch64 => 32,
    };
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let mut configuration = semantic_configuration();
    configuration.target = target;
    let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    &database,
                    revision,
                    &module,
                    Category::Function,
                    "selected",
                ),
                configuration,
            },
            type_arguments: Arc::from([]),
            value_arguments: Arc::from([(
                Arc::from("seed"),
                crate::durable_semantics::DurableConstValue::Integer(0),
            )]),
        }),
    );
    assert_direct_semantic_observation(
        "target-selected comptime call",
        &keyed_attempt,
        &[
            "compiler.declaration-body-plan-artifacts",
            "compiler.declaration-shell",
            "compiler.semantic-nucleus",
        ],
        &[
            "compiler.declaration-body-plan-artifacts",
            "compiler.declaration-occurrence-index",
            "compiler.declaration-shell",
            "compiler.parse-module",
            "compiler.semantic-nucleus",
        ],
        7,
    );
    let V::ComptimeCall(crate::semantic_query_nucleus::ComptimeCallProjection {
        result: ResultProjection::Value(crate::durable_semantics::DurableConstValue::Integer(keyed)),
        ..
    }) = keyed
    else {
        panic!("direct target-selected const failed: {keyed:?}")
    };
    assert_eq!(i128::from(expected), keyed);
}

/// RUE-1112 demand-resolves proof. Once the trusted `\0rue-std/option.rue`
/// module is present in the snapshot's module set — exactly as the host
/// publishes it on the successor after satisfying a
/// `TrustedToolchainModuleDemand` — a directly-rooted `ComptimeCall` for
/// `\0rue-std/option.rue::Option(i64)` resolves the real materialized
/// nominal with std provenance. This is the proven key shape:
///
/// `DeclarationCandidateKey { module: from_trusted_standard_library_path(..),
///  Function, "Option", None }` -> `DeclarationSemanticQueryKey` ->
/// `ComptimeCallQueryKey { type_arguments: [("T", DurableType::I64)] }`.
///
/// The consumption track wires this into AIR; here we only prove
/// resolvability against a present trusted module.
#[test]
fn trusted_std_option_comptime_call_resolves_for_i64() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::{DurableAnonymousNominalShape as Shape, DurableType};
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, ComptimeCallResultProjection as ResultProjection,
        SemanticNucleusKey as Key, SemanticNucleusValue as V,
    };

    // The freestanding fallible-intrinsic program plus the trusted Option
    // module the host published on the successor. `main.rue` names a bare
    // `@parse_i64`, which is the reason the demand was emitted upstream.
    let root = FileId::new(1);
    let option = FileId::new(2);
    let physical = AHashMap::from([
        (root, "/project/main.rue".to_owned()),
        (option, "/sdk/option.rue".to_owned()),
    ]);
    let logical = AHashMap::from([
        (root, "main.rue".to_owned()),
        (option, crate::OPTION_MODULE_LOGICAL_PATH.to_owned()),
    ]);
    let metadata = SourceMetadata::new_with_trusted_standard_library(
        root,
        physical,
        logical,
        AHashSet::from([option]),
    )
    .unwrap();
    let source = SourceSnapshot::new(
        metadata,
        vec![
            (
                root,
                Arc::new("fn main() -> i32 { let x: i32 = 0; x }".to_owned()),
            ),
            (
                option,
                Arc::new(
                    "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }".to_owned(),
                ),
            ),
        ],
    )
    .unwrap();

    let module =
        ModuleId::from_trusted_standard_library_path(crate::OPTION_MODULE_LOGICAL_PATH).unwrap();
    assert!(
        module.is_trusted_standard_library(),
        "the demand resolves against a trusted std module"
    );

    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    &database,
                    revision,
                    &module,
                    Category::Function,
                    "Option",
                ),
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([(Arc::from("T"), DurableType::I64)]),
            value_arguments: Arc::from([]),
        }),
    );

    let V::ComptimeCall(projection) = value else {
        panic!("trusted Option(i64) comptime call did not resolve: {value:?}");
    };
    assert!(
        matches!(projection.result, ResultProjection::Type(_)),
        "Option(i64) must resolve to a materialized type, got {:?}",
        projection.result
    );
    // The real materialized nominal: an Option enum whose `Some` carries the
    // requested `i64` payload and whose `None` is empty.
    let materialized_option = projection.anonymous_nominals.iter().any(|nominal| {
        matches!(
            &nominal.shape,
            Shape::Enum { variants }
                if variants.len() == 2
                    && variants.iter().any(|(name, payload)| {
                        name.as_ref() == "Some"
                            && payload.len() == 1
                            && payload[0] == DurableType::I64
                    })
                    && variants.iter().any(|(name, payload)| {
                        name.as_ref() == "None" && payload.is_empty()
                    })
        )
    });
    assert!(
        materialized_option,
        "Option(i64) must materialize a real Some(i64)/None nominal: {:?}",
        projection.anonymous_nominals
    );
}

#[test]
fn cold_foreign_comptime_probe_admits_owned_program_without_value_evaluation() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn target() -> i32 { @import(\"dep\"); 1 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let producer = StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "target",
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let astgen_before = database
        .declaration_body_plan_astgen_evaluations
        .load(std::sync::atomic::Ordering::Relaxed);
    let probe = database
        .probe_ready_body_facts(
            revision,
            semantic_configuration(),
            "cold-foreign-comptime-probe",
            |provider| provider.probe_comptime_call(&producer, &[], &[]),
        )
        .result
        .unwrap();
    let crate::body_query::ForeignComptimeCallLookup::Admitted(program) = probe else {
        panic!("cold foreign comptime lookup should admit its owned body plan");
    };
    assert_eq!(program.plan.key.declaration, producer);
    assert_eq!(
        program.plan.key.configuration,
        semantic_configuration(),
        "owned admission must retain the exact requested configuration"
    );
    assert_eq!(
        program.callable().expect("callable root").context.as_str(),
        "main.rue"
    );
    assert_eq!(program.imports.imports.len(), 1);
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        astgen_before + 1
    );
    let candidate = declaration_candidate_for_stable_key(&producer).unwrap();
    let comptime_key = crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(
        crate::semantic_query_nucleus::ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: candidate,
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([]),
            value_arguments: Arc::from([]),
        },
    );
    assert!(
        !database
            .semantic_nucleus
            .contains_retained_key(&comptime_key),
        "a cold ready-only probe must not demand or evaluate its comptime value"
    );
}

#[test]
fn ready_foreign_comptime_probe_reuses_full_projection_without_body_materialization() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, ComptimeCallResultProjection, SemanticNucleusKey,
        SemanticNucleusValue,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn selected(comptime seed: i32) -> i32 { seed + 64 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let producer = StableDefinitionKey::from_stable_parts(
        module.clone(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "selected",
        None,
    );
    let declaration =
        declaration_candidate(&database, revision, &module, Category::Function, "selected");
    let key = SemanticNucleusKey::ComptimeCall(ComptimeCallQueryKey {
        declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        },
        type_arguments: Arc::from([]),
        value_arguments: Arc::from([(
            Arc::from("seed"),
            crate::durable_semantics::DurableConstValue::Integer(0),
        )]),
    });
    let value = request_semantic_nucleus(&database, revision, key.clone());
    let SemanticNucleusValue::ComptimeCall(projection) = value else {
        panic!("the setup comptime call must publish a projection");
    };
    assert!(matches!(
        &projection.result,
        ComptimeCallResultProjection::Value(crate::durable_semantics::DurableConstValue::Integer(
            64
        ))
    ));
    let astgen_after_setup = database
        .declaration_body_plan_astgen_evaluations
        .load(std::sync::atomic::Ordering::Relaxed);
    let probe = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "ready-foreign-comptime-probe",
        |provider| {
            provider.probe_comptime_call(
                &producer,
                &[],
                &[(
                    Arc::from("seed"),
                    crate::durable_semantics::DurableConstValue::Integer(0),
                )],
            )
        },
    );
    let outcome = probe.result.unwrap();
    let crate::body_query::ForeignComptimeCallLookup::Ready(observed) = outcome else {
        panic!("the ready probe must retain the published projection");
    };
    assert_eq!(observed, projection);
    assert!(
        probe
            .dependencies
            .iter()
            .any(|dependency| dependency.family() == "compiler.semantic-nucleus")
    );
    assert!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed)
            == astgen_after_setup,
        "a ready hit must not materialize the body-plan artifact"
    );
    assert!(
        database.semantic_nucleus.contains_retained_key(&key),
        "the exact semantic nucleus key remains the observed dependency"
    );
}

#[test]
fn noncomputing_foreign_probe_adapter_does_not_admit_not_ready() {
    let called = std::cell::Cell::new(false);
    let result =
        foreign_comptime_miss_or_not_ready(rue_query::ReadyQueryProbe::<()>::NotReady, || {
            called.set(true);
            panic!("NotReady must not construct the cold-miss admission");
        })
        .unwrap();
    assert!(matches!(
        result,
        crate::body_query::ForeignComptimeCallLookup::NotReady
    ));
    assert!(!called.get());
}

#[test]
fn noncomputing_foreign_probe_adapter_admits_a_cold_miss_once() {
    let calls = std::cell::Cell::new(0);
    let result = foreign_comptime_miss_or_not_ready(rue_query::ReadyQueryProbe::<()>::Miss, || {
        calls.set(calls.get() + 1);
        Ok(crate::body_query::ForeignComptimeCallLookup::NotReady)
    })
    .unwrap();
    assert!(matches!(
        result,
        crate::body_query::ForeignComptimeCallLookup::NotReady
    ));
    assert_eq!(calls.get(), 1);
}

/// Anonymous identity comes from the canonical candidate artifact. A source
/// comment that used to activate the retired reparse-transport fault seam is
/// now inert and cannot alter the producer's structural anchor.
#[test]
fn comptime_anchor_identity_comes_from_the_candidate_artifact() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, ComptimeCallResultProjection, SemanticNucleusKey as Key,
        SemanticNucleusValue as V,
    };

    let program = "fn Option(comptime T: type) -> type { enum { Some(T), None } }\n\
             fn Wrap(comptime T: type) -> type {\n\
                 // __RUE1089_FAULT_DIVERGE__\n\
                 struct {\n\
                     inner: Option(T),\n\
                     fn get_or(self, d: T) -> T {\n\
                         let O = Option(T);\n\
                         match self.inner { O.Some(v) => v, O.None => d }\n\
                     }\n\
                 }\n\
             }\n\
             fn main() -> i32 { let W = Wrap(i32); let O = Option(i32); let w: W = W { inner: O.Some(42) }; w.get_or(0) }";
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", program)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    &database,
                    revision,
                    &module,
                    Category::Function,
                    "Wrap",
                ),
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([(
                Arc::from("T"),
                crate::durable_semantics::DurableType::I32,
            )]),
            value_arguments: Arc::from([]),
        }),
    );
    match value {
        V::ComptimeCall(projection) => {
            assert!(matches!(
                projection.result,
                ComptimeCallResultProjection::Type(_)
            ));
            assert!(!projection.anonymous_nominals.is_empty());
        }
        other => panic!("candidate-artifact anchor evaluation failed: {other:?}"),
    }
}

#[test]
fn direct_ownership_terminals_accept_droppable_and_reject_linear_payloads() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{SemanticNucleusKey as Key, SemanticNucleusValue as V};

    for (source_text, expected_failure) in [
        (
            "enum Maybe { Some, None } fn Gated(comptime T: type) -> type { @require_droppable(T); T } const G = Gated(Maybe); fn main() {}",
            None,
        ),
        (
            "linear struct Token { v: i32 } fn Gated(comptime T: type) -> type { @require_droppable(T); T } const G = Gated(Token); fn main() {}",
            Some(
                "`@require_droppable` requires a trivially-droppable type, but `Token` is `linear` — an owning growable container (e.g. `ArrayBuf`) cannot yet track element linearity, so the element would be leaked (RUE-388)",
            ),
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(
                &database,
                revision,
                &module,
                Category::ConstCandidate,
                "G",
            ),
            configuration: semantic_configuration(),
        };
        let (resolution, resolution_attempt) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ConstResolution(producer.clone()),
        );
        assert_direct_semantic_observation(
            "ownership-gated const producer",
            &resolution_attempt,
            &[
                "compiler.declaration-body-plan-artifacts",
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.semantic-nucleus",
            ],
            &[
                "compiler.declaration-body-plan-artifacts",
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.module-index",
                "compiler.parse-module",
                "compiler.semantic-nucleus",
            ],
            14,
        );
        let V::ConstResolution(crate::semantic_query_nucleus::ConstResolutionProjection::Value {
            deferred_ownership,
            ..
        }) = resolution
        else {
            panic!("direct const producer failed before its ownership gate: {resolution:?}")
        };
        let [gate] = deferred_ownership.as_ref() else {
            panic!("expected one direct ownership gate: {deferred_ownership:?}")
        };
        let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::DeferredOwnership(crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                producer,
                gate: gate.clone(),
            }),
        );
        assert_direct_semantic_observation(
            "deferred ownership terminal",
            &keyed_attempt,
            &[
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.semantic-nucleus",
            ],
            &[
                "compiler.declaration-body-plan-artifacts",
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.module-index",
                "compiler.parse-module",
                "compiler.semantic-nucleus",
            ],
            18,
        );
        match expected_failure {
            None => assert_eq!(keyed, V::DeferredOwnership),
            Some(expected) => {
                assert_eq!(nucleus_failure_message(&keyed).as_deref(), Some(expected));
            }
        }
    }
}

#[test]
fn ownership_property_memo_preserves_decisions_across_repeats_and_recursion() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{SemanticNucleusKey as Key, SemanticNucleusValue as V};

    const LINEAR: &str = "`@require_droppable` requires a trivially-droppable type, but \
             `Top` is `linear` — an owning growable container (e.g. `ArrayBuf`) cannot yet \
             track element linearity, so the element would be leaked (RUE-388)";
    const LINEAR_A: &str = "`@require_droppable` requires a trivially-droppable type, but \
             `A` is `linear` — an owning growable container (e.g. `ArrayBuf`) cannot yet \
             track element linearity, so the element would be leaked (RUE-388)";

    // Each case gates one type behind `@require_droppable`, which runs the
    // recursive ownership walks. The memo answers the repeated mentions
    // from the first walk, so these pin that a reused answer is the same
    // answer.
    for (name, gated, source_text, expected_failure) in [
        (
            // Every aggregate is mentioned three times, so `Mid` and
            // `Leaf` are each walked once and reused twice.
            "repeated non-linear aggregates stay droppable",
            "G",
            "struct Leaf { a: i64, b: i64 }\n\
                 struct Mid { p: Leaf, q: Leaf, r: Leaf }\n\
                 struct Top { x: Mid, y: Mid, z: Mid }\n\
                 fn Gated(comptime T: type) -> type { @require_droppable(T); T }\n\
                 const G = Gated(Top);\n\
                 fn main() {}",
            None,
        ),
        (
            // `Mid` carries a linear field and is mentioned twice. A memo
            // that stored the wrong answer for the second mention would
            // let this pass.
            "linearity survives a reused aggregate answer",
            "G",
            "linear struct Token { v: i32 }\n\
                 struct Mid { a: Token, b: i64 }\n\
                 struct Top { x: Mid, y: Mid }\n\
                 fn Gated(comptime T: type) -> type { @require_droppable(T); T }\n\
                 const G = Gated(Top);\n\
                 fn main() {}",
            Some(LINEAR),
        ),
        (
            // Mutually recursive through pointers. `B` reaches `A` and `A`
            // reaches `B`, and only `A` owns the linear field, so the two
            // must not share one answer.
            "mutually recursive aggregate without the linear field passes",
            "GB",
            "linear struct T { v: i32 }\n\
                 struct B { q: ptr const A, v: i32 }\n\
                 struct A { p: ptr const B, t: T }\n\
                 fn Gated(comptime X: type) -> type { @require_droppable(X); X }\n\
                 const GB = Gated(B);\n\
                 fn main() {}",
            None,
        ),
        (
            "mutually recursive aggregate with the linear field is rejected",
            "GA",
            "linear struct T { v: i32 }\n\
                 struct B { q: ptr const A, v: i32 }\n\
                 struct A { p: ptr const B, t: T }\n\
                 fn Gated(comptime X: type) -> type { @require_droppable(X); X }\n\
                 const GA = Gated(A);\n\
                 fn main() {}",
            Some(LINEAR_A),
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(
                &database,
                revision,
                &module,
                Category::ConstCandidate,
                gated,
            ),
            configuration: semantic_configuration(),
        };
        let (resolution, _) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ConstResolution(producer.clone()),
        );
        let V::ConstResolution(crate::semantic_query_nucleus::ConstResolutionProjection::Value {
            deferred_ownership,
            ..
        }) = resolution
        else {
            panic!("{name}: producer failed before its ownership gate: {resolution:?}")
        };
        let [gate] = deferred_ownership.as_ref() else {
            panic!("{name}: expected one ownership gate: {deferred_ownership:?}")
        };
        let (keyed, _) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::DeferredOwnership(crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                producer,
                gate: gate.clone(),
            }),
        );
        match expected_failure {
            None => assert_eq!(keyed, V::DeferredOwnership, "{name}"),
            Some(expected) => {
                assert_eq!(
                    nucleus_failure_message(&keyed).as_deref(),
                    Some(expected),
                    "{name}"
                );
            }
        }
    }
}

#[test]
fn direct_family_failures_are_deterministic_without_root_prevalidation() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{SemanticNucleusKey as Key, SemanticNucleusValue as V};

    for (source_text, category, name, identity_terminal, expected) in [
        (
            "drop fn Missing(self) {} fn main() {}",
            Category::Destructor,
            "Missing",
            false,
            "unknown type 'Missing' in destructor",
        ),
        (
            "struct S {} drop fn S(self) {} drop fn S(self) {} fn main() {}",
            Category::Destructor,
            "S",
            true,
            "duplicate destructor for type 'S'",
        ),
        (
            "struct S { fn make(a: i32, a: i32) {} } fn main() {}",
            Category::AssociatedFunction,
            "make",
            false,
            "duplicate parameter name 'a'",
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(&database, revision, &module, category, name),
            configuration: semantic_configuration(),
        };
        let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
            &database,
            revision,
            if identity_terminal {
                Key::Identity(query)
            } else {
                Key::Signature(query)
            },
        );
        if identity_terminal {
            assert_direct_semantic_observation(
                "deterministic destructor identity failure",
                &keyed_attempt,
                &["compiler.declaration-shell", "compiler.semantic-nucleus"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.semantic-nucleus",
                ],
                6,
            );
        } else if category == Category::Destructor {
            assert_direct_semantic_observation(
                "deterministic destructor signature failure",
                &keyed_attempt,
                &["compiler.declaration-shell", "compiler.lookup-name"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                ],
                5,
            );
        } else {
            assert_direct_semantic_observation(
                "deterministic parameter failure",
                &keyed_attempt,
                &["compiler.declaration-shell", "compiler.parse-module"],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.parse-module",
                ],
                4,
            );
        }
        assert!(matches!(keyed, V::Failure(_)));
        assert_eq!(
            nucleus_failure_message(&keyed).as_deref(),
            Some(expected),
            "direct keyed failure diverged for {category:?} {name}: {keyed:?}"
        );
    }
}

#[test]
fn direct_declaration_import_family_matches_independent_import_graph_oracle() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source = source_snapshot(
        &[
            (
                1,
                "/project/main.rue",
                "main.rue",
                "const dep = @import(\"dep.rue\"); fn main() -> i32 { dep.value }",
            ),
            (
                2,
                "/project/dep.rue",
                "dep.rue",
                "pub const value: i32 = 42;",
            ),
        ],
        1,
    );
    let discovered = crate::test_support::test_import_graph(&source).unwrap();
    let main = ModuleId::from_logical_path("main.rue").unwrap();
    let expected = discovered
        .records()
        .iter()
        .find(|record| record.importer() == &main && record.normalized_specifier() == "dep.rue")
        .expect("discovered import graph omitted dep.rue")
        .resolution()
        .clone();

    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    database.adopt_test_import_graph_for_revision(revision, discovered);
    let revision = database.current_semantic_revision().unwrap();
    let requested = database.runtime.request_registered(
        &database.declaration_imports,
        revision,
        declaration_import_key(&main, Category::ConstCandidate, "dep", None, 0, "dep.rue"),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(actual)) =
        requested.terminal().unwrap().outcome()
    else {
        panic!("direct declaration-import terminal failed: {requested:?}")
    };
    assert_eq!(actual, &expected);
    assert_eq!(
        requested
            .dependencies()
            .iter()
            .map(|dependency| dependency.node.family())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "compiler.declaration-occurrence-index",
            "compiler.declaration-shell",
            "compiler.parse-module",
        ]),
        "direct import oracle must not pass through a batch/root semantic adapter"
    );
    assert_eq!(requested.dependencies().len(), 3);
    assert_eq!(requested.inputs().len(), 1);
    assert_eq!(requested.inputs()[0].input, test_import_graph_input());
}

#[test]
fn direct_semantic_keys_own_declaration_validity() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let cases = [
        (
            "struct S { x: i32, x: i64 }",
            Category::Struct,
            "S",
            "duplicate-field",
        ),
        ("enum E { A, A }", Category::Enum, "E", "duplicate-variant"),
        (
            "@copy linear struct L { x: i32 }",
            Category::Struct,
            "L",
            "linear-copy",
        ),
    ];
    for (source_text, category, name, expected) in cases {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let declaration = declaration_candidate(&database, revision, &module, category, name);
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        );
        let valid = matches!(
            (&*expected, &value),
            (
                "duplicate-field",
                Value::Failure(Failure::Diagnostic(
                    rue_error::ErrorKind::DuplicateField { .. }
                ))
            ) | (
                "duplicate-variant",
                Value::Failure(Failure::Diagnostic(
                    rue_error::ErrorKind::DuplicateVariant { .. }
                ))
            ) | (
                "linear-copy",
                Value::Failure(Failure::Diagnostic(rue_error::ErrorKind::LinearStructCopy(
                    _
                )))
            )
        );
        assert!(valid, "direct signature did not own {expected}: {value:?}");
    }

    for (source_text, name, expected) in [
        (
            "drop fn Missing(self) {}",
            "Missing",
            "unknown-destructor-owner",
        ),
        (
            "struct S {} drop fn S(self) {} drop fn S(self) {}",
            "S",
            "duplicate-destructor",
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let declaration =
            declaration_candidate(&database, revision, &module, Category::Destructor, name);
        let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        };
        for key in [Key::Signature(query.clone()), Key::Identity(query.clone())] {
            let value = request_semantic_nucleus(&database, revision, key);
            let valid = match expected {
                "unknown-destructor-owner" => matches!(
                    &value,
                    Value::Failure(Failure::Diagnostic(
                        rue_error::ErrorKind::DestructorUnknownType { .. }
                    ))
                ),
                "duplicate-destructor" => matches!(
                    &value,
                    Value::Failure(Failure::DiagnosticAtDeclaration {
                        kind: rue_error::ErrorKind::DuplicateDestructor { .. },
                        declaration,
                    }) if declaration.duplicate_discriminator == 1
                ),
                _ => false,
            };
            assert!(
                valid,
                "direct destructor terminal did not own {expected}: {value:?}"
            );
        }
    }

    for (source_text, category, name) in [
        (
            "struct S { fn m(self, a: i32, a: i32) {} }",
            Category::Method,
            "m",
        ),
        (
            "struct S { fn make(a: i32, a: i32) {} }",
            Category::AssociatedFunction,
            "make",
        ),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let declaration = declaration_candidate(&database, revision, &module, category, name);
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        );
        assert!(
            matches!(
                value,
                Value::Failure(Failure::DiagnosticAtParameter {
                    kind: rue_error::ErrorKind::DuplicateParameter { .. },
                    ordinal: 1,
                })
            ),
            "direct nested signature lost its duplicate occurrence: {value:?}"
        );
    }

    for (source_text, expected_duplicate) in [
        ("const C: i32 = 1; const C: i32 = 2;", true),
        ("fn C() -> i32 { 0 } const C: i32 = 1;", false),
    ] {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, "C");
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        );
        assert!(
            if expected_duplicate {
                matches!(
                    value,
                    Value::Failure(Failure::Diagnostic(
                        rue_error::ErrorKind::DuplicateConstant { .. }
                    ))
                )
            } else {
                matches!(
                    value,
                    Value::Failure(Failure::Diagnostic(
                        rue_error::ErrorKind::DuplicateMixedKindDefinition { .. }
                    ))
                )
            },
            "direct const key did not own name validity: {value:?}"
        );
    }
}

#[test]
fn direct_const_keys_preserve_structured_evaluator_failures() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct P { x: i32 }\
                 const SIZE: i32 = @size_of(i32);\
                 const AGG: P = P { x: 1 };\
                 const ZERO: i32 = 5 / 0;\
                 const OVF: i32 = 2147483647 + 1;\
                 const LOCAL: u8 = { let y: u8 = 255; y + 1 };\
                 const TARGET: i32 = if @target_arch() == Arch.Linux { 1 } else { 0 };\
                 const BOOL: bool = true != false;",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    assert!(matches!(
        query("SIZE"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::ConstExprNotSupported { .. }
        ))
    ));
    assert!(matches!(
        query("AGG"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::ConstExprNotSupported { .. }
        ))
    ));
    for name in ["ZERO", "OVF", "LOCAL"] {
        assert!(matches!(
            query(name),
            Value::Failure(Failure::Diagnostic(
                rue_error::ErrorKind::ComptimeEvaluationFailed { .. }
            ))
        ));
    }
    assert!(matches!(
        query("TARGET"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::UnknownVariant { .. }
        ))
    ));
    assert!(matches!(
        query("BOOL"),
        Value::ConstResolution(ConstResolutionProjection::Value {
            value,
            ..
        }) if matches!(*value, crate::durable_semantics::DurableConstValue::Bool(true))
    ));
}

#[test]
fn direct_const_named_array_length_uses_the_live_evaluator_policy() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const N: i32 = 3;\n\
                 const GLOBAL: type = [i32; N];\n\
                 fn Shadow(comptime N: i32) -> type { [i32; N] }\n\
                 const LOCAL: type = Shadow(4);\n\
                     const NEG_N: i32 = -1;\n\
                     const NEG: type = [i32; NEG_N];\n\
                     const HUGE_N: i32 = 1;\n\
                     const HUGE: type = [i32; HUGE_N];\n\
                     fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    let expected = crate::durable_semantics::DurableConstValue::Type(
        crate::durable_semantics::DurableType::Array {
            element: Arc::new(crate::durable_semantics::DurableType::I32),
            len: 3,
        },
    );
    let (global, global_attempt) = {
        let declaration = declaration_candidate(
            &database,
            revision,
            &module,
            Category::ConstCandidate,
            "GLOBAL",
        );
        request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    assert!(matches!(
        global,
        Value::ConstResolution(ConstResolutionProjection::Value { value, .. })
            if *value == expected
    ));
    assert_eq!(
        global_attempt
            .dependencies()
            .iter()
            .filter(|dependency| {
                dependency.node.family() == "compiler.semantic-nucleus"
                    && dependency
                        .node
                        .key()
                        .contains("const:8:main.rue:ConstCandidate:1:N:")
            })
            .count(),
        1,
        "unbound global named length must observe exactly one const dependency: {:?}",
        global_attempt.dependencies()
    );
    let expected_local = crate::durable_semantics::DurableConstValue::Type(
        crate::durable_semantics::DurableType::Array {
            element: Arc::new(crate::durable_semantics::DurableType::I32),
            len: 4,
        },
    );
    assert!(matches!(
        query("LOCAL"),
        Value::ConstResolution(ConstResolutionProjection::Value { value, .. })
            if *value == expected_local
    ));
    assert!(matches!(
        query("NEG"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { reason }
        )) if reason == "array length expression 'NEG_N' is negative or too large"
    ));
    // The source language has no integer literal wider than u64. Inject
    // the out-of-range semantic value after the real evaluator resolves
    // HUGE_N, so this still exercises the live ArrayRepeat consumer.
    let _huge_override =
        TestSemanticComptimeArrayLengthOverrideGuard::set(Some(i128::from(u64::MAX) + 1));
    let huge_declaration = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "HUGE",
    );
    let (huge, huge_attempt) = request_semantic_nucleus_observed(
        &database,
        revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: huge_declaration,
            configuration: semantic_configuration(),
        }),
    );
    assert!(matches!(
        huge,
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { reason }
        )) if reason == "array length expression 'HUGE_N' is negative or too large"
    ));
    assert_eq!(
        huge_attempt
            .dependencies()
            .iter()
            .filter(|dependency| {
                dependency
                    .node
                    .key()
                    .contains("const:8:main.rue:ConstCandidate:6:HUGE_N:")
            })
            .count(),
        1,
        "too-large conversion must follow the live HUGE_N lookup: {:?}",
        huge_attempt.dependencies()
    );
}

#[test]
fn direct_const_named_array_length_live_local_kinds_do_not_fall_through() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[
            (
                1,
                "/main.rue",
                "main.rue",
                "const N: i32 = 3;\n\
                     fn BoolShadow(comptime N: bool) -> type { [i32; N] }\n\
                     fn TypeShadow(comptime N: type) -> type { [i32; N] }\n\
                     const BOOL: type = BoolShadow(true);\n\
                     const TYPE: type = TypeShadow(i32);\n\
                     const MODULE: type = { let N = @import(\"dep.rue\"); [i32; N] };\n\
                     const TARGET: type = { let N = @target_arch(); [i32; N] };\n\
                     const CYCLE_A: i32 = CYCLE_B;\n\
                     const CYCLE_B: i32 = CYCLE_A;\n\
                     const CYCLE: type = [i32; CYCLE_A];\n\
                     fn main() -> i32 { 0 }\n",
            ),
            (2, "/dep.rue", "dep.rue", "pub const VALUE: i32 = 1;\n"),
        ],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let discovered = crate::test_support::test_import_graph(&source).unwrap();
    database.adopt_test_import_graph_for_revision(revision, discovered);
    let revision = database.current_semantic_revision().unwrap();
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };

    let expected = [
        (
            "BOOL",
            Failure::Diagnostic(rue_error::ErrorKind::InvalidArrayLength {
                reason: "array length expression 'N' is not an integer".into(),
            }),
        ),
        (
            "TYPE",
            Failure::Diagnostic(rue_error::ErrorKind::InvalidArrayLength {
                reason: "array length expression 'N' is not an integer".into(),
            }),
        ),
        (
            "MODULE",
            Failure::Resolution("module used where a value is required".into()),
        ),
        (
            "TARGET",
            Failure::Resolution(
                "target descriptor used where a durable const value is required".into(),
            ),
        ),
    ];
    for (name, expected) in expected {
        let (value, attempt) = query(name);
        assert!(
            matches!(value, Value::Failure(ref actual) if actual == &expected),
            "{name} must preserve the exact live local-kind failure: {value:?}"
        );
        assert!(
            attempt
                .dependencies()
                .iter()
                .all(|dependency| !dependency.node.key().contains(":ConstCandidate:1:N:")),
            "{name} must not query the same-named global length: {:?}",
            attempt.dependencies()
        );
    }

    let (cycle, cycle_attempt) = query("CYCLE");
    assert!(
        matches!(cycle, Value::Failure(Failure::Cycle(_))),
        "live evaluator cycle must remain an explicit terminal: {cycle:?}"
    );
    assert!(
        cycle_attempt
            .dependencies()
            .iter()
            .any(|dependency| dependency.node.key().contains("CYCLE_A")),
        "cycle terminal should retain the exact named-length observation: {:?}",
        cycle_attempt.dependencies()
    );
}

#[test]
fn live_evaluator_named_global_cancellation_preserves_abort_channel() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::SemanticNucleusKey as Key;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const GLOBAL: i32 = 3;\n\
                 const CANCELED: type = [i32; GLOBAL];\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let declaration = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "CANCELED",
    );
    let checks_before = TEST_NAMED_VALUE_CHECKS.with(std::cell::Cell::get);
    let _cancel_named_value = TestSemanticComptimeNamedValueCancelGuard::set(true);
    let attempt = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        }),
        CancellationToken::new(),
    );
    let checks_after = TEST_NAMED_VALUE_CHECKS.with(std::cell::Cell::get);
    assert!(
        checks_after > checks_before,
        "the live evaluator must reach named-value evaluation before cancellation"
    );
    assert!(
        matches!(attempt.abort(), Some(QueryAbort::Canceled)),
        "named-global cancellation must remain the exact query abort: {:?}",
        attempt.abort()
    );
    assert!(
        attempt
            .dependencies()
            .iter()
            .all(|dependency| !dependency.node.key().contains(":ConstCandidate:1:GLOBAL:")),
        "cancellation before named-value conversion must publish no global dependency: {:?}",
        attempt.dependencies()
    );
}

#[test]
fn live_type_provider_named_array_length_cases_preserve_substitution_and_lookup_channels() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const N: i32 = 3;\n\
                 fn Global(value: [i32; N]) -> i32 { value[0] }\n\
                 fn Good(comptime N: i32, value: [i32; N]) -> i32 { value[0] }\n\
                 fn Bad(comptime N: bool, value: [i32; N]) -> i32 { value[0] }\n\
                 const CYCLE_A: i32 = CYCLE_B;\n\
                 const CYCLE_B: i32 = CYCLE_A;\n\
                 fn Cycle(value: [i32; CYCLE_A]) -> i32 { value[0] }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let signature = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::Function, name);
        request_semantic_nucleus_observed(
            &database,
            revision,
            Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };

    let (global, global_attempt) = signature("Global");
    assert!(
        matches!(global, Value::Signature(_)),
        "unbound global integer length must resolve through the live provider: {global:?}"
    );
    assert_eq!(
        global_attempt
            .dependencies()
            .iter()
            .filter(|dependency| {
                dependency.node.family() == "compiler.semantic-nucleus"
                    && dependency
                        .node
                        .key()
                        .contains("const:8:main.rue:ConstCandidate:1:N:")
            })
            .count(),
        1,
        "global length must observe exactly one const dependency: {:?}",
        global_attempt.dependencies()
    );

    let (good, good_attempt) = signature("Good");
    assert!(
        matches!(good, Value::Signature(_)),
        "deferred integer substitution must remain a live provider result: {good:?}"
    );
    assert!(
        !good_attempt
            .dependencies()
            .iter()
            .any(|dependency| dependency.node.key().contains(":ConstCandidate:1:N:")),
        "integer substitution must not fall through to the global const: {:?}",
        good_attempt.dependencies()
    );

    let (bad, bad_attempt) = signature("Bad");
    let bad_debug = format!("{bad:?}");
    assert!(
        bad_debug.contains("non-integer type") || bad_debug.contains("not an integer"),
        "non-integer substitution must preserve the live provider diagnostic: {bad_debug}"
    );
    assert!(
        !bad_attempt
            .dependencies()
            .iter()
            .any(|dependency| dependency.node.key().contains(":ConstCandidate:1:N:")),
        "non-integer substitution must not query the global const: {:?}",
        bad_attempt.dependencies()
    );

    let (cycle, cycle_attempt) = signature("Cycle");
    let cycle_debug = format!("{cycle:?}");
    assert!(
        matches!(cycle, Value::Failure(Failure::Cycle(_))),
        "provider cycle/abort must remain a terminal rather than a global fallback: {cycle_debug}"
    );
    assert!(
        cycle_attempt
            .dependencies()
            .iter()
            .all(|dependency| !dependency.node.key().contains(":ConstCandidate:1:N:")),
        "cycle failure must not perform an extra unrelated global lookup: {:?}",
        cycle_attempt.dependencies()
    );
}

#[test]
fn live_type_provider_array_length_adapter_preserves_integer_boundaries_without_rir() {
    use crate::durable_semantics::{DurableConstValue as V, DurableType as T};
    use crate::semantic_query_nucleus::SemanticNucleusFailure as Failure;
    use rue_air::SemanticTypeSyntaxProvider;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const HUGE: u64 = 1;\n\
                 const BOOL: i32 = 2;\n\
                 const NEG: i32 = 3;\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let captured = std::cell::RefCell::new(None);
    let attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("live-array-length-boundaries"),
        },
        CancellationToken::new(),
        |context| {
            let mut value_substitutions = BTreeMap::new();
            value_substitutions.insert(Arc::from("HUGE"), V::Integer(i128::from(u64::MAX) + 1));
            value_substitutions.insert(Arc::from("BOOL"), V::Bool(true));
            value_substitutions.insert(Arc::from("NEG"), V::Integer(-1));
            let mut deferred_value_parameters = BTreeMap::new();
            deferred_value_parameters.insert(Arc::from("DEFERRED_I"), T::I32);
            deferred_value_parameters.insert(Arc::from("DEFERRED_B"), T::Bool);
            let dependency_source = StableDefinitionKey::from_stable_parts(
                module.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::Function,
                "probe",
                None,
            );
            let mut provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: semantic_configuration(),
                substitutions: BTreeMap::new(),
                value_substitutions,
                deferred_value_parameters,
                anonymous_nominals: BTreeMap::new(),
                dependency_source,
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Signature,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            let mut resolve = |name: &'static str| {
                <SemanticNucleusTypeProvider<'_> as SemanticTypeSyntaxProvider<
                    ModuleId,
                    ModuleId,
                    StableDefinitionKey,
                    StableDefinitionKey,
                    Arc<str>,
                    crate::DurableType,
                    crate::DurableConstValue,
                >>::resolve_array_length(
                    &mut provider,
                    &module,
                    rue_air::SemanticValueSyntax::Name(name),
                )
            };
            let values = (
                resolve("HUGE"),
                resolve("BOOL"),
                resolve("NEG"),
                resolve("DEFERRED_I"),
                resolve("DEFERRED_B"),
                provider.dependencies.clone(),
            );
            *captured.borrow_mut() = Some(values);
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    let attempt = attempt.expect("live provider probe must publish");
    assert!(
        matches!(attempt.outcome(), rue_query::QueryOutcome::Success(_)),
        "live provider probe must publish"
    );
    let (huge, bool_value, negative, deferred_integer, deferred_bool, dependencies) = captured
        .into_inner()
        .expect("provider probe captured its values");
    assert!(matches!(
        huge,
        Err(rue_air::SemanticProviderError::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { ref reason }
        ))) if reason == "array length 'HUGE' (18446744073709551616) is too large"
    ));
    assert!(matches!(
        bool_value,
        Err(rue_air::SemanticProviderError::Failure(Failure::Resolution(reason)))
            if reason.as_ref() == "array length `BOOL` is not an integer"
    ));
    assert!(matches!(
        negative,
        Err(rue_air::SemanticProviderError::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { ref reason }
        ))) if reason == "array length 'NEG' is negative (-1)"
    ));
    assert!(matches!(deferred_integer, Ok(None)));
    assert!(matches!(
        deferred_bool,
        Err(rue_air::SemanticProviderError::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::InvalidArrayLength { ref reason }
        ))) if reason == "array length expression 'DEFERRED_B' has non-integer type bool"
    ));
    assert!(
        dependencies.is_empty(),
        "all lexical substitutions must avoid global lookup/dependency: {dependencies:?}"
    );

    let cancellation = CancellationToken::new();
    let cancel_in_closure = cancellation.clone();
    let canceled_value = std::cell::RefCell::new(None);
    let _canceled_attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("live-array-length-canceled-global"),
        },
        cancellation,
        |context| {
            let dependency_source = StableDefinitionKey::from_stable_parts(
                module.clone(),
                crate::StableDefinitionNamespace::Value,
                crate::StableDefinitionKind::Function,
                "canceled_probe",
                None,
            );
            let mut provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: semantic_configuration(),
                substitutions: BTreeMap::new(),
                value_substitutions: BTreeMap::new(),
                deferred_value_parameters: BTreeMap::new(),
                anonymous_nominals: BTreeMap::new(),
                dependency_source,
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Signature,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            cancel_in_closure.cancel();
            let result = <SemanticNucleusTypeProvider<'_> as SemanticTypeSyntaxProvider<
                ModuleId,
                ModuleId,
                StableDefinitionKey,
                StableDefinitionKey,
                Arc<str>,
                crate::DurableType,
                crate::DurableConstValue,
            >>::resolve_array_length(
                &mut provider,
                &module,
                rue_air::SemanticValueSyntax::Name("UNBOUND"),
            );
            *canceled_value.borrow_mut() = Some(result);
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    assert!(matches!(
        canceled_value.into_inner(),
        Some(Err(rue_air::SemanticProviderError::Abort(
            QueryAbort::Canceled
        )))
    ));
}

#[test]
#[should_panic(expected = "controlled operation panic")]
fn restored_state_kernel_restores_exact_state_when_operation_panics() {
    use std::cell::RefCell;
    use std::rc::Rc;

    struct RestorationAssertion {
        state: Rc<RefCell<BTreeMap<Arc<str>, i32>>>,
        expected: BTreeMap<Arc<str>, i32>,
    }

    impl Drop for RestorationAssertion {
        fn drop(&mut self) {
            assert_eq!(*self.state.borrow(), self.expected);
        }
    }

    let expected = BTreeMap::from([(Arc::from("OLD"), 7_i32)]);
    let state = Rc::new(RefCell::new(expected.clone()));
    let _assertion = RestorationAssertion {
        state: Rc::clone(&state),
        expected,
    };
    let mut active = Rc::clone(&state);
    super::with_restored_state(
        &mut active,
        |state| {
            std::mem::replace(
                &mut *state.borrow_mut(),
                BTreeMap::from([(Arc::from("TRANSIENT"), 9)]),
            )
        },
        |_state| -> () { panic!("controlled operation panic") },
        |state, old| *state.borrow_mut() = old,
    );
}

#[test]
fn live_root_authority_resolves_keyed_substitutions_and_restores_provider_state() {
    use crate::body_query::{DurableComptimeProgramPlan, OwnedComptimeProgramCore};
    use crate::durable_semantics::{DurableConstValue as V, DurableType as T};
    use rue_rir::InstData;

    let snapshot = SourceSnapshot::single(
            "root-type-seam.rue",
            "struct NamedType {} fn target(value: T, count: [i32; N], bad: [i32; MISSING], named: [NamedType; N2]) -> i32 { 1 }",
        )
        .unwrap();
    let module = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
        .unwrap()
        .modules()[0]
        .clone();
    let candidate = module
        .definitions()
        .declaration_keys_in_source_order()
        .find(|candidate| candidate.name.as_ref() == "target")
        .unwrap()
        .clone();
    let artifacts =
        crate::canonical_lower::lower_parsed_declaration_body_plan(&module, &candidate, || Ok(()))
            .unwrap();
    let configuration = semantic_configuration();
    let program_key = crate::body_query::DurableComptimeProgramKey {
        declaration: StableDefinitionKey::from_stable_parts(
            candidate.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "target",
            None,
        ),
        configuration: configuration.clone(),
    };
    let core = OwnedComptimeProgramCore::from_callable_body_plan_without_imports(
        DurableComptimeProgramPlan {
            key: program_key.clone(),
            candidate: candidate.clone(),
        },
        &artifacts,
        || Ok(()),
    )
    .unwrap();
    // Build a deliberately qualified enum node directly in RIR.  The
    // source parser's dotted expression is a FieldGet until semantic
    // resolution, so a source-only fixture cannot exercise AIR's
    // pre-child EnumVariant admission contract.
    let mut qualified_editor = rue_rir::RirEditor::new();
    let qualified_interner = lasso::ThreadedRodeo::new();
    let module_symbol = qualified_interner.get_or_intern("module");
    let type_symbol = qualified_interner.get_or_intern("Arch");
    let variant_symbol = qualified_interner.get_or_intern("X86_64");
    let module_ref = qualified_editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: module_symbol,
            anchor: None,
        },
        span: rue_span::Span::new(0, 6),
    });
    let qualified_body = qualified_editor.add_inst(rue_rir::Inst {
        data: InstData::EnumVariant {
            module: Some(module_ref),
            type_name: type_symbol,
            variant: variant_symbol,
        },
        span: rue_span::Span::new(0, 14),
    });
    let qualified_rir = rue_rir::ValidatedRir::finish(
        qualified_editor,
        &rue_rir::RirValidationContext {
            symbol_count: qualified_interner.len(),
            source_lengths: &[(FileId::DEFAULT, 32)],
        },
    )
    .unwrap();
    let qualified_symbols: Arc<[Arc<str>]> = (0..qualified_interner.len())
        .map(|index| {
            Arc::from(
                qualified_interner
                    .resolve(&lasso::Spur::try_from_usize(index).unwrap())
                    .to_owned(),
            )
        })
        .collect();
    let qualified_key = crate::body_query::DurableComptimeProgramKey {
        declaration: StableDefinitionKey::from_stable_parts(
            candidate.module.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "qualified_test",
            None,
        ),
        configuration: configuration.clone(),
    };
    let qualified_core = OwnedComptimeProgramCore::from_test_rir(
        DurableComptimeProgramPlan {
            key: qualified_key.clone(),
            candidate: candidate.clone(),
        },
        qualified_rir,
        qualified_symbols,
        qualified_body,
        qualified_body,
    );
    let root = core.callable().unwrap().root;
    let (type_syntax, value_syntax, abort_syntax, named_syntax) = match &core.rir.get(root).data {
        InstData::FnDecl { params, .. } => {
            let params = core.rir.params(params).iter();
            let params = params.collect::<Vec<_>>();
            (params[0].ty, params[1].ty, params[2].ty, params[3].ty)
        }
        other => panic!("callable core has unexpected root: {other:?}"),
    };

    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
    let cancellation = CancellationToken::new();
    let cancel_in_closure = cancellation.clone();
    let captured = std::cell::RefCell::new(None);
    let attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("live-root-authority-type-substitutions"),
        },
        cancellation,
        |context| {
            let dependency_source = program_key.declaration.clone();
            let provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: configuration.clone(),
                substitutions: BTreeMap::from([(Arc::from("OLD"), T::I8)]),
                value_substitutions: BTreeMap::from([(Arc::from("OLD"), V::Integer(7))]),
                deferred_value_parameters: BTreeMap::new(),
                anonymous_nominals: BTreeMap::new(),
                dependency_source,
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Signature,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            let session = crate::durable_comptime::DurableComptimeSession::new(
                program_key.declaration.clone(),
                candidate.clone(),
            )
            .unwrap();
            let mut authority = DurableComptimeRootAuthority {
                provider,
                imports: database.declaration_imports.clone(),
                session,
                foreign: DurableComptimeForeignQueryAuthority {
                    context,
                    semantic_nucleus: &database.semantic_nucleus,
                    declaration_body_plan_artifacts: &database.declaration_body_plan_artifacts,
                    configuration: &configuration,
                },
            };
            authority.session.register_program(&core).unwrap();
            authority.session.register_program(&qualified_core).unwrap();

            // Exercise the canonical AIR host with exact registered RIR.
            // Production roots use the same engine/host path below.
            let callable = core.callable().expect("callable fixture").clone();
            let mut host = crate::durable_comptime::DurableComptimeHost::new(&mut authority);
            let mut env = rue_air::ComptimeEnv::new();
            let outcome = rue_air::ComptimeEngine::new(&mut host).evaluate(
                rue_air::ComptimeFrame::expression(program_key.clone(), callable.body),
                &mut env,
            );
            assert!(matches!(
                outcome,
                rue_air::ComptimeOutcome::Known(
                    crate::durable_comptime::EvaluatedSemanticConst::Value(value)
                ) if matches!(value.value, crate::durable_semantics::DurableConstValue::Integer(1))
            ));

            drop(host);

            let expected_types = BTreeMap::from([(Arc::from("OLD"), T::I8)]);
            let expected_values = BTreeMap::from([(Arc::from("OLD"), V::Integer(7))]);
            let wrong_program = crate::body_query::DurableComptimeProgramKey {
                declaration: StableDefinitionKey::from_stable_parts(
                    candidate.module.clone(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::Function,
                    "wrong",
                    None,
                ),
                configuration: configuration.clone(),
            };
            let (type_result, value_result, restored_after_success) = {
                let mut services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                let type_result = services
                    .resolve_type_syntax_with_substitutions(
                        &program_key,
                        type_syntax,
                        &[(Arc::from("T"), T::I64)],
                        &[(Arc::from("T"), V::Integer(9))],
                    )
                    .unwrap();
                let value_result = services
                    .resolve_type_syntax_with_substitutions(
                        &program_key,
                        value_syntax,
                        &[],
                        &[(Arc::from("N"), V::Integer(3))],
                    )
                    .unwrap();
                let restored = authority.provider.substitutions == expected_types
                    && authority.provider.value_substitutions == expected_values;
                (type_result, value_result, restored)
            };

            let provider_failure = {
                let mut services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.resolve_type_syntax_with_substitutions(
                    &wrong_program,
                    type_syntax,
                    &[(Arc::from("T"), T::U8)],
                    &[(Arc::from("N"), V::Integer(4))],
                )
            };
            assert!(matches!(
                &provider_failure,
                Err(rue_air::SemanticTypeSyntaxError::ProviderFailure(_))
            ));
            assert_eq!(authority.provider.substitutions, expected_types);
            assert_eq!(authority.provider.value_substitutions, expected_values);

            let dependencies_before = authority.provider.dependencies.clone();
            let named_result = {
                let mut services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.resolve_type_syntax_with_substitutions(
                    &program_key,
                    named_syntax,
                    &[],
                    &[(Arc::from("N2"), V::Integer(2))],
                )
            };
            assert!(matches!(
                &named_result,
                Ok(T::Array { element, len: 2 })
                    if matches!(element.as_ref(), T::Nominal(key) if key.name() == "NamedType")
            ));
            let new_dependencies = authority
                .provider
                .dependencies
                .difference(&dependencies_before)
                .cloned()
                .collect::<BTreeSet<_>>();
            assert_eq!(new_dependencies.len(), 1);
            assert!(new_dependencies.iter().all(|dependency| matches!(
                &dependency.target,
                crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                    target
                ) if target.module() == &candidate.module
                    && target.name() == "NamedType"
            )));
            assert_eq!(authority.provider.substitutions, expected_types);
            assert_eq!(authority.provider.value_substitutions, expected_values);

            // The qualified EnumVariant fixture is registered above and
            // evaluated through the canonical AIR dispatcher. Cancellation
            // is armed by the host at the admission boundary as a
            // child-evaluation tripwire: if AIR ever evaluates
            // `module_ref` before admission rejects the path, its eval
            // checkpoint must return QueryAbort::Canceled rather than the
            // expected typed semantic HostFailure.
            crate::durable_comptime::set_enum_variant_child_tripwire(Some(
                cancel_in_closure.clone(),
            ));
            let dependencies_before_qualified = authority.provider.dependencies.clone();
            let mut qualified_env = rue_air::ComptimeEnv::new();
            let mut qualified_host =
                crate::durable_comptime::DurableComptimeHost::new(&mut authority);
            let qualified_outcome = rue_air::ComptimeEngine::new(&mut qualified_host).evaluate(
                rue_air::ComptimeFrame::expression(qualified_key, qualified_body),
                &mut qualified_env,
            );
            let rue_air::ComptimeOutcome::HostFailure(qualified_failure) = qualified_outcome else {
                panic!("qualified enum should be a durable host failure: {qualified_outcome:?}");
            };
            assert!(matches!(
                qualified_failure.semantic_failure(),
                Some(crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(
                    message
                )) if message.as_ref() == "expression is not supported in declaration-time comptime"
            ));
            drop(qualified_host);
            crate::durable_comptime::set_enum_variant_child_tripwire(None);
            assert_eq!(
                authority.provider.dependencies, dependencies_before_qualified,
                "qualified enum admission must not evaluate its module child"
            );
            cancel_in_closure.cancel();
            let aborted = {
                let mut services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.resolve_type_syntax_with_substitutions(
                    &program_key,
                    abort_syntax,
                    &[],
                    &[],
                )
            };
            *captured.borrow_mut() = Some((
                type_result,
                value_result,
                restored_after_success,
                provider_failure,
                named_result,
                aborted,
                authority.provider.substitutions.clone(),
                authority.provider.value_substitutions.clone(),
            ));
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    assert!(
        attempt.is_err(),
        "the injected cancellation should abort the probe"
    );
    let (
        type_result,
        value_result,
        restored,
        provider_failure,
        named_result,
        aborted,
        restored_types,
        restored_values,
    ) = captured.into_inner().unwrap();
    assert_eq!(type_result, T::I64);
    assert_eq!(
        value_result,
        T::Array {
            element: Arc::new(T::I32),
            len: 3,
        }
    );
    assert!(restored);
    assert!(matches!(
        &provider_failure,
        Err(rue_air::SemanticTypeSyntaxError::ProviderFailure(_))
    ));
    assert!(named_result.is_ok());
    assert!(matches!(
        aborted,
        Err(rue_air::SemanticTypeSyntaxError::ProviderAbort(
            QueryAbort::Canceled
        ))
    ));
    assert_eq!(restored_types, BTreeMap::from([(Arc::from("OLD"), T::I8)]));
    assert_eq!(
        restored_values,
        BTreeMap::from([(Arc::from("OLD"), V::Integer(7))])
    );
}

#[test]
fn production_root_authority_keyed_admission_preserves_identity_and_dependency() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticDeclarationDependency, SemanticDeclarationDependencyTarget as Target,
        SemanticNucleusFailure as Failure,
    };

    let source = source_snapshot(
        &[
            (1, "/left.rue", "left.rue", "fn target() -> i32 { 1 }\n"),
            (2, "/right.rue", "right.rue", "fn target() -> i32 { 2 }\n"),
        ],
        1,
    );
    let left_module = ModuleId::from_logical_path("left.rue").unwrap();
    let right_module = ModuleId::from_logical_path("right.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let left_candidate = declaration_candidate(
        &database,
        revision,
        &left_module,
        Category::Function,
        "target",
    );
    let right_candidate = declaration_candidate(
        &database,
        revision,
        &right_module,
        Category::Function,
        "target",
    );
    let configuration = semantic_configuration();
    let left_head = StableDefinitionKey::from_stable_parts(
        left_module.clone(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "target",
        None,
    );
    let right_head = StableDefinitionKey::from_stable_parts(
        right_module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "target",
        None,
    );
    let accessing_source = StableDefinitionKey::from_stable_parts(
        left_module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "caller",
        None,
    );
    let unknown_head = StableDefinitionKey::from_stable_parts(
        accessing_source.module().clone(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        "missing",
        None,
    );
    let mismatched_head = StableDefinitionKey::from_stable_parts(
        accessing_source.module().clone(),
        crate::StableDefinitionNamespace::Type,
        crate::StableDefinitionKind::Function,
        "target",
        None,
    );

    let captured = std::cell::RefCell::new(None);
    let attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("production-keyed-call-admission"),
        },
        CancellationToken::new(),
        |context| {
            let provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: configuration.clone(),
                substitutions: BTreeMap::new(),
                value_substitutions: BTreeMap::new(),
                deferred_value_parameters: BTreeMap::new(),
                anonymous_nominals: BTreeMap::new(),
                dependency_source: accessing_source.clone(),
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Body,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            let session = crate::durable_comptime::DurableComptimeSession::new(
                left_head.clone(),
                left_candidate.clone(),
            )
            .unwrap();
            let mut authority = DurableComptimeRootAuthority {
                provider,
                imports: database.declaration_imports.clone(),
                session,
                foreign: DurableComptimeForeignQueryAuthority {
                    context,
                    semantic_nucleus: &database.semantic_nucleus,
                    declaration_body_plan_artifacts: &database.declaration_body_plan_artifacts,
                    configuration: &configuration,
                },
            };
            let first = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &left_head)
            };
            let second = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &right_head)
            };
            let unknown = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &unknown_head)
            };
            let mismatched = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &mismatched_head)
            };
            *captured.borrow_mut() = Some((first, second, unknown, mismatched));
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    assert!(
        attempt.is_ok(),
        "production authority probe should complete"
    );
    let (first, second, unknown, mismatched) = captured.into_inner().unwrap();
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.candidate, left_candidate);
    assert_eq!(second.candidate, right_candidate);
    assert_eq!(first.identity.key, left_head);
    assert_eq!(second.identity.key, right_head);
    assert_eq!(first.configuration, configuration);
    assert_eq!(second.configuration, configuration);
    for (admission, head) in [(&first, &left_head), (&second, &right_head)] {
        assert_eq!(
            admission.dependency,
            SemanticDeclarationDependency {
                source: accessing_source.clone(),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target: Target::NamedValue(head.clone()),
            }
        );
    }
    assert!(
        matches!(&unknown, Err(rue_air::SemanticProviderError::Failure(_))),
        "unexpected unknown-head result: {unknown:?}"
    );
    assert!(
        format!("{unknown:?}").contains("missing"),
        "unknown-head failure lost its requested spelling: {unknown:?}"
    );
    assert!(matches!(
        &mismatched,
        Err(rue_air::SemanticProviderError::Failure(Failure::Resolution(reason)))
            if reason.as_ref() == "comptime function identity does not match requested key"
    ));

    let cancellation = CancellationToken::new();
    let cancel_in_closure = cancellation.clone();
    let aborted = std::cell::RefCell::new(None);
    let _attempt = database.runtime.query(
        &database.provider_probe,
        revision,
        ProviderProbeKey {
            label: Arc::from("production-keyed-call-admission-abort"),
        },
        cancellation,
        |context| {
            cancel_in_closure.cancel();
            let provider = SemanticNucleusTypeProvider {
                context,
                family: &database.semantic_nucleus,
                shells: &database.declaration_shells,
                names: &database.lookup_names,
                configuration: configuration.clone(),
                substitutions: BTreeMap::new(),
                value_substitutions: BTreeMap::new(),
                deferred_value_parameters: BTreeMap::new(),
                anonymous_nominals: BTreeMap::new(),
                dependency_source: accessing_source.clone(),
                dependency_kind: rue_air::DeclarationTypeDependencyKind::Body,
                dependencies: BTreeSet::new(),
                deferred_ownership: BTreeSet::new(),
                ownership_properties: BTreeMap::new(),
            };
            let session = crate::durable_comptime::DurableComptimeSession::new(
                left_head.clone(),
                left_candidate.clone(),
            )
            .unwrap();
            let mut authority = DurableComptimeRootAuthority {
                provider,
                imports: database.declaration_imports.clone(),
                session,
                foreign: DurableComptimeForeignQueryAuthority {
                    context,
                    semantic_nucleus: &database.semantic_nucleus,
                    declaration_body_plan_artifacts: &database.declaration_body_plan_artifacts,
                    configuration: &configuration,
                },
            };
            let result = {
                let services =
                    crate::durable_comptime::DurableComptimeServices::new(&mut authority);
                services.begin_comptime_call_admission_for_key(&accessing_source, &left_head)
            };
            *aborted.borrow_mut() = Some(result);
            Ok(rue_query::QueryOutput::success(ProviderProbeValue))
        },
    );
    assert!(matches!(
        aborted.into_inner(),
        Some(Err(rue_air::SemanticProviderError::Abort(
            QueryAbort::Canceled
        )))
    ));
}

#[test]
fn durable_callable_admission_pipeline_preserves_policy_table() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ComptimeCallQueryKey, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let cases = [
        (
            "undefined",
            "fn outer() -> i32 { missing() }",
            "undefined comptime function `missing`",
        ),
        (
            "arity",
            "fn target(comptime x: i32) -> i32 { x } fn outer() -> i32 { target(1 / 0, 2) }",
            "wrong arity",
        ),
        (
            "borrow mode",
            "fn target(borrow x: i32) -> i32 { x } fn outer() -> i32 { target(1) }",
            "BorrowKeywordMissing",
        ),
        (
            "inout mode",
            "fn target(inout x: i32) -> i32 { x } fn outer() -> i32 { target(1) }",
            "InoutKeywordMissing",
        ),
        (
            "unexpected borrow",
            "fn target(x: i32) -> i32 { x } fn outer() -> i32 { target(borrow 1) }",
            "UnexpectedCallArgumentMode",
        ),
        (
            "unexpected inout",
            "fn target(x: i32) -> i32 { x } fn outer() -> i32 { target(inout 1) }",
            "UnexpectedCallArgumentMode",
        ),
        (
            "nullary value",
            "fn target() -> i32 { 1 } fn outer() -> i32 { target() }",
            "ConstExprNotSupported",
        ),
        (
            "mixed comptime/runtime",
            "fn target(comptime x: i32, y: i32) -> i32 { x + y } fn outer() -> i32 { target(1, 2) }",
            "ConstExprNotSupported",
        ),
    ];

    for (label, source_text, expected) in cases {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let declaration =
            declaration_candidate(&database, revision, &module, Category::Function, "outer");
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::ComptimeCall(ComptimeCallQueryKey {
                declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration,
                    configuration: semantic_configuration(),
                },
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([]),
            }),
        );
        let diagnostic = format!("{value:?}");
        assert!(
            diagnostic.contains(expected),
            "{label} lost its canonical admission diagnostic: {diagnostic}"
        );
    }

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn target() -> type { i32 } fn outer() -> type { target() }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let declaration =
        declaration_candidate(&database, revision, &module, Category::Function, "outer");
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([]),
            value_arguments: Arc::from([]),
        }),
    );
    assert!(
        matches!(
            value,
            Value::ComptimeCall(crate::semantic_query_nucleus::ComptimeCallProjection {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Type(
                    crate::durable_semantics::DurableType::I32
                ),
                ..
            })
        ),
        "nullary type function should remain an admitted callable: {value:?}"
    );

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn target(comptime x: i32, comptime y: i32) -> i32 { x * 10 + y } fn outer() -> i32 { target(1, 2) }",
        )],
        1,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let declaration =
        declaration_candidate(&database, revision, &module, Category::Function, "outer");
    let target = declaration_candidate(&database, revision, &module, Category::Function, "target");
    let identity = |candidate: crate::declaration_candidate::DeclarationCandidateKey| {
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::Identity(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: candidate,
                configuration: semantic_configuration(),
            }),
        );
        let Value::Identity(identity) = value else {
            panic!("expected callable identity, got {value:?}")
        };
        identity.key
    };
    let outer_identity = identity(declaration.clone());
    let target_identity = identity(target);
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            },
            type_arguments: Arc::from([]),
            value_arguments: Arc::from([]),
        }),
    );
    assert!(
        matches!(
            value,
            Value::ComptimeCall(crate::semantic_query_nucleus::ComptimeCallProjection {
                result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(
                    crate::durable_semantics::DurableConstValue::Integer(12)
                ),
                ref dependencies,
                ..
            })
            if dependencies.as_ref()
                == [crate::semantic_query_nucleus::SemanticDeclarationDependency {
                    source: outer_identity,
                    kind: rue_air::DeclarationTypeDependencyKind::Body,
                    target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        target_identity,
                    ),
                }]
        ),
        "ordered parameters and exact published dependency should survive admission: {value:?}"
    );
}

#[test]
fn durable_named_value_projection_covers_each_lookup_kind_and_dependency() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticDeclarationDependencyTarget, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const SCALAR: i32 = 7;\
                 fn Callable() -> i32 { 1 }\
                 struct StructValue { field: i32 }\
                 enum EnumValue { One }\
                 const INNER: i32 = 5;\
                 const SOURCE: i32 = INNER;\
                 const OUT_CHAIN: i32 = SOURCE;\
                 const OUT_SCALAR: i32 = SCALAR;\
                 const OUT_CALLABLE = Callable;\
                 const OUT_STRUCT = StructValue;\
                 const OUT_ENUM = EnumValue;\
                 const OUT_LOCAL: i32 = { let SCALAR: i32 = 9; SCALAR };\
                 const OUT_UNDEFINED: i32 = MISSING;",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    let identity = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
        let Value::Identity(identity) = request_semantic_nucleus(
            &database,
            revision,
            Key::Identity(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        ) else {
            panic!("expected an identity for {name}");
        };
        identity.key
    };

    let stable_key = |namespace, kind, name| {
        crate::StableDefinitionKey::from_stable_parts(module.clone(), namespace, kind, name, None)
    };
    let assert_value = |name: &str,
                        expected_value: crate::durable_semantics::DurableConstValue,
                        expected_type: crate::durable_semantics::DurableType,
                        expected_target: crate::StableDefinitionKey| {
        let Value::ConstResolution(ConstResolutionProjection::Value {
            value,
            ty,
            dependencies,
            ..
        }) = query(name)
        else {
            panic!("expected a durable value projection for {name}");
        };
        assert_eq!(*value, expected_value);
        assert_eq!(ty, expected_type);
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].source, identity(name));
        assert_eq!(
            dependencies[0].kind,
            rue_air::DeclarationTypeDependencyKind::Body
        );
        assert_eq!(
            dependencies[0].target,
            SemanticDeclarationDependencyTarget::NamedValue(expected_target)
        );
    };
    assert_value(
        "OUT_SCALAR",
        crate::durable_semantics::DurableConstValue::Integer(7),
        crate::durable_semantics::DurableType::I32,
        stable_key(
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
            "SCALAR",
        ),
    );
    assert_value(
        "OUT_CALLABLE",
        crate::durable_semantics::DurableConstValue::Function(stable_key(
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "Callable",
        )),
        crate::durable_semantics::DurableType::ComptimeType,
        stable_key(
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "Callable",
        ),
    );
    for (name, nominal_kind, nominal_name) in [
        (
            "OUT_STRUCT",
            crate::StableDefinitionKind::Struct,
            "StructValue",
        ),
        ("OUT_ENUM", crate::StableDefinitionKind::Enum, "EnumValue"),
    ] {
        let Value::ConstResolution(ConstResolutionProjection::Value {
            value,
            ty,
            dependencies,
            ..
        }) = query(name)
        else {
            panic!("expected a durable nominal projection for {name}");
        };
        let nominal_key = stable_key(
            crate::StableDefinitionNamespace::Type,
            nominal_kind,
            nominal_name,
        );
        assert_eq!(
            *value,
            crate::durable_semantics::DurableConstValue::Type(
                crate::durable_semantics::DurableType::Nominal(nominal_key.clone())
            )
        );
        assert_eq!(ty, crate::durable_semantics::DurableType::ComptimeType);
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].source, identity(name));
        assert_eq!(
            dependencies[0].kind,
            rue_air::DeclarationTypeDependencyKind::Body
        );
        assert_eq!(
            dependencies[0].target,
            SemanticDeclarationDependencyTarget::NamedValue(nominal_key)
        );
    }
    assert_value(
        "OUT_CHAIN",
        crate::durable_semantics::DurableConstValue::Integer(5),
        crate::durable_semantics::DurableType::I32,
        stable_key(
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
            "SOURCE",
        ),
    );
    let Value::ConstResolution(ConstResolutionProjection::Value {
        value,
        ty,
        dependencies,
        ..
    }) = query("OUT_LOCAL")
    else {
        panic!("expected a local shadowing projection");
    };
    assert_eq!(
        *value,
        crate::durable_semantics::DurableConstValue::Integer(9)
    );
    assert_eq!(ty, crate::durable_semantics::DurableType::I32);
    assert!(
        dependencies.is_empty(),
        "locals must not become named-value dependencies"
    );
    let undefined = format!("{:?}", query("OUT_UNDEFINED"));
    assert!(undefined.contains("undefined constant"), "{undefined}");
}

#[test]
fn durable_named_value_projection_preserves_real_module_binding_identity() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let context =
        ImportDiscoveryContext::new(902, "/project", Some("/sdk"), "test-policy").unwrap();
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/project/main.rue",
        "/project/main.rue",
        PhysicalFileIdentity::new(1, 1),
        FileMetadataFingerprint::new(1, 2, 3),
        Arc::new(
            "const M = @import(\"dep.rue\");\
                 const OUT_MODULE = M;"
                .to_owned(),
        ),
    )
    .unwrap();
    assembler
        .add_explicit(
            "/project/dep.rue",
            "/project/dep.rue",
            PhysicalFileIdentity::new(2, 1),
            FileMetadataFingerprint::new(4, 5, 6),
            Arc::new("const INNER: i32 = 1;".to_owned()),
        )
        .unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let (snapshot, reads, import_revision, plan) =
        begin_database_plan(&mut database, &mut assembler, context);
    let import_revision =
        publish_manifest_observations(&mut database, &snapshot, reads, &plan, import_revision);
    let revision = Revision::new(
        import_revision.revision_id,
        import_revision.compatibility_token,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let declaration = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "OUT_MODULE",
    );
    let Value::ConstResolution(ConstResolutionProjection::ModuleBinding { key, target }) =
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    else {
        panic!("expected a real imported module binding projection");
    };
    assert_eq!(
        key,
        crate::StableDefinitionKey::from_stable_parts(
            module,
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ModuleBinding,
            "OUT_MODULE",
            None,
        )
    );
    assert_eq!(target, ModuleId::from_logical_path("dep.rue").unwrap());
}

#[test]
fn durable_module_member_projection_preserves_order_types_and_dependencies() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection, SemanticDeclarationDependencyTarget,
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let context =
        ImportDiscoveryContext::new(903, "/project", Some("/sdk"), "test-policy").unwrap();
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/project/main.rue",
        "/project/main.rue",
        PhysicalFileIdentity::new(11, 1),
        FileMetadataFingerprint::new(11, 2, 3),
        Arc::new(
            "const M = @import(\"dep.rue\");\
                 const OUT_SCALAR: i32 = M.SCALAR;\
                 const OUT_CALLABLE = M.Callable;\
                 const OUT_STRUCT = M.StructValue;\
                 const OUT_ENUM = M.EnumValue;\
                 const OUT_NESTED = M.NESTED;\
                 const OUT_UNKNOWN: i32 = M.MISSING;\
                 const OUT_NONMODULE: i32 = OUT_SCALAR.missing;"
                .to_owned(),
        ),
    )
    .unwrap();
    assembler
        .add_explicit(
            "/project/dep.rue",
            "/project/dep.rue",
            PhysicalFileIdentity::new(12, 1),
            FileMetadataFingerprint::new(14, 5, 6),
            Arc::new(
                "pub const INNER: i32 = 2;\
                     pub const SCALAR: i32 = INNER + 5;\
                     pub fn Callable() -> i32 { 1 }\
                     pub struct StructValue { field: i32 }\
                     pub enum EnumValue { One }\
                     pub const NESTED = @import(\"nested.rue\");"
                    .to_owned(),
            ),
        )
        .unwrap();
    assembler
        .add_explicit(
            "/project/nested.rue",
            "/project/nested.rue",
            PhysicalFileIdentity::new(13, 1),
            FileMetadataFingerprint::new(17, 8, 9),
            Arc::new("const LEAF: i32 = 1;".to_owned()),
        )
        .unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let (snapshot, reads, import_revision, plan) =
        begin_database_plan(&mut database, &mut assembler, context);
    let import_revision =
        publish_manifest_observations(&mut database, &snapshot, reads, &plan, import_revision);
    let revision = Revision::new(
        import_revision.revision_id,
        import_revision.compatibility_token,
    );
    let root = ModuleId::from_logical_path("main.rue").unwrap();
    let dep = ModuleId::from_logical_path("dep.rue").unwrap();
    let nested = ModuleId::from_logical_path("nested.rue").unwrap();
    let query = |name: &str| {
        let declaration =
            declaration_candidate(&database, revision, &root, Category::ConstCandidate, name);
        request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        )
    };
    let stable_key = |module: ModuleId, namespace, kind, name: &str| {
        crate::StableDefinitionKey::from_stable_parts(module, namespace, kind, name, None)
    };
    let direct_target =
        |name: &str, module: ModuleId, namespace, kind| stable_key(module, namespace, kind, name);
    let expected_dependencies = |name: &str, target: crate::StableDefinitionKey| {
        vec![
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: stable_key(
                    root.clone(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::ValueConst,
                    name,
                ),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target: SemanticDeclarationDependencyTarget::NamedValue(stable_key(
                    root.clone(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::ModuleBinding,
                    "M",
                )),
            },
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: stable_key(
                    root.clone(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::ValueConst,
                    name,
                ),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target: SemanticDeclarationDependencyTarget::NamedValue(target),
            },
        ]
    };
    let assert_direct_value = |name: &str, value, ty, target: crate::StableDefinitionKey| {
        let Value::ConstResolution(ConstResolutionProjection::Value {
            value: actual,
            ty: actual_ty,
            dependencies,
            ..
        }) = query(name)
        else {
            panic!("expected a value projection for {name}");
        };
        assert_eq!(*actual, value);
        assert_eq!(actual_ty, ty);
        assert_eq!(
            dependencies.as_ref(),
            expected_dependencies(name, target).as_slice()
        );
    };
    assert_direct_value(
        "OUT_SCALAR",
        crate::durable_semantics::DurableConstValue::Integer(7),
        crate::durable_semantics::DurableType::I32,
        direct_target(
            "SCALAR",
            dep.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ValueConst,
        ),
    );
    assert_direct_value(
        "OUT_CALLABLE",
        crate::durable_semantics::DurableConstValue::Function(direct_target(
            "Callable",
            dep.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
        )),
        crate::durable_semantics::DurableType::ComptimeType,
        direct_target(
            "Callable",
            dep.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
        ),
    );
    for (name, kind, member) in [
        (
            "OUT_STRUCT",
            crate::StableDefinitionKind::Struct,
            "StructValue",
        ),
        ("OUT_ENUM", crate::StableDefinitionKind::Enum, "EnumValue"),
    ] {
        let target = direct_target(
            member,
            dep.clone(),
            crate::StableDefinitionNamespace::Type,
            kind,
        );
        assert_direct_value(
            name,
            crate::durable_semantics::DurableConstValue::Type(
                crate::durable_semantics::DurableType::Nominal(target.clone()),
            ),
            crate::durable_semantics::DurableType::ComptimeType,
            target,
        );
    }
    let Value::ConstResolution(ConstResolutionProjection::ModuleBinding { key, target }) =
        query("OUT_NESTED")
    else {
        panic!("expected a nested module binding projection");
    };
    assert_eq!(
        key,
        stable_key(
            root.clone(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::ModuleBinding,
            "OUT_NESTED",
        )
    );
    assert_eq!(target, nested);
    assert!(matches!(
        query("OUT_UNKNOWN"),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::UnknownModuleMember {
                ref module_name,
                ref member_name,
            }
        )) if module_name == "dep.rue" && member_name == "MISSING"
    ));
    assert!(matches!(
        query("OUT_NONMODULE"),
        Value::Failure(Failure::Resolution(message))
            if message.as_ref() == "member access on a non-module const value"
    ));
}

#[test]
fn semantic_nucleus_resolves_exact_signatures_without_whole_module_semantics() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::DurableType as T;
    use crate::semantic_query_nucleus::{
        DeclarationSignatureProjection as Signature, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Node { next: ptr const Node, } fn choose(comptime T: type, value: T) -> T { value }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let configuration = semantic_configuration();

    let node = declaration_candidate(&database, revision, &module, Category::Struct, "Node");
    let node_query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: node,
        configuration: configuration.clone(),
    };
    let identity = request_semantic_nucleus(&database, revision, Key::Identity(node_query.clone()));
    let Value::Identity(identity) = identity else {
        panic!("expected Node identity, got {identity:?}")
    };
    let signature = request_semantic_nucleus(&database, revision, Key::Signature(node_query));
    assert_eq!(
            signature,
            Value::Signature(crate::semantic_query_nucleus::ResolvedDeclarationSignature {
                definition: identity.key.clone(),
                signature: Signature::Struct {
                    fields: vec![(
                        Arc::from("next"),
                        T::PtrConst(Arc::new(T::Nominal(identity.key.clone())))
                    )]
                    .into(),
                    is_copy: false,
                    is_linear: false,
                    is_repr_c: false,
                },
                callable_type_syntax: None,
                anonymous_nominals: Arc::from([]),
                dependencies: vec![
                    crate::semantic_query_nucleus::SemanticDeclarationDependency {
                        source: identity.key.clone(),
                        kind: rue_air::DeclarationTypeDependencyKind::Field,
                        target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                            identity.key,
                        ),
                    },
                ]
                .into(),
                deferred_ownership: Arc::from([]),
            })
        );

    let choose = declaration_candidate(&database, revision, &module, Category::Function, "choose");
    let signature = request_semantic_nucleus(
        &database,
        revision,
        Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: choose,
            configuration,
        }),
    );
    let Value::Signature(crate::semantic_query_nucleus::ResolvedDeclarationSignature {
        signature: Signature::Callable {
            parameters, result, ..
        },
        callable_type_syntax,
        ..
    }) = signature
    else {
        panic!("expected callable signature, got {signature:?}")
    };
    assert_eq!(parameters[0].ty, T::ComptimeType);
    assert_eq!(parameters[1].ty, T::GenericParameter(0));
    assert_eq!(result, T::GenericParameter(0));
    let callable_type_syntax = callable_type_syntax.expect("choose is callable");
    assert_eq!(
        callable_type_syntax
            .parameters
            .iter()
            .map(|root| callable_type_syntax.syntax.render_type(*root).unwrap())
            .collect::<Vec<_>>(),
        ["type", "T"]
    );
    assert_eq!(
        callable_type_syntax
            .syntax
            .render_type(callable_type_syntax.result)
            .unwrap(),
        "T"
    );
}

#[test]
fn nominal_well_formedness_is_a_keyed_query_and_preserves_indirection() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Bad { next: [Bad; 0] } struct Good { next: ptr const Good }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let query = |declaration| crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration,
        configuration: semantic_configuration(),
    };

    let bad = declaration_candidate(&database, revision, &module, Category::Struct, "Bad");
    assert!(matches!(
        request_semantic_nucleus(
            &database,
            revision,
            Key::NominalWellFormedness(query(bad)),
        ),
        Value::Failure(Failure::Diagnostic(
            rue_error::ErrorKind::RecursiveTypeInfiniteSize { ref name, ref cycle }
        )) if name == "Bad" && cycle == "Bad -> Bad"
    ));

    let good = declaration_candidate(&database, revision, &module, Category::Struct, "Good");
    assert_eq!(
        request_semantic_nucleus(&database, revision, Key::NominalWellFormedness(query(good)),),
        Value::NominalWellFormedness,
    );
}

#[test]
fn require_droppable_propagates_signature_cycles_and_accepts_deferred_pointer_graphs() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let cycle_source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Loop(comptime T: type) -> type { @require_droppable(Loop(T)); struct { value: ptr const T } } const X = Loop(i32);",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut cycle_database = RevisionedQueryDatabase::default();
    let cycle_revision = cycle_database.source_revision(
        &super::super::session::ExactSourceInput::new(&cycle_source),
        &cycle_source,
    );
    let alias = declaration_candidate(
        &cycle_database,
        cycle_revision,
        &module,
        Category::ConstCandidate,
        "X",
    );
    let cycle = request_semantic_nucleus(
        &cycle_database,
        cycle_revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: alias,
            configuration: semantic_configuration(),
        }),
    );
    assert!(
        matches!(
            &cycle,
            Value::Failure(Failure::Cycle(nodes))
                if nodes.iter().any(|name| name.as_ref() == "Loop")
        ),
        "unexpected cycle result: {cycle:?}"
    );

    let control_source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Wrap(comptime T: type) -> type { @require_droppable(T); struct { value: ptr const T } } struct Node { next: ptr const Wrap(Node) }",
        )],
        1,
    );
    let mut control_database = RevisionedQueryDatabase::default();
    let control_revision = control_database.source_revision(
        &super::super::session::ExactSourceInput::new(&control_source),
        &control_source,
    );
    let node = declaration_candidate(
        &control_database,
        control_revision,
        &module,
        Category::Struct,
        "Node",
    );
    let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: node,
        configuration: semantic_configuration(),
    };
    let signature = request_semantic_nucleus(
        &control_database,
        control_revision,
        Key::Signature(producer.clone()),
    );
    let Value::Signature(signature) = signature else {
        panic!("expected deferred pointer signature, got {signature:?}")
    };
    let [gate] = signature.deferred_ownership.as_ref() else {
        panic!("expected one deferred ownership gate: {signature:?}")
    };
    assert_eq!(
        request_semantic_nucleus(
            &control_database,
            control_revision,
            Key::DeferredOwnership(crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                producer,
                gate: gate.clone(),
            }),
        ),
        Value::DeferredOwnership,
    );
}

#[test]
fn signature_engine_cycles_publish_family_owned_domain_failures() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::semantic_query_nucleus::{
        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn A(x: B(i32)) -> i32 { 0 } fn B(x: A(i32)) -> i32 { 0 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let declaration = declaration_candidate(&database, revision, &module, Category::Function, "A");
    let result = request_semantic_nucleus(
        &database,
        revision,
        Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        }),
    );
    assert!(
        matches!(
            &result,
            Value::Failure(Failure::SignatureReentry { signature, cycle })
                if signature.name() == "B"
                    && cycle.as_ref() == [Arc::from("A"), Arc::from("B"), Arc::from("A")]
        ),
        "unexpected cycle diagnostic: {result:?}"
    );
}

#[test]
fn semantic_nucleus_evaluates_only_selected_const_dependencies_and_reports_cycles() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::DurableConstValue as Const;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusFailure as Failure,
        SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const base: i32 = 20; const selected: i32 = if true { base + 22 } else { missing }; const left: i32 = right; const right: i32 = left;",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let configuration = semantic_configuration();
    let query = |name: &str| {
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(
                &database,
                revision,
                &module,
                Category::ConstCandidate,
                name,
            ),
            configuration: configuration.clone(),
        })
    };

    let selected = request_semantic_nucleus(&database, revision, query("selected"));
    assert!(matches!(
        selected,
        Value::ConstResolution(Resolution::Value {
            value,
            ..
        }) if matches!(value.as_ref(), Const::Integer(42))
    ));
    let cycle = request_semantic_nucleus(&database, revision, query("left"));
    assert!(
        matches!(cycle, Value::Failure(Failure::Cycle(ref nodes)) if !nodes.is_empty()),
        "expected a domain cycle, got {cycle:?}"
    );
}

#[test]
fn semantic_nucleus_selects_declaration_time_target_branches_from_exact_configuration() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::{DurableConstValue as Const, DurableType as Type};
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "const arch: i32 = match @target_arch() { Arch.X86_64 => 64, Arch.Aarch64 => 32 }; const os: i32 = if @target_os() == Os.Macos { 2 } else { 1 }; const model = match @target_data_model() { DataModel.Ilp32 => i8, DataModel.Lp64 => i64, DataModel.Llp64 => i16 };",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let request = |database: &RevisionedQueryDatabase, target: rue_target::Target, name: &str| {
        let mut configuration = semantic_configuration();
        configuration.target = target;
        request_semantic_nucleus(
            database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    database,
                    revision,
                    &module,
                    Category::ConstCandidate,
                    name,
                ),
                configuration,
            }),
        )
    };

    assert!(matches!(
        request(&database, rue_target::Target::X86_64Linux, "arch"),
        Value::ConstResolution(Resolution::Value {
            value,
            ty: Type::I32,
            ..
        }) if matches!(value.as_ref(), Const::Integer(64))
    ));
    assert!(matches!(
        request(&database, rue_target::Target::Aarch64Macos, "arch"),
        Value::ConstResolution(Resolution::Value {
            value,
            ty: Type::I32,
            ..
        }) if matches!(value.as_ref(), Const::Integer(32))
    ));
    assert!(matches!(
        request(&database, rue_target::Target::Aarch64Macos, "os"),
        Value::ConstResolution(Resolution::Value {
            value,
            ty: Type::I32,
            ..
        }) if matches!(value.as_ref(), Const::Integer(2))
    ));
    assert!(matches!(
        request(&database, rue_target::Target::Aarch64Linux, "model"),
        Value::ConstResolution(Resolution::Value {
            value,
            ty: Type::ComptimeType,
            ..
        }) if matches!(value.as_ref(), Const::Type(Type::I64))
    ));
}

#[test]
fn semantic_nucleus_demand_does_not_touch_unrelated_declarations() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::DurableConstValue as Const;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusKey as Key,
        SemanticNucleusValue as Value,
    };

    let mut text = String::from("const base: i32 = 20; const selected: i32 = base + 22;\n");
    for index in 0..128 {
        text.push_str(&format!("const unrelated{index}: i32 = missing{index};\n"));
    }
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let selected = declaration_candidate(
        &database,
        revision,
        &module,
        Category::ConstCandidate,
        "selected",
    );
    let value = request_semantic_nucleus(
        &database,
        revision,
        Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: selected,
            configuration: semantic_configuration(),
        }),
    );
    assert!(matches!(
        value,
        Value::ConstResolution(Resolution::Value {
            value,
            ..
        }) if matches!(value.as_ref(), Const::Integer(42))
    ));
    assert_eq!(
        database.semantic_nucleus.retention().terminals,
        2,
        "only `selected` and its exact `base` dependency may publish semantic terminals"
    );
}

#[test]
fn semantic_nucleus_lifecycle_distinguishes_terminals_from_control_flow() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use crate::durable_semantics::DurableConstValue as Const;
    use crate::semantic_query_nucleus::{
        ConstResolutionProjection as Resolution, SemanticNucleusFailure as Failure,
        SemanticNucleusKey as Key, SemanticNucleusValue as Value,
    };

    let source_text = (0..=MODULE_QUERY_MEMO_RETENTION)
        .map(|index| format!("const c{index}: i32 = {index};"))
        .chain([
            "const bad: i32 = missing;".to_owned(),
            "const canceled: i32 = 7;".to_owned(),
        ])
        .collect::<Vec<_>>()
        .join("\n");
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", &source_text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database =
        RevisionedQueryDatabase::with_declaration_memo_retention(MODULE_QUERY_MEMO_RETENTION);
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let configuration = semantic_configuration();
    let query = |name: &str| crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: declaration_candidate(
            &database,
            revision,
            &module,
            Category::ConstCandidate,
            name,
        ),
        configuration: configuration.clone(),
    };

    let c0 = Key::ConstResolution(query("c0"));
    let cold = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        c0.clone(),
        CancellationToken::new(),
    );
    assert_eq!(execution(&cold), RequestExecution::Computed);
    let cold_terminal = cold.terminal().unwrap();
    let cold_stamp = cold_terminal.stamp();
    let rue_query::QueryOutcome::Success(cold_value) = cold_terminal.outcome() else {
        unreachable!()
    };
    assert!(matches!(
        cold_value,
        Value::ConstResolution(Resolution::Value {
            value,
            ..
        }) if matches!(value.as_ref(), Const::Integer(0))
    ));

    let warm = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        c0.clone(),
        CancellationToken::new(),
    );
    assert_eq!(execution(&warm), RequestExecution::Reused);
    assert_eq!(warm.terminal().unwrap().stamp(), cold_stamp);
    assert_eq!(warm.terminal().unwrap().outcome(), cold_terminal.outcome());

    let bad = Key::ConstResolution(query("bad"));
    let failed = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        bad.clone(),
        CancellationToken::new(),
    );
    let failed_terminal = failed.terminal().unwrap();
    assert_eq!(failed_terminal.kind(), QueryTerminalKind::Failure);
    assert!(matches!(
        failed_terminal.outcome(),
        rue_query::QueryOutcome::Success(Value::Failure(Failure::Resolution(_)))
    ));
    let failed_again = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        bad,
        CancellationToken::new(),
    );
    assert_eq!(execution(&failed_again), RequestExecution::Reused);
    assert_eq!(
        failed_again.terminal().unwrap().stamp(),
        failed_terminal.stamp(),
        "deterministic semantic failures are reusable terminals"
    );

    let canceled_key = Key::ConstResolution(query("canceled"));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let canceled = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        canceled_key.clone(),
        cancellation,
    );
    assert_eq!(execution(&canceled), RequestExecution::Aborted);
    assert!(canceled.terminal().is_none());
    let after_cancel = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        canceled_key,
        CancellationToken::new(),
    );
    assert_eq!(execution(&after_cancel), RequestExecution::Computed);

    let cycle = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        Key::EngineCycleProbe(query("canceled")),
        CancellationToken::new(),
    );
    assert_eq!(execution(&cycle), RequestExecution::Aborted);
    assert!(matches!(cycle.abort(), Some(QueryAbort::Cycle(_))));
    assert!(cycle.terminal().is_none());

    for index in 1..=MODULE_QUERY_MEMO_RETENTION {
        let requested = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            Key::ConstResolution(query(&format!("c{index}"))),
            CancellationToken::new(),
        );
        assert!(requested.terminal().is_some());
    }
    assert_eq!(
        database.semantic_nucleus.retention().terminals,
        MODULE_QUERY_MEMO_RETENTION
    );
    let after_eviction = database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        c0,
        CancellationToken::new(),
    );
    assert_eq!(execution(&after_eviction), RequestExecution::Computed);
    assert_eq!(
        after_eviction.terminal().unwrap().outcome(),
        cold_terminal.outcome()
    );

    let broken = source_snapshot(
        &[(1, "/main.rue", "main.rue", "const value: i32 = missing;")],
        1,
    );
    let fixed = source_snapshot(&[(1, "/main.rue", "main.rue", "const value: i32 = 42;")], 1);
    let mut recovery = RevisionedQueryDatabase::default();
    let broken_revision = recovery.source_revision(
        &super::super::session::ExactSourceInput::new(&broken),
        &broken,
    );
    let broken_query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: declaration_candidate(
            &recovery,
            broken_revision,
            &module,
            Category::ConstCandidate,
            "value",
        ),
        configuration: configuration.clone(),
    };
    assert!(matches!(
        request_semantic_nucleus(
            &recovery,
            broken_revision,
            Key::ConstResolution(broken_query)
        ),
        Value::Failure(Failure::Resolution(_))
    ));
    let fixed_revision = recovery.source_revision(
        &super::super::session::ExactSourceInput::new(&fixed),
        &fixed,
    );
    let fixed_query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        declaration: declaration_candidate(
            &recovery,
            fixed_revision,
            &module,
            Category::ConstCandidate,
            "value",
        ),
        configuration,
    };
    assert!(matches!(
        request_semantic_nucleus(&recovery, fixed_revision, Key::ConstResolution(fixed_query)),
        Value::ConstResolution(Resolution::Value {
            value,
            ..
        }) if matches!(value.as_ref(), Const::Integer(42))
    ));
}

#[test]
fn declaration_shell_queries_are_keyed_exact_and_payload_stable() {
    let first = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Box { fn get(self) -> i32 { 1 } } const item = 1; fn main() {}",
        )],
        1,
    );
    let edited = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "// shifted file\nstruct Box { fn // comment-only signature trivia\n get(self) -> i32 { 999 } } const item = @import(\"x.rue\"); // shifted again\n fn main() { let x = 2; }",
        )],
        1,
    );
    let main = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let first_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&first),
        &first,
    );
    let indexed = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        first_revision,
        ModuleQueryKey(main.clone()),
        CancellationToken::new(),
    );
    assert_eq!(execution(&indexed), RequestExecution::Computed);
    assert_eq!(indexed.dependencies().len(), 1);
    let terminal = indexed.terminal().unwrap();
    let rue_query::QueryOutcome::Success(indexed_value) = terminal.outcome() else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(indexed_value) = indexed_value else {
        panic!("expected available occurrence index")
    };
    let keys = indexed_value
        .capabilities
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 4);
    let mut shell_stamps = BTreeMap::new();
    for key in &keys {
        let first = database.runtime.request_registered(
            &database.declaration_shells,
            first_revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&first), RequestExecution::Computed);
        shell_stamps.insert(key.stable_identity(), first.terminal().unwrap().stamp());
        assert_eq!(
            first
                .dependencies()
                .iter()
                .map(|dependency| dependency.node.family())
                .collect::<Vec<_>>(),
            vec![
                "compiler.declaration-occurrence-index",
                "compiler.parse-module"
            ]
        );
        let warm = database.runtime.request_registered(
            &database.declaration_shells,
            first_revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&warm), RequestExecution::Reused);
    }

    let edited_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&edited),
        &edited,
    );
    let edited_index = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        edited_revision,
        ModuleQueryKey(main),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(edited_value) = edited_index.terminal().unwrap().outcome()
    else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(edited_value) = edited_value else {
        panic!("expected available edited occurrence index")
    };
    assert_eq!(&indexed_value.capabilities, &edited_value.capabilities);
    for key in &keys {
        let revalidated = database.runtime.request_registered(
            &database.declaration_shells,
            edited_revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        let terminal = revalidated.terminal().unwrap();
        assert_eq!(
            terminal.stamp(),
            shell_stamps[&key.stable_identity()],
            "payload-only edits must preserve the shell publication stamp"
        );
    }
}

#[test]
fn canceled_declaration_shell_request_publishes_no_terminal_and_recovers() {
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
    let main = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let indexed = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        revision,
        ModuleQueryKey(main),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(indexed) = indexed.terminal().unwrap().outcome() else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(indexed) = indexed else {
        panic!("expected available occurrence index")
    };
    let key = indexed.capabilities.keys().next().unwrap().clone();
    let canceled = CancellationToken::new();
    canceled.cancel();
    let aborted = database.runtime.request_registered(
        &database.declaration_shells,
        revision,
        DeclarationShellQueryKey(key.clone()),
        canceled,
    );
    assert_eq!(execution(&aborted), RequestExecution::Aborted);
    assert!(aborted.terminal().is_none());
    let recovered = database.runtime.request_registered(
        &database.declaration_shells,
        revision,
        DeclarationShellQueryKey(key),
        CancellationToken::new(),
    );
    assert_eq!(execution(&recovered), RequestExecution::Computed);
    assert!(recovered.terminal().is_some());
}

#[test]
fn absent_declaration_shell_is_a_typed_position_free_failure_terminal() {
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
        name: Arc::from("missing"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let requested = database.runtime.request_registered(
        &database.declaration_shells,
        revision,
        DeclarationShellQueryKey(key.clone()),
        CancellationToken::new(),
    );
    let terminal = requested.terminal().unwrap();
    assert_eq!(terminal.kind(), QueryTerminalKind::Failure);
    assert!(terminal.diagnostics().is_empty());
    assert!(matches!(
        terminal.outcome(),
        rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Failure(
            crate::declaration_candidate::DeclarationShellFailure::Absent(absent)
        )) if absent == &key
    ));
}

fn project_signature_for_test(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    key: &crate::declaration_candidate::DeclarationCandidateKey,
) -> crate::semantic_query_nucleus::ParsedSemanticSignature {
    let parsed = database.runtime.request_registered(
        &database.parse_modules,
        revision,
        ModuleQueryKey(key.module.clone()),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(parsed) =
        parsed.terminal().expect("parse terminal").outcome()
    else {
        panic!("parse query publishes typed values")
    };
    let module = parsed.result.as_ref().expect("module parses");
    crate::semantic_query_nucleus::project_semantic_signature(module, key)
        .expect("exact signature projects")
}

fn request_signature_for_test(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    declaration: crate::declaration_candidate::DeclarationCandidateKey,
    cancellation: CancellationToken,
) -> QueryRequestAttempt<crate::semantic_query_nucleus::SemanticNucleusValue> {
    database.runtime.request_registered(
        &database.semantic_nucleus,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            },
        ),
        cancellation,
    )
}

#[test]
fn declaration_signature_is_exact_lazy_and_red_green() {
    fn program(selected_type: &str, unrelated_body: u32) -> String {
        let mut source = String::new();
        for index in 0..128 {
            let body = if index == 64 { unrelated_body } else { index };
            source.push_str(&format!("fn unrelated{index}() -> i32 {{ {body} }}\n"));
        }
        source.push_str(&format!(
            "fn selected(value: {selected_type}) -> {selected_type} {{ value }}\n"
        ));
        source
    }

    let first_text = program("i32", 64);
    let unrelated_edit_text = program("i32", 999);
    let selected_edit_text = program("i64", 999);
    let first = source_snapshot(&[(1, "/main.rue", "main.rue", &first_text)], 1);
    let unrelated_edit = source_snapshot(&[(1, "/main.rue", "main.rue", &unrelated_edit_text)], 1);
    let selected_edit = source_snapshot(&[(1, "/main.rue", "main.rue", &selected_edit_text)], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
        name: Arc::from("selected"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();

    let first_revision = revision_for(&mut database, &first);
    let first_request = request_signature_for_test(
        &database,
        first_revision,
        key.clone(),
        CancellationToken::new(),
    );
    assert_eq!(execution(&first_request), RequestExecution::Computed);
    assert!(
        first_request
            .dependencies()
            .iter()
            .any(|dependency| dependency.node.family() == "compiler.parse-module")
    );
    let first_terminal = first_request.terminal().expect("signature terminal");
    let first_stamp = first_terminal.stamp();
    let rue_query::QueryOutcome::Success(
        crate::semantic_query_nucleus::SemanticNucleusValue::Signature(first_signature),
    ) = first_terminal.outcome()
    else {
        panic!("selected signature resolves")
    };
    let syntax = first_signature
        .callable_type_syntax
        .as_ref()
        .expect("callable syntax is retained with the resolved signature");
    assert_eq!(
        syntax.syntax.render_type(syntax.parameters[0]).unwrap(),
        "i32"
    );
    assert_eq!(syntax.syntax.render_type(syntax.result).unwrap(), "i32");

    let warm = request_signature_for_test(
        &database,
        first_revision,
        key.clone(),
        CancellationToken::new(),
    );
    assert_eq!(execution(&warm), RequestExecution::Reused);

    let unrelated_revision = revision_for(&mut database, &unrelated_edit);
    let unrelated_request = request_signature_for_test(
        &database,
        unrelated_revision,
        key.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        unrelated_request
            .terminal()
            .expect("signature terminal")
            .stamp(),
        first_stamp,
        "an unrelated body edit must preserve the authoritative signature stamp"
    );

    let selected_revision = revision_for(&mut database, &selected_edit);
    let selected_request =
        request_signature_for_test(&database, selected_revision, key, CancellationToken::new());
    let selected_terminal = selected_request.terminal().expect("signature terminal");
    assert_ne!(selected_terminal.stamp(), first_stamp);
    let rue_query::QueryOutcome::Success(
        crate::semantic_query_nucleus::SemanticNucleusValue::Signature(selected_signature),
    ) = selected_terminal.outcome()
    else {
        panic!("edited signature resolves")
    };
    let syntax = selected_signature
        .callable_type_syntax
        .as_ref()
        .expect("callable syntax is retained");
    assert_eq!(
        syntax.syntax.render_type(syntax.parameters[0]).unwrap(),
        "i64"
    );
    assert_eq!(syntax.syntax.render_type(syntax.result).unwrap(), "i64");
    assert_eq!(
        database
            .declaration_body_plan_astgen_evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "signature evaluation must not lower a body"
    );
}

#[test]
fn parsed_signature_projection_covers_every_category_and_exact_duplicate() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "@copy linear struct Box { value: i32, fn get(borrow self) -> i32 { self.value } fn make(value: i32) -> Box { Box { value } } }\n\
                 enum Choice { Empty, Value(i32, u64) }\n\
                 drop fn Box(self) {}\n\
                 extern \"C\" { fn foreign(value: ptr const u8) -> i32; }\n\
                 fn duplicate(value: i32) {} fn duplicate(value: i64) {}",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let owner = crate::declaration_candidate::DeclarationCandidateOwner {
        category: Category::Struct,
        name: Arc::from("Box"),
    };
    let key = |category, name: &'static str, owner, duplicate_discriminator| {
        crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category,
            name: Arc::from(name),
            owner,
            duplicate_discriminator,
        }
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

    let structure =
        project_signature_for_test(&database, revision, &key(Category::Struct, "Box", None, 0));
    let crate::semantic_query_nucleus::ParsedSemanticSignature::Struct {
        fields,
        is_copy: true,
        is_linear: true,
        is_repr_c: false,
        ..
    } = &structure
    else {
        panic!("expected compact struct signature, got {structure:?}");
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(structure.symbol(fields[0].name), "value");
    assert_eq!(structure.render_type(fields[0].ty), "i32");

    for (candidate, expected_result, expected_self) in [
        (
            key(Category::Method, "get", Some(owner.clone()), 0),
            "i32",
            true,
        ),
        (
            key(Category::AssociatedFunction, "make", Some(owner.clone()), 0),
            "Box",
            false,
        ),
    ] {
        let signature = project_signature_for_test(&database, revision, &candidate);
        assert!(matches!(
            &signature,
            crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
                result,
                has_self,
                is_extern: false,
                ..
            } if signature.render_type(*result) == expected_result && *has_self == expected_self
        ));
    }

    let enumeration =
        project_signature_for_test(&database, revision, &key(Category::Enum, "Choice", None, 0));
    let crate::semantic_query_nucleus::ParsedSemanticSignature::Enum {
        variants, payloads, ..
    } = &enumeration
    else {
        panic!("expected compact enum signature, got {enumeration:?}");
    };
    assert_eq!(variants.len(), 2);
    assert_eq!(enumeration.symbol(variants[0].name), "Empty");
    assert_eq!(enumeration.symbol(variants[1].name), "Value");
    let payload = &payloads[variants[1].payload_start as usize..variants[1].payload_end as usize];
    assert_eq!(
        payload
            .iter()
            .map(|value| enumeration.render_type(*value))
            .collect::<Vec<_>>(),
        ["i32", "u64"]
    );

    assert!(matches!(
        project_signature_for_test(
            &database,
            revision,
            &key(Category::Destructor, "Box", Some(owner), 0),
        ),
        crate::semantic_query_nucleus::ParsedSemanticSignature::Destructor
    ));
    let foreign = project_signature_for_test(
        &database,
        revision,
        &key(Category::ExternFunction, "foreign", None, 0),
    );
    assert!(matches!(
        &foreign,
        crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
            parameters,
            result,
            is_extern: true,
            ..
        } if parameters.len() == 1
            && foreign.render_type(parameters[0].ty) == "ptr const u8"
            && foreign.render_type(*result) == "i32"
    ));

    for (duplicate_discriminator, expected) in [(0, "i32"), (1, "i64")] {
        let signature = project_signature_for_test(
            &database,
            revision,
            &key(
                Category::Function,
                "duplicate",
                None,
                duplicate_discriminator,
            ),
        );
        assert!(matches!(
            &signature,
            crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
                parameters,
                ..
            } if parameters.len() == 1 && signature.render_type(parameters[0].ty) == expected
        ));
    }
}

#[test]
fn parsed_signature_projection_preserves_every_annotation_type_shape() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use rue_rir::RirTypeSyntaxNode as Node;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn full(\
                    named: i32, \
                    qualified: lib.geo.Point, \
                    unit: (), \
                    never: !, \
                    array_literal: [i32; 4], \
                    array_named: [i32; N], \
                    array_call: [i32; Width(N, 2)], \
                    slice: [i32], \
                    const_pointer: ptr const i32, \
                    mutable_pointer: ptr mut ptr const u8, \
                    type_call: Pair(i32, [i32; 2]), \
                    qualified_call: lib.pair.Pair(i32), \
                    integer_argument: Buffer(-2), \
                 ) -> Str(8) { loop {} }",
        )],
        1,
    );
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module: ModuleId::from_logical_path("main.rue").unwrap(),
        category: Category::Function,
        name: Arc::from("full"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let signature = project_signature_for_test(&database, revision, &key);
    let crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
        syntax,
        parameters,
        result,
        ..
    } = &signature
    else {
        panic!("expected callable signature, got {signature:?}");
    };

    assert_eq!(
        parameters
            .iter()
            .map(|parameter| syntax.render_type(parameter.ty).unwrap())
            .collect::<Vec<_>>(),
        [
            "i32",
            "lib.geo.Point",
            "()",
            "!",
            "[i32; 4]",
            "[i32; N]",
            "[i32; Width(N, 2)]",
            "[i32]",
            "ptr const i32",
            "ptr mut ptr const u8",
            "Pair(i32, [i32; 2])",
            "lib.pair.Pair(i32)",
            "Buffer(-2)",
        ]
    );
    assert_eq!(syntax.render_type(*result).as_deref(), Some("Str(8)"));

    let nodes = syntax.nodes();
    for (name, present) in [
        (
            "named",
            nodes.iter().any(|node| matches!(node, Node::Named(_))),
        ),
        (
            "qualified",
            nodes
                .iter()
                .any(|node| matches!(node, Node::Qualified { .. })),
        ),
        ("unit", nodes.iter().any(|node| matches!(node, Node::Unit))),
        (
            "never",
            nodes.iter().any(|node| matches!(node, Node::Never)),
        ),
        (
            "array",
            nodes.iter().any(|node| matches!(node, Node::Array { .. })),
        ),
        (
            "slice",
            nodes.iter().any(|node| matches!(node, Node::Slice { .. })),
        ),
        (
            "const pointer",
            nodes
                .iter()
                .any(|node| matches!(node, Node::PointerConst { .. })),
        ),
        (
            "mutable pointer",
            nodes
                .iter()
                .any(|node| matches!(node, Node::PointerMut { .. })),
        ),
        (
            "type call",
            nodes
                .iter()
                .any(|node| matches!(node, Node::TypeCall { .. })),
        ),
        (
            "value call",
            nodes
                .iter()
                .any(|node| matches!(node, Node::ValueCall { .. })),
        ),
        (
            "integer argument",
            nodes.iter().any(|node| matches!(node, Node::Integer(_))),
        ),
    ] {
        assert!(present, "signature arena omitted {name}");
    }
    assert_eq!(
        syntax
            .symbols()
            .iter()
            .filter(|symbol| symbol.as_ref() == "i32")
            .count(),
        1,
        "the declaration-local spelling table must deduplicate leaf names"
    );
}

#[test]
fn parsed_signature_projection_excludes_body_peer_and_absolute_trivia() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let first = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn free(value: i32) -> i32 { value }\n\
                 struct Box { value: i32, fn get(borrow self) -> i32 { self.value } }",
        )],
        1,
    );
    let relocated = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "// moved prefix\n\
                 fn free(value: i32) -> i32 // boundary\n\
                     { value + 0 }\n\
                 struct Box { value: i32, fn get(borrow self) -> u64 { 0 } }",
        )],
        1,
    );
    let signature_edit = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn free(value: i64) -> i32 { 0 }\n\
                 struct Box { value: i32, fn get(borrow self) -> u64 { 0 } }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let free = crate::declaration_candidate::DeclarationCandidateKey {
        module: module.clone(),
        category: Category::Function,
        name: Arc::from("free"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let structure = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: Category::Struct,
        name: Arc::from("Box"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();

    let first_revision = revision_for(&mut database, &first);
    let first_free = project_signature_for_test(&database, first_revision, &free);
    let first_structure = project_signature_for_test(&database, first_revision, &structure);

    let relocated_revision = revision_for(&mut database, &relocated);
    assert_eq!(
        project_signature_for_test(&database, relocated_revision, &free),
        first_free,
        "body and absolute-trivia motion must not change a signature"
    );
    assert_eq!(
        project_signature_for_test(&database, relocated_revision, &structure),
        first_structure,
        "a peer method signature must not enter the struct signature"
    );

    let signature_revision = revision_for(&mut database, &signature_edit);
    assert_ne!(
        project_signature_for_test(&database, signature_revision, &free),
        first_free,
        "an exact parameter-type edit must change the signature"
    );
}

#[test]
fn parsed_accessor_signature_uses_exact_owner_facts() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;
    use rue_air::declaration_validation::AccessorBodyVerdict;

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct S { field: i32, fn selected(borrow self) -> borrow i32 { yield self.selected(); } }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: Category::Method,
        name: Arc::from("selected"),
        owner: Some(crate::declaration_candidate::DeclarationCandidateOwner {
            category: Category::Struct,
            name: Arc::from("S"),
        }),
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let signature = project_signature_for_test(&database, revision, &key);
    assert!(matches!(
        signature,
        crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
            is_accessor: true,
            accessor_body: AccessorBodyVerdict::WellFormed,
            accessor_cycle: Some(name),
            ..
        } if name.as_ref() == "selected"
    ));
}

#[test]
fn authoritative_signature_cancellation_publishes_nothing_and_retries() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Category;

    let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn present() {}")], 1);
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let key = crate::declaration_candidate::DeclarationCandidateKey {
        module,
        category: Category::Function,
        name: Arc::from("present"),
        owner: None,
        duplicate_discriminator: 0,
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

    let canceled = CancellationToken::new();
    canceled.cancel();
    let aborted = request_signature_for_test(&database, revision, key.clone(), canceled);
    assert_eq!(execution(&aborted), RequestExecution::Aborted);
    assert!(aborted.terminal().is_none());

    let recovered = request_signature_for_test(&database, revision, key, CancellationToken::new());
    assert_eq!(execution(&recovered), RequestExecution::Computed);
    assert!(matches!(
        recovered.terminal().expect("signature terminal").outcome(),
        rue_query::QueryOutcome::Success(
            crate::semantic_query_nucleus::SemanticNucleusValue::Signature(_)
        )
    ));
}

#[test]
fn declaration_shell_batches_over_64_entries_reuse_without_thrashing() {
    let source_text = (0..129)
        .map(|index| format!("fn f{index}() {{}}"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text.as_str())], 1);
    let main = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
    let indexed = database.runtime.request_registered(
        &database.declaration_occurrence_indexes,
        revision,
        ModuleQueryKey(main),
        CancellationToken::new(),
    );
    let rue_query::QueryOutcome::Success(indexed) = indexed.terminal().unwrap().outcome() else {
        unreachable!()
    };
    let DeclarationOccurrenceIndexValue::Available(indexed) = indexed else {
        panic!("expected available occurrence index")
    };
    let keys = indexed.capabilities.keys().cloned().collect::<Vec<_>>();
    let mut first_stamps = Vec::with_capacity(keys.len());
    for key in &keys {
        let requested = database.runtime.request_registered(
            &database.declaration_shells,
            revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&requested), RequestExecution::Computed);
        first_stamps.push(requested.terminal().unwrap().stamp());
    }
    for (key, first_stamp) in keys.iter().zip(first_stamps) {
        let warm = database.runtime.request_registered(
            &database.declaration_shells,
            revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&warm), RequestExecution::Reused);
        assert_eq!(warm.terminal().unwrap().stamp(), first_stamp);
    }
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
    let first_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&first),
        &first,
    );
    let (first_a_parse, first_a_index) = database.module_terminals(first_revision, a.clone());
    let (first_b_parse, first_b_index) = database.module_terminals(first_revision, b.clone());

    let second_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&second),
        &second,
    );
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
    let first_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&first),
        &first,
    );
    let (first_parse, first_index) = database.module_terminals(first_revision, a.clone());
    let _ = database.module_terminals(first_revision, b.clone());

    let second_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&second),
        &second,
    );
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
    let first_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&first),
        &first,
    );
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

    let second_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&second),
        &second,
    );
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
        database.source_revision(&super::super::session::ExactSourceInput::new(&base), &base);
    let (base_parse, base_index) = database.module_terminals(base_revision, a.clone());
    assert_eq!(database.runtime.metrics().claims, 2);
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
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
        .map(|index| {
            format!(
                "// {}\nfn f{index}() -> i32 {{ {index} }}\n",
                "x".repeat(64_000)
            )
        })
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
    let root = modules[0].clone();
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
    let revision = revision_for(&mut database, &snapshot);
    let (program, work) = database.parse_program(revision, &root, modules);
    assert!(program.is_ok());
    assert_eq!(work.frontier_items, 8);
    assert_eq!(work.frontier_batches, 1);
    assert_eq!(work.frontier_batch_overhead, 1);
    assert_eq!(work.modules_reparsed, 8);
    assert_eq!(work.modules_reused, 0);
    assert!(
        database.runtime_metrics_for_test().peak_query_workers > 1,
        "a cold independent module frontier must occupy multiple workers"
    );
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
    assert_eq!(run(1), run(4));
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
    let mut database = RevisionedQueryDatabase::with_query_concurrency(4);
    let revision = revision_for(&mut database, &snapshot);
    database
        .parse_program(revision, &module, [module.clone()])
        .0
        .unwrap();
    assert_eq!(database.runtime_metrics_for_test().peak_query_workers, 1);
    let (attempt, executions) =
        database.warning_body_reference_frontier(revision, keys.clone(), CancellationToken::new());
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
    assert!(database.runtime_metrics_for_test().peak_query_workers > 1);
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
                    let (next, _) = child_gate
                        .changed
                        .wait_timeout(state, std::time::Duration::from_millis(1))
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state = next;
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
    while state.0 == 0 {
        let now = std::time::Instant::now();
        assert!(
            now < deadline,
            "no warning child reached the in-flight evaluator gate"
        );
        let (next, _) = gate
            .changed
            .wait_timeout(state, deadline.saturating_duration_since(now))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = next;
    }
    drop(state);
    assert!(database.runtime_metrics_for_test().peak_query_workers > 1);
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
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

    let compiler = include_str!("parse_import.rs");
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
    let compiler = include_str!("provider.rs");
    let registry_start = compiler
        .find("struct CanonicalAnonymousNominalRegistry")
        .expect("body provider retains one anonymous registry");
    let registry_end = compiler[registry_start..]
        .find("\n#[derive(Clone)]\npub(crate) struct CompilerBodyDurableSource")
        .map(|offset| registry_start + offset)
        .expect("anonymous registry remains separate from its consumer");
    let registry = &compiler[registry_start..registry_end];
    assert!(registry.contains("with_canonical_producer().into_owned()"));
    assert!(registry.contains("self.by_identity\n            .get("));

    let lookup_start = compiler
        .find("pub(super) fn anonymous_nominal(")
        .expect("body provider retains anonymous lookup");
    let lookup_end = compiler[lookup_start..]
        .find("\n    pub(super) fn signature(")
        .map(|offset| lookup_start + offset)
        .expect("anonymous lookup stays bounded");
    let lookup = &compiler[lookup_start..lookup_end];
    assert!(lookup.contains("dynamic.get(key)"));
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
        Arc::from([]),
        Arc::from([]),
    );

    let mut registry = CanonicalAnonymousNominalRegistry::default();
    registry.extend([&thin]);
    let canonical_thin = registry.get(&canonical).unwrap();
    let wrapped_thin = registry.get(&wrapped).unwrap();
    assert_eq!(canonical_thin.as_ref(), &thin);
    assert!(Rc::ptr_eq(&canonical_thin, &wrapped_thin));
    registry.extend([&rich]);
    registry.extend([&thin]);

    assert_eq!(registry.by_identity.len(), 1);
    let canonical_rich = registry.get(&canonical).unwrap();
    let wrapped_rich = registry.get(&wrapped).unwrap();
    assert_eq!(canonical_rich.as_ref(), &rich);
    assert!(Rc::ptr_eq(&canonical_rich, &wrapped_rich));
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
    let first_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&first),
        &first,
    );
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

    let shifted_revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&shifted),
        &shifted,
    );
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

fn begin_database_plan(
    database: &mut RevisionedQueryDatabase,
    assembler: &mut DiscoverySourceAssembler,
    context: ImportDiscoveryContext,
) -> (
    SourceSnapshot,
    AcceptedReadManifest,
    ImportInputRevision,
    ImportDiscoveryPlan,
) {
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let revision = database
        .begin_import_inputs(&snapshot, context.clone(), reads.clone())
        .unwrap();
    let runtime_revision = Revision::new(revision.revision_id, revision.compatibility_token);
    let root = ModuleId::from_logical_path("main.rue").unwrap();
    let modules = snapshot
        .source_revision()
        .modules()
        .iter()
        .map(|module| module.module.clone())
        .collect::<Vec<_>>();
    let (program, _) = database.parse_program(runtime_revision, &root, modules);
    let plan = ImportDiscoveryPlan::new(&program.unwrap(), context).unwrap();
    (snapshot, reads, revision, plan)
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

fn publish_manifest_observations(
    database: &mut RevisionedQueryDatabase,
    snapshot: &SourceSnapshot,
    reads: AcceptedReadManifest,
    plan: &ImportDiscoveryPlan,
    mut revision: ImportInputRevision,
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
                let Some(entry) = reads
                    .iter()
                    .find(|entry| entry.requested_path() == request.requested_path())
                else {
                    return ImportObservation::absent(request);
                };
                let file_id = snapshot
                    .files()
                    .find(|source| snapshot.module_id(source.file_id) == Some(entry.module()))
                    .unwrap()
                    .file_id;
                let accepted = crate::AcceptedImportSource::new(
                    entry.requested_path(),
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

fn declaration_import_key(
    module: &ModuleId,
    category: crate::declaration_candidate::DeclarationCandidateCategory,
    name: impl Into<Arc<str>>,
    owner: Option<crate::declaration_candidate::DeclarationCandidateOwner>,
    occurrence: u32,
    specifier: &str,
) -> DeclarationImportQueryKey {
    DeclarationImportQueryKey(crate::declaration_candidate::DeclarationImportSiteKey {
        declaration: crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category,
            name: name.into(),
            owner,
            duplicate_discriminator: 0,
        },
        occurrence,
        specifier: Arc::from(specifier),
    })
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
    assert_eq!(
        database
            .module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revisions
            .back()
            .unwrap()
            .snapshot
            .source_revision(),
        snapshot.source_revision()
    );
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

fn revision_for(database: &mut RevisionedQueryDatabase, snapshot: &SourceSnapshot) -> Revision {
    database.source_revision(
        &super::super::session::ExactSourceInput::new(snapshot),
        snapshot,
    )
}

fn named_type_instance(
    module: &ModuleId,
    name: &str,
    kind: crate::StableDefinitionKind,
) -> crate::TypeInstanceKey {
    crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(
        crate::StableDefinitionKey::from_stable_parts(
            module.clone(),
            crate::StableDefinitionNamespace::Type,
            kind,
            name,
            None,
        ),
    ))
}

#[test]
fn durable_nominal_materialization_shares_the_canonical_signature_payload() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Point { x: i32, y: i64 }\nfn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let point = crate::StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Type,
        crate::StableDefinitionKind::Struct,
        Arc::from("Point"),
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "durable-nominal-shared-payload",
        move |provider| {
            let source = CompilerBodyDurableSource::with_anonymous(provider, &[], None);
            let signature = source.signature(&point).expect("Point has a signature");
            let crate::semantic_query_nucleus::DeclarationSignatureProjection::Struct {
                fields: canonical_fields,
                ..
            } = signature.signature
            else {
                panic!("Point has a struct signature")
            };
            let nominal = rue_air::DurableNominalSource::nominal(&source, &point)
                .expect("Point has a durable nominal body");
            let rue_air::DurableNominalBody::Struct {
                fields: materialized_fields,
                ..
            } = nominal.body
            else {
                panic!("Point materializes as a struct")
            };
            Arc::ptr_eq(&canonical_fields, &materialized_fields)
        },
    );
    assert!(
        outcome.result,
        "the durable source must not rebuild an equivalent nominal field vector"
    );
}

#[test]
fn durable_function_materialization_shares_the_canonical_parameter_payload() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn helper(value: i32, count: u64) -> i32 { value }\nfn main() -> i32 { helper(0, 1) }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let helper = crate::StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        Arc::from("helper"),
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "durable-function-shared-parameters",
        move |provider| {
            let source = CompilerBodyDurableSource::with_anonymous(provider, &[], None);
            let signature = source.signature(&helper).expect("helper has a signature");
            let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                parameters: canonical_parameters,
                ..
            } = signature.signature
            else {
                panic!("helper has a callable signature")
            };
            let function = rue_air::DurableCallableSource::function(&source, &helper)
                .expect("helper has a durable function body");
            Arc::ptr_eq(&canonical_parameters, &function.parameters)
        },
    );
    assert!(
        outcome.result,
        "the durable source must not rebuild an equivalent function-parameter vector"
    );
}

#[test]
fn durable_named_member_resolves_each_unique_candidate_with_one_probe() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Counter { value: i32, \
                 fn get(borrow self) -> i32 { self.value } \
                 fn make(value: i32) -> Counter { Counter { value: value } } }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let counter = crate::StableDefinitionKey::from_stable_parts(
        ModuleId::from_logical_path("main.rue").unwrap(),
        crate::StableDefinitionNamespace::Type,
        crate::StableDefinitionKind::Struct,
        Arc::from("Counter"),
        None,
    );
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "durable-named-member-single-probes",
        move |provider| {
            let before = provider
                .meter()
                .method_candidates
                .load(std::sync::atomic::Ordering::Relaxed);
            let source = CompilerBodyDurableSource::with_anonymous(provider, &[], None);
            let get =
                rue_air::DurableBodyLookupSource::named_member(&source, &counter, "Counter", "get");
            let make = rue_air::DurableBodyLookupSource::named_member(
                &source, &counter, "Counter", "make",
            );
            let after = provider
                .meter()
                .method_candidates
                .load(std::sync::atomic::Ordering::Relaxed);
            (get, make, after - before)
        },
    );
    let (get, make, probes) = outcome.result;
    let (get, get_has_self) = get.expect("the sole instance method resolves");
    assert_eq!(get.kind(), crate::StableDefinitionKind::Method);
    assert!(get_has_self);
    let (make, make_has_self) = make.expect("the sole associated function resolves");
    assert_eq!(make.kind(), crate::StableDefinitionKind::AssociatedFunction);
    assert!(!make_has_self);
    assert_eq!(probes, 2, "each member name performs one candidate probe");
}

#[test]
fn durable_named_member_rejects_every_multi_candidate_shape() {
    fn candidate(declaration: u8, has_self_receiver: bool) -> rue_air::MemberCandidate<u8> {
        rue_air::MemberCandidate {
            declaration,
            name: Arc::from("conflict"),
            has_self_receiver,
            kind: if has_self_receiver {
                rue_air::MemberKind::Method
            } else {
                rue_air::MemberKind::AssociatedFunction
            },
            is_public: false,
        }
    }

    let sole_instance = unique_named_member_candidate(vec![candidate(1, true)])
        .expect("one instance method is unique");
    assert!(sole_instance.has_self_receiver);
    let sole_static = unique_named_member_candidate(vec![candidate(2, false)])
        .expect("one associated function is unique");
    assert!(!sole_static.has_self_receiver);
    assert!(
        unique_named_member_candidate::<u8>(Vec::new()).is_none(),
        "an absent member does not resolve"
    );
    assert!(
        unique_named_member_candidate(vec![candidate(3, true), candidate(4, true)]).is_none(),
        "two instance methods are ambiguous"
    );
    assert!(
        unique_named_member_candidate(vec![candidate(5, false), candidate(6, false)]).is_none(),
        "two associated functions are ambiguous"
    );
    assert!(
        unique_named_member_candidate(vec![candidate(7, true), candidate(8, false)]).is_none(),
        "an instance/static pair is ambiguous"
    );
}

fn request_layout(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    ty: crate::TypeInstanceKey,
) -> QueryRequestAttempt<crate::type_queries::LayoutValue> {
    request_layout_for_target(database, revision, ty, crate::Target::X86_64Linux)
}

fn request_layout_for_target(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    ty: crate::TypeInstanceKey,
    target: crate::Target,
) -> QueryRequestAttempt<crate::type_queries::LayoutValue> {
    database.runtime.request_registered(
        &database.layouts,
        revision,
        crate::type_queries::TypeQueryKey {
            ty,
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target,
                preview_features: crate::StablePreviewFeatures::new(
                    &crate::PreviewFeatures::default(),
                ),
            },
        },
        CancellationToken::new(),
    )
}

fn assert_layout_parity(canonical: &crate::type_queries::CanonicalLayout, live: &rue_air::Layout) {
    use crate::type_queries::CanonicalLayoutKind as C;
    use rue_air::LayoutKind as L;
    assert_eq!(
        (canonical.size, canonical.alignment, canonical.stride),
        (live.size, live.alignment, live.stride)
    );
    match (&canonical.kind, &live.kind) {
        (
            C::Array {
                element: Some(canonical_element),
                count: canonical_count,
            },
            L::Array {
                element: live_element,
                count: live_count,
            },
        ) => {
            assert_eq!(canonical_count, live_count);
            assert_layout_parity(canonical_element, live_element);
        }
        (
            C::Struct {
                field_offsets,
                padding_ranges,
            },
            L::Struct {
                field_offsets: live_offsets,
                padding_ranges: live_padding,
            },
        ) => {
            assert_eq!(field_offsets.as_ref(), live_offsets);
            assert_eq!(padding_ranges.as_ref(), live_padding);
        }
        (
            C::Enum {
                tag_size,
                payload_offset,
                variants,
            },
            L::Enum {
                tag,
                payload_offset: live_payload_offset,
                variants: live_variants,
            },
        ) => {
            assert_eq!(*tag_size, tag.size);
            assert_eq!(payload_offset, live_payload_offset);
            assert_eq!(
                variants
                    .iter()
                    .map(|variant| variant.to_vec())
                    .collect::<Vec<_>>(),
                *live_variants
            );
        }
        (C::Scalar, L::Scalar) => {}
        (canonical, live) => panic!("layout kind mismatch: {canonical:?} != {live:?}"),
    }
}

#[test]
fn canonical_layout_matches_frozen_pool_for_padding_nested_arrays_and_enums() {
    use lasso::ThreadedRodeo;
    use rue_air::{EnumDef, StructDef, StructField, Type, TypeInternPool};

    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Padded { first: u8, aligned: u64, tail: u16 }\n\
                 enum Choice { Small(u8, u64), Wide(u32, u16, u64) }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let padded_key = named_type_instance(&module, "Padded", crate::StableDefinitionKind::Struct);
    let choice_key = named_type_instance(&module, "Choice", crate::StableDefinitionKind::Enum);
    let inner_array_key = crate::TypeInstanceKey::Array {
        element: Node::new(padded_key.clone()),
        len: 2,
    };
    let outer_array_key = crate::TypeInstanceKey::Array {
        element: Node::new(inner_array_key),
        len: 3,
    };

    let pool = TypeInternPool::new();
    let interner = ThreadedRodeo::new();
    let padded_id = pool
        .register_struct(
            interner.get_or_intern("Padded"),
            StructDef {
                name: "Padded".into(),
                fields: vec![
                    StructField {
                        name: "first".into(),
                        ty: Type::U8,
                    },
                    StructField {
                        name: "aligned".into(),
                        ty: Type::U64,
                    },
                    StructField {
                        name: "tail".into(),
                        ty: Type::U16,
                    },
                ],
                is_copy: false,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        )
        .0;
    let padded_ty = Type::new_struct(padded_id);
    let inner_array = pool.intern_array_from_type(padded_ty, 2);
    let outer_array = pool.intern_array_from_type(Type::new_array(inner_array), 3);
    let choice_id = pool
        .register_enum(
            interner.get_or_intern("Choice"),
            EnumDef {
                name: "Choice".into(),
                variants: Arc::from(["Small".into(), "Wide".into()]),
                variant_payloads: vec![
                    vec![Type::U8, Type::U64],
                    vec![Type::U32, Type::U16, Type::U64],
                ],
                is_pub: false,
                is_non_exhaustive: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        )
        .0;
    let pool = pool.freeze();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        for (stable, live) in [
            (padded_key.clone(), padded_ty),
            (outer_array_key.clone(), Type::new_array(outer_array)),
            (choice_key.clone(), Type::new_enum(choice_id)),
        ] {
            let attempt = request_layout_for_target(&database, revision, stable, target);
            let terminal = attempt.terminal().unwrap();
            let rue_query::QueryOutcome::Success(crate::type_queries::LayoutValue::Available(
                canonical,
            )) = terminal.outcome()
            else {
                panic!("layout query failed: {terminal:?}");
            };
            assert_layout_parity(canonical, &pool.layout(live));
        }
    }
}

#[test]
fn layout_observes_only_structural_by_value_dependencies() {
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let source = |text| source_snapshot(&[(1, "/main.rue", "main.rue", text)], 1);
    let first = source("struct Foo { value: i64 }");
    let destructor_only = source("struct Foo { value: i64 }\ndrop fn Foo(self) {}");
    let linearity_only = source("linear struct Foo { value: i64 }\ndrop fn Foo(self) {}");
    let shape_edit = source("linear struct Foo { value: i64, extra: i64 }\ndrop fn Foo(self) {}");
    let foo = named_type_instance(&module, "Foo", crate::StableDefinitionKind::Struct);
    let pointer = crate::TypeInstanceKey::PtrConst(Node::new(foo.clone()));
    let slice = crate::TypeInstanceKey::Slice {
        element: Node::new(foo.clone()),
        name: Arc::from("FooSlice"),
    };
    let zero_array = crate::TypeInstanceKey::Array {
        element: Node::new(foo.clone()),
        len: 0,
    };
    let mut database = RevisionedQueryDatabase::default();

    let first_revision = revision_for(&mut database, &first);
    let cold = request_layout(&database, first_revision, foo.clone());
    let cold_stamp = cold.terminal().unwrap().stamp();
    assert_eq!(cold.execution(), RequestExecution::Computed);
    for ty in [pointer.clone(), slice.clone(), zero_array.clone()] {
        assert_eq!(
            request_layout(&database, first_revision, ty).execution(),
            RequestExecution::Computed
        );
    }

    let destructor_revision = revision_for(&mut database, &destructor_only);
    let destructor = request_layout(&database, destructor_revision, foo.clone());
    assert_eq!(destructor.execution(), RequestExecution::Reused);
    assert_eq!(destructor.terminal().unwrap().stamp(), cold_stamp);

    let linear_revision = revision_for(&mut database, &linearity_only);
    let linear = request_layout(&database, linear_revision, foo.clone());
    assert_eq!(linear.execution(), RequestExecution::Reused);
    assert_eq!(linear.terminal().unwrap().stamp(), cold_stamp);

    let shape_revision = revision_for(&mut database, &shape_edit);
    let shape = request_layout(&database, shape_revision, foo);
    assert_eq!(shape.execution(), RequestExecution::Computed);
    assert_ne!(shape.terminal().unwrap().stamp(), cold_stamp);
    for ty in [pointer, slice, zero_array] {
        assert_eq!(
            request_layout(&database, shape_revision, ty).execution(),
            RequestExecution::Reused,
            "non-by-value containment must not observe the element edit"
        );
    }
}

fn request_call_abi(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    callable: crate::FunctionInstanceKey,
    target: crate::Target,
) -> crate::type_queries::CallAbiFacts {
    let attempt = database.runtime.request_registered(
        &database.call_abis,
        revision,
        crate::type_queries::CallAbiQueryKey {
            callable,
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target,
                preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::from(
                    [rue_error::PreviewFeature::CFfi],
                )),
            },
        },
        CancellationToken::new(),
    );
    let terminal = attempt.terminal().unwrap();
    let rue_query::QueryOutcome::Success(crate::type_queries::CallAbiValue::Available(facts)) =
        terminal.outcome()
    else {
        panic!("call ABI query failed: {terminal:?}");
    };
    facts.clone()
}

#[test]
fn call_abi_classifies_native_target_c_named_destructor_and_drop_glue_on_both_targets() {
    use crate::type_queries::{
        CallAbiArgumentClass as A, CallAbiConvention as C, CallAbiReturnClass as R,
    };
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn native(value: [u64; 7]) -> [u64; 7] { value }\n\
                 extern \"C\" { fn foreign(value: u32) -> u32; }\n\
                 pub extern \"C\" fn exported(value: u32) -> u32 { value }\n\
                 pub struct Owner { value: i64 }\n\
                 drop fn Owner(self) {}",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let native = free_function_instance(&module, "native");
    let foreign = free_function_instance(&module, "foreign");
    let exported = free_function_instance(&module, "exported");
    let owner = named_type_instance(&module, "Owner", crate::StableDefinitionKind::Struct);
    let destructor =
        crate::FunctionInstanceKey::Definition(crate::StableDefinitionKey::from_stable_parts(
            module,
            crate::StableDefinitionNamespace::Destructor,
            crate::StableDefinitionKind::Destructor,
            "Owner",
            Some((crate::StableDefinitionKind::Struct, Arc::from("Owner"))),
        ));
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let native = request_call_abi(&database, revision, native.clone(), target);
        assert_eq!(native.convention, C::Native);
        assert!(native.native_symbol.is_some());
        assert_eq!(
            native.return_class,
            if target == crate::Target::X86_64Linux {
                R::NativeIndirect { slots: 7 }
            } else {
                R::NativeRegisters { slots: 7 }
            }
        );

        let foreign = request_call_abi(&database, revision, foreign.clone(), target);
        assert_eq!(
            foreign.convention,
            C::TargetC(if target == crate::Target::X86_64Linux {
                rue_air::TargetCAbiFlavor::SysVAmd64
            } else {
                rue_air::TargetCAbiFlavor::Aapcs64
            })
        );
        assert!(foreign.native_symbol.is_none());
        assert_eq!(
            foreign.return_class,
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::Unsigned { from_bits: 32 }
            }
        );
        assert!(matches!(
            foreign.arguments[0].class,
            A::CScalar {
                extension: rue_air::ScalarAbiExtension::Unsigned { from_bits: 32 }
            }
        ));

        let exported = request_call_abi(&database, revision, exported.clone(), target);
        assert_eq!(exported.convention, C::Native);
        assert_eq!(
            exported.return_class,
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::None
            }
        );

        let destructor = request_call_abi(&database, revision, destructor.clone(), target);
        assert_eq!(destructor.convention, C::Native);
        assert_eq!(destructor.arguments.len(), 1);
        assert!(matches!(
            destructor.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));

        let glue = request_call_abi(
            &database,
            revision,
            crate::FunctionInstanceKey::DropGlue(Node::new(owner.clone())),
            target,
        );
        assert_eq!(glue.convention, C::Native);
        assert_eq!(glue.return_class, R::ZeroSized);
        assert!(matches!(
            glue.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));
    }
}

#[test]
fn call_abi_batches_layouts_across_mixed_modes_and_duplicate_parameter_types() {
    use crate::type_queries::{
        CallAbiArgumentClass as A, CallAbiConvention as C, CallAbiReturnClass as R,
    };
    // `mixed` interleaves reference and by-value parameters and repeats
    // `[u64; 7]`, so the batch is sparser than the parameter list and
    // carries a duplicate key. `scalars` repeats `u32` under Target-C.
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Pair { a: i64, b: i64 }\n\
                 fn mixed(borrow left: Pair, first: [u64; 7], inout right: Pair, \
                 second: [u64; 7], tail: i32) -> [u64; 7] { second }\n\
                 extern \"C\" { fn scalars(first: u32, second: u32, third: u64) -> u32; }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let mixed = free_function_instance(&module, "mixed");
    let scalars = free_function_instance(&module, "scalars");

    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let mixed = request_call_abi(&database, revision, mixed.clone(), target);
        assert_eq!(mixed.convention, C::Native);
        assert_eq!(mixed.arguments.len(), 5);

        // Reference parameters stay layout-free and keep one value slot,
        // and the by-value parameters keep their signature positions even
        // though only they contributed a batch key.
        for index in [0, 2] {
            assert_eq!(mixed.arguments[index].class, A::Reference);
            assert_eq!(mixed.arguments[index].value_slots, 1);
        }
        // The duplicated `[u64; 7]` classifies identically at both
        // positions: one repeated key, one answer.
        assert_eq!(mixed.arguments[1].class, mixed.arguments[3].class);
        assert_eq!(mixed.arguments[1].value_slots, 7);
        assert_eq!(mixed.arguments[3].value_slots, 7);
        assert!(matches!(
            mixed.arguments[1].class,
            A::NativeDirect { slots: 7 } | A::NativeIndirect
        ));
        assert_eq!(mixed.arguments[4].class, A::NativeDirect { slots: 1 });
        // The result layout is the last entry of the same batch.
        assert_eq!(
            mixed.return_class,
            if target == crate::Target::X86_64Linux {
                R::NativeIndirect { slots: 7 }
            } else {
                R::NativeRegisters { slots: 7 }
            }
        );

        let scalars = request_call_abi(&database, revision, scalars.clone(), target);
        assert_eq!(
            scalars.convention,
            C::TargetC(if target == crate::Target::X86_64Linux {
                rue_air::TargetCAbiFlavor::SysVAmd64
            } else {
                rue_air::TargetCAbiFlavor::Aapcs64
            })
        );
        assert_eq!(scalars.arguments.len(), 3);
        assert_eq!(scalars.arguments[0].class, scalars.arguments[1].class);
        assert_eq!(
            scalars.arguments[0].class,
            A::CScalar {
                extension: rue_air::ScalarAbiExtension::Unsigned { from_bits: 32 }
            }
        );
        assert_eq!(
            scalars.arguments[2].class,
            A::CScalar {
                extension: rue_air::ScalarAbiExtension::None
            }
        );
        assert_eq!(
            scalars.return_class,
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::Unsigned { from_bits: 32 }
            }
        );
    }
}

#[test]
fn call_abi_resolves_value_specialized_array_layout_on_both_targets() {
    use crate::type_queries::{
        CallAbiArgumentClass as A, CallAbiConvention as C, CallAbiReturnClass as R,
    };
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn named(comptime N: i32, value: u64) -> u64 { value + N }\n\
                 fn sized(comptime N: i32, value: [u64; N]) -> [u64; N] { value }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let named = free_function_instance(&module, "named");
    let callable = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "sized")),
        arguments: crate::CanonicalArguments {
            types: Arc::from([]),
            values: Arc::from([crate::CanonicalArgumentValue::Integer(7)]),
        },
    };
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let named = request_call_abi(&database, revision, named.clone(), target);
        assert_eq!(named.convention, C::Native);
        assert_eq!(
            named.return_class,
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::None
            }
        );
        assert_eq!(named.arguments.len(), 2);
        assert!(
            named
                .arguments
                .iter()
                .all(|argument| matches!(argument.class, A::NativeDirect { slots: 1 }))
        );

        let facts = request_call_abi(&database, revision, callable.clone(), target);
        assert_eq!(facts.convention, C::Native);
        assert_eq!(
            facts.return_class,
            if target == crate::Target::X86_64Linux {
                R::NativeIndirect { slots: 7 }
            } else {
                R::NativeRegisters { slots: 7 }
            }
        );
        assert_eq!(facts.arguments.len(), 2);
        assert!(matches!(
            facts.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));
        assert_eq!(facts.arguments[0].value_slots, 1);
        assert!(matches!(
            facts.arguments[1].class,
            A::NativeDirect { slots: 7 }
        ));
        assert_eq!(facts.arguments[1].value_slots, 7);
    }
}

#[test]
fn call_abi_derives_anonymous_destructor_signature_from_its_exact_producer() {
    use crate::type_queries::{
        CallAbiArgumentClass as A, CallAbiConvention as C, CallAbiReturnClass as R,
    };
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Box() -> type { struct { value: i64, drop fn(self) {} } }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let producer = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Box")),
        arguments: crate::CanonicalArguments::default(),
    };
    let configuration = semantic_configuration();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    let produced = database.runtime.request_registered(
        &database.body_produced_anonymous,
        revision,
        crate::body_query::BodyQueryKey::new(producer, configuration),
        CancellationToken::new(),
    );
    let terminal = produced.terminal().unwrap();
    let rue_query::QueryOutcome::Success(crate::body_query::ProducedAnonymous::Produced(produced)) =
        terminal.outcome()
    else {
        panic!("anonymous producer failed: {terminal:?}");
    };
    let owner = crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(Node::new(
        produced.0[0].identity.clone(),
    )));
    let callable = crate::FunctionInstanceKey::AnonymousMember {
        owner: Node::new(owner),
        member: crate::AnonymousMemberKey {
            kind: crate::AnonymousMemberKind::Destructor,
            name: Arc::from("__drop"),
        },
    };
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let facts = request_call_abi(&database, revision, callable.clone(), target);
        assert_eq!(facts.convention, C::Native);
        assert_eq!(facts.return_class, R::ZeroSized);
        assert_eq!(facts.arguments.len(), 1);
        assert!(matches!(
            facts.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));
    }
}

/// One stable argument classification against the live classifier's answer
/// for the same type under the same convention.
fn assert_native_arg_parity(
    stable: &crate::type_queries::CallAbiArgument,
    live_class: rue_air::ArgClass,
    live_width: u32,
    context: &str,
) {
    use crate::type_queries::CallAbiArgumentClass as A;
    assert_eq!(
        stable.value_slots, live_width,
        "value-slot width parity for {context}"
    );
    match (stable.class, live_class) {
        (A::Omitted, rue_air::ArgClass::Omitted) => {}
        (A::NativeDirect { slots }, rue_air::ArgClass::Direct { slot_count }) => {
            assert_eq!(slots, slot_count, "direct slot parity for {context}");
        }
        (A::NativeIndirect, rue_air::ArgClass::Indirect) => {}
        (A::Reference, rue_air::ArgClass::Indirect) => {}
        (stable, live) => {
            panic!("argument classification parity mismatch for {context}: {stable:?} != {live:?}")
        }
    }
}

/// One stable return classification against the live classifier's answer.
fn assert_native_return_parity(
    stable: crate::type_queries::CallAbiReturnClass,
    live: rue_air::ReturnClass,
    context: &str,
) {
    use crate::type_queries::CallAbiReturnClass as R;
    match (stable, live) {
        (R::ZeroSized, rue_air::ReturnClass::ZeroSized) => {}
        (
            R::Scalar {
                extension: rue_air::ScalarAbiExtension::None,
            },
            rue_air::ReturnClass::Scalar,
        ) => {}
        (R::NativeRegisters { slots }, rue_air::ReturnClass::Registers { slot_count }) => {
            assert_eq!(slots, slot_count, "register slot parity for {context}");
        }
        (R::NativeIndirect { slots }, rue_air::ReturnClass::Indirect { slot_count }) => {
            assert_eq!(slots, slot_count, "indirect slot parity for {context}");
        }
        (stable, live) => {
            panic!("return classification parity mismatch for {context}: {stable:?} != {live:?}")
        }
    }
}

#[test]
fn call_abi_native_classification_matches_the_live_classifier_on_both_targets() {
    use lasso::ThreadedRodeo;
    use rue_air::{
        ArgConvention, EnumDef, NativeCallAbi, StructDef, StructField, Type, TypeInternPool,
    };
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Empty {}\n\
                 struct Wide { a: u64, b: u64 }\n\
                 struct Narrow { a: u32, b: u32 }\n\
                 struct OneNarrow { a: u32 }\n\
                 struct Nested { inner: Wide, tail: [u64; 2] }\n\
                 struct NestedNarrow { inner: Narrow, tail: u64 }\n\
                 enum Flag { A, B }\n\
                 enum Choice { Small(u8, u64), Wide(u32, u16, u64) }\n\
                 fn scalars(a: i32, b: u32, c: bool, d: i64) -> i64 { d }\n\
                 fn zero(e: Empty) -> Empty { e }\n\
                 fn refs(inout a: i64, borrow b: Wide) {}\n\
                 fn five(v: [u64; 5]) -> [u64; 5] { v }\n\
                 fn six(v: [u64; 6]) -> [u64; 6] { v }\n\
                 fn seven(v: [u64; 7]) -> [u64; 7] { v }\n\
                 fn eight(v: [u64; 8]) -> [u64; 8] { v }\n\
                 fn nine(v: [u64; 9]) -> [u64; 9] { v }\n\
                 fn wide(v: Wide) -> Wide { v }\n\
                 fn narrow(v: Narrow) -> Narrow { v }\n\
                 fn one_narrow(v: OneNarrow) -> OneNarrow { v }\n\
                 fn nested(v: Nested) -> Nested { v }\n\
                 fn nested_narrow(v: NestedNarrow) -> NestedNarrow { v }\n\
                 fn flag(v: Flag) -> Flag { v }\n\
                 fn choice(v: Choice) -> Choice { v }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();

    // Mirror the source types into a live pool so the stable query can be
    // compared against the live classifier's answer for the same shapes.
    let pool = TypeInternPool::new();
    let interner = ThreadedRodeo::new();
    let struct_def = |name: &str, fields: Vec<(&str, Type)>| StructDef {
        name: name.into(),
        fields: fields
            .into_iter()
            .map(|(name, ty)| StructField {
                name: name.into(),
                ty,
            })
            .collect(),
        is_copy: false,
        is_linear: false,
        declared_linear: false,
        destructor: None,
        is_builtin: false,
        is_pub: false,
        file_id: rue_span::FileId::DEFAULT,
    };
    let register = |name: &str, fields: Vec<(&str, Type)>| {
        Type::new_struct(
            pool.register_struct(interner.get_or_intern(name), struct_def(name, fields))
                .0,
        )
    };
    let empty = register("Empty", vec![]);
    let wide = register("Wide", vec![("a", Type::U64), ("b", Type::U64)]);
    let narrow = register("Narrow", vec![("a", Type::U32), ("b", Type::U32)]);
    let one_narrow = register("OneNarrow", vec![("a", Type::U32)]);
    let tail = Type::new_array(pool.intern_array_from_type(Type::U64, 2));
    let nested = register("Nested", vec![("inner", wide), ("tail", tail)]);
    let nested_narrow = register("NestedNarrow", vec![("inner", narrow), ("tail", Type::U64)]);
    let flag = Type::new_enum(
        pool.register_enum(
            interner.get_or_intern("Flag"),
            EnumDef {
                name: "Flag".into(),
                variants: Arc::from(["A".into(), "B".into()]),
                variant_payloads: vec![vec![], vec![]],
                is_pub: false,
                is_non_exhaustive: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        )
        .0,
    );
    let choice = Type::new_enum(
        pool.register_enum(
            interner.get_or_intern("Choice"),
            EnumDef {
                name: "Choice".into(),
                variants: Arc::from(["Small".into(), "Wide".into()]),
                variant_payloads: vec![
                    vec![Type::U8, Type::U64],
                    vec![Type::U32, Type::U16, Type::U64],
                ],
                is_pub: false,
                is_non_exhaustive: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        )
        .0,
    );
    let arrays: Vec<Type> = (5u64..=9)
        .map(|len| Type::new_array(pool.intern_array_from_type(Type::U64, len)))
        .collect();
    let pool = pool.freeze();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    for (target, budget) in [
        (crate::Target::X86_64Linux, 6u32),
        (crate::Target::Aarch64Linux, 8u32),
    ] {
        let live = NativeCallAbi::new(&pool, budget);
        let by_value = ArgConvention::ByValue;
        let cases: Vec<(&str, Vec<(ArgConvention, Type)>, Type)> = vec![
            (
                "scalars",
                vec![
                    (by_value, Type::I32),
                    (by_value, Type::U32),
                    (by_value, Type::BOOL),
                    (by_value, Type::I64),
                ],
                Type::I64,
            ),
            ("zero", vec![(by_value, empty)], empty),
            (
                "refs",
                vec![
                    (ArgConvention::ByReference, Type::I64),
                    (ArgConvention::ByReference, wide),
                ],
                Type::UNIT,
            ),
            ("five", vec![(by_value, arrays[0])], arrays[0]),
            ("six", vec![(by_value, arrays[1])], arrays[1]),
            ("seven", vec![(by_value, arrays[2])], arrays[2]),
            ("eight", vec![(by_value, arrays[3])], arrays[3]),
            ("nine", vec![(by_value, arrays[4])], arrays[4]),
            ("wide", vec![(by_value, wide)], wide),
            ("narrow", vec![(by_value, narrow)], narrow),
            ("one_narrow", vec![(by_value, one_narrow)], one_narrow),
            ("nested", vec![(by_value, nested)], nested),
            (
                "nested_narrow",
                vec![(by_value, nested_narrow)],
                nested_narrow,
            ),
            ("choice", vec![(by_value, choice)], choice),
        ];
        for (name, params, result) in &cases {
            let facts = request_call_abi(
                &database,
                revision,
                free_function_instance(&module, name),
                target,
            );
            assert_eq!(
                facts.convention,
                crate::type_queries::CallAbiConvention::Native,
                "{name} is a native callable"
            );
            assert_eq!(facts.arguments.len(), params.len(), "arity of {name}");
            for (argument, (convention, ty)) in facts.arguments.iter().zip(params) {
                assert_native_arg_parity(
                    argument,
                    live.classify_arg(*ty, *convention),
                    live.arg_slot_width(*ty, *convention),
                    &format!("{name} on {target:?}"),
                );
            }
            assert_native_return_parity(
                facts.return_class,
                live.classify_return(*result),
                &format!("{name} return on {target:?}"),
            );
        }

        // The discriminant-only enum is the one deliberate projection
        // divergence between the planes: the live classifier reports its
        // single tag slot as `Scalar`, while the stable projection keeps
        // reporting the aggregate as one register slot. The physical
        // crossing is identical (one register); this pin keeps the
        // divergence visible instead of letting it drift silently.
        let flag_facts = request_call_abi(
            &database,
            revision,
            free_function_instance(&module, "flag"),
            target,
        );
        assert_eq!(
            live.classify_return(flag),
            rue_air::ReturnClass::Scalar,
            "live plane reports a discriminant-only enum return as a scalar"
        );
        assert_eq!(
            flag_facts.return_class,
            crate::type_queries::CallAbiReturnClass::NativeRegisters { slots: 1 },
            "stable plane projects a discriminant-only enum return as one register slot"
        );
        assert_native_arg_parity(
            &flag_facts.arguments[0],
            live.classify_arg(flag, ArgConvention::ByValue),
            live.arg_slot_width(flag, ArgConvention::ByValue),
            &format!("flag argument on {target:?}"),
        );

        // Pin the classification outcomes themselves, not only the
        // cross-plane agreement: zero-sized values vanish, a slot-identical
        // aggregate stays direct, a multi-slot narrow-leaf aggregate is
        // forced indirect by the compact memory-first rule, and a
        // single-slot narrow aggregate stays direct (RUE-1035).
        use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
        let request = |name: &str| {
            request_call_abi(
                &database,
                revision,
                free_function_instance(&module, name),
                target,
            )
        };
        let zero = request("zero");
        assert!(matches!(zero.arguments[0].class, A::Omitted));
        assert_eq!(zero.return_class, R::ZeroSized);
        let wide_facts = request("wide");
        assert!(matches!(
            wide_facts.arguments[0].class,
            A::NativeDirect { slots: 2 }
        ));
        assert_eq!(wide_facts.return_class, R::NativeRegisters { slots: 2 });
        let narrow_facts = request("narrow");
        assert!(matches!(narrow_facts.arguments[0].class, A::NativeIndirect));
        assert_eq!(narrow_facts.return_class, R::NativeIndirect { slots: 2 });
        let one_narrow_facts = request("one_narrow");
        assert!(matches!(
            one_narrow_facts.arguments[0].class,
            A::NativeDirect { slots: 1 }
        ));
        assert_eq!(
            one_narrow_facts.return_class,
            R::NativeRegisters { slots: 1 }
        );
        let nested_narrow_facts = request("nested_narrow");
        assert!(matches!(
            nested_narrow_facts.arguments[0].class,
            A::NativeIndirect
        ));
        let refs_facts = request("refs");
        assert!(matches!(refs_facts.arguments[0].class, A::Reference));
        assert!(matches!(refs_facts.arguments[1].class, A::Reference));

        // Pin the return-register budget boundary explicitly: budget - 1
        // and budget fit in registers, budget + 1 goes indirect.
        let boundary = |name: &str| request(name).return_class;
        match target {
            crate::Target::X86_64Linux => {
                assert_eq!(boundary("five"), R::NativeRegisters { slots: 5 });
                assert_eq!(boundary("six"), R::NativeRegisters { slots: 6 });
                assert_eq!(boundary("seven"), R::NativeIndirect { slots: 7 });
            }
            _ => {
                assert_eq!(boundary("seven"), R::NativeRegisters { slots: 7 });
                assert_eq!(boundary("eight"), R::NativeRegisters { slots: 8 });
                assert_eq!(boundary("nine"), R::NativeIndirect { slots: 9 });
            }
        }
    }
}

#[test]
fn call_abi_target_c_classification_matches_the_live_classifier_on_both_targets() {
    use lasso::ThreadedRodeo;
    use rue_air::{StructDef, StructField, TargetCCallAbi, Type, TypeInternPool};
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "@repr(c)\n\
                 struct CInner { a: i32, b: i32 }\n\
                 @repr(c)\n\
                 struct CTwelve { a: i32, b: i32, c: i32 }\n\
                 @repr(c)\n\
                 struct CNested { inner: CInner, tail: i64 }\n\
                 @repr(c)\n\
                 struct CLarge { a: i64, b: i64, c: i64 }\n\
                 extern \"C\" {\n\
                     fn c_signed(a: i8, b: i16, c: i32, d: i64) -> i16;\n\
                     fn c_unsigned(a: u8, b: u16, c: u32, d: u64, e: bool) -> u16;\n\
                     fn c_pointers(p: ptr const u8, q: ptr mut u8) -> ptr mut u8;\n\
                     fn c_eight(v: CInner) -> CInner;\n\
                     fn c_twelve(v: CTwelve) -> CTwelve;\n\
                     fn c_sixteen(v: CNested) -> CNested;\n\
                     fn c_large(v: CLarge) -> CLarge;\n\
                 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();

    let pool = TypeInternPool::new();
    let interner = ThreadedRodeo::new();
    let register = |name: &str, fields: Vec<(&str, Type)>| {
        Type::new_struct(
            pool.register_struct(
                interner.get_or_intern(name),
                StructDef {
                    name: name.into(),
                    fields: fields
                        .into_iter()
                        .map(|(name, ty)| StructField {
                            name: name.into(),
                            ty,
                        })
                        .collect(),
                    is_copy: false,
                    is_linear: false,
                    declared_linear: false,
                    destructor: None,
                    is_builtin: false,
                    is_pub: false,
                    file_id: rue_span::FileId::DEFAULT,
                },
            )
            .0,
        )
    };
    let c_inner = register("CInner", vec![("a", Type::I32), ("b", Type::I32)]);
    let c_twelve = register(
        "CTwelve",
        vec![("a", Type::I32), ("b", Type::I32), ("c", Type::I32)],
    );
    let c_nested = register("CNested", vec![("inner", c_inner), ("tail", Type::I64)]);
    let c_large = register(
        "CLarge",
        vec![("a", Type::I64), ("b", Type::I64), ("c", Type::I64)],
    );
    let ptr_const_u8 = Type::new_ptr_const(pool.intern_ptr_const_from_type(Type::U8));
    let ptr_mut_u8 = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
    let pool = pool.freeze();

    let assert_scalar_args = |facts: &crate::type_queries::CallAbiFacts,
                              abi: &TargetCCallAbi,
                              live_params: &[Type],
                              live_result: Type,
                              name: &str| {
        use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
        assert_eq!(facts.arguments.len(), live_params.len(), "arity of {name}");
        for (argument, live_ty) in facts.arguments.iter().zip(live_params) {
            let A::CScalar { extension } = argument.class else {
                panic!("{name} argument is a target-C scalar: {:?}", argument.class);
            };
            assert_eq!(
                extension,
                abi.scalar_arg_extension(*live_ty),
                "argument extension parity for {name}"
            );
        }
        let R::Scalar { extension } = facts.return_class else {
            panic!(
                "{name} return is a target-C scalar: {:?}",
                facts.return_class
            );
        };
        assert_eq!(
            extension,
            abi.scalar_return_extension(live_result),
            "return extension parity for {name}"
        );
    };
    let assert_aggregate = |facts: &crate::type_queries::CallAbiFacts,
                            abi: &TargetCCallAbi,
                            live_ty: Type,
                            name: &str| {
        use crate::type_queries::{CallAbiArgumentClass as A, CallAbiReturnClass as R};
        let layout = pool.layout(live_ty);
        match (
            facts.arguments[0].class,
            abi.classify_aggregate_arg(layout.size, layout.alignment),
        ) {
            (
                A::CIntegerRegisters { eightbytes },
                rue_air::AggregateArgClass::IntegerRegisters {
                    eightbytes: live_eightbytes,
                },
            ) => assert_eq!(eightbytes, live_eightbytes, "eightbyte parity for {name}"),
            (
                A::CByValueStack { size, alignment },
                rue_air::AggregateArgClass::ByValueStack {
                    size: live_size,
                    align: live_align,
                },
            ) => assert_eq!(
                (size, alignment),
                (live_size, live_align),
                "byval parity for {name}"
            ),
            (
                A::CByReferenceCopy { size, alignment },
                rue_air::AggregateArgClass::ByReferenceCopy {
                    size: live_size,
                    align: live_align,
                },
            ) => assert_eq!(
                (size, alignment),
                (live_size, live_align),
                "reference-copy parity for {name}"
            ),
            (stable, live) => {
                panic!("aggregate argument parity mismatch for {name}: {stable:?} != {live:?}")
            }
        }
        match (
            facts.return_class,
            abi.classify_aggregate_return(layout.size, layout.alignment),
        ) {
            (
                R::CIntegerRegisters { eightbytes },
                rue_air::AggregateReturnClass::IntegerRegisters {
                    eightbytes: live_eightbytes,
                },
            ) => assert_eq!(
                eightbytes, live_eightbytes,
                "return eightbyte parity for {name}"
            ),
            (
                R::CIndirect { size, alignment },
                rue_air::AggregateReturnClass::Indirect {
                    size: live_size,
                    align: live_align,
                },
            ) => assert_eq!(
                (size, alignment),
                (live_size, live_align),
                "sret parity for {name}"
            ),
            (stable, live) => {
                panic!("aggregate return parity mismatch for {name}: {stable:?} != {live:?}")
            }
        }
    };

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);
    for target in [crate::Target::X86_64Linux, crate::Target::Aarch64Linux] {
        let flavor = if target == crate::Target::X86_64Linux {
            rue_air::TargetCAbiFlavor::SysVAmd64
        } else {
            rue_air::TargetCAbiFlavor::Aapcs64
        };
        let abi = TargetCCallAbi::new(flavor);
        let request = |name: &str| {
            request_call_abi(
                &database,
                revision,
                free_function_instance(&module, name),
                target,
            )
        };

        let signed = request("c_signed");
        assert_eq!(
            signed.convention,
            crate::type_queries::CallAbiConvention::TargetC(flavor)
        );
        assert_scalar_args(
            &signed,
            &abi,
            &[Type::I8, Type::I16, Type::I32, Type::I64],
            Type::I16,
            "c_signed",
        );
        assert_scalar_args(
            &request("c_unsigned"),
            &abi,
            &[Type::U8, Type::U16, Type::U32, Type::U64, Type::BOOL],
            Type::U16,
            "c_unsigned",
        );
        assert_scalar_args(
            &request("c_pointers"),
            &abi,
            &[ptr_const_u8, ptr_mut_u8],
            ptr_mut_u8,
            "c_pointers",
        );

        // Aggregates at 8, 12, 16, and 24 bytes: one eightbyte, rounding
        // up to two, exactly two, and past the 16-byte register limit
        // where the psABIs diverge (SysV byval stack, AAPCS64 reference
        // to a caller copy; sret for returns on both).
        assert_aggregate(&request("c_eight"), &abi, c_inner, "c_eight");
        assert_aggregate(&request("c_twelve"), &abi, c_twelve, "c_twelve");
        assert_aggregate(&request("c_sixteen"), &abi, c_nested, "c_sixteen");
        assert_aggregate(&request("c_large"), &abi, c_large, "c_large");
    }
}

#[test]
fn call_abi_strbuf_return_uses_sret_on_both_planes() {
    use lasso::ThreadedRodeo;
    use rue_air::{ArgConvention, NativeCallAbi, StructDef, StructField, Type, TypeInternPool};
    let snapshot = trusted_body_snapshot(
        "fn main() -> i32 { 0 }",
        None,
        Some((
            FileId::new(3),
            "pub struct StrBuf { buf: ptr mut u8, cap: u64, len: u64 }\n\
                 pub fn echo(v: StrBuf) -> StrBuf { v }",
        )),
    );
    let strbuf_module =
        ModuleId::from_trusted_standard_library_path(crate::STRBUF_MODULE_LOGICAL_PATH)
            .expect("the strbuf module path is inside the standard-library namespace");

    let pool = TypeInternPool::new();
    let interner = ThreadedRodeo::new();
    let ptr_mut_u8 = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
    let (strbuf_id, _) = pool.register_struct(
        interner.get_or_intern("StrBuf"),
        StructDef {
            name: "StrBuf".into(),
            fields: vec![
                StructField {
                    name: "buf".into(),
                    ty: ptr_mut_u8,
                },
                StructField {
                    name: "cap".into(),
                    ty: Type::U64,
                },
                StructField {
                    name: "len".into(),
                    ty: Type::U64,
                },
            ],
            is_copy: false,
            is_linear: false,
            declared_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: true,
            file_id: rue_span::FileId::DEFAULT,
        },
    );
    pool.set_struct_lang_item(strbuf_id, rue_air::LangItem::StrBuf);
    let strbuf = Type::new_struct(strbuf_id);
    let pool = pool.freeze();

    let mut database = RevisionedQueryDatabase::default();
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
    for (target, budget) in [
        (crate::Target::X86_64Linux, 6u32),
        (crate::Target::Aarch64Linux, 8u32),
    ] {
        let live = NativeCallAbi::new(&pool, budget);
        // The canonical StrBuf always returns through sret even though its
        // three slots fit the return-register budget, and its slot-identical
        // layout keeps by-value arguments direct.
        assert_eq!(
            live.classify_return(strbuf),
            rue_air::ReturnClass::Indirect { slot_count: 3 }
        );
        assert_eq!(
            live.classify_arg(strbuf, ArgConvention::ByValue),
            rue_air::ArgClass::Direct { slot_count: 3 }
        );
        let facts = request_call_abi(
            &database,
            revision,
            free_function_instance(&strbuf_module, "echo"),
            target,
        );
        assert_eq!(
            facts.convention,
            crate::type_queries::CallAbiConvention::Native
        );
        assert_eq!(
            facts.return_class,
            crate::type_queries::CallAbiReturnClass::NativeIndirect { slots: 3 }
        );
        assert_native_arg_parity(
            &facts.arguments[0],
            live.classify_arg(strbuf, ArgConvention::ByValue),
            live.arg_slot_width(strbuf, ArgConvention::ByValue),
            &format!("StrBuf echo argument on {target:?}"),
        );
    }
}

#[test]
fn anonymous_producer_preserves_a_deterministic_body_diagnostic_as_a_typed_failure() {
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "fn Bad() -> type {\n\
                     struct {\n\
                         x: i32,\n\
                         fn get(self) -> i32 { self.x }\n\
                         fn get(self) -> i32 { 0 }\n\
                     }\n\
                 }\n\
                 fn main() -> i32 {\n\
                     let B = Bad();\n\
                     0\n\
                 }",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

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
        .expect("a deterministic producer diagnostic is not query cancellation");
    let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    assert!(output.bodies.iter().any(|body| matches!(
        body.bundle.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyAnalysisBundle {
            transaction: crate::body_query::BodyTransaction::DeterministicFailure { .. },
            ..
        })
    )));
}

fn request_drop_glue(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    ty: crate::TypeInstanceKey,
) -> QueryRequestAttempt<crate::type_queries::DropGlueValue> {
    database.runtime.request_registered(
        &database.drop_glues,
        revision,
        crate::type_queries::TypeQueryKey {
            ty,
            configuration: semantic_configuration(),
        },
        CancellationToken::new(),
    )
}

#[test]
fn drop_glue_reads_the_shape_carried_by_type_facts_instead_of_requesting_it() {
    // RUE-1556: `TypeFacts` already carries the canonical `TypeShape` for
    // its own key — `evaluate_type_facts` stamps the shape it queried onto
    // every value it publishes — so drop glue asking the shape family again
    // was a second lookup for a value already in hand. The saved dependency
    // is still observed transitively through type-facts, so invalidation is
    // unchanged; only the direct edge is gone.
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let source = source_snapshot(
        &[(
            1,
            "/main.rue",
            "main.rue",
            "struct Child { value: i64 }\n\
                 drop fn Child(self) {}\n\
                 struct Outer { first: Child, spacer: i64, second: Child }",
        )],
        1,
    );
    let outer = named_type_instance(&module, "Outer", crate::StableDefinitionKind::Struct);
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &source);

    let attempt = request_drop_glue(&database, revision, outer.clone());
    assert_eq!(attempt.execution(), RequestExecution::Computed);
    let dependencies = attempt.terminal().unwrap().dependencies();

    let families: Vec<&str> = dependencies
        .iter()
        .map(|observation| observation.node.family())
        .collect();
    assert!(
        families.contains(&"compiler.type-facts"),
        "drop glue still depends on the facts it reads the shape from, got {families:?}"
    );
    assert!(
        !families.contains(&"compiler.type-shape"),
        "one drop-glue request must not perform a shape-family lookup of its \
             own; the shape travels with the facts. Observed families: {families:?}"
    );

    // The control for that negative: type-facts does observe the shape
    // family for this same key, so a shape edge is something these
    // dependency lists demonstrably show when one exists.
    let facts_attempt = database.runtime.request_registered(
        &database.type_facts,
        revision,
        crate::type_queries::TypeQueryKey {
            ty: outer,
            configuration: semantic_configuration(),
        },
        CancellationToken::new(),
    );
    let facts_families: Vec<&str> = facts_attempt
        .terminal()
        .unwrap()
        .dependencies()
        .iter()
        .map(|observation| observation.node.family())
        .collect();
    assert!(
        facts_families.contains(&"compiler.type-shape"),
        "type-facts is where the shape is queried and stamped onto the value, \
             got {facts_families:?}"
    );

    // The plan itself is derived from that shape, so a correct read shows up
    // as the same field-granular ownership decisions the shape describes.
    let rue_query::QueryOutcome::Success(crate::type_queries::DropGlueValue::Available(facts)) =
        attempt.terminal().unwrap().outcome()
    else {
        panic!("drop-glue plan did not publish");
    };
    let crate::type_queries::DropGluePlan::Struct { fields } = &facts.plan else {
        panic!("outer must have a struct plan");
    };
    assert_eq!(
        fields
            .iter()
            .map(|field| (field.name.as_ref(), field.drop))
            .collect::<Vec<_>>(),
        [("first", true), ("spacer", false), ("second", true)],
        "the plan must still name every field in shape order"
    );
}

#[test]
fn drop_glue_plan_is_cold_reusable_and_changes_with_order_not_only_nested_set() {
    let module = ModuleId::from_logical_path("main.rue").unwrap();
    let source = |text| source_snapshot(&[(1, "/main.rue", "main.rue", text)], 1);
    let first = source(
        "struct Child { value: i64 }\n\
             drop fn Child(self) {}\n\
             struct Outer { first: Child, spacer: i64, second: Child }",
    );
    let reordered = source(
        "struct Child { value: i64 }\n\
             drop fn Child(self) {}\n\
             struct Outer { spacer: i64, first: Child, second: Child }",
    );
    let outer = named_type_instance(&module, "Outer", crate::StableDefinitionKind::Struct);
    let mut database = RevisionedQueryDatabase::default();
    let first_revision = revision_for(&mut database, &first);
    let cold = request_drop_glue(&database, first_revision, outer.clone());
    assert_eq!(cold.execution(), RequestExecution::Computed);
    let cold_stamp = cold.terminal().unwrap().stamp();
    let cold_machine_symbol = match cold.terminal().unwrap().outcome() {
        rue_query::QueryOutcome::Success(crate::type_queries::DropGlueValue::Available(facts)) => {
            facts
                .machine_symbol
                .clone()
                .expect("drop glue owns its symbol")
        }
        _ => panic!("drop-glue plan did not publish"),
    };
    let reused = request_drop_glue(&database, first_revision, outer.clone());
    assert_eq!(reused.execution(), RequestExecution::Reused);
    assert_eq!(reused.terminal().unwrap().stamp(), cold_stamp);
    assert_eq!(
        match reused.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(crate::type_queries::DropGlueValue::Available(
                facts,
            )) => facts.machine_symbol.as_deref(),
            _ => None,
        },
        Some(cold_machine_symbol.as_ref())
    );

    let reordered_revision = revision_for(&mut database, &reordered);
    let changed = request_drop_glue(&database, reordered_revision, outer);
    assert_eq!(changed.execution(), RequestExecution::Computed);
    assert_ne!(changed.terminal().unwrap().stamp(), cold_stamp);
    let rue_query::QueryOutcome::Success(crate::type_queries::DropGlueValue::Available(facts)) =
        changed.terminal().unwrap().outcome()
    else {
        panic!("drop-glue plan did not publish");
    };
    let crate::type_queries::DropGluePlan::Struct { fields } = &facts.plan else {
        panic!("outer must have a struct plan");
    };
    assert_eq!(
        fields
            .iter()
            .map(|field| (field.name.as_ref(), field.drop))
            .collect::<Vec<_>>(),
        [("spacer", false), ("first", true), ("second", true)]
    );
    assert_eq!(
        facts.machine_symbol.as_deref(),
        Some(cold_machine_symbol.as_ref())
    );
}

fn request_lookup_name(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    module: &ModuleId,
    namespace: DefinitionNamespace,
    name: &str,
) -> QueryRequestAttempt<LookupNameValue> {
    database.runtime.request_registered(
        &database.lookup_names,
        revision,
        LookupNameKey {
            module: module.clone(),
            namespace,
            name: Arc::from(name),
        },
        CancellationToken::new(),
    )
}

fn canonical_of(attempt: &QueryRequestAttempt<LookupNameValue>) -> CanonicalNameResolution {
    let terminal = attempt.terminal().unwrap();
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!("LookupName publishes typed values")
    };
    CanonicalNameResolution::classify(value)
}

fn request_lookup_import(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    module: &ModuleId,
    specifier: &str,
) -> QueryRequestAttempt<LookupImportValue> {
    database.runtime.request_registered(
        &database.lookup_imports,
        revision,
        LookupImportKey {
            module: module.clone(),
            specifier: Arc::from(specifier),
        },
        CancellationToken::new(),
    )
}

fn import_binding(attempt: &QueryRequestAttempt<LookupImportValue>) -> LookupImportValue {
    let terminal = attempt.terminal().unwrap();
    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
        unreachable!("LookupImport publishes typed values")
    };
    value.clone()
}

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

// ---- RUE-1091 r2: type-syntax/nominal ProviderFacts differentials -------
//
// These prove `ProviderTypeFacts` (BodyFactProvider + overlay) resolves every
// type-syntax shape in r2's scope to the same durable type the production
// binder assigned, and materializes each consulted nominal into the overlay
// with byte-identical durable metadata. The reference truth is the
// production durable declaration set (the semantic-nucleus batch projection
// behind `production_declarations`), never the same provider terminal, so
// agreement is a real cross-path proof.

/// Resolve `syntax` through `ProviderTypeFacts` inside one probe, returning
/// the resolved durable type (or `None` when resolution failed / deferred),
/// the overlay metadata materialized for `materialized_key`, and the exact
/// query edges the resolution recorded.
fn resolve_type_via_provider(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    scope: &ModuleId,
    syntax: &str,
    materialized_key: Option<&StableDefinitionKey>,
) -> (
    Option<crate::DurableType>,
    Option<crate::DurableDeclarationPayload>,
    Vec<rue_query::NodeIdentity>,
) {
    let source = format!("fn probe(value: {syntax}) {{}}");
    let (tokens, interner) = rue_lexer::Lexer::new(&source).tokenize().unwrap();
    let (ast, interner) = rue_parser::Parser::new(tokens, interner).parse().unwrap();
    let rue_parser::ast::Item::Function(function) = &ast.items[0] else {
        panic!("type fixture parses as a function");
    };
    let mut builder = rue_rir::RirTypeSyntaxBuilder::default();
    let root = builder
        .push_parser_type(&function.params[0].ty, |symbol| {
            Arc::<str>::from(interner.resolve(&symbol))
        })
        .unwrap();
    let arena = builder.finish();
    let key = materialized_key.cloned();
    // The probe node is memoized by label, so each resolution needs a
    // distinct label or a repeat would reuse the first probe's terminal and
    // never run its closure.
    let label = format!("type-syntax:{syntax}");
    let outcome =
        database.probe_ready_body_facts(revision, semantic_configuration(), &label, |provider| {
            let mut overlay = crate::ProviderMaterialization::default();
            let mut facts = ProviderTypeFacts::new(provider, &mut overlay);
            let resolved =
                rue_air::resolve_structured_semantic_type_syntax(&mut facts, scope, &arena, root)
                    .ok();
            // Resolve a second time through the same overlay: resolution is
            // idempotent and a repeated consultation materializes no second
            // copy (the overlay's minted-once contract).
            let count_after_first = overlay.materialized_nominal_count();
            let mut facts = ProviderTypeFacts::new(provider, &mut overlay);
            let re_resolved =
                rue_air::resolve_structured_semantic_type_syntax(&mut facts, scope, &arena, root)
                    .ok();
            assert_eq!(
                resolved, re_resolved,
                "repeat resolution of {syntax} diverged"
            );
            assert_eq!(
                overlay.materialized_nominal_count(),
                count_after_first,
                "repeat resolution of {syntax} materialized a second overlay copy"
            );
            let materialized = key
                .as_ref()
                .and_then(|key| overlay.materialized_nominal(key).cloned());
            (resolved, materialized)
        });
    let (resolved, materialized) = outcome.result;
    (resolved, materialized, outcome.dependencies)
}

#[test]
fn provider_type_facts_resolve_nominals_and_alias_match_epoch() {
    use crate::DurableType as T;
    use crate::StableDefinitionKind as K;
    let source = "pub struct Point { x: i32, y: i32 }\n\
                      pub enum Shape { Circle, Square }\n\
                      pub const Alias: type = Point;\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let point = durable_decl(&decls, K::Struct, "Point");
    let shape = durable_decl(&decls, K::Enum, "Shape");

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // Root struct: resolves to the exact stable identity the production binder
    // assigned to `Point`, via the module-index lookup — not the epoch table —
    // and materializes `Point`'s durable metadata into the overlay byte for
    // byte.
    let (resolved, materialized, deps) =
        resolve_type_via_provider(&database, revision, &scope, "Point", Some(&point.key));
    assert_eq!(resolved, Some(T::Nominal(point.key.clone())));
    assert_eq!(
        materialized.as_ref(),
        Some(&point.payload),
        "the overlay materialized Point's durable metadata identically to production"
    );
    // Edges land at materialization: the resolution recorded the `Point`
    // name-lookup terminal and its declaration (semantic-nucleus) terminal.
    assert!(
        deps.iter()
            .any(|node| node.family() == "compiler.lookup-name" && node.key().contains("Point")),
        "recorded the Point name-lookup edge: {deps:?}"
    );
    assert!(
        deps.iter()
            .any(|node| node.family() == "compiler.semantic-nucleus"),
        "recorded a declaration (signature/identity) edge at materialization: {deps:?}"
    );
    // The resolved fact's `is_public`/`defining_file` are provider-sourced
    // but not differentially checked here: they are consumed by
    // resolution/visibility, not by durable nominal identity, and their
    // byte-identity is covered by the render/visibility differential.

    // Root enum: same cross-path identity agreement + materialization.
    let (resolved, materialized, _deps) =
        resolve_type_via_provider(&database, revision, &scope, "Shape", Some(&shape.key));
    assert_eq!(resolved, Some(T::Nominal(shape.key.clone())));
    assert_eq!(materialized.as_ref(), Some(&shape.payload));

    // Root type alias: a `const` bound to a type resolves to that type — here
    // the nominal the alias points at, so `Alias` and `Point` collapse to the
    // same durable identity, exactly as the epoch resolves an alias head.
    let (resolved, _materialized, _deps) =
        resolve_type_via_provider(&database, revision, &scope, "Alias", None);
    assert_eq!(resolved, Some(T::Nominal(point.key.clone())));
}

#[test]
fn provider_type_facts_resolve_primitive_and_structural_shapes() {
    use crate::DurableType as T;
    let source = "fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // Enumerate the `SemanticImportType` (durable) arms r2 covers structurally.
    // Each is resolved through the shared logic driven by ProviderTypeFacts;
    // primitives and structural wrappers consult no declaration fact, so they
    // record no dependency edge (proven below).
    let primitive_cases: &[(&str, T)] = &[
        ("i8", T::I8),
        ("i16", T::I16),
        ("i32", T::I32),
        ("i64", T::I64),
        ("u8", T::U8),
        ("u16", T::U16),
        ("u32", T::U32),
        ("u64", T::U64),
        ("usize", T::U64),
        ("isize", T::I64),
        ("bool", T::Bool),
        ("()", T::Unit),
        ("!", T::Never),
        ("type", T::ComptimeType),
    ];
    for (syntax, expected) in primitive_cases {
        let (resolved, _materialized, deps) =
            resolve_type_via_provider(&database, revision, &scope, syntax, None);
        assert_eq!(resolved.as_ref(), Some(expected), "primitive `{syntax}`");
        assert!(
            deps.is_empty(),
            "a primitive consults no declaration fact and records no edge: `{syntax}` -> {deps:?}"
        );
    }

    let structural_cases: &[(&str, T)] = &[
        (
            "[i32; 2]",
            T::Array {
                element: Arc::new(T::I32),
                len: 2,
            },
        ),
        (
            "[u8; 4]",
            T::Array {
                element: Arc::new(T::U8),
                len: 4,
            },
        ),
        ("ptr const i32", T::PtrConst(Arc::new(T::I32))),
        ("ptr mut u64", T::PtrMut(Arc::new(T::U64))),
        (
            "ptr const [i32; 2]",
            T::PtrConst(Arc::new(T::Array {
                element: Arc::new(T::I32),
                len: 2,
            })),
        ),
    ];
    for (syntax, expected) in structural_cases {
        let (resolved, _materialized, deps) =
            resolve_type_via_provider(&database, revision, &scope, syntax, None);
        assert_eq!(resolved.as_ref(), Some(expected), "structural `{syntax}`");
        assert!(
            deps.is_empty(),
            "structural `{syntax}` records no edge: {deps:?}"
        );
    }
}

#[test]
fn provider_type_facts_absent_and_kind_mismatch_do_not_resolve() {
    let source = "pub struct Point { x: i32 }\n\
                      pub enum Shape { A, B }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // An absent name has no candidate and does not resolve (UnknownType).
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Missing", None);
    assert_eq!(resolved, None, "an absent type name does not resolve");

    // A name that exists but as the wrong kind (a function used as a type) is
    // kind-filtered out of the nominal candidate set and does not resolve — the
    // candidate-set-not-winner contract, applied in the shared logic.
    let (resolved, _m, _d) = resolve_type_via_provider(&database, revision, &scope, "main", None);
    assert_eq!(resolved, None, "a function name does not resolve as a type");
}

// The builtin `str` and slice `[T]` name facts — RUE-1091 r6a flips these two
// arms from documented gaps to positive differentials: their durable identity
// is a pure durable fact (a `BuiltinNominal` name+kind for `str`, a
// `Slice { element, name: syntax }` for a slice) needing no new boundary op,
// matching what `export_type_local` reproduces for the epoch's materialized
// `str`/slice struct.
#[test]
fn provider_type_facts_builtin_str_and_slice_names_match_epoch() {
    use crate::DurableType as T;
    let source = "pub struct Point { x: i32 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // `str` resolves to the durable builtin-nominal identity — the exact form
    // `export_type_local` reproduces for the epoch's `str` struct.
    let (resolved, _m, deps) = resolve_type_via_provider(&database, revision, &scope, "str", None);
    assert_eq!(
        resolved,
        Some(T::BuiltinNominal {
            kind: rue_air::SemanticImportNominalKind::Struct,
            name: Arc::from("str"),
        }),
        "`str` resolves to the builtin-nominal durable identity"
    );
    // A pool/overlay-answered name fact records no provider query edge (edge
    // honesty — the builtin identity is not a boundary lookup).
    assert!(
        deps.is_empty(),
        "resolving `str` records no provider edge: {deps:?}"
    );

    // `[i32]` resolves to the durable slice identity whose name IS the slice
    // syntax and whose element is the resolved element type.
    let (resolved, _m, deps) =
        resolve_type_via_provider(&database, revision, &scope, "[i32]", None);
    assert_eq!(
        resolved,
        Some(T::Slice {
            element: Arc::new(T::I32),
            name: Arc::from("[i32]"),
        }),
        "`[i32]` resolves to the slice durable identity keyed by the slice syntax"
    );
    assert!(
        deps.is_empty(),
        "resolving `[i32]` records no provider edge: {deps:?}"
    );
}

// Explicit enumeration of the `SemanticImportType` arms this family does NOT
// yet cover, each with the boundary fact it waits on. Documented as
// not-yet-resolvable (never silently green): a deferred shape resolves to
// `None` through ProviderTypeFacts today, and the differential pins that so a
// later slice that adds the fact flips the arm deliberately.
//   - BuiltinNominal `Str(N)`: a generated fixed-capacity struct whose durable
//     identity is a generated-struct classification (`export_type_local`
//     rejects it as a `ForeignLocalType`). RUE-1091 r6b MINTS it in the pool
//     (`BodyIdentityPool::get_or_create_str_fixed`), but the TYPE-SYNTAX
//     resolution to that durable identity still needs the generated-struct
//     classification — deferred here (owner: Str(N) type-syntax classification).
//   - AnonymousNominal: produced by a body / a comptime call reducing to an
//     anonymous struct (`Pair()` below). RUE-1091 r6b MINTS it in the pool
//     (`find_or_create_anon`, proven cross-path in
//     `provider_endpoint_facts_anonymous_arm_mints_after_registration`), but the
//     anonymous reduction result is a body-level durable value the production
//     declaration binder rejects exporting (`AnonymousNominalType`), so the
//     type-syntax resolution stays deferred here (owner: body-level anonymous
//     type-syntax resolution).
//   - Module / GenericParameter: not reachable as a resolved type-syntax leaf.
// `str` and slice `[T]` are NO LONGER gaps — r6a flipped them (see
// `provider_type_facts_builtin_str_and_slice_names_match_epoch`).
// The comptime type-call arm itself is NO LONGER a gap — r5a flipped it (see
// `provider_type_facts_comptime_calls_match_epoch`); only the anonymous-nominal
// RESULT of such a call is still deferred at the type-syntax boundary (the pool
// mints the identity), and that deferral is pinned here.
#[test]
fn provider_type_facts_deferred_shapes_are_documented_gaps() {
    let source = "pub struct Point { x: i32 }\n\
                      fn Pair() -> type { struct { a: i32 } }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // Two type-syntax resolutions the POOL keystone mints but whose
    // declaration-level type-syntax resolution stays deferred in this slice:
    //  - `Str(8)`: a generated fixed-capacity string struct whose durable
    //    identity is a generated-struct classification (`export_type_local`
    //    rejects it as a `ForeignLocalType`). The pool mints it
    //    (`BodyIdentityPool::get_or_create_str_fixed`, r6b); the type-syntax
    //    resolution to that durable identity needs the generated-struct
    //    classification the r6a report deferred (owner: the Str(N) type-syntax
    //    classification follow-up).
    //  - `Pair()`: reduces to an ANONYMOUS nominal. The pool mints that
    //    identity (`find_or_create_anon`, proven cross-path in
    //    `provider_endpoint_facts_anonymous_arm_mints_after_registration`), but
    //    the anonymous reduction result is a BODY-level durable value the
    //    production declaration binder rejects exporting (`AnonymousNominalType`),
    //    so the type-syntax resolution has no declaration-level cross-path
    //    truth and stays deferred here (owner: body-level anonymous type-syntax
    //    resolution follow-up).
    for deferred in ["Pair()", "Str(8)"] {
        let (resolved, _m, _d) =
            resolve_type_via_provider(&database, revision, &scope, deferred, None);
        assert_eq!(
            resolved, None,
            "`{deferred}` type-syntax resolution is a documented deferral (the pool mints it; \
                 the type-syntax resolution stays deferred)"
        );
    }

    // A covered nominal in the same body still resolves — the deferrals do not
    // poison the family.
    let decls = production_declarations(&snapshot);
    let point = durable_decl(&decls, crate::StableDefinitionKind::Struct, "Point");
    let (resolved, _m, _d) = resolve_type_via_provider(&database, revision, &scope, "Point", None);
    assert_eq!(
        resolved,
        Some(crate::DurableType::Nominal(point.key.clone()))
    );
}

// ---- RUE-1091 r5a: SignatureFacts comptime-call differentials ------------
//
// These prove the flipped `ProviderTypeFacts` comptime-call arms (backed by
// `SignatureFacts` + the argument-parameterized comptime-call boundary op)
// reduce a comptime type/value call to the same durable type/value the
// production nucleus assigned. The reference truth is the production durable
// const declaration whose initializer IS the call, produced independently by
// the semantic-nucleus batch projection, never the same provider terminal.

/// The `Const { value }` durable value the production binder assigned to the
/// value-const named `name`.
fn production_const_value(
    decls: &[crate::durable_semantics::DurableDeclarationSemantic],
    name: &str,
) -> crate::DurableConstValue {
    let decl = durable_decl(decls, crate::StableDefinitionKind::ValueConst, name);
    match &decl.payload {
        crate::durable_semantics::DurableDeclarationPayload::Const { value, .. } => value.clone(),
        other => panic!("const `{name}` is not a value const: {other:?}"),
    }
}

/// The declared type of parameter `index` of the value-const-time signature
/// the production binder assigned to the callable named `name`.
fn production_signature_parameter(
    decls: &[crate::durable_semantics::DurableDeclarationSemantic],
    name: &str,
    index: usize,
) -> crate::DurableType {
    let decl = durable_decl(decls, crate::StableDefinitionKind::Function, name);
    match &decl.payload {
        crate::durable_semantics::DurableDeclarationPayload::Callable { parameters, .. } => {
            parameters[index].ty.clone()
        }
        other => panic!("`{name}` is not callable: {other:?}"),
    }
}

#[test]
fn provider_type_facts_comptime_calls_match_epoch() {
    use crate::DurableType as T;
    // `Id`/`Nth` reduce a comptime TYPE argument to a passthrough type (the
    // nucleus comptime-call terminal the boundary op drives can reduce these).
    // `Nth` additionally binds a comptime VALUE argument — a literal and a
    // scoped const — so `resolve_value_argument`/`const_value_fact` are on the
    // passing path. Each is declared as `const C: type = <call>`, so the
    // production const value is the independent cross-path truth. `Buffer`
    // additionally proves that the candidate-RIR evaluator preserves the
    // expression/type disambiguation for `[T; n]`: an array repeat whose
    // element is a comptime type constructs the array type.
    let source = "pub struct Point { x: i32 }\n\
                      fn Id(comptime T: type) -> type { T }\n\
                      pub fn Nth(comptime T: type, comptime k: i32) -> type { T }\n\
                      pub fn Buffer(comptime n: i32) -> type { [i32; n] }\n\
                      pub const N: i32 = 3;\n\
                      pub const IdPoint: type = Id(Point);\n\
                      pub const IdI32: type = Id(i32);\n\
                      pub const NthP2: type = Nth(Point, 2);\n\
                      pub const NthPN: type = Nth(Point, N);\n\
                      pub const Buffer2: type = Buffer(2);\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let point = durable_decl(&decls, crate::StableDefinitionKind::Struct, "Point");
    let point_type = T::Nominal(point.key.clone());

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // `Id(Point)` — a comptime TYPE argument reduced to the exact nominal
    // identity the production const `IdPoint` holds. Records the comptime-call
    // reduction (semantic-nucleus) edge.
    let (resolved, _m, deps) =
        resolve_type_via_provider(&database, revision, &scope, "Id(Point)", None);
    assert_eq!(resolved, Some(point_type.clone()));
    assert_eq!(
        production_const_value(&decls, "IdPoint"),
        crate::DurableConstValue::Type(point_type.clone()),
        "cross-path: the production const holds the same reduced nominal",
    );
    assert!(
        deps.iter()
            .any(|node| node.family() == "compiler.semantic-nucleus"),
        "a comptime call records its reduction (semantic-nucleus) edge: {deps:?}"
    );

    // `Id(i32)` — the reduction collapses to a primitive; cross-checked
    // against the production const `IdI32`.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Id(i32)", None);
    assert_eq!(resolved, Some(T::I32));
    assert_eq!(
        production_const_value(&decls, "IdI32"),
        crate::DurableConstValue::Type(T::I32),
    );

    // `Nth(Point, 2)` — a comptime VALUE argument from a LITERAL flows through
    // `resolve_value_argument`; the reduction passes the type argument through.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Nth(Point, 2)", None);
    assert_eq!(resolved, Some(point_type.clone()));
    assert_eq!(
        production_const_value(&decls, "NthP2"),
        crate::DurableConstValue::Type(point_type.clone()),
    );

    // `Nth(Point, N)` — a comptime VALUE argument resolved through a SCOPED
    // CONST (`value_argument_fact` -> `const_value_fact`), cross-checked
    // against the production const `NthPN`.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Nth(Point, N)", None);
    assert_eq!(resolved, Some(point_type.clone()));
    assert_eq!(
        production_const_value(&decls, "NthPN"),
        crate::DurableConstValue::Type(point_type),
    );

    // `Buffer(2)` — an array-constructing type ctor reduces through the same
    // candidate artifact, including its comptime value parameter.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "Buffer(2)", None);
    let buffer_type = T::Array {
        element: Arc::new(T::I32),
        len: 2,
    };
    assert_eq!(
        resolved,
        Some(buffer_type.clone()),
        "an array-constructing comptime type ctor reduces through the candidate artifact"
    );
    assert_eq!(
        production_const_value(&decls, "Buffer2"),
        crate::DurableConstValue::Type(buffer_type),
    );
}

#[test]
fn provider_type_facts_named_array_length_matches_epoch() {
    use crate::DurableType as T;
    // A named array length that is a scoped `const` now resolves through
    // `SignatureFacts::const_value_fact` (r5a flip of `resolve_array_length`).
    // The production binder's own resolution of the same `[i32; N]` in a
    // signature is the independent cross-path truth. A comptime CALL in length
    // position stays deferred (r6).
    let source = "const N: i32 = 3;\n\
                      fn use_len(a: [i32; N]) -> i32 { a[0] }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    // `[i32; N]` — the named length `N` resolves to the scoped const's value,
    // matching the durable signature the binder assigned to `use_len`.
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "[i32; N]", None);
    let expected = T::Array {
        element: Arc::new(T::I32),
        len: 3,
    };
    assert_eq!(resolved, Some(expected.clone()));
    assert_eq!(
        production_signature_parameter(&decls, "use_len", 0),
        expected
    );

    // `[i32; missing]` — an unresolvable named length stays a deferred/None
    // resolution (no scoped const `missing` exists).
    let (resolved, _m, _d) =
        resolve_type_via_provider(&database, revision, &scope, "[i32; missing]", None);
    assert_eq!(resolved, None);
}

#[test]
fn signature_facts_constructor_head_carries_named_typed_parameters() {
    use rue_air::BodyFactProvider;
    // SignatureFacts reconstructs the constructor head from `signature()`
    // alone: the durable parameter names (part 1) become the head's parameter
    // names, and `is_type` is `is_comptime && ty == comptime type` — the same
    // predicate the epoch's `constructor_fact` applies from the shell.
    let source = "fn Wrap(comptime T: type, comptime n: i32) -> type { [T; n] }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let scope = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "signature-facts:Wrap",
        |provider| {
            let facts = SignatureFacts::new(provider);
            let resolution =
                provider.lookup_unqualified(&scope, rue_air::ProviderNamespace::ModuleItem, "Wrap");
            let head = facts
                .constructor_head_fact(&scope, resolution, "Wrap")
                .expect("Wrap resolves to a constructor head");
            let names = head
                .parameters
                .iter()
                .map(|p| (p.name.to_string(), p.is_comptime, p.is_type))
                .collect::<Vec<_>>();
            (head.returns_type, names)
        },
    );
    let (returns_type, names) = outcome.result;
    assert!(returns_type, "`Wrap` returns a type");
    assert_eq!(
        names,
        vec![
            ("T".to_string(), true, true),  // comptime type parameter
            ("n".to_string(), true, false), // comptime value parameter
        ],
        "head carries durable parameter names and the type/value split"
    );
    // Absent / non-callable heads do not resolve.
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "signature-facts:absent",
        |provider| {
            let facts = SignatureFacts::new(provider);
            let resolution = provider.lookup_unqualified(
                &scope,
                rue_air::ProviderNamespace::ModuleItem,
                "Missing",
            );
            facts
                .constructor_head_fact(&scope, resolution, "Missing")
                .is_some()
        },
    );
    assert!(!outcome.result, "an absent name has no constructor head");
}

#[test]
fn provider_declaration_facts_match_production_epoch() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "@copy struct Box { value: i32, fn get(borrow self) -> i32 { self.value } }\n\
                 struct Res { handle: i32 }\n\
                 drop fn Res(self) {}\n\
                 fn helper(x: i32) -> i32 { x }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let config = semantic_configuration();

    let helper = declaration_candidate(&database, revision, &m, Cat::Function, "helper");
    let helper_instance = free_function_instance(&m, "helper");
    let box_struct = declaration_candidate(&database, revision, &m, Cat::Struct, "Box");
    let copy_receiver = ReceiverTypeIdentity::new(m.clone(), "Box", Cat::Struct);
    let res_receiver = ReceiverTypeIdentity::new(m.clone(), "Res", Cat::Struct);

    let helper_probe = helper.clone();
    let box_probe = box_struct.clone();
    let copy_probe = copy_receiver.clone();
    let res_probe = res_receiver.clone();
    let metrics_before = database.provider_observation_metrics();
    let outcome =
        database.probe_ready_body_facts(revision, config.clone(), "decl-facts", move |provider| {
            (
                provider.declaration_identity(&helper_probe),
                provider.signature(&helper_probe),
                provider.nominal_well_formedness(&box_probe),
                provider.signature(&box_probe),
                provider.anonymous_facts(&helper_probe),
                provider.language_item(&m, rue_air::ProviderNamespace::ModuleItem, "Box"),
                provider.drop_copy_metadata(&copy_probe),
                provider.drop_copy_metadata(&res_probe),
                provider.trusted_toolchain_facts(&helper_instance),
            )
        });
    let (
        identity,
        signature,
        well_formed,
        box_sig,
        anon,
        lang_item,
        copy_meta,
        res_meta,
        toolchain,
    ) = outcome.result;
    let metrics_after = database.provider_observation_metrics();
    let identity_facts = metrics_after.identity_facts - metrics_before.identity_facts;
    let signature_facts = metrics_after.signature_facts - metrics_before.signature_facts;
    let type_facts = metrics_after.type_facts - metrics_before.type_facts;
    let const_facts = metrics_after.const_facts - metrics_before.const_facts;
    let declaration_facts = metrics_after.declaration_facts - metrics_before.declaration_facts;
    assert_eq!(identity_facts, 1);
    assert_eq!(signature_facts, 4, "two direct and two drop/copy reads");
    assert_eq!(type_facts, 1);
    assert_eq!(const_facts, 0);
    assert_eq!(
        declaration_facts,
        identity_facts + signature_facts + type_facts + const_facts,
        "the declaration total is exactly partitioned by backing fact family"
    );

    // Identity / signature differential against the semantic-nucleus epoch.
    let epoch_identity = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Identity(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: helper.clone(),
                configuration: config.clone(),
            },
        ),
    );
    let crate::semantic_query_nucleus::SemanticNucleusValue::Identity(epoch_identity) =
        epoch_identity
    else {
        panic!("helper has an identity")
    };
    assert_eq!(identity, Some(epoch_identity));

    let epoch_signature = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: helper.clone(),
                configuration: config.clone(),
            },
        ),
    );
    let crate::semantic_query_nucleus::SemanticNucleusValue::Signature(epoch_signature) =
        epoch_signature
    else {
        panic!("helper has a signature")
    };
    assert_eq!(signature.as_ref(), Some(&epoch_signature));
    // Anonymous facts are the signature's own anonymous nominals.
    assert_eq!(anon, Some(epoch_signature.anonymous_nominals.clone()));

    // A well-formed nominal; `@copy` and its destructor are exact facts.
    assert_eq!(
        well_formed,
        Some(rue_air::NominalWellFormedness::WellFormed)
    );
    assert!(matches!(
        box_sig.as_ref().map(|sig| &sig.signature),
        Some(
            crate::semantic_query_nucleus::DeclarationSignatureProjection::Struct {
                is_copy: true,
                ..
            }
        )
    ));
    // `@copy` Box has no destructor; Res has a destructor and is not copy.
    // Both facts are sourced from the destructor lookup + struct signature.
    assert_eq!(
        copy_meta,
        Some(rue_air::DropCopyMetadata {
            has_destructor: false,
            is_copy: true,
        })
    );
    assert_eq!(
        res_meta,
        Some(rue_air::DropCopyMetadata {
            has_destructor: true,
            is_copy: false,
        })
    );
    // A user nominal is not a language item.
    assert_eq!(lang_item, None);
    // A plain function demands no trusted-toolchain module.
    assert!(toolchain.modules().is_empty());

    // Edge-recording proof: declaration facts observe semantic-nucleus (and,
    // for drop metadata, a destructor lookup-name) terminals only.
    let families: std::collections::BTreeSet<&str> = outcome
        .dependencies
        .iter()
        .map(|node| node.family())
        .collect();
    assert!(
        families.contains("compiler.semantic-nucleus"),
        "{families:?}"
    );
    assert!(
        families
            .iter()
            .all(|family| *family == "compiler.semantic-nucleus"
                || *family == "compiler.lookup-name"
                || *family == "compiler.body-toolchain-demands"),
        "declaration facts observe only their exact backing terminals: {families:?}"
    );
}

#[test]
fn provider_repeated_nucleus_fact_reuses_the_request_local_terminal() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn helper(x: i32) -> i32 { x }\nfn main() -> i32 { helper(0) }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let helper = declaration_candidate(&database, revision, &m, Cat::Function, "helper");
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "repeated-nucleus-fact",
        move |provider| {
            let first = provider.signature(&helper);
            let second = provider.signature(&helper);
            (first, second, provider.nucleus_cache_hits.get())
        },
    );

    assert_eq!(outcome.result.0, outcome.result.1);
    assert_eq!(outcome.result.2, 1);
}

#[test]
fn provider_repeated_name_lookup_reuses_the_request_local_terminal() {
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn helper(x: i32) -> i32 { x }\nfn main() -> i32 { helper(0) }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "repeated-name-lookup",
        move |provider| {
            let first =
                provider.lookup_unqualified(&m, rue_air::ProviderNamespace::ModuleItem, "helper");
            let second =
                provider.lookup_unqualified(&m, rue_air::ProviderNamespace::ModuleItem, "helper");
            (first, second, provider.lookup_name_cache_hits.get())
        },
    );

    assert_eq!(outcome.result.0, outcome.result.1);
    assert_eq!(outcome.result.2, 1);
    assert_eq!(
        outcome
            .dependencies
            .iter()
            .filter(|node| node.family() == "compiler.lookup-name")
            .count(),
        1,
        "the cache hit reuses the request's already-observed lookup edge"
    );
}

// ---- RUE-1091 r4b-1: call-resolution ProviderFacts differentials --------
//
// These prove `rue_air::ProviderCallFacts` (the provider-driven realization
// of the r1b `CallResolutionFacts` seam) assembles the family-1C identities
// from the exact body-fact provider (`CompilerBodyFactProvider`) + the body
// identity pool. The durable source the pool consults is built from the
// production durable declaration set (the semantic-nucleus batch
// projection, r2's stable-keyed metadata), so agreement is a real
// cross-path proof, not the same provider terminal.
//
// Scope landed here: free-function and nominal-member info composition
// (including associated functions), lookup selection, callable-symbol
// reversal, const overlays, and the body-local module registry. The
// production cutover owns assembling and registering those body-local facts.

/// Render a pool `Type` to a comparable display through the minted pool, the
/// index-independent parity the 2a/2b contract asserts (never a pool-relative
/// index).
fn render_pool_type(pool: &rue_air::TypeInternPool, ty: rue_air::Type) -> String {
    use rue_air::TypeKind;
    match ty.kind() {
        TypeKind::I8 => "i8".into(),
        TypeKind::I16 => "i16".into(),
        TypeKind::I32 => "i32".into(),
        TypeKind::I64 => "i64".into(),
        TypeKind::U8 => "u8".into(),
        TypeKind::U16 => "u16".into(),
        TypeKind::U32 => "u32".into(),
        TypeKind::U64 => "u64".into(),
        TypeKind::Bool => "bool".into(),
        TypeKind::Unit => "()".into(),
        TypeKind::Never => "!".into(),
        TypeKind::Struct(id) => pool.struct_def(id).name.to_string(),
        TypeKind::Enum(id) => pool.enum_def(id).name.to_string(),
        other => format!("{other:?}"),
    }
}

#[test]
fn provider_call_facts_function_info_is_assembled_from_durable_truth() {
    use crate::StableDefinitionKind as Kind;
    // A free function whose first parameter is a NOMINAL (`Point`): its type
    // resolves through the pool's 2a nominal machinery, its `n`/return through
    // the primitive arms — the full 2a+2b+2c compose behind the seam.
    let source = "pub struct Point { x: i64, y: i64 }\n\
                      @allow(unused_function)\n\
                      pub fn make(p: Point, n: i32) -> i64 { 0 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let make = durable_decl(&decls, Kind::Function, "make");
    let make_key = make.key.clone();

    // The RIR + its interner are body-query inputs the driver fills the
    // request/RIR handle from.
    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let source_adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "call-fn-info",
        move |provider| {
            let facts = rue_air::ProviderCallFacts::new(
                provider,
                source_adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let info = facts
                .function_info(&make_key, "make", file)
                .expect("make resolves through the provider path");
            // Double consult: idempotent, the pool mints nothing new.
            let second = facts
                .function_info(&make_key, "make", file)
                .expect("repeat consult resolves");
            // `FunctionInfo` is not `PartialEq`; compare the load-bearing
            // identity fields to prove the repeat consult is stable.
            assert_eq!(second.declaration, info.declaration);
            assert_eq!(second.body, info.body);
            assert_eq!(
                second.params, info.params,
                "repeat consult re-minted params"
            );

            // Parameter vocabulary (2b), types resolved through 2a — asserted
            // through the index-independent render/name reads (the pool mints
            // its own ids; parity is a display property, not a raw index).
            let (names, types, modes) = facts.with_param_arena(|arena| {
                (
                    arena.names(info.params).to_vec(),
                    arena.types(info.params).to_vec(),
                    arena.modes(info.params).to_vec(),
                )
            });
            assert_eq!(info.params.len(), 2, "two explicit params");
            assert_eq!(facts.resolve_symbol(names[0]), "p");
            assert_eq!(facts.resolve_symbol(names[1]), "n");
            facts.with_type_pool(|pool| {
                assert_eq!(
                    render_pool_type(pool, types[0]),
                    "Point",
                    "the nominal param minted through 2a"
                );
                assert_eq!(render_pool_type(pool, types[1]), "i32");
                assert_eq!(render_pool_type(pool, info.return_type), "i64");
            });
            assert_eq!(modes[0], rue_rir::RirParamMode::Normal);
            info
        },
    );
    let info = outcome.result;

    // The assembled handles resolve back into the exact source declaration:
    // the r4a-2c span contract sources `FunctionInfo.span` from the shell's
    // declaration span, so it must slice to the declaration text, and the
    // declaration/body refs must name the RIR instructions at those spans.
    assert_eq!(
        &source[info.span.start as usize..info.span.end as usize],
        "@allow(unused_function)\npub fn make(p: Point, n: i32) -> i64 { 0 }",
        "assembled span slices to the attributed declaration text"
    );
    let declaration = rir_ref.get(info.declaration);
    assert!(
        matches!(declaration.data, rue_rir::InstData::FnDecl { .. }),
        "the declaration handle names the FnDecl instruction"
    );
    assert_eq!(
        info.span, declaration.span,
        "the assembled span is the declaration instruction's own"
    );
    let body_span = rir_ref.get(info.body).span;
    assert_eq!(
        &source[body_span.start as usize..body_span.end as usize],
        "0",
        "the body handle names the body expression"
    );
    assert_eq!(info.file_id, file);
    assert!(info.is_pub, "make is declared pub");
    assert!(!info.is_generic, "make has no comptime parameters");
    assert!(!info.is_unchecked);
    assert_eq!(
        rir_ref
            .type_syntax()
            .render_type_with(info.return_type_syntax, |symbol| interner.resolve(symbol))
            .as_deref(),
        Some("i64"),
        "the pre-resolution return syntax spells the annotated type"
    );

    // The P-op path consults the pool (durable source) + the RIR handle, not
    // the live provider terminals, so it records no provider query edge — the
    // pool is answered-by-metadata, and edge honesty is a C/B-op property
    // (pinned by the callable-symbol and name-lookup differentials).
    assert!(
        outcome.dependencies.is_empty(),
        "a pool-answered function_info records no provider edge: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_call_facts_function_contains_selects_from_the_candidate_set() {
    let source = "pub struct Point { x: i32 }\n\
                      pub fn helper() -> i32 { 0 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let (rir_out, _semantic, _) =
        crate::test_support::test_frontend_snapshot(&snapshot, &crate::CompileOptions::default())
            .expect("frontend compiles");
    let rir = rir_out.rir();
    let interner = rir_out.semantic_symbols().interner();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let source_adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "call-fn-contains",
        move |provider| {
            let facts = rue_air::ProviderCallFacts::new(
                provider,
                source_adapter,
                rue_air::BodyRirView::from_parts(rir, interner),
            );
            (
                // A declared free function is present.
                facts.function_contains_in_module(&m, "helper"),
                // A struct name is NOT a free function (kind-filtered out of
                // the candidate set — the candidate-sets-not-winners contract).
                facts.function_contains_in_module(&m, "Point"),
                // An absent name.
                facts.function_contains_in_module(&m, "missing"),
            )
        },
    );
    let (helper, point, missing) = outcome.result;
    assert!(helper, "helper is a declared free function");
    assert!(!point, "a struct is not a free function");
    assert!(!missing, "an absent name is not a free function");
    // The lookups are observed through the provider's exact name terminal.
    assert!(
        outcome
            .dependencies
            .iter()
            .any(|node| node.family() == "compiler.lookup-name"),
        "function_contains observes the name-lookup terminal: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_call_facts_method_info_is_assembled_from_durable_truth() {
    use crate::StableDefinitionKind as Kind;
    // A named method whose receiver (`Widget`), one explicit param (`Point`),
    // and return (`i64`) all resolve through the pool's 2a nominal machinery
    // — the r4b-3 backlog item: the receiver preimage `(owner_file,
    // owner_type_name)` threads through the durable method key, recovered by
    // joining the method key's `owner()` back to the owner nominal's durable
    // key (the `DurableDeclSource::method` receiver join).
    let source = "pub struct Point { x: i64, y: i64 }\n\
                      pub struct Widget { id: i64, \
                        fn shift(borrow self, p: Point, n: i32) -> i64 { self.id } }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let shift = durable_decl(&decls, Kind::Method, "shift");
    let shift_key = shift.key.clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let source_adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "call-method-info",
        move |provider| {
            let identity = rue_air::ProviderIdentityContext::new(source_adapter);
            let view = rue_air::BodyRirView::from_parts(rir_ref, interner);
            let facts =
                rue_air::ProviderCallFacts::with_identity(provider, identity.clone(), view.clone());
            let endpoints = rue_air::ProviderEndpointFacts::with_identity(provider, identity, view);
            // `named_method_info` coincides over the named differential scope
            // (no anonymous fallback), mirroring the epoch's `methods.get`.
            let named = facts
                .named_method_info(&shift_key, file, "Widget", "shift")
                .expect("named_method_info resolves");
            let compact_owner = named.struct_type.as_struct().expect("Widget is a struct");
            let compact_name = facts.name_symbol("shift").expect("shift is interned");
            assert_eq!(
                endpoints
                    .method_info(compact_owner, compact_name)
                    .expect("the endpoint facts observe the named registration")
                    .body,
                named.body,
                "endpoint method lookup falls back to the shared named entry"
            );
            let anonymous = rue_air::MethodInfo {
                body: rue_rir::InstRef::from_raw(named.body.as_u32() + 1),
                ..named
            };
            assert!(
                facts
                    .register_anonymous_method(file, "Widget", "shift", anonymous)
                    .expect("shift name is admitted"),
                "the anonymous method registers atomically under both lookup keys"
            );
            let info = facts
                .method_info(&shift_key, file, "Widget", "shift")
                .expect("anonymous method wins over the named collision");
            assert_eq!(info.body, anonymous.body, "anonymous method has precedence");
            assert_ne!(named.body, info.body, "the collision is observable");
            assert_eq!(
                endpoints
                    .method_info(compact_owner, compact_name)
                    .expect("the endpoint facts observe the anonymous registration")
                    .body,
                anonymous.body,
                "endpoint and call facts agree on anonymous-first precedence"
            );
            // Double consult: idempotent, the pool re-mints nothing.
            let second = facts
                .method_info(&shift_key, file, "Widget", "shift")
                .expect("repeat consult resolves");
            assert_eq!(second.body, info.body);
            assert_eq!(
                second.params, info.params,
                "repeat consult re-minted params"
            );

            // Explicit params (self excluded): one nominal (`Point` through
            // 2a) and one primitive, asserted through the index-independent
            // render / resolved-name reads.
            let (names, types, modes) = facts.with_param_arena(|arena| {
                (
                    arena.names(info.params).to_vec(),
                    arena.types(info.params).to_vec(),
                    arena.modes(info.params).to_vec(),
                )
            });
            assert_eq!(info.params.len(), 2, "self is excluded from params");
            assert_eq!(facts.resolve_symbol(names[0]), "p");
            assert_eq!(facts.resolve_symbol(names[1]), "n");
            let (receiver, ret) = facts.with_type_pool(|pool| {
                assert_eq!(render_pool_type(pool, types[0]), "Point");
                assert_eq!(render_pool_type(pool, types[1]), "i32");
                (
                    render_pool_type(pool, info.struct_type),
                    render_pool_type(pool, info.return_type),
                )
            });
            assert_eq!(modes[0], rue_rir::RirParamMode::Normal);

            (info, named, receiver, ret)
        },
    );
    let (info, named, receiver, ret) = outcome.result;

    // The assembled metadata resolves back into the exact source
    // declaration: pool-relative types by index-independent render, RIR
    // handles by the source text at their spans.
    assert_eq!(receiver, "Widget", "receiver is the owning nominal");
    assert_eq!(ret, "i64", "return renders as the annotated type");
    assert!(info.has_self, "shift takes self");
    assert_eq!(info.self_mode, rue_rir::RirParamMode::Borrow);
    assert!(!info.self_is_mut, "shift's receiver is not `mut self`");
    let named_body_span = rir_ref.get(named.body).span;
    assert_eq!(
        &source[named_body_span.start as usize..named_body_span.end as usize],
        "self.id",
        "the named method body handle names shift's body expression"
    );
    assert_ne!(
        info.body, named.body,
        "anonymous collision remains selected"
    );
    assert_eq!(
        &source[info.span.start as usize..info.span.end as usize],
        "fn shift(borrow self, p: Point, n: i32) -> i64 { self.id }",
        "the method span slices to the declaration text"
    );

    // The P-op path consults the pool + the RIR handle, not the live provider
    // terminals, so it records no provider edge (pool answered-by-metadata).
    assert!(
        outcome.dependencies.is_empty(),
        "a pool-answered method_info records no provider edge: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_call_facts_associated_function_is_assembled_from_durable_truth() {
    use crate::StableDefinitionKind as Kind;

    let source = "pub struct Counter { value: i32, \
                        fn make(value: i32) -> Counter { Counter { value: value } } }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let make_key = durable_decl(&decls, Kind::AssociatedFunction, "make")
        .key
        .clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "call-associated-info",
        move |provider| {
            let facts = rue_air::ProviderCallFacts::new(
                provider,
                DurableDeclSource::from_declarations(&decls),
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let info = facts
                .method_info(&make_key, file, "Counter", "make")
                .expect("associated function resolves through the method namespace");
            let types = facts.with_type_pool(|pool| {
                (
                    render_pool_type(pool, info.struct_type),
                    render_pool_type(pool, info.return_type),
                )
            });
            (info, types)
        },
    );
    let (provider, provider_types) = outcome.result;

    assert_eq!(
        provider_types,
        ("Counter".to_owned(), "Counter".to_owned()),
        "owner and return both render as the owning nominal"
    );
    assert!(!provider.has_self, "Counter.make is an associated function");
    assert_eq!(
        provider.params.len(),
        1,
        "make takes one explicit parameter"
    );
    let body_span = rir_ref.get(provider.body).span;
    assert_eq!(
        &source[body_span.start as usize..body_span.end as usize],
        "Counter { value: value }",
        "the body handle names make's body expression"
    );
    assert_eq!(
        &source[provider.span.start as usize..provider.span.end as usize],
        "fn make(value: i32) -> Counter { Counter { value: value } }",
        "the assembled span slices to the declaration text"
    );
    assert!(
        outcome.dependencies.is_empty(),
        "associated assembly uses durable metadata + RIR only: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_named_destructor_metadata_is_retained_on_the_minted_nominal() {
    use crate::StableDefinitionKind as Kind;
    use rue_air::{
        NominalInstanceKey, SemanticDefinitionToken, SemanticModuleToken, TypeInstanceKey,
    };

    let source = "pub struct Box { value: i32 }\n\
                      drop fn Box(self) {}\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let box_key = durable_decl(&decls, Kind::Struct, "Box").key.clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-destructor-metadata",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                DurableDeclSource::from_declarations(&decls),
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let token = facts
                .register_named_nominal(box_key, file.index(), "Box", Kind::Struct)
                .expect("Box name is admitted");
            let ty = facts
                .resolve_instance_type(&TypeInstanceKey::<
                    SemanticDefinitionToken,
                    SemanticModuleToken,
                >::Nominal(NominalInstanceKey::Named(
                    token,
                )))
                .expect("provider pool mints Box");
            facts.with_type_pool(|pool| {
                pool.struct_def(ty.as_struct().expect("Box is a struct"))
                    .destructor
                    .clone()
            })
        },
    );

    assert_eq!(
        outcome.result.as_deref(),
        Some("Box$m_2erue.__drop"),
        "the destructor-bearing nominal must retain its stable destructor symbol"
    );
}

// ---- RUE-1091 r4b-2: endpoint ProviderFacts coverage ---------------------
//
// These prove `rue_air::ProviderEndpointFacts` (the provider-driven
// realization of the family-1A `BodyEndpointProvider` seam) resolves every
// `TypeInstanceKey` arm the body identity pool supports. The driver REUSES
// the same provider-generic `resolve_instance_type` logic production runs
// (`body_endpoint.rs`), driven over the pool + an overlay token space. The
// durable source (`DurableDeclSource`, shared with the r4b-1 block) is
// built from the production durable declaration set, and each resolution is
// pinned through its index-independent render — never a pool-relative
// index.
//
// Scope landed here: `resolve_instance_type` over primitives, named
// struct/enum (the by-file-name lookup + endpoint token space), builtin `str`
// (builtin classification), and the structural array / `ptr const` / `ptr
// mut` wrappers (P); the three RIR ops (R, thin `BodyRirIndex` delegation);
// the provider-boundary nominal-presence check (C). Deferred with cause
// (pinned, never silently answered wrong): module identity → r4b-3 / the
// flip; generic parameter → r5/r6 substitution; anonymous mint-from-digest
// and well-known `Option` → r6; builtin / slice names beyond the pool's
// pre-registered `BUILTIN_ENUMS` + `str` set → r6; the `(StructId, name)`
// endpoint-trait seam → r4b-3.

#[test]
fn provider_endpoint_facts_resolve_instance_type_mints_the_declared_surface() {
    use crate::StableDefinitionKind as Kind;
    use rue_air::{
        NominalInstanceKey as N, SemanticDefinitionToken as DTok, SemanticModuleToken as MTok,
        TypeInstanceKey as T,
    };
    // The full nominal / structural surface: `Point` is a non-copy nominal
    // (its fields resolve through the pool's 2a machinery); `Holder` embeds a
    // nominal field plus the array / `ptr const` / `ptr mut` structural arms;
    // `Color` is a named enum. Each is minted by the provider path and its
    // index-independent render is pinned against the declared source shape.
    let source = "pub struct Point { x: i64, y: i64 }\n\
                      pub enum Color { Red, Green }\n\
                      pub struct Holder { p: Point, arr: [i64; 3], pc: ptr const Point, pm: ptr mut i64 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let point_key = durable_decl(&decls, Kind::Struct, "Point").key.clone();
    let holder_key = durable_decl(&decls, Kind::Struct, "Holder").key.clone();
    let color_key = durable_decl(&decls, Kind::Enum, "Color").key.clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let merged = &stages.merged;
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let durable_module = merged.ast().modules()[0].module_id().clone();
    let conflicting_durable_module =
        crate::ModuleId::from_logical_path("other.rue").expect("second durable module id");

    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&decls);
    let call_adapter = DurableDeclSource::from_declarations(&decls);
    let aggregate_adapter = DurableDeclSource::from_declarations(&decls);
    let durable_module_for_fact = durable_module.clone();

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-resolve",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let named = |token: DTok| -> T<DTok, MTok> { T::Nominal(N::Named(token)) };
            let point_token = facts
                .register_named_nominal(point_key.clone(), 1, "Point", Kind::Struct)
                .expect("Point name is admitted");
            let holder_token = facts
                .register_named_nominal(holder_key.clone(), 1, "Holder", Kind::Struct)
                .expect("Holder name is admitted");
            let color_token = facts
                .register_named_nominal(color_key.clone(), 1, "Color", Kind::Enum)
                .expect("Color name is admitted");

            let point_ty = facts
                .resolve_instance_type(&named(point_token))
                .expect("Point resolves through the provider path");
            // Double consult: the pool dedups, resolution is stable.
            let point_again = facts
                .resolve_instance_type(&named(point_token))
                .expect("repeat consult resolves");
            assert_eq!(
                point_ty, point_again,
                "repeat consult re-minted the nominal"
            );
            let holder_ty = facts
                .resolve_instance_type(&named(holder_token))
                .expect("Holder resolves (nominal field + structural fields)");
            let color_ty = facts
                .resolve_instance_type(&named(color_token))
                .expect("Color resolves through the provider path");

            // Top-level structural / primitive arms of the SHARED
            // `resolve_instance_type` walk.
            let array_ty = facts
                .resolve_instance_type(&T::Array {
                    element: Node::new(T::I64),
                    len: 3,
                })
                .expect("array arm resolves");
            let ptr_const_ty = facts
                .resolve_instance_type(&T::PtrConst(Node::new(named(point_token))))
                .expect("ptr const arm resolves over a nominal");
            let ptr_mut_ty = facts
                .resolve_instance_type(&T::PtrMut(Node::new(T::I64)))
                .expect("ptr mut arm resolves");
            let i64_ty = facts
                .resolve_instance_type(&T::I64)
                .expect("primitive arm resolves");
            let str_ty = facts
                .resolve_instance_type(&T::BuiltinNominal {
                    kind: rue_air::AnonymousNominalKind::Struct,
                    name: std::sync::Arc::from("str"),
                })
                .expect("builtin str resolves through the pool's pre-registered set");

            let module_import_path = durable_module.logical_path().to_owned();
            let module_token = facts
                .register_module(
                    durable_module.clone(),
                    file,
                    "/m.rue",
                    &module_import_path,
                    &module_import_path,
                )
                .expect("durable module registration is consistent");
            assert_eq!(
                facts.register_module(
                    durable_module.clone(),
                    file,
                    "/m.rue",
                    &module_import_path,
                    &module_import_path,
                ),
                Some(module_token),
                "repeat durable module registration dedups"
            );
            assert!(
                facts
                    .register_module(
                        durable_module.clone(),
                        FileId::new(2),
                        "/other.rue",
                        &module_import_path,
                        &module_import_path,
                    )
                    .is_none(),
                "one durable module cannot acquire a conflicting file"
            );
            assert!(
                facts
                    .register_module(
                        conflicting_durable_module,
                        file,
                        "/other.rue",
                        "other.rue",
                        "other.rue",
                    )
                    .is_none(),
                "a second durable module cannot claim an already-registered file"
            );
            let module_ty = facts
                .resolve_instance_type(&T::Module(module_token))
                .expect("module arm resolves through provider module facts");
            let module_file = facts
                .module_file(module_ty)
                .expect("provider module type reverses to its current file");

            let endpoint_render = facts.with_type_pool(|pool| {
                (
                    endpoint_nominal_render(pool, point_ty),
                    endpoint_nominal_render(pool, holder_ty),
                    endpoint_nominal_render(pool, color_ty),
                    endpoint_display(pool, array_ty),
                    endpoint_display(pool, ptr_const_ty),
                    endpoint_display(pool, ptr_mut_ty),
                    endpoint_display(pool, i64_ty),
                    endpoint_display(pool, str_ty),
                    module_file,
                )
            });

            let call_facts = rue_air::ProviderCallFacts::new(
                provider,
                call_adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let call_module = call_facts
                .register_module(
                    durable_module.clone(),
                    file,
                    "/m.rue",
                    &module_import_path,
                    &module_import_path,
                )
                .expect("call driver registers durable module facts");
            let call_module_fact = call_facts
                .module_def(call_module)
                .map(|definition| {
                    (
                        definition.file_id,
                        definition.file_path,
                        definition.import_path,
                        definition.durable_id,
                    )
                })
                .expect("call driver answers module_def");

            let aggregate_facts = rue_air::ProviderAggregateFacts::new(aggregate_adapter);
            let aggregate_module = aggregate_facts
                .register_module(
                    durable_module,
                    file,
                    "/m.rue",
                    &module_import_path,
                    &module_import_path,
                )
                .expect("aggregate driver registers durable module facts");
            let aggregate_module_fact = aggregate_facts
                .module_fact(aggregate_module)
                .expect("aggregate driver answers module facts");

            (endpoint_render, call_module_fact, aggregate_module_fact)
        },
    );
    let (endpoint_render, call_module_fact, aggregate_module_fact) = outcome.result;
    let (
        point_r,
        holder_r,
        color_r,
        array_d,
        ptr_const_d,
        ptr_mut_d,
        i64_d,
        str_d,
        provider_module_file,
    ) = endpoint_render;

    // Named-nominal arms: the full index-independent render is pinned to the
    // declared source shape.
    assert_eq!(
        point_r,
        EndpointNominalRender {
            display: "Point".to_owned(),
            is_copy: false,
            is_pub: true,
            symbol: "Point$m_2erue".to_owned(),
            members: vec![
                ("x".to_owned(), "i64".to_owned()),
                ("y".to_owned(), "i64".to_owned()),
            ],
        },
        "Point resolution renders the declared struct"
    );
    assert_eq!(
        holder_r,
        EndpointNominalRender {
            display: "Holder".to_owned(),
            is_copy: false,
            is_pub: true,
            symbol: "Holder$m_2erue".to_owned(),
            members: vec![
                ("p".to_owned(), "Point".to_owned()),
                ("arr".to_owned(), "[i64; 3]".to_owned()),
                ("pc".to_owned(), "ptr const Point".to_owned()),
                ("pm".to_owned(), "ptr mut i64".to_owned()),
            ],
        },
        "Holder (nominal + structural fields) renders the declared struct"
    );
    assert_eq!(
        color_r,
        EndpointNominalRender {
            display: "Color".to_owned(),
            is_copy: true,
            is_pub: true,
            symbol: "Color$m_2erue".to_owned(),
            members: vec![
                ("Red".to_owned(), String::new()),
                ("Green".to_owned(), String::new()),
            ],
        },
        "Color enum renders the declared variants"
    );

    // Structural / primitive arms of the SHARED `resolve_instance_type`
    // walk render their canonical spellings.
    assert_eq!(array_d, "[i64; 3]", "array arm renders the declared array");
    assert_eq!(ptr_const_d, "ptr const Point");
    assert_eq!(ptr_mut_d, "ptr mut i64");
    assert_eq!(i64_d, "i64", "primitive arm renders directly");
    // Builtin arm: `str` is pre-registered in the pool.
    assert_eq!(
        str_d, "str",
        "builtin str renders as the pre-registered nominal"
    );
    assert_eq!(
        provider_module_file, file,
        "module endpoint + registry resolution reverses to the registered file"
    );
    assert_eq!(
        call_module_fact,
        (
            file,
            "/m.rue".to_owned(),
            "m.rue".to_owned(),
            durable_module_for_fact.as_str().to_owned(),
        ),
        "call module_def carries the registered module facts"
    );
    assert_eq!(
        aggregate_module_fact,
        (file, "/m.rue".to_owned(), "m.rue".to_owned()),
        "aggregate module facts carry the registered paths"
    );

    // The P-op path consults the pool (durable source) + overlay, not the
    // live provider terminals, so it records no provider query edge — edge
    // honesty is a C-op property (pinned by the presence differential below).
    assert!(
        outcome.dependencies.is_empty(),
        "a pool-answered resolution records no provider edge: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_endpoint_facts_deferred_arms_are_pinned_gaps() {
    use crate::StableDefinitionKind as Kind;
    use rue_air::{
        AnonymousNominalKey, AnonymousNominalKind, NominalInstanceKey as N,
        SemanticModuleToken as MTok, StableProducerId, TypeInstanceKey as T,
    };
    // A one-nominal program so the anonymous producer has a real definition
    // token to name; every arm below is a documented pool deferral that must
    // fail closed (never resolve wrong).
    let source = "pub struct Point { x: i64 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let point_key = durable_decl(&decls, Kind::Struct, "Point").key.clone();
    let (rir_out, _semantic, _) =
        crate::test_support::test_frontend_snapshot(&snapshot, &crate::CompileOptions::default())
            .expect("frontend compiles");
    let rir = rir_out.rir();
    let interner = rir_out.semantic_symbols().interner();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-deferred",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir, interner),
            );
            let point_token = facts
                .register_named_nominal(point_key.clone(), file.index(), "Point", Kind::Struct)
                .expect("Point name is admitted");

            // Module identity — r4b-3 / the flip (pool-refused arm).
            let module = facts.resolve_instance_type(&T::Module(MTok::new(0, 0)));
            // Generic parameter — r5 substitution.
            let generic = facts.resolve_instance_type(&T::GenericParameter(0));
            // A slice whose generated struct was NOT seeded still fails closed:
            // the r6a `Slice` arm resolves only AFTER `register_generated_slice`
            // (positive differential in
            // `provider_endpoint_facts_slice_arm_resolves_after_registration`).
            let slice = facts.resolve_instance_type(&T::Slice {
                element: Node::new(T::I64),
                name: std::sync::Arc::from("[]i64"),
            });
            // A genuine non-builtin name (not any builtin under any regime)
            // fails closed — a permanent gap, not an r6 deferral.
            let unknown_builtin = facts.resolve_instance_type(&T::BuiltinNominal {
                kind: AnonymousNominalKind::Struct,
                name: std::sync::Arc::from("NotABuiltin"),
            });
            // An UNSEEDED anonymous key fails closed: the r6b arm mints only
            // for a durable identity seeded by `register_anonymous_nominal`
            // (the positive differential is
            // `provider_endpoint_facts_anonymous_arm_mints_after_registration`),
            // exactly as the unseeded `Slice` arm above fails closed. The pool
            // never invents an anonymous identity.
            let anonymous = facts.resolve_instance_type(&T::Nominal(N::Anonymous(Node::new(
                AnonymousNominalKey {
                    kind: AnonymousNominalKind::Struct,
                    producer: StableProducerId::Definition(point_token),
                    anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
                },
            ))));
            (
                module.is_err(),
                generic.is_err(),
                slice.is_err(),
                unknown_builtin.is_err(),
                anonymous.is_err(),
            )
        },
    );
    let (module, generic, slice, unknown_builtin, anonymous) = outcome.result;
    assert!(module, "module identity fails closed (r4b-3 / flip)");
    assert!(generic, "generic parameter fails closed (r5)");
    assert!(
        slice,
        "an unseeded slice generated-struct name fails closed"
    );
    assert!(
        unknown_builtin,
        "a non-builtin name fails closed (permanent)"
    );
    assert!(
        anonymous,
        "an unseeded anonymous key fails closed (the seeded mint is the r6b positive differential)"
    );
}

// RUE-1091 r6b: the anonymous arm mints once a caller seeds the durable
// identity — the positive half of the deferral this slice flips (the r4b-2
// anonymous-arm pin). The pool relocates the durable producer key to its
// stable content, canonicalizes the producer wrapper on entry, and spells
// the `__anon_struct_{digest}` name; the render below pins that full
// materialization (digest name, symbol, flags, and field vocabulary).
#[test]
fn provider_endpoint_facts_anonymous_arm_mints_after_registration() {
    use rue_air::{
        AnonymousNominalKey, AnonymousNominalKind, NominalInstanceKey as N,
        SemanticDefinitionToken as DTok, StableProducerId, TypeInstanceKey as T,
    };
    // `Holder`'s field `p: Pair()` forces the epoch to instantiate the
    // comptime type function `Pair` at declaration bind, minting the anonymous
    // `struct { a: i32 }` whose producer roots at the INSTALLED function `Pair`
    // (an installed-endpoint producer — the pool's byte-equal minting scope).
    let source = "fn Pair() -> type { struct { a: i32 } }\n\
                      struct Holder { p: Pair() }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);

    // Independently produce the durable declaration set + the durable
    // anonymous nominal (the pool's inputs) through the nucleus projection.
    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let merged = &stages.merged;
    let mut proj_db = RevisionedQueryDatabase::default();
    let proj_revision = revision_for(&mut proj_db, &snapshot);
    let projection = proj_db
        .projected_declaration_semantics(
            proj_revision,
            merged.ast(),
            rue_target::Target::X86_64Linux,
            &crate::PreviewFeatures::default(),
            CancellationToken::new(),
        )
        .expect("declaration semantics project");
    assert_eq!(
        projection.anonymous_nominals.len(),
        1,
        "the program mints exactly one anonymous nominal"
    );
    // The durable identity is fed to the pool RAW: the declaration-SIGNATURE
    // projection retains the empty-argument specialization wrapper
    // (`Function(Specialization { base, args: [] })`) that production
    // body-export collapses to `Function(base)`
    // (`canonical_function_producer`). The pool canonicalizes ON ENTRY
    // (`find_or_create_anon` collapses via `with_canonical_producer`, and the
    // adapter keys shapes canonically), so handing the non-collapsed form
    // must dedup onto — and spell the digest of — the collapsed form. This is
    // the entry-canonicalization proof, not a de-quirked input.
    let durable_identity = projection.anonymous_nominals[0].identity.clone();

    let rir = &stages.rir;
    let rir_ref = rir.rir();
    let interner = rir.semantic_symbols().interner();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&projection.declarations)
        .with_anonymous_nominals(&projection.anonymous_nominals);
    let identity_for_probe = durable_identity.clone();

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-anon-mint",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            // Direct pool mint (the keystone): mint the anonymous nominal from
            // its durable identity + shape.
            let minted = facts
                .mint_anonymous(&identity_for_probe)
                .expect("the pool mints the anonymous nominal");
            // Idempotency: a repeat consult re-mints nothing.
            let again = facts
                .mint_anonymous(&identity_for_probe)
                .expect("repeat consult re-resolves");
            assert_eq!(minted, again, "the pool re-minted the anonymous nominal");
            // Entry canonicalization: the RAW projected identity (with its
            // empty-argument specialization wrapper) and its collapsed
            // canonical-producer form dedup onto the same minted nominal.
            let collapsed = identity_for_probe.with_canonical_producer().into_owned();
            let canonical_mint = facts
                .mint_anonymous(&collapsed)
                .expect("the collapsed identity resolves by dedup");
            assert_eq!(
                minted, canonical_mint,
                "the RAW identity must collapse onto the canonical producer form"
            );

            // The resolve_instance_type anonymous arm: seed the issued→durable
            // map, then resolve an issued-domain anonymous key — the r4b-2
            // anonymous-arm flip. The issued key is an arbitrary lookup handle;
            // the durable key drives the mint / digest.
            let issued = AnonymousNominalKey {
                kind: AnonymousNominalKind::Struct,
                producer: StableProducerId::Definition(DTok::new(0x5b, 1)),
                anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
            };
            facts.register_anonymous_nominal(issued.clone(), identity_for_probe.clone());
            let via_arm = facts
                .resolve_instance_type(&T::Nominal(N::Anonymous(Node::new(issued))))
                .expect("the seeded anonymous arm resolves");
            assert_eq!(via_arm, minted, "the arm and direct mint agree");

            facts.with_type_pool(|pool| endpoint_nominal_render(pool, minted))
        },
    );
    let pool_render = outcome.result;

    // The full materialization is pinned: the `__anon_struct_{digest}` name
    // (stable digest over the durable identity), copyability, visibility,
    // mangled symbol, and field vocabulary.
    assert!(
        pool_render.display.starts_with("__anon_struct_"),
        "the pool spells the digest name: {}",
        pool_render.display
    );
    assert_eq!(
        pool_render.members,
        vec![("a".to_owned(), "i32".to_owned())],
        "the anonymous struct retains the produced field vocabulary"
    );
    assert_eq!(
        pool_render.symbol, "__anon_struct_5451c1711507279538bfbd6f415d97aa",
        "the mangled symbol is the stable digest spelling"
    );
    assert!(!pool_render.is_pub, "an anonymous nominal is not `pub`");
    assert!(
        pool_render.is_copy,
        "a single-`i32` anonymous struct is copyable"
    );
}

// RUE-1091 r6b: the ENUM analog of the anonymous mint. The pool mints
// through `mint_anon_enum` from the durable shape, spelling the
// `__anon_enum_{digest}` bare source symbol. The pool is fed the RAW
// projected identity (empty-argument specialization wrapper
// retained), so the enum path exercises entry canonicalization too.
#[test]
fn provider_endpoint_facts_anonymous_enum_mints_from_durable_identity() {
    use rue_air::{
        AnonymousNominalKey, AnonymousNominalKind, NominalInstanceKey as N,
        SemanticDefinitionToken as DTok, StableProducerId, TypeInstanceKey as T,
    };
    // `Holder`'s field `o: Wrap()` forces the epoch to instantiate the
    // comptime type function `Wrap` at declaration bind, minting the anonymous
    // `enum { Some(i32), None }` whose producer roots at the INSTALLED
    // function `Wrap`.
    let source = "fn Wrap() -> type { enum { Some(i32), None } }\n\
                      struct Holder { o: Wrap() }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);

    // Independently produce the durable declaration set + the durable
    // anonymous nominal (the pool's inputs) through the nucleus projection.
    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let merged = &stages.merged;
    let mut proj_db = RevisionedQueryDatabase::default();
    let proj_revision = revision_for(&mut proj_db, &snapshot);
    let projection = proj_db
        .projected_declaration_semantics(
            proj_revision,
            merged.ast(),
            rue_target::Target::X86_64Linux,
            &crate::PreviewFeatures::default(),
            CancellationToken::new(),
        )
        .expect("declaration semantics project");
    assert_eq!(
        projection.anonymous_nominals.len(),
        1,
        "the program mints exactly one anonymous nominal"
    );
    assert!(
        matches!(
            projection.anonymous_nominals[0].shape,
            crate::durable_semantics::DurableAnonymousNominalShape::Enum { .. }
        ),
        "the projected anonymous nominal is an enum"
    );
    // RAW identity — the wrapper collapse is the pool's entry obligation.
    let durable_identity = projection.anonymous_nominals[0].identity.clone();
    let durable_source_symbol = projection.anonymous_nominals[0].source_symbol().clone();
    let durable_drop_glue_symbol = crate::local_semantic_materialization::rooted_callable_symbol(
        &crate::FunctionInstanceKey::DropGlue(Node::new(crate::TypeInstanceKey::Nominal(
            crate::NominalInstanceKey::Anonymous(Node::new(durable_identity.clone())),
        ))),
    );

    let rir = &stages.rir;
    let rir_ref = rir.rir();
    let interner = rir.semantic_symbols().interner();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&projection.declarations)
        .with_anonymous_nominals(&projection.anonymous_nominals);
    let identity_for_probe = durable_identity.clone();
    let expected_source_symbol = durable_source_symbol.clone();
    let expected_drop_glue_symbol = durable_drop_glue_symbol.clone();

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-anon-enum-mint",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            // Direct pool mint from the RAW durable identity + shape.
            let minted = facts
                .mint_anonymous(&identity_for_probe)
                .expect("the pool mints the anonymous enum");
            // Idempotency: a repeat consult re-mints nothing.
            let again = facts
                .mint_anonymous(&identity_for_probe)
                .expect("repeat consult re-resolves");
            assert_eq!(minted, again, "the pool re-minted the anonymous enum");
            // Entry canonicalization: the RAW projected identity and its
            // collapsed canonical-producer form dedup onto the same mint.
            let collapsed = identity_for_probe.with_canonical_producer().into_owned();
            let canonical_mint = facts
                .mint_anonymous(&collapsed)
                .expect("the collapsed identity resolves by dedup");
            assert_eq!(
                minted, canonical_mint,
                "the RAW identity must collapse onto the canonical producer form"
            );

            // The resolve_instance_type anonymous arm over an issued ENUM key.
            let issued = AnonymousNominalKey {
                kind: AnonymousNominalKind::Enum,
                producer: StableProducerId::Definition(DTok::new(0x5b, 1)),
                anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
            };
            facts.register_anonymous_nominal(issued.clone(), identity_for_probe.clone());
            let via_arm = facts
                .resolve_instance_type(&T::Nominal(N::Anonymous(Node::new(issued))))
                .expect("the seeded anonymous enum arm resolves");
            assert_eq!(via_arm, minted, "the arm and direct mint agree");

            facts.with_type_pool(|pool| {
                let render = endpoint_nominal_render(pool, minted);
                let frozen = pool.clone().freeze();
                let drop_glue = rue_air::drop_glue_names::enum_drop_glue_name(
                    minted.as_enum().expect("the minted nominal is an enum"),
                    &frozen,
                );
                (render, drop_glue)
            })
        },
    );
    let (pool_render, pool_drop_glue_symbol) = outcome.result;

    assert_eq!(
        pool_render.symbol,
        expected_source_symbol.as_ref(),
        "the live pool enum symbol must equal the durable source symbol"
    );
    assert_eq!(
        pool_drop_glue_symbol,
        expected_drop_glue_symbol.as_ref(),
        "the live enum drop glue must equal the durable DropGlue symbol"
    );

    // The full materialization is pinned: the bare `__anon_enum_{digest}`
    // name (stable digest over the durable identity), copyability,
    // visibility, and variant vocabulary.
    assert!(
        pool_render.display.starts_with("__anon_enum_"),
        "the pool spells the enum digest name: {}",
        pool_render.display
    );
    assert_eq!(pool_render.display.len(), "__anon_enum_".len() + 32);
    assert_eq!(
        pool_render
            .members
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["Some", "None"],
        "the anonymous enum retains the produced variant vocabulary"
    );
    assert!(!pool_render.is_pub, "an anonymous nominal is not `pub`");
    assert!(
        pool_render.is_copy,
        "an `i32`-payload anonymous enum is copyable"
    );
}

// RUE-1091 r6c: the well-known `Option` install. The trusted std
// `Option(payload)` specializations a body's fallible intrinsics demand
// (RUE-1112) are minted by the provider-side pool's
// `install_well_known_option_types` through `find_or_create_anon` via the
// real `DurableDeclSource` adapter, starting from the declaration-level
// durable truth (the nucleus `ComptimeCall` terminals the production demand
// loop roots). The full materializations are pinned: the digest-spelled
// `__anon_enum_{digest}` names, copyability, visibility, and variant
// vocabulary.
//
// The export-as-produced ruling: the pool records each installed canonical
// identity under `is_well_known_option_identity`, so the body publication
// path treats those identities as produced by the analyzed body, never as
// pre-existing imports.
#[test]
fn provider_well_known_option_install_mints_the_demanded_payloads() {
    use crate::semantic_query_nucleus::{
        ComptimeCallResultProjection as ResultProjection, SemanticNucleusKey as Key,
        SemanticNucleusValue as V,
    };

    // The freestanding fallible-intrinsic program plus the trusted `Option`
    // module published at its trusted logical path. `main` names
    // `@parse_i64` and `@parse_u32`, so its registered demand node names two
    // payloads and each maps directly to one exact comptime key.
    let root = FileId::new(1);
    let option = FileId::new(2);
    let physical = AHashMap::from([
        (root, "/project/main.rue".to_owned()),
        (option, "/sdk/option.rue".to_owned()),
    ]);
    let logical = AHashMap::from([
        (root, "main.rue".to_owned()),
        (option, crate::OPTION_MODULE_LOGICAL_PATH.to_owned()),
    ]);
    let metadata = SourceMetadata::new_with_trusted_standard_library(
        root,
        physical,
        logical,
        AHashSet::from([option]),
    )
    .unwrap();
    let snapshot = SourceSnapshot::new(
        metadata,
        vec![
            (
                root,
                Arc::new(
                    "fn main() -> i32 { let a = @parse_i64(\"1\"); \
                         let b = @parse_u32(\"2\"); 0 }"
                        .to_owned(),
                ),
            ),
            (
                option,
                Arc::new(
                    "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }".to_owned(),
                ),
            ),
        ],
    )
    .unwrap();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;

    let configuration = semantic_configuration();
    let demands = [
        crate::well_known_option::FalliblePayload::I64,
        crate::well_known_option::FalliblePayload::U32,
    ]
    .map(|kind| crate::well_known_option::exact_option_query(kind, &configuration));

    // Resolve each demand through the nucleus — the declaration-level
    // durable truth BOTH installs consume — assembling the same
    // `WellKnownOptionResolution` the production `body_transaction` builds.
    let mut nucleus_db = RevisionedQueryDatabase::default();
    let nucleus_revision = nucleus_db.source_revision(
        &super::super::session::ExactSourceInput::new(&snapshot),
        &snapshot,
    );
    let mut option_by_payload = Vec::new();
    let mut nominals: BTreeMap<
        crate::AnonymousNominalKey,
        crate::durable_semantics::DurableAnonymousNominal,
    > = BTreeMap::new();
    for (payload, call) in demands {
        let value =
            request_semantic_nucleus(&nucleus_db, nucleus_revision, Key::ComptimeCall(call));
        let V::ComptimeCall(projection) = value else {
            panic!("trusted Option comptime call did not resolve: {value:?}");
        };
        let ResultProjection::Type(option_type) = &projection.result else {
            panic!(
                "Option(payload) must resolve to a type: {:?}",
                projection.result
            );
        };
        option_by_payload.push((payload, option_type.clone()));
        for nominal in projection.anonymous_nominals.iter() {
            nominals.insert(nominal.identity.clone(), nominal.clone());
        }
    }
    let resolution = crate::body_query::WellKnownOptionResolution {
        option_by_payload: Arc::from(option_by_payload),
        anonymous_nominals: Arc::from(nominals.into_values().collect::<Vec<_>>()),
    };
    assert_eq!(
        resolution.anonymous_nominals.len(),
        2,
        "one trusted Option enum per demanded payload"
    );
    assert!(
        resolution.anonymous_nominals.iter().all(|nominal| matches!(
            nominal.shape,
            crate::durable_semantics::DurableAnonymousNominalShape::Enum { .. }
        )),
        "the trusted registry holds enum shapes only"
    );

    // ------------------------------------------------------------------
    // The provider-side pool install: the demanded durable identities and
    // registry pairs, minted through `BodyIdentityPool::
    // install_well_known_option_types` over the real `DurableDeclSource`
    // adapter built from the production durable declarations.
    // ------------------------------------------------------------------
    let decls = production_declarations(&snapshot);
    let adapter = DurableDeclSource::from_declarations(&decls)
        .with_anonymous_nominals(&resolution.anonymous_nominals);
    let identities: Vec<crate::AnonymousNominalKey> = resolution
        .anonymous_nominals
        .iter()
        .map(|nominal| nominal.identity.clone())
        .collect();
    let pairs: Vec<(crate::DurableType, crate::DurableType)> =
        resolution.option_by_payload.iter().cloned().collect();

    let rir_ref = rir.rir();
    let interner = rir.semantic_symbols().interner();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "well-known-option-install",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            facts
                .install_well_known_option_types(&identities, &pairs)
                .expect("the pool installs the well-known registry");
            // Idempotent: a repeat install dedups onto the same identities.
            facts
                .install_well_known_option_types(&identities, &pairs)
                .expect("a repeat install is a pure dedup");

            // The export-as-produced ruling, pool side: each installed
            // identity answers the baseline-subtraction predicate.
            assert_eq!(facts.well_known_option_identity_count(), 2);
            for identity in &identities {
                assert!(
                    facts.is_well_known_option_identity(identity),
                    "an installed identity carries the produced ruling"
                );
            }

            // Materializations, fetched by dedup lookup (nothing re-mints).
            let renders: Vec<EndpointNominalRender> = identities
                .iter()
                .map(|identity| {
                    let ty = facts
                        .mint_anonymous(identity)
                        .expect("an installed identity resolves by dedup");
                    facts.with_type_pool(|pool| endpoint_nominal_render(pool, ty))
                })
                .collect();
            let i64_option = facts
                .well_known_option_for_payload(rue_air::Type::I64)
                .expect("the pool registry answers i64");
            let u32_option = facts
                .well_known_option_for_payload(rue_air::Type::U32)
                .expect("the pool registry answers u32");
            let i64_render = facts.with_type_pool(|pool| endpoint_nominal_render(pool, i64_option));
            let u32_render = facts.with_type_pool(|pool| endpoint_nominal_render(pool, u32_option));
            (renders, i64_render, u32_render)
        },
    );
    let (mut pool_renders, pool_i64_render, pool_u32_render) = outcome.result;
    pool_renders.sort_by(|a, b| a.display.cmp(&b.display));

    // The install materialized exactly the demanded Option enums: digest
    // names, variant vocabulary, copyability, and visibility are pinned.
    for render in &pool_renders {
        assert!(
            render.display.starts_with("__anon_enum_"),
            "the pool spells the digest name: {}",
            render.display
        );
        assert_eq!(
            render
                .members
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["Some", "None"],
            "each Option enum retains the trusted variant vocabulary"
        );
        assert!(!render.is_pub, "an anonymous nominal is not `pub`");
        assert!(render.is_copy, "an integer-payload Option enum is copyable");
    }
    assert_eq!(pool_i64_render.display.len(), "__anon_enum_".len() + 32);
    assert_eq!(pool_u32_render.display.len(), "__anon_enum_".len() + 32);
    let mut registry_renders = vec![pool_i64_render, pool_u32_render];
    registry_renders.sort_by(|a, b| a.display.cmp(&b.display));
    assert_eq!(
        pool_renders, registry_renders,
        "the demand registry answers with the exact installed materializations"
    );
}

// RUE-1091 r6a: the `Slice` arm resolves once a caller seeds the generated
// slice struct with `register_generated_slice`, minting the fat-pointer
// struct — the positive half of the deferral this slice flips.
#[test]
fn provider_endpoint_facts_slice_arm_resolves_after_registration() {
    use rue_air::{SemanticImportType as D, TypeInstanceKey as T};
    // The signature slice `[i64]` names the generated slice struct the pool
    // mints (ADR-0043).
    let source = "fn take(s: [i64]) -> i64 { 0 }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    // The slice mint needs no durable nominals (its element is a primitive),
    // so an empty durable source suffices.
    let adapter = DurableDeclSource::from_declarations(&[]);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-slice",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            // Seed the generated slice, then resolve the `Slice` arm.
            facts
                .register_generated_slice(&D::I64, "[i64]")
                .expect("register mints the slice struct");
            let key = T::Slice {
                element: Node::new(T::I64),
                name: std::sync::Arc::from("[i64]"),
            };
            let first = facts.resolve_instance_type(&key).expect("slice resolves");
            // Idempotency: a repeat consult returns the same id.
            let second = facts
                .resolve_instance_type(&key)
                .expect("slice re-resolves");
            assert_eq!(first, second, "repeat slice resolution diverged");
            facts.with_type_pool(|pool| endpoint_nominal_render(pool, first))
        },
    );
    // The provider-minted slice renders the generated fat-pointer struct
    // (name, copyability, visibility, symbol, fields).
    assert_eq!(
        outcome.result,
        EndpointNominalRender {
            display: "[i64]".to_owned(),
            is_copy: true,
            is_pub: true,
            symbol: "[i64]".to_owned(),
            members: vec![
                ("ptr".to_owned(), "ptr const i64".to_owned()),
                ("len".to_owned(), "u64".to_owned()),
            ],
        },
        "the generated `[i64]` slice struct materialization is pinned"
    );
    // A pool-answered materialization records no provider query edge (edge
    // honesty — the slice identity is minted, not a boundary lookup).
    assert!(
        outcome.dependencies.is_empty(),
        "the seeded slice resolution records no provider edge: {:?}",
        outcome.dependencies
    );
}

#[test]
fn provider_endpoint_facts_rir_ops_and_nominal_presence() {
    // Two structs sharing a method name, a destructor, and a free function:
    // the three RIR ops must disambiguate by the owner preimage, and the
    // provider-boundary presence check must kind-filter nominals from
    // functions.
    let source = "struct Widget { id: u32, \
             fn bump(self) -> u32 { self.id } \
             fn reset() -> u32 { 0 } }\n\
             struct Gadget { n: i32, \
             fn bump(self) -> i32 { self.n } }\n\
             drop fn Widget(self) {}\n\
             fn helper() -> i32 { 0 }\n\
             fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let decls = production_declarations(&snapshot);
    let (rir_out, _semantic, _) =
        crate::test_support::test_frontend_snapshot(&snapshot, &crate::CompileOptions::default())
            .expect("frontend compiles");
    let rir = rir_out.rir();
    let interner = rir_out.semantic_symbols().interner();

    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let adapter = DurableDeclSource::from_declarations(&decls);

    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-rir-ops",
        move |provider| {
            let facts = rue_air::ProviderEndpointFacts::new(
                provider,
                adapter,
                rue_air::BodyRirView::from_parts(rir, interner),
            );

            // (R) first_free_function: a free function resolves; a method
            // name and an absent name do not.
            let helper = facts.first_free_function("helper", file);
            let bump_free = facts.first_free_function("bump", file);
            let absent_free = facts.first_free_function("nonexistent", file);

            // (R) named_method_declaration: same-named methods on distinct
            // owners stay distinct; an absent method fails closed.
            let widget_bump = facts.named_method_declaration(file, "Widget", "bump");
            let gadget_bump = facts.named_method_declaration(file, "Gadget", "bump");
            let widget_reset = facts.named_method_declaration(file, "Widget", "reset");
            let widget_absent = facts.named_method_declaration(file, "Widget", "nonexistent");

            // (R) destructor: present for Widget, absent for Gadget.
            let widget_drop = facts.destructor(file, "Widget");
            let gadget_drop = facts.destructor(file, "Gadget");

            // (C) nominal presence via the provider boundary (records the
            // lookup-name edge): a struct is present, a function is not (kind
            // filter), an absent name is not.
            let widget_present = facts.nominal_contains_in_module(
                &m,
                "Widget",
                rue_air::AnonymousNominalKind::Struct,
            );
            let helper_as_struct = facts.nominal_contains_in_module(
                &m,
                "helper",
                rue_air::AnonymousNominalKind::Struct,
            );
            let missing_present = facts.nominal_contains_in_module(
                &m,
                "Missing",
                rue_air::AnonymousNominalKind::Struct,
            );

            (
                helper.is_some(),
                bump_free.is_none(),
                absent_free.is_none(),
                widget_bump,
                gadget_bump,
                widget_reset.is_some(),
                widget_absent.is_none(),
                widget_drop.is_some(),
                gadget_drop.is_none(),
                widget_present,
                helper_as_struct,
                missing_present,
            )
        },
    );
    let (
        helper,
        bump_not_free,
        absent_free,
        widget_bump,
        gadget_bump,
        widget_reset,
        widget_absent,
        widget_drop,
        gadget_no_drop,
        widget_present,
        helper_as_struct,
        missing_present,
    ) = outcome.result;
    assert!(helper, "helper is a free function");
    assert!(bump_not_free, "bump is a method, not a free function");
    assert!(absent_free, "an absent free function fails closed");
    assert!(widget_bump.is_some() && gadget_bump.is_some());
    assert_ne!(
        widget_bump, gadget_bump,
        "same-named methods on distinct owners stay distinct declarations"
    );
    assert!(widget_reset, "Widget.reset resolves");
    assert!(widget_absent, "an absent method fails closed");
    assert!(widget_drop, "Widget has a destructor");
    assert!(gadget_no_drop, "Gadget has no destructor");
    assert!(widget_present, "Widget is a declared struct");
    assert!(!helper_as_struct, "a free function is not a struct");
    assert!(!missing_present, "an absent name is not a struct");

    // The presence check observes the provider's name-lookup terminal — the
    // post-flip edge truth the epoch's table lookup masks.
    assert!(
        outcome
            .dependencies
            .iter()
            .any(|node| node.family() == "compiler.lookup-name"),
        "nominal presence observes the name-lookup terminal: {:?}",
        outcome.dependencies
    );
}

// ---- RUE-1091 flip-prep: const identity differential --------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConstInfoRender {
    is_pub: bool,
    ty: String,
    value: String,
    span: rue_span::Span,
}

fn render_const_info(
    info: &rue_air::ConstInfo,
    pool: &rue_air::TypeInternPool,
    resolve_symbol: impl Fn(lasso::Spur) -> String,
) -> ConstInfoRender {
    use rue_air::ConstValue as V;
    let value = match info.value {
        V::Integer(value) => format!("integer:{value}"),
        V::Bool(value) => format!("bool:{value}"),
        V::Type(value) => format!("type:{}", endpoint_display(pool, value)),
        V::Function(value) => format!("function:{}", resolve_symbol(value.spur())),
        V::Unit => "unit".to_owned(),
        V::String(value) => format!("string:{}", resolve_symbol(value.spur())),
    };
    ConstInfoRender {
        is_pub: info.is_pub,
        ty: endpoint_display(pool, info.ty),
        value,
        span: info.span,
    }
}

#[test]
fn provider_const_info_assembly_composes_durable_truth_with_exact_spans() {
    use crate::StableDefinitionKind as Kind;

    // Exercise scalar, nominal type-valued, function-valued, and string
    // constants plus a module binding joined through the shared provider
    // module registry.
    let root = "pub struct Point { x: i32 }\n\
                    fn helper() -> i32 { 1 }\n\
                    pub const LIMIT: i64 = 7;\n\
                    const POINT_KIND: type = Point;\n\
                    const ALIAS = helper;\n\
                    const TEXT: str = \"hello\";\n\
                    const dep = @import(\"dep.rue\");\n\
                    fn main() -> i32 { 0 }\n";
    let dep = "pub const DEP_VALUE: i32 = 9;\n";
    let snapshot = source_snapshot(
        &[
            (1, "/project/main.rue", "main.rue", root),
            (2, "/project/dep.rue", "dep.rue", dep),
        ],
        1,
    );
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let value_keys = ["LIMIT", "POINT_KIND", "ALIAS", "TEXT"].map(|name| {
        (
            name,
            durable_decl(&decls, Kind::ValueConst, name).key.clone(),
        )
    });
    let module_target = match &durable_decl(&decls, Kind::ModuleBinding, "dep").payload {
        crate::durable_semantics::DurableDeclarationPayload::ModuleBinding { target } => {
            target.clone()
        }
        _ => unreachable!(),
    };

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    // Expected declaration spans, derived from the fixture text.
    let span_of = |text: &str| {
        let start = u32::try_from(root.find(text).unwrap()).unwrap();
        rue_span::Span::with_file(file, start, start + u32::try_from(text.len()).unwrap())
    };

    // Pool side: the production durable declaration adapter plus the real
    // ProviderEndpointFacts registration primitive, which composes the
    // exact `(file, name)` RIR handle with the durable const record.
    let rir_ref = rir.rir();
    let adapter = DurableDeclSource::from_declarations(&decls);
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "endpoint-const-info",
        move |provider| {
            let identity = rue_air::ProviderIdentityContext::new(adapter);
            let rir_view = rue_air::BodyRirView::from_parts(rir_ref, interner);
            let facts = rue_air::ProviderEndpointFacts::with_identity(
                provider,
                identity.clone(),
                rir_view.clone(),
            );
            let calls =
                rue_air::ProviderCallFacts::with_identity(provider, identity.clone(), rir_view);
            let aggregate = rue_air::ProviderAggregateFacts::with_identity(identity);
            let mut rendered = Vec::new();
            for (name, key) in value_keys {
                let info = facts
                    .const_info(&key, file, name)
                    .unwrap_or_else(|| panic!("pool resolves {name}"));
                let again = facts
                    .const_info(&key, file, name)
                    .unwrap_or_else(|| panic!("pool re-resolves {name}"));
                let first = facts.with_type_pool(|pool| {
                    render_const_info(&info, pool, |symbol| facts.resolve_const_symbol(symbol))
                });
                let second = facts.with_type_pool(|pool| {
                    render_const_info(&again, pool, |symbol| facts.resolve_const_symbol(symbol))
                });
                assert_eq!(first, second, "repeat consult re-minted {name}");
                calls.register_value_const(file, name, info.clone());
                aggregate.register_value_const(file, name, info);
                assert!(calls.value_const(file, name).is_some());
                assert!(matches!(
                    aggregate.select_module_type_member(file, name),
                    rue_air::ProviderModuleMember::Const
                ));
                let aggregate_info = aggregate
                    .value_const(file, name)
                    .expect("aggregate preserves the assembled const");
                let aggregate_render = aggregate.with_type_pool(|pool| {
                    render_const_info(&aggregate_info, pool, |symbol| {
                        facts.resolve_const_symbol(symbol)
                    })
                });
                assert_eq!(
                    aggregate_render, first,
                    "aggregate overlay changed the provider-assembled ConstInfo"
                );
                rendered.push((name, first));
            }

            facts
                .register_module(
                    module_target.clone(),
                    FileId::new(2),
                    "/project/dep.rue",
                    module_target.logical_path(),
                    module_target.logical_path(),
                )
                .expect("target module registers in the shared identity context");
            let module = facts
                .module_binding_info(file, "dep", &module_target, false)
                .expect("module binding joins its durable target to the registry");
            calls.register_module_binding(file, "dep", module.clone());
            aggregate.register_module_binding(file, "dep", module.clone());
            assert!(calls.module_binding(file, "dep").is_some());
            assert!(matches!(
                aggregate.select_module_type_member(file, "dep"),
                rue_air::ProviderModuleMember::Const
            ));
            let aggregate_module = aggregate
                .module_binding(file, "dep")
                .expect("aggregate preserves the assembled module binding");
            let module = facts.with_type_pool(|pool| {
                render_const_info(&module, pool, |symbol| facts.resolve_const_symbol(symbol))
            });
            let aggregate_module = aggregate.with_type_pool(|pool| {
                render_const_info(&aggregate_module, pool, |symbol| {
                    facts.resolve_const_symbol(symbol)
                })
            });
            assert_eq!(
                aggregate_module, module,
                "aggregate overlay changed the provider-assembled module ConstInfo"
            );
            (rendered, module)
        },
    );
    let expected = vec![
        (
            "LIMIT",
            ConstInfoRender {
                is_pub: true,
                ty: "i64".to_owned(),
                value: "integer:7".to_owned(),
                span: span_of("pub const LIMIT: i64 = 7;"),
            },
        ),
        (
            "POINT_KIND",
            ConstInfoRender {
                is_pub: false,
                ty: "type".to_owned(),
                value: "type:Point".to_owned(),
                span: span_of("const POINT_KIND: type = Point;"),
            },
        ),
        (
            "ALIAS",
            ConstInfoRender {
                is_pub: false,
                ty: "type".to_owned(),
                value: "function:helper".to_owned(),
                span: span_of("const ALIAS = helper;"),
            },
        ),
        (
            "TEXT",
            ConstInfoRender {
                is_pub: false,
                ty: "str".to_owned(),
                value: "string:hello".to_owned(),
                span: span_of("const TEXT: str = \"hello\";"),
            },
        ),
    ];
    assert_eq!(
        outcome.result.0, expected,
        "pool-assembled ConstInfo must compose the durable record with the exact RIR span"
    );
    assert_eq!(
        outcome.result.1,
        ConstInfoRender {
            is_pub: false,
            ty: "Module(ModuleId(0))".to_owned(),
            value: "type:Module(ModuleId(0))".to_owned(),
            span: span_of("const dep = @import(\"dep.rue\");"),
        },
        "the module binding joins its durable target to the shared registry"
    );
    assert!(
        outcome.dependencies.is_empty(),
        "const assembly uses durable metadata + RIR only: {:?}",
        outcome.dependencies
    );
}

// ---- RUE-1091 r4b-3: aggregate ProviderFacts coverage --------------------
//
// These prove `rue_air::ProviderAggregateFacts` (the provider-driven
// realization of the family-1D `AggregateFacts` seam) selects the declared
// aggregate/field/variant winner. The selection ORDER lives in the
// provider-generic free functions the driver merely supplies facts to
// (`select_module_type_member`'s struct→enum→const short-circuit,
// `select_qualified_type`'s enum→struct, `select_struct_literal_head`'s
// const→struct→builtin) — the exact r1c candidate order. The driver reuses
// the shared `DurableDeclSource` (the r4b-1 durable set) for its 2a pool;
// each winner is pinned through its index-independent render.
//
// Scope landed here: struct/enum-by-file-name (P, pool mint via the overlay
// reverse), builtins (P, pool pre-registered set), `is_accessible` (O,
// request-local file paths), const overlays, and the body-local module
// registry. The production cutover owns assembling and registering those
// request-local facts.

/// The tag + index-independent display of a [`rue_air::ProviderModuleMember`],
/// rendered through the pool that minted its type.
fn describe_member(
    member: &rue_air::ProviderModuleMember,
    pool: &rue_air::TypeInternPool,
) -> (&'static str, Option<String>) {
    match member {
        rue_air::ProviderModuleMember::Struct(ty) => ("struct", Some(endpoint_display(pool, *ty))),
        rue_air::ProviderModuleMember::Enum(ty) => ("enum", Some(endpoint_display(pool, *ty))),
        rue_air::ProviderModuleMember::Const => ("const", None),
        rue_air::ProviderModuleMember::Absent => ("absent", None),
    }
}

/// The tag + display of a [`rue_air::ProviderQualifiedType`].
fn describe_qualified(
    qualified: &rue_air::ProviderQualifiedType,
    pool: &rue_air::TypeInternPool,
) -> (&'static str, Option<String>) {
    match qualified {
        rue_air::ProviderQualifiedType::Enum(ty) => ("enum", Some(endpoint_display(pool, *ty))),
        rue_air::ProviderQualifiedType::Struct(ty) => ("struct", Some(endpoint_display(pool, *ty))),
        rue_air::ProviderQualifiedType::Absent => ("absent", None),
    }
}

#[test]
fn provider_aggregate_facts_resolve_nominals_and_builtins() {
    use crate::StableDefinitionKind as Kind;
    // A user struct and enum (minted through the pool's 2a machinery via the
    // `(file, name)` overlay reverse) plus the pool's pre-registered builtins.
    let source = "pub struct Point { x: i64, y: i64 }\n\
                      pub enum Color { Red, Green }\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let point_key = durable_decl(&decls, Kind::Struct, "Point").key.clone();
    let color_key = durable_decl(&decls, Kind::Enum, "Color").key.clone();

    let mut facts =
        rue_air::ProviderAggregateFacts::new(DurableDeclSource::from_declarations(&decls));
    facts.register_named_nominal(point_key, file, "Point");
    facts.register_named_nominal(color_key, file, "Color");

    let point = facts.struct_in_file(file, "Point").expect("Point resolves");
    let point_again = facts
        .struct_in_file(file, "Point")
        .expect("repeat resolves");
    assert_eq!(point, point_again, "repeat consult dedups the nominal");
    let color = facts.enum_in_file(file, "Color").expect("Color resolves");
    let str_ty = facts.builtin_struct("str").expect("builtin str resolves");
    let arch_ty = facts
        .builtin_enum("Arch")
        .expect("builtin Arch enum resolves");

    // A struct is not an enum and vice versa (kind-filtered by the id kind).
    assert!(
        facts.enum_in_file(file, "Point").is_none(),
        "Point is not an enum"
    );
    assert!(
        facts.struct_in_file(file, "Color").is_none(),
        "Color is not a struct"
    );
    assert!(
        facts.struct_in_file(file, "Absent").is_none(),
        "absent fails closed"
    );
    assert!(
        facts.builtin_struct("NotABuiltin").is_none(),
        "unknown builtin fails closed"
    );

    facts.with_type_pool(|pool| {
        assert_eq!(endpoint_display(pool, point), "Point");
        assert_eq!(endpoint_display(pool, color), "Color");
        // Builtins are pre-registered in the pool.
        assert_eq!(endpoint_display(pool, str_ty), "str");
        assert_eq!(endpoint_display(pool, arch_ty), "Arch");
    });
}

#[test]
fn provider_aggregate_facts_selection_order_follows_the_candidate_ranking() {
    use crate::StableDefinitionKind as Kind;
    // A struct, an enum, and a value constant sharing one module exercise
    // the struct→enum→const short-circuit.
    let source = "pub struct Point { x: i64 }\n\
                      pub enum Color { Red, Green }\n\
                      pub const LIMIT: i64 = 7;\n\
                      fn main() -> i32 { 0 }\n";
    let snapshot = source_snapshot(&[(1, "/m.rue", "m.rue", source)], 1);
    let file = FileId::new(1);
    let decls = production_declarations(&snapshot);
    let point_key = durable_decl(&decls, Kind::Struct, "Point").key.clone();
    let color_key = durable_decl(&decls, Kind::Enum, "Color").key.clone();
    let limit_key = durable_decl(&decls, Kind::ValueConst, "LIMIT").key.clone();

    let stages = crate::test_support::test_frontend_stages(&snapshot).unwrap();
    let rir = &stages.rir;
    let interner = rir.semantic_symbols().interner();
    let expected_limit_span = {
        let text = "pub const LIMIT: i64 = 7;";
        let start = u32::try_from(source.find(text).unwrap()).unwrap();
        rue_span::Span::with_file(file, start, start + u32::try_from(text.len()).unwrap())
    };

    let rir_ref = rir.rir();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let outcome = database.probe_ready_body_facts(
        revision,
        semantic_configuration(),
        "aggregate-selection-order",
        move |provider| {
            let identity =
                rue_air::ProviderIdentityContext::new(DurableDeclSource::from_declarations(&decls));
            let endpoint = rue_air::ProviderEndpointFacts::with_identity(
                provider,
                identity.clone(),
                rue_air::BodyRirView::from_parts(rir_ref, interner),
            );
            let mut facts = rue_air::ProviderAggregateFacts::with_identity(identity);
            facts.register_named_nominal(point_key, file, "Point");
            facts.register_named_nominal(color_key, file, "Color");
            let limit_info = endpoint
                .const_info(&limit_key, file, "LIMIT")
                .expect("endpoint assembles LIMIT from durable truth + exact RIR span");
            let endpoint_limit_render = endpoint.with_type_pool(|pool| {
                render_const_info(&limit_info, pool, |symbol| {
                    endpoint.resolve_const_symbol(symbol)
                })
            });
            facts.register_value_const(file, "LIMIT", limit_info);
            let aggregate_limit_info = facts
                .value_const(file, "LIMIT")
                .expect("aggregate retains LIMIT's complete ConstInfo");
            let aggregate_limit_render = facts.with_type_pool(|pool| {
                render_const_info(&aggregate_limit_info, pool, |symbol| {
                    endpoint.resolve_const_symbol(symbol)
                })
            });

            // select_module_type_member: struct wins first, enum second,
            // const third, absent last.
            let member_point = facts.select_module_type_member(file, "Point");
            let member_color = facts.select_module_type_member(file, "Color");
            let member_limit = facts.select_module_type_member(file, "LIMIT");
            let member_absent = facts.select_module_type_member(file, "Ghost");
            let qualified_color = facts.select_qualified_type(file, "Color");
            let qualified_point = facts.select_qualified_type(file, "Point");
            let qenum_color = facts.select_qualified_enum(file, "Color");
            let qenum_point = facts.select_qualified_enum(file, "Point");
            let head_point = facts.select_struct_literal_head(file, "Point");

            facts.with_type_pool(|pool| {
                (
                    describe_member(&member_point, pool),
                    describe_member(&member_color, pool),
                    describe_member(&member_limit, pool).0,
                    describe_member(&member_absent, pool).0,
                    describe_qualified(&qualified_color, pool),
                    describe_qualified(&qualified_point, pool),
                    qenum_color.is_some(),
                    qenum_point.is_some(),
                    match head_point {
                        rue_air::ProviderStructHead::Named(ty) => Some(endpoint_display(pool, ty)),
                        _ => None,
                    },
                    endpoint_limit_render,
                    aggregate_limit_render,
                )
            })
        },
    );
    let (
        (mp_tag, mp_disp),
        (mc_tag, mc_disp),
        ml_tag,
        ma_tag,
        (qc_tag, qc_disp),
        (qp_tag, qp_disp),
        qenum_color,
        qenum_point,
        head_point,
        endpoint_limit_render,
        aggregate_limit_render,
    ) = outcome.result;
    assert_eq!(
        endpoint_limit_render,
        ConstInfoRender {
            is_pub: true,
            ty: "i64".to_owned(),
            value: "integer:7".to_owned(),
            span: expected_limit_span,
        },
        "endpoint assembles LIMIT from durable truth plus the exact RIR span"
    );
    assert_eq!(
        aggregate_limit_render, endpoint_limit_render,
        "aggregate const overlay preserves provider-assembled type, value, visibility, and span"
    );

    // select_module_type_member winners: struct, enum, const, absent.
    assert_eq!(mp_tag, "struct");
    assert_eq!(mp_disp.as_deref(), Some("Point"));
    assert_eq!(mc_tag, "enum");
    assert_eq!(mc_disp.as_deref(), Some("Color"));
    assert_eq!(ml_tag, "const", "the provider selects the const member");
    assert_eq!(ma_tag, "absent");

    // Qualified selection: enum→struct order.
    assert_eq!((qc_tag, qc_disp.as_deref()), ("enum", Some("Color")));
    assert_eq!((qp_tag, qp_disp.as_deref()), ("struct", Some("Point")));

    // select_qualified_enum: enum resolves, struct does not.
    assert!(qenum_color, "Color qualified-enum resolves");
    assert!(!qenum_point, "Point is not a qualified enum");

    // select_struct_literal_head: unqualified struct head → Named.
    assert_eq!(head_point.as_deref(), Some("Point"));
}

#[test]
fn provider_aggregate_facts_is_accessible_follows_the_directory_domain() {
    // The visibility domain is the parent directory, so a private item is
    // visible within its own file and from a sibling file, but not across
    // directories; a public item is visible either way. The driver decides
    // from the registered physical paths (a request-local body-query input,
    // not a durable fact), proving the visibility short-circuit.
    let root_src = "pub struct A { x: i32 }\n\
             fn main() -> i32 { 0 }\n";
    let leaf_src = "pub struct B { y: i32 }\n";
    let sibling_src = "pub struct C { z: i32 }\n";
    let root_file = FileId::new(1);
    let leaf_file = FileId::new(2);
    let sibling_file = FileId::new(3);
    let unknown_file = FileId::new(4);
    let metadata = SourceMetadata::new_with_trusted_standard_library(
        root_file,
        AHashMap::from([
            (root_file, "/project/main.rue".to_owned()),
            (leaf_file, "/project/std/leaf.rue".to_owned()),
            (sibling_file, "/project/helper.rue".to_owned()),
        ]),
        AHashMap::from([
            (root_file, "main.rue".to_owned()),
            (leaf_file, "\0rue-std/leaf.rue".to_owned()),
            (sibling_file, "helper.rue".to_owned()),
        ]),
        AHashSet::from([leaf_file]),
    )
    .expect("trusted-std metadata is valid");
    let snapshot = SourceSnapshot::new(
        metadata,
        vec![
            (root_file, Arc::new(root_src.to_owned())),
            (leaf_file, Arc::new(leaf_src.to_owned())),
            (sibling_file, Arc::new(sibling_src.to_owned())),
        ],
    )
    .expect("three-file snapshot is valid");

    let decls = production_declarations(&snapshot);

    // No K-typed argument pins the pool key here (is_accessible is path-only),
    // so name the durable key / module explicitly.
    let mut facts = rue_air::ProviderAggregateFacts::<StableDefinitionKey, ModuleId, _>::new(
        DurableDeclSource::from_declarations(&decls),
    );
    // Register the snapshot's physical paths — the request-local body-query
    // input the visibility short-circuit consults.
    let physical_paths = [
        (root_file, "/project/main.rue"),
        (leaf_file, "/project/std/leaf.rue"),
        (sibling_file, "/project/helper.rue"),
    ];
    for (file, path) in physical_paths {
        facts.register_file_path(file, path);
    }

    // Every combination of (accessing, defining, is_public) follows the
    // parent-directory visibility rule: public is always visible, and a
    // private item is visible exactly when both files share a directory.
    let directory_of = |wanted: FileId| {
        physical_paths
            .iter()
            .find(|(file, _)| *file == wanted)
            .map(|(_, path)| &path[..path.rfind('/').unwrap()])
            .unwrap()
    };
    for &accessing in &[root_file, leaf_file, sibling_file] {
        for &defining in &[root_file, leaf_file, sibling_file] {
            for &is_public in &[false, true] {
                let expected = is_public || directory_of(accessing) == directory_of(defining);
                assert_eq!(
                    facts.is_accessible(accessing, defining, is_public),
                    expected,
                    "is_accessible for accessing={accessing:?} defining={defining:?} pub={is_public}"
                );
            }
        }
    }
    // Spot the load-bearing rows: same file sees private; cross-directory
    // private is hidden; public crosses.
    assert!(
        facts.is_accessible(root_file, root_file, false),
        "same file sees private"
    );
    assert!(
        facts.is_accessible(root_file, sibling_file, false),
        "same-directory sibling sees private"
    );
    assert!(
        !facts.is_accessible(root_file, leaf_file, false),
        "cross-dir private hidden"
    );
    assert!(
        facts.is_accessible(root_file, leaf_file, true),
        "public crosses directories"
    );
    assert!(
        facts.is_accessible(root_file, unknown_file, false)
            && facts.is_accessible(unknown_file, root_file, false),
        "an unknown path remains permissive"
    );
}

#[test]
fn provider_member_candidates_span_methods_and_assoc_fns_with_signature_handles() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
    use rue_air::BodyFactProvider;
    // `get` is a method (self receiver); `make` is an associated function
    // (no self). Both share the compiler's method table and the production
    // resolver discriminates on `has_self` (MethodCalledAsAssocFn /
    // AssocFnCalledAsMethod). The provider must reach BOTH.
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "struct Counter { value: i32, \
                 fn get(borrow self) -> i32 { self.value } \
                 fn make(start: i32) -> Counter { Counter { value: start } } }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let config = semantic_configuration();
    let receiver = ReceiverTypeIdentity::new(m.clone(), "Counter", Cat::Struct);

    let receiver_probe = receiver.clone();
    let outcome =
        database.probe_ready_body_facts(revision, config.clone(), "members", move |provider| {
            (
                provider.method_candidates(&receiver_probe, "get"),
                provider.method_candidates(&receiver_probe, "make"),
                provider.method_candidates(&receiver_probe, "absent_member"),
                provider.operator_candidates(&receiver_probe, rue_air::OperatorName::Add),
            )
        });
    let (get, make, absent, add) = outcome.result;

    // `get` is a method (has_self); the candidate carries a follow-up handle.
    assert_eq!(get.len(), 1, "get is a candidate SET of one");
    let get_candidate = &get[0];
    assert_eq!(get_candidate.name.as_ref(), "get");
    assert_eq!(get_candidate.kind, rue_air::MemberKind::Method);
    assert!(
        get_candidate.has_self_receiver,
        "get takes a self receiver, sourced from its signature"
    );

    // `make` is an associated function (no self) and is reached through the
    // SAME member op — the BLOCKER-A category the old impl could not express.
    assert_eq!(make.len(), 1);
    let make_candidate = &make[0];
    assert_eq!(make_candidate.kind, rue_air::MemberKind::AssociatedFunction);
    assert!(
        !make_candidate.has_self_receiver,
        "make takes no self receiver — the MethodCalledAsAssocFn discriminator"
    );

    assert!(absent.is_empty());
    assert!(add.is_empty(), "Counter overloads no operator");

    // BLOCKER B: from a candidate's follow-up handle, the full signature is
    // reachable and equals the production epoch's — including receiver mode,
    // parameter modes, and return type.
    let sig_probe = get_candidate.declaration.clone();
    let sig_outcome =
        database.probe_ready_body_facts(revision, config.clone(), "member-sig", move |provider| {
            provider.signature(&sig_probe)
        });
    let provider_sig = sig_outcome.result.expect("get has a signature");
    let epoch_sig = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: get_candidate.declaration.clone(),
                configuration: config.clone(),
            },
        ),
    );
    let crate::semantic_query_nucleus::SemanticNucleusValue::Signature(epoch_sig) = epoch_sig
    else {
        panic!("get has a signature")
    };
    assert_eq!(
        provider_sig, epoch_sig,
        "the candidate handle fetches the exact production signature (modes + return type)"
    );
    assert!(
        sig_outcome.dependencies.iter().any(|node| {
            node.family() == "compiler.semantic-nucleus"
                && node.key().starts_with("signature:")
                && node.key().contains("get")
        }),
        "signature facts observe the signature projection: {:?}",
        sig_outcome.dependencies
    );
    assert!(
        !sig_outcome.dependencies.iter().any(|node| {
            node.family() == "compiler.semantic-nucleus"
                && node.key().starts_with("identity:")
                && node.key().contains("get")
        }),
        "the resolved signature carries its definition key without a peer identity request: {:?}",
        sig_outcome.dependencies
    );

    // Differential: the candidate's visibility matches the method's own
    // semantic-nucleus identity terminal.
    let epoch_identity = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::Identity(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: get_candidate.declaration.clone(),
                configuration: config,
            },
        ),
    );
    let crate::semantic_query_nucleus::SemanticNucleusValue::Identity(epoch_identity) =
        epoch_identity
    else {
        panic!("get has an identity")
    };
    assert_eq!(get_candidate.is_public, epoch_identity.is_public);

    // Edge-recording proof: candidates are sourced from semantic-nucleus,
    // for both the method and the associated-function member.
    assert!(
        outcome
            .dependencies
            .iter()
            .any(|node| node.family() == "compiler.semantic-nucleus" && node.key().contains("get")),
        "method candidate observes the method's nucleus terminal: {:?}",
        outcome.dependencies
    );
    assert!(
            outcome
                .dependencies
                .iter()
                .any(|node| node.family() == "compiler.semantic-nucleus"
                    && node.key().contains("make")),
            "assoc-fn candidate observes the assoc fn's nucleus terminal: {:?}",
            outcome.dependencies
        );
}

#[test]
fn provider_differential_over_representative_bodies() {
    use crate::declaration_candidate::DeclarationCandidateCategory as Cat;
    use rue_air::BodyFactProvider;
    // A body with a deterministic diagnostic: an ill-formed nominal naming an
    // undefined field type.
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "struct Bad { field: Missing }\n\
                 struct Good { value: i32 }\n\
                 fn plain(x: i32) -> i32 { x }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let m = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let config = semantic_configuration();

    let bad = declaration_candidate(&database, revision, &m, Cat::Struct, "Bad");
    let good = declaration_candidate(&database, revision, &m, Cat::Struct, "Good");
    let plain = declaration_candidate(&database, revision, &m, Cat::Function, "plain");

    let bad_probe = bad.clone();
    let good_probe = good.clone();
    let plain_probe = plain.clone();
    let outcome = database.probe_ready_body_facts(
        revision,
        config.clone(),
        "representative",
        move |provider| {
            (
                provider.nominal_well_formedness(&bad_probe),
                provider.nominal_well_formedness(&good_probe),
                provider.signature(&plain_probe),
            )
        },
    );
    let (bad_wf, good_wf, plain_sig) = outcome.result;

    // The diagnostics body's nominal is ill-formed; the good one is not. Both
    // match the semantic-nucleus well-formedness terminal.
    assert_eq!(bad_wf, Some(rue_air::NominalWellFormedness::IllFormed));
    assert_eq!(good_wf, Some(rue_air::NominalWellFormedness::WellFormed));
    let epoch_bad = request_semantic_nucleus(
        &database,
        revision,
        crate::semantic_query_nucleus::SemanticNucleusKey::NominalWellFormedness(
            crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: bad.clone(),
                configuration: config.clone(),
            },
        ),
    );
    assert!(
        matches!(
            epoch_bad,
            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(_)
        ),
        "the production epoch also fails Bad's well-formedness"
    );

    assert!(plain_sig.is_some());

    let plain_instance = free_function_instance(&m, "plain");
    let producer = database.probe_body_facts(
        revision,
        config.clone(),
        "representative-producer",
        move |provider| provider.producer_body_facts(&plain_instance),
    );
    assert!(matches!(
        producer,
        Ok(ProviderProbeOutcome {
            result: Some(crate::body_query::ProducedAnonymous::Produced(_)),
            ..
        })
    ));

    let missing_module = ModuleId::from_logical_path("missing.rue").unwrap();
    let missing = database.probe_body_facts(
        revision,
        config,
        "representative-missing",
        move |provider| {
            provider.lookup_unqualified(
                &missing_module,
                rue_air::ProviderNamespace::ModuleItem,
                "never",
            )
        },
    );
    assert!(matches!(
        missing,
        Err(CompilerBodyProviderStatus::Incomplete(
            CompilerBodyProviderIncomplete::MissingInput(_)
        ))
    ));
}

#[test]
fn retained_provider_specialization_materializes_with_live_air_parity() {
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn selected(comptime N: i32, value: i32) -> i32 { value + N }\n\
                 fn main() -> i32 { selected(7, 5) }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let base_instance = free_function_instance(&module, "selected");
    let crate::FunctionInstanceKey::Definition(base) = base_instance else {
        unreachable!("free function helper returns a definition")
    };
    let arguments = crate::CanonicalArguments {
        types: Arc::from([]),
        values: Arc::from([crate::CanonicalArgumentValue::Integer(7)]),
    };
    let instance = crate::FunctionInstanceKey::Specialization {
        base: Node::new(crate::FunctionInstanceKey::Definition(base.clone())),
        arguments: arguments.clone(),
    };
    let configuration = semantic_configuration();
    let key = crate::body_query::BodyQueryKey::new(instance.clone(), configuration.clone());
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let input = database
        .body_input(revision, key, CancellationToken::new())
        .expect("specialized body input request completes");
    let rue_query::QueryOutcome::Success(crate::body_query::BodyInputValue::Available(input)) =
        input.outcome()
    else {
        panic!("specialized body input is available: {input:?}")
    };
    let input = input.clone();
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
        .expect("canonical specialized body plan materializes");
    let preview = configuration
        .preview_features
        .names()
        .iter()
        .filter_map(|name| name.parse().ok())
        .collect();
    let owner_source = rue_air::DurableBodySourceLocator {
        file_id: input.source.file_id,
        physical_path: input.source.physical_path.clone(),
        source_length: input.source.source_length,
    };
    let probe_base = base.clone();
    let probe_arguments = arguments.clone();
    let probe_module = module.clone();
    let target = configuration.target;
    let outcome = database.probe_ready_body_facts(
        revision,
        configuration,
        "retained-specialization-local-materialization",
        move |provider| {
            let source = CompilerBodyDurableSource::with_anonymous(
                provider,
                &[],
                Some((probe_module, owner_source)),
            );
            rue_air::analyze_provider_specialized_body(
                provider,
                source,
                &bundle,
                probe_base.clone(),
                probe_base.name(),
                &probe_arguments,
                target,
                preview,
                &rue_air::ProviderWellKnownOptionFacts {
                    nominals: Vec::new(),
                    option_by_payload: Vec::new(),
                },
            )
        },
    );
    let analyzed = outcome
        .result
        .expect("real provider specialization analysis succeeds");

    // Capture the retained live semantic result before relocating its
    // durable export. These fields are deliberately the provider result's
    // issuing AIR, pool, interner, strings, and warnings — not a second
    // materialization wrapper.
    let live_name = analyzed.function.name.clone();
    let live_air = format!("{:?}", analyzed.function.air);
    let live_callable_kind = analyzed.function.callable_kind;
    let live_num_locals = analyzed.function.num_locals;
    let live_num_param_slots = analyzed.function.num_param_slots;
    let live_param_modes = analyzed.function.param_modes.clone();
    let live_allow_unreachable_code = analyzed.function.allow_unreachable_code;
    let live_body_start = analyzed
        .function
        .air
        .iter()
        .map(|(_, instruction)| instruction.span.start)
        .min()
        .expect("specialized AIR is non-empty");
    let live_body_end = analyzed
        .function
        .air
        .iter()
        .map(|(_, instruction)| instruction.span.end)
        .max()
        .expect("specialized AIR is non-empty");
    let live_pool_len = analyzed.type_pool.len();
    let live_source_symbol = analyzed
        .interner
        .get("value")
        .expect("retained provider interner owns analyzed source symbols");
    assert_eq!(analyzed.interner.resolve(&live_source_symbol), "value");
    let live_strings = analyzed.strings.clone();
    let live_warnings = format!("{:?}", analyzed.warnings);

    let definition_tokens = analyzed
        .definition_tokens
        .into_iter()
        .collect::<AHashMap<_, _>>();
    let module_tokens = analyzed
        .module_tokens
        .into_iter()
        .collect::<AHashMap<_, _>>();
    let definition = |token: &rue_air::SemanticDefinitionToken| {
        definition_tokens
            .get(token)
            .cloned()
            .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
    };
    let relocate_module = |token: &rue_air::SemanticModuleToken| {
        module_tokens
            .get(token)
            .cloned()
            .ok_or(rue_air::SemanticStableResolutionFailure::Missing)
    };
    let live_identity = analyzed
        .function
        .identity
        .try_map_identities(&definition, &relocate_module)
        .expect("retained function identity relocates");
    let identity = analyzed
        .export
        .identity
        .try_map_keys(&definition, &relocate_module)
        .expect("specialization identity relocates");
    let body = analyzed
        .export
        .body
        .try_map_keys(&definition, &relocate_module)
        .expect("specialized body relocates");
    let dependencies = analyzed
        .export
        .dependencies
        .iter()
        .map(&definition)
        .collect::<Result<Vec<_>, _>>()
        .expect("specialization dependencies relocate");
    let canonical = crate::body_query::CanonicalBody::Specialization {
        identity,
        body,
        dependencies: dependencies.into(),
        dependency_boundary_complete: analyzed.export.dependency_boundary_complete,
    };
    let body_span = rue_span::Span::with_file(input.source.file_id, live_body_start, live_body_end);
    let callable = crate::local_semantic_materialization::LocalCallableFact {
        identity: instance,
        symbol: Arc::from(live_name.as_str()),
    };
    let materialized = crate::local_semantic_materialization::materialize_canonical_body_for_test(
        &canonical,
        body_span,
        &[],
        &[],
        std::slice::from_ref(&callable),
        &[],
        std::slice::from_ref(&module),
        &[],
        &[],
    )
    .expect("durable provider export materializes in a fresh local epoch");

    assert_eq!(materialized.identity, live_identity);
    assert_eq!(materialized.name, live_name);
    assert_eq!(materialized.callable_kind, live_callable_kind);
    assert_eq!(format!("{:?}", materialized.air), live_air);
    assert_eq!(materialized.num_locals, live_num_locals);
    assert_eq!(materialized.num_param_slots, live_num_param_slots);
    assert_eq!(materialized.param_modes, live_param_modes);
    assert_eq!(
        materialized.allow_unreachable_code,
        live_allow_unreachable_code
    );
    assert_eq!(materialized.type_pool.len(), live_pool_len);
    let local_name_symbol = materialized
        .interner
        .get(&materialized.name)
        .expect("local materialization interner owns the function symbol");
    assert_eq!(
        materialized.interner.resolve(&local_name_symbol),
        materialized.name
    );
    assert_eq!(materialized.strings, live_strings);
    assert_eq!(format!("{:?}", materialized.warnings), live_warnings);
}

#[test]
fn provider_producer_facts_preserve_specialization_instance_terminal() {
    use rue_air::BodyFactProvider;
    let snapshot = source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "fn Pair() -> type { struct { value: i32 } }\n\
                 fn main() -> i32 { 0 }\n",
        )],
        1,
    );
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = revision_for(&mut database, &snapshot);
    let configuration = semantic_configuration();
    let pair_instance = crate::FunctionInstanceKey::Specialization {
        base: Node::new(free_function_instance(&module, "Pair")),
        arguments: crate::CanonicalArguments::default(),
    };
    let registered_key =
        crate::body_query::BodyQueryKey::new(pair_instance.clone(), configuration.clone());

    let provider_instance = pair_instance.clone();
    let outcome = database.probe_ready_body_facts(
        revision,
        configuration,
        "producer-specialization-instance",
        move |provider| {
            (
                provider.producer_body_facts(&provider_instance),
                provider.trusted_toolchain_facts(&provider_instance),
            )
        },
    );
    let (provided, provider_toolchain) = outcome.result;
    let provided = provided.expect("the Pair specialization publishes producer facts");

    let direct = database.runtime.request_registered(
        &database.body_produced_anonymous,
        revision,
        registered_key.clone(),
        CancellationToken::new(),
    );
    let terminal = direct
        .terminal()
        .expect("the specialization-shaped registered terminal is retained");
    let rue_query::QueryOutcome::Success(expected) = terminal.outcome() else {
        panic!("the specialization-shaped registered terminal succeeds")
    };
    assert!(crate::body_query::produced_anonymous_equal(
        &provided, expected
    ));
    assert!(matches!(
        provided,
        crate::body_query::ProducedAnonymous::Produced(ref produced) if !produced.0.is_empty()
    ));
    let producer_edges = outcome
        .dependencies
        .iter()
        .filter(|node| node.family() == "compiler.body-produced-anonymous")
        .map(|node| node.key().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        producer_edges,
        BTreeSet::from([registered_key.stable_identity()]),
        "the provider must observe only the exact specialization body terminal"
    );

    let direct_toolchain = database.runtime.request_registered(
        &database.body_toolchain_demands,
        revision,
        registered_key.clone(),
        CancellationToken::new(),
    );
    let toolchain_terminal = direct_toolchain
        .terminal()
        .expect("the specialization-shaped toolchain terminal is retained");
    let rue_query::QueryOutcome::Success(expected_toolchain) = toolchain_terminal.outcome() else {
        panic!("the specialization-shaped toolchain terminal succeeds")
    };
    assert_eq!(provider_toolchain, *expected_toolchain);
    let toolchain_edges = outcome
        .dependencies
        .iter()
        .filter(|node| node.family() == "compiler.body-toolchain-demands")
        .map(|node| node.key().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        toolchain_edges,
        BTreeSet::from([registered_key.stable_identity()]),
        "the provider must observe only the exact specialization toolchain terminal"
    );
}

// ---- RUE-1091 slice 3c: PublishedRootLookupLease pressure acceptance ----

/// A source with enough distinct module-item names to exceed a small lookup
/// floor: unique positives (A, B, C, main), an ambiguous pair (`dup`), and
/// room for many negatives.
fn lookup_pressure_source() -> SourceSnapshot {
    source_snapshot(
        &[(
            1,
            "/m.rue",
            "m.rue",
            "pub struct A {}\npub struct B {}\npub struct C {}\n\
                 fn dup() {}\nfn dup() {}\nfn main() -> i32 { 0 }\n",
        )],
        1,
    )
}

/// The claims incurred by requesting one module-item lookup: zero when the
/// terminal is warm (retained, reused), positive when it was evicted and must
/// re-derive. The runtime claim counter is the executable warm/cold oracle.
fn lookup_claims_delta(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    module: &ModuleId,
    name: &str,
) -> u64 {
    let before = database.runtime.metrics().claims;
    let _ = request_lookup_name(
        database,
        revision,
        module,
        DefinitionNamespace::ModuleItem,
        name,
    );
    database.runtime.metrics().claims - before
}

/// The current node incarnation of one module-item lookup terminal. Reading a
/// warm key returns its retained incarnation; an evicted key would rebuild a
/// fresh incarnation, which is exactly what a birth-eviction window would
/// leave behind.
fn lookup_incarnation(
    database: &RevisionedQueryDatabase,
    revision: Revision,
    module: &ModuleId,
    name: &str,
) -> u64 {
    request_lookup_name(
        database,
        revision,
        module,
        DefinitionNamespace::ModuleItem,
        name,
    )
    .terminal()
    .unwrap()
    .node_incarnation()
}

#[test]
fn published_lookup_root_pressure_exceeds_floor_supersedes_and_meters_thrash() {
    use rue_air::BodyFactProvider;
    use rue_air::ProviderNamespace::ModuleItem as NS;
    let source = lookup_pressure_source();
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    // A tiny floor of 4 lookup terminals per family, so a root that consults
    // more names than the floor forces grow-with-pressure.
    let mut database = RevisionedQueryDatabase::with_declaration_memo_retention(4);
    let revision = revision_for(&mut database, &source);
    let config = semantic_configuration();

    // Root R1 (success): the six ADR key classes at once — positive (A, B),
    // superseded-only positives (C, main), negatives (Nope1, Nope2), an
    // ambiguous qualified lookup (dup), and a failed import (missing.rue).
    // Eight+ distinct terminals, well past the floor of 4.
    assert!(database.publish_lookup_root(
        revision,
        config.clone(),
        "probe-r1",
        "root",
        false,
        |p| {
            p.lookup_unqualified(&module, NS, "A");
            p.lookup_unqualified(&module, NS, "B");
            p.lookup_unqualified(&module, NS, "C");
            p.lookup_unqualified(&module, NS, "main");
            p.lookup_unqualified(&module, NS, "Nope1");
            p.lookup_unqualified(&module, NS, "Nope2");
            p.lookup_qualified(&module, NS, "dup");
            p.resolve_import(&module, "missing.rue");
        }
    ));

    let after_r1 = database.lookup_pressure_metrics();
    assert_eq!(after_r1.published_roots, 1);
    assert!(
        after_r1.leased_terminals >= 8,
        "the published root leased every observed terminal: {after_r1:?}"
    );
    assert!(
        after_r1.protected_growth > 0,
        "the current root grew a lookup family past its floor of 4 rather than \
             evict a protected pin: {after_r1:?}"
    );
    assert!(
        after_r1.retained_family_terminals > 4,
        "the family was grown to hold the protected current-root set, not \
             evicted down to the floor: {after_r1:?}"
    );

    // Every current-root key is warm: revisiting reuses the retained terminal
    // (falsifier: retained memory falls after publication; a rooted key evicts).
    for name in ["A", "B", "C", "main", "Nope1", "Nope2", "dup"] {
        assert_eq!(
            lookup_claims_delta(&database, revision, &module, name),
            0,
            "current-root key `{name}` must stay warm"
        );
    }
    let a_incarnation = lookup_incarnation(&database, revision, &module, "A");

    // Root R2 (deterministic failure): a SMALLER set — hot {A, B}, ambiguous
    // dup, one fresh negative (Nope3), and the failed import. Promotion
    // supersedes R1, batch-releasing R1's now-unneeded pins.
    assert!(database.publish_lookup_root(
        revision,
        config.clone(),
        "probe-r2",
        "root",
        false,
        |p| {
            p.lookup_unqualified(&module, NS, "A");
            p.lookup_unqualified(&module, NS, "B");
            p.lookup_qualified(&module, NS, "dup");
            p.lookup_unqualified(&module, NS, "Nope3");
            p.resolve_import(&module, "missing.rue");
        }
    ));

    let after_r2 = database.lookup_pressure_metrics();
    assert_eq!(
        after_r2.published_roots, 1,
        "the successor replaced the prior published root, it did not accumulate"
    );
    // The CURRENT FAILURE root's keys stay warm (falsifier: a current failure
    // key is evicted).
    for name in ["A", "B", "dup", "Nope3"] {
        assert_eq!(
            lookup_claims_delta(&database, revision, &module, name),
            0,
            "current failure-root key `{name}` must stay warm"
        );
    }
    // A current-root key kept its exact terminal across the handoff — no
    // rebuild, so the same incarnation.
    assert_eq!(
        lookup_incarnation(&database, revision, &module, "A"),
        a_incarnation,
        "a current-root key is never rebuilt across a supersession"
    );
    // The superseded root's released entries returned toward the floor: total
    // retained lookup terminals fell after supersession.
    assert!(
        after_r2.retained_family_terminals < after_r1.retained_family_terminals,
        "the superseded root's unneeded entries were reclaimed toward the bound: \
             {after_r1:?} -> {after_r2:?}"
    );

    // Root R3 (fixed successor): observe hot {A, B} plus the SUPERSEDED key C,
    // which R2 dropped and pressure evicted. Observing it re-derives it once —
    // metered as retention-induced thrash — while A, B stay warm (no thrash).
    let rederiv_before = after_r2.rederivations_after_eviction;
    assert!(database.publish_lookup_root(
        revision,
        config.clone(),
        "probe-r3",
        "root",
        false,
        |p| {
            p.lookup_unqualified(&module, NS, "A");
            p.lookup_unqualified(&module, NS, "B");
            p.lookup_unqualified(&module, NS, "C");
        }
    ));
    let after_r3 = database.lookup_pressure_metrics();
    assert_eq!(
        after_r3.rederivations_after_eviction,
        rederiv_before + 1,
        "exactly the evicted superseded key C re-derived; the warm keys A and B \
             did not: {after_r2:?} -> {after_r3:?}"
    );
    // The rederivation is invisible: C now resolves to the same canonical
    // Unique fact it did under R1.
    let c = request_lookup_name(
        &database,
        revision,
        &module,
        DefinitionNamespace::ModuleItem,
        "C",
    );
    assert!(matches!(
        canonical_of(&c),
        CanonicalNameResolution::Unique(_)
    ));
}

#[test]
fn published_lookup_root_handoff_has_no_birth_eviction_window() {
    use rue_air::BodyFactProvider;
    use rue_air::ProviderNamespace::ModuleItem as NS;
    let source = lookup_pressure_source();
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::with_declaration_memo_retention(4);
    let revision = revision_for(&mut database, &source);
    let config = semantic_configuration();

    // Root A holds four shared terminals (at the floor).
    assert!(database.publish_lookup_root(
        revision,
        config.clone(),
        "probe-a",
        "root",
        false,
        |p| {
            for name in ["A", "B", "C", "main"] {
                p.lookup_unqualified(&module, NS, name);
            }
        }
    ));
    let shared_incarnations: Vec<u64> = ["A", "B", "C", "main"]
        .iter()
        .map(|name| lookup_incarnation(&database, revision, &module, name))
        .collect();

    // Drive successor root B observing the SAME shared terminals — its pins
    // are acquired while B's request lease is live — but do NOT promote yet.
    let observed_b = {
        let captured: std::cell::RefCell<Option<ObservedLookupRoot>> =
            std::cell::RefCell::new(None);
        database
            .runtime
            .query(
                &database.provider_probe,
                revision,
                ProviderProbeKey {
                    label: Arc::from("probe-b"),
                },
                CancellationToken::new(),
                |context| {
                    let provider = CompilerBodyFactProvider::new(
                        database.compiler_body_provider_queries(context, config.clone()),
                    );
                    for name in ["A", "B", "C", "main"] {
                        provider.lookup_unqualified(&module, NS, name);
                    }
                    *captured.borrow_mut() = Some(provider.take_observed_root());
                    Ok(QueryOutput::success(ProviderProbeValue))
                },
            )
            .expect("probe B published");
        captured.into_inner().unwrap()
    };

    // Deterministic pressure injected DURING the handoff window — after B's
    // pins are acquired, before B supersedes A: request many fresh negative
    // keys to drive the family far past its floor and force eviction passes.
    // B's explicit pins (and A's, until released) protect the shared terminals
    // throughout, so none is evicted.
    for i in 0..24 {
        let _ = request_lookup_name(
            &database,
            revision,
            &module,
            DefinitionNamespace::ModuleItem,
            &format!("Filler{i}"),
        );
    }

    // Now supersede A with B. If a birth-eviction window existed (B pinned a
    // terminal after protection lapsed), a shared terminal would have been
    // evicted and rebuilt with a fresh incarnation.
    database.promote_published_lookup_root("root".to_owned(), observed_b);

    for (name, incarnation) in ["A", "B", "C", "main"].iter().zip(&shared_incarnations) {
        assert_eq!(
            lookup_claims_delta(&database, revision, &module, name),
            0,
            "shared key `{name}` survived the handoff warm"
        );
        assert_eq!(
            lookup_incarnation(&database, revision, &module, name),
            *incarnation,
            "shared key `{name}` kept its exact terminal — no birth-eviction window"
        );
    }
}

#[test]
fn published_lookup_root_never_promotes_canceled_or_speculative() {
    use rue_air::BodyFactProvider;
    use rue_air::ProviderNamespace::ModuleItem as NS;
    let source = lookup_pressure_source();
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::with_declaration_memo_retention(4);
    let revision = revision_for(&mut database, &source);
    let config = semantic_configuration();

    // A request that aborts before publishing a root promotes nothing: its
    // observed pins release with the request.
    let published = database.publish_lookup_root(
        revision,
        config.clone(),
        "probe-cancel",
        "root",
        true,
        |p| {
            for name in ["A", "B", "C", "main"] {
                p.lookup_unqualified(&module, NS, name);
            }
        },
    );
    assert!(!published, "a canceled attempt publishes no root");
    let metrics = database.lookup_pressure_metrics();
    assert_eq!(
        metrics.published_roots, 0,
        "a canceled attempt promotes no root (never-promote rule)"
    );
    assert_eq!(
        metrics.leased_terminals, 0,
        "a canceled attempt leases no terminal into the session lease"
    );

    // A missing exact input is also provisional: it must not be converted
    // into an absent lookup and promoted as if the provider were ready.
    let missing_module = ModuleId::from_logical_path("missing.rue").unwrap();
    assert!(
        !database.publish_lookup_root(
            revision,
            config.clone(),
            "probe-missing-input",
            "root-missing-input",
            false,
            |p| {
                p.lookup_unqualified(&missing_module, NS, "never");
            },
        ),
        "a MissingInput provider status publishes no root"
    );
    assert_eq!(
        database.lookup_pressure_metrics().published_roots,
        0,
        "the MissingInput attempt does not promote a root"
    );

    // The keys the canceled attempt merely validated are speculative: no
    // published root observes them, so they are not lease-pinned and stay
    // evictable. Drive the family far past its floor and confirm a speculative
    // key can be reclaimed (falsifier: a speculative key becomes rooted).
    for i in 0..24 {
        let _ = request_lookup_name(
            &database,
            revision,
            &module,
            DefinitionNamespace::ModuleItem,
            &format!("Filler{i}"),
        );
    }
    assert_eq!(
        database.lookup_pressure_metrics().leased_terminals,
        0,
        "speculative keys are never promoted, so the lease stays empty under pressure"
    );
    assert!(
        lookup_claims_delta(&database, revision, &module, "C") >= 1,
        "the speculative key C was evictable and re-derived under pressure"
    );
}

#[test]
fn published_lookup_root_edit_error_fix_loop_keeps_failure_set_warm() {
    use rue_air::BodyFactProvider;
    use rue_air::ProviderNamespace::ModuleItem as NS;
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::with_declaration_memo_retention(4);
    let config = semantic_configuration();

    // Three iterations of an edit/error/fix loop. Each iteration edits an
    // UNRELATED trailing declaration (so the observed keys A, B, dup keep their
    // position-free facts and stay green) and republishes the SAME logical
    // root over {A, B, dup}: a success, then a deterministic-failure body, then
    // a fix. The failure root's lookup set stays warm between iterations —
    // proven by zero rederivations for its keys throughout.
    let variants = [
        "pub struct A {}\npub struct B {}\nfn dup() {}\nfn dup() {}\nfn main() -> i32 { 0 }\n",
        "pub struct A {}\npub struct B {}\nfn dup() {}\nfn dup() {}\nfn main() -> i32 { 1 }\n\
             pub fn extra_one() {}\n",
        "pub struct A {}\npub struct B {}\nfn dup() {}\nfn dup() {}\nfn main() -> i32 { 2 }\n",
    ];
    let labels = ["loop-success", "loop-error", "loop-fix"];
    let mut a_incarnation = None;
    for (iteration, (variant, label)) in variants.iter().zip(labels).enumerate() {
        let source = source_snapshot(&[(1, "/m.rue", "m.rue", variant)], 1);
        let revision = revision_for(&mut database, &source);
        assert!(database.publish_lookup_root(
            revision,
            config.clone(),
            label,
            "root",
            false,
            |p| {
                p.lookup_unqualified(&module, NS, "A");
                p.lookup_unqualified(&module, NS, "B");
                p.lookup_qualified(&module, NS, "dup");
            }
        ));
        // The failure root's (and every iteration's) lookup keys are warm: no
        // key was evicted between iterations, so none re-derived.
        assert_eq!(
            database
                .lookup_pressure_metrics()
                .rederivations_after_eviction,
            0,
            "iteration {iteration}: the retained deterministic dependency set stayed \
                 warm, so no key re-derived"
        );
        for name in ["A", "B", "dup"] {
            assert_eq!(
                lookup_claims_delta(&database, revision, &module, name),
                0,
                "iteration {iteration}: key `{name}` stays warm across the loop"
            );
        }
        // The hot key keeps its exact terminal identity across the whole loop.
        let incarnation = lookup_incarnation(&database, revision, &module, "A");
        match a_incarnation {
            None => a_incarnation = Some(incarnation),
            Some(previous) => assert_eq!(
                incarnation, previous,
                "iteration {iteration}: the retained key `A` is never rebuilt across the loop"
            ),
        }
    }
}

#[test]
fn published_lookup_root_empty_successor_replaces_prior_lease() {
    use rue_air::BodyFactProvider;
    use rue_air::ProviderNamespace::ModuleItem as NS;
    let source = lookup_pressure_source();
    let module = ModuleId::from_logical_path("m.rue").unwrap();
    let mut database = RevisionedQueryDatabase::with_declaration_memo_retention(4);
    let revision = revision_for(&mut database, &source);
    let config = semantic_configuration();

    // A published root with a live, non-empty lease.
    assert!(database.publish_lookup_root(
        revision,
        config.clone(),
        "probe-r1",
        "root",
        false,
        |p| {
            for name in ["A", "B", "C", "main"] {
                p.lookup_unqualified(&module, NS, name);
            }
        }
    ));
    let before = database.lookup_pressure_metrics();
    assert_eq!(before.published_roots, 1);
    assert!(
        before.leased_terminals >= 4,
        "the root leased its observed terminals: {before:?}"
    );
    // Empty is an exact successor set, not an absent update. A body that no
    // longer consults a lookup must release its predecessor's pins.
    database.promote_published_lookup_root("root".to_owned(), ObservedLookupRoot::new());
    let after = database.lookup_pressure_metrics();
    assert_eq!(
        after.published_roots, before.published_roots,
        "the same logical root remains published"
    );
    assert_eq!(
        after.leased_terminals, 0,
        "the empty exact successor releases every predecessor pin"
    );
    assert_eq!(
        after.rederivations_after_eviction, before.rederivations_after_eviction,
        "replacement itself performs no lookup"
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

    let runtime = super::REVISIONED_DATABASE_SOURCE;
    assert!(!runtime.contains(concat!("fn lower_owned_", "body_input(")));
    assert!(!runtime.contains(concat!("struct OwnedBody", "Lowering")));
    assert!(!runtime.contains(concat!("\"compiler.", "body-input\"")));
    assert!(!runtime.contains("\"compiler.body-source-locator\""));
    let body_source_basis = include_str!("registrations/body/body_source_bases.rs");
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&generic),
        &generic,
    );
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
        &super::super::session::ExactSourceInput::new(&extern_source),
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
        &super::super::session::ExactSourceInput::new(&malformed),
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
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
    let revision = database.source_revision(
        &super::super::session::ExactSourceInput::new(&source),
        &source,
    );
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

fn anonymous_identity_for_digest_test(
    name: &str,
    kind: rue_air::AnonymousNominalKind,
) -> crate::AnonymousNominalKey {
    let module = ModuleId::from_logical_path("digest-test.rue").unwrap();
    let definition = crate::StableDefinitionKey::from_stable_parts(
        module,
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        Arc::from(name),
        None,
    );
    crate::AnonymousNominalKey {
        kind,
        producer: crate::StableProducerId::Function(Node::new(
            crate::FunctionInstanceKey::Definition(definition),
        )),
        anchor: rue_rir::RirStructuralAnchor::new(vec![
            rue_rir::RirStructuralPathSegment::Body,
            rue_rir::RirStructuralPathSegment::AnonymousType(0),
        ]),
    }
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

#[test]
fn type_syntax_adapters_preserve_comptime_and_signature_diagnostics() {
    use rue_air::{SemanticResolutionError as E, SemanticTypeSyntaxFailure as F};

    let nested = E::ComptimeCallTypeArgument {
        constructor: Arc::from("Box"),
        argument_index: 0,
        argument: Arc::from("Sef"),
        error: Box::new(E::Semantic(F::UnknownType {
            syntax: Arc::from("Sef"),
        })),
    };
    let comptime = crate::durable_comptime::durable_comptime_type_syntax_failure(nested.clone());
    assert!(matches!(
        comptime,
        crate::durable_comptime::DurableComptimeFailure::Failure(value)
            if matches!(value.as_ref(), crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(reason)
                if reason.contains("Semantic(UnknownType"))
    ));

    let signature = semantic_type_query_failure(nested);
    assert!(matches!(
        signature,
        ResolveSemanticSignatureError::Failure(value)
            if matches!(value.as_ref(), crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::UnknownType(syntax)
            ) if syntax == "Sef")
    ));
}

#[test]
fn deferred_value_call_diagnostics_are_stable_and_keep_query_channels() {
    use rue_air::{
        SemanticComptimeCallExpectation as Expectation, SemanticResolutionError as E,
        SemanticTypeSyntaxFailure as F,
    };

    let site = StableDefinitionKey::from_stable_parts(
        ModuleId::from_logical_path("test.rue").unwrap(),
        crate::StableDefinitionNamespace::Value,
        crate::StableDefinitionKind::Function,
        Arc::from("callee"),
        None,
    );
    let classify = |failure| semantic_type_query_failure(E::Semantic(failure));
    let value_arity = classify(F::InvalidConstructorArity {
        constructor: Arc::from("value"),
        site: site.clone(),
        expected: 1,
        found: 0,
        expectation: Expectation::Value,
    });
    let ResolveSemanticSignatureError::Failure(value_arity) = value_arity else {
        panic!("value-call arity must be a stable diagnostic")
    };
    let crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
        ErrorKind::ComptimeEvaluationFailed { reason },
    ) = *value_arity
    else {
        panic!("value-call arity must preserve E1200")
    };
    assert_eq!(
        reason,
        "value-returning comptime function `value` expects 1 comptime argument, but 0 were provided"
    );
    assert!(!reason.contains("type constructor"));
    assert!(!reason.contains("InvalidConstructorArity"));

    let runtime = classify(F::RuntimeConstructorParameter {
        constructor: Arc::from("runtime"),
        site: site.clone(),
        expected: 1,
        found: 1,
        expectation: Expectation::Value,
    });
    let ResolveSemanticSignatureError::Failure(runtime) = runtime else {
        panic!("runtime value-call rejection must be a stable diagnostic")
    };
    let crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
        ErrorKind::ComptimeEvaluationFailed { reason },
    ) = *runtime
    else {
        panic!("runtime value-call rejection must preserve E1200")
    };
    assert_eq!(
        reason,
        "call `runtime(...)` is not a compile-time value because all parameters must be `comptime`"
    );
    assert!(!reason.contains("RuntimeConstructorParameter"));

    let type_arity = classify(F::InvalidConstructorArity {
        constructor: Arc::from("Box"),
        site,
        expected: 1,
        found: 0,
        expectation: Expectation::Type,
    });
    assert!(matches!(
        type_arity,
        ResolveSemanticSignatureError::Failure(value)
            if matches!(
                value.as_ref(),
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(reason)
                    if reason.as_ref() == "type constructor `Box` expects 1 comptime type argument(s), but 0 provided"
            )
    ));

    let type_runtime = classify(F::RuntimeConstructorParameter {
        constructor: Arc::from("RuntimeBox"),
        site: StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("test.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from("RuntimeBox"),
            None,
        ),
        expected: 1,
        found: 1,
        expectation: Expectation::Type,
    });
    assert!(matches!(
        type_runtime,
        ResolveSemanticSignatureError::Failure(value)
            if matches!(
                value.as_ref(),
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(reason)
                    if reason.as_ref() == "type constructor `RuntimeBox` cannot have runtime parameters; all parameters must be `comptime`"
            )
    ));

    let zero_parameter = classify(F::RuntimeConstructorParameter {
        constructor: Arc::from("zero"),
        site: StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("test.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from("zero"),
            None,
        ),
        expected: 0,
        found: 0,
        expectation: Expectation::Value,
    });
    assert!(matches!(
        zero_parameter,
        ResolveSemanticSignatureError::Failure(value)
            if matches!(
                value.as_ref(),
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    ErrorKind::ComptimeEvaluationFailed { reason }
                ) if reason == "call `zero(...)` is not a compile-time value because its callee must declare at least one `comptime` parameter"
            )
    ));

    let provider_failure =
        crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from("provider"));
    let preserved = semantic_type_query_failure(E::ProviderFailure(provider_failure.clone()));
    assert!(matches!(
        preserved,
        ResolveSemanticSignatureError::Failure(value) if *value == provider_failure
    ));
    assert!(matches!(
        semantic_type_query_failure(E::ProviderAbort(rue_query::QueryAbort::Canceled)),
        ResolveSemanticSignatureError::Abort(rue_query::QueryAbort::Canceled)
    ));
}
