//! Query runtime tests.

use ahash::AHashSet;
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::Condvar;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{OnceLock, Weak};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use super::*;

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
fn active_validations_stay_inline_and_detect_cycles() {
    let mut active = ActiveValidations::default();
    for incarnation in 1..=INLINE_ACTIVE_VALIDATIONS as u64 {
        assert!(active.insert(incarnation));
    }
    assert!(matches!(active, ActiveValidations::Inline { .. }));
    assert!(!active.insert(4));
    assert!(active.remove(&4));
    assert!(active.insert(4));
    assert!(!active.remove(&99));
}

#[test]
fn active_validations_promote_for_deep_cones() {
    let mut active = ActiveValidations::default();
    for incarnation in 1..=(INLINE_ACTIVE_VALIDATIONS as u64 + 2) {
        assert!(active.insert(incarnation));
    }
    assert!(matches!(active, ActiveValidations::Hashed(_)));
    assert!(!active.insert(1));
    assert!(!active.insert(INLINE_ACTIVE_VALIDATIONS as u64 + 2));
    assert!(active.remove(&1));
    assert!(active.insert(1));
}

#[test]
fn validation_proof_stack_keeps_common_depth_inline_without_growing_tasks() {
    assert!(std::mem::size_of::<ValidationProofStack>() <= std::mem::size_of::<Vec<u8>>());

    let mut proofs = ValidationProofStack::new();
    proofs.resize(8, VALIDATION_PROOF_REGISTERED);
    assert!(!proofs.spilled());

    proofs.push(VALIDATION_PROOF_REGISTERED);
    assert!(proofs.spilled());
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key(&'static str);

impl QueryKey for Key {
    fn stable_identity(&self) -> String {
        self.0.to_owned()
    }

    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.0.hash(hasher);
    }
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

fn assert_validation_work_consistent(work: ValidationWork) {
    assert_eq!(
        work.traversals,
        work.successful_traversals + work.dirty_traversals + work.aborted_traversals,
        "every validation traversal has exactly one outcome"
    );
    assert_eq!(
        work.node_visits,
        work.active_cycle_prunes + work.memo_hits + work.memo_misses,
        "every erased-node visit has exactly one outcome"
    );
    assert_eq!(
        work.memo_misses,
        work.certificate_misses + work.proof_reacquisition_misses,
        "every memo miss has exactly one cause"
    );
    assert_eq!(
        work.registry_probes,
        work.dependency_observations + work.successful_traversals,
        "validation probes once per dependency and successful root certificate"
    );
    assert!(work.registry_index_lookups <= work.registry_probes);
    assert!(work.registry_misses <= work.registry_index_lookups);
    assert!(work.endorsement_hits <= work.endorsement_probes);
    assert!(work.duplicate_terminal_lease_observations <= work.terminal_lease_observations);
    assert!(
        work.demand_reuses + work.demand_computes + work.demand_joins + work.demand_aborts
            <= work.demands
    );
    assert!(work.certificates_published <= work.successful_traversals);
}

#[test]
fn endorsement_probe_totals_are_derived_from_disjoint_outcomes() {
    let work = AtomicValidationWork::default();
    work.endorsement_hits.fetch_add(3, Ordering::Relaxed);
    work.endorsement_misses.fetch_add(2, Ordering::Relaxed);

    let snapshot = work.snapshot();
    assert_eq!(snapshot.endorsement_hits, 3);
    assert_eq!(snapshot.endorsement_probes, 5);

    let transferred = work.take();
    assert_eq!(transferred, snapshot);
    assert_eq!(work.snapshot(), ValidationWork::default());

    work.add(transferred);
    assert_eq!(work.snapshot(), snapshot);
}

#[test]
fn validation_demand_totals_are_derived_from_results_and_early_exits() {
    let work = AtomicValidationWork::default();
    work.demand_reuses.fetch_add(3, Ordering::Relaxed);
    work.demand_aborts.fetch_add(1, Ordering::Relaxed);
    drop(ValidationDemandWork::new(&work));
    drop(ValidationDemandWork::new(&work));

    let snapshot = work.snapshot();
    assert_eq!(snapshot.demands, 6);
    assert_eq!(snapshot.demand_reuses, 3);
    assert_eq!(snapshot.demand_aborts, 1);

    let transferred = work.take();
    assert_eq!(transferred, snapshot);
    assert_eq!(work.snapshot(), ValidationWork::default());

    work.add(transferred);
    assert_eq!(work.snapshot(), snapshot);

    let completed = ValidationDemandWork::new(&work);
    work.demand_computes.fetch_add(1, Ordering::Relaxed);
    completed.finish();
    let completed_snapshot = work.snapshot();
    assert_eq!(completed_snapshot.demands, 7);
    assert_eq!(completed_snapshot.demand_computes, 1);
}

#[test]
fn dependency_validation_work_flushes_complete_and_early_prefixes() {
    fn inspect(work: &AtomicValidationWork, stop_after: Option<u64>) -> bool {
        let mut dependency_work = DependencyValidationWork {
            work,
            observations: 0,
        };
        for observation in 1..=3 {
            dependency_work.observe();
            if stop_after == Some(observation) {
                return false;
            }
        }
        true
    }

    let work = AtomicValidationWork::default();
    assert!(inspect(&work, None));
    assert!(!inspect(&work, Some(2)));
    let work = work.snapshot();
    assert_eq!(work.dependency_observations, 5);
    assert_eq!(work.registry_probes, 5);
}

#[test]
fn atomic_validation_work_derives_totals_and_preserves_them_across_transfer() {
    let work = AtomicValidationWork::default();
    work.successful_traversals.fetch_add(2, Ordering::Relaxed);
    work.dirty_traversals.fetch_add(3, Ordering::Relaxed);
    work.aborted_traversals.fetch_add(4, Ordering::Relaxed);
    work.dependency_observations.fetch_add(5, Ordering::Relaxed);
    work.active_cycle_prunes.fetch_add(6, Ordering::Relaxed);
    work.memo_hits.fetch_add(7, Ordering::Relaxed);
    work.certificate_misses.fetch_add(8, Ordering::Relaxed);
    work.proof_reacquisition_misses
        .fetch_add(9, Ordering::Relaxed);
    work.unique_terminal_lease_observations
        .fetch_add(10, Ordering::Relaxed);
    work.duplicate_terminal_lease_observations
        .fetch_add(2, Ordering::Relaxed);

    let expected = ValidationWork {
        traversals: 9,
        successful_traversals: 2,
        dirty_traversals: 3,
        aborted_traversals: 4,
        dependency_observations: 5,
        registry_probes: 7,
        node_visits: 30,
        active_cycle_prunes: 6,
        memo_hits: 7,
        memo_misses: 17,
        certificate_misses: 8,
        proof_reacquisition_misses: 9,
        terminal_lease_observations: 12,
        duplicate_terminal_lease_observations: 2,
        ..ValidationWork::default()
    };
    assert_eq!(work.snapshot(), expected);
    assert_eq!(work.take(), expected);
    assert_eq!(work.snapshot(), ValidationWork::default());

    work.add(expected);
    assert_eq!(work.snapshot(), expected);
}

#[test]
fn validation_traversal_work_records_every_outcome_once() {
    let work = AtomicValidationWork::default();
    {
        let traversal = ValidationTraversalWork {
            work: &work,
            outcome: None,
        };
        traversal.finish(true);
    }
    {
        let traversal = ValidationTraversalWork {
            work: &work,
            outcome: None,
        };
        traversal.finish(false);
    }
    {
        let _traversal = ValidationTraversalWork {
            work: &work,
            outcome: None,
        };
    }

    let work = work.snapshot();
    assert_eq!(work.traversals, 3);
    assert_eq!(work.successful_traversals, 1);
    assert_eq!(work.dirty_traversals, 1);
    assert_eq!(work.aborted_traversals, 1);
}

#[test]
fn exact_terminal_task_cache_avoids_a_duplicate_lease_observation() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Key, u64>("lease-metrics-leaf", 8).unwrap();
    let root = runtime.family::<Key, u64>("lease-metrics-root", 8).unwrap();
    let leaf_for_root = leaf.clone();
    let before = runtime.metrics().validation;

    let attempt = runtime.request(
        &root,
        revision(1),
        Key("root"),
        CancellationToken::new(),
        move |context| {
            context.query(&leaf_for_root, Key("shared"), |_| {
                Ok(QueryOutput::success(1))
            })?;
            context.query(&leaf_for_root, Key("shared"), |_| {
                panic!("the second exact request must reuse the retained terminal")
            })?;
            Ok(QueryOutput::success(0))
        },
    );
    assert_eq!(attempt.execution(), RequestExecution::Computed);
    let work = runtime.metrics().validation.saturating_sub(before);

    assert_eq!(work.terminal_lease_observations, 2);
    assert_eq!(work.duplicate_terminal_lease_observations, 0);
    assert_eq!(attempt.nested_attempts().len(), 2);
    assert_eq!(
        attempt.nested_attempts()[1].execution(),
        RequestExecution::Reused,
        "the task-local fast path preserves request-ledger classification"
    );
    assert_validation_work_consistent(work);
}

#[test]
fn exact_terminal_task_cache_cannot_bypass_cancellation() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime.family::<Key, u64>("cache-cancel-leaf", 8).unwrap();
    let root = runtime.family::<Key, u64>("cache-cancel-root", 8).unwrap();
    let leaf_for_root = leaf.clone();
    let cancellation = CancellationToken::new();
    let cancellation_for_root = cancellation.clone();

    let attempt = runtime.request(
        &root,
        revision(1),
        Key("root"),
        cancellation,
        move |context| {
            context.query(&leaf_for_root, Key("shared"), |_| {
                Ok(QueryOutput::success(1))
            })?;
            cancellation_for_root.cancel();
            assert!(matches!(
                context.query(&leaf_for_root, Key("shared"), |_| {
                    panic!("a canceled cache hit must not invoke its evaluator")
                }),
                Err(QueryAbort::Canceled)
            ));
            Ok(QueryOutput::success(0))
        },
    );

    assert_eq!(attempt.abort(), Some(&QueryAbort::Canceled));
    assert_eq!(attempt.nested_attempts().len(), 2);
    assert_eq!(
        attempt.nested_attempts()[1].abort(),
        Some(&QueryAbort::Canceled)
    );
}

// A numeric key for tests that need an unbounded supply of distinct keys
// (e.g. flooding a family past a tiny retention bound).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Slot(u64);

impl QueryKey for Slot {
    fn stable_identity(&self) -> String {
        self.0.to_string()
    }

    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.0.hash(hasher);
    }
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

fn revision(id: u64) -> Revision {
    Revision::new(id, id)
}

// Deterministic single-hasher probe used only to demonstrate that two keys
// land in the same bucket. The live memo map uses independently keyed
// AHash; this fixed-seed `DefaultHasher` just makes the collision
// assertion reproducible.
fn hash_of<K: std::hash::Hash>(key: &K) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
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

#[test]
fn noncomputing_join_reuses_current_owner_without_claiming_or_revalidating() {
    let runtime = QueryRuntime::new(2);
    let current = revision(1);
    publish_empty(&runtime, [current]);
    let claimed = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let target_runs = Arc::new(AtomicUsize::new(0));
    let dependency_runs = Arc::new(AtomicUsize::new(0));
    let dependency = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-dependency", 8, {
            let dependency_runs = dependency_runs.clone();
            move |_, _, _| {
                dependency_runs.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(4))
            }
        })
        .unwrap();
    let target = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-target", 8, {
            let target_runs = target_runs.clone();
            let dependency = dependency.clone();
            let claimed = claimed.clone();
            let release = release.clone();
            move |context, _, _| {
                target_runs.fetch_add(1, Ordering::SeqCst);
                context.query_registered(&dependency, Key("dep"))?;
                claimed.wait();
                release.wait();
                Ok(QueryOutput::success(9))
            }
        })
        .unwrap();
    let owner = thread::spawn({
        let target = target.clone();
        let runtime = runtime.clone();
        move || {
            runtime
                .request_registered(&target, current, Key("busy"), CancellationToken::new())
                .into_result()
                .unwrap()
        }
    });
    claimed.wait();

    let parked = Arc::new(Barrier::new(2));
    runtime.set_interpose({
        let parked = parked.clone();
        Arc::new(move |site| {
            if site == InterposeSite::NodeJoinPark {
                parked.wait();
            }
        })
    });
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-root", 8, {
            let target = target.clone();
            move |context, _, _| {
                let ReadyQueryProbe::Ready(terminal) =
                    context.join_registered_noncomputing(&target, Key("busy"))?
                else {
                    unreachable!("the owner publishes a current terminal")
                };
                assert_eq!(terminal.outcome(), &QueryOutcome::Success(9));
                Ok(QueryOutput::success(10))
            }
        })
        .unwrap();
    // The root request runs concurrently so the owner can be released
    // after the non-computing join parks.
    let root_thread = thread::spawn({
        let root = root.clone();
        let runtime = runtime.clone();
        move || runtime.request_registered(&root, current, Key("root"), CancellationToken::new())
    });
    parked.wait();
    release.wait();
    let root_attempt = root_thread.join().unwrap();
    assert_eq!(root_attempt.execution(), RequestExecution::Computed);
    let owner_terminal = owner.join().unwrap();
    runtime.clear_interpose();
    assert_eq!(target_runs.load(Ordering::SeqCst), 1);
    assert_eq!(dependency_runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        root_attempt.dependencies(),
        &[Observation {
            node: owner_terminal.node().clone(),
            incarnation: owner_terminal.node_incarnation(),
            stamp: owner_terminal.stamp(),
        }]
    );
    let joined = root_attempt
        .nested_attempts()
        .iter()
        .find(|attempt| attempt.node().family() == "noncomputing-target")
        .expect("joined target must be recorded in the nested ledger");
    assert_eq!(joined.node().key(), "busy");
    assert_eq!(joined.execution(), RequestExecution::Joined);
}

#[test]
fn noncomputing_join_returns_not_ready_on_wait_graph_contention() {
    let runtime = QueryRuntime::new(2);
    let current = revision(1);
    publish_empty(&runtime, [current]);
    let left_slot = Arc::new(OnceLock::new());
    let right_slot = Arc::new(OnceLock::new());
    let left_started = Arc::new(Barrier::new(2));
    let continue_left = Arc::new(Barrier::new(2));
    let parked = Arc::new(Barrier::new(2));
    runtime.set_interpose({
        let parked = parked.clone();
        Arc::new(move |site| {
            if site == InterposeSite::NodeJoinPark {
                parked.wait();
            }
        })
    });

    let left_slot_for_right = left_slot.clone();
    let right_slot_for_left = right_slot.clone();
    let left_started_for_body = left_started.clone();
    let continue_left_for_body = continue_left.clone();
    let left = runtime
        .family_with_evaluator::<Key, u64, _>(
            "noncomputing-contended-left",
            8,
            move |context, _, _| {
                left_started_for_body.wait();
                continue_left_for_body.wait();
                let right = right_slot_for_left.get().expect("right family initialized");
                assert!(matches!(
                    context.join_registered_noncomputing(right, Key("right"))?,
                    ReadyQueryProbe::NotReady
                ));
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let right = runtime
        .family_with_evaluator::<Key, u64, _>(
            "noncomputing-contended-right",
            8,
            move |context, _, _| {
                let left = left_slot_for_right.get().expect("left family initialized");
                let ReadyQueryProbe::Ready(terminal) =
                    context.join_registered_noncomputing(left, Key("left"))?
                else {
                    unreachable!("left publishes after the contended join declines");
                };
                assert_eq!(terminal.outcome(), &QueryOutcome::Success(1));
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();
    left_slot.set(left.clone()).unwrap();
    right_slot.set(right.clone()).unwrap();

    let left_task = thread::spawn({
        let runtime = runtime.clone();
        let left = left.clone();
        move || {
            runtime
                .request_registered(&left, current, Key("left"), CancellationToken::new())
                .into_result()
                .unwrap()
        }
    });
    left_started.wait();
    let right_task = thread::spawn({
        let runtime = runtime.clone();
        let right = right.clone();
        move || {
            runtime
                .request_registered(&right, current, Key("right"), CancellationToken::new())
                .into_result()
                .unwrap()
        }
    });
    // The right owner is now parked waiting for the left owner. The left
    // owner can therefore attempt the reverse edge and deterministically
    // receive Contended/NotReady instead of claiming a second attempt.
    parked.wait();
    continue_left.wait();
    assert_eq!(
        left_task.join().unwrap().outcome(),
        &QueryOutcome::Success(1)
    );
    assert_eq!(
        right_task.join().unwrap().outcome(),
        &QueryOutcome::Success(2)
    );
    runtime.clear_interpose();
    assert_eq!(runtime.metrics().claims, 2);
    assert_eq!(runtime.metrics().joins, 2);
    assert_eq!(runtime.metrics().declined_joins, 1);
}

#[test]
fn noncomputing_join_observes_pending_handoff_without_recomputation() {
    let runtime = QueryRuntime::new(2);
    let current = revision(1);
    publish_empty(&runtime, [current]);
    let child_runs = Arc::new(AtomicUsize::new(0));
    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let child = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-pending-child", 8, {
            let child_runs = child_runs.clone();
            let commits = commits.clone();
            let aborts = aborts.clone();
            move |context, _, _| {
                child_runs.fetch_add(1, Ordering::SeqCst);
                context.register_attempt_handoff(CountingHandoff {
                    commits: commits.clone(),
                    aborts: aborts.clone(),
                });
                Ok(QueryOutput::success(1))
            }
        })
        .unwrap();
    let outer = runtime
        .family::<Key, u64>("noncomputing-pending-outer", 8)
        .unwrap();
    let child_for_outer = child.clone();
    assert_eq!(
        runtime
            .query(
                &outer,
                current,
                Key("outer"),
                CancellationToken::new(),
                move |context| {
                    context.query_registered(&child_for_outer, Key("child"))?;
                    Err(QueryAbort::Canceled)
                },
            )
            .unwrap_err(),
        QueryAbort::Canceled
    );
    assert_eq!(child_runs.load(Ordering::SeqCst), 1);
    assert_eq!(commits.load(Ordering::SeqCst), 0);

    let observer = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-pending-observer", 8, {
            let child = child.clone();
            move |context, _, _| {
                let ReadyQueryProbe::Ready(terminal) =
                    context.join_registered_noncomputing(&child, Key("child"))?
                else {
                    unreachable!("pending handoff is safely observed by the lifecycle");
                };
                assert_eq!(terminal.outcome(), &QueryOutcome::Success(1));
                Ok(QueryOutput::success(2))
            }
        })
        .unwrap();
    runtime
        .request_registered(
            &observer,
            current,
            Key("observer"),
            CancellationToken::new(),
        )
        .into_result()
        .unwrap();
    assert!(child.contains_retained_key(&Key("child")));
    assert_eq!(child_runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        commits.load(Ordering::SeqCst),
        1,
        "the observing root may complete the pending handoff exactly once"
    );
}

#[test]
fn noncomputing_join_cancellation_unwinds_waiter_and_leaves_owner_usable() {
    let runtime = QueryRuntime::new(2);
    let current = revision(1);
    publish_empty(&runtime, [current]);
    let claimed = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let target_runs = Arc::new(AtomicUsize::new(0));
    let target = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-cancel-target", 8, {
            let claimed = claimed.clone();
            let release = release.clone();
            let target_runs = target_runs.clone();
            move |_, _, _| {
                target_runs.fetch_add(1, Ordering::SeqCst);
                claimed.wait();
                release.wait();
                Ok(QueryOutput::success(7))
            }
        })
        .unwrap();
    let owner = thread::spawn({
        let runtime = runtime.clone();
        let target = target.clone();
        move || {
            runtime
                .request_registered(&target, current, Key("busy"), CancellationToken::new())
                .into_result()
                .unwrap()
        }
    });
    claimed.wait();

    let parked = Arc::new(Barrier::new(2));
    runtime.set_interpose({
        let parked = parked.clone();
        Arc::new(move |site| {
            if site == InterposeSite::NodeJoinPark {
                parked.wait();
            }
        })
    });
    let observer = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-cancel-observer", 8, {
            let target = target.clone();
            move |context, _, _| {
                context.join_registered_noncomputing(&target, Key("busy"))?;
                Ok(QueryOutput::success(1))
            }
        })
        .unwrap();
    let cancellation = CancellationToken::new();
    let observer_task = thread::spawn({
        let runtime = runtime.clone();
        let observer = observer.clone();
        let cancellation = cancellation.clone();
        move || runtime.request_registered(&observer, current, Key("observer"), cancellation)
    });
    parked.wait();
    cancellation.cancel();
    release.wait();
    assert_eq!(
        observer_task.join().unwrap().abort(),
        Some(&QueryAbort::Canceled)
    );
    assert_eq!(owner.join().unwrap().outcome(), &QueryOutcome::Success(7));
    runtime.clear_interpose();
    assert_eq!(target_runs.load(Ordering::SeqCst), 1);
    let reused =
        runtime.request_registered(&target, current, Key("busy"), CancellationToken::new());
    assert_eq!(reused.execution(), RequestExecution::Reused);
    assert_eq!(target_runs.load(Ordering::SeqCst), 1);
}

