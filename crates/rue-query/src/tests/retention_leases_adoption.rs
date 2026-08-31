//! Query runtime tests: retention leases adoption subsystem.

use super::fixtures::*;
use super::*;

fn budget_unit_charge(value_charge: u64) -> u64 {
    let output = QueryOutput::success(0_u64).with_retained_value_charge(value_charge);
    retained_terminal_charge(
        &output.outcome,
        output.retained_value_charge,
        &[],
        &[],
        &[],
        &[],
    )
    .0
}

#[test]
fn active_joiner_protects_terminal_until_wake_with_zero_retention() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family::<Key, u64>("zero-retention-join", 0)
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner = thread::spawn(move || {
        owner_runtime.query(
            &owner_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
            |_| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(QueryOutput::success(11))
            },
        )
    });
    started_rx.recv().unwrap();
    let join_runtime = runtime.clone();
    let join_family = family.clone();
    let joiner = thread::spawn(move || {
        join_runtime.query(
            &join_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
            |_| panic!("active waiter must receive the owner's terminal"),
        )
    });
    runtime.wait_for_metrics(|metrics| metrics.joins == 1);
    finish_tx.send(()).unwrap();
    let owner = owner.join().unwrap().unwrap();
    let joined = joiner.join().unwrap().unwrap();
    assert!(Arc::ptr_eq(&owner, &joined));
    assert_eq!(joined.outcome(), &QueryOutcome::Success(11));
    assert_eq!(family.retention().terminals, 0);
    assert_eq!(family.retention().memo_nodes, 0);
}
#[test]
fn canceled_last_waiter_reclaims_terminal_with_zero_retention() {
    let runtime = QueryRuntime::with_retention_budgets(
        2,
        RetentionBudgets {
            retained_bytes: 0,
            dependency_pins: 0,
        },
    );
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family::<Key, u64>("zero-retention-cancel", 0)
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let owner = thread::spawn({
        let runtime = runtime.clone();
        let family = family.clone();
        move || {
            runtime.query(
                &family,
                revision(1),
                Key("shared"),
                CancellationToken::new(),
                |_| {
                    started_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Ok(QueryOutput::success(11))
                },
            )
        }
    });
    started_rx.recv().unwrap();

    let (parked_tx, parked_rx) = mpsc::channel();
    let release_park = Arc::new(Barrier::new(2));
    let blocked_once = Arc::new(AtomicBool::new(false));
    runtime.set_interpose(Arc::new({
        let release_park = release_park.clone();
        let blocked_once = blocked_once.clone();
        move |site| {
            if site == InterposeSite::NodeJoinPark && !blocked_once.swap(true, Ordering::SeqCst) {
                parked_tx.send(()).unwrap();
                release_park.wait();
            }
        }
    }));
    let cancellation = CancellationToken::new();
    let waiter = thread::spawn({
        let runtime = runtime.clone();
        let family = family.clone();
        let cancellation = cancellation.clone();
        move || {
            runtime.query(&family, revision(1), Key("shared"), cancellation, |_| {
                panic!("waiter cannot become owner while shared work is live")
            })
        }
    });
    parked_rx.recv().unwrap();

    // Publish while the only waiter is parked. The terminal remains above
    // the zero budget solely because that waiter still protects it.
    finish_tx.send(()).unwrap();
    while runtime.metrics().green_publications != 1 {
        thread::yield_now();
    }
    assert!(runtime.metrics().retained_bytes > 0);

    let canceler = thread::spawn({
        let cancellation = cancellation.clone();
        move || cancellation.cancel()
    });
    while !cancellation.is_canceled() {
        thread::yield_now();
    }
    release_park.wait();
    canceler.join().unwrap();
    owner.join().unwrap().unwrap();
    assert_eq!(waiter.join().unwrap().unwrap_err(), QueryAbort::Canceled);
    runtime.clear_interpose();
    assert_eq!(runtime.metrics().retained_bytes, 0);
    assert_eq!(runtime.metrics().retained_dependency_pins, 0);
    assert_eq!(family.retention().terminals, 0);
    assert_eq!(family.retention().memo_nodes, 0);
}
#[test]
fn zero_retention_reclaims_empty_nodes_under_key_churn() {
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct NumberKey(u64);

    impl QueryKey for NumberKey {
        fn stable_identity(&self) -> String {
            self.0.to_string()
        }

        fn stable_hash(&self, hasher: &mut StableHasher) {
            self.0.hash(hasher);
        }
    }

    let runtime = QueryRuntime::new(1);
    let family = runtime.family::<NumberKey, u64>("key-churn", 0).unwrap();
    for key in 0..256 {
        publish_empty(&runtime, [revision(key + 1)]);
        runtime
            .query(
                &family,
                revision(key + 1),
                NumberKey(key),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(key)),
            )
            .unwrap();
    }
    assert_eq!(
        family.retention(),
        FamilyRetention {
            memo_nodes: 0,
            terminals: 0,
            terminal_limit: 0,
        }
    );
}
#[test]
fn foreign_same_named_family_cannot_pin_terminal() {
    let first_runtime = QueryRuntime::new(1);
    publish_empty(&first_runtime, [revision(1)]);
    let first = first_runtime.family::<Key, u64>("same-name", 1).unwrap();
    let terminal = first_runtime
        .query(
            &first,
            revision(1),
            Key("key"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(1)),
        )
        .unwrap();
    let second_runtime = QueryRuntime::new(1);
    let second = second_runtime.family::<Key, u64>("same-name", 1).unwrap();
    assert!(matches!(
        second.pin_terminal(&terminal),
        Err(PinError::ForeignFamily)
    ));
    assert_eq!(first.retention().terminals, 1);
    assert_eq!(second.retention().terminals, 0);
}
#[test]
fn evicted_and_recreated_node_cannot_repeat_an_old_observation() {
    let runtime = QueryRuntime::new(1);
    let first_revision = Revision::new(1, 7);
    let second_revision = Revision::new(2, 7);
    publish_empty(&runtime, [first_revision, second_revision]);
    let leaf = runtime.family::<Key, u64>("aba-leaf", 0).unwrap();
    let parent = runtime.family::<Key, u64>("aba-parent", 4).unwrap();
    let run = |revision, leaf_value| {
        let leaf = leaf.clone();
        runtime.query(
            &parent,
            revision,
            Key("parent"),
            CancellationToken::new(),
            move |context| {
                context.query(&leaf, Key("leaf"), |_| Ok(QueryOutput::success(leaf_value)))?;
                Ok(QueryOutput::success(9))
            },
        )
    };
    let first = run(first_revision, 1).unwrap();
    assert_eq!(leaf.retention().memo_nodes, 0);
    let before = runtime.metrics().validation;
    let second = run(second_revision, 2).unwrap();
    let validation = runtime.metrics().validation.saturating_sub(before);
    assert_eq!(first.dependencies()[0].stamp, 1);
    assert_eq!(second.dependencies()[0].stamp, 1);
    assert_ne!(
        first.dependencies()[0].incarnation,
        second.dependencies()[0].incarnation
    );
    assert_ne!(first.dependencies(), second.dependencies());
    // The ABA-safe child identity forced recomputation. Dependency lists
    // are provenance and do not make equal parent semantics green.
    assert_eq!(first.stamp(), second.stamp());
    assert_eq!(runtime.metrics().claims, 4);
    assert!(validation.registry_index_lookups > 0);
    assert!(validation.registry_misses > 0);
}
#[test]
fn unpublished_revisions_fail_closed_and_active_views_are_bounded_and_pinned() {
    let runtime = QueryRuntime::new(1);
    let family = runtime.family::<Key, u64>("revision-liveness", 2).unwrap();
    assert_eq!(
        runtime
            .request(
                &family,
                revision(1),
                Key("missing"),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(1)),
            )
            .abort(),
        Some(&QueryAbort::UnpublishedRevision(revision(1)))
    );

    let active = revision(2);
    publish_empty(&runtime, [active]);
    let revision_pin = family.retain_revision(active);
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let worker_runtime = runtime.clone();
    let worker_family = family.clone();
    let worker = thread::spawn(move || {
        worker_runtime.query(
            &worker_family,
            active,
            Key("active"),
            CancellationToken::new(),
            |_| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(QueryOutput::success(2))
            },
        )
    });
    started_rx.recv().unwrap();
    for id in 3..=(REVISION_RETENTION_LIMIT as u64 + 10) {
        publish_empty(&runtime, [revision(id)]);
    }
    assert_eq!(
        runtime.metrics().retained_revisions,
        REVISION_RETENTION_LIMIT as u64
    );
    runtime.publish_revision(active, []).unwrap();
    finish_tx.send(()).unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap().outcome(),
        &QueryOutcome::Success(2)
    );
    assert_eq!(
        runtime
            .query(
                &family,
                active,
                Key("active"),
                CancellationToken::new(),
                |_| panic!("explicit revision pin retains the reusable view"),
            )
            .unwrap()
            .outcome(),
        &QueryOutcome::Success(2)
    );
    drop(revision_pin);
    assert_eq!(
        runtime.publish_revision(revision(1), []),
        Err(RevisionError::Retired(revision(1)))
    );
}
#[test]
fn family_policy_and_selection_preserve_last_good_above_terminals() {
    #[derive(Debug, Clone)]
    struct Value {
        canonical: u64,
        presentation: u64,
    }

    fn canonical_equal(left: &Value, right: &Value) -> bool {
        left.canonical == right.canonical
    }

    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1), revision(2), revision(3)]);
    let family = runtime
        .family_with_equality::<Key, Value>("family-policy", 4, canonical_equal)
        .unwrap();
    let success = runtime
        .query(
            &family,
            revision(1),
            Key("selected"),
            CancellationToken::new(),
            |_| {
                Ok(QueryOutput::success(Value {
                    canonical: 1,
                    presentation: 10,
                }))
            },
        )
        .unwrap();
    let red = runtime
        .query(
            &family,
            revision(2),
            Key("selected"),
            CancellationToken::new(),
            |_| {
                Ok(QueryOutput::success(Value {
                    canonical: 1,
                    presentation: 20,
                }))
            },
        )
        .unwrap();
    assert_eq!(success.stamp(), red.stamp());
    let QueryOutcome::Success(value) = red.outcome() else {
        unreachable!()
    };
    assert_eq!(value.presentation, 20);

    let failure = runtime
        .query(
            &family,
            revision(3),
            Key("selected"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::failure(QueryFailure::new("E", "failed"))),
        )
        .unwrap();
    let mut selection = family.selection();
    selection.publish(&success).unwrap();
    selection.publish_candidate(&red).unwrap();
    assert!(Arc::ptr_eq(selection.current().unwrap(), &red));
    assert!(Arc::ptr_eq(selection.last_good().unwrap(), &success));
    assert!(selection.reselect_last_good().unwrap());
    assert!(Arc::ptr_eq(selection.current().unwrap(), &success));
    assert!(Arc::ptr_eq(selection.last_good().unwrap(), &success));
    selection.publish(&red).unwrap();
    selection.publish(&failure).unwrap();
    assert!(Arc::ptr_eq(selection.current().unwrap(), &failure));
    assert!(Arc::ptr_eq(selection.last_good().unwrap(), &red));
}
// -----------------------------------------------------------------------
// RUE-1087: bounded retention is correctness-safe. A terminal the current
// computation still needs can never be evicted. These adversarial tests use
// tiny retention caps.
// -----------------------------------------------------------------------

