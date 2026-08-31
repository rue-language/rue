//! Query runtime tests: identity digest subsystem.

use super::fixtures::*;
use super::*;

/// A key whose presentation text is equal for unequal keys, which
/// [`QueryKey::stable_identity`] explicitly permits. Text therefore cannot
/// break an identity tie, and ADR-0074 does not ask it to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EqualTextKey(u8);

impl QueryKey for EqualTextKey {
    fn stable_identity(&self) -> String {
        "collision".to_owned()
    }

    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.0.hash(hasher);
    }
}

static CONTAINS_HASH_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CountingKey(u64);

impl std::hash::Hash for CountingKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        CONTAINS_HASH_CALLS.fetch_add(1, Ordering::Relaxed);
        state.write_u64(self.0);
    }
}

impl QueryKey for CountingKey {
    fn stable_identity(&self) -> String {
        self.0.to_string()
    }

    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.0.hash(hasher);
    }
}

#[test]
fn node_identity_order_is_family_then_stable_hash() {
    let shared_family: Arc<str> = Arc::from("family");
    let shared_a = NodeIdentity::new(shared_family.clone(), Arc::from("a"));
    let shared_b = NodeIdentity::new(shared_family.clone(), Arc::from("b"));
    assert!(Arc::ptr_eq(&shared_a.inner.family, &shared_b.inner.family));
    // Within a family the digest decides, not the text.
    assert_eq!(
        shared_a.cmp(&shared_b),
        shared_a.stable_hash().cmp(&shared_b.stable_hash())
    );
    assert_ne!(shared_a.stable_hash(), shared_b.stable_hash());

    // The family name still leads, and it is compared as text so two
    // separately allocated `Arc<str>` spellings agree.
    let distinct_family_a: Arc<str> = Arc::from(String::from("same"));
    let distinct_family_b: Arc<str> = Arc::from(String::from("same"));
    assert!(!Arc::ptr_eq(&distinct_family_a, &distinct_family_b));
    let equal_text_a = NodeIdentity::new(distinct_family_a, Arc::from("key"));
    let equal_text_b = NodeIdentity::new(distinct_family_b, Arc::from("key"));
    assert_eq!(equal_text_a.cmp(&equal_text_b), std::cmp::Ordering::Equal);
    assert_eq!(equal_text_a, equal_text_b);

    let alpha = NodeIdentity::new(Arc::from("alpha"), Arc::from("key"));
    let beta = NodeIdentity::new(Arc::from("beta"), Arc::from("key"));
    assert_eq!(alpha.cmp(&beta), "alpha".cmp("beta"));
    assert_eq!(alpha.stable_hash(), beta.stable_hash());
    assert_ne!(alpha, beta);
}
/// ADR-0074 falsifier: the digest is derived from the key's own content,
/// with compile-time keys, so it is identical in every process and does
/// not depend on construction order, sharing, or a per-run seed.
#[test]
fn stable_key_hash_is_content_derived_and_process_independent() {
    // Pinned digests. A change here changes the published dependency
    // order of every existing build, so it must be a deliberate decision.
    assert_eq!(
        stable_key_hash(&Key("a")).to_u128(),
        0xb7cc_bbb6_dffb_00f0_c159_cd0d_4338_ccba
    );
    assert_eq!(
        stable_key_hash(&Key("")).to_u128(),
        stable_key_hash(&Key("")).to_u128()
    );

    // Equal content, independently allocated, digests equally.
    let owned = String::from("some-longer-key-value");
    let first = TextKey(Arc::from(owned.as_str()));
    let second = TextKey(Arc::from(owned.as_str()));
    assert!(!Arc::ptr_eq(&first.0, &second.0));
    assert_eq!(stable_key_hash(&first), stable_key_hash(&second));

    // Distinct content separates, including at the byte-boundary cases
    // the streaming hasher folds a length into.
    let mut seen = BTreeSet::new();
    for length in 0..40_usize {
        let text = "k".repeat(length);
        assert!(
            seen.insert(stable_key_hash(&TextKey(Arc::from(text.as_str()))).to_u128()),
            "distinct keys of length {length} must not share a digest"
        );
    }

    // The digest is not the text's own hash: it is fixed-width and the
    // hasher is keyed with constants, never seeded.
    let mut hasher = StableHasher::new();
    assert_eq!(hasher.finish128(), StableHasher::new().finish128());
    hasher.write_u64(0);
    assert_ne!(hasher.finish128(), StableHasher::new().finish128());
}
/// ADR-0074 falsifier: with the test-only hasher override collapsing every
/// digest, distinct keys still order totally and deterministically through
/// the structural collision witness and never compare equal — including
/// when the two keys render byte-identical presentation text.
#[test]
fn forced_stable_hash_collision_stays_total_and_unequal() {
    let _forced = ForcedStableHashCollision::enter();

    // Equal digests, equal names, unequal keys.
    let text_family: Arc<str> = Arc::from("forced-collision-equal-text");
    let text_first = NodeIdentity::from_key(text_family.clone(), &EqualTextKey(1));
    let text_second = NodeIdentity::from_key(text_family, &EqualTextKey(2));
    assert_eq!(text_first.stable_hash(), text_second.stable_hash());
    assert_eq!(text_first.key(), text_second.key());
    assert_ne!(
        text_first, text_second,
        "distinct keys never compare equal, even sharing a digest and a name"
    );
    assert_eq!(text_first.cmp(&text_second), std::cmp::Ordering::Less);
    assert_eq!(text_second.cmp(&text_first), std::cmp::Ordering::Greater);

    let family: Arc<str> = Arc::from("forced-collision");
    let first = NodeIdentity::from_key(family.clone(), &Key("alpha"));
    let second = NodeIdentity::from_key(family.clone(), &Key("beta"));
    let third = NodeIdentity::from_key(family.clone(), &Key("gamma"));
    assert_eq!(first.stable_hash(), second.stable_hash());
    assert_eq!(second.stable_hash(), third.stable_hash());

    // Distinct identities never compare equal, even when they collide.
    assert_ne!(first, second);
    assert_ne!(second, third);
    assert_eq!(first, first.clone());

    // The order is total, antisymmetric, transitive, and content-derived
    // rather than construction-ordered.
    assert_eq!(first.cmp(&second), std::cmp::Ordering::Less);
    assert_eq!(second.cmp(&first), std::cmp::Ordering::Greater);
    assert_eq!(first.cmp(&third), std::cmp::Ordering::Less);
    let mut sorted = vec![third.clone(), first.clone(), second.clone()];
    sorted.sort();
    assert_eq!(
        sorted.iter().map(NodeIdentity::key).collect::<Vec<_>>(),
        vec!["alpha", "beta", "gamma"]
    );

    // A runtime whose every node collides still resolves distinct memo
    // nodes and publishes one canonical dependency order, in the same
    // order whichever order the dependencies were demanded in.
    for demand in [
        [Key("gamma"), Key("alpha"), Key("beta")],
        [Key("beta"), Key("gamma"), Key("alpha")],
    ] {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1)]);
        let leaf = runtime
            .family_with_evaluator::<Key, u64, _>("collision-leaf", 8, |_, _, key| {
                Ok(QueryOutput::success(key.0.len() as u64))
            })
            .unwrap();
        let leaf_for_root = leaf.clone();
        let root = runtime
            .family_with_evaluator::<Key, u64, _>("collision-root", 8, move |context, _, _| {
                for key in demand.clone() {
                    context.query_registered(&leaf_for_root, key)?;
                }
                Ok(QueryOutput::success(0))
            })
            .unwrap();
        let terminal = runtime
            .request_registered(&root, revision(1), Key("root"), CancellationToken::new())
            .into_result()
            .unwrap();
        assert_eq!(
            terminal
                .dependencies()
                .iter()
                .map(|dependency| dependency.node.key())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"],
            "a whole family of collisions still publishes one canonical order"
        );
    }
}
/// ADR-0074 falsifier: two unequal keys that render one name are two
/// identities. Presentation text may collide; identity may not.
#[test]
fn equal_display_text_still_yields_distinct_identities() {
    let family: Arc<str> = Arc::from("equal-text");
    let first = NodeIdentity::from_key(family.clone(), &EqualTextKey(1));
    let second = NodeIdentity::from_key(family, &EqualTextKey(2));
    assert_eq!(first.key(), second.key());
    assert_ne!(first.stable_hash(), second.stable_hash());
    assert_ne!(first, second);
    assert_eq!(
        first.cmp(&second),
        first.stable_hash().cmp(&second.stable_hash())
    );
}
/// ADR-0074 falsifier: `Debug` still names a node. `session.rs` renders
/// dependency nodes with `format!("{:?}", dependency.node)`, so deferring
/// the format must not turn a debug dump into an anonymous one.
#[test]
fn debug_formatting_still_materializes_a_memo_node_name() {
    let runtime = QueryRuntime::new(1);
    let family = runtime.family::<Key, u64>("debug-identity", 1).unwrap();
    let lease = family.node(Key("named")).unwrap();
    assert_eq!(
        runtime
            .metrics()
            .display_identities
            .memo_node_materializations,
        0,
        "minting a node names nothing"
    );

    assert_eq!(
        format!("{:?}", lease.node.identity),
        r#"NodeIdentity { family: "debug-identity", key: "named" }"#
    );
    assert_eq!(
        runtime
            .metrics()
            .display_identities
            .memo_node_materializations,
        1
    );
}
/// ADR-0074 falsifier: two fresh runs of the same graph publish identical
/// dependency orders and identical retained charges, at any worker count.
/// The digest is keyed with compile-time constants, so nothing here is
/// seeded per process, per run, or per schedule.
#[test]
fn published_order_and_charge_are_identical_across_runs_and_worker_counts() {
    fn run(workers: usize) -> (Vec<(String, String, u64)>, u64, u64) {
        let runtime = QueryRuntime::new(workers);
        publish_empty(&runtime, [revision(1)]);
        let leaf = runtime
            .family_with_evaluator::<Key, u64, _>("determinism-leaf", 8, |_, _, key| {
                Ok(QueryOutput::success(key.0.len() as u64))
            })
            .unwrap();
        let leaf_for_root = leaf.clone();
        let root = runtime
            .family_with_evaluator::<Key, u64, _>("determinism-root", 8, move |context, _, _| {
                context.query_registered_batch(
                    &leaf_for_root,
                    [
                        Key("zeta"),
                        Key("alpha"),
                        Key("mu"),
                        Key("beta"),
                        Key("omicron"),
                        Key("gamma"),
                    ],
                )?;
                Ok(QueryOutput::success(0))
            })
            .unwrap();
        let terminal = runtime
            .request_registered(&root, revision(1), Key("root"), CancellationToken::new())
            .into_result()
            .unwrap();
        let order = terminal
            .dependencies()
            .iter()
            .map(|dependency| {
                (
                    dependency.node.family().to_owned(),
                    dependency.node.key().to_owned(),
                    dependency.stamp,
                )
            })
            .collect::<Vec<_>>();
        let metrics = runtime.metrics();
        (
            order,
            metrics.retained_bytes,
            metrics.retained_dependency_pins,
        )
    }

    let single = run(1);
    assert_eq!(single, run(1), "two runs at one worker agree exactly");
    assert_eq!(
        single,
        run(4),
        "four workers publish the same order and charge"
    );
    assert_eq!(single, run(8), "and so do eight");
    assert_eq!(single.0.len(), 6);
}
/// ADR-0074 falsifier: the retained charge is denominated structurally, so
/// a longer name costs nothing and every identity costs the same 16 bytes.
#[test]
fn retained_charge_is_independent_of_presentation_length() {
    fn charge(dependencies: &[Observation], inputs: &[InputObservation]) -> u64 {
        retained_terminal_charge(
            &QueryOutcome::Success(0_u64),
            Some(0),
            &[],
            &[],
            dependencies,
            inputs,
        )
        .0
    }

    let short = NodeIdentity::from_key(Arc::from("f"), &Key("a"));
    let long = NodeIdentity::from_key(
        Arc::from("a-considerably-longer-family-name"),
        &Key("a-considerably-longer-key-identity-than-the-other-one"),
    );
    let observation = |node: NodeIdentity| Observation {
        node,
        incarnation: 1,
        stamp: 1,
    };

    assert_eq!(
        charge(&[observation(short.clone())], &[]),
        charge(&[observation(long.clone())], &[]),
        "an observed dependency costs a fixed identity charge"
    );
    assert_eq!(
        charge(&[], &[]) + std::mem::size_of::<Observation>() as u64 + IDENTITY_CHARGE_BYTES,
        charge(&[observation(long)], &[])
    );
    assert_eq!(
        charge(
            &[],
            &[InputObservation {
                input: InputIdentity::new("s", "a"),
                stamp: 1,
            }]
        ),
        charge(
            &[],
            &[InputObservation {
                input: InputIdentity::new("a-much-longer-input-family", "a-much-longer-input-key"),
                stamp: 1,
            }]
        ),
        "an observed input costs a fixed identity charge"
    );
    // Neither identity was named to compute any of that.
    assert!(
        short.inner.text.get().is_some(),
        "cold identities preformat"
    );
}
#[test]
fn display_identity_metrics_attribute_only_materialized_sources() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);

    let child = runtime
        .family_with_evaluator::<Key, u64, _>("identity-child", 8, |_, _, key| {
            Ok(QueryOutput::success(key.0.len() as u64))
        })
        .unwrap();
    let child_for_root = child.clone();
    let batch_root = runtime
        .family_with_evaluator::<Key, u64, _>("identity-batch-root", 8, move |context, _, _| {
            context.query_registered_batch(&child_for_root, [Key("aa"), Key("bbb")])?;
            Ok(QueryOutput::success(0))
        })
        .unwrap();
    runtime
        .request_registered(&batch_root, revision(1), Key("r"), CancellationToken::new())
        .into_result()
        .unwrap();

    let foreign_runtime = QueryRuntime::new(1);
    let foreign = foreign_runtime
        .family::<Key, u64>("identity-foreign", 8)
        .unwrap();
    let abort_root = runtime
        .family_with_evaluator::<Key, u64, _>("identity-abort-root", 8, move |context, _, _| {
            context.query(&foreign, Key("oops"), |_| Ok(QueryOutput::success(1)))?;
            Ok(QueryOutput::success(0))
        })
        .unwrap();
    assert_eq!(
        runtime
            .request_registered(&abort_root, revision(1), Key("x"), CancellationToken::new(),)
            .abort(),
        Some(&QueryAbort::ForeignRuntime)
    );

    // ADR-0074: four memo nodes were minted and none of them was named.
    // Ordering, equality, hashing, publication order, and the retained
    // charge are all structural now, so the only formatted identity here
    // is the one the aborted nested request had to name.
    assert_eq!(
        runtime.metrics().display_identities,
        DisplayIdentityMetrics {
            memo_node_materializations: 0,
            memo_node_bytes: 0,
            structured_wait_materializations: 0,
            structured_wait_bytes: 0,
            abort_fallback_materializations: 1,
            abort_fallback_bytes: 4,
        }
    );

    // Asking a memo node what it is called formats it exactly once and
    // counts exactly one materialization.
    let lease = child.node(Key("aa")).unwrap();
    assert_eq!(lease.node.identity.key(), "aa");
    assert_eq!(lease.node.identity.key(), "aa");
    assert_eq!(
        runtime
            .metrics()
            .display_identities
            .memo_node_materializations,
        1
    );
    assert_eq!(runtime.metrics().display_identities.memo_node_bytes, 2);
}
#[test]
fn retained_identity_hasher_mixes_every_runtime_identity_component() {
    let stamp_hash = |identity: (u64, u64)| {
        let mut hasher = RetainedIdentityHasher::default();
        std::hash::Hash::hash(&identity, &mut hasher);
        hasher.finish()
    };
    let terminal_hash = |identity: (u64, u64, Revision)| {
        let mut hasher = RetainedIdentityHasher::default();
        std::hash::Hash::hash(&identity, &mut hasher);
        hasher.finish()
    };

    assert_eq!(stamp_hash((7, 11)), stamp_hash((7, 11)));
    assert_ne!(stamp_hash((7, 11)), stamp_hash((8, 11)));
    assert_ne!(stamp_hash((7, 11)), stamp_hash((7, 12)));

    assert_eq!(
        terminal_hash((7, 11, revision(13))),
        terminal_hash((7, 11, revision(13)))
    );
    assert_ne!(
        terminal_hash((7, 11, revision(13))),
        terminal_hash((8, 11, revision(13)))
    );
    assert_ne!(
        terminal_hash((7, 11, revision(13))),
        terminal_hash((7, 12, revision(13)))
    );
    assert_ne!(
        terminal_hash((7, 11, revision(13))),
        terminal_hash((7, 11, revision(14)))
    );
}
#[test]
fn node_registry_avoids_population_traversal() {
    const NODE_COUNT: usize = 512;

    let runtime = QueryRuntime::new(1);
    let family = runtime
        .family::<Slot, u64>("bounded-node-registry-maintenance", 1)
        .unwrap();
    let leases = (0..NODE_COUNT)
        .map(|key| family.node(Slot(key as u64)).unwrap())
        .collect::<Vec<_>>();

    {
        let registry = read(&runtime.core.nodes);
        assert_eq!(registry.len(), NODE_COUNT);
        assert_eq!(
            registry.entry_visits.load(Ordering::Relaxed),
            0,
            "registration must not inspect previously registered node values"
        );
    }

    drop(leases);

    let registry = read(&runtime.core.nodes);
    assert_eq!(
        registry.len(),
        0,
        "dropping a node must remove its exact entry without waiting for another insertion"
    );
    assert_eq!(
        registry.entry_visits.load(Ordering::Relaxed),
        NODE_COUNT,
        "each node drop must inspect only its exact incarnation entry"
    );
}
#[test]
fn node_registry_exact_reads_can_overlap() {
    let runtime = QueryRuntime::new(1);
    let family = runtime
        .family::<Key, u64>("concurrent-node-registry-reads", 1)
        .unwrap();
    let lease = family.node(Key("node")).unwrap();
    let incarnation = lease.node.incarnation;

    let first_registry = read(&runtime.core.nodes);
    let first_node = first_registry
        .get(&incarnation)
        .and_then(Weak::upgrade)
        .expect("the leased node remains registered");
    let second_registry = runtime
        .core
        .nodes
        .try_read()
        .expect("a second exact registry read must not wait for the first");
    let second_node = second_registry
        .get(&incarnation)
        .and_then(Weak::upgrade)
        .expect("the same leased node remains registered");

    assert!(Arc::ptr_eq(&first_node, &second_node));
}
#[test]
fn runtime_identity_resolves_live_exact_nodes_without_the_shared_registry() {
    let runtime = QueryRuntime::new(1);
    let family = runtime
        .family::<Key, u64>("direct-node-identity", 1)
        .unwrap();
    let lease = family.node(Key("node")).unwrap();
    let identity = lease.node.identity.clone();
    let incarnation = lease.node.incarnation;
    let work = AtomicValidationWork::default();

    let direct = runtime
        .core
        .validation_node(&identity, incarnation, &work)
        .expect("the live runtime-created identity resolves its exact node");
    assert_eq!(direct.incarnation(), incarnation);
    assert_eq!(work.snapshot().registry_index_lookups, 0);
    drop(direct);

    let display_only = NodeIdentity::from_key(identity.inner.family.clone(), &Key("node"));
    let fallback = runtime
        .core
        .validation_node(&display_only, incarnation, &work)
        .expect("a display-only identity falls back to the exact registry entry");
    assert_eq!(fallback.incarnation(), incarnation);
    assert_eq!(work.snapshot().registry_index_lookups, 1);
    drop(fallback);

    let foreign = QueryRuntime::new(1);
    assert!(
        foreign
            .core
            .validation_node(&identity, incarnation, &work)
            .is_none(),
        "a direct handle from another runtime must fail closed"
    );
    assert_eq!(work.snapshot().registry_index_lookups, 2);

    drop(lease);
    assert!(
        identity
            .registered_node(runtime.core.identity, incarnation)
            .is_none()
    );
    assert_eq!(read(&runtime.core.nodes).len(), 0);
}
#[test]
fn memo_hits_in_distinct_shards_proceed_while_one_shard_is_held() {
    let runtime = QueryRuntime::new(2);
    let family = runtime
        .family::<Slot, u64>("sharded-memo-independence", 4)
        .unwrap();
    // Precreate both nodes so the cross-thread access below is a pure hit.
    let held_key = Slot(0);
    let held_lease = family.node(held_key.clone()).unwrap();
    let other_key = (1_u64..)
        .map(Slot)
        .find(|candidate| {
            family.inner.nodes.shard_index(candidate) != family.inner.nodes.shard_index(&held_key)
        })
        .expect("an unbounded key supply reaches a second shard");
    let other_lease = family.node(other_key.clone()).unwrap();

    // Hold one shard exclusively. A hit on a key in a different shard must
    // complete anyway; under the retired whole-index mutex this join would
    // deadlock, so completion is the deterministic concurrency witness.
    let shard_guard = family.inner.nodes.shard(&held_key);
    let hit = thread::spawn({
        let family = family.clone();
        let other_key = other_key.clone();
        move || family.node(other_key).unwrap()
    });
    let hit_lease = hit.join().unwrap();
    assert!(
        Arc::ptr_eq(&hit_lease.node, &other_lease.node),
        "the concurrent hit must observe the already-published node"
    );
    drop(shard_guard);

    // Once released, the held shard serves its key again unchanged.
    let held_again = family.node(held_key).unwrap();
    assert!(Arc::ptr_eq(&held_again.node, &held_lease.node));

    drop((held_lease, held_again, other_lease, hit_lease));
    assert_eq!(
        family.retention().memo_nodes,
        0,
        "lifetime-coupled removal must still reclaim both shards' nodes"
    );
}
#[test]
fn concurrent_memo_misses_publish_one_canonical_node() {
    const RACERS: usize = 8;
    let runtime = QueryRuntime::new(2);
    let family = runtime
        .family::<Key, u64>("sharded-memo-single-mint", 4)
        .unwrap();
    let barrier = Arc::new(Barrier::new(RACERS));
    let racers = (0..RACERS)
        .map(|_| {
            let family = family.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                family.node(Key("contested")).unwrap()
            })
        })
        .collect::<Vec<_>>();
    let leases = racers
        .into_iter()
        .map(|racer| racer.join().unwrap())
        .collect::<Vec<_>>();

    let canonical = &leases[0].node;
    for lease in &leases {
        assert!(
            Arc::ptr_eq(&lease.node, canonical),
            "every concurrent miss must land on one published node"
        );
    }
    assert_eq!(family.retention().memo_nodes, 1);
    assert_eq!(
        read(&runtime.core.nodes).len(),
        1,
        "exactly one incarnation is minted for the contested key"
    );

    drop(leases);
    assert_eq!(family.retention().memo_nodes, 0);
    assert_eq!(read(&runtime.core.nodes).len(), 0);
}
#[test]
fn node_registry_distinguishes_incarnations_and_removes_only_the_matching_node() {
    let runtime = QueryRuntime::new(1);
    let family = runtime
        .family::<Key, u64>("exact-node-registry-removal", 1)
        .unwrap();
    let first = family.node(Key("first")).unwrap();
    let second = family.node(Key("second")).unwrap();
    let first_incarnation = first.node.incarnation;
    let second_incarnation = second.node.incarnation;
    assert_ne!(first_incarnation, second_incarnation);

    {
        let registry = read(&runtime.core.nodes);
        let registered_first = registry
            .get(&first_incarnation)
            .and_then(Weak::upgrade)
            .expect("the first incarnation is registered");
        let registered_second = registry
            .get(&second_incarnation)
            .and_then(Weak::upgrade)
            .expect("the second incarnation is registered");
        let expected_first: Arc<dyn ErasedNode> = first.node.clone();
        let expected_second: Arc<dyn ErasedNode> = second.node.clone();
        assert!(Arc::ptr_eq(&registered_first, &expected_first));
        assert!(Arc::ptr_eq(&registered_second, &expected_second));
    }

    write(&runtime.core.nodes).remove(first_incarnation, &second.node.registry_self);
    assert!(
        runtime.core.registered_node(first_incarnation).is_some(),
        "a different allocation must not remove the occupied incarnation"
    );
    assert!(runtime.core.registered_node(second_incarnation).is_some());

    drop(first);
    assert!(runtime.core.registered_node(first_incarnation).is_none());
    assert!(runtime.core.registered_node(second_incarnation).is_some());
    drop(second);
    assert_eq!(read(&runtime.core.nodes).len(), 0);
}
#[test]
fn exact_retained_key_probe_uses_hash_lookup_not_predicate_enumeration() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family::<CountingKey, u64>("exact-retained-key", 128)
        .unwrap();
    for key in 0..64 {
        runtime
            .query(
                &family,
                revision(1),
                CountingKey(key),
                CancellationToken::new(),
                move |_| Ok(QueryOutput::success(key)),
            )
            .unwrap();
    }

    CONTAINS_HASH_CALLS.store(0, Ordering::Relaxed);
    assert!(family.contains_retained_key(&CountingKey(63)));
    assert_eq!(
        CONTAINS_HASH_CALLS.load(Ordering::Relaxed),
        2,
        "an exact retained-key probe hashes the requested key a bounded \
             number of times (shard selection plus one in-shard lookup), \
             independent of the retained population"
    );
    let present_probe_hashes = CONTAINS_HASH_CALLS.load(Ordering::Relaxed);
    assert!(!family.contains_retained_key(&CountingKey(64)));
    let missing_probe_hashes = CONTAINS_HASH_CALLS
        .load(Ordering::Relaxed)
        .saturating_sub(present_probe_hashes);
    assert!(
        (1..=2).contains(&missing_probe_hashes),
        "a missing exact retained-key probe hashes only for shard selection \
             and, when the selected shard exists, one in-shard lookup"
    );

    let predicate_visits = AtomicUsize::new(0);
    assert!(!family.any_retained_key(|_| {
        predicate_visits.fetch_add(1, Ordering::Relaxed);
        false
    }));
    assert_eq!(
        predicate_visits.load(Ordering::Relaxed),
        64,
        "predicate enumeration visits the retained-key population"
    );
}
#[test]
fn ready_probe_observes_exact_terminal_without_demanding_a_miss() {
    let runtime = QueryRuntime::new(1);
    let first = revision(1);
    publish_empty(&runtime, [first]);
    let runs = Arc::new(AtomicUsize::new(0));
    let runs_for_family = runs.clone();
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("ready-probe", 8, move |_, _, _| {
            runs_for_family.fetch_add(1, Ordering::Relaxed);
            Ok(QueryOutput::success(7))
        })
        .unwrap();

    let computed = runtime
        .request_registered(&family, first, Key("ready"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    drop(computed);

    let root = runtime
        .family_with_evaluator::<Key, u64, _>("ready-probe-root", 8, {
            let family = family.clone();
            move |context, _, _| {
                let ReadyQueryProbe::Ready(terminal) =
                    context.probe_registered_ready(&family, Key("ready"))?
                else {
                    unreachable!("the setup terminal is current and retained")
                };
                assert_eq!(terminal.outcome(), &QueryOutcome::Success(7));
                Ok(QueryOutput::success(8))
            }
        })
        .unwrap();
    let root_attempt =
        runtime.request_registered(&root, first, Key("root"), CancellationToken::new());
    assert!(root_attempt.abort().is_none());
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    assert_eq!(root_attempt.terminal().unwrap().dependencies().len(), 1);
    assert_eq!(
        root_attempt
            .nested_attempts()
            .iter()
            .filter(|attempt| attempt.node().family() == "ready-probe")
            .count(),
        1
    );

    drop(root_attempt);
    drop(root);
    drop(family);
}
#[test]
fn ready_probe_miss_and_stale_do_not_create_or_execute_work() {
    let runtime = QueryRuntime::new(1);
    let first = revision(1);
    let second = revision(2);
    publish_empty(&runtime, [first, second]);
    let runs = Arc::new(AtomicUsize::new(0));
    let runs_for_family = runs.clone();
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("ready-probe-negative", 8, move |_, _, _| {
            runs_for_family.fetch_add(1, Ordering::Relaxed);
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("ready-probe-negative-root", 8, {
            let family = family.clone();
            move |context, _, _| {
                assert!(matches!(
                    context.probe_registered_ready(&family, Key("absent"))?,
                    ReadyQueryProbe::Miss
                ));
                Ok(QueryOutput::success(0))
            }
        })
        .unwrap();
    runtime
        .request_registered(&root, first, Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(runs.load(Ordering::Relaxed), 0);
    assert!(!family.contains_retained_key(&Key("absent")));

    runtime
        .request_registered(&family, first, Key("stale"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    let stale_root = runtime
        .family_with_evaluator::<Key, u64, _>("ready-probe-stale-root", 8, {
            let family = family.clone();
            move |context, _, _| {
                assert!(matches!(
                    context.probe_registered_ready(&family, Key("stale"))?,
                    ReadyQueryProbe::Miss
                ));
                Ok(QueryOutput::success(0))
            }
        })
        .unwrap();
    runtime
        .request_registered(&stale_root, second, Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(runs.load(Ordering::Relaxed), 1);
}
#[test]
fn ready_probe_reuses_task_cache_after_indexed_terminal_is_retired() {
    let runtime = QueryRuntime::new(2);
    let current = revision(1);
    publish_empty(&runtime, [current]);
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("ready-probe-task-cache", 1, |_, _, key| {
            Ok(QueryOutput::success(if key.0 == "first" { 1 } else { 2 }))
        })
        .unwrap();
    runtime
        .request_registered(&family, current, Key("first"), CancellationToken::new())
        .into_result()
        .unwrap();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("ready-probe-task-cache-root", 1, {
            let family = family.clone();
            let runtime = runtime.clone();
            move |context, _, _| {
                assert!(matches!(
                    context.probe_registered_ready(&family, Key("first"))?,
                    ReadyQueryProbe::Ready(_)
                ));
                let node_lease = family
                    .existing_node(&Key("first"))
                    .expect("the first memo incarnation remains indexed");
                let attempt_id = lock(&node_lease.node.state)
                    .attempts
                    .iter()
                    .find_map(|attempt| {
                        matches!(attempt.state, AttemptState::Terminal { .. }).then_some(attempt.id)
                    })
                    .expect("the first incarnation has a terminal attempt");
                family.detach_terminal_attempt(&node_lease.node, attempt_id);
                assert!(
                    family.contains_retained_key(&Key("first")),
                    "the exact-node lease must keep the retired incarnation indexed"
                );
                drop(node_lease);
                assert!(
                    !family.contains_retained_key(&Key("first")),
                    "the indexed first incarnation should be retired"
                );
                runtime
                    .request_registered(&family, current, Key("first"), CancellationToken::new())
                    .into_result()?;
                assert!(family.contains_retained_key(&Key("first")));
                assert!(matches!(
                    context.probe_registered_ready(&family, Key("first"))?,
                    ReadyQueryProbe::Ready(_)
                ));
                Ok(QueryOutput::success(0))
            }
        })
        .unwrap();
    runtime
        .request_registered(&root, current, Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
}
#[test]
fn ready_probe_reports_in_progress_without_joining_or_waiting() {
    let runtime = QueryRuntime::new(2);
    let current = revision(1);
    publish_empty(&runtime, [current]);
    let claimed = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let runs = Arc::new(AtomicUsize::new(0));
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("ready-probe-progress", 8, {
            let claimed = claimed.clone();
            let release = release.clone();
            let runs = runs.clone();
            move |_, _, _| {
                runs.fetch_add(1, Ordering::SeqCst);
                claimed.wait();
                release.wait();
                Ok(QueryOutput::success(1))
            }
        })
        .unwrap();
    let owner = thread::spawn({
        let family = family.clone();
        let runtime = runtime.clone();
        move || {
            runtime
                .request_registered(&family, current, Key("busy"), CancellationToken::new())
                .into_result()
                .unwrap()
        }
    });
    claimed.wait();

    let root = runtime
        .family_with_evaluator::<Key, u64, _>("ready-probe-progress-root", 8, {
            let family = family.clone();
            move |context, _, _| {
                assert!(matches!(
                    context.probe_registered_ready(&family, Key("busy"))?,
                    ReadyQueryProbe::NotReady
                ));
                Ok(QueryOutput::success(2))
            }
        })
        .unwrap();
    let root_attempt =
        runtime.request_registered(&root, current, Key("root"), CancellationToken::new());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert!(root_attempt.terminal().unwrap().dependencies().is_empty());
    assert!(root_attempt.nested_attempts().is_empty());
    root_attempt.into_result().unwrap();

    release.wait();
    owner.join().unwrap();
}
// ADR-0063 Phase 7 hashed-memo-index focused coverage.

#[test]
fn forced_hash_collision_keys_resolve_to_distinct_nodes_via_eq() {
    // A key whose `Hash` is a constant: every value collides in one bucket,
    // so the memo map must separate distinct keys through exact `Eq` alone.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct OneBucketKey(u64);

    impl std::hash::Hash for OneBucketKey {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            state.write_u8(0);
        }
    }

    impl QueryKey for OneBucketKey {
        fn stable_identity(&self) -> String {
            // Distinct display identities to prove lookup never consults the
            // display string: only typed `Eq` chooses the node.
            format!("one-bucket:{}", self.0)
        }

        fn stable_hash(&self, hasher: &mut StableHasher) {
            self.0.hash(hasher);
        }
    }

    assert_ne!(OneBucketKey(1), OneBucketKey(2));
    assert_eq!(hash_of(&OneBucketKey(1)), hash_of(&OneBucketKey(2)));

    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family::<OneBucketKey, u64>("one-bucket", 16)
        .unwrap();
    let first = runtime
        .query(
            &family,
            revision(1),
            OneBucketKey(1),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(10)),
        )
        .unwrap();
    let second = runtime
        .query(
            &family,
            revision(1),
            OneBucketKey(2),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(20)),
        )
        .unwrap();
    // Two colliding keys produced two distinct memo nodes and two bodies.
    assert!(!Arc::ptr_eq(&first, &second));
    assert_ne!(first.outcome(), second.outcome());
    assert_eq!(family.retention().memo_nodes, 2);
    assert_eq!(runtime.metrics().claims, 2);
    // Re-querying an existing colliding key reuses its node (no new claim),
    // proving the `Eq` fallback finds the right bucket entry.
    let reuse = runtime
        .query(
            &family,
            revision(1),
            OneBucketKey(1),
            CancellationToken::new(),
            |_| panic!("colliding key 1 must reuse its retained terminal"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&first, &reuse));
    assert_eq!(runtime.metrics().claims, 2);

    let family_for_root = family.clone();
    let root = runtime.family::<Key, u64>("one-bucket-root", 4).unwrap();
    runtime
        .query(
            &root,
            revision(1),
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let first = context.query(&family_for_root, OneBucketKey(1), |_| {
                    panic!("colliding key 1 must reuse its retained terminal")
                })?;
                let second = context.query(&family_for_root, OneBucketKey(2), |_| {
                    panic!("colliding key 2 must reuse its retained terminal")
                })?;
                let first_again = context.query(&family_for_root, OneBucketKey(1), |_| {
                    panic!("the task cache must resolve colliding key 1 through Eq")
                })?;
                assert_eq!(first.outcome(), &QueryOutcome::Success(10));
                assert_eq!(second.outcome(), &QueryOutcome::Success(20));
                assert!(Arc::ptr_eq(&first, &first_again));
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();
    assert_eq!(runtime.metrics().claims, 3);
}
#[test]
fn evict_then_recreate_in_hashed_index_yields_new_incarnation() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1), revision(2)]);
    // Zero-retention leaf: its node is evicted from the hashed index as soon
    // as no lease holds it, forcing a fresh incarnation on the next request.
    let leaf = runtime.family::<Key, u64>("recreate-leaf", 0).unwrap();
    let parent = runtime.family::<Key, u64>("recreate-parent", 4).unwrap();
    let observe = |revision| {
        let leaf = leaf.clone();
        runtime
            .query(
                &parent,
                revision,
                Key("root"),
                CancellationToken::new(),
                move |context| {
                    context.query(&leaf, Key("leaf"), |_| Ok(QueryOutput::success(1)))?;
                    Ok(QueryOutput::success(0))
                },
            )
            .unwrap()
    };
    let first = observe(revision(1));
    // The zero-retention leaf node left the hashed index entirely.
    assert_eq!(leaf.retention().memo_nodes, 0);
    let second = observe(revision(2));
    let first_incarnation = first.dependencies()[0].incarnation;
    let second_incarnation = second.dependencies()[0].incarnation;
    assert_ne!(
        first_incarnation, second_incarnation,
        "eviction followed by recreation must mint a distinct incarnation"
    );
}
#[test]
fn concurrent_same_key_requests_join_one_hashed_computation() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime.family::<Key, u64>("concurrent-join", 16).unwrap();
    let waiters = 6;
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner = thread::spawn(move || {
        owner_runtime
            .query(
                &owner_family,
                revision(1),
                Key("shared"),
                CancellationToken::new(),
                |_| {
                    started_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Ok(QueryOutput::success(7))
                },
            )
            .unwrap()
    });
    started_rx.recv().unwrap();
    let joins_before = runtime.metrics().joins;
    let mut joiners = Vec::new();
    for _ in 0..waiters {
        let runtime = runtime.clone();
        let family = family.clone();
        joiners.push(thread::spawn(move || {
            runtime
                .query(
                    &family,
                    revision(1),
                    Key("shared"),
                    CancellationToken::new(),
                    |_| panic!("a joiner on the shared key must not compute"),
                )
                .unwrap()
        }));
    }
    while runtime.metrics().joins < joins_before + waiters as u64 {
        thread::yield_now();
    }
    finish_tx.send(()).unwrap();
    let owner_terminal = owner.join().unwrap();
    for joiner in joiners {
        let terminal = joiner.join().unwrap();
        assert!(Arc::ptr_eq(&terminal, &owner_terminal));
    }
    // Exactly one node was claimed for the shared key; all waiters joined it.
    assert_eq!(runtime.metrics().claims, 1);
    assert_eq!(runtime.metrics().joins, waiters as u64);
    assert_eq!(family.retention().memo_nodes, 1);
}
#[test]
fn incarnation_hasher_preserves_the_exact_u64_hash() {
    for incarnation in [0, 1, 2, u32::MAX as u64, u64::MAX - 1, u64::MAX] {
        let mut hasher = IncarnationHasher::default();
        std::hash::Hash::hash(&incarnation, &mut hasher);
        assert_eq!(
            hasher.finish(),
            incarnation,
            "the private registry hasher must use the runtime-owned incarnation directly"
        );
    }
}
#[test]
fn family_names_and_key_text_form_collision_free_node_identities() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let left = runtime.family::<Key, u64>("left", 2).unwrap();
    let right = runtime.family::<Key, u64>("right", 2).unwrap();
    assert!(matches!(
        runtime.family::<Key, u64>("left", 2),
        Err(FamilyError::DuplicateName(_))
    ));
    let left = runtime
        .query(
            &left,
            revision(1),
            Key("same"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(1)),
        )
        .unwrap();
    let right = runtime
        .query(
            &right,
            revision(1),
            Key("same"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(1)),
        )
        .unwrap();
    assert_ne!(left.node(), right.node());
    assert_eq!(left.node().key(), right.node().key());
    assert_eq!(
        std::mem::size_of::<NodeIdentity>(),
        std::mem::size_of::<usize>(),
        "cloned display identities must remain one shared pointer"
    );
    assert_eq!(
        std::mem::size_of::<Observation>(),
        3 * std::mem::size_of::<usize>(),
        "dependency observations retain one shared identity plus incarnation and stamp"
    );
}