#[test]
fn noncomputing_join_keeps_cold_stale_and_aborted_owners_noncomputing() {
    let runtime = QueryRuntime::new(2);
    let first = revision(1);
    let second = revision(2);
    publish_empty(&runtime, [first, second]);
    let target_runs = Arc::new(AtomicUsize::new(0));
    let dependency_runs = Arc::new(AtomicUsize::new(0));
    let dependency = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-negative-dependency", 8, {
            let dependency_runs = dependency_runs.clone();
            move |_, _, _| {
                dependency_runs.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(1))
            }
        })
        .unwrap();
    let target = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-negative-target", 8, {
            let target_runs = target_runs.clone();
            let dependency = dependency.clone();
            move |context, _, key| {
                target_runs.fetch_add(1, Ordering::SeqCst);
                if key.0 == "stale" {
                    context.query_registered(&dependency, Key("dep"))?;
                }
                if key.0 == "abort" {
                    return Err(QueryAbort::Canceled);
                }
                Ok(QueryOutput::success(2))
            }
        })
        .unwrap();

    let cold_root = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-cold-root", 8, {
            let target = target.clone();
            move |context, _, _| {
                assert!(matches!(
                    context.join_registered_noncomputing(&target, Key("cold"))?,
                    ReadyQueryProbe::Miss
                ));
                Ok(QueryOutput::success(0))
            }
        })
        .unwrap();
    runtime
        .request_registered(&cold_root, first, Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(target_runs.load(Ordering::SeqCst), 0);
    assert_eq!(dependency_runs.load(Ordering::SeqCst), 0);

    runtime
        .request_registered(&target, first, Key("stale"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(target_runs.load(Ordering::SeqCst), 1);
    assert_eq!(dependency_runs.load(Ordering::SeqCst), 1);
    let stale_root = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-stale-root", 8, {
            let target = target.clone();
            move |context, _, _| {
                assert!(matches!(
                    context.join_registered_noncomputing(&target, Key("stale"))?,
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
    assert_eq!(target_runs.load(Ordering::SeqCst), 1);
    assert_eq!(dependency_runs.load(Ordering::SeqCst), 1);

    let claimed = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let abort_target = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-abort-target", 8, {
            let target_runs = target_runs.clone();
            let claimed = claimed.clone();
            let release = release.clone();
            move |_, _, _| {
                target_runs.fetch_add(1, Ordering::SeqCst);
                claimed.wait();
                release.wait();
                Err(QueryAbort::Canceled)
            }
        })
        .unwrap();
    let owner = thread::spawn({
        let abort_target = abort_target.clone();
        let runtime = runtime.clone();
        move || {
            runtime
                .request_registered(
                    &abort_target,
                    second,
                    Key("abort"),
                    CancellationToken::new(),
                )
                .into_result()
        }
    });
    claimed.wait();
    let abort_root = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-abort-root", 8, {
            let abort_target = abort_target.clone();
            move |context, _, _| {
                assert!(matches!(
                    context.join_registered_noncomputing(&abort_target, Key("abort"))?,
                    ReadyQueryProbe::Miss
                ));
                Ok(QueryOutput::success(0))
            }
        })
        .unwrap();
    let parked = Arc::new(Barrier::new(2));
    runtime.set_interpose({
        let parked = parked.clone();
        Arc::new(move |site| {
            if site == InterposeSite::NodeJoinPark {
                parked.wait();
            }
        })
    });
    let root_thread = thread::spawn({
        let abort_root = abort_root.clone();
        let runtime = runtime.clone();
        move || {
            runtime
                .request_registered(&abort_root, second, Key("root"), CancellationToken::new())
                .into_result()
                .unwrap()
        }
    });
    parked.wait();
    release.wait();
    root_thread.join().unwrap();
    runtime.clear_interpose();
    assert!(matches!(owner.join().unwrap(), Err(QueryAbort::Canceled)));
    assert_eq!(
        target_runs.load(Ordering::SeqCst),
        2,
        "owner abort runs once; the non-computing observer never retries it"
    );
}

#[test]
fn noncomputing_join_reports_an_exact_same_request_cycle() {
    let runtime = QueryRuntime::new(1);
    let current = revision(1);
    publish_empty(&runtime, [current]);
    let slot = Arc::new(OnceLock::new());
    let slot_for_body = slot.clone();
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("noncomputing-cycle", 8, move |context, _, key| {
            let family = slot_for_body.get().expect("family initialized");
            assert!(matches!(
                context.join_registered_noncomputing(family, key.clone()),
                Err(QueryAbort::Cycle(_))
            ));
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    slot.set(family.clone()).unwrap();
    runtime
        .request_registered(&family, current, Key("cycle"), CancellationToken::new())
        .into_result()
        .unwrap();
}

#[test]
fn noncomputing_join_primitive_has_no_claim_or_evaluator_authority() {
    let source = include_str!("node.rs");
    let primitive = source
        .split("fn join_task_registered_noncomputing(")
        .nth(1)
        .and_then(|source| {
            source
                .split("fn query_task_registered_for_validation(")
                .next()
        })
        .expect("non-computing registered join primitive");
    for required in [
        "existing_node(key)",
        "attempt.revision != task.revision",
        "self.join(task, node, attempt, owner)",
        "ReadyQueryProbe::NotReady",
    ] {
        assert!(
            primitive.contains(required),
            "non-computing join must retain its exact-current safety gate: {required}"
        );
    }
    for forbidden in [
        "self.node(",
        "valid_for_revision",
        "Some(evaluator",
        "Action::Compute",
        "publish(",
        "detach_terminal_attempt",
        "metrics.claims",
    ] {
        assert!(
            !primitive.contains(forbidden),
            "non-computing join regained evaluator/claim authority: {forbidden}"
        );
    }
}

fn publish_empty(runtime: &QueryRuntime, revisions: impl IntoIterator<Item = Revision>) {
    for revision in revisions {
        runtime.publish_revision(revision, []).unwrap();
    }
}

#[derive(Debug)]
struct RecordingHandoff {
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordingHandoff {
    fn new(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self { events }
    }
}

impl QueryAttemptHandoff for RecordingHandoff {
    fn commit(&mut self) {
        lock(&self.events).push("commit");
    }

    fn abort(&mut self) {
        lock(&self.events).push("abort");
    }
}

#[derive(Debug)]
struct RetryStateHandoff {
    name: &'static str,
    pending: bool,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl QueryAttemptHandoff for RetryStateHandoff {
    fn commit(&mut self) {
        assert!(self.pending, "a retryable handoff commits from pending");
        self.pending = false;
        lock(&self.events).push(self.name);
    }

    fn abort(&mut self) {
        assert!(
            !self.pending,
            "an unattempted handoff must not be aborted during rollback"
        );
        self.pending = true;
        lock(&self.events).push("abort");
    }
}

#[derive(Debug)]
struct PublicationHandoff {
    family: QueryFamily<Key, u64>,
    key: Key,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl QueryAttemptHandoff for PublicationHandoff {
    fn commit(&mut self) {
        assert!(
            self.family.contains_retained_key(&self.key),
            "attempt handoff commits only after its terminal is installed"
        );
        lock(&self.events).push("commit");
    }

    fn abort(&mut self) {
        lock(&self.events).push("abort");
    }
}

#[derive(Debug)]
struct PinSetHandoff {
    pins: Option<RetainedPinSet>,
    committed: Arc<Mutex<Option<RetainedPinSet>>>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl QueryAttemptHandoff for PinSetHandoff {
    fn commit(&mut self) {
        let pins = self
            .pins
            .take()
            .expect("a rolled-back handoff restores its pending pins");
        *lock(&self.committed) = Some(pins);
        lock(&self.events).push("commit");
    }

    fn abort(&mut self) {
        if self.pins.is_none() {
            self.pins = lock(&self.committed).take();
        }
        lock(&self.events).push("abort");
    }
}

#[derive(Debug)]
struct CountingHandoff {
    commits: Arc<AtomicUsize>,
    aborts: Arc<AtomicUsize>,
}

impl QueryAttemptHandoff for CountingHandoff {
    fn commit(&mut self) {
        self.commits.fetch_add(1, Ordering::SeqCst);
    }

    fn abort(&mut self) {
        self.aborts.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct BlockingHandoff {
    commit_started: mpsc::Sender<()>,
    release_commit: Arc<Barrier>,
    commits: Arc<AtomicUsize>,
}

impl QueryAttemptHandoff for BlockingHandoff {
    fn commit(&mut self) {
        self.commits.fetch_add(1, Ordering::SeqCst);
        self.commit_started.send(()).unwrap();
        self.release_commit.wait();
    }

    fn abort(&mut self) {}
}

#[derive(Debug)]
struct CancelBlockingHandoff {
    commit_started: mpsc::Sender<()>,
    release_commit: Arc<Barrier>,
    block_once: Arc<AtomicBool>,
    commits: Arc<AtomicUsize>,
    aborts: Arc<AtomicUsize>,
}

impl QueryAttemptHandoff for CancelBlockingHandoff {
    fn commit(&mut self) {
        self.commits.fetch_add(1, Ordering::SeqCst);
        if self.block_once.swap(false, Ordering::SeqCst) {
            self.commit_started.send(()).unwrap();
            self.release_commit.wait();
        }
    }

    fn abort(&mut self) {
        self.aborts.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct ReentrantRequestHandoff {
    runtime: QueryRuntime,
    family: QueryFamily<Key, u64>,
    other: QueryFamily<Key, u64>,
    key: Key,
    revision: Revision,
    evaluator_runs: Arc<AtomicUsize>,
    results: Arc<Mutex<Vec<(&'static str, QueryAbort)>>>,
}

impl ReentrantRequestHandoff {
    fn probe(&self, phase: &'static str) {
        let same_runs = self.evaluator_runs.clone();
        let same = self
            .runtime
            .query(
                &self.family,
                self.revision,
                self.key.clone(),
                CancellationToken::new(),
                move |_| {
                    same_runs.fetch_add(1, Ordering::SeqCst);
                    Ok(QueryOutput::success(9))
                },
            )
            .unwrap_err();
        let other_runs = self.evaluator_runs.clone();
        let other = self
            .runtime
            .query(
                &self.other,
                self.revision,
                Key("different-root"),
                CancellationToken::new(),
                move |_| {
                    other_runs.fetch_add(1, Ordering::SeqCst);
                    Ok(QueryOutput::success(10))
                },
            )
            .unwrap_err();
        lock(&self.results).extend([(phase, same), (phase, other)]);
    }
}

impl QueryAttemptHandoff for ReentrantRequestHandoff {
    fn commit(&mut self) {
        self.probe("commit");
    }

    fn abort(&mut self) {
        self.probe("abort");
    }
}

#[test]
fn attempt_handoff_commits_after_publication_without_becoming_memo_state() {
    let runtime = QueryRuntime::new(1);
    let input = InputIdentity::new("source", "handoff");
    let first_revision = revision(1);
    let second_revision = revision(2);
    runtime
        .publish_revision(first_revision, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second_revision, [(input.clone(), 2)])
        .unwrap();
    let family = runtime.family::<Key, u64>("attempt-handoff", 4).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));

    let run = |revision, offset| {
        let family_for_handoff = family.clone();
        let events = events.clone();
        let input = input.clone();
        runtime
            .query(
                &family,
                revision,
                Key("body"),
                CancellationToken::new(),
                move |context| {
                    context.input(input)?;
                    context.register_attempt_handoff(PublicationHandoff {
                        family: family_for_handoff,
                        key: Key("body"),
                        events,
                    });
                    Ok(QueryOutput::success(7)
                        .with_diagnostics(vec![QueryDiagnostic::new(
                            "stable",
                            "payload",
                            Some(PresentationPosition::new("main.rue", offset)),
                        )])
                        .with_work(vec![WorkItem::new("body", 3)]))
                },
            )
            .unwrap()
    };

    let first = run(first_revision, 10);
    let second = run(second_revision, 20);
    assert_eq!(first.stamp(), second.stamp(), "equal semantics stay red");
    assert_eq!(first.outcome(), second.outcome());
    assert_eq!(first.work(), second.work());
    assert_eq!(
        second.diagnostics()[0]
            .presentation
            .as_ref()
            .unwrap()
            .offset,
        20
    );
    assert_eq!(*lock(&events), ["commit", "commit"]);

    let reused = runtime.request(
        &family,
        second_revision,
        Key("body"),
        CancellationToken::new(),
        |_| panic!("a warm reuse does not execute an evaluator or a handoff"),
    );
    assert_eq!(reused.execution(), RequestExecution::Reused);
    assert_eq!(*lock(&events), ["commit", "commit"]);
}

#[test]
fn joined_request_runs_only_the_owner_attempts_handoff() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let barrier = Arc::new(Barrier::new(2));
    let evaluator_events = events.clone();
    let evaluator_barrier = barrier.clone();
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("attempt-handoff-join", 4, move |context, _, _| {
            context.register_attempt_handoff(RecordingHandoff::new(evaluator_events.clone()));
            evaluator_barrier.wait();
            evaluator_barrier.wait();
            Ok(QueryOutput::success(1))
        })
        .unwrap();

    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner = thread::spawn(move || {
        owner_runtime.request_registered(
            &owner_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
        )
    });
    barrier.wait();
    let waiter_runtime = runtime.clone();
    let waiter_family = family.clone();
    let waiter = thread::spawn(move || {
        waiter_runtime.request_registered(
            &waiter_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
        )
    });
    runtime.wait_for_metrics(|metrics| metrics.joins == 1);
    barrier.wait();

    let owner = owner.join().unwrap();
    let waiter = waiter.join().unwrap();
    assert_eq!(owner.execution(), RequestExecution::Computed);
    assert_eq!(waiter.execution(), RequestExecution::Joined);
    assert_eq!(*lock(&events), ["commit"]);
}

#[test]
fn attempt_handoff_aborts_on_error_and_post_body_cancellation() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family::<Key, u64>("attempt-handoff-abort", 4)
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));

    let error_events = events.clone();
    assert_eq!(
        runtime
            .query(
                &family,
                revision(1),
                Key("error"),
                CancellationToken::new(),
                move |context| {
                    context.register_attempt_handoff(RecordingHandoff::new(error_events));
                    Err(QueryAbort::MissingInput(InputIdentity::new(
                        "source", "missing",
                    )))
                },
            )
            .unwrap_err(),
        QueryAbort::MissingInput(InputIdentity::new("source", "missing"))
    );

    let cancellation = CancellationToken::new();
    let cancel_from_body = cancellation.clone();
    let cancel_events = events.clone();
    assert_eq!(
        runtime
            .query(
                &family,
                revision(1),
                Key("canceled"),
                cancellation,
                move |context| {
                    context.register_attempt_handoff(RecordingHandoff::new(cancel_events));
                    cancel_from_body.cancel();
                    Ok(QueryOutput::success(1))
                },
            )
            .unwrap_err(),
        QueryAbort::Canceled
    );

    assert_eq!(*lock(&events), ["abort", "abort"]);
    assert!(!family.contains_retained_key(&Key("error")));
    assert!(!family.contains_retained_key(&Key("canceled")));
}

#[test]
fn handoff_callbacks_cannot_reenter_same_or_different_query_roots() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family::<Key, u64>("handoff-reentrant-root", 4)
        .unwrap();
    let other = runtime
        .family::<Key, u64>("handoff-reentrant-other", 4)
        .unwrap();
    let evaluator_runs = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(Vec::new()));

    let commit_handoff = ReentrantRequestHandoff {
        runtime: runtime.clone(),
        family: family.clone(),
        other: other.clone(),
        key: Key("commit"),
        revision: revision(1),
        evaluator_runs: evaluator_runs.clone(),
        results: results.clone(),
    };
    runtime
        .query(
            &family,
            revision(1),
            Key("commit"),
            CancellationToken::new(),
            move |context| {
                context.register_attempt_handoff(commit_handoff);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();

    let abort_handoff = ReentrantRequestHandoff {
        runtime: runtime.clone(),
        family: family.clone(),
        other: other.clone(),
        key: Key("abort"),
        revision: revision(1),
        evaluator_runs: evaluator_runs.clone(),
        results: results.clone(),
    };
    assert_eq!(
        runtime
            .query(
                &family,
                revision(1),
                Key("abort"),
                CancellationToken::new(),
                move |context| {
                    context.register_attempt_handoff(abort_handoff);
                    Err(QueryAbort::Canceled)
                },
            )
            .unwrap_err(),
        QueryAbort::Canceled
    );

    assert_eq!(
        *lock(&results),
        [
            ("commit", QueryAbort::Canceled),
            ("commit", QueryAbort::Canceled),
            ("abort", QueryAbort::Canceled),
            ("abort", QueryAbort::Canceled),
        ]
    );
    assert_eq!(evaluator_runs.load(Ordering::SeqCst), 0);

    let runs = evaluator_runs.clone();
    runtime
        .query(
            &other,
            revision(1),
            Key("after-callback"),
            CancellationToken::new(),
            move |_| {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(11))
            },
        )
        .unwrap();
    assert_eq!(
        evaluator_runs.load(Ordering::SeqCst),
        1,
        "the thread-local phase guard is removed after each callback"
    );
}

#[test]
fn panic_unwind_aborts_attempt_handoffs_before_resuming() {
    // The Rue unit runner treats even a caught panic as a failed test, so
    // guard the unwind branch structurally and exercise the same consuming
    // abort operation directly.
    let events = Arc::new(Mutex::new(Vec::new()));
    TaskFrameOutput {
        dependencies: Vec::new(),
        inputs: Vec::new(),
        work: Vec::new(),
        handoffs: AttemptHandoffs {
            pending: vec![Box::new(RecordingHandoff::new(events.clone()))],
            observed: Vec::new(),
        },
        cone_missing_observation: false,
    }
    .abort_handoffs();
    assert_eq!(*lock(&events), ["abort"]);

    let source = include_str!("node.rs");
    let unwind = source
        .split("Err(payload) => {")
        .nth(1)
        .expect("query evaluator has a panic-unwind branch")
        .split("resume_unwind(payload)")
        .next()
        .expect("panic-unwind branch resumes the original payload");
    assert!(
        unwind.contains("frame.abort_handoffs();"),
        "panic unwinding must abort attempt resources before resuming"
    );
}

#[test]
fn attempt_handoff_transfers_retained_pins_only_on_commit() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime
        .family::<Slot, u64>("attempt-handoff-leaf", 1)
        .unwrap();
    let root = runtime
        .family::<Key, u64>("attempt-handoff-root", 2)
        .unwrap();
    let leaf_computes = Arc::new(AtomicUsize::new(0));
    let committed = Arc::new(Mutex::new(None));
    let events = Arc::new(Mutex::new(Vec::new()));

    let leaf_for_root = leaf.clone();
    let computes = leaf_computes.clone();
    let committed_for_root = committed.clone();
    let events_for_root = events.clone();
    runtime
        .query(
            &root,
            revision(1),
            Key("commit"),
            CancellationToken::new(),
            move |context| {
                let terminal = context.query(&leaf_for_root, Slot(0), move |_| {
                    computes.fetch_add(1, Ordering::SeqCst);
                    Ok(QueryOutput::success(0))
                })?;
                let mut pins = RetainedPinSet::new();
                assert!(pins.lease(leaf_for_root.pin_terminal(&terminal).unwrap()));
                context.register_attempt_handoff(PinSetHandoff {
                    pins: Some(pins),
                    committed: committed_for_root,
                    events: events_for_root,
                });
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();

    for slot in 1..=8 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(slot),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(slot)),
            )
            .unwrap();
    }
    runtime
        .query(
            &leaf,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            |_| panic!("the committed handoff retains the observed terminal"),
        )
        .unwrap();
    assert_eq!(leaf_computes.load(Ordering::SeqCst), 1);

    drop(lock(&committed).take());
    for slot in 9..=16 {
        runtime
            .query(
                &leaf,
                revision(1),
                Slot(slot),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(slot)),
            )
            .unwrap();
    }
    let computes = leaf_computes.clone();
    runtime
        .query(
            &leaf,
            revision(1),
            Slot(0),
            CancellationToken::new(),
            move |_| {
                computes.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();
    assert_eq!(leaf_computes.load(Ordering::SeqCst), 2);
    assert_eq!(*lock(&events), ["commit"]);

    let leaf_for_abort = leaf.clone();
    let abort_events = events.clone();
    let abort_committed = committed.clone();
    assert_eq!(
        runtime
            .query(
                &root,
                revision(1),
                Key("abort"),
                CancellationToken::new(),
                move |context| {
                    let terminal = context
                        .query(&leaf_for_abort, Slot(99), |_| Ok(QueryOutput::success(99)))?;
                    let mut pins = RetainedPinSet::new();
                    assert!(pins.lease(leaf_for_abort.pin_terminal(&terminal).unwrap()));
                    context.register_attempt_handoff(PinSetHandoff {
                        pins: Some(pins),
                        committed: abort_committed,
                        events: abort_events,
                    });
                    Err(QueryAbort::Canceled)
                },
            )
            .unwrap_err(),
        QueryAbort::Canceled
    );
    assert!(lock(&committed).is_none());
    assert_eq!(*lock(&events), ["commit", "abort"]);
}

#[test]
fn speculative_validation_defers_handoff_until_an_observed_reuse() {
    let runtime = QueryRuntime::new(1);
    let input = InputIdentity::new("source", "speculative-handoff");
    let first_revision = Revision::new(1, 1);
    let second_revision = Revision::new(2, 1);
    runtime
        .publish_revision(first_revision, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second_revision, [(input.clone(), 2)])
        .unwrap();

    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let child_input = input.clone();
    let child_commits = commits.clone();
    let child_aborts = aborts.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>(
            "speculative-handoff-child",
            8,
            move |context, _, _| {
                context.input(child_input.clone())?;
                context.register_attempt_handoff(CountingHandoff {
                    commits: child_commits.clone(),
                    aborts: child_aborts.clone(),
                });
                Ok(QueryOutput::success(7))
            },
        )
        .unwrap();
    let root_child = child.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "speculative-handoff-root",
            8,
            move |context, _, _| {
                context.query_registered(&root_child, Key("child"))?;
                Ok(QueryOutput::success(9))
            },
        )
        .unwrap();

    let first =
        runtime.request_registered(&root, first_revision, Key("root"), CancellationToken::new());
    assert_eq!(first.execution(), RequestExecution::Computed);
    assert_eq!(commits.load(Ordering::SeqCst), 1);

    let validated = runtime.request_registered(
        &root,
        second_revision,
        Key("root"),
        CancellationToken::new(),
    );
    assert_eq!(validated.execution(), RequestExecution::Reused);
    assert_eq!(
        commits.load(Ordering::SeqCst),
        1,
        "speculative validation must not promote its newly computed child"
    );
    assert_eq!(aborts.load(Ordering::SeqCst), 0);

    let observed = runtime.request_registered(
        &child,
        second_revision,
        Key("child"),
        CancellationToken::new(),
    );
    assert_eq!(observed.execution(), RequestExecution::Reused);
    assert_eq!(commits.load(Ordering::SeqCst), 2);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);
}

#[test]
fn speculative_nested_handoffs_follow_their_parent_terminal_until_observed() {
    let runtime = QueryRuntime::new(1);
    let child_input = InputIdentity::new("source", "speculative-nested-child");
    let middle_input = InputIdentity::new("source", "speculative-nested-middle");
    let first_revision = Revision::new(10, 1);
    let second_revision = Revision::new(11, 1);
    runtime
        .publish_revision(
            first_revision,
            [(child_input.clone(), 1), (middle_input.clone(), 1)],
        )
        .unwrap();
    runtime
        .publish_revision(
            second_revision,
            [(child_input.clone(), 2), (middle_input.clone(), 2)],
        )
        .unwrap();

    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let child_source = child_input.clone();
    let child_commits = commits.clone();
    let child_aborts = aborts.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>(
            "speculative-nested-handoff-child",
            8,
            move |context, _, _| {
                context.input(child_source.clone())?;
                context.register_attempt_handoff(CountingHandoff {
                    commits: child_commits.clone(),
                    aborts: child_aborts.clone(),
                });
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let middle_child = child.clone();
    let middle_source = middle_input.clone();
    let middle_commits = commits.clone();
    let middle_aborts = aborts.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>(
            "speculative-nested-handoff-middle",
            8,
            move |context, _, _| {
                context.input(middle_source.clone())?;
                context.query_registered(&middle_child, Key("child"))?;
                context.register_attempt_handoff(CountingHandoff {
                    commits: middle_commits.clone(),
                    aborts: middle_aborts.clone(),
                });
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();
    let root_middle = middle.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "speculative-nested-handoff-root",
            8,
            move |context, _, _| {
                context.query_registered(&root_middle, Key("middle"))?;
                Ok(QueryOutput::success(3))
            },
        )
        .unwrap();

    let first =
        runtime.request_registered(&root, first_revision, Key("root"), CancellationToken::new());
    assert_eq!(first.execution(), RequestExecution::Computed);
    assert_eq!(commits.load(Ordering::SeqCst), 2);

    let validated = runtime.request_registered(
        &root,
        second_revision,
        Key("root"),
        CancellationToken::new(),
    );
    assert_eq!(validated.execution(), RequestExecution::Reused);
    assert_eq!(
        commits.load(Ordering::SeqCst),
        2,
        "nested handoffs created by speculative evaluation stay pending"
    );
    assert_eq!(aborts.load(Ordering::SeqCst), 0);

    let observed = runtime.request_registered(
        &middle,
        second_revision,
        Key("middle"),
        CancellationToken::new(),
    );
    assert_eq!(observed.execution(), RequestExecution::Reused);
    assert_eq!(
        commits.load(Ordering::SeqCst),
        4,
        "observing the parent commits both its own and nested handoffs"
    );
    assert_eq!(aborts.load(Ordering::SeqCst), 0);
}

#[test]
fn nested_handoff_stays_pending_when_the_outer_root_aborts() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let child_commits = commits.clone();
    let child_aborts = aborts.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>(
            "nested-pending-handoff-child",
            4,
            move |context, _, _| {
                context.register_attempt_handoff(CountingHandoff {
                    commits: child_commits.clone(),
                    aborts: child_aborts.clone(),
                });
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let root = runtime
        .family::<Key, u64>("nested-pending-handoff-root", 4)
        .unwrap();
    let child_for_root = child.clone();
    assert_eq!(
        runtime
            .query(
                &root,
                revision(1),
                Key("root"),
                CancellationToken::new(),
                move |context| {
                    context.query_registered(&child_for_root, Key("child"))?;
                    Err(QueryAbort::Canceled)
                },
            )
            .unwrap_err(),
        QueryAbort::Canceled
    );
    assert_eq!(commits.load(Ordering::SeqCst), 0);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);

    let child_for_reused_root = child.clone();
    assert_eq!(
        runtime
            .query(
                &root,
                revision(1),
                Key("root"),
                CancellationToken::new(),
                move |context| {
                    context.query_registered(&child_for_reused_root, Key("child"))?;
                    Err(QueryAbort::Canceled)
                },
            )
            .unwrap_err(),
        QueryAbort::Canceled
    );
    assert_eq!(
        commits.load(Ordering::SeqCst),
        0,
        "a nested reuse remains pending when its outer root aborts"
    );

    let observed =
        runtime.request_registered(&child, revision(1), Key("child"), CancellationToken::new());
    assert_eq!(observed.execution(), RequestExecution::Reused);
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);
}

#[test]
fn nested_join_handoff_stays_pending_when_both_outer_roots_abort() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let body_barrier = Arc::new(Barrier::new(2));
    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let evaluator_barrier = body_barrier.clone();
    let evaluator_commits = commits.clone();
    let evaluator_aborts = aborts.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>(
            "nested-join-pending-handoff-child",
            4,
            move |context, _, _| {
                context.register_attempt_handoff(CountingHandoff {
                    commits: evaluator_commits.clone(),
                    aborts: evaluator_aborts.clone(),
                });
                evaluator_barrier.wait();
                evaluator_barrier.wait();
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let root = runtime
        .family::<Key, u64>("nested-join-pending-handoff-root", 4)
        .unwrap();

    let owner_runtime = runtime.clone();
    let owner_root = root.clone();
    let owner_child = child.clone();
    let owner = thread::spawn(move || {
        owner_runtime.query(
            &owner_root,
            revision(1),
            Key("owner"),
            CancellationToken::new(),
            move |context| {
                context.query_registered(&owner_child, Key("child"))?;
                Err(QueryAbort::Canceled)
            },
        )
    });
    body_barrier.wait();
    let waiter_runtime = runtime.clone();
    let waiter_root = root.clone();
    let waiter_child = child.clone();
    let waiter = thread::spawn(move || {
        waiter_runtime.query(
            &waiter_root,
            revision(1),
            Key("waiter"),
            CancellationToken::new(),
            move |context| {
                context.query_registered(&waiter_child, Key("child"))?;
                Err(QueryAbort::Canceled)
            },
        )
    });
    runtime.wait_for_metrics(|metrics| metrics.joins == 1);
    body_barrier.wait();
    assert_eq!(owner.join().unwrap().unwrap_err(), QueryAbort::Canceled);
    assert_eq!(waiter.join().unwrap().unwrap_err(), QueryAbort::Canceled);
    assert_eq!(commits.load(Ordering::SeqCst), 0);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);

    let observed =
        runtime.request_registered(&child, revision(1), Key("child"), CancellationToken::new());
    assert_eq!(observed.execution(), RequestExecution::Reused);
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);
}

#[test]
fn opposite_order_roots_commit_shared_pending_handoffs_without_deadlock() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let child_commits = commits.clone();
    let child_aborts = aborts.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>(
            "opposite-order-handoff-child",
            4,
            move |context, _, key| {
                context.register_attempt_handoff(CountingHandoff {
                    commits: child_commits.clone(),
                    aborts: child_aborts.clone(),
                });
                Ok(QueryOutput::success(u64::from(key.0 == "b")))
            },
        )
        .unwrap();
    let seed = runtime
        .family::<Key, u64>("opposite-order-handoff-seed", 2)
        .unwrap();
    let seed_child = child.clone();
    assert_eq!(
        runtime
            .query(
                &seed,
                revision(1),
                Key("seed"),
                CancellationToken::new(),
                move |context| {
                    context.query_registered(&seed_child, Key("a"))?;
                    context.query_registered(&seed_child, Key("b"))?;
                    Err(QueryAbort::Canceled)
                },
            )
            .unwrap_err(),
        QueryAbort::Canceled
    );

    let root = runtime
        .family::<Key, u64>("opposite-order-handoff-root", 2)
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let first_runtime = runtime.clone();
    let first_root = root.clone();
    let first_child = child.clone();
    let first_barrier = barrier.clone();
    let first = thread::spawn(move || {
        first_runtime.query(
            &first_root,
            revision(1),
            Key("first"),
            CancellationToken::new(),
            move |context| {
                context.query_registered(&first_child, Key("a"))?;
                context.query_registered(&first_child, Key("b"))?;
                first_barrier.wait();
                Ok(QueryOutput::success(1))
            },
        )
    });
    let second_runtime = runtime.clone();
    let second_root = root.clone();
    let second_child = child.clone();
    let second = thread::spawn(move || {
        second_runtime.query(
            &second_root,
            revision(1),
            Key("second"),
            CancellationToken::new(),
            move |context| {
                context.query_registered(&second_child, Key("b"))?;
                context.query_registered(&second_child, Key("a"))?;
                barrier.wait();
                Ok(QueryOutput::success(2))
            },
        )
    });
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    assert_eq!(commits.load(Ordering::SeqCst), 2);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);
}

#[test]
fn pending_handoff_aborts_when_its_terminal_is_evicted() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let child_commits = commits.clone();
    let child_aborts = aborts.clone();
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "evicted-pending-handoff-child",
            1,
            move |context, _, key| {
                context.register_attempt_handoff(CountingHandoff {
                    commits: child_commits.clone(),
                    aborts: child_aborts.clone(),
                });
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    let root = runtime
        .family::<Key, u64>("evicted-pending-handoff-root", 1)
        .unwrap();
    let child_for_root = child.clone();
    assert_eq!(
        runtime
            .query(
                &root,
                revision(1),
                Key("root"),
                CancellationToken::new(),
                move |context| {
                    context.query_registered(&child_for_root, Slot(0))?;
                    Err(QueryAbort::Canceled)
                },
            )
            .unwrap_err(),
        QueryAbort::Canceled
    );
    assert_eq!(commits.load(Ordering::SeqCst), 0);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);

    runtime.request_registered(&child, revision(1), Slot(1), CancellationToken::new());
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(
        aborts.load(Ordering::SeqCst),
        1,
        "eviction aborts the still-pending resource outside memo state"
    );
}

