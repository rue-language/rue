//! Registered-batch execution tests.

use std::hash::Hash;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use super::*;

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

fn revision(id: u64) -> Revision {
    Revision::new(id, id)
}

fn publish_empty(runtime: &QueryRuntime, revisions: impl IntoIterator<Item = Revision>) {
    for revision in revisions {
        runtime.publish_revision(revision, []).unwrap();
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
struct PinSetHandoff {
    pins: Option<RetainedPinSet>,
    committed: Arc<Mutex<Option<RetainedPinSet>>>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl QueryAttemptHandoff for PinSetHandoff {
    fn commit(&mut self) {
        *lock(&self.committed) = self.pins.take();
        lock(&self.events).push("commit");
    }

    fn abort(&mut self) {
        if self.pins.is_none() {
            self.pins = lock(&self.committed).take();
        }
        lock(&self.events).push("abort");
    }
}

fn run_registered_batch(worker_count: usize) -> QueryRequestAttempt<Arc<[u64]>> {
    let runtime = QueryRuntime::new(worker_count);
    publish_empty(&runtime, [revision(1)]);
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let rendezvous = (worker_count > 1).then(|| Arc::new(Barrier::new(worker_count.min(4))));
    let active_for_child = active.clone();
    let peak_for_child = peak.clone();
    let rendezvous_for_child = rendezvous.clone();
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>("registered-batch-child", 8, move |_, _, key| {
            let now = active_for_child.fetch_add(1, Ordering::AcqRel) + 1;
            peak_for_child.fetch_max(now, Ordering::AcqRel);
            if let Some(rendezvous) = &rendezvous_for_child {
                rendezvous.wait();
                thread::sleep(Duration::from_millis((3 - key.0) * 2));
            }
            active_for_child.fetch_sub(1, Ordering::AcqRel);
            Ok(QueryOutput::success(key.0)
                .with_diagnostics(vec![QueryDiagnostic::new(
                    format!("diagnostic-{}", key.0),
                    format!("payload-{}", key.0),
                    None,
                )])
                .with_work(vec![WorkItem::new("batch-child", 1)]))
        })
        .unwrap();
    let child_for_root = child.clone();
    let root = runtime
        .family_with_evaluator::<Key, Arc<[u64]>, _>(
            "registered-batch-root",
            8,
            move |context, _, _| {
                let terminals =
                    context.query_registered_batch(&child_for_root, (0..4).map(Slot))?;
                let mut values = Vec::new();
                let mut diagnostics = Vec::new();
                for terminal in terminals {
                    let QueryOutcome::Success(value) = terminal.outcome() else {
                        unreachable!()
                    };
                    values.push(*value);
                    diagnostics.extend_from_slice(terminal.diagnostics());
                }
                Ok(QueryOutput::success(Arc::from(values)).with_diagnostics(diagnostics))
            },
        )
        .unwrap();
    let attempt =
        runtime.request_registered(&root, revision(1), Key("root"), CancellationToken::new());
    let metrics = runtime.metrics();
    assert_eq!(metrics.batch_worker_slots_requested, 3);
    assert_eq!(
        metrics.batch_worker_slots_granted,
        worker_count.saturating_sub(1).min(3) as u64
    );
    assert_eq!(
        metrics.batch_worker_lanes_entered,
        metrics.batch_worker_slots_granted + 1,
        "every granted logical worker slot and the donating parent lane must execute"
    );
    assert_eq!(
        metrics.batch_worker_thread_births, metrics.batch_worker_slots_granted,
        "the first batch creates exactly one reusable OS worker per admitted extra lane"
    );
    if metrics.batch_worker_thread_births == 0 {
        assert_eq!(metrics.batch_worker_coordinator_residual_ns, 0);
    } else {
        assert!(
            metrics.batch_worker_coordinator_residual_ns > 0,
            "worker construction and batch dispatch must have observable coordinator latency"
        );
    }
    assert_eq!(
        peak.load(Ordering::Acquire),
        worker_count.min(4),
        "the registered batch must use the runtime's shared permit budget"
    );
    attempt
}

#[test]
fn reusable_worker_births_do_not_repeat_across_registered_batches() {
    let runtime = QueryRuntime::new(2);
    let startup = runtime.metrics();
    assert_eq!(startup.batch_worker_thread_births, 0);
    assert_eq!(startup.batch_worker_coordinator_residual_ns, 0);
    publish_empty(&runtime, [revision(1)]);
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>("reused-worker-child", 8, |_, _, key| {
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let child_for_root = child.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("reused-worker-root", 8, move |context, _, _| {
            let mut sum = 0;
            for keys in [[Slot(1), Slot(2)], [Slot(3), Slot(4)]] {
                for terminal in context.query_registered_batch(&child_for_root, keys)? {
                    let QueryOutcome::Success(value) = terminal.outcome() else {
                        unreachable!()
                    };
                    sum += *value;
                }
            }
            Ok(QueryOutput::success(sum))
        })
        .unwrap();

    let result = runtime
        .request_registered(&root, revision(1), Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
    assert_eq!(result.outcome(), &QueryOutcome::Success(10));
    let metrics = runtime.metrics();
    assert_eq!(metrics.batch_worker_slots_requested, 2);
    assert_eq!(metrics.batch_worker_slots_granted, 2);
    assert_eq!(metrics.batch_worker_lanes_entered, 4);
    assert_eq!(
        metrics.batch_worker_thread_births, 1,
        "both batches must reuse the runtime's one physical worker"
    );
    assert!(
        metrics.batch_worker_coordinator_residual_ns > startup.batch_worker_coordinator_residual_ns
    );
}

#[test]
fn registered_batch_one_and_many_permits_reduce_adversarial_completion_stably() {
    let one = run_registered_batch(1);
    let many = run_registered_batch(4);
    let one_terminal = one.terminal().unwrap();
    let many_terminal = many.terminal().unwrap();
    assert_eq!(one_terminal.outcome(), many_terminal.outcome());
    assert_eq!(one_terminal.diagnostics(), many_terminal.diagnostics());
    assert_eq!(one_terminal.work(), many_terminal.work());
    assert_eq!(
        one.dependencies()
            .iter()
            .map(|dependency| dependency.node.key())
            .collect::<Vec<_>>(),
        many.dependencies()
            .iter()
            .map(|dependency| dependency.node.key())
            .collect::<Vec<_>>()
    );
    for attempt in [&one, &many] {
        assert_eq!(
            attempt
                .nested_attempts()
                .iter()
                .map(|nested| nested.node().key())
                .collect::<Vec<_>>(),
            vec!["0", "1", "2", "3"]
        );
    }
}

#[test]
fn registered_batch_children_cache_exact_terminal_reobservations() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("lease-metrics-leaf", 8, |_, _, _| {
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let leaf_for_branch = leaf.clone();
    let branch = runtime
        .family_with_evaluator::<Slot, u64, _>("lease-metrics-branch", 8, move |context, _, key| {
            context.query_registered(&leaf_for_branch, Key("shared"))?;
            context.query_registered(&leaf_for_branch, Key("shared"))?;
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let branch_for_root = branch.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("lease-metrics-root", 8, move |context, _, _| {
            context.query_registered_batch(&branch_for_root, [Slot(0), Slot(1)])?;
            Ok(QueryOutput::success(0))
        })
        .unwrap();

    let before = runtime.metrics().validation;
    let attempt =
        runtime.request_registered(&root, revision(1), Key("root"), CancellationToken::new());
    assert_eq!(attempt.execution(), RequestExecution::Computed);
    let work = runtime.metrics().validation.saturating_sub(before);

    assert_eq!(work.terminal_lease_observations, 5);
    assert_eq!(work.duplicate_terminal_lease_observations, 0);
    let leaf_executions = attempt
        .nested_attempts()
        .iter()
        .filter(|nested| nested.node().family() == "lease-metrics-leaf")
        .map(NestedQueryAttempt::execution)
        .collect::<Vec<_>>();
    assert_eq!(leaf_executions.len(), 4, "{leaf_executions:?}");
    assert_eq!(leaf_executions[1], RequestExecution::Reused);
    assert_eq!(leaf_executions[3], RequestExecution::Reused);
    assert!(leaf_executions.iter().all(|execution| matches!(
        execution,
        RequestExecution::Computed | RequestExecution::Joined | RequestExecution::Reused
    )));
}

#[derive(Debug, PartialEq, Eq)]
struct SharedValidationSnapshot {
    execution: RequestExecution,
    outcome: u64,
    nested: Vec<(String, RequestExecution, Option<QueryAbort>)>,
    leaf_runs: usize,
    branch_runs: usize,
    root_runs: usize,
}

fn run_parallel_shared_dependency_validation(worker_count: usize) -> SharedValidationSnapshot {
    let runtime = QueryRuntime::new(worker_count);
    let first = Revision::new(1, 7);
    let second = Revision::new(2, 7);
    let leaf_input = InputIdentity::new("source", "parallel-validation-leaf");
    let root_input = InputIdentity::new("source", "parallel-validation-root");
    runtime
        .publish_revision(first, [(leaf_input.clone(), 1), (root_input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(leaf_input.clone(), 1), (root_input.clone(), 2)])
        .unwrap();

    let leaf_runs = Arc::new(AtomicUsize::new(0));
    let leaf_runs_for_evaluator = leaf_runs.clone();
    let leaf_input_for_evaluator = leaf_input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>(
            "parallel-validation-leaf",
            8,
            move |context, _, _| {
                leaf_runs_for_evaluator.fetch_add(1, Ordering::Relaxed);
                context.input(leaf_input_for_evaluator.clone())?;
                Ok(QueryOutput::success(10))
            },
        )
        .unwrap();

    let branch_runs = Arc::new(AtomicUsize::new(0));
    let branch_runs_for_evaluator = branch_runs.clone();
    let leaf_for_branch = leaf.clone();
    let branch = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "parallel-validation-branch",
            8,
            move |context, _, key| {
                branch_runs_for_evaluator.fetch_add(1, Ordering::Relaxed);
                let leaf = context.query_registered(&leaf_for_branch, Key("leaf"))?;
                let QueryOutcome::Success(value) = leaf.outcome() else {
                    unreachable!()
                };
                Ok(QueryOutput::success(*value + key.0))
            },
        )
        .unwrap();

    let root_runs = Arc::new(AtomicUsize::new(0));
    let root_runs_for_evaluator = root_runs.clone();
    let root_input_for_evaluator = root_input.clone();
    let branch_for_root = branch.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "parallel-validation-root",
            8,
            move |context, _, _| {
                root_runs_for_evaluator.fetch_add(1, Ordering::Relaxed);
                context.input(root_input_for_evaluator.clone())?;
                let branches =
                    context.query_registered_batch(&branch_for_root, [Slot(0), Slot(1)])?;
                let sum = branches
                    .iter()
                    .map(|terminal| match terminal.outcome() {
                        QueryOutcome::Success(value) => *value,
                        QueryOutcome::Failure(_) => unreachable!(),
                    })
                    .sum();
                Ok(QueryOutput::success(sum))
            },
        )
        .unwrap();

    let initial = runtime.request_registered(&root, first, Key("root"), CancellationToken::new());
    assert_eq!(initial.execution(), RequestExecution::Computed);
    assert_eq!(
        initial.terminal().unwrap().outcome(),
        &QueryOutcome::Success(21)
    );
    drop(initial);

    let validated =
        runtime.request_registered(&root, second, Key("root"), CancellationToken::new());
    assert_eq!(
        validated.abort(),
        None,
        "shared validation must not report a cycle"
    );
    let QueryOutcome::Success(outcome) = validated.terminal().unwrap().outcome() else {
        unreachable!()
    };
    let snapshot = SharedValidationSnapshot {
        execution: validated.execution(),
        outcome: *outcome,
        nested: validated
            .nested_attempts()
            .iter()
            .map(|attempt| {
                (
                    format!("{}:{}", attempt.node().family(), attempt.node().key()),
                    attempt.execution(),
                    attempt.abort().cloned(),
                )
            })
            .collect(),
        leaf_runs: leaf_runs.load(Ordering::Relaxed),
        branch_runs: branch_runs.load(Ordering::Relaxed),
        root_runs: root_runs.load(Ordering::Relaxed),
    };
    drop(validated);
    drop(root);
    drop(branch);
    drop(leaf);
    assert_eq!(
        read(&runtime.core.nodes).len(),
        0,
        "parallel validation must leave no registry entries after family teardown"
    );
    snapshot
}

#[test]
fn registered_batch_shared_dependency_validation_matches_one_and_many_workers() {
    let one = run_parallel_shared_dependency_validation(1);
    let many = run_parallel_shared_dependency_validation(4);

    // A parallel sibling may publish the shared leaf's current certificate
    // before the other sibling reaches it, so the operational ledger can
    // contain one or two leaf validation demands. The semantic result and
    // the two branch reuses are schedule-independent.
    for snapshot in [&one, &many] {
        assert_eq!(snapshot.execution, RequestExecution::Computed);
        assert_eq!(snapshot.outcome, 21);
        assert_eq!(snapshot.leaf_runs, 1, "the shared leaf remains green");
        assert_eq!(
            snapshot.branch_runs, 2,
            "both branches validate without recomputing"
        );
        assert_eq!(
            snapshot.root_runs, 2,
            "the changed root input forces one recomputation"
        );
        assert_eq!(
            snapshot
                .nested
                .iter()
                .filter(|(node, execution, abort)| {
                    node.starts_with("parallel-validation-branch:")
                        && *execution == RequestExecution::Reused
                        && abort.is_none()
                })
                .count(),
            2
        );
        assert!(snapshot.nested.iter().all(|(_, _, abort)| abort.is_none()));
    }
}

#[test]
fn later_batch_sibling_borrows_the_completed_siblings_registered_proof() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(1, 9);
    let second = Revision::new(2, 9);
    let leaf_input = InputIdentity::new("source", "batch-authority-leaf");
    let root_input = InputIdentity::new("source", "batch-authority-root");
    runtime
        .publish_revision(first, [(leaf_input.clone(), 1), (root_input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(leaf_input.clone(), 1), (root_input.clone(), 2)])
        .unwrap();

    let leaf_input_for_evaluator = leaf_input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("batch-authority-leaf", 8, move |context, _, _| {
            Ok(QueryOutput::success(
                context.input(leaf_input_for_evaluator.clone())?,
            ))
        })
        .unwrap();
    let leaf_for_branch = leaf.clone();
    let branch = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "batch-authority-branch",
            8,
            move |context, _, key| {
                let leaf = context.query_registered(&leaf_for_branch, Key("shared"))?;
                let QueryOutcome::Success(value) = leaf.outcome() else {
                    unreachable!()
                };
                Ok(QueryOutput::success(*value + key.0))
            },
        )
        .unwrap();
    let branch_for_root = branch.clone();
    let root_input_for_evaluator = root_input.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("batch-authority-root", 8, move |context, _, _| {
            let root_stamp = context.input(root_input_for_evaluator.clone())?;
            let _proof = context.endorse_registered_validations();
            let branches = context.query_registered_batch(&branch_for_root, [Slot(0), Slot(1)])?;
            let retained = context
                .retain_observed_terminal_cones_from(&branches, &[])
                .expect("the joined batch transfers its complete shared proof cone");
            let sum = branches
                .iter()
                .map(|terminal| match terminal.outcome() {
                    QueryOutcome::Success(value) => *value,
                    QueryOutcome::Failure(_) => unreachable!(),
                })
                .sum::<u64>();
            drop(retained);
            Ok(QueryOutput::success(sum + root_stamp))
        })
        .unwrap();

    let initial = runtime.request_registered(&root, first, Key("root"), CancellationToken::new());
    assert_eq!(
        initial.terminal().unwrap().outcome(),
        &QueryOutcome::Success(4)
    );
    drop(initial);

    let before = runtime.metrics().validation;
    let current = runtime.request_registered(&root, second, Key("root"), CancellationToken::new());
    assert_eq!(
        current.terminal().unwrap().outcome(),
        &QueryOutcome::Success(5)
    );
    let work = runtime.metrics().validation.saturating_sub(before);
    assert_eq!(work.proof_reacquisition_misses, 0);
    assert_eq!(
        work.demands, 1,
        "the first sibling validates the shared leaf once; the later sibling borrows that proof"
    );
    assert!(work.endorsement_hits > 0);
}

#[test]
fn nested_batch_sibling_borrows_the_enclosing_batches_registered_proof() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(1, 9);
    let second = Revision::new(2, 9);
    let leaf_input = InputIdentity::new("source", "nested-batch-authority-leaf");
    let root_input = InputIdentity::new("source", "nested-batch-authority-root");
    runtime
        .publish_revision(first, [(leaf_input.clone(), 1), (root_input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(leaf_input.clone(), 1), (root_input.clone(), 2)])
        .unwrap();

    let leaf_input_for_evaluator = leaf_input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>(
            "nested-batch-authority-leaf",
            8,
            move |context, _, _| {
                Ok(QueryOutput::success(
                    context.input(leaf_input_for_evaluator.clone())?,
                ))
            },
        )
        .unwrap();
    let leaf_for_inner = leaf.clone();
    let inner = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "nested-batch-authority-inner",
            8,
            move |context, _, key| {
                let leaf = context.query_registered(&leaf_for_inner, Key("shared"))?;
                let QueryOutcome::Success(value) = leaf.outcome() else {
                    unreachable!()
                };
                Ok(QueryOutput::success(*value + key.0))
            },
        )
        .unwrap();
    let inner_for_outer = inner.clone();
    let outer = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "nested-batch-authority-outer",
            8,
            move |context, _, key| {
                let inner = context
                    .query_registered_batch(&inner_for_outer, [key.clone()])?
                    .pop()
                    .unwrap();
                let QueryOutcome::Success(value) = inner.outcome() else {
                    unreachable!()
                };
                Ok(QueryOutput::success(*value))
            },
        )
        .unwrap();
    let outer_for_root = outer.clone();
    let root_input_for_evaluator = root_input.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "nested-batch-authority-root",
            8,
            move |context, _, _| {
                context.input(root_input_for_evaluator.clone())?;
                let _proof = context.endorse_registered_validations();
                let branches =
                    context.query_registered_batch(&outer_for_root, [Slot(0), Slot(1)])?;
                let retained = context
                    .retain_observed_terminal_cones_from(&branches, &[])
                    .expect("nested batches transfer their complete shared proof cone");
                drop(retained);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();

    runtime
        .request_registered(&root, first, Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
    let before = runtime.metrics().validation;
    runtime
        .request_registered(&root, second, Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
    let work = runtime.metrics().validation.saturating_sub(before);
    assert_eq!(work.proof_reacquisition_misses, 0);
    assert_eq!(
        work.demands, 3,
        "each retained outer/inner root validates, but the later nested batch borrows the shared leaf proof"
    );
}

fn run_stale_reverse_dependency_validation(worker_count: usize) {
    let runtime = QueryRuntime::new(worker_count);
    let first = Revision::new(3, 11);
    let second = Revision::new(4, 11);
    let shape_input = InputIdentity::new("source", "stale-reverse-dependency-shape");
    runtime
        .publish_revision(first, [(shape_input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(shape_input.clone(), 2)])
        .unwrap();

    // Keep the graph family ordered before the shape family. Retained
    // dependency validation is canonical rather than evaluation ordered,
    // so the stale reverse graph edge is checked before the changed shape
    // edge which would otherwise dirty the predecessor immediately.
    let shape_input_for_evaluator = shape_input.clone();
    let shapes = runtime
        .family_with_evaluator::<Key, u64, _>("z-stale-reverse-shape", 8, move |context, _, _| {
            Ok(QueryOutput::success(
                context.input(shape_input_for_evaluator.clone())?,
            ))
        })
        .unwrap();
    let graph_slot = Arc::new(std::sync::OnceLock::<QueryFamily<Key, u64>>::new());
    let graph_slot_for_evaluator = graph_slot.clone();
    let shapes_for_graph = shapes.clone();
    let graph = runtime
        .family_with_evaluator::<Key, u64, _>("a-stale-reverse-graph", 8, move |context, _, key| {
            let shape = context.query_registered(&shapes_for_graph, key.clone())?;
            let QueryOutcome::Success(shape) = shape.outcome() else {
                unreachable!()
            };
            let dependency = match (key.0, *shape) {
                ("a", 1) | ("b", 2) => None,
                ("b", 1) => Some(Key("a")),
                ("a", 2) => Some(Key("b")),
                _ => unreachable!(),
            };
            if let Some(dependency) = dependency {
                context.query_registered_batch(
                    graph_slot_for_evaluator.get().unwrap(),
                    [dependency],
                )?;
            }
            Ok(QueryOutput::success(*shape))
        })
        .unwrap();
    graph_slot.set(graph.clone()).unwrap();

    // Revision one is B -> A. Revision two reverses the legal graph to
    // A -> B. While computing the new A, validating old B observes its
    // stale B -> A edge before B's changed shape. That speculative edge
    // must dirty B, whose current leaf evaluation then lets A finish.
    runtime
        .request_registered(&graph, first, Key("b"), CancellationToken::new())
        .into_result()
        .unwrap();
    let current = runtime.request_registered(&graph, second, Key("a"), CancellationToken::new());
    assert_eq!(
        current.abort(),
        None,
        "a stale predecessor edge is not a current dependency cycle"
    );
    assert_eq!(
        current.terminal().unwrap().outcome(),
        &QueryOutcome::Success(2)
    );
    assert_eq!(
        runtime.metrics().cycles,
        0,
        "speculative validation cycles are not reported as current cycles"
    );
}

#[test]
fn stale_reverse_dependency_validation_recomputes_with_one_and_many_workers() {
    for worker_count in [1, 4] {
        run_stale_reverse_dependency_validation(worker_count);
    }
}

/// A retained dependency observation names one exact node incarnation, but
/// re-demand resolves the family memo by key. Retention retires a node from
/// that memo as soon as it holds no terminals and no live users, and the
/// next request for the key builds a fresh incarnation whose stamps restart
/// at one. Meanwhile the validation walk holds the retired incarnation alive
/// through the incarnation registry, so both nodes exist and the fresh
/// node's first stamp is numerically equal to the retained observation's.
///
/// Comparing stamps alone therefore certifies a dependent against a
/// different node's computation, which reuses a stale value outright. The
/// interposition retires the child's incarnation at exactly the instant the
/// walk is about to re-demand it, which is the window that concurrent
/// retention hits on its own under load.
fn run_superseded_dependency_incarnation(worker_count: usize) {
    let runtime = QueryRuntime::new(worker_count);
    let input = InputIdentity::new("source", "superseded-incarnation");
    let first = Revision::new(1, 7);
    let second = Revision::new(2, 7);
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 2)])
        .unwrap();

    // One retained terminal: publishing any second key in this family
    // retires the first key's node from the family memo.
    let input_for_child = input.clone();
    let children = runtime
        .family_with_evaluator::<Key, u64, _>("superseded-child", 1, move |context, _, _| {
            Ok(QueryOutput::success(
                context.input(input_for_child.clone())?,
            ))
        })
        .unwrap();
    let children_for_parent = children.clone();
    let parents = runtime
        .family_with_evaluator::<Key, u64, _>("superseded-parent", 8, move |context, _, _| {
            let child = context.query_registered(&children_for_parent, Key("child"))?;
            let QueryOutcome::Success(child) = child.outcome() else {
                unreachable!()
            };
            Ok(QueryOutput::success(*child))
        })
        .unwrap();

    let observe = |revision| {
        let attempt =
            runtime.request_registered(&parents, revision, Key("root"), CancellationToken::new());
        assert_eq!(attempt.abort(), None);
        let QueryOutcome::Success(value) = *attempt.terminal().unwrap().outcome() else {
            unreachable!()
        };
        value
    };

    assert_eq!(observe(first), 1);

    let retire = Arc::new(AtomicBool::new(false));
    let retire_for_hook = retire.clone();
    let runtime_for_hook = runtime.clone();
    let children_for_hook = children.clone();
    runtime.set_interpose(Arc::new(move |site| {
        if site != InterposeSite::RetainedDependencyDemand
            || retire_for_hook.swap(true, Ordering::SeqCst)
        {
            return;
        }
        // Publishing a second key evicts the child's only retained terminal,
        // which retires its node from the family memo while this walk still
        // holds that exact incarnation alive.
        runtime_for_hook
            .request_registered(
                &children_for_hook,
                second,
                Key("filler"),
                CancellationToken::new(),
            )
            .into_result()
            .unwrap();
        // Rebuild the key as a fresh incarnation whose first stamp reads the
        // same as the retained observation's. Re-demand then finds a
        // publishable terminal and reuses it, which is what lets a bare
        // stamp comparison certify the dependent against the wrong node.
        runtime_for_hook
            .request_registered(
                &children_for_hook,
                second,
                Key("child"),
                CancellationToken::new(),
            )
            .into_result()
            .unwrap();
    }));

    let observed = observe(second);
    runtime.clear_interpose();
    assert!(
        retire.load(Ordering::SeqCst),
        "the retained dependency was re-demanded, so the window was exercised"
    );
    assert_eq!(
        observed, 2,
        "a superseded incarnation cannot certify a dependent, whatever its stamp reads"
    );
    assert!(
        runtime.metrics().validation.superseded >= 1,
        "the retired incarnation is reported rather than silently accepted"
    );
}

#[test]
fn superseded_dependency_incarnation_recomputes_with_one_and_many_workers() {
    for worker_count in [2, 4] {
        run_superseded_dependency_incarnation(worker_count);
    }
}

#[test]
fn registered_batch_transfers_exact_leases_and_handoffs_before_child_teardown() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let child_commits = commits.clone();
    let child_aborts = aborts.clone();
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-leased-child",
            0,
            move |context, _, key| {
                context.register_attempt_handoff(CountingHandoff {
                    commits: child_commits.clone(),
                    aborts: child_aborts.clone(),
                });
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    let committed = Arc::new(Mutex::new(None));
    let events = Arc::new(Mutex::new(Vec::new()));
    let child_for_root = child.clone();
    let committed_for_root = committed.clone();
    let events_for_root = events.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-leased-root",
            1,
            move |context, _, _| {
                context.query_registered_batch(&child_for_root, (0..3).map(Slot))?;
                context.register_attempt_handoff(PinSetHandoff {
                    pins: Some(context.retain_observed_terminals()),
                    committed: committed_for_root.clone(),
                    events: events_for_root.clone(),
                });
                Ok(QueryOutput::success(3))
            },
        )
        .unwrap();

    let attempt =
        runtime.request_registered(&root, revision(1), Key("root"), CancellationToken::new());
    assert!(attempt.terminal().is_some());
    assert_eq!(commits.load(Ordering::SeqCst), 3);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);
    assert_eq!(lock(&committed).as_ref().map(RetainedPinSet::len), Some(3));
    assert_eq!(child.retention().terminals, 3);
    drop(lock(&committed).take());
    assert_eq!(child.retention().terminals, 0);
}

#[test]
fn retained_family_handoff_pins_only_that_family_and_rejects_foreign_runtime() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let selected = runtime
        .family_with_evaluator::<Slot, u64, _>("retained-family-selected", 0, |_, _, key| {
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let unrelated = runtime
        .family_with_evaluator::<Slot, u64, _>("retained-family-unrelated", 0, |_, _, key| {
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let foreign_runtime = QueryRuntime::new(1);
    publish_empty(&foreign_runtime, [revision(1)]);
    let foreign = foreign_runtime
        .family_with_evaluator::<Slot, u64, _>("retained-family-foreign", 0, |_, _, key| {
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let committed = Arc::new(Mutex::new(None));
    let events = Arc::new(Mutex::new(Vec::new()));
    let selected_for_publication = selected.clone();
    let unrelated_for_publication = unrelated.clone();
    let foreign_for_publication = foreign.clone();
    let committed_for_publication = committed.clone();
    let events_for_publication = events.clone();
    let publication = runtime
        .family_with_evaluator::<Key, u64, _>(
            "retained-family-publication",
            0,
            move |context, _, _| {
                context.query_registered_batch(&selected_for_publication, (0..3).map(Slot))?;
                context.query_registered_batch(&unrelated_for_publication, (0..2).map(Slot))?;
                assert!(matches!(
                    context.retain_observed_family(&foreign_for_publication),
                    Err(RetainTerminalConeError::ForeignRuntime)
                ));
                context.register_attempt_handoff(PinSetHandoff {
                    pins: Some(
                        context
                            .retain_observed_family(&selected_for_publication)
                            .expect("the selected family belongs to this runtime"),
                    ),
                    committed: committed_for_publication.clone(),
                    events: events_for_publication.clone(),
                });
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();

    let attempt = runtime.request_registered(
        &publication,
        revision(1),
        Key("publish"),
        CancellationToken::new(),
    );
    assert!(attempt.terminal().is_some());
    assert_eq!(lock(&committed).as_ref().map(RetainedPinSet::len), Some(3));
    assert_eq!(selected.retention().terminals, 3);
    assert_eq!(unrelated.retention().terminals, 0);
    drop(lock(&committed).take());
    assert_eq!(selected.retention().terminals, 0);
}

#[test]
fn registered_batch_transfers_one_encapsulating_handoff_lifecycle_per_child() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let commits = Arc::new(AtomicUsize::new(0));
    let aborts = Arc::new(AtomicUsize::new(0));
    let leaf_commits = commits.clone();
    let leaf_aborts = aborts.clone();
    let leaf = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-handoff-leaf",
            8,
            move |context, _, key| {
                context.register_attempt_handoff(CountingHandoff {
                    commits: leaf_commits.clone(),
                    aborts: leaf_aborts.clone(),
                });
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    let leaf_for_child = leaf.clone();
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-handoff-child",
            8,
            move |context, _, key| {
                context.query_registered(&leaf_for_child, key.clone())?;
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    let child_for_root = child.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-handoff-root",
            8,
            move |context, _, _| {
                context.query_registered_batch(&child_for_root, (0..3).map(Slot))?;
                let stack = lock(&context.task.stack);
                let observed = &stack
                    .last()
                    .expect("the root evaluator owns one frame")
                    .observed_handoffs;
                assert_eq!(
                    observed.len(),
                    3,
                    "each child transfers its root lifecycle, not its flattened history"
                );
                assert!(
                    observed
                        .iter()
                        .all(|lifecycle| lifecycle.observed.len() == 1),
                    "each transferred root still owns its leaf lifecycle"
                );
                Ok(QueryOutput::success(3))
            },
        )
        .unwrap();

    let attempt =
        runtime.request_registered(&root, revision(1), Key("root"), CancellationToken::new());
    assert!(attempt.terminal().is_some());
    assert_eq!(commits.load(Ordering::SeqCst), 3);
    assert_eq!(aborts.load(Ordering::SeqCst), 0);
}

#[test]
fn exact_terminal_cone_excludes_unrelated_observations_and_retains_every_dependency() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime
        .family_with_evaluator::<Slot, u64, _>("exact-cone-leaf", 0, |_, _, key| {
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let leaf_for_middle = leaf.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>("exact-cone-middle", 0, move |context, _, _| {
            context.query_registered_batch(&leaf_for_middle, [Slot(0), Slot(2)])?;
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let committed = Arc::new(Mutex::new(None));
    let events = Arc::new(Mutex::new(Vec::new()));
    let leaf_for_publication = leaf.clone();
    let middle_for_publication = middle.clone();
    let committed_for_publication = committed.clone();
    let events_for_publication = events.clone();
    let publication = runtime
        .family_with_evaluator::<Key, u64, _>("exact-cone-publication", 0, move |context, _, _| {
            let _proof = context.endorse_registered_validations();
            context.query_registered(&leaf_for_publication, Slot(1))?;
            let current = context.query_registered(&middle_for_publication, Key("current"))?;
            context.register_attempt_handoff(PinSetHandoff {
                pins: Some(
                    context
                        .retain_observed_terminal_cone(&current)
                        .expect("the registered current cone is complete"),
                ),
                committed: committed_for_publication.clone(),
                events: events_for_publication.clone(),
            });
            Ok(QueryOutput::success(1))
        })
        .unwrap();

    let attempt = runtime.request_registered(
        &publication,
        revision(1),
        Key("publish"),
        CancellationToken::new(),
    );
    assert!(attempt.terminal().is_some());
    assert_eq!(
        lock(&committed).as_ref().map(RetainedPinSet::len),
        Some(3),
        "the exact cone contains the current terminal and both batched leaves, \
             not the unrelated leaf"
    );
    assert_eq!(leaf.retention().terminals, 2);
    assert_eq!(middle.retention().terminals, 1);
}

#[test]
fn exact_terminal_cones_walk_one_deduplicated_union() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime
        .family_with_evaluator::<Slot, u64, _>("exact-cones-union-leaf", 0, |_, _, key| {
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let leaf_for_root = leaf.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>("exact-cones-union-root", 0, move |context, _, _| {
            context.query_registered(&leaf_for_root, Slot(0))?;
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let root_for_publication = root.clone();
    let runtime_for_publication = runtime.clone();
    let publication = runtime
        .family_with_evaluator::<Key, u64, _>(
            "exact-cones-union-publication",
            0,
            move |context, _, _| {
                let _proof = context.endorse_registered_validations();
                context.query_registered(&root_for_publication, Key("unrelated"))?;
                let first = context.query_registered(&root_for_publication, Key("first"))?;
                let second = context.query_registered(&root_for_publication, Key("second"))?;
                let before = runtime_for_publication.metrics().active_retained_pins;
                let retained = context
                    .retain_observed_terminal_cones_from(&[first.clone(), second, first], &[])
                    .unwrap();
                assert_eq!(
                    retained.len(),
                    3,
                    "duplicate roots and their shared leaf are retained once, while the \
                         unrelated observed root is excluded"
                );
                assert_eq!(
                    runtime_for_publication.metrics().active_retained_pins,
                    before + 3,
                    "deduplication accounting matches the exact union"
                );
                drop(retained);
                assert_eq!(
                    runtime_for_publication.metrics().active_retained_pins,
                    before
                );
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();

    assert!(
        runtime
            .request_registered(
                &publication,
                revision(1),
                Key("publish"),
                CancellationToken::new(),
            )
            .terminal()
            .is_some()
    );
}

#[test]
fn exact_terminal_cone_requires_proof_authority_and_an_observed_root() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("exact-cone-rejection-leaf", 1, |_, _, _| {
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let unobserved = runtime
        .request_registered(
            &leaf,
            revision(1),
            Key("unobserved"),
            CancellationToken::new(),
        )
        .into_result()
        .unwrap();
    let unobserved_for_check = unobserved.clone();
    let leaf_for_check = leaf.clone();
    let check = runtime
        .family_with_evaluator::<Key, u64, _>(
            "exact-cone-rejection-check",
            1,
            move |context, _, _| {
                let observed = context.query_registered(&leaf_for_check, Key("observed"))?;
                assert!(matches!(
                    context.retain_observed_terminal_cone(&observed),
                    Err(RetainTerminalConeError::NoRegisteredValidationScope)
                ));
                let _proof = context.endorse_registered_validations();
                assert!(matches!(
                    context.retain_observed_terminal_cone(&unobserved_for_check),
                    Err(RetainTerminalConeError::RootNotObserved)
                ));
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    assert!(
        runtime
            .request_registered(&check, revision(1), Key("check"), CancellationToken::new(),)
            .terminal()
            .is_some()
    );
}

#[test]
fn exact_terminal_cone_rejects_an_incomplete_dependency_observation() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let leaf = runtime
        .family_with_evaluator::<Slot, u64, _>("exact-cone-incomplete-leaf", 1, |_, _, key| {
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let leaf_for_middle = leaf.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>(
            "exact-cone-incomplete-middle",
            1,
            move |context, _, _| {
                context.query_registered(&leaf_for_middle, Slot(0))?;
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let middle_for_check = middle.clone();
    let check = runtime
        .family_with_evaluator::<Key, u64, _>(
            "exact-cone-incomplete-check",
            1,
            move |context, _, _| {
                let _proof = context.endorse_registered_validations();
                let current = context.query_registered(&middle_for_check, Key("current"))?;
                let dependency = current.dependencies()[0].clone();
                let removed = {
                    let mut leases = lock(&context.task.leases);
                    let index = leases
                        .held
                        .iter()
                        .position(|lease| {
                            let identity = lease.identity();
                            identity.0 == dependency.incarnation && identity.1 == dependency.stamp
                        })
                        .expect("the child dependency starts observed");
                    leases.held.remove(index)
                };
                context.task.core.metrics.task_leases_released(1);
                drop(removed);
                assert!(matches!(
                    context.retain_observed_terminal_cone(&current),
                    Err(RetainTerminalConeError::DependencyNotObserved(observation))
                        if observation == dependency
                ));
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    assert!(
        runtime
            .request_registered(&check, revision(1), Key("check"), CancellationToken::new(),)
            .terminal()
            .is_some()
    );
}

#[test]
fn exact_terminal_cone_inherits_a_missing_grandchild_from_a_published_snapshot() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1), revision(2)]);
    let leaf = runtime
        .family_with_evaluator::<Slot, u64, _>("fallback-grandchild-leaf", 8, |_, _, key| {
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let leaf_for_middle = leaf.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>(
            "fallback-grandchild-middle",
            8,
            move |context, _, _| {
                context.query_registered(&leaf_for_middle, Slot(1))?;
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();
    let middle_for_root = middle.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "fallback-grandchild-root",
            8,
            move |context, _, _| {
                context.query_registered(&middle_for_root, Key("middle"))?;
                Ok(QueryOutput::success(3))
            },
        )
        .unwrap();

    let old_root = runtime
        .request_registered(&root, revision(1), Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
    let old_middle = runtime
        .request_registered(
            &middle,
            revision(1),
            Key("middle"),
            CancellationToken::new(),
        )
        .into_result()
        .unwrap();
    let old_leaf = runtime
        .request_registered(&leaf, revision(1), Slot(1), CancellationToken::new())
        .into_result()
        .unwrap();
    let mut fallback = RetainedPinSet::new();
    fallback.lease(root.pin_terminal(&old_root).unwrap());
    fallback.lease(middle.pin_terminal(&old_middle).unwrap());
    fallback.lease(leaf.pin_terminal(&old_leaf).unwrap());
    let fallback = Arc::new(fallback);

    let root_for_check = root.clone();
    let check = runtime
        .family_with_evaluator::<Key, u64, _>(
            "fallback-grandchild-check",
            1,
            move |context, _, _| {
                let _proof = context.endorse_registered_validations();
                let current = context.query_registered(&root_for_check, Key("root"))?;
                let middle_edge = current.dependencies()[0].clone();
                let leaf_edge = {
                    let leases = lock(&context.task.leases);
                    leases
                        .held
                        .iter()
                        .find(|lease| {
                            let identity = lease.identity();
                            identity.0 == middle_edge.incarnation && identity.1 == middle_edge.stamp
                        })
                        .unwrap()
                        .dependencies()[0]
                        .clone()
                };
                let removed = {
                    let mut leases = lock(&context.task.leases);
                    let index = leases
                        .held
                        .iter()
                        .position(|lease| {
                            let identity = lease.identity();
                            identity.0 == leaf_edge.incarnation && identity.1 == leaf_edge.stamp
                        })
                        .unwrap();
                    leases.held.remove(index)
                };
                context.task.core.metrics.task_leases_released(1);
                drop(removed);
                let retained = context
                    .retain_observed_terminal_cone_from(&current, std::slice::from_ref(&fallback))
                    .unwrap();
                assert_eq!(retained.len(), 3);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    assert!(
        runtime
            .request_registered(&check, revision(2), Key("check"), CancellationToken::new())
            .terminal()
            .is_some()
    );
}

#[test]
fn exact_terminal_cone_prefers_current_same_stamp_dependencies_over_predecessor() {
    let runtime = QueryRuntime::new(1);
    let input = InputIdentity::new("source", "fallback-choice");
    runtime
        .publish_revision(revision(1), [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(revision(2), [(input.clone(), 2)])
        .unwrap();
    let leaf = runtime
        .family_with_evaluator::<Slot, u64, _>("fallback-choice-leaf", 8, |_, _, key| {
            Ok(QueryOutput::success(key.0))
        })
        .unwrap();
    let leaf_for_choice = leaf.clone();
    let input_for_choice = input.clone();
    let choice = runtime
        .family_with_evaluator::<Key, u64, _>("fallback-choice-root", 8, move |context, _, _| {
            let selected = context.input(input_for_choice.clone())?;
            context.query_registered(&leaf_for_choice, Slot(selected))?;
            Ok(QueryOutput::success(7))
        })
        .unwrap();
    let old_root = runtime
        .request_registered(&choice, revision(1), Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
    let old_leaf = runtime
        .request_registered(&leaf, revision(1), Slot(1), CancellationToken::new())
        .into_result()
        .unwrap();
    let mut fallback = RetainedPinSet::new();
    fallback.lease(choice.pin_terminal(&old_root).unwrap());
    fallback.lease(leaf.pin_terminal(&old_leaf).unwrap());
    let fallback = Arc::new(fallback);

    let choice_for_check = choice.clone();
    let old_root_for_check = old_root.clone();
    let check = runtime
        .family_with_evaluator::<Key, u64, _>("fallback-choice-check", 1, move |context, _, _| {
            let _proof = context.endorse_registered_validations();
            let current = context.query_registered(&choice_for_check, Key("root"))?;
            assert_eq!(
                current.node_incarnation(),
                old_root_for_check.node_incarnation()
            );
            assert_eq!(current.stamp(), old_root_for_check.stamp());
            assert_ne!(current.dependencies(), old_root_for_check.dependencies());
            let current_leaf = current.dependencies()[0].clone();
            let retained = context
                .retain_observed_terminal_cone_from(&current, std::slice::from_ref(&fallback))
                .unwrap();
            assert_eq!(retained.len(), 2, "the old leaf is excluded");
            assert!(retained.held.iter().any(|lease| {
                let identity = lease.identity();
                identity.0 == current_leaf.incarnation && identity.1 == current_leaf.stamp
            }));
            assert!(
                retained
                    .held
                    .iter()
                    .all(|lease| lease.identity().0 != old_leaf.node_incarnation()),
                "the fallback's old leaf identity must not replace the current leaf"
            );
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    assert!(
        runtime
            .request_registered(&check, revision(2), Key("check"), CancellationToken::new())
            .terminal()
            .is_some()
    );
}

#[test]
fn exact_terminal_cone_rejects_fallback_roots_and_foreign_runtime_snapshots() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let family = runtime
        .family_with_evaluator::<Key, u64, _>("fallback-authority", 8, |_, _, _| {
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let terminal = runtime
        .request_registered(&family, revision(1), Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
    let mut inherited_only = RetainedPinSet::new();
    inherited_only.lease(family.pin_terminal(&terminal).unwrap());
    let inherited_only = Arc::new(inherited_only);

    let foreign_runtime = QueryRuntime::new(1);
    publish_empty(&foreign_runtime, [revision(1)]);
    let foreign_family = foreign_runtime
        .family_with_evaluator::<Key, u64, _>("fallback-foreign", 8, |_, _, _| {
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let foreign_terminal = foreign_runtime
        .request_registered(
            &foreign_family,
            revision(1),
            Key("root"),
            CancellationToken::new(),
        )
        .into_result()
        .unwrap();
    let mut foreign = RetainedPinSet::new();
    foreign.lease(foreign_family.pin_terminal(&foreign_terminal).unwrap());
    let foreign = Arc::new(foreign);

    let family_for_check = family.clone();
    let terminal_for_check = terminal.clone();
    let check = runtime
        .family_with_evaluator::<Key, u64, _>(
            "fallback-authority-check",
            1,
            move |context, _, _| {
                assert!(matches!(
                    context.endorse_registered_validations_from(std::slice::from_ref(&foreign)),
                    Err(RetainTerminalConeError::ForeignRuntime)
                ));
                let _proof = context.endorse_registered_validations();
                assert!(matches!(
                    context.retain_observed_terminal_cone_from(
                        &terminal_for_check,
                        std::slice::from_ref(&inherited_only),
                    ),
                    Err(RetainTerminalConeError::RootNotObserved)
                ));
                let current = context.query_registered(&family_for_check, Key("current"))?;
                assert!(matches!(
                    context.retain_observed_terminal_cone_from(
                        &current,
                        std::slice::from_ref(&foreign),
                    ),
                    Err(RetainTerminalConeError::ForeignRuntime)
                ));
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    assert!(
        runtime
            .request_registered(&check, revision(1), Key("check"), CancellationToken::new())
            .terminal()
            .is_some()
    );
}

#[test]
fn registered_batch_first_abort_has_the_stable_observation_and_work_prefix() {
    let runtime = QueryRuntime::new(3);
    publish_empty(&runtime, [revision(1)]);
    let ran = Arc::new(Mutex::new(Vec::new()));
    let ran_for_child = ran.clone();
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-failing-child",
            8,
            move |context, _, key| {
                lock(&ran_for_child).push(key.0);
                context.record_work(WorkItem::new(format!("child-{}", key.0), 1));
                if key.0 == 1 {
                    Err(QueryAbort::MissingInput(InputIdentity::new(
                        "batch", "missing",
                    )))
                } else {
                    Ok(QueryOutput::success(key.0))
                }
            },
        )
        .unwrap();
    let child_for_root = child.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-failing-root",
            8,
            move |context, _, _| {
                context.query_registered_batch(&child_for_root, (0..3).map(Slot))?;
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();

    let attempt =
        runtime.request_registered(&root, revision(1), Key("root"), CancellationToken::new());
    assert_eq!(
        attempt.abort(),
        Some(&QueryAbort::MissingInput(InputIdentity::new(
            "batch", "missing"
        )))
    );
    let mut ran = lock(&ran).clone();
    ran.sort_unstable();
    assert_eq!(ran, vec![0, 1, 2]);
    assert_eq!(
        attempt
            .nested_attempts()
            .iter()
            .map(|nested| nested.node().key())
            .collect::<Vec<_>>(),
        vec!["0", "1"]
    );
    assert_eq!(
        attempt
            .work()
            .iter()
            .map(|(identity, amount)| (identity.as_ref(), *amount))
            .collect::<Vec<_>>(),
        vec![("child-0", 1), ("child-1", 1)]
    );
}

#[test]
fn registered_batch_cancellation_is_inherited_and_keeps_the_stable_prefix() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let cancellation = CancellationToken::new();
    let cancellation_for_child = cancellation.clone();
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-cancel-child",
            8,
            move |context, _, key| {
                context.record_work(WorkItem::new(format!("cancel-child-{}", key.0), 1));
                if key.0 == 1 {
                    cancellation_for_child.cancel();
                }
                context.check_canceled()?;
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    let child_for_root = child.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-cancel-root",
            8,
            move |context, _, _| {
                context.query_registered_batch(&child_for_root, (0..3).map(Slot))?;
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();
    let attempt = runtime.request_registered(&root, revision(1), Key("root"), cancellation);
    assert_eq!(attempt.abort(), Some(&QueryAbort::Canceled));
    assert_eq!(
        attempt
            .work()
            .iter()
            .map(|(identity, amount)| (identity.as_ref(), *amount))
            .collect::<Vec<_>>(),
        vec![("cancel-child-0", 1), ("cancel-child-1", 1)]
    );
}

#[test]
fn registered_batch_preserves_typed_dependency_cycles() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1)]);
    let family_slot = Arc::new(std::sync::OnceLock::new());
    let family_slot_for_evaluator = family_slot.clone();
    let cyclic = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-cycle",
            8,
            move |context, _, key| {
                let family = family_slot_for_evaluator.get().unwrap();
                context.query_registered(family, key.clone())?;
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    family_slot.set(cyclic.clone()).unwrap();
    let cyclic_for_root = cyclic.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-cycle-root",
            8,
            move |context, _, _| {
                context.query_registered_batch(&cyclic_for_root, [Slot(0)])?;
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();
    let attempt =
        runtime.request_registered(&root, revision(1), Key("root"), CancellationToken::new());
    assert!(
        matches!(attempt.abort(), Some(QueryAbort::Cycle(_))),
        "exact recursive dependency must remain a typed cycle, got {:?}",
        attempt.abort()
    );
}

fn run_registered_batch_parent_cycle(worker_count: usize, through_external: bool) {
    let runtime = QueryRuntime::new(worker_count);
    publish_empty(&runtime, [revision(1)]);
    let root_slot = Arc::new(std::sync::OnceLock::<QueryFamily<Key, u64>>::new());
    let root_slot_for_external = root_slot.clone();
    let external = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-parent-cycle-external",
            8,
            move |context, _, key| {
                context.query_registered(root_slot_for_external.get().unwrap(), Key("root"))?;
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    let root_slot_for_child = root_slot.clone();
    let external_for_child = external.clone();
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-parent-cycle-child",
            8,
            move |context, _, key| {
                if through_external {
                    context.query_registered(&external_for_child, key.clone())?;
                } else {
                    context.query_registered(root_slot_for_child.get().unwrap(), Key("root"))?;
                }
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    let child_for_root = child.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-parent-cycle-root",
            8,
            move |context, _, _| {
                context.query_registered_batch(&child_for_root, [Slot(0)])?;
                Ok(QueryOutput::success(0))
            },
        )
        .unwrap();
    root_slot.set(root.clone()).unwrap();

    let attempt =
        runtime.request_registered(&root, revision(1), Key("root"), CancellationToken::new());
    assert!(
        matches!(attempt.abort(), Some(QueryAbort::Cycle(_))),
        "a structured batch parent cycle must abort with {worker_count} permits \
             (through_external={through_external}), got {:?}",
        attempt.abort()
    );
}

#[test]
fn registered_batch_parent_cycles_abort_with_one_and_many_permits() {
    for worker_count in [1, 4] {
        run_registered_batch_parent_cycle(worker_count, false);
        run_registered_batch_parent_cycle(worker_count, true);
    }
}

#[test]
fn registered_batch_child_taints_the_enclosing_registered_validation_proof() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1), revision(2)]);
    let external = runtime
        .family::<Slot, u64>("registered-batch-proof-external", 1)
        .unwrap();
    let external_for_child = external.clone();
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-proof-child",
            1,
            move |context, _, key| {
                context.query(&external_for_child, key.clone(), |_| {
                    Ok(QueryOutput::success(key.0))
                })?;
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    runtime
        .request_registered(&child, revision(1), Slot(0), CancellationToken::new())
        .into_result()
        .unwrap();
    let child_for_root = child.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-proof-root",
            1,
            move |context, _, _| {
                let proof = context.task.begin_validation();
                context.query_registered_batch(&child_for_root, [Slot(0)])?;
                assert!(
                    !proof.registered_only(),
                    "an unregistered evaluator in a batch child must taint the parent's proof"
                );
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    assert!(
        runtime
            .request_registered(&root, revision(2), Key("root"), CancellationToken::new(),)
            .terminal()
            .is_some()
    );
}

#[test]
fn nested_registered_batch_child_taints_every_enclosing_validation_proof() {
    let runtime = QueryRuntime::new(3);
    publish_empty(&runtime, [revision(1), revision(2)]);
    let external = runtime
        .family::<Slot, u64>("nested-registered-batch-proof-external", 1)
        .unwrap();
    let external_for_leaf = external.clone();
    let leaf = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "nested-registered-batch-proof-leaf",
            1,
            move |context, _, key| {
                context.query(&external_for_leaf, key.clone(), |_| {
                    Ok(QueryOutput::success(key.0))
                })?;
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    runtime
        .request_registered(&leaf, revision(1), Slot(0), CancellationToken::new())
        .into_result()
        .unwrap();

    let leaf_for_middle = leaf.clone();
    let middle = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "nested-registered-batch-proof-middle",
            1,
            move |context, _, key| {
                context.query_registered_batch(&leaf_for_middle, [key.clone()])?;
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    let middle_for_root = middle.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "nested-registered-batch-proof-root",
            1,
            move |context, _, _| {
                let proof = context.task.begin_validation();
                context.query_registered_batch(&middle_for_root, [Slot(0)])?;
                assert!(
                    !proof.registered_only(),
                    "an unregistered evaluator in a nested batch must taint every ancestor"
                );
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    assert!(
        runtime
            .request_registered(&root, revision(2), Key("root"), CancellationToken::new(),)
            .terminal()
            .is_some()
    );
}

#[test]
fn registered_batch_absorbs_fallbacks_backing_child_endorsements() {
    let runtime = QueryRuntime::new(2);
    publish_empty(&runtime, [revision(1), revision(2)]);

    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>("registered-batch-fallback-leaf", 8, |_, _, _| {
            Ok(QueryOutput::success(1))
        })
        .unwrap();
    let leaf_for_middle = leaf.clone();
    let middle = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-fallback-middle",
            8,
            move |context, _, _| {
                context.query_registered(&leaf_for_middle, Key("leaf"))?;
                Ok(QueryOutput::success(2))
            },
        )
        .unwrap();
    runtime
        .request_registered(
            &middle,
            revision(1),
            Key("middle"),
            CancellationToken::new(),
        )
        .into_result()
        .unwrap();
    let middle_terminal = runtime
        .request_registered(
            &middle,
            revision(2),
            Key("middle"),
            CancellationToken::new(),
        )
        .into_result()
        .unwrap();
    let leaf_terminal = runtime
        .request_registered(&leaf, revision(2), Key("leaf"), CancellationToken::new())
        .into_result()
        .unwrap();
    let mut child_fallback = RetainedPinSet::new();
    child_fallback.lease(leaf.pin_terminal(&leaf_terminal).unwrap());
    let child_fallback = Arc::new(child_fallback);

    let middle_for_child = middle.clone();
    let child_fallback_for_child = child_fallback.clone();
    let child = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-fallback-child",
            8,
            move |context, _, key| {
                let _nested = context
                    .endorse_registered_validations_from(std::slice::from_ref(
                        &child_fallback_for_child,
                    ))
                    .unwrap();
                context.query_registered(&middle_for_child, Key("middle"))?;
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();

    let outer_fallback = Arc::new(RetainedPinSet::new());
    let child_for_root = child.clone();
    let outer_fallback_for_root = outer_fallback.clone();
    let child_fallback_for_root = child_fallback.clone();
    let middle_terminal_for_root = middle_terminal.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-fallback-root",
            8,
            move |context, _, _| {
                let baseline_refs = Arc::strong_count(&child_fallback_for_root);
                {
                    let _outer = context
                        .endorse_registered_validations_from(std::slice::from_ref(
                            &outer_fallback_for_root,
                        ))
                        .unwrap();
                    context.query_registered_batch(&child_for_root, [Slot(0)])?;
                    assert_eq!(
                        context.task.validation_endorsement_authority_for_terminal(
                            &middle_terminal_for_root,
                        ),
                        ValidationEndorsementAuthority::TaskLocal,
                    );
                    assert!(
                        lock(&context.task.validation_endorsements)[0]
                            .fallbacks
                            .iter()
                            .any(|fallback| Arc::ptr_eq(fallback, &child_fallback_for_root,))
                    );
                    assert_eq!(
                        Arc::strong_count(&child_fallback_for_root),
                        baseline_refs + 1,
                    );
                }
                assert_eq!(Arc::strong_count(&child_fallback_for_root), baseline_refs,);
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    runtime
        .request_registered(&root, revision(2), Key("root"), CancellationToken::new())
        .into_result()
        .unwrap();
}

#[test]
fn registered_batch_abort_releases_shared_validation_leases() {
    let runtime = QueryRuntime::new(1);
    let first = Revision::new(1, 9);
    let second = Revision::new(2, 9);
    let input = InputIdentity::new("source", "registered-batch-abort-proof");
    runtime
        .publish_revision(first, [(input.clone(), 1)])
        .unwrap();
    runtime
        .publish_revision(second, [(input.clone(), 1)])
        .unwrap();

    let input_for_leaf = input.clone();
    let leaf = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-abort-proof-leaf",
            8,
            move |context, _, _| Ok(QueryOutput::success(context.input(input_for_leaf.clone())?)),
        )
        .unwrap();
    let leaf_for_branch = leaf.clone();
    let branch = runtime
        .family_with_evaluator::<Slot, u64, _>(
            "registered-batch-abort-proof-branch",
            8,
            move |context, _, key| {
                if key.0 == 1 {
                    return Err(QueryAbort::MissingInput(InputIdentity::new(
                        "batch",
                        "proof-abort",
                    )));
                }
                context.query_registered(&leaf_for_branch, Key("leaf"))?;
                Ok(QueryOutput::success(key.0))
            },
        )
        .unwrap();
    runtime
        .request_registered(&branch, first, Slot(0), CancellationToken::new())
        .into_result()
        .unwrap();
    runtime
        .request_registered(&branch, first, Slot(2), CancellationToken::new())
        .into_result()
        .unwrap();

    let branch_for_root = branch.clone();
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-abort-proof-root",
            8,
            move |context, _, _| {
                let _proof = context.endorse_registered_validations();
                context.query_registered_batch(&branch_for_root, [Slot(0), Slot(2), Slot(1)])?;
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();

    let before = runtime.metrics().validation;
    let attempt = runtime.request_registered(&root, second, Key("root"), CancellationToken::new());
    assert_eq!(
        attempt.abort(),
        Some(&QueryAbort::MissingInput(InputIdentity::new(
            "batch",
            "proof-abort"
        )))
    );
    let work = runtime.metrics().validation.saturating_sub(before);
    assert!(
        work.endorsement_hits > 0,
        "the second successful sibling borrowed the first sibling's published proof: {work:?}"
    );
    drop(attempt);
    assert_eq!(
        runtime.metrics().active_task_leases,
        0,
        "aborting the parent drops the completed sibling's shared proof pins"
    );
}

#[test]
#[cfg(panic = "unwind")]
fn registered_batch_panic_restores_the_parent_permit() {
    let runtime = QueryRuntime::new(1);
    publish_empty(&runtime, [revision(1)]);
    let root = runtime
        .family_with_evaluator::<Key, u64, _>(
            "registered-batch-panicking-root",
            8,
            |context, _, _| {
                let panicked = catch_unwind(AssertUnwindSafe(|| {
                    let _donation = ParentPermitDonation::new(context.task.clone());
                    panic!("batch coordinator panic");
                }));
                assert!(panicked.is_err());
                assert!(
                    context.task.owns_permit.load(Ordering::Acquire),
                    "unwinding the batch join interval must restore the parent permit"
                );
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let attempt =
        runtime.request_registered(&root, revision(1), Key("root"), CancellationToken::new());
    assert!(attempt.terminal().is_some());

    let recovery = runtime
        .family::<Key, u64>("registered-batch-panic-recovery", 1)
        .unwrap();
    let recovered = runtime
        .query(
            &recovery,
            revision(1),
            Key("recovered"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(1)),
        )
        .unwrap();
    assert_eq!(recovered.outcome(), &QueryOutcome::Success(1));
}
