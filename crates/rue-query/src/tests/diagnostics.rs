//! Query runtime tests: diagnostics subsystem.

use super::fixtures::*;
use super::*;

#[test]
fn structured_wait_labels_remain_lazy_and_live_until_cycle_rendering() {
    let runtime = QueryRuntime::new(1);
    let items = Arc::new(RegisteredBatchItems {
        family: Arc::from("lazy-structured"),
        items: vec![RegisteredBatchItem {
            request_id: 2,
            key: Key("child"),
            ready_at: Instant::now(),
        }],
    });
    let weak_items = Arc::downgrade(&items);
    let labels: Arc<dyn StructuredWaitLabels> = items.clone();
    let guard = StructuredWaitGuard::new(
        runtime.core.clone(),
        TaskId(1),
        labels.clone(),
        [(TaskId(2), 0)],
    )
    .expect("an acyclic structured edge is registered");

    assert_eq!(
        runtime
            .metrics()
            .display_identities
            .structured_wait_materializations,
        0,
        "registering an ordinary structured wait never formats its key"
    );
    drop(items);
    drop(labels);
    assert!(
        weak_items.upgrade().is_some(),
        "the live wait edge retains its batch label table"
    );

    let cycle = runtime
        .core
        .begin_wait(
            TaskId(2),
            TaskId(1),
            WaitEdgeLabel::Materialized(NodeIdentity::new(
                Arc::from("ordinary"),
                Arc::from("root"),
            )),
        )
        .expect_err("the reverse wait closes a cycle");
    assert_eq!(
        cycle
            .iter()
            .map(|node| (node.family(), node.key()))
            .collect::<Vec<_>>(),
        vec![("lazy-structured", "child"), ("ordinary", "root")],
        "lazy rendering preserves the exact canonical cycle text"
    );
    assert_eq!(
        runtime.metrics().display_identities,
        DisplayIdentityMetrics {
            structured_wait_materializations: 1,
            structured_wait_bytes: 5,
            ..DisplayIdentityMetrics::default()
        }
    );

    drop(guard);
    assert!(
        lock(&runtime.core.wait_graph).is_empty(),
        "dropping the batch removes its label table with the wait edge"
    );
    assert!(
        weak_items.upgrade().is_none(),
        "removing the last wait edge releases its batch label table"
    );
}
#[test]
fn terminal_diagnostics_and_work_are_worker_order_independent() {
    fn run(workers: usize, reverse: bool) -> Arc<QueryTerminal<u64>> {
        let runtime = QueryRuntime::new(workers);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime.family::<Key, u64>("determinism", 2).unwrap();
        runtime
            .query(
                &family,
                revision(1),
                Key("root"),
                CancellationToken::new(),
                move |_| {
                    let mut diagnostics = vec![
                        QueryDiagnostic::new("b", "two", None),
                        QueryDiagnostic::new("a", "one", None),
                    ];
                    let mut work = vec![
                        WorkItem::new("lowered", 2),
                        WorkItem::new("visited", 1),
                        WorkItem::new("lowered", 3),
                    ];
                    if reverse {
                        diagnostics.reverse();
                        work.reverse();
                    }
                    Ok(QueryOutput::success(7)
                        .with_diagnostics(diagnostics)
                        .with_work(work))
                },
            )
            .unwrap()
    }

    let serial = run(1, false);
    let parallel = run(8, true);
    assert_eq!(serial.outcome(), parallel.outcome());
    assert_eq!(serial.diagnostics(), parallel.diagnostics());
    assert_eq!(serial.work(), parallel.work());
    assert_eq!(serial.dependencies(), parallel.dependencies());
    assert_eq!(serial.stamp(), parallel.stamp());
}
#[test]
fn revisions_and_key_identities_fail_closed_on_conflicts() {
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CollidingKey(u8);

    // Force every key into a single hash bucket. Distinct keys must still
    // resolve to distinct memo nodes through exact `Eq`; hashing is only a
    // bucketing hint. `Hash` agrees with `Eq` (all keys hash identically,
    // which is permitted — only inequality of hashes across equal values
    // would be a bug).
    impl std::hash::Hash for CollidingKey {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            state.write_u8(0);
        }
    }

    impl QueryKey for CollidingKey {
        /// Deliberately equal for unequal keys. Presentation text is
        /// allowed to collide, which is exactly why ADR-0074 does not let
        /// it decide identity.
        fn stable_identity(&self) -> String {
            "collision".to_owned()
        }

        /// The bucketing `Hash` above is deliberately constant; the
        /// structural digest is not. It absorbs the key's real field, so
        /// two unequal keys get two identities even though they render
        /// one name.
        fn stable_hash(&self, hasher: &mut StableHasher) {
            self.0.hash(hasher);
        }
    }

    let runtime = QueryRuntime::new(1);
    let input = InputIdentity::new("source", "main.rue");
    assert_eq!(
        runtime.publish_revision(revision(1), [(input.clone(), 1), (input.clone(), 2)]),
        Err(RevisionError::ConflictingInput(input))
    );
    publish_empty(&runtime, [revision(1)]);

    let family = runtime
        .family::<CollidingKey, u64>("colliding-keys", 2)
        .unwrap();
    let first = runtime
        .query(
            &family,
            revision(1),
            CollidingKey(1),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(1)),
        )
        .unwrap();
    let second = runtime
        .query(
            &family,
            revision(1),
            CollidingKey(2),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(2)),
        )
        .unwrap();
    // The two keys are deliberately unequal yet hash into one bucket, so the
    // memo map must fall back to exact `Eq` to keep them apart.
    assert_ne!(CollidingKey(1), CollidingKey(2));
    assert_eq!(hash_of(&CollidingKey(1)), hash_of(&CollidingKey(2)));
    assert_eq!(first.node().family(), "colliding-keys");
    // Both nodes render one name, because `stable_identity` is allowed to
    // collide for unequal keys.
    assert_eq!(first.node().key(), "collision");
    assert_eq!(second.node().key(), "collision");
    // ADR-0074 contract change: identity is structural, so two unequal
    // keys are two identities even when they render the same text. Before
    // ADR-0074 identity *was* the rendered pair and these compared equal;
    // making display text decide identity is precisely what this fixture
    // shows to be unsound. The schedule-dependent incarnation still stays
    // out of the canonical order.
    assert_ne!(first.node(), second.node());
    assert_ne!(first.node().stable_hash(), second.node().stable_hash());
    assert!(!Arc::ptr_eq(&first, &second));
    assert_ne!(first.outcome(), second.outcome());
}