#[test]
fn blocking_commit_gates_concurrent_reuse() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let (commit_started_tx, commit_started_rx) = mpsc::channel();
    let release_commit = Arc::new(Barrier::new(2));
    let commits = Arc::new(AtomicUsize::new(0));
    let evaluator_release = release_commit.clone();
    let evaluator_commits = commits.clone();
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("blocking-handoff-reuse", 4, move |context, _, _| {
            context.register_attempt_handoff(BlockingHandoff {
                commit_started: commit_started_tx.clone(),
                release_commit: evaluator_release.clone(),
                commits: evaluator_commits.clone(),
            });
            Ok(QueryOutput::success(1))
        })
        .unwrap();

    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner = thread::spawn(move || {
        owner_runtime.request_registered(
            &owner_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
        )
    });
    commit_started_rx.recv().unwrap();

    let (waiter_done_tx, waiter_done_rx) = mpsc::channel();
    let waiter_runtime = runtime.clone();
    let waiter_family = family.clone();
    let waiter = thread::spawn(move || {
        let result = waiter_runtime.request_registered(
            &waiter_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
        );
        waiter_done_tx.send(()).unwrap();
        result
    });
    assert!(
        waiter_done_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "a reusable terminal is not returned before its handoff commits"
    );
    release_commit.wait();

    assert_eq!(
        owner.join().unwrap().execution(),
        RequestExecution::Computed
    );
    assert_eq!(waiter.join().unwrap().execution(), RequestExecution::Reused);
    assert_eq!(commits.load(Ordering::SeqCst), 1);
}

#[test]
fn canceled_reuse_waiter_does_not_wait_for_a_blocked_commit() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let (commit_started_tx, commit_started_rx) = mpsc::channel();
    let release_commit = Arc::new(Barrier::new(2));
    let evaluator_release = release_commit.clone();
    let family = runtime
        .family_with_evaluator::<Key, u64, _>(
            "cancel-blocked-handoff-reuse",
            4,
            move |context, _, _| {
                context.register_attempt_handoff(BlockingHandoff {
                    commit_started: commit_started_tx.clone(),
                    release_commit: evaluator_release.clone(),
                    commits: Arc::new(AtomicUsize::new(0)),
                });
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner = thread::spawn(move || {
        owner_runtime.request_registered(
            &owner_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
        )
    });
    commit_started_rx.recv().unwrap();

    let cancellation = CancellationToken::new();
    let (at_park_tx, at_park_rx) = mpsc::channel();
    let hook_cancellation = cancellation.clone();
    runtime.set_interpose(Arc::new(move |site| {
        if site == InterposeSite::HandoffCommitPark {
            at_park_tx.send(()).unwrap();
            while !hook_cancellation.is_canceled() {
                thread::yield_now();
            }
        }
    }));
    let cancel_token = cancellation.clone();
    let canceler = thread::spawn(move || {
        at_park_rx.recv().unwrap();
        // The waiter still owns the predicate lock here. `cancel` must
        // set its predicate, synchronize with that lock, then notify only
        // after parking begins.
        cancel_token.cancel();
    });
    let waiter_cancellation = cancellation.clone();
    let (waiter_done_tx, waiter_done_rx) = mpsc::channel();
    let waiter_runtime = runtime.clone();
    let waiter_family = family.clone();
    let waiter = thread::spawn(move || {
        let result = waiter_runtime.request_registered(
            &waiter_family,
            revision(1),
            Key("shared"),
            waiter_cancellation,
        );
        waiter_done_tx.send(()).unwrap();
        result
    });
    let waiter_woke = waiter_done_rx.recv_timeout(Duration::from_secs(1));
    if waiter_woke.is_err() {
        release_commit.wait();
        let _ = owner.join().unwrap();
        let _ = waiter.join().unwrap();
        canceler.join().unwrap();
        runtime.clear_interpose();
        panic!("cancellation did not wake a root waiting for another root's commit");
    }
    let canceled = waiter.join().unwrap();
    canceler.join().unwrap();
    runtime.clear_interpose();
    assert_eq!(canceled.abort(), Some(&QueryAbort::Canceled));

    release_commit.wait();
    owner.join().unwrap();
}

#[test]
fn blocking_commit_does_not_hold_the_execution_permit() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let (commit_started_tx, commit_started_rx) = mpsc::channel();
    let release_commit = Arc::new(Barrier::new(2));
    let evaluator_release = release_commit.clone();
    let committing = runtime
        .family_with_evaluator::<Key, u64, _>("blocking-handoff-permit", 2, move |context, _, _| {
            context.register_attempt_handoff(BlockingHandoff {
                commit_started: commit_started_tx.clone(),
                release_commit: evaluator_release.clone(),
                commits: Arc::new(AtomicUsize::new(0)),
            });
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let unrelated = runtime
        .family::<Key, u64>("blocking-handoff-unrelated", 2)
        .unwrap();
    let owner_runtime = runtime.clone();
    let owner_family = committing.clone();
    let owner = thread::spawn(move || {
        owner_runtime.request_registered(
            &owner_family,
            revision(1),
            Key("commit"),
            CancellationToken::new(),
        )
    });
    commit_started_rx.recv().unwrap();

    runtime
        .query(
            &unrelated,
            revision(1),
            Key("ready"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(2)),
        )
        .unwrap();
    release_commit.wait();
    owner.join().unwrap();
}

#[test]
fn blocking_commit_gates_a_waiter_that_joined_before_publication() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let body_barrier = Arc::new(Barrier::new(2));
    let release_commit = Arc::new(Barrier::new(2));
    let (commit_started_tx, commit_started_rx) = mpsc::channel();
    let commits = Arc::new(AtomicUsize::new(0));
    let evaluator_body = body_barrier.clone();
    let evaluator_release = release_commit.clone();
    let evaluator_commits = commits.clone();
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("blocking-handoff-join", 4, move |context, _, _| {
            context.register_attempt_handoff(BlockingHandoff {
                commit_started: commit_started_tx.clone(),
                release_commit: evaluator_release.clone(),
                commits: evaluator_commits.clone(),
            });
            evaluator_body.wait();
            evaluator_body.wait();
            Ok(QueryOutput::success(1))
        })
        .unwrap();

    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner = thread::spawn(move || {
        owner_runtime.request_registered(
            &owner_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
        )
    });
    body_barrier.wait();

    let (waiter_done_tx, waiter_done_rx) = mpsc::channel();
    let waiter_runtime = runtime.clone();
    let waiter_family = family.clone();
    let waiter = thread::spawn(move || {
        let result = waiter_runtime.request_registered(
            &waiter_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
        );
        waiter_done_tx.send(()).unwrap();
        result
    });
    runtime.wait_for_metrics(|metrics| metrics.joins == 1);
    body_barrier.wait();
    commit_started_rx.recv().unwrap();
    assert!(
        waiter_done_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "a joined terminal is not returned before its handoff commits"
    );
    release_commit.wait();

    assert_eq!(
        owner.join().unwrap().execution(),
        RequestExecution::Computed
    );
    assert_eq!(waiter.join().unwrap().execution(), RequestExecution::Joined);
    assert_eq!(commits.load(Ordering::SeqCst), 1);
}

#[test]
fn cancellation_during_commit_rolls_the_handoff_back_to_pending() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let (commit_started_tx, commit_started_rx) = mpsc::channel();
    let release_commit = Arc::new(Barrier::new(2));
    let block_once = Arc::new(AtomicBool::new(true));
    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let evaluator_release = release_commit.clone();
    let evaluator_block = block_once.clone();
    let evaluator_commits = commits.clone();
    let evaluator_aborts = aborts.clone();
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("canceled-handoff-commit", 4, move |context, _, _| {
            context.register_attempt_handoff(CancelBlockingHandoff {
                commit_started: commit_started_tx.clone(),
                release_commit: evaluator_release.clone(),
                block_once: evaluator_block.clone(),
                commits: evaluator_commits.clone(),
                aborts: evaluator_aborts.clone(),
            });
            context.register_attempt_handoff(RetryStateHandoff {
                name: "suffix-commit",
                pending: true,
                events: Arc::new(Mutex::new(Vec::new())),
            });
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let cancellation = CancellationToken::new();
    let owner_cancellation = cancellation.clone();
    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner = thread::spawn(move || {
        owner_runtime.request_registered(
            &owner_family,
            revision(1),
            Key("shared"),
            owner_cancellation,
        )
    });
    commit_started_rx.recv().unwrap();
    cancellation.cancel();
    release_commit.wait();
    let canceled = owner.join().unwrap();
    assert_eq!(canceled.abort(), Some(&QueryAbort::Canceled));
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(aborts.load(Ordering::SeqCst), 1);

    let retry = runtime.request_registered(
        &family,
        revision(1),
        Key("shared"),
        CancellationToken::new(),
    );
    assert_eq!(retry.execution(), RequestExecution::Reused);
    assert_eq!(commits.load(Ordering::SeqCst), 2);
    assert_eq!(aborts.load(Ordering::SeqCst), 1);
}

#[test]
fn cancellation_preserves_unattempted_lifecycles_for_retry() {
    let runtime = QueryRuntime::new(1);
    let events = Arc::new(Mutex::new(Vec::new()));
    let first = Arc::new(AttemptHandoffLifecycle::new(
        vec![Box::new(RetryStateHandoff {
            name: "first",
            pending: true,
            events: events.clone(),
        })],
        Vec::new(),
    ));
    let second = Arc::new(AttemptHandoffLifecycle::new(
        vec![
            Box::new(RetryStateHandoff {
                name: "second",
                pending: true,
                events: events.clone(),
            }),
            Box::new(RetryStateHandoff {
                name: "third",
                pending: true,
                events: events.clone(),
            }),
        ],
        Vec::new(),
    ));
    let owner = TaskId(1);
    let cancellation = CancellationToken::new();
    let AttemptHandoffCommit::Claimed(mut first_handoffs) =
        first.begin_commit(owner, &cancellation, &runtime.core)
    else {
        panic!("fresh lifecycle is claimable")
    };
    let AttemptHandoffCommit::Claimed(second_handoffs) =
        second.begin_commit(owner, &cancellation, &runtime.core)
    else {
        panic!("the untouched lifecycle is claimable")
    };
    first_handoffs[0].commit();
    rollback_handoff_batches(
        owner,
        vec![
            (first.clone(), first_handoffs),
            (second.clone(), second_handoffs),
        ],
        vec![1, 0],
    );

    let retry = TaskId(2);
    let AttemptHandoffCommit::Claimed(mut first_handoffs) =
        first.begin_commit(retry, &cancellation, &runtime.core)
    else {
        panic!("the attempted lifecycle returns to pending")
    };
    let AttemptHandoffCommit::Claimed(mut second_handoffs) =
        second.begin_commit(retry, &cancellation, &runtime.core)
    else {
        panic!("the unattempted lifecycle remains pending")
    };
    for handoff in first_handoffs.iter_mut().chain(second_handoffs.iter_mut()) {
        handoff.commit();
    }
    first.finish_commit(retry);
    second.finish_commit(retry);
    assert_eq!(
        *lock(&events),
        ["first", "abort", "first", "second", "third"]
    );
}

#[test]
fn successful_handoff_commit_unregisters_its_cancellation_watcher() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family_with_evaluator::<Key, u64, _>(
            "handoff-cancellation-watcher",
            2,
            move |context, _, _| {
                context.register_attempt_handoff(CountingHandoff {
                    commits: Arc::new(AtomicUsize::new(0)),
                    aborts: Arc::new(AtomicUsize::new(0)),
                });
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let cancellation = CancellationToken::new();
    for _ in 0..8 {
        let attempt =
            runtime.request_registered(&family, revision(1), Key("shared"), cancellation.clone());
        assert!(attempt.terminal().is_some());
        assert!(
            lock(&cancellation.inner.watchers).is_empty(),
            "root completion unregisters its lifecycle wait"
        );
    }
}

#[test]
fn empty_committed_handoff_chains_share_one_terminal_lifecycle() {
    let shared = AttemptHandoffs {
        pending: Vec::new(),
        observed: Vec::new(),
    }
    .into_lifecycle();
    assert!(shared.is_committed());
    assert!(shared.observed.is_empty());

    let mut current = shared.clone();
    for _ in 0..10_000 {
        current = AttemptHandoffs {
            pending: Vec::new(),
            observed: vec![current],
        }
        .into_lifecycle();
        assert!(
            Arc::ptr_eq(&current, &shared),
            "an empty committed chain must not retain a lifecycle DAG"
        );
    }

    let committed_child = Arc::new(AttemptHandoffLifecycle::committed());
    let child = Arc::downgrade(&committed_child);
    let collapsed = AttemptHandoffs {
        pending: Vec::new(),
        observed: vec![committed_child],
    }
    .into_lifecycle();
    assert!(Arc::ptr_eq(&collapsed, &shared));
    assert!(
        child.upgrade().is_none(),
        "collapsing a committed child must release its Arc"
    );
}

#[test]
fn committed_handoff_aggregates_collect_without_traversing_their_dag() {
    let runtime = QueryRuntime::new(1);
    let cancellation = CancellationToken::new();
    let owner = TaskId(40);
    let mut level = vec![Arc::new(AttemptHandoffLifecycle::new(
        Vec::new(),
        Vec::new(),
    ))];

    for _ in 0..12 {
        level = (0..2)
            .map(|_| Arc::new(AttemptHandoffLifecycle::new(Vec::new(), level.clone())))
            .collect();
    }
    let root = Arc::new(AttemptHandoffLifecycle::new(Vec::new(), level));
    let AttemptHandoffCommit::Claimed(handoffs) =
        root.begin_commit(owner, &cancellation, &runtime.core)
    else {
        panic!("the raw aggregate starts pending")
    };
    assert!(handoffs.is_empty());
    root.finish_commit(owner);

    let mut observed = Vec::new();
    assert!(AttemptHandoffLifecycle::collect_observed(
        &root,
        &mut observed
    ));
    assert!(
        observed.is_empty(),
        "a committed aggregate is already terminal and contributes no pending DAG"
    );

    let task = Task {
        id: TaskId(43),
        core: runtime.core.clone(),
        revision: revision(1),
        revision_epoch: 0,
        cancellation: CancellationToken::new(),
        owns_permit: AtomicBool::new(false),
        permit_timing: Mutex::new(PermitTiming::default()),
        longest_query_dependency_chain: AtomicU64::new(0),
        publish_critical_path: true,
        stack: Mutex::new(Vec::new()),
        ancestry: Arc::from([]),
        nested_attempts: Mutex::new(Vec::new()),
        nested_attempt_filters: Mutex::new(Vec::new()),
        validation_endorsements: Mutex::new(Vec::new()),
        batch_validation_authority: None,
        validation_proof_parent: None,
        validation_proofs: Mutex::new(ValidationProofStack::new()),
        validation_work: AtomicValidationWork::default(),
        leases: Mutex::new(TaskLeases::default()),
        query_cache: Mutex::new(TaskQueryCache::default()),
        observed_handoffs: Mutex::new(Vec::new()),
        checked_handoffs: Mutex::new(AHashSet::new()),
        handoff_validation_visits: AtomicUsize::new(0),
        validation_endorsement_index_probes: AtomicUsize::new(0),
    };
    task.push(ExactNodeIdentity {
        display: NodeIdentity::new(Arc::from("handoff-test"), Arc::from("root")),
        incarnation: 1,
    });
    assert!(task.observe_handoff(AttemptHandoffLifecycle::shared_committed()));
    assert!(task.observe_handoff(Arc::new(AttemptHandoffLifecycle::committed())));
    assert!(
        lock(&task.stack)[0].observed_handoffs.is_empty(),
        "a computing frame must not retain either form of committed lifecycle"
    );
    lock(&task.stack).clear();
    assert!(task.observe_handoff(AttemptHandoffLifecycle::shared_committed()));
    assert!(
        lock(&task.observed_handoffs).is_empty(),
        "a root must not retain the shared committed lifecycle"
    );
}

#[test]
fn rooted_task_validates_each_shared_handoff_lifecycle_once() {
    let runtime = QueryRuntime::new(1);
    let mut chain = vec![Arc::new(AttemptHandoffLifecycle::new(
        Vec::new(),
        Vec::new(),
    ))];
    for _ in 0..1_024 {
        chain.push(Arc::new(AttemptHandoffLifecycle::new(
            Vec::new(),
            vec![chain.last().unwrap().clone()],
        )));
    }
    let task = Task {
        id: TaskId(44),
        core: runtime.core.clone(),
        revision: revision(1),
        revision_epoch: 0,
        cancellation: CancellationToken::new(),
        owns_permit: AtomicBool::new(false),
        permit_timing: Mutex::new(PermitTiming::default()),
        longest_query_dependency_chain: AtomicU64::new(0),
        publish_critical_path: true,
        stack: Mutex::new(Vec::new()),
        ancestry: Arc::from([]),
        nested_attempts: Mutex::new(Vec::new()),
        nested_attempt_filters: Mutex::new(Vec::new()),
        validation_endorsements: Mutex::new(Vec::new()),
        batch_validation_authority: None,
        validation_proof_parent: None,
        validation_proofs: Mutex::new(ValidationProofStack::new()),
        validation_work: AtomicValidationWork::default(),
        leases: Mutex::new(TaskLeases::default()),
        query_cache: Mutex::new(TaskQueryCache::default()),
        observed_handoffs: Mutex::new(Vec::new()),
        checked_handoffs: Mutex::new(AHashSet::new()),
        handoff_validation_visits: AtomicUsize::new(0),
        validation_endorsement_index_probes: AtomicUsize::new(0),
    };
    task.push(ExactNodeIdentity {
        display: NodeIdentity::new(Arc::from("handoff-cache-test"), Arc::from("root")),
        incarnation: 1,
    });

    assert!(task.observe_handoff(chain.last().unwrap().clone()));
    assert_eq!(
        task.handoff_validation_visits.load(Ordering::Relaxed),
        chain.len()
    );
    for lifecycle in chain.iter().rev() {
        assert!(task.observe_handoff(lifecycle.clone()));
    }
    assert_eq!(
        task.handoff_validation_visits.load(Ordering::Relaxed),
        chain.len(),
        "repeated observations of a shared deep DAG are constant-time cache hits"
    );
}

#[test]
fn handoff_canonicalization_preserves_callbacks_and_commit_abort_behavior() {
    let runtime = QueryRuntime::new(1);
    let cancellation = CancellationToken::new();
    let events = Arc::new(Mutex::new(Vec::new()));

    let committing = AttemptHandoffs {
        pending: vec![Box::new(RecordingHandoff::new(events.clone()))],
        observed: Vec::new(),
    }
    .into_lifecycle();
    assert!(!committing.is_committed());
    let owner = TaskId(41);
    let AttemptHandoffCommit::Claimed(mut handoffs) =
        committing.begin_commit(owner, &cancellation, &runtime.core)
    else {
        panic!("a local callback must remain claimable")
    };
    assert_eq!(handoffs.len(), 1);
    handoffs[0].commit();
    committing.finish_commit(owner);
    assert_eq!(*lock(&events), ["commit"]);

    let aborting = AttemptHandoffs {
        pending: vec![Box::new(RecordingHandoff::new(events.clone()))],
        observed: vec![committing],
    }
    .into_lifecycle();
    assert!(!aborting.is_committed());
    aborting.abort();
    assert_eq!(*lock(&events), ["commit", "abort"]);
}

#[test]
fn handoff_canonicalization_preserves_nonterminal_and_aborted_children() {
    let runtime = QueryRuntime::new(1);
    let cancellation = CancellationToken::new();
    let pending = Arc::new(AttemptHandoffLifecycle::new(Vec::new(), Vec::new()));
    let pending_parent = AttemptHandoffs {
        pending: Vec::new(),
        observed: vec![pending.clone()],
    }
    .into_lifecycle();
    assert!(!pending_parent.is_committed());
    assert_eq!(pending_parent.observed.len(), 1);
    assert!(Arc::ptr_eq(&pending_parent.observed[0], &pending));

    let owner = TaskId(42);
    let AttemptHandoffCommit::Claimed(handoffs) =
        pending.begin_commit(owner, &cancellation, &runtime.core)
    else {
        panic!("a pending child must remain claimable")
    };
    let committing_parent = AttemptHandoffs {
        pending: Vec::new(),
        observed: vec![pending.clone()],
    }
    .into_lifecycle();
    assert!(!committing_parent.is_committed());
    assert!(Arc::ptr_eq(&committing_parent.observed[0], &pending));
    pending.rollback_commit(owner, handoffs);

    pending.abort();
    let aborted_parent = AttemptHandoffs {
        pending: Vec::new(),
        observed: vec![pending.clone()],
    }
    .into_lifecycle();
    assert!(!aborted_parent.is_committed());
    let mut observed = Vec::new();
    assert!(!AttemptHandoffLifecycle::collect_observed(
        &aborted_parent,
        &mut observed
    ));
}

#[test]
fn shared_committed_handoff_lifecycle_is_concurrent_and_idempotent() {
    let shared = AttemptHandoffLifecycle::shared_committed();
    let threads = (0..8)
        .map(|index| {
            let shared = shared.clone();
            thread::spawn(move || {
                let runtime = QueryRuntime::new(1);
                let cancellation = CancellationToken::new();
                for _ in 0..1_000 {
                    let lifecycle = AttemptHandoffs {
                        pending: Vec::new(),
                        observed: vec![shared.clone()],
                    }
                    .into_lifecycle();
                    assert!(Arc::ptr_eq(&lifecycle, &shared));
                    assert!(matches!(
                        lifecycle.begin_commit(TaskId(index + 1), &cancellation, &runtime.core),
                        AttemptHandoffCommit::Committed
                    ));
                    lifecycle.abort();
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    assert!(shared.is_committed());
}

#[test]
fn panicking_commit_uses_transactional_rollback_before_retry() {
    // Rue's unit runner marks any unwind as a failed test, even when the
    // runtime catches it. Exercise the exact rollback helper directly and
    // structurally pin the catch branch that invokes it.
    let runtime = QueryRuntime::new(1);
    let events = Arc::new(Mutex::new(Vec::new()));
    let first = Arc::new(AttemptHandoffLifecycle::new(
        vec![Box::new(RecordingHandoff::new(events.clone()))],
        Vec::new(),
    ));
    let second = Arc::new(AttemptHandoffLifecycle::new(
        vec![Box::new(RecordingHandoff::new(events.clone()))],
        Vec::new(),
    ));
    let first_owner = TaskId(1);
    let cancellation = CancellationToken::new();
    let AttemptHandoffCommit::Claimed(mut first_handoffs) =
        first.begin_commit(first_owner, &cancellation, &runtime.core)
    else {
        panic!("fresh lifecycle is claimable")
    };
    let AttemptHandoffCommit::Claimed(mut second_handoffs) =
        second.begin_commit(first_owner, &cancellation, &runtime.core)
    else {
        panic!("every lifecycle in the root batch is claimable")
    };
    first_handoffs[0].commit();
    second_handoffs[0].commit();
    rollback_handoff_batches(
        first_owner,
        vec![
            (first.clone(), first_handoffs),
            (second.clone(), second_handoffs),
        ],
        vec![1, 1],
    );

    let retry_owner = TaskId(2);
    let AttemptHandoffCommit::Claimed(mut first_handoffs) =
        first.begin_commit(retry_owner, &cancellation, &runtime.core)
    else {
        panic!("rollback returns the lifecycle to pending")
    };
    let AttemptHandoffCommit::Claimed(mut second_handoffs) =
        second.begin_commit(retry_owner, &cancellation, &runtime.core)
    else {
        panic!("aggregate rollback returns every lifecycle to pending")
    };
    first_handoffs[0].commit();
    second_handoffs[0].commit();
    first.finish_commit(retry_owner);
    second.finish_commit(retry_owner);
    assert_eq!(
        *lock(&events),
        ["commit", "commit", "abort", "abort", "commit", "commit"]
    );

    let source = include_str!("task.rs");
    let commit_body = source
        .split("fn commit_handoffs(&self)")
        .nth(1)
        .expect("root task has a handoff commit barrier")
        .split("fn discard_observed_handoffs")
        .next()
        .expect("commit barrier ends before root-abort cleanup");
    assert!(commit_body.contains("commit_handoff(&mut **handoff)"));
    assert!(
        commit_body.contains("rollback_handoff_batches(self.id, batches, attempted_callbacks)")
    );
}

#[test]
fn abort_unwind_marks_the_lifecycle_fail_closed() {
    // The test runner rejects caught unwinds, so model the exact
    // post-catch transition and structurally pin the abort catch site.
    let runtime = QueryRuntime::new(1);
    let lifecycle = Arc::new(AttemptHandoffLifecycle::new(
        vec![Box::new(RecordingHandoff::new(Arc::new(Mutex::new(
            Vec::new(),
        ))))],
        Vec::new(),
    ));
    let owner = TaskId(7);
    let cancellation = CancellationToken::new();
    let AttemptHandoffCommit::Claimed(handoffs) =
        lifecycle.begin_commit(owner, &cancellation, &runtime.core)
    else {
        panic!("fresh lifecycle is claimable")
    };
    lifecycle.abort_failed_commit(owner, handoffs);
    let mut observed = Vec::new();
    assert!(
        !AttemptHandoffLifecycle::collect_observed(&lifecycle, &mut observed),
        "an abort unwind leaves no reusable partial lifecycle"
    );
    assert!(matches!(
        lifecycle.begin_commit(TaskId(8), &cancellation, &runtime.core),
        AttemptHandoffCommit::Aborted
    ));

    let source = include_str!("task.rs");
    let rollback = source
        .split("fn rollback_handoff_batches")
        .nth(1)
        .expect("root rollback helper exists")
        .split("impl AttemptHandoffLifecycle")
        .next()
        .expect("rollback helper has a bounded body");
    assert!(rollback.contains("abort_handoff(&mut **handoff).is_err()"));
    assert!(rollback.contains("lifecycle.abort_failed_commit(owner, handoffs)"));
}

#[test]
fn nested_handoff_eviction_during_root_claim_is_nonterminal() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let child = Arc::new(AttemptHandoffLifecycle::new(
        vec![Box::new(RecordingHandoff::new(events.clone()))],
        Vec::new(),
    ));
    let parent = Arc::new(AttemptHandoffLifecycle::new(
        vec![Box::new(RecordingHandoff::new(events.clone()))],
        vec![child.clone()],
    ));
    let task = Arc::new(Task {
        id: TaskId(99),
        core: runtime.core.clone(),
        revision: revision(1),
        revision_epoch: 0,
        cancellation: CancellationToken::new(),
        owns_permit: AtomicBool::new(false),
        permit_timing: Mutex::new(PermitTiming::default()),
        longest_query_dependency_chain: AtomicU64::new(0),
        publish_critical_path: true,
        stack: Mutex::new(Vec::new()),
        ancestry: Arc::from([]),
        nested_attempts: Mutex::new(Vec::new()),
        nested_attempt_filters: Mutex::new(Vec::new()),
        validation_endorsements: Mutex::new(Vec::new()),
        batch_validation_authority: None,
        validation_proof_parent: None,
        validation_proofs: Mutex::new(ValidationProofStack::new()),
        validation_work: AtomicValidationWork::default(),
        leases: Mutex::new(TaskLeases::default()),
        query_cache: Mutex::new(TaskQueryCache::default()),
        observed_handoffs: Mutex::new(Vec::new()),
        checked_handoffs: Mutex::new(AHashSet::new()),
        handoff_validation_visits: AtomicUsize::new(0),
        validation_endorsement_index_probes: AtomicUsize::new(0),
    });
    assert!(task.observe_handoff(parent.clone()));
    child.abort();
    assert!(matches!(
        task.commit_handoffs(),
        Err(RootHandoffCommitFailure::Invalidated)
    ));
    assert_eq!(*lock(&events), ["abort"]);
    drop(task);
    drop(parent);
    drop(child);
    assert_eq!(*lock(&events), ["abort", "abort"]);
}

#[test]
fn overlay_replaces_one_stamp_recomputing_its_observer_and_reusing_the_rest() {
    let runtime = QueryRuntime::new(1);
    let family = runtime.family::<Key, u64>("overlay", 8).unwrap();
    let base = InputIdentity::new("source", "base.rue");
    let topology = InputIdentity::new("derived", "topology");
    let leaf = InputIdentity::new("source", "leaf.rue");
    let parent = Revision::new(1, 7);
    let successor = Revision::new(2, 7);
    runtime
        .publish_revision(parent, [(base.clone(), 11), (topology.clone(), 1)])
        .unwrap();

    // Queries at the parent: one reads the stable base leaf, one reads the
    // aggregate derived identity that the overlay will re-stamp.
    let base_terminal = runtime
        .query(&family, parent, Key("base"), CancellationToken::new(), {
            let base = base.clone();
            move |context| {
                assert_eq!(context.input(base.clone())?, 11);
                Ok(QueryOutput::success(1))
            }
        })
        .unwrap();
    let topology_terminal = runtime
        .query(
            &family,
            parent,
            Key("topology"),
            CancellationToken::new(),
            {
                let topology = topology.clone();
                move |context| {
                    assert_eq!(context.input(topology.clone())?, 1);
                    Ok(QueryOutput::success(10))
                }
            },
        )
        .unwrap();
    assert_eq!(topology_terminal.outcome(), &QueryOutcome::Success(10));

    // A sparse successor overlay: adds one leaf and RE-STAMPS the aggregate
    // derived identity (same stable identity, new stamp).
    runtime
        .publish_revision_overlay(
            successor,
            parent,
            [(leaf.clone(), 22), (topology.clone(), 2)],
        )
        .unwrap();

    // The base-reading terminal is REUSED across the overlay boundary: its
    // inherited leaf is unchanged, so the compute never re-runs.
    let reused = runtime
        .query(
            &family,
            successor,
            Key("base"),
            CancellationToken::new(),
            |_| panic!("an inherited unchanged leaf must reuse the parent's green terminal"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&base_terminal, &reused));

    // The observer of the re-stamped identity is DIRTIED and recomputes
    // against the overlay's new stamp.
    let recomputed = runtime
        .query(
            &family,
            successor,
            Key("topology"),
            CancellationToken::new(),
            {
                let topology = topology.clone();
                move |context| {
                    assert_eq!(context.input(topology.clone())?, 2);
                    Ok(QueryOutput::success(20))
                }
            },
        )
        .unwrap();
    assert_eq!(recomputed.outcome(), &QueryOutcome::Success(20));

    // The added leaf resolves through the delta.
    let added = runtime
        .query(&family, successor, Key("leaf"), CancellationToken::new(), {
            let leaf = leaf.clone();
            move |context| {
                assert_eq!(context.input(leaf.clone())?, 22);
                Ok(QueryOutput::success(2))
            }
        })
        .unwrap();
    assert_eq!(added.outcome(), &QueryOutcome::Success(2));
}

#[test]
fn overlay_rejects_missing_parent_generation_crossing_and_id_reuse() {
    let runtime = QueryRuntime::new(1);
    let base = InputIdentity::new("source", "base.rue");
    let parent = Revision::new(1, 7);
    runtime
        .publish_revision(parent, [(base.clone(), 11)])
        .unwrap();

    // A parent that is not a retained revision of this lineage is rejected.
    assert!(matches!(
        runtime.publish_revision_overlay(
            Revision::new(3, 7),
            Revision::new(9, 7),
            [(InputIdentity::new("source", "x"), 1)],
        ),
        Err(RevisionError::OverlayParentUnavailable(_))
    ));
    // An overlay never crosses a fresh observation generation: a child whose
    // compatibility token differs from its parent's is rejected.
    assert!(matches!(
        runtime.publish_revision_overlay(
            Revision::new(2, 8),
            parent,
            [(InputIdentity::new("source", "x"), 1)],
        ),
        Err(RevisionError::IncompatibleOverlayParent(_))
    ));
    // The lineage is acyclic and monotonic: a child id at or below its
    // parent's is rejected.
    assert!(matches!(
        runtime.publish_revision_overlay(
            Revision::new(1, 7),
            parent,
            [(InputIdentity::new("source", "x"), 1)],
        ),
        Err(RevisionError::NonMonotonicOverlay(_))
    ));
    // Re-publishing the same overlay view is idempotent.
    let leaf = InputIdentity::new("source", "leaf.rue");
    runtime
        .publish_revision_overlay(Revision::new(4, 7), parent, [(leaf.clone(), 5)])
        .unwrap();
    runtime
        .publish_revision_overlay(Revision::new(4, 7), parent, [(leaf.clone(), 5)])
        .unwrap();
    // A different delta for the same revision id is rejected.
    assert!(matches!(
        runtime.publish_revision_overlay(Revision::new(4, 7), parent, [(leaf.clone(), 6)]),
        Err(RevisionError::AlreadyPublished(_))
    ));
    // One publication supplying two stamps for the same leaf is rejected.
    assert!(matches!(
        runtime.publish_revision_overlay(
            Revision::new(5, 7),
            parent,
            [(leaf.clone(), 5), (leaf.clone(), 6)],
        ),
        Err(RevisionError::ConflictingInput(_))
    ));
}

#[test]
fn overlay_chain_beyond_retention_keeps_newest_child_queryable() {
    let runtime = QueryRuntime::new(1);
    let family = runtime.family::<Slot, u64>("overlay-chain", 4).unwrap();
    let base = InputIdentity::new("source", "base.rue");
    let mut parent = Revision::new(1, 7);
    runtime
        .publish_revision(parent, [(base.clone(), 11)])
        .unwrap();

    // A chain of overlays far beyond the revision-retention limit. Each child
    // owns its parent's input node by Arc, so ancestor revision-store ENTRIES
    // may retire freely (no pinning) while every child's logical view stays
    // complete.
    let depth = REVISION_RETENTION_LIMIT as u64 + 8;
    for step in 0..depth {
        let child = Revision::new(parent.id() + 1, 7);
        runtime
            .publish_revision_overlay(
                child,
                parent,
                [(
                    InputIdentity::new("source", format!("leaf-{step}")),
                    step + 1,
                )],
            )
            .unwrap();
        parent = child;
    }
    // Retention stayed bounded: ancestor entries retired.
    assert!(runtime.metrics().retained_revisions <= REVISION_RETENTION_LIMIT as u64);

    // The newest child still resolves the ORIGINAL inherited leaf (whose
    // publishing entry retired long ago) and its own newest delta leaf.
    let newest = parent;
    let inherited = runtime
        .query(&family, newest, Slot(1), CancellationToken::new(), {
            let base = base.clone();
            move |context| {
                assert_eq!(context.input(base.clone())?, 11);
                Ok(QueryOutput::success(1))
            }
        })
        .unwrap();
    assert_eq!(inherited.outcome(), &QueryOutcome::Success(1));
    let newest_leaf = InputIdentity::new("source", format!("leaf-{}", depth - 1));
    let delta = runtime
        .query(&family, newest, Slot(2), CancellationToken::new(), {
            move |context| {
                assert_eq!(context.input(newest_leaf.clone())?, depth);
                Ok(QueryOutput::success(2))
            }
        })
        .unwrap();
    assert_eq!(delta.outcome(), &QueryOutcome::Success(2));
}

#[test]
fn compatible_exact_key_is_computed_once_and_joined_by_many_waiters() {
    let runtime = QueryRuntime::new(8);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime.family::<Key, u64>("join", 16).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner = thread::spawn(move || {
        owner_runtime.query(
            &owner_family,
            revision(1),
            Key("same"),
            CancellationToken::new(),
            |_| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(QueryOutput::success(42))
            },
        )
    });
    started_rx.recv().unwrap();

    let mut joiners = Vec::new();
    for _ in 0..7 {
        let runtime = runtime.clone();
        let family = family.clone();
        joiners.push(thread::spawn(move || {
            runtime.query(
                &family,
                revision(1),
                Key("same"),
                CancellationToken::new(),
                |_| panic!("a compatible joiner must not compute"),
            )
        }));
    }
    runtime.wait_for_metrics(|metrics| metrics.joins == 7);
    finish_tx.send(()).unwrap();

    let terminal = owner.join().unwrap().unwrap();
    assert_eq!(terminal.outcome(), &QueryOutcome::Success(42));
    for joiner in joiners {
        assert!(Arc::ptr_eq(&terminal, &joiner.join().unwrap().unwrap()));
    }
    assert_eq!(runtime.metrics().claims, 1);
    assert_eq!(runtime.metrics().body_completions, 1);
}

#[test]
fn different_ready_keys_execute_concurrently() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime.family::<Key, u64>("overlap", 4).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles = [Key("a"), Key("b")].map(|key| {
        let runtime = runtime.clone();
        let family = family.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            runtime.query(&family, revision(1), key, CancellationToken::new(), |_| {
                barrier.wait();
                Ok(QueryOutput::success(1))
            })
        })
    });
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    assert_eq!(runtime.metrics().peak_active_bodies, 2);
}

#[test]
fn one_permit_joiner_donates_to_already_claimed_owner() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let outer = runtime.family::<Key, u64>("outer", 4).unwrap();
    let target = runtime.family::<Key, u64>("target", 4).unwrap();
    let (outer_started_tx, outer_started_rx) = mpsc::channel();
    let (enter_join_tx, enter_join_rx) = mpsc::channel();
    let (target_ran_tx, target_ran_rx) = mpsc::channel();

    let outer_runtime = runtime.clone();
    let outer_family = outer.clone();
    let outer_target = target.clone();
    let outer_thread = thread::spawn(move || {
        outer_runtime.query(
            &outer_family,
            revision(1),
            Key("outer"),
            CancellationToken::new(),
            |context| {
                outer_started_tx.send(()).unwrap();
                enter_join_rx.recv().unwrap();
                let dependency = context.query(&outer_target, Key("queued"), |_| {
                    panic!("the previously claimed owner must compute")
                })?;
                assert_eq!(dependency.outcome(), &QueryOutcome::Success(7));
                Ok(QueryOutput::success(8))
            },
        )
    });
    outer_started_rx.recv().unwrap();

    let owner_runtime = runtime.clone();
    let owner_target = target.clone();
    let owner = thread::spawn(move || {
        owner_runtime.query(
            &owner_target,
            revision(1),
            Key("queued"),
            CancellationToken::new(),
            |_| {
                target_ran_tx.send(()).unwrap();
                Ok(QueryOutput::success(7))
            },
        )
    });
    runtime.wait_for_metrics(|metrics| metrics.claims == 2);
    enter_join_tx.send(()).unwrap();
    target_ran_rx.recv().unwrap();
    assert_eq!(
        owner.join().unwrap().unwrap().outcome(),
        &QueryOutcome::Success(7)
    );
    assert_eq!(
        outer_thread.join().unwrap().unwrap().outcome(),
        &QueryOutcome::Success(8)
    );
    assert_eq!(runtime.metrics().donated_permits, 1);
}

/// The single-permit shape of the contention above: the queued root must
/// neither starve behind the permit holder nor be failed for a loop that
/// only exists between two independent roots.
#[test]
fn one_permit_cross_task_contention_neither_starves_nor_fails() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let left = runtime.family::<Key, u64>("one-cycle-left", 4).unwrap();
    let right = runtime.family::<Key, u64>("one-cycle-right", 4).unwrap();
    let (left_started_tx, left_started_rx) = mpsc::channel();
    let (enter_wait_tx, enter_wait_rx) = mpsc::channel();

    let left_runtime = runtime.clone();
    let left_root = left.clone();
    let left_dependency = right.clone();
    let left_task = thread::spawn(move || {
        left_runtime.query(
            &left_root,
            revision(1),
            Key("a"),
            CancellationToken::new(),
            |context| {
                left_started_tx.send(()).unwrap();
                enter_wait_rx.recv().unwrap();
                context.query(&left_dependency, Key("b"), |_| Ok(QueryOutput::success(2)))?;
                Ok(QueryOutput::success(1))
            },
        )
    });
    left_started_rx.recv().unwrap();

    let right_runtime = runtime.clone();
    let right_root = right.clone();
    let right_dependency = left.clone();
    let right_task = thread::spawn(move || {
        right_runtime.query(
            &right_root,
            revision(1),
            Key("b"),
            CancellationToken::new(),
            |context| {
                context.query(&right_dependency, Key("a"), |_| {
                    // Reached only when the left root's attempt is still
                    // in flight and waiting for it would deadlock.
                    Ok(QueryOutput::success(1))
                })?;
                Ok(QueryOutput::success(2))
            },
        )
    });
    runtime.wait_for_metrics(|metrics| metrics.claims == 2);
    enter_wait_tx.send(()).unwrap();

    assert_eq!(
        left_task.join().unwrap().unwrap().outcome(),
        &QueryOutcome::Success(1)
    );
    assert_eq!(
        right_task.join().unwrap().unwrap().outcome(),
        &QueryOutcome::Success(2)
    );
    assert_eq!(runtime.metrics().cycles, 0);
    assert_eq!(runtime.metrics().declined_joins, 1);
    assert_eq!(runtime.metrics().donated_permits, 1);
}

#[test]
fn exact_stack_cycle_is_not_reported_as_starvation() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime.family::<Key, u64>("self-cycle", 4).unwrap();
    let nested = family.clone();
    let result = runtime.query(
        &family,
        revision(1),
        Key("a"),
        CancellationToken::new(),
        move |context| {
            context.query(&nested, Key("a"), |_| Ok(QueryOutput::success(1)))?;
            Ok(QueryOutput::success(2))
        },
    );
    let QueryAbort::Cycle(nodes) = result.unwrap_err() else {
        panic!("expected a true cycle");
    };
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].key(), "a");
}