// 1. A long in-progress rooted traversal: an early terminal observed by the
//    root request survives eviction pressure from later unobserved
//    publications, and a later nested projection of it reuses it.
#[test]
fn in_progress_request_protects_early_observed_terminal_under_pressure() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Slot, u64>("t1-leaf", 2).unwrap();
    let driver = runtime.family::<Key, u64>("t1-driver", 8).unwrap();
    let early_computes = Arc::new(AtomicUsize::new(0));

    let (observed_tx, observed_rx) = mpsc::channel();
    let (continue_tx, continue_rx) = mpsc::channel();

    let root = {
        let runtime = runtime.clone();
        let leaf = leaf.clone();
        let early_computes = early_computes.clone();
        thread::spawn(move || {
            runtime
                .query(
                    &driver,
                    revision(1),
                    Key("root"),
                    CancellationToken::new(),
                    move |context| {
                        let counter = early_computes.clone();
                        let first = context.query(&leaf, Slot(0), move |_| {
                            counter.fetch_add(1, Ordering::SeqCst);
                            Ok(QueryOutput::success(1))
                        })?;
                        observed_tx.send(()).unwrap();
                        continue_rx.recv().unwrap();
                        let projected = context.query(&leaf, Slot(0), |_| {
                            panic!("the leased early terminal must still be live")
                        })?;
                        assert!(
                            Arc::ptr_eq(&first, &projected),
                            "early observed terminal must survive eviction pressure"
                        );
                        Ok(QueryOutput::success(projected.stamp()))
                    },
                )
                .unwrap()
        })
    };

    observed_rx.recv().unwrap();
    // Flood the leaf family with terminals no live request observes.
    for i in 1..=12 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    assert!(
        runtime.metrics().evictions > 0,
        "the tiny cap must have evicted the unobserved fillers"
    );
    continue_tx.send(()).unwrap();
    root.join().unwrap();
    assert_eq!(
        early_computes.load(Ordering::SeqCst),
        1,
        "the leased early terminal was reused, never recomputed"
    );
}
// 2. The Caldera shape: a single still-running request publishes many later
//    terminals, then projects the earliest producer. The live closure grows
//    past the tiny cap rather than evicting anything it still needs.
#[test]
fn caldera_shape_projects_early_producer_after_many_later_publications() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let producers = runtime.family::<Slot, u64>("t2-producers", 4).unwrap();
    let driver = runtime.family::<Key, u64>("t2-driver", 4).unwrap();
    let early_computes = Arc::new(AtomicUsize::new(0));
    let counter = early_computes.clone();
    let metrics_runtime = runtime.clone();

    runtime
        .query(
            &driver,
            revision(1),
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let c = counter.clone();
                let early = context.query(&producers, Slot(0), move |_| {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(QueryOutput::success(0))
                })?;
                // Publish many later producers in the same running request.
                for i in 1..=32 {
                    context.query(&producers, Slot(i), move |_| Ok(QueryOutput::success(i)))?;
                }
                // Project the earliest producer after every later publication.
                let projected = context.query(&producers, Slot(0), |_| {
                    panic!("early producer must still be live")
                })?;
                assert!(Arc::ptr_eq(&early, &projected));
                // Assertions taken while the request is still live: the live
                // closure grew past the tiny cap and evicted nothing it still
                // needs. (Once the request completes its leases release and the
                // now-speculative producers evict down to the cap — expected.)
                assert_eq!(
                    metrics_runtime.metrics().evictions,
                    0,
                    "a live closure must never evict a terminal it still needs"
                );
                assert!(
                    metrics_runtime.metrics().retention_growth > 0,
                    "the live closure had to grow past the tiny cap under pressure"
                );
                Ok(QueryOutput::success(projected.stamp()))
            },
        )
        .unwrap();

    assert_eq!(early_computes.load(Ordering::SeqCst), 1);
}
// 3. Cancellation releases request-only pins: after the request aborts, its
//    leased terminals become evictable again.
#[test]
fn cancellation_releases_request_scoped_lease() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Slot, u64>("t3-leaf", 2).unwrap();
    let driver = runtime.family::<Key, u64>("t3-driver", 8).unwrap();
    let early_computes = Arc::new(AtomicUsize::new(0));
    let token = CancellationToken::new();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (continue_tx, continue_rx) = mpsc::channel();

    let root = {
        let runtime = runtime.clone();
        let leaf = leaf.clone();
        let counter = early_computes.clone();
        let token = token.clone();
        thread::spawn(move || {
            runtime.request(&driver, revision(1), Key("root"), token, move |context| {
                let c = counter.clone();
                context.query(&leaf, Slot(0), move |_| {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(QueryOutput::success(1))
                })?;
                observed_tx.send(()).unwrap();
                continue_rx.recv().unwrap();
                context.check_canceled()?;
                Ok(QueryOutput::success(0))
            })
        })
    };

    observed_rx.recv().unwrap(); // early leased by the in-progress request
    token.cancel();
    continue_tx.send(()).unwrap();
    let attempt = root.join().unwrap();
    assert_eq!(attempt.abort(), Some(&QueryAbort::Canceled));

    // The request's lease is gone, so the early terminal is evictable again.
    for i in 1..=8 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    let counter = early_computes.clone();
    runtime
        .query(
            &leaf,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    assert_eq!(
        early_computes.load(Ordering::SeqCst),
        2,
        "after cancellation the early terminal was evictable and recomputed"
    );
}
// 4. A successful current revision remains usable after its request
//    completes: promotion into a pinned revision root survives completion
//    and later eviction pressure.
#[test]
fn promoted_revision_root_survives_request_completion() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1), revision(2)]);
    let leaf = runtime.family::<Slot, u64>("t4-leaf", 2).unwrap();
    let keep_computes = Arc::new(AtomicUsize::new(0));

    let counter = keep_computes.clone();
    let promoted = runtime
        .query(
            &leaf,
            revision(1),
            Slot(1000),
            CancellationToken::new(),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    // Promote the successful revision-1 closure into a pinned revision root.
    let revision_root = leaf.retain_revision(revision(1));

    // Later, unrelated work under revision 2 floods the tiny cap.
    for i in 0..8 {
        runtime
            .query(
                &leaf,
                revision(2),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    assert!(runtime.metrics().evictions > 0);

    // The promoted terminal is still usable after its request completed.
    let reused = runtime
        .query(
            &leaf,
            revision(1),
            Slot(1000),
            CancellationToken::new(),
            |_| panic!("the promoted revision root must be reused"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&promoted, &reused));
    assert_eq!(keep_computes.load(Ordering::SeqCst), 1);
    drop(revision_root);
}
// 5. Publishing a successor revision lets the old revision's retained closure
//    be reclaimed once its root is released, while the successor stays pinned.
#[test]
fn successor_revision_reclaims_old_closure_once_released() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1), revision(2)]);
    let leaf = runtime.family::<Slot, u64>("t5-leaf", 2).unwrap();
    let old_computes = Arc::new(AtomicUsize::new(0));

    let counter = old_computes.clone();
    let old = runtime
        .query(
            &leaf,
            revision(1),
            Slot(1000),
            CancellationToken::new(),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let old_root = leaf.retain_revision(revision(1));

    // Publish a successor revision and promote its closure too.
    let new = runtime
        .query(
            &leaf,
            revision(2),
            Slot(1000),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(2)),
        )
        .unwrap();
    let new_root = leaf.retain_revision(revision(2));
    assert!(!Arc::ptr_eq(&old, &new));

    // Release the old revision root; its retained closure is now reclaimable.
    drop(old_root);
    for i in 0..8 {
        runtime
            .query(
                &leaf,
                revision(2),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }

    // The pinned successor closure is still reused...
    let new_again = runtime
        .query(
            &leaf,
            revision(2),
            Slot(1000),
            CancellationToken::new(),
            |_| panic!("the pinned successor revision must be reused"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&new, &new_again));
    // ...while the released old closure was reclaimed and recomputes.
    let counter = old_computes.clone();
    let old_again = runtime
        .query(
            &leaf,
            revision(1),
            Slot(1000),
            CancellationToken::new(),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    assert!(!Arc::ptr_eq(&old, &old_again));
    assert_eq!(old_computes.load(Ordering::SeqCst), 2);
    drop(new_root);
}
// 6. Speculative terminals — computed by requests that complete without
//    promotion — remain evictable. Publication alone does not pin.
#[test]
fn speculative_terminals_remain_evictable_publication_alone_does_not_pin() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Slot, u64>("t6-leaf", 2).unwrap();
    let first_computes = Arc::new(AtomicUsize::new(0));

    let counter = first_computes.clone();
    runtime
        .query(
            &leaf,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();
    for i in 1..=8 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }

    assert_eq!(
        leaf.retention().terminals,
        2,
        "only the tiny cap governs unpromoted publications"
    );
    assert!(runtime.metrics().evictions >= 6);
    assert_eq!(
        runtime.metrics().retention_growth,
        0,
        "no live closure forced growth past the cap"
    );

    // The earliest speculative terminal was evicted and recomputes on demand.
    let counter = first_computes.clone();
    runtime
        .query(
            &leaf,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();
    assert_eq!(first_computes.load(Ordering::SeqCst), 2);
}
// -----------------------------------------------------------------------
// RUE-1087 (adversarial-review follow-up): lease acquisition is *atomic*
// with retention at every handoff site. Each test parks the target thread at
// the exact handoff instant (via the deterministic interposition hook) and
// drives a concurrent enforcer into that window from a second thread with
// barriers — no sleeps. With the atomic handoff the target terminal is
// already protected when the window opens, so the enforcer can never detach
// it; the pre-fix code detaches it and a later query recomputes.
// -----------------------------------------------------------------------

// 1. Publish window: pressure applied at the instant a freshly published
//    terminal is exposed and enqueued cannot evict it — with the fix its pin
//    exists before it is evictable.
#[test]
fn publish_window_pin_precedes_evictability() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Slot, u64>("pw-leaf", 1).unwrap();
    let target_computes = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(2));
    let fired = Arc::new(AtomicBool::new(false));

    // Concurrent enforcer: once the target publication has exposed the
    // terminal, flood the family to force a full enforcement pass, then
    // release the parked publisher.
    let presser = {
        let runtime = runtime.clone();
        let leaf = leaf.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait(); // publisher is parked at publish-exposed
            for i in 100..108 {
                runtime
                    .query(
                        &leaf,
                        revision(1),
                        Slot(i),
                        CancellationToken::new(),
                        move |_| Ok(QueryOutput::success(i)),
                    )
                    .unwrap();
            }
            barrier.wait(); // enforcement pass complete; resume publisher
        })
    };

    let root = {
        let runtime = runtime.clone();
        let leaf = leaf.clone();
        let target_computes = target_computes.clone();
        let barrier = barrier.clone();
        let fired = fired.clone();
        thread::spawn(move || {
            let inner_runtime = runtime.clone();
            let inner_leaf = leaf.clone();
            runtime
                .query(
                    &leaf,
                    revision(1),
                    Slot(9999),
                    CancellationToken::new(),
                    move |context| {
                        let leaf = &inner_leaf;
                        // An earlier leased terminal occupies the one retained
                        // slot, so the just-published target is the only
                        // unprotected eviction candidate in the window.
                        context.query(leaf, Slot(0), |_| Ok(QueryOutput::success(0)))?;
                        // Arm the window hook *now*, so it fires on the target's
                        // publication (Slot 1), not the earlier one (Slot 0).
                        let hook_barrier = barrier.clone();
                        let hook_fired = fired.clone();
                        inner_runtime.set_interpose(Arc::new(move |site| {
                            if site == InterposeSite::PublishExposed
                                && !hook_fired.swap(true, Ordering::SeqCst)
                            {
                                hook_barrier.wait(); // enforcer begins
                                hook_barrier.wait(); // enforcer done
                            }
                        }));
                        let counter = target_computes.clone();
                        let target = context.query(leaf, Slot(1), move |_| {
                            counter.fetch_add(1, Ordering::SeqCst);
                            Ok(QueryOutput::success(1))
                        })?;
                        // The target survived the publish-window pressure and is
                        // still the live retained terminal.
                        let again = context.query(leaf, Slot(1), |_| {
                            panic!("target must survive publish-window pressure")
                        })?;
                        assert!(Arc::ptr_eq(&target, &again));
                        Ok(QueryOutput::success(target.stamp()))
                    },
                )
                .unwrap();
        })
    };

    root.join().unwrap();
    presser.join().unwrap();
    assert!(
        runtime.metrics().evictions > 0,
        "the enforcer must have evicted the unprotected fillers"
    );
    assert_eq!(
        target_computes.load(Ordering::SeqCst),
        1,
        "the just-published target was never detached and recomputed"
    );
}
// 2. Join window, LAST-waiter: the waiter transfers its protection into a pin
//    before decrementing the waiter count, so even when the count falls to
//    zero there is no instant in which the joined terminal is unprotected. A
//    concurrent enforcer racing the handoff never detaches it.
#[test]
fn join_window_last_waiter_handoff_leaves_no_unprotected_instant() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Slot, u64>("jw-leaf", 1).unwrap();
    let owner_computes = Arc::new(AtomicUsize::new(0));
    let joiner_recomputes = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(2));
    let fired = Arc::new(AtomicBool::new(false));

    // The window hook fires exactly once, at the joiner's waiter→pin handoff.
    {
        let hook_barrier = barrier.clone();
        let hook_fired = fired.clone();
        runtime.set_interpose(Arc::new(move |site| {
            if site == InterposeSite::JoinHandoff && !hook_fired.swap(true, Ordering::SeqCst) {
                hook_barrier.wait(); // enforcer begins
                hook_barrier.wait(); // enforcer done
            }
        }));
    }

    let (owner_started_tx, owner_started_rx) = mpsc::channel();
    let (owner_go_tx, owner_go_rx) = mpsc::channel();

    // Owner computes the shared terminal, then blocks so a joiner can enqueue
    // as a waiter before publication.
    let owner = {
        let runtime = runtime.clone();
        let leaf = leaf.clone();
        let owner_computes = owner_computes.clone();
        thread::spawn(move || {
            let attempt = runtime.request(
                &leaf,
                revision(1),
                Slot(0),
                CancellationToken::new(),
                move |_| {
                    owner_computes.fetch_add(1, Ordering::SeqCst);
                    owner_started_tx.send(()).unwrap();
                    owner_go_rx.recv().unwrap();
                    Ok(QueryOutput::success(1))
                },
            );
            // Hand the attempt (its result lease) to the coordinator so it can
            // be dropped *inside* the join window, leaving the joined terminal
            // protected only by the waiter→pin handoff under test.
            attempt.terminal().unwrap().stamp();
            attempt
        })
    };

    owner_started_rx.recv().unwrap();

    // Joiner requests the same key+revision while it is still computing, so it
    // joins as a waiter and parks on the condvar until publication.
    let joiner = {
        let runtime = runtime.clone();
        let leaf = leaf.clone();
        let joiner_recomputes = joiner_recomputes.clone();
        thread::spawn(move || {
            let joined = runtime
                .query(
                    &leaf,
                    revision(1),
                    Slot(0),
                    CancellationToken::new(),
                    |_| panic!("the joiner must join the owner, not compute"),
                )
                .unwrap();
            // After the handoff window, the joined terminal is still the live
            // retained terminal: re-querying reuses it, never recomputes.
            let again = runtime
                .query(
                    &leaf,
                    revision(1),
                    Slot(0),
                    CancellationToken::new(),
                    |_| {
                        joiner_recomputes.fetch_add(1, Ordering::SeqCst);
                        Ok(QueryOutput::success(2))
                    },
                )
                .unwrap();
            assert!(
                Arc::ptr_eq(&joined, &again),
                "the joined terminal must survive the last-waiter handoff"
            );
        })
    };

    // Wait until the joiner is parked as a waiter, then let the owner publish.
    runtime.wait_for_metrics(|metrics| metrics.joins >= 1);
    owner_go_tx.send(()).unwrap();

    // The owner's request completes; take its attempt so we control its lease.
    let owner_attempt = owner.join().unwrap();

    // Rendezvous with the parked joiner (post-decrement, pre-return). Drop the
    // owner's attempt lease *now*, so the joined terminal is protected only by
    // the waiter→pin handoff, then flood to force an enforcement pass.
    barrier.wait(); // joiner parked at JoinHandoff
    drop(owner_attempt);
    for i in 100..108 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                move |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    barrier.wait(); // release the joiner

    joiner.join().unwrap();
    assert_eq!(owner_computes.load(Ordering::SeqCst), 1);
    assert_eq!(
        joiner_recomputes.load(Ordering::SeqCst),
        0,
        "the joined terminal was never detached, so nothing recomputed"
    );
    assert!(runtime.metrics().evictions > 0);
}
// 3. Reuse window: a candidate found in the memo cannot be evicted between
//    discovery and lease transfer — the discovery pin holds it retained
//    through recursive validation and the pressure applied in that window.
#[test]
fn reuse_window_candidate_survives_between_discovery_and_lease() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Slot, u64>("rw-leaf", 1).unwrap();
    let computes = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(2));
    let fired = Arc::new(AtomicBool::new(false));

    // Pre-compute the reuse candidate in a throwaway request; its attempt (and
    // result lease) drops here, leaving the terminal speculative in the memo.
    {
        let counter = computes.clone();
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(0),
                CancellationToken::new(),
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(QueryOutput::success(5))
                },
            )
            .unwrap();
    }
    assert_eq!(computes.load(Ordering::SeqCst), 1);

    // The window hook fires once, when the reuse candidate has been discovered
    // and pinned under the node lock, before recursive validation.
    {
        let hook_barrier = barrier.clone();
        let hook_fired = fired.clone();
        runtime.set_interpose(Arc::new(move |site| {
            if site == InterposeSite::ReuseDiscovered && !hook_fired.swap(true, Ordering::SeqCst) {
                hook_barrier.wait(); // enforcer begins
                hook_barrier.wait(); // enforcer done
            }
        }));
    }

    // A long-lived root reuses the candidate, then re-projects it. The root's
    // lease keeps the (correct) terminal retained so the projection is
    // observable after the window closes.
    let root = {
        let runtime = runtime.clone();
        let leaf = leaf.clone();
        thread::spawn(move || {
            let inner_leaf = leaf.clone();
            runtime
                .query(
                    &leaf,
                    revision(1),
                    Slot(7777),
                    CancellationToken::new(),
                    move |context| {
                        let leaf = &inner_leaf;
                        let first = context.query(leaf, Slot(0), |_| {
                            panic!("candidate must be reused, not recomputed")
                        })?;
                        let again = context.query(leaf, Slot(0), |_| {
                            panic!("reused candidate must still be the retained terminal")
                        })?;
                        assert!(
                            Arc::ptr_eq(&first, &again),
                            "the reused candidate must not have been detached in the window"
                        );
                        Ok(QueryOutput::success(first.stamp()))
                    },
                )
                .unwrap();
        })
    };

    // Concurrent enforcer drives pressure into the reuse window.
    barrier.wait(); // root parked at ReuseDiscovered
    for i in 100..108 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                move |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    barrier.wait(); // release the root

    root.join().unwrap();
    assert!(runtime.metrics().evictions > 0);
    assert_eq!(
        computes.load(Ordering::SeqCst),
        1,
        "the candidate was reused throughout, never detached and recomputed"
    );
}
// 3b. Reuse discovery cost: the window pin above protects the candidate
//     being validated, and the walk returns on the first candidate that
//     validates. Taking that pin on every retained attempt instead makes one
//     request O(retained attempts) — and because each surplus release is the
//     last pin on its terminal, each one also runs a retention enforcement
//     pass. A session which republishes the same key across revisions then
//     pays quadratically in its revision count (RUE-1262). The enforcement
//     counter observes exactly that, so measuring one reuse at two retained
//     depths separates per-request cost from history size.
#[test]
fn reuse_discovery_cost_is_independent_of_retained_attempt_depth() {
    fn enforcements_for_one_reuse(depth: u64) -> u64 {
        let runtime = QueryRuntime::new(1);
        let input = InputIdentity::new("source", "reuse-depth");
        // A bound far above `depth`: the node is meant to legitimately keep
        // every attempt, so nothing here is reclaimed before the reuse.
        let family = runtime.family::<Key, u64>("reuse-depth", 4096).unwrap();
        for stamp in 1..=depth {
            let revision = Revision::new(stamp, 1);
            runtime
                .publish_revision(revision, [(input.clone(), stamp)])
                .unwrap();
            let observed = input.clone();
            runtime
                .query(
                    &family,
                    revision,
                    Key("same"),
                    CancellationToken::new(),
                    move |context| {
                        context.input(observed)?;
                        Ok(QueryOutput::success(stamp))
                    },
                )
                .unwrap();
        }

        // A successor revision which changes nothing: the newest attempt is
        // the first candidate examined, and it validates.
        let reuse = Revision::new(depth + 1, 1);
        runtime
            .publish_revision(reuse, [(input.clone(), depth)])
            .unwrap();
        let before = runtime.metrics();
        let observed = input.clone();
        runtime
            .query(
                &family,
                reuse,
                Key("same"),
                CancellationToken::new(),
                move |context| {
                    context.input(observed)?;
                    unreachable!("the newest retained attempt is still valid")
                },
            )
            .unwrap();
        let after = runtime.metrics();
        assert_eq!(after.reuses - before.reuses, 1);
        assert_eq!(after.claims - before.claims, 0);
        after.retention_enforcements - before.retention_enforcements
    }

    let shallow = enforcements_for_one_reuse(4);
    let deep = enforcements_for_one_reuse(32);
    assert_eq!(
        deep, shallow,
        "one reuse costs the same whether the node retains 4 attempts or 32"
    );
}
// A rooted request which invalidates many parent/child pairs used to run
// retention once for every stale discovery pin and every speculative child
// publication. The task boundary is the safe batching boundary: all pins
// release together, and an already-converged family needs no strict pass.
#[test]
fn validation_only_publication_sweeps_are_batched_per_rooted_request() {
    const CHILDREN: u64 = 64;

    let runtime = QueryRuntime::new(1);
    let first = Revision::new(1, 1);
    let second = Revision::new(2, 1);
    let inputs = (0..CHILDREN)
        .map(|slot| InputIdentity::new("source", format!("child-{slot}")))
        .collect::<Vec<_>>();
    runtime
        .publish_revision(first, inputs.iter().cloned().map(|input| (input, 1)))
        .unwrap();

    let child = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "batched-validation-publish-child",
            4096,
            move |context, _, key| {
                let input = InputIdentity::new("source", format!("child-{}", key.0));
                Ok(QueryOutput::success(context.input(input)?))
            },
        )
        .unwrap();
    let child_for_parent = child.clone();
    let parent = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "batched-validation-publish-parent",
            4096,
            move |context, _, key| {
                let child = context.query_registered(&child_for_parent, key.clone())?;
                let QueryOutcome::Success(value) = child.outcome() else {
                    unreachable!("the child publishes typed values")
                };
                Ok(QueryOutput::success(*value))
            },
        )
        .unwrap();
    for slot in 0..CHILDREN {
        runtime
            .request_registered(&parent, first, Slot(slot), CancellationToken::new())
            .into_result()
            .unwrap();
    }

    runtime
        .publish_revision(second, inputs.into_iter().map(|input| (input, 2)))
        .unwrap();
    let root = runtime
        .family::<Key, u64>("batched-validation-publish-root", 4096)
        .unwrap();
    let before = runtime.metrics();
    runtime
        .query(
            &root,
            second,
            Key("root"),
            CancellationToken::new(),
            |context| {
                for slot in 0..CHILDREN {
                    let terminal = context.query_registered(&parent, Slot(slot))?;
                    assert_eq!(terminal.outcome(), &QueryOutcome::Success(2));
                }
                Ok(QueryOutput::success(CHILDREN))
            },
        )
        .unwrap();
    let after = runtime.metrics();

    assert_eq!(
        after.retention_enforcements - before.retention_enforcements,
        0,
        "all three families remain below their bounds and at strict watermarks"
    );
    assert_eq!(
        after.retention_scan_entries - before.retention_scan_entries,
        0
    );
}
#[test]
fn strict_retention_rebases_a_stale_watermark_below_the_family_limit() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family::<Key, u64>("stale-strict-watermark", 8)
        .unwrap();
    runtime
        .query(
            &family,
            revision(1),
            Key("root"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(1)),
        )
        .unwrap();
    assert_eq!(family.inner.retained_count.load(Ordering::Acquire), 1);

    family.inner.next_publish_sweep.store(64, Ordering::Release);
    let before = runtime.metrics().retention_enforcements;
    family.enforce_retention();
    assert_eq!(runtime.metrics().retention_enforcements, before + 1);
    assert_eq!(
        family.inner.next_publish_sweep.load(Ordering::Acquire),
        family.inner.retention_limit + 1
    );
}
// 4. Promotion gap: pressure applied precisely between request completion
//    (attempt returned, task and its request-scoped leases dropped) and
//    session/revision promotion must not evict the result. The attempt
//    carries a live result lease that bridges the gap until selection
//    registers a successor protection — the same invariant the compiler's
//    RevisionedFamily::select relies on across the crate boundary.
#[test]
fn promotion_gap_result_lease_bridges_until_selection() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Slot, u64>("pg-leaf", 1).unwrap();
    let computes = Arc::new(AtomicUsize::new(0));

    let counter = computes.clone();
    let attempt = runtime.request(
        &leaf,
        revision(1),
        Slot(0),
        CancellationToken::new(),
        move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(QueryOutput::success(7))
        },
    );
    let produced = attempt
        .terminal()
        .expect("request produced a terminal")
        .clone();

    // The producing task (and its leases) has already dropped. Apply pressure
    // in the gap before promotion: only the attempt-carried result lease keeps
    // `produced` retained here.
    for i in 1..=8 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    assert!(
        runtime.metrics().evictions > 0,
        "the tiny cap evicted the unprotected fillers"
    );

    // Promote into a session/revision selection root, then release the attempt
    // lease: protection passes to the selection with no gap.
    let mut selection = leaf.selection();
    selection
        .publish(&produced)
        .expect("promotion pins the retained terminal");
    assert!(Arc::ptr_eq(selection.current().unwrap(), &produced));
    drop(attempt);

    for i in 9..=16 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }

    // The promoted terminal is still the retained one: a later request reuses
    // it and never recomputes a detached replacement.
    let reused = runtime
        .query(
            &leaf,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            |_| panic!("promoted terminal must be reused, not recomputed"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&reused, &produced));
    assert_eq!(computes.load(Ordering::SeqCst), 1);
    drop(selection);
}
// Growing one live protected family is linear, not quadratic: geometric
// publication watermarks examine each retained prefix only a constant number
// of times, while the final forced release still converges to the floor.
#[test]
fn protected_publish_retention_work_is_linear_at_two_sizes() {
    fn scan_work(bodies: u64) -> u64 {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime
            .family::<Slot, u64>(format!("publish-scan-{bodies}"), 1)
            .unwrap();
        let driver = runtime
            .family::<Key, u64>(format!("publish-scan-driver-{bodies}"), 1)
            .unwrap();
        let measured = Arc::new(AtomicU64::new(0));
        let measured_slot = measured.clone();
        let metrics_runtime = runtime.clone();
        let published = family.clone();

        runtime
            .query(
                &driver,
                revision(1),
                Key("root"),
                CancellationToken::new(),
                move |context| {
                    let before = metrics_runtime.metrics().retention_scan_entries;
                    for i in 0..bodies {
                        context.query(&published, Slot(i), move |_| Ok(QueryOutput::success(i)))?;
                    }
                    let after = metrics_runtime.metrics().retention_scan_entries;
                    measured_slot.store(after - before, Ordering::SeqCst);
                    Ok(QueryOutput::success(bodies))
                },
            )
            .unwrap();

        assert!(
            runtime.metrics().retention_growth > 0,
            "the live request grew past the family's soft floor"
        );
        assert_eq!(
            family.retention().terminals,
            family.retention().terminal_limit,
            "the forced release-side pass still converges to the floor"
        );
        measured.load(Ordering::SeqCst)
    }

    const SMALL: u64 = 64;
    const LARGE: u64 = 128;
    let small = scan_work(SMALL);
    let large = scan_work(LARGE);

    assert!(
        small <= 2 * SMALL,
        "geometric publish sweeps examine at most a linear prefix: {small}"
    );
    assert!(
        large <= 2 * LARGE,
        "geometric publish sweeps examine at most a linear prefix: {large}"
    );
    assert!(
        large <= 2 * small + 2,
        "doubling the protected closure must not square scan work: {small} -> {large}"
    );
}
// Releasing a large task lease set is linear, not quadratic: a request whose
// body leases many terminals in one family runs exactly one retention
// enforcement pass at completion — not one per released pin — while still
// converging to the same final retained state as the per-pin path.
#[test]
fn duplicate_pin_drop_defers_retention_until_the_last_pin_releases() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family::<Slot, u64>("last-pin-enforcement", 0)
        .unwrap();
    let attempt = runtime.request(
        &family,
        revision(1),
        Slot(0),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(0)),
    );
    let terminal = attempt.terminal().unwrap().clone();
    let first = family.pin_terminal(&terminal).unwrap();
    let last = family.pin_terminal(&terminal).unwrap();
    drop(attempt);
    let before = runtime.metrics().retention_enforcements;

    drop(first);
    assert_eq!(
        runtime.metrics().retention_enforcements,
        before,
        "a non-last pin release cannot make retention progress"
    );
    assert_eq!(family.retention().terminals, 1);

    drop(last);
    assert_eq!(
        runtime.metrics().retention_enforcements,
        before + 1,
        "the last pin release runs the deferred retention pass"
    );
    assert_eq!(family.retention().terminals, 0);
}
#[test]
fn batched_task_lease_release_enforces_once_per_family_and_converges() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    // One family, tiny cap: the Caldera shape (many body terminals, one
    // family, released together at rooted-request completion).
    let family = runtime.family::<Slot, u64>("batch-release", 2).unwrap();

    // A sentinel held retained by an explicit external pin across the batched
    // release, proving the single pass still keeps protected entries while
    // evicting the rest down to the cap.
    let sentinel = runtime
        .query(
            &family,
            revision(1),
            Slot(9999),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(9999)),
        )
        .unwrap();
    let sentinel_pin = family.pin_terminal(&sentinel).unwrap();

    const LEASED: u64 = 64;
    let before_release = Arc::new(AtomicU64::new(0));
    let evictions_before_release = Arc::new(AtomicU64::new(0));
    let active_leases_before_release = Arc::new(AtomicU64::new(0));

    let metrics_runtime = runtime.clone();
    let before_slot = before_release.clone();
    let evict_slot = evictions_before_release.clone();
    let active_lease_slot = active_leases_before_release.clone();
    let leaf = family.clone();

    // One rooted request leases LEASED distinct terminals in the single
    // family, all held by its task, then aborts. Aborting avoids a root
    // publication, so the only post-body enforcement is the batched release
    // of the task's lease set — the exact work under test.
    let attempt = runtime.request(
        &family,
        revision(1),
        Slot(0),
        CancellationToken::new(),
        move |context| {
            for i in 1..=LEASED {
                context.query(&leaf, Slot(i), move |_| Ok(QueryOutput::success(i)))?;
            }
            // Snapshot at the request's last live instant, before its task
            // (holding all LEASED pins) drops and the batched release runs.
            let snapshot = metrics_runtime.metrics();
            before_slot.store(snapshot.retention_enforcements, Ordering::SeqCst);
            evict_slot.store(snapshot.evictions, Ordering::SeqCst);
            active_lease_slot.store(snapshot.active_task_leases, Ordering::SeqCst);
            Err(QueryAbort::Canceled)
        },
    );
    assert_eq!(attempt.abort(), Some(&QueryAbort::Canceled));

    let after = runtime.metrics();
    let before = before_release.load(Ordering::SeqCst);
    let evict_before = evictions_before_release.load(Ordering::SeqCst);
    assert_eq!(
        active_leases_before_release.load(Ordering::SeqCst),
        LEASED,
        "the ownership gauge counts the exact live request lease set"
    );
    assert_eq!(
        after.active_task_leases, 0,
        "request completion releases every task-owned lease"
    );
    assert_eq!(
        after.peak_task_leases, LEASED,
        "peak request ownership is allocator-independent and stable"
    );

    // Linearity: releasing LEASED pins in one family ran exactly ONE
    // enforcement pass. The per-pin `Drop` path would have run LEASED (64).
    assert_eq!(
        after.retention_enforcements - before,
        1,
        "batched release of a single family must enforce exactly once, not once per pin"
    );

    // Convergence: that single pass still evicted the now-unprotected leased
    // terminals down to the configured cap, exactly as the per-pin path would.
    assert!(
        after.evictions > evict_before,
        "the batched pass still evicts the released terminals down to the cap"
    );
    assert_eq!(
        family.retention().terminals,
        family.retention().terminal_limit,
        "post-release retention converges to the configured cap"
    );

    // The externally pinned sentinel is a protected entry: the pass retained it.
    let reused = runtime
        .query(
            &family,
            revision(1),
            Slot(9999),
            CancellationToken::new(),
            |_| panic!("the protected sentinel must be retained across batched release"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&reused, &sentinel));
    drop(sentinel_pin);
}
#[test]
fn batched_release_runs_one_aggregate_pass_after_all_families() {
    let runtime = QueryRuntime::with_retention_budgets(
        1,
        RetentionBudgets {
            retained_bytes: 0,
            dependency_pins: u64::MAX,
        },
    );
    publish_empty(&runtime, [revision(1)]);
    let first = runtime.family::<Slot, u64>("batch-global-a", 8).unwrap();
    let second = runtime.family::<Slot, u64>("batch-global-b", 8).unwrap();
    let first_attempt = runtime.request(
        &first,
        revision(1),
        Slot(0),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(0)),
    );
    let first_pin = first
        .pin_terminal(first_attempt.terminal().unwrap())
        .unwrap();
    drop(first_attempt);
    let second_attempt = runtime.request(
        &second,
        revision(1),
        Slot(0),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(0)),
    );
    let second_pin = second
        .pin_terminal(second_attempt.terminal().unwrap())
        .unwrap();
    drop(second_attempt);

    let before = runtime.metrics().retained_byte_pressure_events;
    let mut held: Vec<Box<dyn ObservedLease>> = vec![Box::new(first_pin), Box::new(second_pin)];
    batched_release(&mut held);

    let after = runtime.metrics();
    assert_eq!(
        after.retained_byte_pressure_events - before,
        1,
        "heterogeneous batched teardown runs one runtime-wide pressure pass"
    );
    assert_eq!(after.retained_bytes, 0);
}
// A session-held `RetainedPinSet` keeps a completed request's observed
// terminal retained past its task, deduplicates a re-lease of the same
// terminal, and hands off atomically to a successor set: pressure applied
// while both sets hold the shared terminal cannot evict it, and only after
// the successor is installed does releasing the predecessor free anything.
#[test]
fn retained_pin_set_bridges_dedups_and_hands_off_atomically() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Slot, u64>("retained-set-leaf", 2).unwrap();
    let computes = Arc::new(AtomicUsize::new(0));

    let counter = computes.clone();
    let attempt = runtime.request(
        &leaf,
        revision(1),
        Slot(0),
        CancellationToken::new(),
        move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(QueryOutput::success(7))
        },
    );
    let produced = attempt.terminal().expect("request produced").clone();

    // Transfer an explicit pin into a session-held set while the attempt's
    // result lease still protects `produced`, then release the attempt: the
    // set now solely bridges the terminal past the request.
    let mut set_a = RetainedPinSet::new();
    assert!(set_a.lease(leaf.pin_terminal(&produced).unwrap()));
    // A redundant re-lease of the same terminal is dropped, not double-held.
    assert!(!set_a.lease(leaf.pin_terminal(&produced).unwrap()));
    assert_eq!(set_a.len(), 1);
    assert_eq!(runtime.metrics().active_retained_pins, 1);
    drop(attempt);

    // Pressure in the gap: only `set_a` protects `produced` now.
    for i in 1..=8 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    assert!(runtime.metrics().evictions > 0, "fillers were evicted");
    let reused = runtime
        .query(
            &leaf,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            |_| panic!("the set-retained terminal must be reused, not recomputed"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&reused, &produced));

    // Atomic handoff: the successor set acquires its own pin on the SAME
    // terminal (protected the whole time by `set_a`), pressure is applied
    // while both hold it, then the successor is installed and the predecessor
    // released. No instant leaves `produced` unprotected.
    let mut set_b = RetainedPinSet::new();
    assert!(set_b.lease(leaf.pin_terminal(&produced).unwrap()));
    assert_eq!(runtime.metrics().active_retained_pins, 2);
    assert_eq!(runtime.metrics().peak_retained_pins, 2);
    for i in 9..=16 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    let published = std::mem::replace(&mut set_a, set_b); // install successor
    drop(published); // then release the predecessor's pin
    assert_eq!(runtime.metrics().active_retained_pins, 1);
    let reused = runtime
        .query(
            &leaf,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            |_| panic!("the handed-off terminal must survive the swap"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&reused, &produced));
    assert_eq!(computes.load(Ordering::SeqCst), 1, "never recomputed");

    // Dropping the last set finally releases the terminal: it is no longer a
    // protected root, so ordinary pressure can evict it.
    let evictions_before = runtime.metrics().evictions;
    drop(set_a);
    assert_eq!(runtime.metrics().active_retained_pins, 0);
    for i in 17..=24 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    assert!(
        runtime.metrics().evictions > evictions_before,
        "the released terminal is now evictable under pressure"
    );
}
// A selected result's attempt-carried bridge lease ends the instant selection
// registers a successor protection — not when the (possibly long-ledgered)
// attempt finally drops. Protection is continuous across the handoff.
#[test]
fn selected_result_releases_bridge_lease_before_attempt_drop() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime.family::<Slot, u64>("bridge-release", 1).unwrap();

    // Complete a request; its attempt carries a bridge lease on the result.
    // Keep the attempt alive for the whole test — standing in for the
    // compiler's bounded attempt ledger retaining completed attempts.
    let computes = Arc::new(AtomicUsize::new(0));
    let counter = computes.clone();
    let attempt = runtime.request(
        &family,
        revision(1),
        Slot(0),
        CancellationToken::new(),
        move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(QueryOutput::success(0))
        },
    );
    let produced = attempt
        .terminal()
        .expect("request produced a terminal")
        .clone();
    assert_eq!(computes.load(Ordering::SeqCst), 1);

    // Register a successor protection, then end the bridge. Order matters: the
    // selection pins first, so releasing the bridge is a pure narrowing with
    // no instant in which `produced` is unprotected.
    let mut selection = family.selection();
    selection.publish(&produced).unwrap();
    attempt.release_result_lease();

    // While the selection holds it, the result survives eviction pressure —
    // the handoff left no unprotected instant.
    for i in 1..=8 {
        runtime
            .query(
                &family,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    let still = runtime
        .query(
            &family,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            |_| panic!("the selection must retain the result"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&still, &produced));

    // Drop the successor. Nothing protects the result now — the bridge is
    // already gone though the attempt is still alive. Under the pre-fix leak
    // the ledgered attempt would keep pinning it and it would never evict.
    drop(selection);
    for i in 9..=16 {
        runtime
            .query(
                &family,
                revision(1),
                Slot(i),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(i)),
            )
            .unwrap();
    }
    let recompute_counter = computes.clone();
    let recomputed = runtime
        .query(
            &family,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            move |_| {
                recompute_counter.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();
    assert!(
        !Arc::ptr_eq(&recomputed, &produced),
        "the released bridge let the unprotected result evict"
    );
    assert_eq!(
        computes.load(Ordering::SeqCst),
        2,
        "the evicted result had to be recomputed after the bridge released"
    );

    // The bridge ended at selection, not at attempt drop: the attempt is only
    // dropped now, at the end of the test.
    assert!(attempt.terminal().is_some());
    drop(attempt);
}
/// An adopted terminal is recorded with its EXACT identity — node,
/// incarnation, and stamp — and the endorsement makes the dependent
/// validate green at the adopting revision even though the adopted
/// terminal's original leaf does not exist there. No key hash or content
/// comparison is involved.
#[test]
fn adopted_terminal_is_an_exact_dependency_across_revisions() {
    let runtime = QueryRuntime::new(1);
    let family = runtime
        .content_addressed_family_with_equality::<Key, u64>("adopt", 8, PartialEq::eq)
        .unwrap();
    let dependents = runtime.family::<Key, u64>("adopt-dependent", 8).unwrap();
    let source = InputIdentity::new("source", "a.rue");
    let parent = Revision::new(1, 7);
    let successor = Revision::new(2, 7);
    runtime
        .publish_revision(parent, [(source.clone(), 11)])
        .unwrap();
    // The successor revision does NOT carry the adopted terminal's leaf.
    runtime.publish_revision(successor, []).unwrap();
    let predecessor = runtime
        .query(&family, parent, Key("pred"), CancellationToken::new(), {
            let source = source.clone();
            move |context| {
                context.input(source.clone())?;
                Ok(QueryOutput::success(7))
            }
        })
        .unwrap();

    let before_adoption = runtime.metrics().validation;
    let dependent = runtime
        .query(
            &dependents,
            successor,
            Key("succ"),
            CancellationToken::new(),
            {
                let family = family.clone();
                let adoptable = family.adoptable_terminal(&predecessor).unwrap();
                move |context| {
                    family
                        .observe_adopted_terminal(context, &adoptable)
                        .unwrap();
                    Ok(QueryOutput::success(8))
                }
            },
        )
        .unwrap();
    let adoption_work = runtime.metrics().validation.saturating_sub(before_adoption);
    assert_eq!(
        adoption_work.terminal_lease_observations, 1,
        "the dependent query is metered, but exact-capability adoption does not visit a query memo"
    );
    assert_eq!(adoption_work.duplicate_terminal_lease_observations, 0);
    // The recorded observation is the held terminal's exact identity.
    assert_eq!(dependent.dependencies().len(), 1);
    let observation = &dependent.dependencies()[0];
    assert_eq!(observation.node, *predecessor.node());
    assert_eq!(observation.incarnation, predecessor.node_incarnation());
    assert_eq!(observation.stamp, predecessor.stamp());
    // The dependent revalidates green at the adopting revision through the
    // endorsement: the compute never re-runs.
    let reused = runtime
        .query(
            &dependents,
            successor,
            Key("succ"),
            CancellationToken::new(),
            |_| panic!("a green adopted dependency must reuse the terminal"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&reused, &dependent));
}
/// Recording a stale or evicted terminal is REJECTED — never silently
/// re-derived — and a terminal of another family is refused outright.
#[test]
fn adopting_a_stale_or_foreign_terminal_is_rejected() {
    let runtime = QueryRuntime::new(1);
    let family = runtime
        .content_addressed_family_with_equality::<Key, u64>("adopt-stale", 1, PartialEq::eq)
        .unwrap();
    let dependents = runtime
        .content_addressed_family_with_equality::<Key, u64>(
            "adopt-stale-dependent",
            8,
            PartialEq::eq,
        )
        .unwrap();
    let parent = Revision::new(1, 7);
    let successor = Revision::new(2, 7);
    publish_empty(&runtime, [parent, successor]);
    let stale = runtime
        .query(
            &family,
            parent,
            Key("old"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(1)),
        )
        .unwrap();
    // Flood the retention-1 family so the held terminal is evicted.
    runtime
        .query(
            &family,
            parent,
            Key("new"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(2)),
        )
        .unwrap();
    runtime
        .query(
            &dependents,
            successor,
            Key("succ"),
            CancellationToken::new(),
            {
                let family = family.clone();
                let dependents = dependents.clone();
                let stale = family.adoptable_terminal(&stale).unwrap();
                move |context| {
                    assert_eq!(
                        family.observe_adopted_terminal(context, &stale),
                        Err(AdoptTerminalError::Evicted),
                        "an evicted terminal must be rejected, not re-derived"
                    );
                    assert_eq!(
                        dependents.observe_adopted_terminal(context, &stale),
                        Err(AdoptTerminalError::ForeignFamily),
                        "another family's terminal must be refused"
                    );
                    Ok(QueryOutput::success(0))
                }
            },
        )
        .unwrap();
}
/// The content-addressed registration is the SOLE minting authority for
/// adoption capabilities: an ordinary input-dependent family cannot mint
/// one at all, so it can never endorse a stale value input-free after its
/// input changes — the unsound path is unreachable, not merely
/// discouraged.
#[test]
fn ordinary_input_dependent_family_cannot_mint_adoption_capability() {
    let runtime = QueryRuntime::new(1);
    let family = runtime.family::<Key, u64>("adopt-ordinary", 8).unwrap();
    let source = InputIdentity::new("source", "a.rue");
    let parent = Revision::new(1, 7);
    runtime
        .publish_revision(parent, [(source.clone(), 11)])
        .unwrap();
    // An input-DEPENDENT terminal: its value would change with the leaf.
    let terminal = runtime
        .query(&family, parent, Key("k"), CancellationToken::new(), {
            let source = source.clone();
            move |context| {
                let stamp = context.input(source.clone())?;
                Ok(QueryOutput::success(stamp))
            }
        })
        .unwrap();
    // The leaf changes in a later revision; endorsing the old terminal
    // there would validate a stale value green — minting is refused.
    assert_eq!(
        family.adoptable_terminal(&terminal).unwrap_err(),
        AdoptTerminalError::NotContentAddressed,
    );
}
/// Endorsements are ordinary retained publications: metered, enqueued for
/// retention, and evictable. An adoption chain longer than the family's
/// retention limit stays bounded — old endorsements are deterministically
/// evicted rather than accumulating unevictable attempts.
#[test]
fn adoption_chain_is_bounded_by_family_retention() {
    const LIMIT: usize = 4;
    let runtime = QueryRuntime::new(1);
    let family = runtime
        .content_addressed_family_with_equality::<Key, u64>("adopt-bound", LIMIT, PartialEq::eq)
        .unwrap();
    let dependents = runtime
        .family::<Slot, u64>("adopt-bound-dependent", LIMIT)
        .unwrap();
    let parent = Revision::new(1, 7);
    runtime.publish_revision(parent, []).unwrap();
    let predecessor = runtime
        .query(
            &family,
            parent,
            Key("pred"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(7)),
        )
        .unwrap();
    let adoptable = family.adoptable_terminal(&predecessor).unwrap();
    // Keep the ORIGINAL terminal protected (as a live selection would);
    // its endorsements are unprotected and must cycle out.
    let _pin = family.pin_terminal(&predecessor).unwrap();
    // Adopt across far more distinct revisions than the retention limit.
    for round in 2..(LIMIT as u64 * 8) {
        let revision = Revision::new(round, 7);
        runtime.publish_revision(revision, []).unwrap();
        runtime
            .query(
                &dependents,
                revision,
                Slot(round),
                CancellationToken::new(),
                {
                    let family = family.clone();
                    let adoptable = adoptable.clone();
                    move |context| {
                        family
                            .observe_adopted_terminal(context, &adoptable)
                            .unwrap();
                        Ok(QueryOutput::success(round))
                    }
                },
            )
            .unwrap();
        // Bounded: retained terminals never exceed the configured limit
        // by more than the protected roots (the pinned original).
        let retention = family.retention();
        assert!(
            retention.terminals <= LIMIT + 1,
            "adoption endorsements must stay bounded by family retention: {retention:?}"
        );
    }
}
/// Endorsement protection is atomic with insertion, and its lease
/// identity is distinct from its predecessor's. At retention limit 1 the
/// enforcement rotations themselves are the pressure: one pass runs inside
/// the endorsement's own insertion (where a zero-pin endorsement would be
/// evicted at birth while the pinned predecessor rotates through), one at
/// every pin release (where a lease identity collapsed with the
/// predecessor's `(incarnation, stamp)` would drop the endorsement's pin
/// and evict it on the spot), and one at request teardown. The adopting
/// attempt's lease holds the endorsement through all of them, so the
/// dependent still revalidates GREEN without recomputation.
#[test]
fn adoption_survives_birth_eviction_pressure_at_retention_limit_one() {
    let runtime = QueryRuntime::new(1);
    let family = runtime
        .content_addressed_family_with_equality::<Key, u64>("adopt-birth", 1, PartialEq::eq)
        .unwrap();
    let dependents = runtime
        .family::<Key, u64>("adopt-birth-dependent", 8)
        .unwrap();
    let source = InputIdentity::new("source", "a.rue");
    let parent = Revision::new(1, 7);
    let successor = Revision::new(2, 7);
    runtime
        .publish_revision(parent, [(source.clone(), 11)])
        .unwrap();
    // The successor revision lacks the predecessor's source leaf, so ONLY
    // the endorsement can validate the recorded dependency there.
    runtime.publish_revision(successor, []).unwrap();
    let predecessor = runtime
        .query(&family, parent, Key("pred"), CancellationToken::new(), {
            let source = source.clone();
            move |context| {
                context.input(source.clone())?;
                Ok(QueryOutput::success(7))
            }
        })
        .unwrap();
    let adoptable = family.adoptable_terminal(&predecessor).unwrap();
    let computes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dependent = runtime
        .query(
            &dependents,
            successor,
            Key("succ"),
            CancellationToken::new(),
            {
                let family = family.clone();
                let adoptable = adoptable.clone();
                let computes = computes.clone();
                move |context| {
                    computes.fetch_add(1, Ordering::SeqCst);
                    family
                        .observe_adopted_terminal(context, &adoptable)
                        .unwrap();
                    Ok(QueryOutput::success(8))
                }
            },
        )
        .unwrap();
    assert_eq!(computes.load(Ordering::SeqCst), 1);
    // The retention-1 bound held: at most one unprotected terminal
    // survives the teardown rotation, and it is the ENDORSEMENT (the
    // predecessor rotated out), so the family stays within its bound.
    let retention = family.retention();
    assert!(
        retention.terminals <= 1,
        "the retention bound must hold after the adopting request: {retention:?}"
    );
    // The dependent revalidates GREEN through the surviving endorsement:
    // the compute never re-runs.
    let reused = runtime
        .query(
            &dependents,
            successor,
            Key("succ"),
            CancellationToken::new(),
            |_| panic!("a green adopted dependency must reuse the terminal"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&reused, &dependent));
    assert_eq!(computes.load(Ordering::SeqCst), 1);
}
/// Exact-terminal adoption never touches the predecessor key's `Hash` or
/// `Eq`: after the predecessor is computed, its key instrumentation is
/// FROZEN (any further hash or equality panics), and adoption still
/// succeeds — the node is located by incarnation alone.
#[test]
fn adoption_never_hashes_or_compares_the_predecessor_key() {
    #[derive(Clone)]
    struct FrozenKey {
        name: &'static str,
        frozen: Arc<std::sync::atomic::AtomicBool>,
    }
    impl std::hash::Hash for FrozenKey {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            assert!(
                !self.frozen.load(Ordering::SeqCst),
                "the predecessor key was hashed after freeze"
            );
            self.name.hash(state);
        }
    }
    impl PartialEq for FrozenKey {
        fn eq(&self, other: &Self) -> bool {
            assert!(
                !self.frozen.load(Ordering::SeqCst),
                "the predecessor key was equality-compared after freeze"
            );
            self.name == other.name
        }
    }
    impl Eq for FrozenKey {}
    impl QueryKey for FrozenKey {
        fn stable_identity(&self) -> String {
            self.name.to_owned()
        }

        fn stable_hash(&self, hasher: &mut StableHasher) {
            self.name.hash(hasher);
        }
    }

    let runtime = QueryRuntime::new(1);
    let family = runtime
        .content_addressed_family_with_equality::<FrozenKey, u64>("adopt-frozen", 8, PartialEq::eq)
        .unwrap();
    let dependents = runtime
        .family::<Key, u64>("adopt-frozen-dependent", 8)
        .unwrap();
    let parent = Revision::new(1, 7);
    let successor = Revision::new(2, 7);
    publish_empty(&runtime, [parent, successor]);
    let frozen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let key = FrozenKey {
        name: "pred",
        frozen: frozen.clone(),
    };
    let predecessor = runtime
        .query(&family, parent, key, CancellationToken::new(), |_| {
            Ok(QueryOutput::success(7))
        })
        .unwrap();
    let adoptable = family.adoptable_terminal(&predecessor).unwrap();
    // From here on, ANY hash or equality of the predecessor key panics.
    frozen.store(true, Ordering::SeqCst);
    let dependent = runtime
        .query(
            &dependents,
            successor,
            Key("succ"),
            CancellationToken::new(),
            {
                let family = family.clone();
                let adoptable = adoptable.clone();
                move |context| {
                    family
                        .observe_adopted_terminal(context, &adoptable)
                        .unwrap();
                    Ok(QueryOutput::success(8))
                }
            },
        )
        .unwrap();
    assert_eq!(dependent.dependencies().len(), 1);
    assert_eq!(dependent.dependencies()[0].stamp, predecessor.stamp());
    // Reuse validation of the dependent likewise never touches the key.
    let reused = runtime
        .query(
            &dependents,
            successor,
            Key("succ"),
            CancellationToken::new(),
            |_| panic!("a green adopted dependency must reuse the terminal"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&reused, &dependent));
}
#[test]
fn family_estimator_charges_success_without_an_output_override() {
    let runtime = QueryRuntime::new(1);
    let family = runtime
        .family_with_equality_and_retained_charge::<Key, u64>(
            "charged-family",
            8,
            PartialEq::eq,
            |_| 1234,
        )
        .unwrap();
    publish_empty(&runtime, [revision(1)]);
    let terminal = runtime
        .query(
            &family,
            revision(1),
            Key("value"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(7)),
        )
        .unwrap();
    assert!(terminal.retained_charge() >= 1234);
    assert_eq!(runtime.metrics().retained_bytes, terminal.retained_charge());
}
#[test]
fn family_watermarks_bound_cross_family_probe_count() {
    let runtime = QueryRuntime::with_retention_budgets(
        1,
        RetentionBudgets {
            retained_bytes: 1024 * 1024 * 1024,
            dependency_pins: u64::MAX,
        },
    );
    let family = runtime
        .family_with_equality_and_retained_charge::<Key, u64>(
            "probe-quantum",
            512,
            PartialEq::eq,
            |_| 64 * 1024,
        )
        .unwrap();
    publish_empty(&runtime, [revision(1)]);
    for index in 0..256 {
        let key = Box::leak(format!("probe-{index}").into_boxed_str());
        runtime
            .query(
                &family,
                revision(1),
                Key(key),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(index)),
            )
            .unwrap();
    }
    let metrics = runtime.metrics();
    assert!(metrics.aggregate_retention_probes > 0);
    assert!(
        metrics.aggregate_retention_probes <= 3,
        "a family probes by charge quantum, not per publication: {metrics:?}"
    );
}
#[test]
fn byte_probe_rebases_after_reclaim_and_enforces_on_regrowth() {
    let budgets = RetentionBudgets {
        retained_bytes: 1024,
        dependency_pins: u64::MAX,
    };
    let mut retention = FamilyRetentionQueue::<Key, u64>::new(budgets);
    let quantum = retention.byte_probe_quantum;
    assert!(quantum > 1);

    assert!(retention.publish(
        RetentionEntry {
            node: Weak::new(),
            attempt: 1,
        },
        quantum * 8,
        0,
    ));
    retention.remove_charge(quantum * 7, 0);
    assert!(retention.next_byte_probe - retention.retained_bytes <= quantum);

    assert!(!retention.publish(
        RetentionEntry {
            node: Weak::new(),
            attempt: 2,
        },
        quantum - 1,
        0,
    ));
    assert!(retention.publish(
        RetentionEntry {
            node: Weak::new(),
            attempt: 3,
        },
        1,
        0,
    ));
}
#[test]
fn dependency_pin_probe_rebases_after_reclaim_and_enforces_on_regrowth() {
    let budgets = RetentionBudgets {
        retained_bytes: u64::MAX,
        dependency_pins: 1024,
    };
    let mut retention = FamilyRetentionQueue::<Key, u64>::new(budgets);
    let quantum = retention.pin_probe_quantum;
    assert!(quantum > 1);

    assert!(retention.publish(
        RetentionEntry {
            node: Weak::new(),
            attempt: 1,
        },
        0,
        quantum * 8,
    ));
    retention.remove_charge(0, quantum * 7);
    assert!(retention.next_pin_probe - retention.dependency_pins <= quantum);

    assert!(!retention.publish(
        RetentionEntry {
            node: Weak::new(),
            attempt: 2,
        },
        0,
        quantum - 1,
    ));
    assert!(retention.publish(
        RetentionEntry {
            node: Weak::new(),
            attempt: 3,
        },
        0,
        1,
    ));
}
#[test]
fn byte_pressure_evicts_in_stable_family_round_robin_order() {
    let unit = budget_unit_charge(100);
    let runtime = QueryRuntime::with_retention_budgets(
        1,
        RetentionBudgets {
            retained_bytes: unit * 2,
            dependency_pins: u64::MAX,
        },
    );
    let first = runtime
        .family_with_equality_and_retained_charge::<Key, u64>("budget-a", 8, PartialEq::eq, |_| 100)
        .unwrap();
    let second = runtime
        .family_with_equality_and_retained_charge::<Key, u64>("budget-b", 8, PartialEq::eq, |_| 100)
        .unwrap();
    publish_empty(&runtime, [revision(1)]);
    let caller_held = runtime
        .query(
            &first,
            revision(1),
            Key("0"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(0)),
        )
        .unwrap();
    for (key, value) in [("0", 1), ("1", 2)] {
        runtime
            .query(
                &second,
                revision(1),
                Key(key),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(value)),
            )
            .unwrap();
    }
    assert_eq!(caller_held.outcome(), &QueryOutcome::Success(0));
    assert_eq!(first.retention().terminals, 0);
    assert_eq!(second.retention().terminals, 2);
    runtime
        .query(
            &first,
            revision(1),
            Key("1"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(3)),
        )
        .unwrap();
    // A later pressure event resumes after the family evicted above rather
    // than always restarting at the lowest family token.
    assert_eq!(first.retention().terminals, 1);
    assert_eq!(second.retention().terminals, 1);
    let metrics = runtime.metrics();
    assert_eq!(metrics.retained_bytes, unit * 2);
    assert_eq!(metrics.retained_byte_evictions, 2);
}
#[test]
fn aggregate_sweep_bookkeeping_does_not_scale_with_evictions() {
    // RUE-1850: the sweep used to re-sum every registered family's charge
    // before each single-terminal eviction attempt. Each sum acquires every
    // family's retention mutex — the same lock publishers and evictors need —
    // so a sweep evicting E terminals cost O(E x families) lock cycles purely
    // for bookkeeping.
    //
    // Evictions now report the charge they reclaim, so the aggregate is
    // decremented in place and summed once per round. A sweep therefore takes a
    // fixed number of sums regardless of how many terminals it evicts: one on
    // entry, one per round, and one to record the final retained figures. The
    // pre-fix code took one more per eviction attempt on top of that, so a
    // regression pushes the ratio past this bound.
    const KEYS: [&str; 24] = [
        "k00", "k01", "k02", "k03", "k04", "k05", "k06", "k07", "k08", "k09", "k10", "k11", "k12",
        "k13", "k14", "k15", "k16", "k17", "k18", "k19", "k20", "k21", "k22", "k23",
    ];
    const MAX_SUMS_PER_SWEEP: u64 = 4;

    let unit = budget_unit_charge(100);
    let runtime = QueryRuntime::with_retention_budgets(
        1,
        RetentionBudgets {
            retained_bytes: unit,
            dependency_pins: u64::MAX,
        },
    );
    let family = runtime
        .family_with_equality_and_retained_charge::<Key, u64>("sweep", 64, PartialEq::eq, |_| 100)
        .unwrap();
    publish_empty(&runtime, [revision(1)]);
    for (index, key) in KEYS.iter().enumerate() {
        runtime
            .query(
                &family,
                revision(1),
                Key(key),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(index as u64)),
            )
            .unwrap();
    }

    let metrics = runtime.metrics();
    assert!(
        metrics.retained_byte_evictions > 0,
        "fixture did not apply byte pressure"
    );
    assert!(
        metrics.retention_charge_snapshots
            <= metrics.aggregate_retention_probes * MAX_SUMS_PER_SWEEP,
        "cross-family charge sums ({}) exceeded {MAX_SUMS_PER_SWEEP} per sweep \
         ({} probes) while evicting {} terminals — the per-candidate re-sum is back",
        metrics.retention_charge_snapshots,
        metrics.aggregate_retention_probes,
        metrics.retained_byte_evictions,
    );
}
#[test]
fn protected_byte_overflow_reclaims_when_request_bridge_releases() {
    let unit = budget_unit_charge(100);
    let runtime = QueryRuntime::with_retention_budgets(
        1,
        RetentionBudgets {
            retained_bytes: unit,
            dependency_pins: u64::MAX,
        },
    );
    let family = runtime
        .family_with_equality_and_retained_charge::<Key, u64>("protected", 8, PartialEq::eq, |_| {
            100
        })
        .unwrap();
    publish_empty(&runtime, [revision(1)]);
    let first = runtime.request(
        &family,
        revision(1),
        Key("0"),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(0)),
    );
    let second = runtime.request(
        &family,
        revision(1),
        Key("1"),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(1)),
    );
    assert!(runtime.metrics().retained_byte_overflow_events > 0);
    assert_eq!(runtime.metrics().retained_bytes, unit * 2);
    drop(second);
    assert_eq!(runtime.metrics().retained_bytes, unit);
    assert_eq!(family.retention().terminals, 1);
    drop(first);
}
#[test]
fn protected_overflow_does_not_repeat_aggregate_scans_below_watermark() {
    let unit = budget_unit_charge(100);
    let runtime = QueryRuntime::with_retention_budgets(
        1,
        RetentionBudgets {
            retained_bytes: unit,
            dependency_pins: u64::MAX,
        },
    );
    let family = runtime
        .family_with_equality_and_retained_charge::<Key, u64>("watermark", 0, PartialEq::eq, |_| {
            100
        })
        .unwrap();
    publish_empty(&runtime, [revision(1)]);
    let first = runtime.request(
        &family,
        revision(1),
        Key("0"),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(0)),
    );
    let second = runtime.request(
        &family,
        revision(1),
        Key("1"),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(1)),
    );
    let before = runtime.metrics();
    let third = runtime.request(
        &family,
        revision(1),
        Key("2"),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(2)),
    );
    let after = runtime.metrics();

    assert_eq!(
        after.retained_byte_pressure_events, before.retained_byte_pressure_events,
        "family-limit enforcement must not bypass the aggregate watermark"
    );
    assert_eq!(
        after.retention_scan_entries - before.retention_scan_entries,
        0,
        "both family and aggregate geometric watermarks suppress a protected rescan"
    );
    drop((third, second, first));
}
#[test]
fn aggregate_pressure_respects_selection_and_revision_roots() {
    let make_runtime = || {
        QueryRuntime::with_retention_budgets(
            1,
            RetentionBudgets {
                retained_bytes: 0,
                dependency_pins: u64::MAX,
            },
        )
    };

    let runtime = make_runtime();
    let family = runtime.family::<Key, u64>("selection-root", 8).unwrap();
    publish_empty(&runtime, [revision(1)]);
    let attempt = runtime.request(
        &family,
        revision(1),
        Key("value"),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(1)),
    );
    let mut selection = family.selection();
    selection.publish(attempt.terminal().unwrap()).unwrap();
    attempt.release_result_lease();
    drop(attempt);
    assert!(runtime.metrics().retained_bytes > 0);
    drop(selection);
    assert_eq!(runtime.metrics().retained_bytes, 0);

    let runtime = make_runtime();
    let family = runtime.family::<Key, u64>("revision-root", 8).unwrap();
    publish_empty(&runtime, [revision(1)]);
    let attempt = runtime.request(
        &family,
        revision(1),
        Key("value"),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(1)),
    );
    let revision_root = family.retain_revision(revision(1));
    attempt.release_result_lease();
    drop(attempt);
    assert!(runtime.metrics().retained_bytes > 0);
    drop(revision_root);
    assert_eq!(runtime.metrics().retained_bytes, 0);
}
#[test]
fn dependency_observation_budget_reclaims_pull_validation_edges() {
    let runtime = QueryRuntime::with_retention_budgets(
        1,
        RetentionBudgets {
            retained_bytes: u64::MAX,
            dependency_pins: 0,
        },
    );
    let leaf = runtime.family::<Key, u64>("pin-leaf", 8).unwrap();
    let root = runtime.family::<Key, u64>("pin-root", 8).unwrap();
    publish_empty(&runtime, [revision(1)]);
    runtime
        .query(
            &leaf,
            revision(1),
            Key("leaf"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(1)),
        )
        .unwrap();
    let rooted = runtime
        .query(
            &root,
            revision(1),
            Key("root"),
            CancellationToken::new(),
            |context| {
                context.query(&leaf, Key("leaf"), |_| {
                    panic!("the retained leaf is reused")
                })?;
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();
    assert_eq!(rooted.dependencies().len(), 1);
    let metrics = runtime.metrics();
    assert_eq!(metrics.retained_dependency_pins, 0);
    assert!(metrics.dependency_pin_overflow_events > 0);
    assert!(metrics.dependency_pin_evictions > 0);
}
#[test]
fn concurrent_publisher_hands_pending_pressure_to_sweep_owner() {
    let runtime = QueryRuntime::with_retention_budgets(
        2,
        RetentionBudgets {
            retained_bytes: 0,
            dependency_pins: u64::MAX,
        },
    );
    let family = runtime.family::<Key, u64>("sweep-handoff", 8).unwrap();
    publish_empty(&runtime, [revision(1)]);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let blocked_once = Arc::new(AtomicBool::new(false));
    runtime.set_interpose(Arc::new({
        let entered = entered.clone();
        let release = release.clone();
        move |site| {
            if site == InterposeSite::RetentionSweepRelease
                && !blocked_once.swap(true, Ordering::SeqCst)
            {
                entered.wait();
                release.wait();
            }
        }
    }));
    let first = thread::spawn({
        let runtime = runtime.clone();
        let family = family.clone();
        move || {
            runtime
                .query(
                    &family,
                    revision(1),
                    Key("a"),
                    CancellationToken::new(),
                    |_| Ok(QueryOutput::success(1)),
                )
                .unwrap()
        }
    });
    entered.wait();
    let second = thread::spawn({
        let runtime = runtime.clone();
        let family = family.clone();
        move || {
            runtime
                .query(
                    &family,
                    revision(1),
                    Key("b"),
                    CancellationToken::new(),
                    |_| Ok(QueryOutput::success(2)),
                )
                .unwrap()
        }
    });
    let held_second = second.join().unwrap();
    release.wait();
    let held_first = first.join().unwrap();
    assert_eq!(held_first.outcome(), &QueryOutcome::Success(1));
    assert_eq!(held_second.outcome(), &QueryOutcome::Success(2));
    assert_eq!(runtime.metrics().retained_bytes, 0);
    assert_eq!(family.retention().terminals, 0);
}
#[test]
fn dropping_last_family_releases_charge_while_terminal_arc_lives() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let terminal = {
        let family = runtime.family::<Key, u64>("family-drop", 8).unwrap();
        runtime
            .query(
                &family,
                revision(1),
                Key("value"),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(7)),
            )
            .unwrap()
    };
    assert_eq!(runtime.metrics().retained_bytes, 0);
    assert_eq!(runtime.metrics().retained_terminals, 0);
    assert_eq!(terminal.outcome(), &QueryOutcome::Success(7));
}

