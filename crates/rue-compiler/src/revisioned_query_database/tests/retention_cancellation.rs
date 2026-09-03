use super::*;

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