/// Two independent roots can reach an overlapping set of nodes in opposite
/// orders. Each root's own graph is acyclic — neither body asks for anything
/// that asks back — so the loop exists only in the wait graph, which records
/// who happened to claim which attempt first. Failing a request on that is
/// reporting a scheduling artifact as a program property, and which root
/// loses is decided by thread interleaving. Both must complete instead, with
/// the contended attempt duplicated rather than waited on.
#[test]
fn cross_task_attempt_contention_duplicates_rather_than_failing() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let left = runtime.family::<Key, u64>("cycle-left", 4).unwrap();
    let right = runtime.family::<Key, u64>("cycle-right", 4).unwrap();
    let barrier = Arc::new(Barrier::new(2));

    let left_runtime = runtime.clone();
    let left_root = left.clone();
    let left_dependency = right.clone();
    let left_barrier = barrier.clone();
    let a = thread::spawn(move || {
        left_runtime.query(
            &left_root,
            revision(1),
            Key("a"),
            CancellationToken::new(),
            |context| {
                left_barrier.wait();
                context.query(&left_dependency, Key("b"), |_| Ok(QueryOutput::success(2)))?;
                Ok(QueryOutput::success(1))
            },
        )
    });

    let right_runtime = runtime.clone();
    let right_root = right.clone();
    let right_dependency = left.clone();
    let right_barrier = barrier;
    let b = thread::spawn(move || {
        right_runtime.query(
            &right_root,
            revision(1),
            Key("b"),
            CancellationToken::new(),
            |context| {
                right_barrier.wait();
                context.query(&right_dependency, Key("a"), |_| Ok(QueryOutput::success(1)))?;
                Ok(QueryOutput::success(2))
            },
        )
    });
    assert_eq!(
        a.join().unwrap().unwrap().outcome(),
        &QueryOutcome::Success(1)
    );
    assert_eq!(
        b.join().unwrap().unwrap().outcome(),
        &QueryOutcome::Success(2)
    );
    assert_eq!(
        runtime.metrics().cycles,
        0,
        "no body asked for a node that asks back, so no cycle exists to report"
    );
}

/// A dependency cycle is a property of the request's structure, so it must
/// still be reported exactly — including through a registered batch, whose
/// children evaluate on their own tasks with their own stacks. Declining a
/// deadlocking join only moves the work; the enclosing chain is what makes
/// the recursion terminate on the node that actually repeats.
#[test]
fn registered_batch_dependency_cycle_is_reported_exactly() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let ring_slot = Arc::new(std::sync::OnceLock::<QueryFamily<Key, u64>>::new());
    let ring_slot_for_evaluator = ring_slot.clone();
    let ring = runtime
        .family_with_evaluator::<Key, u64, _>("ring", 8, move |context, _, key: &Key| {
            let next = if key.0 == "a" { Key("b") } else { Key("a") };
            context.query_registered_batch(
                ring_slot_for_evaluator.get().expect("ring is installed"),
                [next],
            )?;
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    ring_slot.set(ring.clone()).unwrap();

    let attempt =
        runtime.request_registered(&ring, revision(1), Key("a"), CancellationToken::new());
    let Some(QueryAbort::Cycle(nodes)) = attempt.abort() else {
        panic!("a body which transitively asks for itself is a true cycle");
    };
    assert_eq!(nodes.len(), 2);
    // The rendered cycle names every memo node on it, in canonical order.
    // Any future change to when a memo-node key is formatted has to keep
    // this text exactly, because consumers match cycle members by name.
    assert_eq!(
        nodes
            .iter()
            .map(|node| (node.family(), node.key()))
            .collect::<Vec<_>>(),
        vec![("ring", "a"), ("ring", "b")]
    );
    assert_eq!(runtime.metrics().cycles, 1);
}

#[test]
fn incompatible_revisions_do_not_join_and_can_overlap() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1), revision(2)]);
    let family = runtime.family::<Key, u64>("revisions", 4).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles = [revision(1), revision(2)].map(|revision| {
        let runtime = runtime.clone();
        let family = family.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            runtime.query(
                &family,
                revision,
                Key("same"),
                CancellationToken::new(),
                |_| {
                    barrier.wait();
                    Ok(QueryOutput::success(revision.id()))
                },
            )
        })
    });
    let values = handles.map(|handle| handle.join().unwrap().unwrap());
    assert_eq!(values[0].revision(), revision(1));
    assert_eq!(values[1].revision(), revision(2));
    assert_eq!(runtime.metrics().claims, 2);
    assert_eq!(runtime.metrics().joins, 0);
    assert_eq!(runtime.metrics().peak_active_bodies, 2);
}

#[test]
fn red_publication_preserves_stamp_across_revision_and_position_changes() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1), revision(2), revision(3)]);
    let leaf = runtime.family::<Key, u64>("red-leaf", 8).unwrap();
    let parent = runtime.family::<Key, u64>("red-parent", 8).unwrap();

    let run = |revision, value, offset| {
        let leaf = leaf.clone();
        runtime.query(
            &parent,
            revision,
            Key("parent"),
            CancellationToken::new(),
            move |context| {
                let _child = context.query(&leaf, Key("leaf"), |_| {
                    Ok(
                        QueryOutput::success(value).with_diagnostics(vec![QueryDiagnostic::new(
                            "leaf-warning",
                            "same semantic payload",
                            Some(PresentationPosition::new("main.rue", offset)),
                        )]),
                    )
                })?;
                Ok(QueryOutput::success(10).with_work(vec![WorkItem::new("visited", 1)]))
            },
        )
    };

    let first = run(revision(1), 5, 1).unwrap();
    let second = run(revision(2), 5, 99).unwrap();
    assert_eq!(first.stamp(), second.stamp());
    assert_eq!(first.dependencies(), second.dependencies());
    let leaf_second = runtime
        .query(
            &leaf,
            revision(2),
            Key("leaf"),
            CancellationToken::new(),
            |_| panic!("compatible terminal is retained"),
        )
        .unwrap();
    assert_eq!(
        leaf_second.diagnostics()[0]
            .presentation
            .as_ref()
            .unwrap()
            .offset,
        99
    );

    let third = run(revision(3), 6, 100).unwrap();
    // The green child forces the parent body to run, but equal parent
    // semantics retain the parent's red stamp.
    assert_eq!(second.stamp(), third.stamp());
    assert_ne!(second.dependencies(), third.dependencies());
    assert_eq!(runtime.metrics().claims, 6);
    assert!(runtime.metrics().red_publications >= 2);
}