#[test]
fn failures_are_reused_and_retention_respects_pins() {
    let runtime = QueryRuntime::new(1);
    publish_empty(
        &runtime,
        [
            revision(1),
            Revision::new(99, 1),
            revision(2),
            revision(3),
            revision(4),
        ],
    );
    let family = runtime.family::<Key, u64>("retention", 2).unwrap();
    let failed = runtime
        .query(
            &family,
            revision(1),
            Key("failure"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::failure(QueryFailure::new("E1", "broken"))),
        )
        .unwrap();
    let reused_failure = runtime
        .query(
            &family,
            Revision::new(99, 1),
            Key("failure"),
            CancellationToken::new(),
            |_| panic!("a retained failure validates like a success"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&failed, &reused_failure));
    let terminal_pin = family.pin_terminal(&failed).unwrap();
    let second_pin = family.retain_revision(revision(2));
    let third_pin = family.retain_revision(revision(3));
    for (id, key) in [(2, "second"), (3, "third"), (4, "fourth")] {
        runtime
            .query(
                &family,
                revision(id),
                Key(key),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(id)),
            )
            .unwrap();
    }
    assert_eq!(
        failed.outcome(),
        &QueryOutcome::Failure(QueryFailure::new("E1", "broken"))
    );
    assert!(runtime.metrics().retained_terminals >= 3);
    drop(terminal_pin);
    drop(second_pin);
    drop(third_pin);
    assert_eq!(runtime.metrics().retained_terminals, 2);
    assert!(runtime.metrics().evictions >= 2);
}
