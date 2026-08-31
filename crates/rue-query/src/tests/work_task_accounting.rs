//! Query runtime tests: work task accounting subsystem.

use super::fixtures::*;
use super::*;

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
    assert_eq!(serial_metrics.batch_worker_slots_requested, 0);
    assert_eq!(serial_metrics.batch_worker_slots_granted, 0);
    assert_eq!(serial_metrics.batch_worker_lanes_entered, 0);
    assert_eq!(serial_metrics.batch_worker_thread_births, 0);
    assert_eq!(serial_metrics.batch_worker_coordinator_residual_ns, 0);
    assert_eq!(parallel_metrics.donated_permits, 1);
    assert_eq!(parallel_metrics.ready_items, 2);
    assert_eq!(parallel_metrics.batch_worker_slots_requested, 1);
    assert_eq!(parallel_metrics.batch_worker_slots_granted, 1);
    assert_eq!(parallel_metrics.batch_worker_lanes_entered, 2);
    assert_eq!(parallel_metrics.batch_worker_thread_births, 1);
    assert!(parallel_metrics.batch_worker_coordinator_residual_ns > 0);
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