#[test]
fn canceling_a_waiter_does_not_cancel_shared_work() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime.family::<Key, u64>("waiter-cancel", 4).unwrap();
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
                Ok(QueryOutput::success(1))
            },
        )
    });
    started_rx.recv().unwrap();
    let waiter_token = CancellationToken::new();
    let (at_park_tx, at_park_rx) = mpsc::channel();
    let hook_token = waiter_token.clone();
    runtime.set_interpose(Arc::new(move |site| {
        if site == InterposeSite::NodeJoinPark {
            at_park_tx.send(()).unwrap();
            while !hook_token.is_canceled() {
                thread::yield_now();
            }
        }
    }));
    let waiter_runtime = runtime.clone();
    let waiter_family = family.clone();
    let waiter_cancel = waiter_token.clone();
    let watcher_token = waiter_token.clone();
    let (waiter_done_tx, waiter_done_rx) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let result = waiter_runtime.query(
            &waiter_family,
            revision(1),
            Key("shared"),
            waiter_token,
            |_| panic!("waiter cannot become owner while shared work is live"),
        );
        waiter_done_tx.send(()).unwrap();
        result
    });
    let canceler = thread::spawn(move || {
        at_park_rx.recv().unwrap();
        // The joiner owns the predicate lock and has observed a computing
        // attempt. Cancellation becomes true before that lock is released.
        waiter_cancel.cancel();
    });
    let waiter_woke = waiter_done_rx.recv_timeout(Duration::from_secs(1));
    if waiter_woke.is_err() {
        finish_tx.send(()).unwrap();
        let _ = owner.join().unwrap();
        let _ = waiter.join().unwrap();
        canceler.join().unwrap();
        runtime.clear_interpose();
        panic!("cancellation did not wake a parked node join");
    }
    assert_eq!(waiter.join().unwrap().unwrap_err(), QueryAbort::Canceled);
    canceler.join().unwrap();
    runtime.clear_interpose();
    assert!(
        lock(&watcher_token.inner.watchers).is_empty(),
        "the canceled node join unregisters its watcher"
    );
    finish_tx.send(()).unwrap();
    assert_eq!(
        owner.join().unwrap().unwrap().outcome(),
        &QueryOutcome::Success(1)
    );
}

#[test]
fn canceled_owner_does_not_publish_and_live_waiter_reclaims() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime.family::<Key, u64>("owner-cancel", 4).unwrap();
    let owner_token = CancellationToken::new();
    let owner_cancel = owner_token.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let owner_runtime = runtime.clone();
    let owner_family = family.clone();
    let owner = thread::spawn(move || {
        owner_runtime.query(
            &owner_family,
            revision(1),
            Key("shared"),
            owner_token,
            |_| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(QueryOutput::success(1))
            },
        )
    });
    started_rx.recv().unwrap();

    let waiter_runtime = runtime.clone();
    let waiter_family = family.clone();
    let waiter = thread::spawn(move || {
        waiter_runtime.query(
            &waiter_family,
            revision(1),
            Key("shared"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(2)),
        )
    });
    runtime.wait_for_metrics(|metrics| metrics.joins == 1);
    owner_cancel.cancel();
    finish_tx.send(()).unwrap();
    assert_eq!(owner.join().unwrap().unwrap_err(), QueryAbort::Canceled);
    assert_eq!(
        waiter.join().unwrap().unwrap().outcome(),
        &QueryOutcome::Success(2)
    );
    assert_eq!(runtime.metrics().green_publications, 1);
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
fn adaptive_registered_batch_stays_inline_when_parallelism_is_impossible() {
    fn run(workers: usize) -> (Arc<QueryTerminal<u64>>, RuntimeMetrics) {
        let runtime = QueryRuntime::new(workers);
        publish_empty(&runtime, [revision(1)]);
        let leaf = runtime
            .family_with_evaluator::<Key, u64, _>("adaptive-batch-leaf", 8, |_, _, key| {
                Ok(QueryOutput::success(key.0.len() as u64))
            })
            .unwrap();
        let leaf_for_root = leaf.clone();
        let root = runtime
            .family_with_evaluator::<Key, u64, _>("adaptive-batch-root", 8, move |context, _, _| {
                let keys = [Key("aa"), Key("bbb")];
                let terminals =
                    context.query_registered_adaptive_batch_refs(&leaf_for_root, keys.iter())?;
                let sum = terminals
                    .iter()
                    .map(|terminal| match terminal.outcome() {
                        QueryOutcome::Success(value) => *value,
                        QueryOutcome::Failure(_) => unreachable!("leaf cannot fail"),
                    })
                    .sum();
                Ok(QueryOutput::success(sum))
            })
            .unwrap();
        let terminal = runtime
            .request_registered(&root, revision(1), Key("root"), CancellationToken::new())
            .into_result()
            .unwrap();
        let metrics = runtime.metrics();
        (terminal, metrics)
    }

    let (serial, serial_metrics) = run(1);
    let (parallel, parallel_metrics) = run(2);
    assert_eq!(serial.outcome(), &QueryOutcome::Success(5));
    assert_eq!(serial.outcome(), parallel.outcome());
    let dependency_keys = |terminal: &QueryTerminal<u64>| {
        terminal
            .dependencies()
            .iter()
            .map(|dependency| dependency.node.key().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(dependency_keys(&serial), dependency_keys(&parallel));
    assert_eq!(serial_metrics.donated_permits, 0);
    assert_eq!(serial_metrics.ready_items, 0);
    assert_eq!(parallel_metrics.donated_permits, 1);
    assert_eq!(parallel_metrics.ready_items, 2);
}

#[test]
fn adaptive_registered_batch_stays_inline_when_nested_capacity_is_saturated() {
    fn run(workers: usize) -> (Arc<QueryTerminal<u64>>, RuntimeMetrics) {
        let runtime = QueryRuntime::new(workers);
        publish_empty(&runtime, [revision(1)]);
        let leaves = runtime
            .family_with_evaluator::<Key, u64, _>("adaptive-saturated-leaf", 8, |_, _, key| {
                Ok(QueryOutput::success(key.0.len() as u64))
            })
            .unwrap();
        let leaves_for_middle = leaves.clone();
        let middle = runtime
            .family_with_evaluator::<Key, u64, _>(
                "adaptive-saturated-middle",
                8,
                move |context, _, key| {
                    let leaf_keys = match key.0 {
                        "m0" => [Key("m0-aa"), Key("m0-bbb")],
                        "m1" => [Key("m1-aa"), Key("m1-bbb")],
                        "m2" => [Key("m2-aa"), Key("m2-bbb")],
                        "m3" => [Key("m3-aa"), Key("m3-bbb")],
                        _ => unreachable!("test root only requests known middle keys"),
                    };
                    let terminals =
                        context.query_registered_adaptive_batch(&leaves_for_middle, leaf_keys)?;
                    let sum = terminals
                        .iter()
                        .map(|terminal| match terminal.outcome() {
                            QueryOutcome::Success(value) => *value,
                            QueryOutcome::Failure(_) => unreachable!("leaf cannot fail"),
                        })
                        .sum();
                    Ok(QueryOutput::success(sum))
                },
            )
            .unwrap();
        let middle_for_root = middle.clone();
        let root = runtime
            .family_with_evaluator::<Key, u64, _>(
                "adaptive-saturated-root",
                8,
                move |context, _, _| {
                    let terminals = context.query_registered_adaptive_batch(
                        &middle_for_root,
                        [Key("m0"), Key("m1"), Key("m2"), Key("m3")],
                    )?;
                    let sum = terminals
                        .iter()
                        .map(|terminal| match terminal.outcome() {
                            QueryOutcome::Success(value) => *value,
                            QueryOutcome::Failure(_) => unreachable!("middle cannot fail"),
                        })
                        .sum();
                    Ok(QueryOutput::success(sum))
                },
            )
            .unwrap();
        let terminal = runtime
            .request_registered(&root, revision(1), Key("root"), CancellationToken::new())
            .into_result()
            .unwrap();
        let metrics = runtime.metrics();
        (terminal, metrics)
    }

    let (serial, serial_metrics) = run(1);
    let (saturated, saturated_metrics) = run(4);
    assert_eq!(serial.outcome(), &QueryOutcome::Success(44));
    assert_eq!(saturated.outcome(), serial.outcome());
    let stable_dependencies = |terminal: &QueryTerminal<u64>| {
        terminal
            .dependencies()
            .iter()
            .map(|dependency| {
                (
                    dependency.node.family().to_owned(),
                    dependency.node.key().to_owned(),
                    dependency.stamp,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        stable_dependencies(&saturated),
        stable_dependencies(&serial)
    );
    assert_eq!(saturated.work(), serial.work());
    // The outer four-item batch consumes all three nested worker slots.
    // Its four children are therefore the only structured items; each
    // child's two leaf requests stay ordered in that child task.
    assert_eq!(saturated_metrics.donated_permits, 1);
    assert_eq!(saturated_metrics.ready_items, 4);
    assert_eq!(serial_metrics.donated_permits, 0);
    assert_eq!(serial_metrics.ready_items, 0);
}

#[test]
fn adaptive_registered_batch_rejects_an_empty_foreign_family() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let foreign_runtime = QueryRuntime::new(1);
    publish_empty(&foreign_runtime, [revision(1)]);
    let foreign = foreign_runtime
        .family_with_evaluator::<Key, u64, _>("adaptive-foreign", 2, |_, _, _| {
            Ok(QueryOutput::success(0))
        })
        .unwrap();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("adaptive-foreign-root", 2, move |context, _, _| {
            context.query_registered_adaptive_batch(&foreign, std::iter::empty::<Key>())?;
            Ok(QueryOutput::success(0))
        })
        .unwrap();

    let attempt =
        runtime.request_registered(&root, revision(1), Key("root"), CancellationToken::new());
    assert_eq!(attempt.abort(), Some(&QueryAbort::ForeignRuntime));
}

#[test]
fn task_dependencies_deduplicate_by_incarnation_and_publish_in_stable_order() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let dependency = runtime
        .family::<Key, u64>("dependency-order-leaf", 8)
        .unwrap();
    let root = runtime
        .family::<Key, u64>("dependency-order-root", 8)
        .unwrap();
    let dependency_for_root = dependency.clone();
    let rooted = runtime
        .query(
            &root,
            revision(1),
            Key("root"),
            CancellationToken::new(),
            move |context| {
                context.query(&dependency_for_root, Key("z"), |_| {
                    Ok(QueryOutput::success(1))
                })?;
                context.query(&dependency_for_root, Key("z"), |_| {
                    panic!("the inline duplicate reuses its terminal")
                })?;
                context.query(&dependency_for_root, Key("a"), |_| {
                    Ok(QueryOutput::success(2))
                })?;
                context.query(&dependency_for_root, Key("z"), |_| {
                    panic!("the hashed duplicate reuses its terminal")
                })?;
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();

    assert_eq!(
        rooted
            .dependencies()
            .iter()
            .map(|dependency| dependency.node.key())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
}

#[test]
fn task_inputs_inline_one_then_promote_in_stable_order() {
    let mut inputs = InlineOrderedMap::default();
    let later = InputIdentity::new("source", "z.rue");
    let earlier = InputIdentity::new("source", "a.rue");

    let assert_same_stamp = |previous: &mut u64, current| assert_eq!(*previous, current);
    inputs.insert_with(later.clone(), 7, assert_same_stamp);
    inputs.insert_with(later, 7, assert_same_stamp);
    assert!(matches!(inputs, InlineOrderedMap::One(_, 7)));

    inputs.insert_with(earlier.clone(), 3, assert_same_stamp);
    inputs.insert_with(earlier, 3, assert_same_stamp);
    assert_eq!(
        inputs.into_entries(),
        [
            (InputIdentity::new("source", "a.rue"), 3),
            (InputIdentity::new("source", "z.rue"), 7),
        ]
    );
}

#[test]
fn task_work_inline_one_then_promote_and_aggregate_in_stable_order() {
    let mut work = InlineOrderedMap::default();
    let add_amount = |previous: &mut u64, current| *previous += current;

    work.insert_with(Arc::from("visited"), 2, add_amount);
    work.insert_with(Arc::from("visited"), 3, add_amount);
    assert!(matches!(work, InlineOrderedMap::One(_, 5)));

    work.insert_with(Arc::from("lowered"), 7, add_amount);
    work.insert_with(Arc::from("visited"), 11, add_amount);
    assert_eq!(
        work.into_entries(),
        [
            (Arc::<str>::from("lowered"), 7),
            (Arc::<str>::from("visited"), 16),
        ]
    );
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
fn cross_revision_reuse_validates_every_exact_input_leaf() {
    let runtime = QueryRuntime::new(1);
    let family = runtime.family::<Key, u64>("validated", 4).unwrap();
    let input = InputIdentity::new("source", "main.rue");
    let first_revision = Revision::new(1, 7);
    let equivalent_revision = Revision::new(2, 7);
    let changed_revision = Revision::new(3, 7);
    let missing_revision = Revision::new(4, 7);
    runtime
        .publish_revision(first_revision, [(input.clone(), 11)])
        .unwrap();
    runtime
        .publish_revision(equivalent_revision, [(input.clone(), 11)])
        .unwrap();
    runtime
        .publish_revision(changed_revision, [(input.clone(), 12)])
        .unwrap();
    runtime.publish_revision(missing_revision, []).unwrap();

    let first = runtime
        .query(
            &family,
            first_revision,
            Key("same"),
            CancellationToken::new(),
            |context| {
                assert_eq!(context.input(input.clone())?, 11);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let reused = runtime
        .query(
            &family,
            equivalent_revision,
            Key("same"),
            CancellationToken::new(),
            |_| panic!("equal compatibility is insufficient; exact leaves prove reuse"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&first, &reused));

    let recomputed = runtime
        .query(
            &family,
            changed_revision,
            Key("same"),
            CancellationToken::new(),
            |context| {
                assert_eq!(context.input(input.clone())?, 12);
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();
    assert_eq!(recomputed.outcome(), &QueryOutcome::Success(2));
    let missing = runtime.request(
        &family,
        missing_revision,
        Key("same"),
        CancellationToken::new(),
        |context| {
            context.input(input.clone())?;
            unreachable!("missing exact input fails closed")
        },
    );
    assert_eq!(
        missing.abort(),
        Some(&QueryAbort::MissingInput(input.clone()))
    );
    assert_eq!(runtime.metrics().claims, 3);
    assert_eq!(runtime.metrics().reuses, 1);
}

#[test]
fn revision_scoped_validation_memo_visits_a_diamond_node_once() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(20, 9);
    let second = Revision::new(21, 9);
    let input = InputIdentity::new("source", "diamond");
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 1)])
        .unwrap();

    let leaf_input = input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("validation-diamond-leaf", 8, move |context, _, _| {
            context.input(leaf_input.clone())?;
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let leaf_for_left = leaf.clone();
    let left = runtime
        .family_with_evaluator::<Key, u64, _>("validation-diamond-left", 8, move |context, _, _| {
            context.query_registered(&leaf_for_left, Key("leaf"))?;
            Ok(QueryOutput::success(2))
        })
        .unwrap();
    let leaf_for_right = leaf.clone();
    let right = runtime
        .family_with_evaluator::<Key, u64, _>(
            "validation-diamond-right",
            8,
            move |context, _, _| {
                context.query_registered(&leaf_for_right, Key("leaf"))?;
                Ok(QueryOutput::success(3))
            },
        )
        .unwrap();
    let left_for_root = left.clone();
    let right_for_root = right.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("validation-diamond-root", 8, move |context, _, _| {
            context.query_registered(&left_for_root, Key("left"))?;
            context.query_registered(&right_for_root, Key("right"))?;
            Ok(QueryOutput::success(4))
        })
        .unwrap();

    assert_eq!(
        runtime
            .request_registered(&root, first, Key("root"), CancellationToken::new())
            .execution(),
        RequestExecution::Computed
    );
    let before = runtime.metrics();
    let reused = runtime.request_registered(&root, second, Key("root"), CancellationToken::new());
    assert_eq!(reused.execution(), RequestExecution::Reused);
    let after = runtime.metrics();
    let validation = after.validation.saturating_sub(before.validation);
    assert_validation_work_consistent(validation);
    assert_eq!(validation.memo_misses, 3);
    assert_eq!(validation.certificate_misses, 3);
    assert_eq!(validation.proof_reacquisition_misses, 0);
    assert_eq!(
        validation.memo_hits, 1,
        "the second edge into the shared leaf uses its revision memo"
    );
    assert_eq!(
        reused
            .nested_attempts()
            .iter()
            .filter(|attempt| attempt.node().family() == "validation-diamond-leaf")
            .count(),
        1,
        "the shared leaf is re-demanded only on its first path"
    );
}

/// One diamond over one present leaf plus the certificate-relevant
/// revisions for the ADR-0073 gate tests below.
fn certificate_epoch_fixture(
    runtime: &QueryRuntime,
) -> (
    QueryFamily<Key, u64>,
    QueryFamily<Key, u64>,
    QueryFamily<Key, u64>,
    QueryFamily<Key, u64>,
) {
    let input = InputIdentity::new("source", "epoch-diamond");
    let leaf_input = input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("epoch-leaf", 8, move |context, _, _| {
            context.input(leaf_input.clone())?;
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let leaf_for_left = leaf.clone();
    let left = runtime
        .family_with_evaluator::<Key, u64, _>("epoch-left", 8, move |context, _, _| {
            context.query_registered(&leaf_for_left, Key("leaf"))?;
            Ok(QueryOutput::success(2))
        })
        .unwrap();
    let leaf_for_right = leaf.clone();
    let right = runtime
        .family_with_evaluator::<Key, u64, _>("epoch-right", 8, move |context, _, _| {
            context.query_registered(&leaf_for_right, Key("leaf"))?;
            Ok(QueryOutput::success(3))
        })
        .unwrap();
    let left_for_root = left.clone();
    let right_for_root = right.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("epoch-root", 8, move |context, _, _| {
            context.query_registered(&left_for_root, Key("left"))?;
            context.query_registered(&right_for_root, Key("right"))?;
            Ok(QueryOutput::success(4))
        })
        .unwrap();
    (leaf, left, right, root)
}

#[test]
fn additive_overlay_carries_certificates_without_a_cone_walk() {
    let runtime = QueryRuntime::new(1);
    let base = Revision::new(30, 11);
    let extended = Revision::new(31, 11);
    let input = InputIdentity::new("source", "epoch-diamond");
    runtime.publish_revision(base, [(input, 1)]).unwrap();
    runtime
        .publish_revision_overlay(
            extended,
            base,
            [(InputIdentity::new("source", "appended"), 7)],
        )
        .unwrap();
    let (_leaf, _left, _right, root) = certificate_epoch_fixture(&runtime);
    assert_eq!(
        runtime
            .request_registered(&root, base, Key("root"), CancellationToken::new())
            .execution(),
        RequestExecution::Computed
    );
    let before = runtime.metrics();
    let reused = runtime.request_registered(&root, extended, Key("root"), CancellationToken::new());
    assert_eq!(reused.execution(), RequestExecution::Reused);
    let validation = runtime
        .metrics()
        .validation
        .saturating_sub(before.validation);
    assert_validation_work_consistent(validation);
    assert_eq!(
        validation.certificate_misses, 0,
        "a strictly additive head extension preserves the epoch, so every \
             base-revision certificate is accepted forward without a re-walk"
    );
    assert_eq!(
        validation.demands, 0,
        "no dependency is re-demanded when its certificate carries"
    );
}

#[test]
fn missing_leaf_observation_blocks_certificate_carry() {
    let runtime = QueryRuntime::new(1);
    let base = Revision::new(40, 12);
    let still_absent = Revision::new(41, 12);
    let satisfied = Revision::new(42, 12);
    let probed = InputIdentity::new("candidate", "maybe.rue");
    runtime
        .publish_revision(base, [(InputIdentity::new("source", "present"), 1)])
        .unwrap();
    runtime
        .publish_revision_overlay(
            still_absent,
            base,
            [(InputIdentity::new("source", "unrelated"), 2)],
        )
        .unwrap();
    runtime
        .publish_revision_overlay(satisfied, still_absent, [(probed.clone(), 9)])
        .unwrap();
    let probe_input = probed.clone();
    let prober = runtime
        .family_with_evaluator::<Key, u64, _>("epoch-prober", 8, move |context, _, _| {
            Ok(QueryOutput::success(
                context.optional_input(probe_input.clone()).unwrap_or(0),
            ))
        })
        .unwrap();
    let prober_for_parent = prober.clone();
    let parent_over_prober = runtime
        .family_with_evaluator::<Key, u64, _>("epoch-prober-parent", 8, move |context, _, _| {
            let attempt = context.query_registered(&prober_for_parent, Key("probe"))?;
            let QueryOutcome::Success(value) = attempt.outcome() else {
                panic!("prober publishes typed values");
            };
            Ok(QueryOutput::success(*value))
        })
        .unwrap();
    assert_eq!(
        runtime
            .request_registered(
                &parent_over_prober,
                base,
                Key("parent"),
                CancellationToken::new(),
            )
            .execution(),
        RequestExecution::Computed
    );

    // The appended unrelated leaf preserves the epoch, but the prober's
    // cone observed a missing leaf, so its certificate must not carry:
    // the parent's dependency validation re-demands the prober and
    // re-proves the absence exactly instead of riding the gate.
    let before = runtime.metrics();
    let revalidated = runtime.request_registered(
        &parent_over_prober,
        still_absent,
        Key("parent"),
        CancellationToken::new(),
    );
    assert_eq!(revalidated.execution(), RequestExecution::Reused);
    let validation = runtime
        .metrics()
        .validation
        .saturating_sub(before.validation);
    assert_validation_work_consistent(validation);
    assert!(
        validation.certificate_misses >= 1,
        "a missing-leaf cone never rides the cross-revision certificate gate"
    );
    assert!(
        validation.demands >= 1,
        "the impure dependency is re-demanded rather than carried"
    );

    // Adding the probed leaf changes what the cone observed: recompute.
    let recomputed = runtime.request_registered(
        &parent_over_prober,
        satisfied,
        Key("parent"),
        CancellationToken::new(),
    );
    assert_eq!(recomputed.execution(), RequestExecution::Computed);
    let terminal = recomputed.terminal().unwrap();
    let QueryOutcome::Success(value) = terminal.outcome() else {
        panic!("parent republishes the prober's value");
    };
    assert_eq!(*value, 9, "the satisfied probe observes the added leaf");
}

#[test]
fn sibling_overlay_children_never_share_certificates() {
    let runtime = QueryRuntime::new(1);
    let parent = Revision::new(50, 13);
    let first_child = Revision::new(51, 13);
    let second_child = Revision::new(52, 13);
    let input = InputIdentity::new("source", "epoch-diamond");
    runtime.publish_revision(parent, [(input, 1)]).unwrap();
    runtime
        .publish_revision_overlay(
            first_child,
            parent,
            [(InputIdentity::new("source", "first"), 5)],
        )
        .unwrap();
    // The parent is no longer the extension head, so the second child of
    // the same parent starts a fresh epoch even though its delta is
    // strictly additive.
    runtime
        .publish_revision_overlay(
            second_child,
            parent,
            [(InputIdentity::new("source", "second"), 6)],
        )
        .unwrap();
    let (_leaf, _left, _right, root) = certificate_epoch_fixture(&runtime);
    assert_eq!(
        runtime
            .request_registered(&root, first_child, Key("root"), CancellationToken::new())
            .execution(),
        RequestExecution::Computed
    );
    let before = runtime.metrics();
    let sibling =
        runtime.request_registered(&root, second_child, Key("root"), CancellationToken::new());
    assert_eq!(sibling.execution(), RequestExecution::Reused);
    let validation = runtime
        .metrics()
        .validation
        .saturating_sub(before.validation);
    assert_validation_work_consistent(validation);
    assert!(
        validation.certificate_misses >= 1,
        "a certificate minted under one sibling child must not validate \
             the other: the siblings never share an epoch"
    );
}

#[test]
fn certificates_are_directional_across_the_epoch_chain() {
    let runtime = QueryRuntime::new(1);
    let base = Revision::new(60, 14);
    let extended = Revision::new(61, 14);
    let input = InputIdentity::new("source", "epoch-diamond");
    runtime.publish_revision(base, [(input, 1)]).unwrap();
    runtime
        .publish_revision_overlay(
            extended,
            base,
            [(InputIdentity::new("source", "appended"), 7)],
        )
        .unwrap();
    let (_leaf, _left, _right, root) = certificate_epoch_fixture(&runtime);

    // First demand under the NEWER revision: certificates are minted at
    // the extension, so an older pinned request must reject them
    // (a newer certificate can assert nothing about inputs the pin does
    // not contain) and re-validate.
    assert_eq!(
        runtime
            .request_registered(&root, extended, Key("root"), CancellationToken::new())
            .execution(),
        RequestExecution::Computed
    );
    let before = runtime.metrics();
    let pinned = runtime.request_registered(&root, base, Key("root"), CancellationToken::new());
    assert_eq!(pinned.execution(), RequestExecution::Reused);
    let validation = runtime
        .metrics()
        .validation
        .saturating_sub(before.validation);
    assert_validation_work_consistent(validation);
    assert!(
        validation.certificate_misses >= 1,
        "a newer-revision certificate is rejected at an older pin"
    );

    // The old-pin validation overwrote the slot with base-revision
    // certificates; the newer revision then accepts them FORWARD along
    // the same epoch without any re-walk.
    let before = runtime.metrics();
    let forward =
        runtime.request_registered(&root, extended, Key("root"), CancellationToken::new());
    assert_eq!(forward.execution(), RequestExecution::Reused);
    let validation = runtime
        .metrics()
        .validation
        .saturating_sub(before.validation);
    assert_validation_work_consistent(validation);
    assert_eq!(
        validation.certificate_misses, 0,
        "an older same-epoch certificate is accepted forward"
    );
    assert_eq!(validation.demands, 0);
}

#[test]
fn purity_transition_with_an_equal_value_changes_the_stamp() {
    // The four-step soundness sequence from the ADR-0073 review: a child
    // whose value stays identical while its cone turns impure must mint a
    // NEW stamp, so parents recompute and inherit the impurity instead of
    // staying certified with stale safety metadata that a later additive
    // revision would wrongly carry past a satisfied absence.
    let runtime = QueryRuntime::new(1);
    let pure_mode = Revision::new(70, 15);
    let probing_mode = Revision::new(71, 15);
    let satisfied = Revision::new(72, 15);
    let mode = InputIdentity::new("config", "mode");
    let probed = InputIdentity::new("candidate", "late.rue");
    runtime
        .publish_revision(pure_mode, [(mode.clone(), 1)])
        .unwrap();
    // An ordinary edit: a fresh full view with the changed mode leaf.
    runtime
        .publish_revision(probing_mode, [(mode.clone(), 2)])
        .unwrap();
    runtime
        .publish_revision_overlay(satisfied, probing_mode, [(probed.clone(), 9)])
        .unwrap();

    let child_mode = mode.clone();
    let child_probe = probed.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>("purity-child", 8, move |context, _, _| {
            let mode = context.input(child_mode.clone())?;
            if mode == 1 {
                // Pure cone, value 0.
                Ok(QueryOutput::success(0))
            } else {
                // Impure cone, SAME value 0 while the probe is absent.
                Ok(QueryOutput::success(
                    context.optional_input(child_probe.clone()).unwrap_or(0),
                ))
            }
        })
        .unwrap();
    let child_for_parent = child.clone();
    let parent = runtime
        .family_with_evaluator::<Key, u64, _>("purity-parent", 8, move |context, _, _| {
            let attempt = context.query_registered(&child_for_parent, Key("child"))?;
            let QueryOutcome::Success(value) = attempt.outcome() else {
                panic!("the child publishes typed values");
            };
            Ok(QueryOutput::success(*value))
        })
        .unwrap();

    // Step 1: pure cone, value 0.
    let first =
        runtime.request_registered(&parent, pure_mode, Key("parent"), CancellationToken::new());
    assert_eq!(first.execution(), RequestExecution::Computed);
    let child_pure =
        runtime.request_registered(&child, pure_mode, Key("child"), CancellationToken::new());
    let child_pure_stamp = child_pure.terminal().unwrap().stamp();

    // Step 2: the edit flips the child's cone impure with an EQUAL value.
    // The purity transition is part of red/green identity, so the child
    // mints a new stamp and the parent recomputes, inheriting the bit.
    let edited = runtime.request_registered(
        &parent,
        probing_mode,
        Key("parent"),
        CancellationToken::new(),
    );
    assert_eq!(edited.execution(), RequestExecution::Computed);
    let child_probing =
        runtime.request_registered(&child, probing_mode, Key("child"), CancellationToken::new());
    assert_ne!(
        child_probing.terminal().unwrap().stamp(),
        child_pure_stamp,
        "an equal-value purity transition must not reuse the pure stamp"
    );

    // Steps 3-4: the additive revision satisfies the probed absence. The
    // parent must recompute to the newly present value; a carried
    // certificate returning the stale 0 is exactly the unsound outcome.
    let final_result =
        runtime.request_registered(&parent, satisfied, Key("parent"), CancellationToken::new());
    assert_eq!(final_result.execution(), RequestExecution::Computed);
    let terminal = final_result.terminal().unwrap();
    let QueryOutcome::Success(value) = terminal.outcome() else {
        panic!("the parent republishes the child's value");
    };
    assert_eq!(
        *value, 9,
        "the satisfied absence must reach the parent through recomputation"
    );
}

#[test]
fn removing_a_terminal_invalidates_only_its_certificate() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(25, 9);
    let second = Revision::new(26, 9);
    let input = InputIdentity::new("source", "certificate-retention");
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 2)])
        .unwrap();

    let evaluator_input = input.clone();
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("certificate-retention", 8, move |context, _, _| {
            Ok(QueryOutput::success(
                context.input(evaluator_input.clone())?,
            ))
        })
        .unwrap();
    let first_terminal = runtime
        .request_registered(&family, first, Key("value"), CancellationToken::new())
        .into_result()
        .unwrap();
    let second_terminal = runtime
        .request_registered(&family, second, Key("value"), CancellationToken::new())
        .into_result()
        .unwrap();
    let node = family.node(Key("value")).unwrap();
    let attempt_id = |revision| {
        lock(&node.node.state)
            .attempts
            .iter()
            .find(|attempt| attempt.revision == revision)
            .unwrap()
            .id
    };

    assert_eq!(
        lock(&node.node.state).validated_at,
        Some(ValidationCertificate {
            revision: second,
            stamp: second_terminal.stamp,
            terminal_revision: second,
            registered_only: false,
            epoch: read(&runtime.core.revisions).epoch_of(second.id).unwrap(),
            cone_missing_observation: false,
        })
    );
    family.detach_terminal_attempt(&node.node, attempt_id(first));
    assert_eq!(
        lock(&node.node.state)
            .validated_at
            .unwrap()
            .terminal_revision,
        second,
        "removing an older terminal preserves the current certificate"
    );
    family.detach_terminal_attempt(&node.node, attempt_id(second));
    assert!(lock(&node.node.state).validated_at.is_none());

    drop(first_terminal);
    drop(second_terminal);
}

#[test]
fn registered_batch_counts_current_certificates_rejected_for_missing_proof_lease() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(30, 12);
    let second = Revision::new(31, 12);
    let input = InputIdentity::new("source", "proof-reacquisition");
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 1)])
        .unwrap();

    let input_for_leaf = input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>(
            "proof-reacquisition-leaf",
            8,
            move |context, _, _| {
                context.input(input_for_leaf.clone())?;
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let leaf_for_middle = leaf.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>(
            "proof-reacquisition-middle",
            8,
            move |context, _, _| {
                context.query_registered(&leaf_for_middle, Key("leaf"))?;
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();
    let middle_for_top = middle.clone();
    let top = runtime
        .family_with_evaluator::<Key, u64, _>("proof-reacquisition-top", 8, move |context, _, _| {
            context.query_registered(&middle_for_top, Key("middle"))?;
            Ok(QueryOutput::success(3))
        })
        .unwrap();

    runtime
        .request_registered(&top, first, Key("top"), CancellationToken::new())
        .into_result()
        .unwrap();
    runtime
        .request_registered(&top, second, Key("top"), CancellationToken::new())
        .into_result()
        .unwrap();

    let before = runtime.metrics().validation;
    let root = runtime
        .family::<Key, u64>("proof-reacquisition-root", 1)
        .unwrap();
    let top_for_root = top.clone();
    runtime
        .request(
            &root,
            second,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _proof = context.endorse_registered_validations();
                context.query_registered_batch(&top_for_root, [Key("top")])?;
                Ok(QueryOutput::success(0))
            },
        )
        .into_result()
        .unwrap();

    let validation = runtime.metrics().validation.saturating_sub(before);
    assert_validation_work_consistent(validation);
    assert_eq!(validation.certificate_misses, 0);
    assert_eq!(validation.proof_reacquisition_misses, 2);
    assert_eq!(validation.memo_misses, 2);
    assert_eq!(validation.demand_reuses, 2);

    let top_terminal = runtime
        .request_registered(&top, second, Key("top"), CancellationToken::new())
        .into_result()
        .unwrap();
    let middle_terminal = runtime
        .request_registered(&middle, second, Key("middle"), CancellationToken::new())
        .into_result()
        .unwrap();
    let leaf_terminal = runtime
        .request_registered(&leaf, second, Key("leaf"), CancellationToken::new())
        .into_result()
        .unwrap();
    let mut retained = RetainedPinSet::new();
    retained.lease(top.pin_terminal(&top_terminal).unwrap());
    retained.lease(middle.pin_terminal(&middle_terminal).unwrap());
    retained.lease(leaf.pin_terminal(&leaf_terminal).unwrap());
    let retained = Arc::new(retained);

    let before = runtime.metrics().validation;
    let borrowed_root = runtime
        .family::<Key, u64>("proof-reacquisition-borrowed-root", 1)
        .unwrap();
    let top_for_borrowed_root = top.clone();
    let retained_for_root = retained.clone();
    runtime
        .request(
            &borrowed_root,
            second,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let baseline_refs = Arc::strong_count(&retained_for_root);
                {
                    let _proof = context
                        .endorse_registered_validations_from(std::slice::from_ref(
                            &retained_for_root,
                        ))
                        .unwrap();
                    assert_eq!(Arc::strong_count(&retained_for_root), baseline_refs + 1);
                    context.query_registered_batch(&top_for_borrowed_root, [Key("top")])?;
                }
                assert_eq!(Arc::strong_count(&retained_for_root), baseline_refs);
                Ok(QueryOutput::success(0))
            },
        )
        .into_result()
        .unwrap();

    let borrowed = runtime.metrics().validation.saturating_sub(before);
    assert_validation_work_consistent(borrowed);
    assert_eq!(borrowed.certificate_misses, 0);
    assert_eq!(borrowed.proof_reacquisition_misses, 0);
    assert_eq!(borrowed.memo_misses, 0);
    assert_eq!(borrowed.demands, 0);
    assert_eq!(borrowed.node_visits, 1);
    assert_eq!(borrowed.memo_hits, 1);
    assert_eq!(borrowed.endorsement_probes, 2);
    assert_eq!(borrowed.endorsement_hits, 1);
}

#[test]
fn registered_batch_seeds_parent_proofs_into_its_shared_authority() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(40, 13);
    let second = Revision::new(41, 13);
    let input = InputIdentity::new("source", "batch-seed");
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 1)])
        .unwrap();

    let input_for_leaf = input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("batch-seed-leaf", 8, move |context, _, _| {
            context.input(input_for_leaf.clone())?;
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let leaf_for_middle = leaf.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>("batch-seed-middle", 8, move |context, _, _| {
            context.query_registered(&leaf_for_middle, Key("leaf"))?;
            Ok(QueryOutput::success(2))
        })
        .unwrap();
    let middle_for_top = middle.clone();
    let top = runtime
        .family_with_evaluator::<Key, u64, _>("batch-seed-top", 8, move |context, _, _| {
            context.query_registered(&middle_for_top, Key("middle"))?;
            Ok(QueryOutput::success(3))
        })
        .unwrap();

    runtime
        .request_registered(&top, first, Key("top"), CancellationToken::new())
        .into_result()
        .unwrap();
    runtime
        .request_registered(&top, second, Key("top"), CancellationToken::new())
        .into_result()
        .unwrap();

    let before = runtime.metrics().validation;
    let root = runtime.family::<Key, u64>("batch-seed-root", 1).unwrap();
    let top_for_root = top.clone();
    runtime
        .request(
            &root,
            second,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _proof = context.endorse_registered_validations();
                // The first batch pays the reacquisition demands and its
                // absorbed proofs land in this task's endorsement scope.
                context.query_registered_batch(&top_for_root, [Key("top")])?;
                // A later batch in the same proof scope starts with those
                // proofs seeded into its shared authority, so its children
                // borrow them instead of re-demanding the same cone.
                context.query_registered_batch(&top_for_root, [Key("top")])?;
                Ok(QueryOutput::success(0))
            },
        )
        .into_result()
        .unwrap();

    let validation = runtime.metrics().validation.saturating_sub(before);
    assert_validation_work_consistent(validation);
    assert_eq!(validation.certificate_misses, 0);
    assert_eq!(
        validation.proof_reacquisition_misses, 2,
        "only the first batch re-leases the cone; the second borrows the seed"
    );
    assert_eq!(validation.demand_reuses, 2);
}

#[test]
fn batch_authority_publishes_proofs_atomically_with_backing() {
    let runtime = QueryRuntime::new(1);
    let revision = Revision::new(60, 19);
    let input = InputIdentity::new("source", "publish-proof");
    runtime
        .publish_revision(revision, [(input.clone(), 1)])
        .unwrap();
    let input_for_leaf = input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("publish-proof-leaf", 8, move |context, _, _| {
            context.input(input_for_leaf.clone())?;
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let terminal = runtime
        .request_registered(&leaf, revision, Key("leaf"), CancellationToken::new())
        .into_result()
        .unwrap();
    let identity = (terminal.node_incarnation, terminal.stamp, terminal.revision);

    // Only a batch that claimed extra workers is a per-proof publication
    // target; a sequential batch inside it publishes upward to it.
    let sequential = Arc::new(BatchValidationAuthority::new(
        runtime.core.clone(),
        None,
        false,
    ));
    assert!(sequential.nearest_concurrent().is_none());
    let concurrent = Arc::new(BatchValidationAuthority::new(
        runtime.core.clone(),
        Some(sequential.clone()),
        true,
    ));
    assert!(std::ptr::eq(
        concurrent.nearest_concurrent().unwrap(),
        concurrent.as_ref()
    ));
    let nested_sequential =
        BatchValidationAuthority::new(runtime.core.clone(), Some(concurrent.clone()), false);
    assert!(std::ptr::eq(
        nested_sequential.nearest_concurrent().unwrap(),
        concurrent.as_ref()
    ));

    assert!(!concurrent.retains_endorsement(identity.0, identity.1, identity.2));
    concurrent.publish_proof(
        identity,
        Box::new(leaf.pin_terminal(&terminal).unwrap()),
        true,
    );
    assert!(concurrent.retains_endorsement(identity.0, identity.1, identity.2));
    // Visibility probes walk the parent chain, so a sequential batch
    // nested inside the concurrent one sees the published proof too.
    assert!(nested_sequential.retains_endorsement(identity.0, identity.1, identity.2));

    // A racing duplicate publication keeps exactly one backing lease and
    // releases the redundant pin outside the authority's write lock.
    concurrent.publish_proof(
        identity,
        Box::new(leaf.pin_terminal(&terminal).unwrap()),
        true,
    );
    let state = read(&concurrent.state);
    assert_eq!(state.leases.held.len(), 1);
    assert!(state.endorsements.contains(&identity));
}

#[test]
fn concurrent_batch_children_borrow_proofs_published_before_sibling_completion() {
    // RUE-1584: the intra-batch first-touch race. Two children start
    // concurrently over the same certificate-valid cone; without
    // per-proof publication, neither sees the other's proofs until a
    // whole child completes, so both re-lease the shared cone. The
    // choreography parks the second child before its first probe, lets
    // the first child publish its cone proofs mid-item, and then holds
    // the first child short of completion while the second validates —
    // so any borrowed hit the second child gets can only come from the
    // per-proof publication path.
    let runtime = QueryRuntime::new(2);
    let first = Revision::new(70, 23);
    let second = Revision::new(71, 23);
    let input = InputIdentity::new("source", "first-touch");
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 1)])
        .unwrap();

    let input_for_leaf = input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("first-touch-leaf", 8, move |context, _, _| {
            context.input(input_for_leaf.clone())?;
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let leaf_for_middle = leaf.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>("first-touch-middle", 8, move |context, _, _| {
            context.query_registered(&leaf_for_middle, Key("leaf"))?;
            Ok(QueryOutput::success(2))
        })
        .unwrap();
    let middle_for_top = middle.clone();
    let top = runtime
        .family_with_evaluator::<Key, u64, _>("first-touch-top", 8, move |context, _, _| {
            context.query_registered(&middle_for_top, Key("middle"))?;
            Ok(QueryOutput::success(3))
        })
        .unwrap();

    for key in ["top-a", "top-b"] {
        for revision in [first, second] {
            runtime
                .request_registered(&top, revision, Key(key), CancellationToken::new())
                .into_result()
                .unwrap();
        }
    }

    // Install the choreography only after warm-up, so the batch's own
    // workers are the first threads the hook ever sees. Both workers must
    // rendezvous holding one item each before either validates: without
    // that, one worker can drain the whole queue before the other wakes,
    // and sequential completion publication alone would satisfy every
    // assertion below without the per-proof path ever firing.
    let rendezvous = Arc::new((StdMutex::new(0usize), Condvar::new()));
    let rendezvoused: Arc<StdMutex<AHashSet<thread::ThreadId>>> =
        Arc::new(StdMutex::new(AHashSet::new()));
    let first_worker: Arc<StdMutex<Option<thread::ThreadId>>> = Arc::new(StdMutex::new(None));
    let first_worker_publishes = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new((StdMutex::new((false, false)), Condvar::new()));
    {
        let rendezvous = rendezvous.clone();
        let rendezvoused = rendezvoused.clone();
        let first_worker = first_worker.clone();
        let first_worker_publishes = first_worker_publishes.clone();
        let gate = gate.clone();
        runtime.set_interpose(Arc::new(move |site| {
            let me = thread::current().id();
            match site {
                InterposeSite::ReuseDiscovered => {
                    if !rendezvoused.lock().unwrap().insert(me) {
                        return;
                    }
                    {
                        let (lock, condvar) = &*rendezvous;
                        let mut arrivals = lock.lock().unwrap();
                        *arrivals += 1;
                        condvar.notify_all();
                        let (arrivals, timeout) = condvar
                            .wait_timeout_while(arrivals, Duration::from_secs(30), |arrivals| {
                                *arrivals < 2
                            })
                            .unwrap();
                        drop(arrivals);
                        assert!(
                            !timeout.timed_out(),
                            "the batch never ran its two items on two workers; \
                                 the race under test cannot occur on one thread"
                        );
                    }
                    let mut owner = first_worker.lock().unwrap();
                    match *owner {
                        None => *owner = Some(me),
                        Some(claimed) if claimed == me => {}
                        Some(_) => {
                            drop(owner);
                            let (lock, condvar) = &*gate;
                            let released = lock.lock().unwrap();
                            let (released, timeout) = condvar
                                .wait_timeout_while(released, Duration::from_secs(30), |released| {
                                    !released.0
                                })
                                .unwrap();
                            drop(released);
                            assert!(
                                !timeout.timed_out(),
                                "the first toucher never published its third proof; \
                                     per-proof batch publication is not happening"
                            );
                        }
                    }
                }
                InterposeSite::BatchProofPublished => {
                    let is_first = *first_worker.lock().unwrap() == Some(me);
                    let (lock, condvar) = &*gate;
                    if is_first {
                        // leaf, middle, and the item's own root: after the
                        // third publication the first child's whole cone
                        // is visible while the child itself is still
                        // running. Release the sibling, then park until it
                        // has finished borrowing.
                        if first_worker_publishes.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
                            let mut released = lock.lock().unwrap();
                            released.0 = true;
                            condvar.notify_all();
                            let (released, timeout) = condvar
                                .wait_timeout_while(released, Duration::from_secs(30), |released| {
                                    !released.1
                                })
                                .unwrap();
                            drop(released);
                            assert!(
                                !timeout.timed_out(),
                                "the sibling never published its own root after borrowing"
                            );
                        }
                    } else {
                        let mut released = lock.lock().unwrap();
                        released.1 = true;
                        condvar.notify_all();
                    }
                }
                _ => {}
            }
        }));
    }

    let before = runtime.metrics().validation;
    let root = runtime.family::<Key, u64>("first-touch-root", 1).unwrap();
    let top_for_root = top.clone();
    runtime
        .request(
            &root,
            second,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _proof = context.endorse_registered_validations();
                context.query_registered_batch(&top_for_root, [Key("top-a"), Key("top-b")])?;
                Ok(QueryOutput::success(0))
            },
        )
        .into_result()
        .unwrap();
    runtime.clear_interpose();

    let validation = runtime.metrics().validation.saturating_sub(before);
    assert_validation_work_consistent(validation);
    assert_eq!(validation.certificate_misses, 0);
    assert_eq!(
        validation.proof_reacquisition_misses, 2,
        "only the first toucher re-leases the shared cone; its sibling \
             borrows the mid-item publications"
    );
    assert_eq!(validation.demands, 2);
    assert_eq!(validation.demand_reuses, 2);
    assert_eq!(
        validation.memo_hits, 1,
        "the second child's shared dependency validates as a borrowed memo hit"
    );
}

#[test]
fn borrowed_validation_authority_does_not_endorse_a_stale_root() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(1, 1);
    let second = Revision::new(2, 1);
    let input = InputIdentity::new("source", "borrowed-stale-root");
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 2)])
        .unwrap();

    let runs = Arc::new(AtomicUsize::new(0));
    let runs_for_evaluator = runs.clone();
    let input_for_evaluator = input.clone();
    let value = runtime
        .family_with_evaluator::<Key, u64, _>(
            "borrowed-stale-root-value",
            8,
            move |context, _, _| {
                runs_for_evaluator.fetch_add(1, Ordering::Relaxed);
                Ok(QueryOutput::success(
                    context.input(input_for_evaluator.clone())?,
                ))
            },
        )
        .unwrap();
    let first_terminal = runtime
        .request_registered(&value, first, Key("value"), CancellationToken::new())
        .into_result()
        .unwrap();
    let mut retained = RetainedPinSet::new();
    retained.lease(value.pin_terminal(&first_terminal).unwrap());
    let retained = Arc::new(retained);

    let root = runtime
        .family::<Key, u64>("borrowed-stale-root-request", 1)
        .unwrap();
    let value_for_root = value.clone();
    let first_terminal_for_root = first_terminal.clone();
    let retained_for_root = retained.clone();
    let result = runtime
        .request(
            &root,
            second,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _proof = context
                    .endorse_registered_validations_from(std::slice::from_ref(&retained_for_root))
                    .unwrap();
                assert_eq!(
                    context
                        .task
                        .validation_endorsement_authority_for_terminal(&first_terminal_for_root,),
                    ValidationEndorsementAuthority::Borrowed,
                );
                assert_eq!(
                    context
                        .task
                        .validation_candidate_endorsement_authority_for_terminal(
                            &first_terminal_for_root,
                        ),
                    ValidationEndorsementAuthority::Missing,
                    "fallback retention cannot bypass candidate-root validation",
                );
                let terminal = context.query_registered(&value_for_root, Key("value"))?;
                let QueryOutcome::Success(value) = terminal.outcome() else {
                    unreachable!()
                };
                Ok(QueryOutput::success(*value))
            },
        )
        .into_result()
        .unwrap();

    assert_eq!(result.outcome(), &QueryOutcome::Success(2));
    assert_eq!(runs.load(Ordering::Relaxed), 2);
}

#[test]
fn task_local_root_authority_requires_the_exact_terminal_revision() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(1, 1);
    let second = Revision::new(2, 2);
    publish_empty(&runtime, [first, second]);

    let runs = Arc::new(AtomicUsize::new(0));
    let runs_for_evaluator = runs.clone();
    let value = runtime
        .family_with_evaluator::<Key, u64, _>(
            "task-local-exact-revision-value",
            8,
            move |_, _, _| {
                runs_for_evaluator.fetch_add(1, Ordering::Relaxed);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let first_terminal = runtime
        .request_registered(&value, first, Key("value"), CancellationToken::new())
        .into_result()
        .unwrap();
    let second_terminal = runtime
        .request_registered(&value, second, Key("value"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(first_terminal.stamp, second_terminal.stamp);
    assert_ne!(first_terminal.revision, second_terminal.revision);

    let root = runtime
        .family::<Key, u64>("task-local-exact-revision-root", 1)
        .unwrap();
    let value_for_root = value.clone();
    let first_terminal_for_root = first_terminal.clone();
    let second_terminal_for_root = second_terminal.clone();
    runtime
        .request(
            &root,
            first,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _proof = context.endorse_registered_validations();
                context.task.endorse_validation(&first_terminal_for_root);
                assert_eq!(
                    context
                        .task
                        .validation_endorsement_authority_for_terminal(&first_terminal_for_root,),
                    ValidationEndorsementAuthority::TaskLocal,
                );
                assert_eq!(
                    context
                        .task
                        .validation_endorsement_authority_for_terminal(&second_terminal_for_root,),
                    ValidationEndorsementAuthority::Missing,
                );
                assert!(context.task.validation_endorsed_identity(
                    first_terminal_for_root.node_incarnation,
                    first_terminal_for_root.stamp,
                    first_terminal_for_root.revision,
                ));
                let selected = context.query_registered(&value_for_root, Key("value"))?;
                assert!(Arc::ptr_eq(&selected, &first_terminal_for_root));
                Ok(QueryOutput::success(1))
            },
        )
        .into_result()
        .unwrap();
    assert_eq!(runs.load(Ordering::Relaxed), 2);
}

#[test]
fn borrowed_validation_authority_accepts_an_equal_cone_across_revisions() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(1, 1);
    let second = Revision::new(2, 2);
    publish_empty(&runtime, [first, second]);

    let value = runtime
        .family_with_evaluator::<Key, u64, _>("borrowed-equal-cone-value", 8, |_, _, _| {
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let first_terminal = runtime
        .request_registered(&value, first, Key("value"), CancellationToken::new())
        .into_result()
        .unwrap();
    let second_terminal = runtime
        .request_registered(&value, second, Key("value"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(first_terminal.stamp, second_terminal.stamp);
    assert_ne!(first_terminal.revision, second_terminal.revision);

    let mut retained = RetainedPinSet::new();
    retained.lease(value.pin_terminal(&second_terminal).unwrap());
    let retained = Arc::new(retained);
    let root = runtime
        .family::<Key, u64>("borrowed-equal-cone-root", 1)
        .unwrap();
    let retained_for_root = retained.clone();
    let first_terminal_for_root = first_terminal.clone();
    let second_terminal_for_root = second_terminal.clone();
    runtime
        .request(
            &root,
            first,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _proof = context
                    .endorse_registered_validations_from(std::slice::from_ref(&retained_for_root))
                    .unwrap();
                assert_eq!(
                    context
                        .task
                        .validation_endorsement_authority_for_terminal(&first_terminal_for_root,),
                    ValidationEndorsementAuthority::Borrowed,
                );
                assert_eq!(
                    context
                        .task
                        .validation_endorsement_authority_for_terminal(&second_terminal_for_root,),
                    ValidationEndorsementAuthority::Borrowed,
                );
                Ok(QueryOutput::success(1))
            },
        )
        .into_result()
        .unwrap();
}

#[test]
fn same_stamp_fallback_supplies_its_complete_alternate_cone() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(1, 1);
    let second = Revision::new(2, 1);
    let selector = InputIdentity::new("source", "exact-fallback-cone");
    runtime
        .publish_revision(first, [(selector.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(selector.clone(), 2)])
        .unwrap();

    let left = runtime
        .family_with_evaluator::<Key, u64, _>("exact-fallback-left", 4, |_, _, _| {
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let right = runtime
        .family_with_evaluator::<Key, u64, _>("exact-fallback-right", 4, |_, _, _| {
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let selector_for_leaf = selector.clone();
    let left_for_leaf = left.clone();
    let right_for_leaf = right.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("exact-fallback-leaf", 4, move |context, _, _| {
            if context.input(selector_for_leaf.clone())? == 1 {
                context.query_registered(&left_for_leaf, Key("left"))?;
            } else {
                context.query_registered(&right_for_leaf, Key("right"))?;
            }
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let leaf_for_parent = leaf.clone();
    let parent = runtime
        .family_with_evaluator::<Key, u64, _>("exact-fallback-parent", 4, move |context, _, _| {
            context.query_registered(&leaf_for_parent, Key("leaf"))?;
            Ok(QueryOutput::success(1))
        })
        .unwrap();

    let parent_first = runtime
        .request_registered(&parent, first, Key("parent"), CancellationToken::new())
        .into_result()
        .unwrap();
    let leaf_first = runtime
        .request_registered(&leaf, first, Key("leaf"), CancellationToken::new())
        .into_result()
        .unwrap();
    let left_first = runtime
        .request_registered(&left, first, Key("left"), CancellationToken::new())
        .into_result()
        .unwrap();
    let leaf_second = runtime
        .request_registered(&leaf, second, Key("leaf"), CancellationToken::new())
        .into_result()
        .unwrap();
    let right_second = runtime
        .request_registered(&right, second, Key("right"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(leaf_first.stamp, leaf_second.stamp);
    assert_ne!(leaf_first.revision, leaf_second.revision);
    assert_ne!(leaf_first.dependencies(), leaf_second.dependencies());

    let mut fallback = RetainedPinSet::new();
    fallback.lease(leaf.pin_terminal(&leaf_second).unwrap());
    fallback.lease(right.pin_terminal(&right_second).unwrap());
    let fallback = Arc::new(fallback);

    // The current certificate deliberately names the first terminal. The
    // fallback holds an equal-stamp terminal for the same node, but with a
    // different complete dependency cone at the second revision. Query
    // edges name the shared node/stamp, so promotion may safely retain the
    // fallback representative and must walk that representative's edges.
    let leaf_node = leaf.node(Key("leaf")).unwrap();
    assert!(leaf_node.node.mark_validated(ValidationCertificate {
        revision: first,
        stamp: leaf_first.stamp,
        terminal_revision: leaf_first.revision,
        registered_only: true,
        epoch: read(&runtime.core.revisions).epoch_of(first.id).unwrap(),
        cone_missing_observation: false,
    }));
    drop(leaf_node);

    let root = runtime
        .family::<Key, u64>("exact-fallback-cone-root", 1)
        .unwrap();
    let parent_for_root = parent.clone();
    let fallback_for_root = fallback.clone();
    let parent_first_for_root = parent_first.clone();
    let leaf_first_for_root = leaf_first.clone();
    let leaf_second_for_root = leaf_second.clone();
    let left_first_for_root = left_first.clone();
    let right_second_for_root = right_second.clone();
    runtime
        .request(
            &root,
            first,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _proof = context
                    .endorse_registered_validations_from(std::slice::from_ref(&fallback_for_root))
                    .unwrap();
                let selected = context.query_registered(&parent_for_root, Key("parent"))?;
                assert!(Arc::ptr_eq(&selected, &parent_first_for_root));
                let retained = context
                    .retain_observed_terminal_cone(&selected)
                    .expect("the complete equal-stamp fallback cone is retained");
                assert!(!retained.observed.contains(&(
                    leaf_first_for_root.node_incarnation,
                    leaf_first_for_root.stamp,
                    leaf_first_for_root.revision,
                )));
                assert!(!retained.observed.contains(&(
                    left_first_for_root.node_incarnation,
                    left_first_for_root.stamp,
                    left_first_for_root.revision,
                )));
                assert!(retained.observed.contains(&(
                    leaf_second_for_root.node_incarnation,
                    leaf_second_for_root.stamp,
                    leaf_second_for_root.revision,
                )));
                assert!(retained.observed.contains(&(
                    right_second_for_root.node_incarnation,
                    right_second_for_root.stamp,
                    right_second_for_root.revision,
                )));
                Ok(QueryOutput::success(0))
            },
        )
        .into_result()
        .unwrap();
}

#[test]
fn red_green_validation_is_direct_recursive_and_semantic() {
    let runtime = QueryRuntime::new(1);
    let leaf = runtime.family::<Key, u64>("rg-leaf", 8).unwrap();
    let middle = runtime.family::<Key, u64>("rg-middle", 8).unwrap();
    let root = runtime.family::<Key, u64>("rg-root", 8).unwrap();
    let input = InputIdentity::new("source", "main");
    let revisions = [
        Revision::new(10, 1),
        Revision::new(11, 1),
        Revision::new(12, 1),
    ];
    for (revision, stamp) in revisions.into_iter().zip([1, 2, 3]) {
        runtime
            .publish_revision(revision, [(input.clone(), stamp)])
            .unwrap();
    }

    let initial_leaf = leaf.clone();
    let initial_middle = middle.clone();
    let first = runtime
        .query(
            &root,
            revisions[0],
            Key("root"),
            CancellationToken::new(),
            |context| {
                let middle = context.query(&initial_middle, Key("middle"), |context| {
                    let leaf = context.query(&initial_leaf, Key("leaf"), |context| {
                        context.input(input.clone())?;
                        Ok(QueryOutput::success(10))
                    })?;
                    let QueryOutcome::Success(value) = leaf.outcome() else {
                        unreachable!()
                    };
                    Ok(QueryOutput::success(*value))
                })?;
                assert_eq!(middle.inputs().len(), 0);
                Ok(QueryOutput::success(99))
            },
        )
        .unwrap();
    assert_eq!(first.inputs().len(), 0);
    assert_eq!(first.dependencies().len(), 1);
    let first_leaf = runtime
        .query(
            &leaf,
            revisions[0],
            Key("leaf"),
            CancellationToken::new(),
            |_| panic!("initial leaf terminal is retained"),
        )
        .unwrap();

    // The direct leaf changes input but recomputes to equal semantics, so
    // it stays red. Recursive validation can then reuse both ancestors.
    let red_leaf = runtime
        .query(
            &leaf,
            revisions[1],
            Key("leaf"),
            CancellationToken::new(),
            |context| {
                context.input(input.clone())?;
                Ok(QueryOutput::success(10))
            },
        )
        .unwrap();
    assert_eq!(red_leaf.stamp(), first_leaf.stamp());
    let before = runtime.metrics().validation;
    let reused = runtime.request(
        &root,
        revisions[1],
        Key("root"),
        CancellationToken::new(),
        |_| panic!("validated red dependency chain must reuse the root"),
    );
    assert_eq!(reused.execution(), RequestExecution::Reused);
    let direct_validation = runtime.metrics().validation.saturating_sub(before);
    assert!(direct_validation.registry_probes > 0);
    assert_eq!(
        direct_validation.registry_index_lookups, 0,
        "live runtime-created observations resolve without the shared registry"
    );

    // A green leaf makes the middle invalid, which recursively makes the
    // root recompute. Equal root semantics still retain the root stamp.
    let green_leaf = runtime
        .query(
            &leaf,
            revisions[2],
            Key("leaf"),
            CancellationToken::new(),
            |context| {
                context.input(input.clone())?;
                Ok(QueryOutput::success(11))
            },
        )
        .unwrap();
    assert_ne!(green_leaf.stamp(), red_leaf.stamp());
    let recompute_leaf = leaf.clone();
    let recompute_middle = middle.clone();
    let recomputed = runtime.request(
        &root,
        revisions[2],
        Key("root"),
        CancellationToken::new(),
        |context| {
            let middle = context.query(&recompute_middle, Key("middle"), |context| {
                let leaf = context.query(&recompute_leaf, Key("leaf"), |_| {
                    panic!("green leaf was already validated")
                })?;
                let QueryOutcome::Success(value) = leaf.outcome() else {
                    unreachable!()
                };
                Ok(QueryOutput::success(*value))
            })?;
            assert_eq!(middle.outcome(), &QueryOutcome::Success(11));
            Ok(QueryOutput::success(99))
        },
    );
    assert_eq!(recomputed.execution(), RequestExecution::Computed);
    assert_eq!(recomputed.terminal().unwrap().stamp(), first.stamp());
}

#[test]
fn registered_evaluators_propagate_red_from_a_root_only_request() {
    let runtime = QueryRuntime::new(1);
    let input = InputIdentity::new("source", "registered");
    let leaf_runs = Arc::new(AtomicUsize::new(0));
    let leaf_input = input.clone();
    let leaf_counter = leaf_runs.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("registered-leaf", 8, move |context, _, _| {
            leaf_counter.fetch_add(1, Ordering::Relaxed);
            let stamp = context.input(leaf_input.clone())?;
            Ok(QueryOutput::success(if stamp < 3 { 10 } else { 11 }))
        })
        .unwrap();

    let middle_runs = Arc::new(AtomicUsize::new(0));
    let middle_leaf = leaf.clone();
    let middle_counter = middle_runs.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>("registered-middle", 8, move |context, _, _| {
            middle_counter.fetch_add(1, Ordering::Relaxed);
            let leaf = context.query_registered(&middle_leaf, Key("leaf"))?;
            let QueryOutcome::Success(value) = leaf.outcome() else {
                unreachable!()
            };
            Ok(QueryOutput::success(*value))
        })
        .unwrap();

    let root_runs = Arc::new(AtomicUsize::new(0));
    let root_middle = middle.clone();
    let root_counter = root_runs.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("registered-root", 8, move |context, _, _| {
            root_counter.fetch_add(1, Ordering::Relaxed);
            context.query_registered(&root_middle, Key("middle"))?;
            Ok(QueryOutput::success(99))
        })
        .unwrap();

    let revisions = [
        Revision::new(30, 1),
        Revision::new(31, 1),
        Revision::new(32, 1),
    ];
    for (revision, stamp) in revisions.into_iter().zip([1, 2, 3]) {
        runtime
            .publish_revision(revision, [(input.clone(), stamp)])
            .unwrap();
    }

    let first =
        runtime.request_registered(&root, revisions[0], Key("root"), CancellationToken::new());
    assert_eq!(first.execution(), RequestExecution::Computed);
    assert_eq!(leaf_runs.load(Ordering::Relaxed), 1);
    assert_eq!(middle_runs.load(Ordering::Relaxed), 1);
    assert_eq!(root_runs.load(Ordering::Relaxed), 1);

    // Only the root is requested. Validation demands the dirty leaf by its
    // recorded exact key. Equal leaf semantics preserve its stamp, so both
    // ancestors validate without running their bodies.
    let red =
        runtime.request_registered(&root, revisions[1], Key("root"), CancellationToken::new());
    assert_eq!(red.execution(), RequestExecution::Reused);
    assert_eq!(leaf_runs.load(Ordering::Relaxed), 2);
    assert_eq!(middle_runs.load(Ordering::Relaxed), 1);
    assert_eq!(root_runs.load(Ordering::Relaxed), 1);
    assert!(
        red.nested_attempts()
            .iter()
            .any(|attempt| attempt.node().family() == "registered-leaf"
                && attempt.execution() == RequestExecution::Computed)
    );

    // A green leaf invalidates and recomputes each ancestor. The root's
    // own equal semantics remain red even though its body must run.
    let green =
        runtime.request_registered(&root, revisions[2], Key("root"), CancellationToken::new());
    assert_eq!(green.execution(), RequestExecution::Computed);
    assert_eq!(leaf_runs.load(Ordering::Relaxed), 3);
    assert_eq!(middle_runs.load(Ordering::Relaxed), 2);
    assert_eq!(root_runs.load(Ordering::Relaxed), 2);
    assert_eq!(
        green.terminal().unwrap().stamp(),
        first.terminal().unwrap().stamp()
    );
}

#[test]
fn nested_validation_does_not_promote_transitive_dependencies_to_the_caller() {
    let runtime = QueryRuntime::new(1);
    let leaf_input = InputIdentity::new("source", "validation-leaf");
    let root_input = InputIdentity::new("source", "validation-root");
    let leaf_source = leaf_input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("validation-leaf", 8, move |context, _, _| {
            context.input(leaf_source.clone())?;
            Ok(QueryOutput::success(10).with_work(vec![WorkItem::new("validation-leaf-work", 1)]))
        })
        .unwrap();
    let middle_leaf = leaf.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>("validation-middle", 8, move |context, _, _| {
            context.query_registered(&middle_leaf, Key("leaf"))?;
            Ok(QueryOutput::success(20))
        })
        .unwrap();
    let root_middle = middle.clone();
    let root_source = root_input.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("validation-root", 8, move |context, _, _| {
            context.input(root_source.clone())?;
            context.query_registered(&root_middle, Key("middle"))?;
            Ok(QueryOutput::success(30))
        })
        .unwrap();
    let first_revision = Revision::new(40, 1);
    let second_revision = Revision::new(41, 1);
    runtime
        .publish_revision(
            first_revision,
            [(leaf_input.clone(), 1), (root_input.clone(), 1)],
        )
        .unwrap();
    runtime
        .publish_revision(second_revision, [(leaf_input, 2), (root_input, 2)])
        .unwrap();

    runtime.request_registered(&root, first_revision, Key("root"), CancellationToken::new());
    let recomputed = runtime.request_registered(
        &root,
        second_revision,
        Key("root"),
        CancellationToken::new(),
    );

    assert_eq!(recomputed.execution(), RequestExecution::Computed);
    let dependencies = recomputed.terminal().unwrap().dependencies();
    assert_eq!(dependencies.len(), 1);
    assert_eq!(dependencies[0].node.family(), "validation-middle");
    assert!(
        recomputed
            .work()
            .iter()
            .any(|(identity, amount)| identity.as_ref() == "validation-leaf-work" && *amount == 1)
    );
}

#[test]
fn unavailable_registered_validation_invalidates_a_supplied_parent() {
    let runtime = QueryRuntime::new(1);
    let input = InputIdentity::new("source", "validation-canceled-child");
    let child_input = input.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>(
            "validation-canceled-child",
            8,
            move |context, _, _| {
                let stamp = context.input(child_input.clone())?;
                if stamp == 1 {
                    Ok(QueryOutput::success(10))
                } else {
                    Err(QueryAbort::Canceled)
                }
            },
        )
        .unwrap();
    let root = runtime
        .family::<Key, u64>("validation-supplied-root", 8)
        .unwrap();
    let first_revision = Revision::new(50, 1);
    let second_revision = Revision::new(51, 1);
    runtime
        .publish_revision(first_revision, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second_revision, [(input, 2)])
        .unwrap();

    let initial_child = child.clone();
    runtime
        .query(
            &root,
            first_revision,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                context.query_registered(&initial_child, Key("child"))?;
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();

    let recomputed = runtime.request(
        &root,
        second_revision,
        Key("root"),
        CancellationToken::new(),
        |_| Ok(QueryOutput::success(2)),
    );
    assert_eq!(recomputed.execution(), RequestExecution::Computed);
    assert_eq!(
        recomputed.terminal().unwrap().outcome(),
        &QueryOutcome::Success(2)
    );
}

#[test]
fn nested_requests_retain_computed_and_reused_lifecycles() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let child = runtime
        .family::<Key, u64>("nested-ledger-child", 4)
        .unwrap();
    let root = runtime.family::<Key, u64>("nested-ledger-root", 4).unwrap();

    let first_child = child.clone();
    let first = runtime.request(
        &root,
        revision(1),
        Key("first"),
        CancellationToken::new(),
        move |context| {
            context.query(&first_child, Key("child"), |_| {
                Ok(QueryOutput::success(7).with_work(vec![WorkItem::new("child", 2)]))
            })?;
            Ok(QueryOutput::success(1))
        },
    );
    assert_eq!(first.nested_attempts().len(), 1);
    assert_eq!(
        first.nested_attempts()[0].execution(),
        RequestExecution::Computed
    );
    assert_eq!(
        first.nested_attempts()[0].id(),
        first.nested_attempts()[0].origin_request_id()
    );
    assert!(first.nested_attempts()[0].node_incarnation().is_some());
    assert_eq!(
        first.nested_attempts()[0].work(),
        &[(Arc::<str>::from("child"), 2)]
    );

    let reused_child = child.clone();
    let second = runtime.request(
        &root,
        revision(1),
        Key("second"),
        CancellationToken::new(),
        move |context| {
            context.query(&reused_child, Key("child"), |_| {
                panic!("the retained child must be reused")
            })?;
            Ok(QueryOutput::success(2))
        },
    );
    assert_eq!(second.nested_attempts().len(), 1);
    assert_eq!(
        second.nested_attempts()[0].execution(),
        RequestExecution::Reused
    );
    assert!(second.nested_attempts()[0].work().is_empty());
    assert_eq!(
        second.nested_attempts()[0].origin_request_id(),
        first.nested_attempts()[0].id()
    );
}

#[test]
fn nested_attempt_filter_is_nestable_unwind_safe_and_preserves_request_ids() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let selected = runtime
        .family::<Key, u64>("nested-filter-selected", 8)
        .unwrap();
    let suppressed = runtime
        .family::<Key, u64>("nested-filter-suppressed", 8)
        .unwrap();
    let root = runtime.family::<Key, u64>("nested-filter-root", 8).unwrap();

    let selected_for_root = selected.clone();
    let suppressed_for_root = suppressed.clone();
    let attempt = runtime.request(
        &root,
        revision(1),
        Key("root"),
        CancellationToken::new(),
        move |context| {
            let _outer = context.retain_nested_attempts_for(&["nested-filter-selected"]);
            context.query(&selected_for_root, Key("cold"), |_| {
                Ok(QueryOutput::success(1))
            })?;
            context.query(&suppressed_for_root, Key("cold"), |_| {
                Ok(QueryOutput::success(2))
            })?;
            {
                let _inner = context.retain_nested_attempts_for(&[
                    "nested-filter-selected",
                    "nested-filter-suppressed",
                ]);
                context.query(&selected_for_root, Key("cold"), |_| {
                    panic!("the selected terminal is retained")
                })?;
                context.query(&suppressed_for_root, Key("cold"), |_| {
                    panic!("the suppressed terminal is retained")
                })?;
            }
            #[cfg(panic = "unwind")]
            {
                let unwind = catch_unwind(AssertUnwindSafe(|| {
                    let _inner = context.retain_nested_attempts_for(&["nested-filter-suppressed"]);
                    panic!("exercise filter restoration")
                }));
                assert!(unwind.is_err());
            }
            #[cfg(panic = "abort")]
            {
                // Rue's production/test profile aborts on panic, so the
                // executable restoration check uses ordinary RAII here.
                // The unwind profile above exercises the same Drop path
                // during catch_unwind.
                let _inner = context.retain_nested_attempts_for(&["nested-filter-suppressed"]);
            }
            context.query(&selected_for_root, Key("cold"), |_| {
                panic!("the outer selection survives inner unwinding")
            })?;
            let ignored_abort = context.query(&suppressed_for_root, Key("abort"), |_| {
                Err(QueryAbort::Canceled)
            });
            assert!(matches!(ignored_abort, Err(QueryAbort::Canceled)));
            context.query(&selected_for_root, Key("abort"), |_| {
                Err(QueryAbort::Canceled)
            })?;
            unreachable!("the selected abort propagates")
        },
    );

    assert_eq!(attempt.execution(), RequestExecution::Aborted);
    assert_eq!(attempt.abort(), Some(&QueryAbort::Canceled));
    assert_eq!(attempt.nested_attempts().len(), 4);
    assert!(
        attempt
            .nested_attempts()
            .iter()
            .all(|nested| nested.node().family() == "nested-filter-selected")
    );
    assert_eq!(
        attempt
            .nested_attempts()
            .iter()
            .map(NestedQueryAttempt::execution)
            .collect::<Vec<_>>(),
        [
            RequestExecution::Computed,
            RequestExecution::Reused,
            RequestExecution::Reused,
            RequestExecution::Aborted,
        ]
    );
    assert!(
        attempt
            .nested_attempts()
            .windows(2)
            .any(|pair| pair[1].id() > pair[0].id() + 1),
        "suppressed rows still consume request identities"
    );
}

#[test]
fn nested_attempt_filter_preserves_green_red_cancel_work_leases_and_handoffs() {
    #[derive(Debug, PartialEq, Eq)]
    struct Run {
        executions: Vec<RequestExecution>,
        values: Vec<Option<u64>>,
        dependencies: Vec<usize>,
        inputs: Vec<usize>,
        work: Vec<Vec<(Arc<str>, u64)>>,
        commits: usize,
        aborts: usize,
    }

    fn run(filtered: bool) -> Run {
        let runtime = QueryRuntime::new(1);
        let input = InputIdentity::new("source", "nested-filter-parity");
        let first = Revision::new(40, 1);
        let green = Revision::new(41, 1);
        let red = Revision::new(42, 1);
        runtime
            .publish_revision(first, [(input.clone(), 7)])
            .unwrap();
        runtime
            .publish_revision(green, [(input.clone(), 7)])
            .unwrap();
        runtime.publish_revision(red, [(input.clone(), 8)]).unwrap();
        let noise = runtime
            .family_with_evaluator::<Key, u64, _>("nested-filter-parity-noise", 1, |_, _, _| {
                Ok(QueryOutput::success(10))
            })
            .unwrap();
        let commits = Arc::new(AtomicUsize::new(0));
        let aborts = Arc::new(AtomicUsize::new(0));
        let input_for_child = input.clone();
        let noise_for_child = noise.clone();
        let commits_for_child = commits.clone();
        let aborts_for_child = aborts.clone();
        let child = runtime
            .family_with_evaluator::<Key, u64, _>(
                "nested-filter-parity-child",
                1,
                move |context, _, key| {
                    let noise = context.query_registered(&noise_for_child, Key("noise"))?;
                    let QueryOutcome::Success(noise) = noise.outcome() else {
                        unreachable!()
                    };
                    let stamp = context.input(input_for_child.clone())?;
                    context.record_work(WorkItem::new("child-work", 1));
                    context.register_attempt_handoff(CountingHandoff {
                        commits: commits_for_child.clone(),
                        aborts: aborts_for_child.clone(),
                    });
                    Ok(QueryOutput::success(stamp + noise + key.0.len() as u64))
                },
            )
            .unwrap();
        let root = runtime
            .family::<Key, u64>("nested-filter-parity-root", 8)
            .unwrap();

        let request = |revision, root_key, child_key, cancellation: CancellationToken, cancel| {
            let child = child.clone();
            let cancellation_for_body = cancellation.clone();
            runtime.request(&root, revision, root_key, cancellation, move |context| {
                let _filter = filtered
                    .then(|| context.retain_nested_attempts_for(&["nested-filter-parity-child"]));
                let child = context.query_registered(&child, child_key)?;
                let QueryOutcome::Success(value) = child.outcome() else {
                    unreachable!()
                };
                if cancel {
                    cancellation_for_body.cancel();
                }
                Ok(QueryOutput::success(*value))
            })
        };

        let cold = request(
            first,
            Key("cold-root"),
            Key("value"),
            CancellationToken::new(),
            false,
        );
        let warm = request(
            green,
            Key("green-root"),
            Key("value"),
            CancellationToken::new(),
            false,
        );
        let changed = request(
            red,
            Key("red-root"),
            Key("value"),
            CancellationToken::new(),
            false,
        );
        let canceled = request(
            red,
            Key("cancel-root"),
            Key("cancel"),
            CancellationToken::new(),
            true,
        );
        let recovered = request(
            red,
            Key("recover-root"),
            Key("cancel"),
            CancellationToken::new(),
            false,
        );
        let attempts = [&cold, &warm, &changed, &canceled, &recovered];
        if filtered {
            assert!(attempts.iter().all(|attempt| {
                attempt
                    .nested_attempts()
                    .iter()
                    .all(|nested| nested.node().family() == "nested-filter-parity-child")
            }));
        } else {
            assert!(attempts.iter().any(|attempt| {
                attempt
                    .nested_attempts()
                    .iter()
                    .any(|nested| nested.node().family() == "nested-filter-parity-noise")
            }));
        }

        Run {
            executions: attempts.iter().map(|attempt| attempt.execution()).collect(),
            values: attempts
                .iter()
                .map(|attempt| {
                    attempt.terminal().and_then(|terminal| {
                        let QueryOutcome::Success(value) = terminal.outcome() else {
                            return None;
                        };
                        Some(*value)
                    })
                })
                .collect(),
            dependencies: attempts
                .iter()
                .map(|attempt| attempt.dependencies().len())
                .collect(),
            inputs: attempts
                .iter()
                .map(|attempt| attempt.inputs().len())
                .collect(),
            work: attempts
                .iter()
                .map(|attempt| attempt.work().to_vec())
                .collect(),
            commits: commits.load(Ordering::SeqCst),
            aborts: aborts.load(Ordering::SeqCst),
        }
    }

    let ordinary = run(false);
    let filtered = run(true);
    assert_eq!(filtered, ordinary);
    assert_eq!(
        filtered.executions,
        [
            RequestExecution::Computed,
            RequestExecution::Computed,
            RequestExecution::Computed,
            RequestExecution::Aborted,
            RequestExecution::Computed,
        ]
    );
    assert_eq!(filtered.commits, 3);
    assert_eq!(filtered.aborts, 0);
    assert_eq!(filtered.inputs, [0, 0, 0, 0, 0]);
}

#[test]
fn validation_endorsement_identity_lookup_uses_one_hash_index_probe() {
    const ENDORSEMENTS: u64 = 65_536;

    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family::<Key, u64>("endorsement-index-target", 1)
        .unwrap();
    let terminal = runtime
        .query(
            &family,
            revision(1),
            Key("target"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(1)),
        )
        .unwrap();
    let root = runtime
        .family::<Key, u64>("endorsement-index-root", 1)
        .unwrap();
    let attempt = runtime.request(
        &root,
        revision(1),
        Key("root"),
        CancellationToken::new(),
        move |context| {
            let outer = context.endorse_registered_validations();
            lock(&context.task.validation_endorsements)[0]
                .identities
                .extend((0..ENDORSEMENTS).map(|identity| {
                    (
                        identity,
                        identity.wrapping_mul(17),
                        Revision::new(identity, identity.rotate_left(7)),
                    )
                }));
            let inner = context.endorse_registered_validations();

            assert!(context.task.validation_endorsed_identity(
                ENDORSEMENTS - 1,
                (ENDORSEMENTS - 1).wrapping_mul(17),
                Revision::new(ENDORSEMENTS - 1, (ENDORSEMENTS - 1).rotate_left(7),),
            ));
            assert_eq!(
                context
                    .task
                    .validation_endorsement_index_probes
                    .load(Ordering::Relaxed),
                1,
                "lookup work is one hash-index probe, not one visit per endorsement"
            );

            context.task.endorse_validation(&terminal);
            assert!(context.task.validation_endorsed_identity(
                terminal.node_incarnation,
                terminal.stamp,
                terminal.revision,
            ));
            drop(inner);
            assert!(context.task.validation_endorsed_identity(
                terminal.node_incarnation,
                terminal.stamp,
                terminal.revision,
            ));
            drop(outer);
            assert!(!context.task.validation_endorsed_identity(
                terminal.node_incarnation,
                terminal.stamp,
                terminal.revision,
            ));
            assert_eq!(
                context
                    .task
                    .validation_endorsement_index_probes
                    .load(Ordering::Relaxed),
                3,
                "nested scopes and scope teardown do not add index probes"
            );
            Ok(QueryOutput::success(1))
        },
    );
    assert_eq!(attempt.execution(), RequestExecution::Computed);
}

#[test]
fn registered_validation_endorsement_pins_exact_cone_and_is_lexically_scoped() {
    let runtime = QueryRuntime::new(1);
    let input = InputIdentity::new("source", "validation-endorsement");
    let first = Revision::new(50, 1);
    let second = Revision::new(51, 1);
    runtime
        .publish_revision(first, [(input.clone(), 7)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 7)])
        .unwrap();

    let input_for_c = input.clone();
    let c = runtime
        .family_with_evaluator::<Key, u64, _>("endorsement-c", 1, move |context, _, key| {
            context.input(input_for_c.clone())?;
            context.record_work(WorkItem::new("c-work", 1));
            Ok(QueryOutput::success(key.0.len() as u64))
        })
        .unwrap();
    let c_for_b = c.clone();
    let b = runtime
        .family_with_evaluator::<Key, u64, _>("endorsement-b", 1, move |context, _, key| {
            let c = context.query_registered(&c_for_b, key.clone())?;
            let QueryOutcome::Success(value) = c.outcome() else {
                unreachable!()
            };
            Ok(QueryOutput::success(*value + 1))
        })
        .unwrap();
    let b_for_a = b.clone();
    let a = runtime
        .family_with_evaluator::<Key, u64, _>("endorsement-a", 1, move |context, _, key| {
            let b = context.query_registered(&b_for_a, key.clone())?;
            let QueryOutcome::Success(value) = b.outcome() else {
                unreachable!()
            };
            Ok(QueryOutput::success(*value + 1))
        })
        .unwrap();
    runtime
        .request_registered(&a, first, Key("target"), CancellationToken::new())
        .into_result()
        .unwrap();

    let root = runtime.family::<Key, u64>("endorsement-root", 4).unwrap();
    let a_for_root = a.clone();
    let attempt = runtime.request(
        &root,
        second,
        Key("root"),
        CancellationToken::new(),
        move |context| {
            {
                let _endorsements = context.endorse_registered_validations();
                context.query_registered(&a_for_root, Key("target"))?;
                context.query_registered(&a_for_root, Key("noise"))?;
                assert!(
                    a_for_root.contains_retained_key(&Key("target")),
                    "the endorsed target survives retention-one churn"
                );
                context.query_registered(&a_for_root, Key("target"))?;
            }
            // The proof is lexical even though its safety pins may remain
            // in the task lease set until the rooted request ends.
            context.query_registered(&a_for_root, Key("target"))?;
            Ok(QueryOutput::success(1))
        },
    );
    assert_eq!(attempt.execution(), RequestExecution::Computed);
    assert!(
        attempt
            .dependencies()
            .iter()
            .all(|dependency| { dependency.node.family() == "endorsement-a" })
    );
    let target_counts = |family: &str| {
        attempt
            .nested_attempts()
            .iter()
            .filter(|nested| nested.node().family() == family && nested.node().key() == "target")
            .count()
    };
    assert_eq!(target_counts("endorsement-a"), 3);
    assert_eq!(
        target_counts("endorsement-b"),
        1,
        "the revision memo skips repeated B validation across A requests"
    );
    assert_eq!(
        target_counts("endorsement-c"),
        1,
        "the revision memo remains available after the endorsement scope"
    );
    assert!(
        attempt
            .nested_attempts()
            .iter()
            .filter(|nested| {
                nested.node().family() == "endorsement-a" && nested.node().key() == "target"
            })
            .all(|nested| {
                nested.execution() == RequestExecution::Reused && nested.work().is_empty()
            })
    );
    drop(attempt);
    assert!(a.retention().terminals <= a.retention().terminal_limit);
    assert!(b.retention().terminals <= b.retention().terminal_limit);
    assert!(c.retention().terminals <= c.retention().terminal_limit);
}

#[test]
fn registered_validation_endorsement_rejects_unregistered_cones_and_cancellation() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1), revision(2)]);
    let external = runtime
        .family::<Key, u64>("endorsement-external", 4)
        .unwrap();
    let external_for_registered = external.clone();
    let registered = runtime
        .family_with_evaluator::<Key, u64, _>("endorsement-tainted", 4, move |context, _, key| {
            let external = context.query(&external_for_registered, key.clone(), |_| {
                Ok(QueryOutput::success(3))
            })?;
            let QueryOutcome::Success(value) = external.outcome() else {
                unreachable!()
            };
            Ok(QueryOutput::success(*value))
        })
        .unwrap();
    runtime
        .request_registered(
            &registered,
            revision(1),
            Key("target"),
            CancellationToken::new(),
        )
        .into_result()
        .unwrap();

    let root = runtime
        .family::<Key, u64>("endorsement-tainted-root", 4)
        .unwrap();
    let registered_for_root = registered.clone();
    let cancellation = CancellationToken::new();
    let cancellation_for_root = cancellation.clone();
    let attempt = runtime.request(
        &root,
        revision(2),
        Key("root"),
        cancellation,
        move |context| {
            let _endorsements = context.endorse_registered_validations();
            let first = context.query_registered(&registered_for_root, Key("target"))?;
            assert!(
                !context.task.validation_endorsed(&first),
                "an unregistered dependency taints the enclosing proof"
            );
            let second = context.query_registered(&registered_for_root, Key("target"))?;
            assert!(
                !context.task.validation_endorsed(&second),
                "the tainted proof is never cached"
            );
            cancellation_for_root.cancel();
            assert!(matches!(
                context.query_registered(&registered_for_root, Key("target")),
                Err(QueryAbort::Canceled)
            ));
            Ok(QueryOutput::success(1))
        },
    );
    assert_eq!(attempt.execution(), RequestExecution::Aborted);
    assert_eq!(attempt.abort(), Some(&QueryAbort::Canceled));
}

#[test]
fn validation_only_computation_repairs_registered_endorsement() {
    let runtime = QueryRuntime::new(1);
    let input = InputIdentity::new("source", "endorsement-compute");
    let first = Revision::new(60, 1);
    let second = Revision::new(61, 1);
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 2)])
        .unwrap();
    let child_runs = Arc::new(AtomicUsize::new(0));
    let child_runs_for_evaluator = child_runs.clone();
    let input_for_child = input.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>(
            "endorsement-computed-child",
            1,
            move |context, _, _| {
                child_runs_for_evaluator.fetch_add(1, Ordering::SeqCst);
                context.input(input_for_child.clone())?;
                // Equal semantic output gives the successor the same green
                // stamp even though validation had to compute it.
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let child_for_parent = child.clone();
    let parent = runtime
        .family_with_evaluator::<Key, u64, _>(
            "endorsement-computed-parent",
            4,
            move |context, _, _| {
                context.query_registered(&child_for_parent, Key("child"))?;
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();
    runtime
        .request_registered(&parent, first, Key("parent"), CancellationToken::new())
        .into_result()
        .unwrap();

    let root = runtime
        .family::<Key, u64>("endorsement-computed-root", 4)
        .unwrap();
    let parent_for_root = parent.clone();
    runtime
        .request(
            &root,
            second,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _endorsements = context.endorse_registered_validations();
                let parent = context.query_registered(&parent_for_root, Key("parent"))?;
                assert!(
                    context.task.validation_endorsed(&parent),
                    "a published computed child allows one reuse traversal to certify its parent"
                );
                context
                    .retain_observed_terminal_cone(&parent)
                    .expect("the repair traversal leases the computed child's complete cone");
                Ok(QueryOutput::success(0))
            },
        )
        .into_result()
        .unwrap();
    assert_eq!(child_runs.load(Ordering::SeqCst), 2);
}

#[test]
fn validation_only_join_repairs_registered_endorsement() {
    let runtime = QueryRuntime::new(2);
    let input = InputIdentity::new("source", "endorsement-join");
    let first = Revision::new(70, 1);
    let second = Revision::new(71, 1);
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 2)])
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let barrier_for_child = barrier.clone();
    let input_for_child = input.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>(
            "endorsement-joined-child",
            2,
            move |context, _, _| {
                let stamp = context.input(input_for_child.clone())?;
                if stamp == 2 {
                    barrier_for_child.wait();
                    barrier_for_child.wait();
                }
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let child_for_parent = child.clone();
    let parent = runtime
        .family_with_evaluator::<Key, u64, _>(
            "endorsement-joined-parent",
            4,
            move |context, _, _| {
                context.query_registered(&child_for_parent, Key("child"))?;
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();
    runtime
        .request_registered(&parent, first, Key("parent"), CancellationToken::new())
        .into_result()
        .unwrap();

    let owner_runtime = runtime.clone();
    let owner_child = child.clone();
    let owner = thread::spawn(move || {
        owner_runtime.request_registered(
            &owner_child,
            second,
            Key("child"),
            CancellationToken::new(),
        )
    });
    barrier.wait();

    let waiter_runtime = runtime.clone();
    let waiter_parent = parent.clone();
    let waiter = thread::spawn(move || {
        let root = waiter_runtime
            .family::<Key, u64>("endorsement-joined-root", 4)
            .unwrap();
        waiter_runtime.request(
            &root,
            second,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _endorsements = context.endorse_registered_validations();
                let parent = context.query_registered(&waiter_parent, Key("parent"))?;
                assert!(
                    context.task.validation_endorsed(&parent),
                    "a published joined child allows one reuse traversal to certify its parent"
                );
                context
                    .retain_observed_terminal_cone(&parent)
                    .expect("the repair traversal leases the joined child's complete cone");
                Ok(QueryOutput::success(0))
            },
        )
    });
    runtime.wait_for_metrics(|metrics| metrics.joins >= 1);
    barrier.wait();
    assert_eq!(
        owner.join().unwrap().execution(),
        RequestExecution::Computed
    );
    assert_eq!(
        waiter.join().unwrap().execution(),
        RequestExecution::Computed
    );
}

#[test]
fn observed_join_reacquires_registered_cone_before_promotion() {
    let runtime = QueryRuntime::new(2);
    let current = revision(1);
    publish_empty(&runtime, [current]);

    let child = runtime
        .family_with_evaluator::<Key, u64, _>("observed-join-cone-child", 2, |_, _, _| {
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let barrier_for_parent = barrier.clone();
    let child_for_parent = child.clone();
    let parent = runtime
        .family_with_evaluator::<Key, u64, _>(
            "observed-join-cone-parent",
            2,
            move |context, _, _| {
                context.query_registered(&child_for_parent, Key("child"))?;
                barrier_for_parent.wait();
                barrier_for_parent.wait();
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();

    let owner_runtime = runtime.clone();
    let owner_parent = parent.clone();
    let owner = thread::spawn(move || {
        owner_runtime.request_registered(
            &owner_parent,
            current,
            Key("parent"),
            CancellationToken::new(),
        )
    });
    barrier.wait();

    let root = runtime
        .family::<Key, u64>("observed-join-cone-root", 2)
        .unwrap();
    let waiter_runtime = runtime.clone();
    let waiter_parent = parent.clone();
    let waiter = thread::spawn(move || {
        waiter_runtime.request(
            &root,
            current,
            Key("root"),
            CancellationToken::new(),
            move |context| {
                let _endorsements = context.endorse_registered_validations();
                let parent = context.query_registered(&waiter_parent, Key("parent"))?;
                context
                    .retain_observed_terminal_cone(&parent)
                    .expect("an observed join reacquires its owner's descendant cone");
                Ok(QueryOutput::success(0))
            },
        )
    });
    runtime.wait_for_metrics(|metrics| metrics.joins >= 1);
    barrier.wait();

    assert_eq!(
        owner.join().unwrap().execution(),
        RequestExecution::Computed
    );
    assert_eq!(
        waiter.join().unwrap().execution(),
        RequestExecution::Computed
    );
}

#[test]
fn endorsement_hits_preserve_handoff_commit_and_abort_lifecycle() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let commits_for_child = commits.clone();
    let aborts_for_child = aborts.clone();
    let child = runtime
        .family_with_evaluator::<Key, u64, _>(
            "endorsement-handoff-child",
            1,
            move |context, _, _| {
                context.register_attempt_handoff(CountingHandoff {
                    commits: commits_for_child.clone(),
                    aborts: aborts_for_child.clone(),
                });
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let seed = runtime
        .family::<Key, u64>("endorsement-handoff-seed", 4)
        .unwrap();

    let seed_pending = |key: Key| {
        let child = child.clone();
        let attempt = runtime.request(
            &seed,
            revision(1),
            key.clone(),
            CancellationToken::new(),
            move |context| {
                context.query_registered(&child, key)?;
                Err(QueryAbort::Canceled)
            },
        );
        assert_eq!(attempt.execution(), RequestExecution::Aborted);
    };
    seed_pending(Key("commit"));
    assert_eq!(commits.load(Ordering::SeqCst), 0);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);

    let child_for_commit = child.clone();
    let committed = runtime.request(
        &seed,
        revision(1),
        Key("commit-root"),
        CancellationToken::new(),
        move |context| {
            let _endorsements = context.endorse_registered_validations();
            context.query_registered(&child_for_commit, Key("commit"))?;
            context.query_registered(&child_for_commit, Key("commit"))?;
            Ok(QueryOutput::success(1))
        },
    );
    assert_eq!(committed.execution(), RequestExecution::Computed);
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);

    seed_pending(Key("abort"));
    let child_for_abort = child.clone();
    let aborted = runtime.request(
        &seed,
        revision(1),
        Key("abort-root"),
        CancellationToken::new(),
        move |context| {
            let _endorsements = context.endorse_registered_validations();
            context.query_registered(&child_for_abort, Key("abort"))?;
            context.query_registered(&child_for_abort, Key("abort"))?;
            Err(QueryAbort::Canceled)
        },
    );
    assert_eq!(aborted.execution(), RequestExecution::Aborted);
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);
    drop(aborted);

    let churn =
        runtime.request_registered(&child, revision(1), Key("churn"), CancellationToken::new());
    assert_eq!(churn.execution(), RequestExecution::Computed);
    assert_eq!(commits.load(Ordering::SeqCst), 2);
    assert_eq!(
        aborts.load(Ordering::SeqCst),
        1,
        "eviction aborts the still-pending endorsement-hit lifecycle"
    );
}

#[test]
fn registered_same_family_cycle_aborts_recovers_and_does_not_retain_itself() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("registered-self-cycle", 4, |context, family, key| {
            match key.0 {
                "left" => {
                    context.query_registered(family, Key("right"))?;
                    Ok(QueryOutput::success(1))
                }
                "right" => {
                    context.query_registered(family, Key("left"))?;
                    Ok(QueryOutput::success(2))
                }
                "recovery" => Ok(QueryOutput::success(3)),
                _ => unreachable!(),
            }
        })
        .unwrap();
    let weak = family.downgrade();

    let cycle =
        runtime.request_registered(&family, revision(1), Key("left"), CancellationToken::new());
    assert!(matches!(cycle.abort(), Some(QueryAbort::Cycle(_))));
    assert!(
        cycle
            .nested_attempts()
            .iter()
            .any(|attempt| attempt.execution() == RequestExecution::Aborted)
    );

    let recovery = runtime.request_registered(
        &family,
        revision(1),
        Key("recovery"),
        CancellationToken::new(),
    );
    assert_eq!(recovery.execution(), RequestExecution::Computed);
    assert_eq!(
        recovery.terminal().unwrap().outcome(),
        &QueryOutcome::Success(3)
    );

    drop(recovery);
    drop(cycle);
    drop(family);
    assert!(weak.upgrade().is_none());
}

#[test]
fn aborted_requests_retain_nested_dependency_input_and_work_prefixes() {
    let runtime = QueryRuntime::new(1);
    let grandchild = runtime.family::<Key, u64>("abort-grandchild", 4).unwrap();
    let child = runtime.family::<Key, u64>("abort-child", 4).unwrap();
    let root = runtime.family::<Key, u64>("abort-root", 4).unwrap();
    let input = InputIdentity::new("source", "abort");
    let revision = Revision::new(20, 1);
    runtime
        .publish_revision(revision, [(input.clone(), 7)])
        .unwrap();

    let nested_child = child.clone();
    let nested_grandchild = grandchild.clone();
    let attempt = runtime.request(
        &root,
        revision,
        Key("root"),
        CancellationToken::new(),
        |context| {
            context.query(&nested_child, Key("child"), |context| {
                context.input(input)?;
                context.query(&nested_grandchild, Key("grandchild"), |_| {
                    Ok(QueryOutput::success(1).with_work(vec![WorkItem::new("grandchild", 2)]))
                })?;
                context.record_work(WorkItem::new("child-prefix", 3));
                Err(QueryAbort::Canceled)
            })?;
            unreachable!("nested abort propagates")
        },
    );
    assert_eq!(attempt.execution(), RequestExecution::Aborted);
    assert_eq!(attempt.abort(), Some(&QueryAbort::Canceled));
    assert_eq!(attempt.dependencies().len(), 1);
    assert_eq!(
        attempt.inputs(),
        &[InputObservation {
            input: InputIdentity::new("source", "abort"),
            stamp: 7
        }]
    );
    assert_eq!(
        attempt.work(),
        &[
            (Arc::<str>::from("child-prefix"), 3),
            (Arc::<str>::from("grandchild"), 2),
        ]
    );
    assert_eq!(attempt.nested_attempts().len(), 2);
    assert_eq!(
        attempt.nested_attempts()[0].execution(),
        RequestExecution::Computed
    );
    assert_eq!(
        attempt.nested_attempts()[1].execution(),
        RequestExecution::Aborted
    );
    assert_eq!(
        attempt.nested_attempts()[1].abort(),
        Some(&QueryAbort::Canceled)
    );
    assert_eq!(
        attempt.nested_attempts()[1].inputs(),
        &[InputObservation {
            input: InputIdentity::new("source", "abort"),
            stamp: 7
        }]
    );
}

#[test]
fn revision_store_exact_reads_can_overlap() {
    let runtime = QueryRuntime::new(1);
    let revision = Revision::new(1, 7);
    let input = InputIdentity::new("source", "concurrent-revision-reads");
    runtime
        .publish_revision(revision, [(input.clone(), 11)])
        .unwrap();

    let first_store = read(&runtime.core.revisions);
    assert_eq!(first_store.input_stamp(revision.id, &input), Some(11));
    let second_store = runtime
        .core
        .revisions
        .try_read()
        .expect("a second exact revision-store read must not wait for the first");
    assert_eq!(second_store.input_stamp(revision.id, &input), Some(11));
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

#[test]
fn optional_inputs_record_negative_observations_and_invalidate_on_publication() {
    let runtime = QueryRuntime::new(1);
    let family = runtime.family::<Key, bool>("optional-input", 4).unwrap();
    let input = InputIdentity::new("candidate", "helper.rue");
    let absent = revision(20);
    let present = revision(21);
    let absent_again = revision(22);
    runtime.publish_revision(absent, []).unwrap();
    runtime
        .publish_revision(present, [(input.clone(), 7)])
        .unwrap();
    runtime.publish_revision(absent_again, []).unwrap();

    let first = runtime.request(
        &family,
        absent,
        Key("helper"),
        CancellationToken::new(),
        |context| {
            Ok(QueryOutput::success(
                context.optional_input(input.clone()).is_some(),
            ))
        },
    );
    assert_eq!(first.execution(), RequestExecution::Computed);
    assert_eq!(
        first.inputs(),
        &[InputObservation {
            input: input.clone(),
            stamp: 0,
        }]
    );

    let changed = runtime.request(
        &family,
        present,
        Key("helper"),
        CancellationToken::new(),
        |context| {
            Ok(QueryOutput::success(
                context.optional_input(input.clone()).is_some(),
            ))
        },
    );
    assert_eq!(changed.execution(), RequestExecution::Computed);
    assert!(matches!(
        changed.terminal().unwrap().outcome(),
        QueryOutcome::Success(true)
    ));

    let absent_result = runtime.request(
        &family,
        absent_again,
        Key("helper"),
        CancellationToken::new(),
        |context| {
            Ok(QueryOutput::success(
                context.optional_input(input.clone()).is_some(),
            ))
        },
    );
    assert!(matches!(
        absent_result.terminal().unwrap().outcome(),
        QueryOutcome::Success(false)
    ));
    assert_eq!(
        runtime.publish_revision(revision(23), [(input.clone(), 0)]),
        Err(RevisionError::ReservedInputStamp(input))
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
