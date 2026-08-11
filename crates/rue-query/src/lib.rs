//! Parallel, demand-driven query execution primitives.
//!
//! This crate owns execution mechanics only. Compiler query families keep
//! their typed keys, results, equality, and algorithms outside the runtime.

use std::any::Any;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak};

use ahash::{AHashMap, AHashSet, RandomState};

static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

const VALIDATION_PROOF_REGISTERED: u8 = 0;
const VALIDATION_PROOF_RETRYABLE: u8 = 1;
const VALIDATION_PROOF_UNREGISTERED: u8 = 2;
const VALIDATION_PUBLISH_SWEEP_QUANTUM: usize = 64;

/// Initial runtime-wide soft budget for deterministic retained terminal charge.
///
/// This is an accounting budget rather than an allocator/RSS promise. Protected
/// terminals may exceed it; the runtime records that pressure and reclaims the
/// excess as soon as protection releases.
pub const DEFAULT_RETAINED_BYTE_BUDGET: u64 = 8 * 1024 * 1024 * 1024;

/// Initial runtime-wide soft budget for retained dependency and input
/// observations.
pub const DEFAULT_DEPENDENCY_PIN_BUDGET: u64 = 4_000_000;

/// Runtime-wide soft retention budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionBudgets {
    /// Deterministic terminal/artifact charge in bytes.
    pub retained_bytes: u64,
    /// Retained dependency plus input observation edges.
    pub dependency_pins: u64,
}

impl Default for RetentionBudgets {
    fn default() -> Self {
        Self {
            retained_bytes: DEFAULT_RETAINED_BYTE_BUDGET,
            dependency_pins: DEFAULT_DEPENDENCY_PIN_BUDGET,
        }
    }
}

/// Registered evaluators historically ran on the requesting compiler thread,
/// whose platform stack is materially larger than Rust's default spawned-thread
/// stack. Keep structured batch children at that established floor so moving a
/// valid deeply nested query onto a worker cannot create a stack overflow.
const REGISTERED_BATCH_WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

thread_local! {
    static HANDOFF_CALLBACK_PHASE: Cell<Option<HandoffCallbackPhase>> = const { Cell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandoffCallbackPhase {
    Commit,
    Abort,
}

struct HandoffCallbackGuard {
    previous: Option<HandoffCallbackPhase>,
}

impl HandoffCallbackGuard {
    fn enter(phase: HandoffCallbackPhase) -> Self {
        let previous = HANDOFF_CALLBACK_PHASE.with(|active| active.replace(Some(phase)));
        assert!(previous.is_none(), "attempt handoff callbacks do not nest");
        Self { previous }
    }

    fn active() -> bool {
        HANDOFF_CALLBACK_PHASE.with(|active| active.get().is_some())
    }
}

#[cfg(test)]
mod registered_batch_tests {
    use std::sync::Barrier;
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct Key(&'static str);

    impl QueryKey for Key {
        fn stable_identity(&self) -> String {
            self.0.to_owned()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct Slot(u64);

    impl QueryKey for Slot {
        fn stable_identity(&self) -> String {
            self.0.to_string()
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
        assert_eq!(
            peak.load(Ordering::Acquire),
            worker_count.min(4),
            "the registered batch must use the runtime's shared permit budget"
        );
        attempt
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
            .family_with_evaluator::<Slot, u64, _>(
                "lease-metrics-branch",
                8,
                move |context, _, key| {
                    context.query_registered(&leaf_for_branch, Key("shared"))?;
                    context.query_registered(&leaf_for_branch, Key("shared"))?;
                    Ok(QueryOutput::success(key.0))
                },
            )
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

        let initial =
            runtime.request_registered(&root, first, Key("root"), CancellationToken::new());
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
            .family_with_evaluator::<Key, u64, _>(
                "batch-authority-leaf",
                8,
                move |context, _, _| {
                    Ok(QueryOutput::success(
                        context.input(leaf_input_for_evaluator.clone())?,
                    ))
                },
            )
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
            .family_with_evaluator::<Key, u64, _>(
                "batch-authority-root",
                8,
                move |context, _, _| {
                    let root_stamp = context.input(root_input_for_evaluator.clone())?;
                    let _proof = context.endorse_registered_validations();
                    let branches =
                        context.query_registered_batch(&branch_for_root, [Slot(0), Slot(1)])?;
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
                },
            )
            .unwrap();

        let initial =
            runtime.request_registered(&root, first, Key("root"), CancellationToken::new());
        assert_eq!(
            initial.terminal().unwrap().outcome(),
            &QueryOutcome::Success(4)
        );
        drop(initial);

        let before = runtime.metrics().validation;
        let current =
            runtime.request_registered(&root, second, Key("root"), CancellationToken::new());
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
            .family_with_evaluator::<Key, u64, _>(
                "z-stale-reverse-shape",
                8,
                move |context, _, _| {
                    Ok(QueryOutput::success(
                        context.input(shape_input_for_evaluator.clone())?,
                    ))
                },
            )
            .unwrap();
        let graph_slot = Arc::new(std::sync::OnceLock::<QueryFamily<Key, u64>>::new());
        let graph_slot_for_evaluator = graph_slot.clone();
        let shapes_for_graph = shapes.clone();
        let graph = runtime
            .family_with_evaluator::<Key, u64, _>(
                "a-stale-reverse-graph",
                8,
                move |context, _, key| {
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
                },
            )
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
        let current =
            runtime.request_registered(&graph, second, Key("a"), CancellationToken::new());
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
            let attempt = runtime.request_registered(
                &parents,
                revision,
                Key("root"),
                CancellationToken::new(),
            );
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
            .family_with_evaluator::<Key, u64, _>(
                "exact-cone-publication",
                0,
                move |context, _, _| {
                    let _proof = context.endorse_registered_validations();
                    context.query_registered(&leaf_for_publication, Slot(1))?;
                    let current =
                        context.query_registered(&middle_for_publication, Key("current"))?;
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
            .family_with_evaluator::<Key, u64, _>(
                "exact-cones-union-root",
                0,
                move |context, _, _| {
                    context.query_registered(&leaf_for_root, Slot(0))?;
                    Ok(QueryOutput::success(1))
                },
            )
            .unwrap();
        let root_for_publication = root.clone();
        let runtime_for_publication = runtime.clone();
        let publication = runtime
            .family_with_evaluator::<Key, u64, _>(
                "exact-cones-union-publication",
                0,
                move |context, _, _| {
                    let _proof = context.endorse_registered_validations();
                    let first = context.query_registered(&root_for_publication, Key("first"))?;
                    let second = context.query_registered(&root_for_publication, Key("second"))?;
                    let before = runtime_for_publication.metrics().active_retained_pins;
                    let retained = context
                        .retain_observed_terminal_cones_from(&[first, second], &[])
                        .unwrap();
                    assert_eq!(retained.len(), 3, "the shared leaf is retained once");
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
                                identity.0 == dependency.incarnation
                                    && identity.1 == dependency.stamp
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
                                identity.0 == middle_edge.incarnation
                                    && identity.1 == middle_edge.stamp
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
                        .retain_observed_terminal_cone_from(
                            &current,
                            std::slice::from_ref(&fallback),
                        )
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
            .family_with_evaluator::<Key, u64, _>(
                "fallback-choice-root",
                8,
                move |context, _, _| {
                    let selected = context.input(input_for_choice.clone())?;
                    context.query_registered(&leaf_for_choice, Slot(selected))?;
                    Ok(QueryOutput::success(7))
                },
            )
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
            .family_with_evaluator::<Key, u64, _>(
                "fallback-choice-check",
                1,
                move |context, _, _| {
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
                        .retain_observed_terminal_cone_from(
                            &current,
                            std::slice::from_ref(&fallback),
                        )
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
                        context
                            .query_registered(root_slot_for_child.get().unwrap(), Key("root"))?;
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
                move |context, _, _| {
                    Ok(QueryOutput::success(context.input(input_for_leaf.clone())?))
                },
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
                    context
                        .query_registered_batch(&branch_for_root, [Slot(0), Slot(2), Slot(1)])?;
                    Ok(QueryOutput::success(1))
                },
            )
            .unwrap();

        let before = runtime.metrics().validation;
        let attempt =
            runtime.request_registered(&root, second, Key("root"), CancellationToken::new());
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
}

impl Drop for HandoffCallbackGuard {
    fn drop(&mut self) {
        HANDOFF_CALLBACK_PHASE.with(|active| active.set(self.previous));
    }
}

/// A logical key suitable for a retained query family.
///
/// `Hash` must agree with `Eq`: keys that compare equal must hash equal. The
/// memo map is keyed by the typed key itself, so hash collisions never conflate
/// distinct keys — they are resolved by exact `Self::eq`. Implementors that
/// embed `Arc<[T]>` or map/set payloads must derive or write `Hash`
/// consistently with their `Eq`.
pub trait QueryKey: Clone + Eq + Hash + Send + Sync + 'static {
    /// A deterministic user-visible identity within the family.
    ///
    /// This text is presentation only and may collide. Exact `Self::eq`
    /// remains authoritative for memo-node lookup.
    fn stable_identity(&self) -> String;
}

/// Collision-free identity of one immutable input leaf.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputIdentity {
    family: Arc<str>,
    key: Arc<str>,
}

impl InputIdentity {
    /// Creates a family/key input identity.
    pub fn new(family: impl Into<Arc<str>>, key: impl Into<Arc<str>>) -> Self {
        Self {
            family: family.into(),
            key: key.into(),
        }
    }

    /// Stable input-family name.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Stable key within the input family.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Exact value stamp of one leaf in an immutable input revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputObservation {
    /// Collision-free input identity.
    pub input: InputIdentity,
    /// Family-owned exact value stamp. This is not a memo-node identity.
    pub stamp: u64,
}

/// An immutable input revision pinned by one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision {
    id: u64,
    compatibility: u64,
}

impl Revision {
    /// Creates a revision. Equal compatibility tokens assert equivalent inputs.
    pub const fn new(id: u64, compatibility: u64) -> Self {
        Self { id, compatibility }
    }

    /// The immutable publication identity.
    pub const fn id(self) -> u64 {
        self.id
    }

    /// Returns whether retained work may be validated across two revisions.
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.compatibility == other.compatibility
    }
}

/// Canonical user-visible identity of one logical memo node.
///
/// The display identity is shared by the node, its terminals, and every
/// dependency observation. Runtime-created identities also carry a weak,
/// non-owning route back to the exact erased node. Equality, ordering, hashing,
/// and display remain defined solely by the stable family/key pair.
#[derive(Clone)]
pub struct NodeIdentity {
    inner: Arc<NodeIdentityData>,
}

struct NodeIdentityData {
    family: Arc<str>,
    key: Box<str>,
    runtime_identity: Option<u64>,
    node: Option<Weak<dyn ErasedNode>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ExactNodeIdentity {
    display: NodeIdentity,
    incarnation: u64,
}

impl NodeIdentity {
    fn new(family: Arc<str>, key: Box<str>) -> Self {
        Self {
            inner: Arc::new(NodeIdentityData {
                family,
                key,
                runtime_identity: None,
                node: None,
            }),
        }
    }

    fn registered(
        family: Arc<str>,
        key: Box<str>,
        runtime_identity: u64,
        node: Weak<dyn ErasedNode>,
    ) -> Self {
        Self {
            inner: Arc::new(NodeIdentityData {
                family,
                key,
                runtime_identity: Some(runtime_identity),
                node: Some(node),
            }),
        }
    }

    fn registered_node(
        &self,
        runtime_identity: u64,
        incarnation: u64,
    ) -> Option<Arc<dyn ErasedNode>> {
        if self.inner.runtime_identity != Some(runtime_identity) {
            return None;
        }
        let node = self.inner.node.as_ref()?.upgrade()?;
        (node.incarnation() == incarnation).then_some(node)
    }

    /// Stable family name.
    pub fn family(&self) -> &str {
        &self.inner.family
    }

    /// Family-defined stable key identity.
    pub fn key(&self) -> &str {
        &self.inner.key
    }
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NodeIdentity")
            .field("family", &self.inner.family)
            .field("key", &self.inner.key)
            .finish()
    }
}

impl PartialEq for NodeIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.family() == other.family() && self.key() == other.key()
    }
}

impl Eq for NodeIdentity {}

impl PartialOrd for NodeIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NodeIdentity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.family(), self.key()).cmp(&(other.family(), other.key()))
    }
}

impl Hash for NodeIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.family().hash(state);
        self.key().hash(state);
    }
}

/// Identity observed by a dependent query.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Observation {
    /// Logical dependency node.
    pub node: NodeIdentity,
    /// Opaque session-local node incarnation, preventing stamp ABA after eviction.
    pub incarnation: u64,
    /// Red/green terminal publication stamp.
    pub stamp: u64,
}

/// A deterministic query failure which is safe to retain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryFailure {
    /// Stable failure class.
    pub code: Arc<str>,
    /// Canonical failure payload.
    pub payload: Arc<str>,
}

impl QueryFailure {
    /// Creates a retained failure.
    pub fn new(code: impl Into<Arc<str>>, payload: impl Into<Arc<str>>) -> Self {
        Self {
            code: code.into(),
            payload: payload.into(),
        }
    }
}

/// A semantic diagnostic plus a separately replaceable presentation position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryDiagnostic {
    /// Stable producer-defined identity.
    pub identity: Arc<str>,
    /// Canonical semantic payload.
    pub payload: Arc<str>,
    /// Current presentation position, excluded from red/green equality.
    pub presentation: Option<PresentationPosition>,
}

impl QueryDiagnostic {
    /// Creates a diagnostic.
    pub fn new(
        identity: impl Into<Arc<str>>,
        payload: impl Into<Arc<str>>,
        presentation: Option<PresentationPosition>,
    ) -> Self {
        Self {
            identity: identity.into(),
            payload: payload.into(),
            presentation,
        }
    }
}

/// Current source position used only when presenting a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PresentationPosition {
    /// Stable source identity.
    pub source: Arc<str>,
    /// Current byte offset.
    pub offset: u32,
}

impl PresentationPosition {
    /// Creates a presentation position.
    pub fn new(source: impl Into<Arc<str>>, offset: u32) -> Self {
        Self {
            source: source.into(),
            offset,
        }
    }
}

/// One deterministic structural-work contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItem {
    /// Stable metric identity.
    pub identity: Arc<str>,
    /// Additive amount.
    pub amount: u64,
}

impl WorkItem {
    /// Creates one contribution.
    pub fn new(identity: impl Into<Arc<str>>, amount: u64) -> Self {
        Self {
            identity: identity.into(),
            amount,
        }
    }
}

/// Canonical success or retained deterministic failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryOutcome<V> {
    /// Successfully computed value.
    Success(V),
    /// Deterministic family failure.
    Failure(QueryFailure),
}

/// Family-owned semantic classification of a terminal record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryTerminalKind {
    /// A successful family record.
    Success,
    /// A deterministic family failure record.
    Failure,
}

/// Private computation output awaiting atomic publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOutput<V> {
    outcome: QueryOutcome<V>,
    kind: QueryTerminalKind,
    diagnostics: Vec<QueryDiagnostic>,
    work: Vec<WorkItem>,
    retained_value_charge: Option<u64>,
}

impl<V> QueryOutput<V> {
    /// Creates a successful output.
    pub fn success(value: V) -> Self {
        Self {
            outcome: QueryOutcome::Success(value),
            kind: QueryTerminalKind::Success,
            diagnostics: Vec::new(),
            work: Vec::new(),
            retained_value_charge: None,
        }
    }

    /// Creates a deterministic terminal failure.
    pub fn failure(failure: QueryFailure) -> Self {
        Self {
            outcome: QueryOutcome::Failure(failure),
            kind: QueryTerminalKind::Failure,
            diagnostics: Vec::new(),
            work: Vec::new(),
            retained_value_charge: None,
        }
    }

    /// Attaches diagnostics. Publication sorts them deterministically.
    pub fn with_diagnostics(mut self, diagnostics: Vec<QueryDiagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Attaches structural work. Publication reduces it by stable identity.
    pub fn with_work(mut self, work: Vec<WorkItem>) -> Self {
        self.work = work;
        self
    }

    /// Adds this success value's deterministic reachable heap charge.
    ///
    /// The terminal envelope already includes the inline `V` storage, so this
    /// charge is only for heap-owned allocations reachable through the value.
    /// The runtime adds the terminal envelope, diagnostics,
    /// structural work, dependencies, and inputs automatically. Shared
    /// allocations may be charged in full by every terminal which reaches them.
    /// Failure values ignore this override because their canonical strings are
    /// charged directly.
    pub fn with_retained_value_charge(mut self, bytes: u64) -> Self {
        self.retained_value_charge = Some(bytes);
        self
    }

    /// Applies the typed family's semantic success/failure classification.
    /// This is useful when both variants retain the same typed record shape.
    pub fn with_terminal_kind(mut self, kind: QueryTerminalKind) -> Self {
        self.kind = kind;
        self
    }
}

/// One non-semantic resource whose ownership follows a computed query attempt.
///
/// Evaluators register a handoff through
/// [`QueryContext::register_attempt_handoff`] while the attempt is active. The
/// resulting terminal retains the handoff internally while it is pending. The
/// runtime calls [`Self::commit`] only when the whole top-level rooted task that
/// observed it completes successfully. Speculative validation does not commit;
/// a later observed reuse can. Cancellation or panic while committing rolls the
/// attempted prefix back to pending for a later root, while terminal eviction
/// calls [`Self::abort`] to release a still-pending resource.
///
/// Handoffs are attempt-local control resources. They are never stored in
/// [`QueryOutput`] or [`QueryTerminal`], do not participate in equality or
/// stamps, and receive no [`QueryContext`], so committing one cannot execute a
/// nested dependency query. Public query requests made reentrantly by a commit
/// or abort callback fail immediately with [`QueryAbort::Canceled`]; callbacks
/// cannot publish work outside the enclosing root's rollback boundary.
pub trait QueryAttemptHandoff: fmt::Debug + Send + 'static {
    /// Transfer the resource at successful rooted-task completion.
    ///
    /// If this method unwinds, the runtime calls [`Self::abort`] in reverse order
    /// on this callback and the earlier attempted prefix. Callbacks not yet
    /// attempted remain untouched. Lifecycles whose attempted callbacks roll
    /// back are restored to pending before the original panic resumes, so
    /// `commit` may be called again on a later root. If an abort callback also
    /// unwinds, that lifecycle is permanently marked aborted; any terminal graph
    /// containing it becomes unavailable and cannot be reused as a partial
    /// publication.
    fn commit(&mut self);

    /// Roll back a partial commit, or release a pending resource on eviction.
    ///
    /// Implementations should not unwind. An unwind fails the lifecycle closed
    /// as described on [`Self::commit`]; it is never restored to pending.
    fn abort(&mut self);
}

/// An immutable published terminal.
#[derive(Debug)]
pub struct QueryTerminal<V> {
    family_token: FamilyToken,
    node: NodeIdentity,
    node_incarnation: u64,
    revision: Revision,
    stamp: u64,
    origin_request: u64,
    outcome: QueryOutcome<V>,
    kind: QueryTerminalKind,
    diagnostics: Arc<[QueryDiagnostic]>,
    work: Arc<[(Arc<str>, u64)]>,
    dependencies: Arc<[Observation]>,
    inputs: Arc<[InputObservation]>,
    retained_charge: u64,
    dependency_pin_charge: u64,
    pins: AtomicUsize,
}

impl<V> QueryTerminal<V> {
    /// Canonical display identity of the node which owns this attempt.
    pub fn node(&self) -> &NodeIdentity {
        &self.node
    }

    /// Exact revision under which the computing task ran.
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Red/green publication identity.
    pub const fn stamp(&self) -> u64 {
        self.stamp
    }

    /// Opaque session-local incarnation of the node which owns this terminal,
    /// preventing stamp ABA after eviction.
    pub const fn node_incarnation(&self) -> u64 {
        self.node_incarnation
    }

    /// Runtime request which originally computed this immutable terminal.
    pub fn origin_request_id(&self) -> u64 {
        self.origin_request
    }

    /// Canonical success or deterministic failure.
    pub fn outcome(&self) -> &QueryOutcome<V> {
        &self.outcome
    }

    /// Family-owned semantic success/failure classification.
    pub fn kind(&self) -> QueryTerminalKind {
        self.kind
    }

    /// Deterministically sorted diagnostics with current positions.
    pub fn diagnostics(&self) -> &[QueryDiagnostic] {
        &self.diagnostics
    }

    /// Deterministically reduced structural work.
    pub fn work(&self) -> &[(Arc<str>, u64)] {
        &self.work
    }

    /// Exact dependency observations made by the computing task.
    pub fn dependencies(&self) -> &[Observation] {
        &self.dependencies
    }

    /// Exact input leaves read directly by the computing body.
    pub fn inputs(&self) -> &[InputObservation] {
        &self.inputs
    }

    /// Deterministic runtime-wide retained artifact charge.
    pub const fn retained_charge(&self) -> u64 {
        self.retained_charge
    }

    /// Retained dependency and input observation edges charged to this terminal.
    pub const fn dependency_pin_charge(&self) -> u64 {
        self.dependency_pin_charge
    }
}

/// A non-terminal query-control result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryAbort {
    /// This request was canceled. No terminal was published.
    Canceled,
    /// Exact nodes participating in a true dependency cycle.
    Cycle(Arc<[NodeIdentity]>),
    /// The family belongs to a different runtime.
    ForeignRuntime,
    /// The requested immutable revision has not been published or was retired.
    UnpublishedRevision(Revision),
    /// A query requested a leaf absent from its pinned immutable revision.
    MissingInput(InputIdentity),
}

/// How one immutable request attempt reached its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestExecution {
    /// This request owned and ran the query body.
    Computed,
    /// This request reused a validated retained terminal.
    Reused,
    /// This request parked behind the compatible exact-key owner.
    Joined,
    /// This request ended without publishing a terminal.
    Aborted,
}

/// Runtime-owned immutable record of one nested query request.
///
/// The value remains owned by its typed terminal. This type-erased lifecycle
/// is sufficient for diagnostics, metrics, and provenance without forging a
/// second typed memo record in a compatibility adapter.
#[derive(Debug, Clone)]
pub struct NestedQueryAttempt {
    id: u64,
    node: NodeIdentity,
    node_incarnation: Option<u64>,
    origin_request: u64,
    execution: RequestExecution,
    terminal_revision: Option<Revision>,
    terminal_stamp: Option<u64>,
    abort: Option<QueryAbort>,
    dependencies: Arc<[Observation]>,
    inputs: Arc<[InputObservation]>,
    work: Arc<[(Arc<str>, u64)]>,
}

impl NestedQueryAttempt {
    /// Runtime-local identity of this nested request.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Display identity of the node requested by this lifecycle.
    pub fn node(&self) -> &NodeIdentity {
        &self.node
    }

    /// Opaque exact node incarnation when the request returned a terminal.
    pub fn node_incarnation(&self) -> Option<u64> {
        self.node_incarnation
    }

    /// Caller-owned origin frozen into a computed terminal, or inherited from
    /// the terminal reused or joined by this request.
    pub fn origin_request_id(&self) -> u64 {
        self.origin_request
    }

    /// Computed, reused, joined, or aborted lifecycle classification.
    pub fn execution(&self) -> RequestExecution {
        self.execution
    }

    /// Revision of the terminal returned by this request.
    pub fn terminal_revision(&self) -> Option<Revision> {
        self.terminal_revision
    }

    /// Red/green stamp of the terminal returned by this request.
    pub fn terminal_stamp(&self) -> Option<u64> {
        self.terminal_stamp
    }

    /// Non-terminal control result, present exactly for aborted attempts.
    pub fn abort(&self) -> Option<&QueryAbort> {
        self.abort.as_ref()
    }

    /// Exact dependency prefix observed by this nested request.
    pub fn dependencies(&self) -> &[Observation] {
        &self.dependencies
    }

    /// Exact direct-input prefix observed by this nested request.
    pub fn inputs(&self) -> &[InputObservation] {
        &self.inputs
    }

    /// Runtime-owned work prefix for this nested request.
    pub fn work(&self) -> &[(Arc<str>, u64)] {
        &self.work
    }
}

/// Runtime-owned immutable record of one top-level query request.
pub struct QueryRequestAttempt<V> {
    id: u64,
    origin_request: u64,
    execution: RequestExecution,
    terminal: Option<Arc<QueryTerminal<V>>>,
    abort: Option<QueryAbort>,
    dependencies: Arc<[Observation]>,
    inputs: Arc<[InputObservation]>,
    work: Arc<[(Arc<str>, u64)]>,
    nested_attempts: Arc<[NestedQueryAttempt]>,
    /// A retention lease on `terminal`, acquired while the producing task (and
    /// its request-scoped leases) were still alive, so the published result stays
    /// retained across the gap between this request completing and the caller
    /// registering a successor protection (a session/revision selection root via
    /// [`QuerySelection::publish`]). Its sole job is to bridge that gap.
    ///
    /// Once a successor protection exists, the bridge is redundant and must end
    /// promptly: an attempt record may be held for a long time (the compiler
    /// ledgers up to 256 completed attempts), and a lingering bridge pin would
    /// keep an otherwise-evictable terminal retained for the record's whole life.
    /// [`release_result_lease`](QueryRequestAttempt::release_result_lease) ends it
    /// explicitly the instant selection has pinned the same terminal — a pure
    /// narrowing of protection with no window in which the result is unprotected.
    /// Callers that never register a successor simply drop the attempt, releasing
    /// the bridge at teardown. Held behind a `Mutex` so the release is a `&self`
    /// operation on the possibly-shared attempt record.
    ///
    /// `None` for aborted or setup-failed requests, which carry no terminal.
    /// Type-erased over the family key so the attempt need not name `K`.
    result_lease: Mutex<Option<Box<dyn ObservedLease>>>,
}

impl<V: fmt::Debug> fmt::Debug for QueryRequestAttempt<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueryRequestAttempt")
            .field("id", &self.id)
            .field("origin_request", &self.origin_request)
            .field("execution", &self.execution)
            .field("terminal", &self.terminal)
            .field("abort", &self.abort)
            .field("dependencies", &self.dependencies)
            .field("inputs", &self.inputs)
            .field("work", &self.work)
            .field("nested_attempts", &self.nested_attempts)
            .field("result_lease", &lock(&self.result_lease).is_some())
            .finish()
    }
}

impl<V> QueryRequestAttempt<V> {
    /// Session-local request identity.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Caller-owned origin identity frozen by the runtime for this request.
    pub fn origin_request_id(&self) -> u64 {
        self.origin_request
    }

    /// Computed, reused, joined, or aborted lifecycle classification.
    pub fn execution(&self) -> RequestExecution {
        self.execution
    }

    /// Published terminal, absent for a non-terminal abort.
    pub fn terminal(&self) -> Option<&Arc<QueryTerminal<V>>> {
        self.terminal.as_ref()
    }

    /// Non-terminal control result, present exactly for aborted attempts.
    pub fn abort(&self) -> Option<&QueryAbort> {
        self.abort.as_ref()
    }

    /// Exact dependency prefix observed by this request.
    pub fn dependencies(&self) -> &[Observation] {
        &self.dependencies
    }

    /// Exact input prefix observed directly, or propagated through an aborted
    /// nested request which had no terminal dependency to represent it.
    pub fn inputs(&self) -> &[InputObservation] {
        &self.inputs
    }

    /// Exact structural-work prefix owned by this request.
    ///
    /// Reuse and join attempts carry no historical work. Computed and aborted
    /// attempts retain work performed by their own task before termination.
    pub fn work(&self) -> &[(Arc<str>, u64)] {
        &self.work
    }

    /// Runtime-owned nested request ledger in deterministic completion order.
    /// Descendants precede the parent nested request which demanded them.
    pub fn nested_attempts(&self) -> &[NestedQueryAttempt] {
        &self.nested_attempts
    }

    /// Origin terminal revision for reuse/join provenance.
    pub fn origin_revision(&self) -> Option<Revision> {
        self.terminal.as_ref().map(|terminal| terminal.revision())
    }

    /// Ends the attempt-carried bridge lease now that a successor protection
    /// holds the result.
    ///
    /// Call this immediately after registering a successor protection for the
    /// terminal — a [`QuerySelection::publish`] root, typically — and never
    /// before. The bridge lease exists only to keep the result retained between
    /// request completion and that registration; once the successor pins the same
    /// terminal, continuing to hold the bridge just pins an otherwise-evictable
    /// terminal for as long as the (possibly long-lived, ledgered) attempt record
    /// survives. Releasing here is a pure narrowing of protection: the successor
    /// still holds the terminal, so there is no instant in which it is
    /// unprotected. Idempotent, and a no-op for aborted or setup-failed attempts
    /// that never held a bridge lease. The released pin's own decrement and single
    /// retention pass run here, outside the attempt lock.
    pub fn release_result_lease(&self) {
        let released = lock(&self.result_lease).take();
        drop(released);
    }

    /// Converts this attempt to the legacy terminal-or-abort call shape.
    pub fn into_result(self) -> Result<Arc<QueryTerminal<V>>, QueryAbort> {
        match (self.terminal, self.abort) {
            (Some(terminal), None) => Ok(terminal),
            (None, Some(abort)) => Err(abort),
            _ => unreachable!("request attempt has exactly one outcome"),
        }
    }
}

/// Cooperative request cancellation.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

#[derive(Debug, Default)]
struct CancellationInner {
    canceled: AtomicBool,
    next_watcher: AtomicU64,
    watchers: Mutex<Vec<(u64, Weak<WaitCell>)>>,
}

impl CancellationToken {
    /// Creates a live token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels this waiter/request and wakes its parked joins.
    pub fn cancel(&self) {
        if self.inner.canceled.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut watchers = lock(&self.inner.watchers);
        watchers.retain(|(_, watcher)| {
            let Some(waiter) = watcher.upgrade() else {
                return false;
            };
            waiter.notify_all();
            true
        });
    }

    /// Whether cancellation has been requested.
    pub fn is_canceled(&self) -> bool {
        self.inner.canceled.load(Ordering::Acquire)
    }

    fn watch(&self, waiter: &Arc<WaitCell>) -> u64 {
        let id = self.inner.next_watcher.fetch_add(1, Ordering::Relaxed);
        lock(&self.inner.watchers).push((id, Arc::downgrade(waiter)));
        if self.is_canceled() {
            waiter.notify_all();
        }
        id
    }

    fn unwatch(&self, id: u64) {
        lock(&self.inner.watchers).retain(|(current, _)| *current != id);
    }
}

/// Deterministic work performed while validating retained query terminals.
///
/// These counters describe semantic operations rather than elapsed time. The
/// hot path accumulates them on the rooted request's [`Task`]; the runtime
/// merges one aggregate when that task completes. This keeps the counters
/// continuously available without a shared atomic update per validation edge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidationWork {
    /// Retained-terminal validation traversals started.
    pub traversals: u64,
    /// Traversals which proved their retained terminal current.
    pub successful_traversals: u64,
    /// Traversals which proved their retained terminal dirty.
    pub dirty_traversals: u64,
    /// Traversals aborted by cancellation or an engine error.
    pub aborted_traversals: u64,
    /// Direct source/input observations inspected.
    pub input_observations: u64,
    /// Retained dependency observations inspected.
    pub dependency_observations: u64,
    /// Exact node-incarnation resolutions requested by validation.
    pub registry_probes: u64,
    /// Resolutions which had to consult the shared incarnation registry.
    pub registry_index_lookups: u64,
    /// Registry probes which found no live exact incarnation.
    pub registry_misses: u64,
    /// Erased node validation entry-point visits.
    pub node_visits: u64,
    /// Recursive visits pruned because the incarnation was already active.
    pub active_cycle_prunes: u64,
    /// Node visits satisfied by a revision-scoped validation certificate.
    pub memo_hits: u64,
    /// Node visits which had to inspect or re-demand the node.
    pub memo_misses: u64,
    /// Memo misses with no usable exact certificate and live matching terminal.
    pub certificate_misses: u64,
    /// Memo misses caused only by a registered proof scope lacking the exact lease.
    pub proof_reacquisition_misses: u64,
    /// Exact registered-cone endorsement lookups.
    pub endorsement_probes: u64,
    /// Endorsement lookups which actually bypassed recursive or root validation.
    pub endorsement_hits: u64,
    /// Exact query-result terminal lease observations attempted by requests.
    pub terminal_lease_observations: u64,
    /// Query-result terminal observations already leased by the same task.
    pub duplicate_terminal_lease_observations: u64,
    /// Family-owned validation demands issued after a memo miss.
    pub demands: u64,
    /// Validation demands answered by a retained terminal.
    pub demand_reuses: u64,
    /// Validation demands which computed a terminal.
    pub demand_computes: u64,
    /// Validation demands which joined an in-flight terminal.
    pub demand_joins: u64,
    /// Validation demands which aborted without a terminal.
    pub demand_aborts: u64,
    /// Demands whose recorded incarnation had been retired or replaced.
    pub superseded: u64,
    /// New exact node/revision/stamp validation certificates published.
    pub certificates_published: u64,
}

impl ValidationWork {
    /// Returns the non-negative counter delta from an earlier snapshot.
    #[must_use]
    pub fn saturating_sub(self, earlier: Self) -> Self {
        macro_rules! subtract_fields {
            ($($field:ident),+ $(,)?) => {
                Self {
                    $($field: self.$field.saturating_sub(earlier.$field)),+
                }
            };
        }
        subtract_fields!(
            traversals,
            successful_traversals,
            dirty_traversals,
            aborted_traversals,
            input_observations,
            dependency_observations,
            registry_probes,
            registry_index_lookups,
            registry_misses,
            node_visits,
            active_cycle_prunes,
            memo_hits,
            memo_misses,
            certificate_misses,
            proof_reacquisition_misses,
            endorsement_probes,
            endorsement_hits,
            terminal_lease_observations,
            duplicate_terminal_lease_observations,
            demands,
            demand_reuses,
            demand_computes,
            demand_joins,
            demand_aborts,
            superseded,
            certificates_published,
        )
    }

    /// Adds another request's counters into this aggregate.
    pub fn saturating_add_assign(&mut self, other: Self) {
        macro_rules! add_fields {
            ($($field:ident),+ $(,)?) => {
                $(self.$field = self.$field.saturating_add(other.$field);)+
            };
        }
        add_fields!(
            traversals,
            successful_traversals,
            dirty_traversals,
            aborted_traversals,
            input_observations,
            dependency_observations,
            registry_probes,
            registry_index_lookups,
            registry_misses,
            node_visits,
            active_cycle_prunes,
            memo_hits,
            memo_misses,
            certificate_misses,
            proof_reacquisition_misses,
            endorsement_probes,
            endorsement_hits,
            terminal_lease_observations,
            duplicate_terminal_lease_observations,
            demands,
            demand_reuses,
            demand_computes,
            demand_joins,
            demand_aborts,
            superseded,
            certificates_published,
        );
    }
}

#[derive(Debug, Default)]
struct AtomicValidationWork {
    traversals: AtomicU64,
    successful_traversals: AtomicU64,
    dirty_traversals: AtomicU64,
    aborted_traversals: AtomicU64,
    input_observations: AtomicU64,
    dependency_observations: AtomicU64,
    registry_probes: AtomicU64,
    registry_index_lookups: AtomicU64,
    registry_misses: AtomicU64,
    node_visits: AtomicU64,
    active_cycle_prunes: AtomicU64,
    memo_hits: AtomicU64,
    memo_misses: AtomicU64,
    certificate_misses: AtomicU64,
    proof_reacquisition_misses: AtomicU64,
    endorsement_probes: AtomicU64,
    endorsement_hits: AtomicU64,
    terminal_lease_observations: AtomicU64,
    duplicate_terminal_lease_observations: AtomicU64,
    demands: AtomicU64,
    demand_reuses: AtomicU64,
    demand_computes: AtomicU64,
    demand_joins: AtomicU64,
    demand_aborts: AtomicU64,
    superseded: AtomicU64,
    certificates_published: AtomicU64,
}

impl AtomicValidationWork {
    fn snapshot(&self) -> ValidationWork {
        ValidationWork {
            traversals: self.traversals.load(Ordering::Relaxed),
            successful_traversals: self.successful_traversals.load(Ordering::Relaxed),
            dirty_traversals: self.dirty_traversals.load(Ordering::Relaxed),
            aborted_traversals: self.aborted_traversals.load(Ordering::Relaxed),
            input_observations: self.input_observations.load(Ordering::Relaxed),
            dependency_observations: self.dependency_observations.load(Ordering::Relaxed),
            registry_probes: self.registry_probes.load(Ordering::Relaxed),
            registry_index_lookups: self.registry_index_lookups.load(Ordering::Relaxed),
            registry_misses: self.registry_misses.load(Ordering::Relaxed),
            node_visits: self.node_visits.load(Ordering::Relaxed),
            active_cycle_prunes: self.active_cycle_prunes.load(Ordering::Relaxed),
            memo_hits: self.memo_hits.load(Ordering::Relaxed),
            memo_misses: self.memo_misses.load(Ordering::Relaxed),
            certificate_misses: self.certificate_misses.load(Ordering::Relaxed),
            proof_reacquisition_misses: self.proof_reacquisition_misses.load(Ordering::Relaxed),
            endorsement_probes: self.endorsement_probes.load(Ordering::Relaxed),
            endorsement_hits: self.endorsement_hits.load(Ordering::Relaxed),
            terminal_lease_observations: self.terminal_lease_observations.load(Ordering::Relaxed),
            duplicate_terminal_lease_observations: self
                .duplicate_terminal_lease_observations
                .load(Ordering::Relaxed),
            demands: self.demands.load(Ordering::Relaxed),
            demand_reuses: self.demand_reuses.load(Ordering::Relaxed),
            demand_computes: self.demand_computes.load(Ordering::Relaxed),
            demand_joins: self.demand_joins.load(Ordering::Relaxed),
            demand_aborts: self.demand_aborts.load(Ordering::Relaxed),
            superseded: self.superseded.load(Ordering::Relaxed),
            certificates_published: self.certificates_published.load(Ordering::Relaxed),
        }
    }

    fn take(&self) -> ValidationWork {
        ValidationWork {
            traversals: self.traversals.swap(0, Ordering::Relaxed),
            successful_traversals: self.successful_traversals.swap(0, Ordering::Relaxed),
            dirty_traversals: self.dirty_traversals.swap(0, Ordering::Relaxed),
            aborted_traversals: self.aborted_traversals.swap(0, Ordering::Relaxed),
            input_observations: self.input_observations.swap(0, Ordering::Relaxed),
            dependency_observations: self.dependency_observations.swap(0, Ordering::Relaxed),
            registry_probes: self.registry_probes.swap(0, Ordering::Relaxed),
            registry_index_lookups: self.registry_index_lookups.swap(0, Ordering::Relaxed),
            registry_misses: self.registry_misses.swap(0, Ordering::Relaxed),
            node_visits: self.node_visits.swap(0, Ordering::Relaxed),
            active_cycle_prunes: self.active_cycle_prunes.swap(0, Ordering::Relaxed),
            memo_hits: self.memo_hits.swap(0, Ordering::Relaxed),
            memo_misses: self.memo_misses.swap(0, Ordering::Relaxed),
            certificate_misses: self.certificate_misses.swap(0, Ordering::Relaxed),
            proof_reacquisition_misses: self.proof_reacquisition_misses.swap(0, Ordering::Relaxed),
            endorsement_probes: self.endorsement_probes.swap(0, Ordering::Relaxed),
            endorsement_hits: self.endorsement_hits.swap(0, Ordering::Relaxed),
            terminal_lease_observations: self
                .terminal_lease_observations
                .swap(0, Ordering::Relaxed),
            duplicate_terminal_lease_observations: self
                .duplicate_terminal_lease_observations
                .swap(0, Ordering::Relaxed),
            demands: self.demands.swap(0, Ordering::Relaxed),
            demand_reuses: self.demand_reuses.swap(0, Ordering::Relaxed),
            demand_computes: self.demand_computes.swap(0, Ordering::Relaxed),
            demand_joins: self.demand_joins.swap(0, Ordering::Relaxed),
            demand_aborts: self.demand_aborts.swap(0, Ordering::Relaxed),
            superseded: self.superseded.swap(0, Ordering::Relaxed),
            certificates_published: self.certificates_published.swap(0, Ordering::Relaxed),
        }
    }

    fn add(&self, work: ValidationWork) {
        macro_rules! add_fields {
            ($($field:ident),+ $(,)?) => {
                $(if work.$field != 0 {
                    self.$field.fetch_add(work.$field, Ordering::Relaxed);
                })+
            };
        }
        add_fields!(
            traversals,
            successful_traversals,
            dirty_traversals,
            aborted_traversals,
            input_observations,
            dependency_observations,
            registry_probes,
            registry_index_lookups,
            registry_misses,
            node_visits,
            active_cycle_prunes,
            memo_hits,
            memo_misses,
            certificate_misses,
            proof_reacquisition_misses,
            endorsement_probes,
            endorsement_hits,
            terminal_lease_observations,
            duplicate_terminal_lease_observations,
            demands,
            demand_reuses,
            demand_computes,
            demand_joins,
            demand_aborts,
            superseded,
            certificates_published,
        );
    }
}

/// Display-only query identities materialized by the runtime.
///
/// The byte counters record the UTF-8 length returned by
/// [`QueryKey::stable_identity`]. Family names are shared separately and are
/// not included. Typed keys remain authoritative for memo lookup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayIdentityMetrics {
    /// Identities created once for new memo-node incarnations.
    pub memo_node_materializations: u64,
    /// Formatted key bytes retained by new memo-node incarnations.
    pub memo_node_bytes: u64,
    /// Structured batch identities materialized only to render wait cycles.
    pub structured_wait_materializations: u64,
    /// Formatted structured-batch key bytes used to render wait cycles.
    pub structured_wait_bytes: u64,
    /// Identities created lazily when nested requests abort.
    pub abort_fallback_materializations: u64,
    /// Formatted key bytes created lazily when nested requests abort.
    pub abort_fallback_bytes: u64,
}

/// Deterministic structural execution counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeMetrics {
    /// New computations claimed.
    pub claims: u64,
    /// Compatible in-flight requests a task elected to share. A join declined
    /// as unwaitable is counted here too and again under `declined_joins`.
    pub joins: u64,
    /// Compatible retained terminals reused.
    pub reuses: u64,
    /// Retained-terminal validation work.
    pub validation: ValidationWork,
    /// Presentation-only query identity materialization.
    pub display_identities: DisplayIdentityMetrics,
    /// Query bodies which completed before publication checks.
    pub body_completions: u64,
    /// Recomputations whose observable stamp stayed red.
    pub red_publications: u64,
    /// New or observably changed green publications.
    pub green_publications: u64,
    /// Canceled computations or waiters.
    pub cancellations: u64,
    /// True dependency cycles reported.
    pub cycles: u64,
    /// Joins declined because waiting would have closed a wait-graph loop, each
    /// resolved by computing a private attempt.
    pub declined_joins: u64,
    /// Retained terminal attempts evicted.
    pub evictions: u64,
    /// Current retained terminal attempts.
    pub retained_terminals: u64,
    /// Current deterministic charge for retained terminal artifacts.
    pub retained_bytes: u64,
    /// Peak deterministic charge for retained terminal artifacts.
    pub peak_retained_bytes: u64,
    /// Current retained dependency and input observation edges.
    pub retained_dependency_pins: u64,
    /// Peak retained dependency and input observation edges.
    pub peak_retained_dependency_pins: u64,
    /// Configured runtime-wide soft artifact-charge budget.
    pub retained_byte_budget: u64,
    /// Configured runtime-wide soft dependency-observation budget.
    pub dependency_pin_budget: u64,
    /// Cross-family charge aggregations triggered by family-local watermarks.
    pub aggregate_retention_probes: u64,
    /// Deterministic family-local byte/pin quanta between aggregate probes.
    pub retained_byte_probe_quantum: u64,
    pub dependency_pin_probe_quantum: u64,
    /// Worst-case soft-budget detection overshoot from all currently live
    /// families publishing just below their next probe watermark.
    pub retained_byte_probe_overshoot_bound: u64,
    pub dependency_pin_probe_overshoot_bound: u64,
    /// Enforcement passes which began above the byte budget.
    pub retained_byte_pressure_events: u64,
    /// Enforcement passes which began above the dependency-pin budget.
    pub dependency_pin_pressure_events: u64,
    /// Byte-pressure passes unable to reach budget because all candidates were
    /// protected.
    pub retained_byte_overflow_events: u64,
    /// Dependency-pressure passes unable to reach budget because all candidates
    /// were protected.
    pub dependency_pin_overflow_events: u64,
    /// Largest protected byte overage observed after an enforcement pass.
    pub peak_retained_byte_overage: u64,
    /// Largest protected dependency-pin overage observed after enforcement.
    pub peak_dependency_pin_overage: u64,
    /// Terminals evicted while byte pressure was active.
    pub retained_byte_evictions: u64,
    /// Terminals evicted while dependency-pin pressure was active.
    pub dependency_pin_evictions: u64,
    /// Retention passes forced to grow past the configured terminal bound
    /// because every eviction candidate was a protected root (waiter, pin,
    /// request-scoped observation lease, or retained revision). This is the
    /// bounded-retention pressure marker: under a live closure exceeding the
    /// configured budget the policy is to grow and record the event here, never
    /// to evict a terminal the current computation still needs.
    pub retention_growth: u64,
    /// Retention enforcement passes run. Each `enforce_retention` call — one full
    /// eviction scan of a single family — increments this once. Batched
    /// task-lease teardown deliberately runs one pass per distinct family
    /// involved rather than one per released pin, so releasing N pins in one
    /// family raises this by one, not by N; this counter is what makes that
    /// linearity observable to tests.
    pub retention_enforcements: u64,
    /// Retention-queue entries examined by enforcement passes.
    ///
    /// Unlike `retention_enforcements`, this measures the work inside a pass.
    /// Publish-side batching keeps this linear when a live protected closure
    /// grows past its configured soft floor.
    pub retention_scan_entries: u64,
    /// Peak simultaneously executing query bodies.
    pub peak_active_bodies: u64,
    /// Terminals currently protected by rooted-request observation leases.
    pub active_task_leases: u64,
    /// Peak terminals simultaneously protected by rooted-request observation
    /// leases. Unlike RSS, this is an allocator-independent ownership gauge.
    pub peak_task_leases: u64,
    /// Terminals currently protected by session-owned retained pin sets.
    pub active_retained_pins: u64,
    /// Peak terminals simultaneously protected by session-owned retained pin
    /// sets. This includes atomic predecessor/successor handoff overlap.
    pub peak_retained_pins: u64,
    /// Times a parked joiner released its permit.
    pub donated_permits: u64,
    /// Immutable revision views currently retained by the runtime.
    pub retained_revisions: u64,
    /// Configured immutable-revision view bound.
    pub revision_limit: u64,
}

/// Bounded retained ownership for one query family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyRetention {
    /// Logical memo nodes currently owned by the family.
    pub memo_nodes: usize,
    /// Terminal attempts currently retained across those nodes.
    pub terminals: usize,
    /// Configured terminal bound. Protected roots — waiters, explicit pins,
    /// request-scoped observation leases, and retained revisions — may exceed it
    /// temporarily; the excess is reclaimed as those roots release.
    pub terminal_limit: usize,
}

#[derive(Debug, Default)]
struct Metrics {
    claims: AtomicU64,
    joins: AtomicU64,
    reuses: AtomicU64,
    validation: AtomicValidationWork,
    memo_node_identity_materializations: AtomicU64,
    memo_node_identity_bytes: AtomicU64,
    structured_wait_identity_materializations: AtomicU64,
    structured_wait_identity_bytes: AtomicU64,
    abort_fallback_identity_materializations: AtomicU64,
    abort_fallback_identity_bytes: AtomicU64,
    body_completions: AtomicU64,
    red_publications: AtomicU64,
    green_publications: AtomicU64,
    cancellations: AtomicU64,
    cycles: AtomicU64,
    declined_joins: AtomicU64,
    evictions: AtomicU64,
    retained_terminals: AtomicU64,
    peak_retained_bytes: AtomicU64,
    peak_retained_dependency_pins: AtomicU64,
    retained_byte_pressure_events: AtomicU64,
    dependency_pin_pressure_events: AtomicU64,
    aggregate_retention_probes: AtomicU64,
    retained_byte_overflow_events: AtomicU64,
    dependency_pin_overflow_events: AtomicU64,
    peak_retained_byte_overage: AtomicU64,
    peak_dependency_pin_overage: AtomicU64,
    retained_byte_evictions: AtomicU64,
    dependency_pin_evictions: AtomicU64,
    retention_growth: AtomicU64,
    retention_enforcements: AtomicU64,
    retention_scan_entries: AtomicU64,
    active_bodies: AtomicU64,
    peak_active_bodies: AtomicU64,
    active_task_leases: AtomicU64,
    peak_task_leases: AtomicU64,
    active_retained_pins: AtomicU64,
    peak_retained_pins: AtomicU64,
    donated_permits: AtomicU64,
}

impl Metrics {
    fn snapshot(
        &self,
        budgets: RetentionBudgets,
        retention: RuntimeRetentionSnapshot,
    ) -> RuntimeMetrics {
        RuntimeMetrics {
            claims: self.claims.load(Ordering::Relaxed),
            joins: self.joins.load(Ordering::Relaxed),
            reuses: self.reuses.load(Ordering::Relaxed),
            validation: self.validation.snapshot(),
            display_identities: DisplayIdentityMetrics {
                memo_node_materializations: self
                    .memo_node_identity_materializations
                    .load(Ordering::Relaxed),
                memo_node_bytes: self.memo_node_identity_bytes.load(Ordering::Relaxed),
                structured_wait_materializations: self
                    .structured_wait_identity_materializations
                    .load(Ordering::Relaxed),
                structured_wait_bytes: self.structured_wait_identity_bytes.load(Ordering::Relaxed),
                abort_fallback_materializations: self
                    .abort_fallback_identity_materializations
                    .load(Ordering::Relaxed),
                abort_fallback_bytes: self.abort_fallback_identity_bytes.load(Ordering::Relaxed),
            },
            body_completions: self.body_completions.load(Ordering::Relaxed),
            red_publications: self.red_publications.load(Ordering::Relaxed),
            green_publications: self.green_publications.load(Ordering::Relaxed),
            cancellations: self.cancellations.load(Ordering::Relaxed),
            cycles: self.cycles.load(Ordering::Relaxed),
            declined_joins: self.declined_joins.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            retained_terminals: self.retained_terminals.load(Ordering::Relaxed),
            retained_bytes: retention.retained_bytes,
            peak_retained_bytes: self.peak_retained_bytes.load(Ordering::Relaxed),
            retained_dependency_pins: retention.dependency_pins,
            peak_retained_dependency_pins: self
                .peak_retained_dependency_pins
                .load(Ordering::Relaxed),
            retained_byte_budget: budgets.retained_bytes,
            dependency_pin_budget: budgets.dependency_pins,
            aggregate_retention_probes: self.aggregate_retention_probes.load(Ordering::Relaxed),
            retained_byte_probe_quantum: retention_probe_quantum(
                budgets.retained_bytes,
                1024 * 1024,
                32 * 1024 * 1024,
            ),
            dependency_pin_probe_quantum: retention_probe_quantum(
                budgets.dependency_pins,
                4096,
                65_536,
            ),
            retained_byte_probe_overshoot_bound: retention.live_families.saturating_mul(
                retention_probe_quantum(budgets.retained_bytes, 1024 * 1024, 32 * 1024 * 1024),
            ),
            dependency_pin_probe_overshoot_bound: retention.live_families.saturating_mul(
                retention_probe_quantum(budgets.dependency_pins, 4096, 65_536),
            ),
            retained_byte_pressure_events: self
                .retained_byte_pressure_events
                .load(Ordering::Relaxed),
            dependency_pin_pressure_events: self
                .dependency_pin_pressure_events
                .load(Ordering::Relaxed),
            retained_byte_overflow_events: self
                .retained_byte_overflow_events
                .load(Ordering::Relaxed),
            dependency_pin_overflow_events: self
                .dependency_pin_overflow_events
                .load(Ordering::Relaxed),
            peak_retained_byte_overage: self.peak_retained_byte_overage.load(Ordering::Relaxed),
            peak_dependency_pin_overage: self.peak_dependency_pin_overage.load(Ordering::Relaxed),
            retained_byte_evictions: self.retained_byte_evictions.load(Ordering::Relaxed),
            dependency_pin_evictions: self.dependency_pin_evictions.load(Ordering::Relaxed),
            retention_growth: self.retention_growth.load(Ordering::Relaxed),
            retention_enforcements: self.retention_enforcements.load(Ordering::Relaxed),
            retention_scan_entries: self.retention_scan_entries.load(Ordering::Relaxed),
            peak_active_bodies: self.peak_active_bodies.load(Ordering::Relaxed),
            active_task_leases: self.active_task_leases.load(Ordering::Relaxed),
            peak_task_leases: self.peak_task_leases.load(Ordering::Relaxed),
            active_retained_pins: self.active_retained_pins.load(Ordering::Relaxed),
            peak_retained_pins: self.peak_retained_pins.load(Ordering::Relaxed),
            donated_permits: self.donated_permits.load(Ordering::Relaxed),
            retained_revisions: 0,
            revision_limit: REVISION_RETENTION_LIMIT as u64,
        }
    }

    fn body_entered(&self) {
        let active = self.active_bodies.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak_active_bodies.fetch_max(active, Ordering::AcqRel);
    }

    fn record_memo_node_identity(&self, bytes: usize) {
        self.memo_node_identity_materializations
            .fetch_add(1, Ordering::Relaxed);
        self.memo_node_identity_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn record_structured_wait_identity(&self, bytes: usize) {
        self.structured_wait_identity_materializations
            .fetch_add(1, Ordering::Relaxed);
        self.structured_wait_identity_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn record_abort_fallback_identity(&self, bytes: usize) {
        self.abort_fallback_identity_materializations
            .fetch_add(1, Ordering::Relaxed);
        self.abort_fallback_identity_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn body_left(&self) {
        self.active_bodies.fetch_sub(1, Ordering::AcqRel);
    }

    fn task_lease_acquired(&self) {
        let active = self.active_task_leases.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_task_leases.fetch_max(active, Ordering::Relaxed);
    }

    fn task_leases_released(&self, count: usize) {
        self.active_task_leases
            .fetch_sub(count as u64, Ordering::Relaxed);
    }

    fn retained_pin_acquired(&self) {
        let active = self.active_retained_pins.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_retained_pins.fetch_max(active, Ordering::Relaxed);
    }

    fn retained_pins_released(&self, count: usize) {
        self.active_retained_pins
            .fetch_sub(count as u64, Ordering::Relaxed);
    }
}

/// Shared execution substrate for all query families in one database.
#[derive(Debug, Clone)]
pub struct QueryRuntime {
    core: Arc<RuntimeCore>,
}

#[derive(Debug)]
struct RuntimeCore {
    identity: u64,
    permits: PermitBudget,
    /// Active task-to-task waits. Ordinary joins reuse their memo node's
    /// materialized identity. Structured edges retain only a shared batch table
    /// plus an item index, formatting the typed key only if the edge participates
    /// in a cycle which must be rendered.
    wait_graph: Mutex<BTreeMap<TaskId, BTreeMap<TaskId, WaitEdgeLabel>>>,
    family_names: Mutex<BTreeSet<Arc<str>>>,
    revisions: RwLock<RevisionStore>,
    nodes: RwLock<NodeRegistry>,
    retention_families: Mutex<BTreeMap<u64, Weak<dyn RetentionFamily>>>,
    retention_budgets: RetentionBudgets,
    /// Aggregate thresholds are consulted only after a family-local probe, not
    /// by every publication.
    next_retained_byte_sweep: AtomicU64,
    next_dependency_pin_sweep: AtomicU64,
    /// Stable family token at which the next pressure pass begins. Only the
    /// single claimed sweep owner mutates this cursor, so the runtime can carry
    /// round-robin fairness across separate pressure events without putting a
    /// lock on ordinary publication.
    retention_sweep_cursor: AtomicU64,
    retention_sweep_claimed: AtomicBool,
    retention_sweep_pending: AtomicBool,
    /// Spawned structured-batch workers currently alive across every root and
    /// nesting depth. The caller always executes one child inline; this global
    /// counter caps additional OS threads at `permits.maximum - 1`.
    batch_workers: AtomicUsize,
    next_task: AtomicU64,
    next_family: AtomicU64,
    next_node: AtomicU64,
    metrics: Metrics,
    #[cfg(test)]
    test_events: TestEvents,
    /// Deterministic interposition hook for concurrency tests. When installed it
    /// is invoked at the retention-handoff sites (publication exposure, join
    /// waiter→pin handoff, reuse candidate discovery) so a test can drive a
    /// concurrent enforcer into the exact window the atomic handoff is meant to
    /// close. Never present in non-test builds and free of cost otherwise.
    #[cfg(test)]
    interpose: InterposeSlot,
}

#[derive(Debug, Default)]
struct NodeRegistry {
    // The index never owns a node. Its entries are removed by the matching
    // node's destructor, so registration and cleanup each address one
    // incarnation rather than scanning the live population.
    entries: HashMap<u64, RegisteredNode, BuildHasherDefault<IncarnationHasher>>,
    // Deterministic structural work: every inspection of a stored registry
    // value charges this counter. The counter is shared with the values so a
    // retain-style population traversal cannot hide behind one API call.
    #[cfg(test)]
    entry_visits: Arc<AtomicUsize>,
}

/// Identity hashing for runtime-owned, monotonically assigned incarnation IDs.
///
/// These keys are never caller-controlled, and the registry does not expose
/// iteration order. This hasher is deliberately private to `NodeRegistry`:
/// caller-controlled typed family keys must keep their randomized hashers.
/// Using the ID directly gives exact registry operations expected O(1) lookup
/// without paying a general-purpose string-resistant hashing cost.
#[derive(Debug, Default)]
struct IncarnationHasher(u64);

impl Hasher for IncarnationHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // `Hash for u64` uses `write_u64`; this fallback only keeps the Hasher
        // implementation total if the standard hashing route ever changes.
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        self.0 = hash;
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

#[derive(Debug)]
struct RegisteredNode {
    node: Weak<dyn ErasedNode>,
    #[cfg(test)]
    entry_visits: Arc<AtomicUsize>,
}

/// One family-local FIFO exposed to the runtime only while aggregate retention
/// is under pressure. Ordinary publication never consults this registry.
trait RetentionFamily: fmt::Debug + Send + Sync {
    /// Evicts the oldest currently unprotected terminal in this family. Stale
    /// FIFO entries are discarded as they are encountered.
    fn evict_one(&self) -> bool;

    /// Exact family-local byte/pin gauges without walking terminal nodes.
    fn charge_snapshot(&self) -> FamilyChargeSnapshot;
}

struct RetentionFamilyDriver {
    name: Arc<str>,
    evict_one: Box<dyn Fn() -> bool + Send + Sync>,
    charge_snapshot: Box<dyn Fn() -> FamilyChargeSnapshot + Send + Sync>,
}

impl fmt::Debug for RetentionFamilyDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetentionFamilyDriver")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl RetentionFamily for RetentionFamilyDriver {
    fn evict_one(&self) -> bool {
        (self.evict_one)()
    }

    fn charge_snapshot(&self) -> FamilyChargeSnapshot {
        (self.charge_snapshot)()
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct FamilyChargeSnapshot {
    retained_bytes: u64,
    dependency_pins: u64,
}

#[derive(Debug, Default, Clone, Copy)]
struct RuntimeRetentionSnapshot {
    retained_bytes: u64,
    dependency_pins: u64,
    live_families: u64,
}

impl RegisteredNode {
    fn ptr_eq(&self, node: &Weak<dyn ErasedNode>) -> bool {
        #[cfg(test)]
        self.entry_visits.fetch_add(1, Ordering::Relaxed);
        Weak::ptr_eq(&self.node, node)
    }

    // This is the operation used by the former scan-on-insert implementation.
    // Keeping its structural charge on the stored value makes that regression
    // deterministic without adding production telemetry.
    #[cfg(test)]
    #[allow(dead_code)]
    fn strong_count(&self) -> usize {
        self.entry_visits.fetch_add(1, Ordering::Relaxed);
        self.node.strong_count()
    }
}

impl NodeRegistry {
    fn get(&self, incarnation: &u64) -> Option<&Weak<dyn ErasedNode>> {
        self.entries.get(incarnation).map(|entry| &entry.node)
    }

    fn insert(&mut self, incarnation: u64, node: Weak<dyn ErasedNode>) -> bool {
        let node = RegisteredNode {
            node,
            #[cfg(test)]
            entry_visits: self.entry_visits.clone(),
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = self.entries.entry(incarnation) {
            entry.insert(node);
            true
        } else {
            false
        }
    }

    fn remove(&mut self, incarnation: u64, node: &Weak<dyn ErasedNode>) {
        if self
            .entries
            .get(&incarnation)
            .is_some_and(|registered| registered.ptr_eq(node))
        {
            self.entries.remove(&incarnation);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Test-only holder for the interposition hook, with a `Debug` impl so the
/// enclosing `RuntimeCore` can still derive `Debug`.
#[cfg(test)]
#[derive(Default)]
struct InterposeSlot(Mutex<Option<Arc<dyn Fn(InterposeSite) + Send + Sync>>>);

#[cfg(test)]
impl fmt::Debug for InterposeSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InterposeSlot")
            .finish_non_exhaustive()
    }
}

/// The retention-handoff sites a concurrency test may interpose on.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterposeSite {
    /// A freshly published terminal has just been exposed and enqueued; with the
    /// fix its request lease pin is already held.
    PublishExposed,
    /// A joiner has transferred waiter protection into a pin and decremented the
    /// waiter count.
    JoinHandoff,
    /// The first reuse candidate has been discovered and pinned under the node
    /// lock, before recursive validation releases the lock.
    ReuseDiscovered,
    /// A node joiner observed a still-computing attempt and live cancellation
    /// token while holding the predicate lock, immediately before parking.
    NodeJoinPark,
    /// A lifecycle waiter observed neither completion nor cancellation while
    /// holding the predicate lock, immediately before atomically parking.
    HandoffCommitPark,
    /// Aggregate retention has finished a pass and is about to hand sweep
    /// ownership to any publisher which arrived concurrently.
    RetentionSweepRelease,
    /// A retained dependency's exact node has been upgraded from the
    /// incarnation registry and missed its validation memo, immediately before
    /// re-demanding the key from family-owned authority.
    RetainedDependencyDemand,
}

const REVISION_RETENTION_LIMIT: usize = 64;

#[derive(Debug)]
struct RevisionStore {
    entries: BTreeMap<u64, RevisionEntry>,
    retired_through: u64,
}

impl RevisionStore {
    /// The stamp of `input` in `revision_id`, resolving through the revision's
    /// overlay chain. `None` means the leaf is absent (a recorded-absent optional
    /// leaf reads as absent here).
    fn input_stamp(&self, revision_id: u64, input: &InputIdentity) -> Option<u64> {
        self.entries.get(&revision_id)?.inputs.stamp(input)
    }

    /// Whether `input` is present in `revision_id` (through the overlay chain).
    fn input_present(&self, revision_id: u64, input: &InputIdentity) -> bool {
        self.input_stamp(revision_id, input).is_some()
    }
}

#[derive(Debug)]
struct RevisionEntry {
    revision: Revision,
    inputs: Arc<RevisionInputs>,
    active_requests: usize,
}

/// Overlay chains longer than this are compacted into one complete map at the
/// next successor publication, so lookup depth stays bounded even if a caller
/// publishes an unexpectedly long chain.
const OVERLAY_COMPACTION_DEPTH: usize = 16;

/// The immutable leaf view of one revision: either a complete map, or a sparse
/// successor overlay whose unresolved leaves are STRUCTURALLY INHERITED from the
/// parent's input node by `Arc` (RUE-1112). The parent input node is owned by the
/// overlay itself, so the parent's revision-store ENTRY may retire while every
/// child's logical view stays complete — retention never has to pin ancestors.
#[derive(Debug)]
enum RevisionInputs {
    Full(Arc<BTreeMap<InputIdentity, u64>>),
    Overlay {
        parent: Arc<RevisionInputs>,
        delta: Arc<BTreeMap<InputIdentity, u64>>,
        depth: usize,
    },
}

impl RevisionInputs {
    /// Resolve one leaf: the newest overlay delta wins, then ancestors in order.
    fn stamp(&self, input: &InputIdentity) -> Option<u64> {
        match self {
            Self::Full(map) => map.get(input).copied(),
            Self::Overlay { parent, delta, .. } => {
                delta.get(input).copied().or_else(|| parent.stamp(input))
            }
        }
    }

    fn depth(&self) -> usize {
        match self {
            Self::Full(_) => 0,
            Self::Overlay { depth, .. } => *depth,
        }
    }

    /// Materialize the complete logical leaf map (used only for compaction).
    fn flatten(&self) -> BTreeMap<InputIdentity, u64> {
        match self {
            Self::Full(map) => map.as_ref().clone(),
            Self::Overlay { parent, delta, .. } => {
                let mut merged = parent.flatten();
                for (input, stamp) in delta.iter() {
                    merged.insert(input.clone(), *stamp);
                }
                merged
            }
        }
    }
}

struct RevisionLease {
    core: Arc<RuntimeCore>,
    revision: Revision,
}

impl Drop for RevisionLease {
    fn drop(&mut self) {
        let mut revisions = write(&self.core.revisions);
        let entry = revisions
            .entries
            .get_mut(&self.revision.id)
            .filter(|entry| entry.revision == self.revision)
            .expect("active request pins its published revision");
        entry.active_requests -= 1;
        self.core.enforce_revision_retention(&mut revisions);
    }
}

#[cfg(test)]
#[allow(dead_code)]
#[derive(Debug, Default)]
struct TestEvents {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl QueryRuntime {
    /// Creates a runtime with one shared structured concurrency budget.
    pub fn new(max_concurrency: usize) -> Self {
        Self::with_retention_budgets(max_concurrency, RetentionBudgets::default())
    }

    /// Creates a runtime with explicit runtime-wide soft retention budgets.
    ///
    /// This constructor is primarily useful for deterministic policy tests and
    /// benchmark calibration. A zero budget is valid: newly published work is
    /// reclaimed immediately when it has no live protection.
    pub fn with_retention_budgets(
        max_concurrency: usize,
        retention_budgets: RetentionBudgets,
    ) -> Self {
        assert!(max_concurrency > 0, "query concurrency must be nonzero");
        Self {
            core: Arc::new(RuntimeCore {
                identity: NEXT_RUNTIME_ID.fetch_add(1, Ordering::Relaxed),
                permits: PermitBudget::new(max_concurrency),
                wait_graph: Mutex::new(BTreeMap::new()),
                family_names: Mutex::new(BTreeSet::new()),
                revisions: RwLock::new(RevisionStore {
                    entries: BTreeMap::new(),
                    retired_through: 0,
                }),
                nodes: RwLock::new(NodeRegistry::default()),
                retention_families: Mutex::new(BTreeMap::new()),
                retention_budgets,
                next_retained_byte_sweep: AtomicU64::new(
                    retention_budgets.retained_bytes.saturating_add(1),
                ),
                next_dependency_pin_sweep: AtomicU64::new(
                    retention_budgets.dependency_pins.saturating_add(1),
                ),
                retention_sweep_cursor: AtomicU64::new(1),
                retention_sweep_claimed: AtomicBool::new(false),
                retention_sweep_pending: AtomicBool::new(false),
                batch_workers: AtomicUsize::new(0),
                next_task: AtomicU64::new(1),
                next_family: AtomicU64::new(1),
                next_node: AtomicU64::new(1),
                metrics: Metrics::default(),
                #[cfg(test)]
                test_events: TestEvents::default(),
                #[cfg(test)]
                interpose: InterposeSlot::default(),
            }),
        }
    }

    /// Creates a typed family with deterministic FIFO terminal retention.
    ///
    /// This convenience form assigns no heap-owned success-value charge. A
    /// caller retaining heap-backed values should use the corresponding
    /// `*_and_retained_charge` constructor or set an exact charge on each
    /// [`QueryOutput`]. The terminal envelope, diagnostics, work, and
    /// observations are always charged by the runtime.
    pub fn family<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Eq + Send + Sync + 'static,
    {
        self.family_with_equality(stable_name, retention_limit, PartialEq::eq)
    }

    /// Creates a typed family with family-owned canonical value equality.
    ///
    /// Compiler families use this when their retained success values do not
    /// implement blanket `Eq`, or when only a canonical projection participates
    /// in red/green publication. This convenience form assigns no heap-owned
    /// success-value charge; see [`Self::family_with_equality_and_retained_charge`].
    pub fn family_with_equality<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_with_optional_evaluator(stable_name, retention_limit, value_equal, |_| 0, None)
    }

    /// Creates an unregistered family with an estimator for heap-owned success
    /// value data.
    pub fn family_with_equality_and_retained_charge<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_with_optional_evaluator(
            stable_name,
            retention_limit,
            value_equal,
            retained_value_charge,
            None,
        )
    }

    /// Creates a typed family with a canonical family-owned evaluator.
    ///
    /// Registered evaluators can be demanded by dependency validation from an
    /// exact retained key. They therefore support root-only red propagation;
    /// no per-request `FnOnce` is retained or reconstructed.
    pub fn family_with_evaluator<K, V, E>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        evaluator: E,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Eq + Send + Sync + 'static,
        E: Fn(&QueryContext, &QueryFamily<K, V>, &K) -> Result<QueryOutput<V>, QueryAbort>
            + Send
            + Sync
            + 'static,
    {
        self.family_with_equality_and_evaluator(
            stable_name,
            retention_limit,
            PartialEq::eq,
            evaluator,
        )
    }

    /// Creates a typed family with family-owned equality and evaluator.
    pub fn family_with_equality_and_evaluator<K, V, E>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        evaluator: E,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        E: Fn(&QueryContext, &QueryFamily<K, V>, &K) -> Result<QueryOutput<V>, QueryAbort>
            + Send
            + Sync
            + 'static,
    {
        self.family_with_optional_evaluator(
            stable_name,
            retention_limit,
            value_equal,
            |_| 0,
            Some(Arc::new(evaluator)),
        )
    }

    /// Creates a registered family with an allocator-independent estimator for
    /// heap-owned success-value data. The estimator excludes inline `V` storage,
    /// which is already part of the terminal envelope.
    pub fn family_with_equality_and_evaluator_and_retained_charge<K, V, E>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
        evaluator: E,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        E: Fn(&QueryContext, &QueryFamily<K, V>, &K) -> Result<QueryOutput<V>, QueryAbort>
            + Send
            + Sync
            + 'static,
    {
        self.family_with_optional_evaluator(
            stable_name,
            retention_limit,
            value_equal,
            retained_value_charge,
            Some(Arc::new(evaluator)),
        )
    }

    fn family_with_optional_evaluator<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
        evaluator: Option<Arc<FamilyEvaluator<K, V>>>,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_inner(
            stable_name,
            retention_limit,
            value_equal,
            retained_value_charge,
            evaluator,
            false,
        )
    }

    /// Creates a typed family REGISTERED CONTENT-ADDRESSED: the family asserts
    /// every record is a pure function of its key alone, so no revision leaf
    /// can change the value behind an unchanged key. This registration is the
    /// SOLE minting authority for [`AdoptableTerminal`]
    /// ([`QueryFamily::adoptable_terminal`]); an ordinary input-dependent
    /// family can never endorse a stale value through terminal adoption.
    pub fn content_addressed_family_with_equality<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_inner(stable_name, retention_limit, value_equal, |_| 0, None, true)
    }

    /// Creates a content-addressed family with an estimator for heap-owned
    /// success-value data.
    pub fn content_addressed_family_with_equality_and_retained_charge<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_inner(
            stable_name,
            retention_limit,
            value_equal,
            retained_value_charge,
            None,
            true,
        )
    }

    fn family_inner<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
        evaluator: Option<Arc<FamilyEvaluator<K, V>>>,
        content_addressed: bool,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        let name = stable_name.into();
        if name.is_empty() {
            return Err(FamilyError::EmptyName);
        }
        if !lock(&self.core.family_names).insert(name.clone()) {
            return Err(FamilyError::DuplicateName(name));
        }
        let family_number = self.core.next_family.fetch_add(1, Ordering::Relaxed);
        let inner = Arc::new(FamilyInner {
            core: Arc::downgrade(&self.core),
            name: name.clone(),
            token: FamilyToken {
                runtime: self.core.identity,
                family: family_number,
            },
            content_addressed,
            retention_limit,
            value_equal,
            retained_value_charge,
            evaluator,
            nodes: ShardedNodeIndex::new(),
            retention: Mutex::new(FamilyRetentionQueue::new(self.core.retention_budgets)),
            retained_count: AtomicUsize::new(0),
            next_publish_sweep: AtomicUsize::new(retention_limit.saturating_add(1)),
            retained_nodes: AtomicUsize::new(0),
            retained_revisions: Mutex::new(BTreeMap::new()),
        });
        let weak_core = Arc::downgrade(&self.core);
        let weak_inner = Arc::downgrade(&inner);
        let charge_inner = Arc::downgrade(&inner);
        let retention_driver: Arc<dyn RetentionFamily> = Arc::new(RetentionFamilyDriver {
            name,
            evict_one: Box::new(move || {
                let Some(core) = weak_core.upgrade() else {
                    return false;
                };
                let Some(inner) = weak_inner.upgrade() else {
                    return false;
                };
                evict_one_from_family(&core, &inner)
            }),
            charge_snapshot: Box::new(move || {
                charge_inner
                    .upgrade()
                    .map_or_else(FamilyChargeSnapshot::default, |inner| {
                        family_charge_snapshot(&inner)
                    })
            }),
        });
        lock(&self.core.retention_families)
            .insert(family_number, Arc::downgrade(&retention_driver));
        Ok(QueryFamily {
            core: self.core.clone(),
            inner,
            retention_driver,
        })
    }

    /// Publishes the complete immutable leaf view for one revision.
    ///
    /// Re-publishing the same view is idempotent. A different view for an
    /// existing revision is rejected, so a pinned query can never observe a
    /// mutable input revision.
    pub fn publish_revision(
        &self,
        revision: Revision,
        inputs: impl IntoIterator<Item = (InputIdentity, u64)>,
    ) -> Result<(), RevisionError> {
        let mut exact = BTreeMap::new();
        for (input, stamp) in inputs {
            if stamp == 0 {
                return Err(RevisionError::ReservedInputStamp(input));
            }
            if let Some(previous) = exact.insert(input.clone(), stamp)
                && previous != stamp
            {
                return Err(RevisionError::ConflictingInput(input));
            }
        }
        let inputs = Arc::new(exact);
        let mut revisions = write(&self.core.revisions);
        match revisions.entries.get(&revision.id) {
            Some(previous) if previous.revision == revision => match previous.inputs.as_ref() {
                RevisionInputs::Full(previous_inputs)
                    if previous_inputs.as_ref() == inputs.as_ref() =>
                {
                    Ok(())
                }
                _ => Err(RevisionError::AlreadyPublished(revision)),
            },
            Some(_) => Err(RevisionError::AlreadyPublished(revision)),
            None => {
                if revision.id <= revisions.retired_through {
                    return Err(RevisionError::Retired(revision));
                }
                revisions.entries.insert(
                    revision.id,
                    RevisionEntry {
                        revision,
                        inputs: Arc::new(RevisionInputs::Full(inputs)),
                        active_requests: 0,
                    },
                );
                self.core.enforce_revision_retention(&mut revisions);
                Ok(())
            }
        }
    }

    /// Publishes a sparse immutable successor overlay revision (RUE-1112).
    ///
    /// The successor is a logically complete immutable input view: `delta` leaves
    /// resolve from the overlay, and every other leaf is STRUCTURALLY INHERITED
    /// from `parent`'s input node by `Arc` — only the delta is materialized. The
    /// delta may ADD new leaves and may RE-STAMP an existing leaf identity (the
    /// ordinary sparse representation of a changed derived input in a new
    /// immutable revision — the parent view itself is never mutated, and
    /// observers of a re-stamped identity are dirtied by red/green validation
    /// exactly as under a full publication). Removal is inexpressible.
    ///
    /// `parent` must be a currently retained revision of this runtime with the
    /// SAME compatibility token (an overlay never crosses a fresh observation
    /// generation), and `revision.id` must be strictly newer than `parent.id`
    /// (acyclic, monotonic lineage). The overlay owns the parent's input node, so
    /// the parent's revision-store entry may retire without breaking any child;
    /// chains deeper than [`OVERLAY_COMPACTION_DEPTH`] are compacted at
    /// publication. This layer does NOT verify domain additivity — a caller such
    /// as the compiler's trusted-toolchain lineage enforces its own
    /// strictly-additive source contract before publishing. Fresh generations
    /// keep using the complete [`Self::publish_revision`] path.
    pub fn publish_revision_overlay(
        &self,
        revision: Revision,
        parent: Revision,
        delta: impl IntoIterator<Item = (InputIdentity, u64)>,
    ) -> Result<(), RevisionError> {
        let mut delta_map = BTreeMap::new();
        for (input, stamp) in delta {
            if stamp == 0 {
                return Err(RevisionError::ReservedInputStamp(input));
            }
            if let Some(previous) = delta_map.insert(input.clone(), stamp)
                && previous != stamp
            {
                return Err(RevisionError::ConflictingInput(input));
            }
        }
        let delta_map = Arc::new(delta_map);
        let mut revisions = write(&self.core.revisions);
        let Some(parent_entry) = revisions
            .entries
            .get(&parent.id)
            .filter(|entry| entry.revision == parent)
        else {
            return Err(RevisionError::OverlayParentUnavailable(parent));
        };
        if revision.compatibility != parent.compatibility {
            return Err(RevisionError::IncompatibleOverlayParent(parent));
        }
        if revision.id <= parent.id {
            return Err(RevisionError::NonMonotonicOverlay(revision));
        }
        let parent_inputs = parent_entry.inputs.clone();
        let inputs = if parent_inputs.depth() >= OVERLAY_COMPACTION_DEPTH {
            // Compact a deep chain into one complete map so lookup depth stays
            // bounded; the merged map is the same logical view.
            let mut merged = parent_inputs.flatten();
            for (input, stamp) in delta_map.iter() {
                merged.insert(input.clone(), *stamp);
            }
            Arc::new(RevisionInputs::Full(Arc::new(merged)))
        } else {
            Arc::new(RevisionInputs::Overlay {
                depth: parent_inputs.depth() + 1,
                parent: parent_inputs,
                delta: delta_map.clone(),
            })
        };
        match revisions.entries.get(&revision.id) {
            Some(previous) if previous.revision == revision => {
                // Idempotent only for the identical logical view: same parent
                // input node and identical delta (or the identical compaction).
                let identical = match (previous.inputs.as_ref(), inputs.as_ref()) {
                    (
                        RevisionInputs::Overlay {
                            parent: previous_parent,
                            delta: previous_delta,
                            ..
                        },
                        RevisionInputs::Overlay { parent, delta, .. },
                    ) => Arc::ptr_eq(previous_parent, parent) && previous_delta == delta,
                    (RevisionInputs::Full(previous_map), RevisionInputs::Full(map)) => {
                        previous_map == map
                    }
                    _ => false,
                };
                if identical {
                    Ok(())
                } else {
                    Err(RevisionError::AlreadyPublished(revision))
                }
            }
            Some(_) => Err(RevisionError::AlreadyPublished(revision)),
            None => {
                if revision.id <= revisions.retired_through {
                    return Err(RevisionError::Retired(revision));
                }
                revisions.entries.insert(
                    revision.id,
                    RevisionEntry {
                        revision,
                        inputs,
                        active_requests: 0,
                    },
                );
                self.core.enforce_revision_retention(&mut revisions);
                Ok(())
            }
        }
    }

    /// Executes or reuses one top-level query in a newly allocated task.
    pub fn query<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        compute: F,
    ) -> Result<Arc<QueryTerminal<V>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        self.request(family, revision, key, cancellation, compute)
            .into_result()
    }

    /// Executes one top-level request and retains its lifecycle even on abort.
    pub fn request<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        compute: F,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        self.request_with_origin(family, revision, key, cancellation, None, compute)
    }

    /// Executes a top-level request with a caller-owned provenance identity.
    ///
    /// The runtime freezes this identity into computed terminals and all
    /// lifecycle attempts, avoiding a separately retained origin registry in
    /// compatibility adapters.
    pub fn request_with_origin<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        origin_request: Option<u64>,
        compute: F,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        self.request_impl(
            family,
            revision,
            key,
            cancellation,
            origin_request,
            Some(compute),
        )
    }

    /// Executes a top-level request through a family-owned evaluator.
    pub fn request_registered<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.request_registered_with_origin(family, revision, key, cancellation, None)
    }

    /// Executes a registered top-level request with caller-owned provenance.
    pub fn request_registered_with_origin<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        origin_request: Option<u64>,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "closure-free requests require a registered evaluator"
        );
        self.request_impl::<K, V, fn(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>>(
            family,
            revision,
            key,
            cancellation,
            origin_request,
            None,
        )
    }

    fn request_impl<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        origin_request: Option<u64>,
        compute: Option<F>,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        if HandoffCallbackGuard::active() {
            return QueryRequestAttempt {
                id: 0,
                origin_request: origin_request.unwrap_or(0),
                execution: RequestExecution::Aborted,
                terminal: None,
                abort: Some(QueryAbort::Canceled),
                dependencies: Arc::from([]),
                inputs: Arc::from([]),
                work: Arc::from([]),
                nested_attempts: Arc::from([]),
                result_lease: Mutex::new(None),
            };
        }
        if !Arc::ptr_eq(&self.core, &family.core) {
            return QueryRequestAttempt {
                id: 0,
                origin_request: origin_request.unwrap_or(0),
                execution: RequestExecution::Aborted,
                terminal: None,
                abort: Some(QueryAbort::ForeignRuntime),
                dependencies: Arc::from([]),
                inputs: Arc::from([]),
                work: Arc::from([]),
                nested_attempts: Arc::from([]),
                result_lease: Mutex::new(None),
            };
        }
        let id = self.core.next_task.fetch_add(1, Ordering::Relaxed);
        let origin_request = origin_request.unwrap_or(id);
        let Some(_revision_lease) = self.core.pin_revision(revision) else {
            return QueryRequestAttempt {
                id,
                origin_request,
                execution: RequestExecution::Aborted,
                terminal: None,
                abort: Some(QueryAbort::UnpublishedRevision(revision)),
                dependencies: Arc::from([]),
                inputs: Arc::from([]),
                work: Arc::from([]),
                nested_attempts: Arc::from([]),
                result_lease: Mutex::new(None),
            };
        };
        let task = Arc::new(Task {
            id: TaskId(id),
            core: self.core.clone(),
            revision,
            cancellation,
            owns_permit: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
            ancestry: Arc::from([]),
            nested_attempts: Mutex::new(Vec::new()),
            nested_attempt_filters: Mutex::new(Vec::new()),
            validation_endorsements: Mutex::new(Vec::new()),
            batch_validation_authority: None,
            validation_proofs: Mutex::new(Vec::new()),
            validation_work: AtomicValidationWork::default(),
            leases: Mutex::new(TaskLeases::default()),
            query_cache: Mutex::new(TaskQueryCache::default()),
            observed_handoffs: Mutex::new(Vec::new()),
            checked_handoffs: Mutex::new(HashSet::new()),
            #[cfg(test)]
            handoff_validation_visits: AtomicUsize::new(0),
            #[cfg(test)]
            validation_endorsement_index_probes: AtomicUsize::new(0),
        });
        let result = match compute {
            Some(compute) => family.query_task(task.clone(), key, origin_request, compute),
            None => family.query_task_registered(task.clone(), key, origin_request),
        };
        let result = match result {
            TaskQueryResult::Terminal { terminal, work, .. } if task.cancellation.is_canceled() => {
                task.discard_observed_handoffs();
                TaskQueryResult::Aborted {
                    abort: QueryAbort::Canceled,
                    dependencies: terminal.dependencies().to_vec(),
                    inputs: terminal.inputs().to_vec(),
                    work,
                }
            }
            TaskQueryResult::Terminal {
                terminal,
                execution,
                work,
            } => match task.commit_handoffs() {
                Ok(()) => TaskQueryResult::Terminal {
                    terminal,
                    execution,
                    work,
                },
                Err(RootHandoffCommitFailure::Canceled) => TaskQueryResult::Aborted {
                    abort: QueryAbort::Canceled,
                    dependencies: terminal.dependencies().to_vec(),
                    inputs: terminal.inputs().to_vec(),
                    work,
                },
                Err(RootHandoffCommitFailure::Invalidated) => TaskQueryResult::Aborted {
                    abort: QueryAbort::Canceled,
                    dependencies: terminal.dependencies().to_vec(),
                    inputs: terminal.inputs().to_vec(),
                    work,
                },
                Err(RootHandoffCommitFailure::Panicked(payload)) => resume_unwind(payload),
            },
            aborted @ TaskQueryResult::Aborted { .. } => {
                task.discard_observed_handoffs();
                aborted
            }
        };
        let nested_attempts: Arc<[NestedQueryAttempt]> = lock(&task.nested_attempts).clone().into();
        match result {
            TaskQueryResult::Terminal {
                terminal,
                execution,
                work,
            } => {
                let origin_request = terminal.origin_request_id();
                // Carry a live result lease out of the request. The producing task
                // is still alive here (it drops at the end of this function), and
                // it still holds its own request-scoped lease on `terminal`, so
                // `terminal` is currently retained. Pinning it now hands protection
                // from the task's about-to-drop lease to the returned attempt with
                // no gap. The caller keeps the attempt alive until after it
                // registers a successor protection (selection root), closing the
                // promotion window entirely.
                let result_lease = family
                    .pin_terminal(&terminal)
                    .ok()
                    .map(|pin| Box::new(pin) as Box<dyn ObservedLease>);
                QueryRequestAttempt {
                    id,
                    origin_request,
                    execution,
                    dependencies: terminal.dependencies.clone(),
                    inputs: terminal.inputs.clone(),
                    work: work.into(),
                    terminal: Some(terminal),
                    abort: None,
                    nested_attempts,
                    result_lease: Mutex::new(result_lease),
                }
            }
            TaskQueryResult::Aborted {
                abort,
                dependencies,
                inputs,
                work,
            } => QueryRequestAttempt {
                id,
                origin_request,
                execution: RequestExecution::Aborted,
                terminal: None,
                abort: Some(abort),
                dependencies: dependencies.into(),
                inputs: inputs.into(),
                work: work.into(),
                nested_attempts,
                result_lease: Mutex::new(None),
            },
        }
    }

    /// Returns a point-in-time structural metrics snapshot.
    pub fn metrics(&self) -> RuntimeMetrics {
        let retention = self.core.retention_snapshot();
        self.core.record_retention_peaks(retention);
        let mut metrics = self
            .core
            .metrics
            .snapshot(self.core.retention_budgets, retention);
        metrics.retained_revisions = read(&self.core.revisions).entries.len() as u64;
        metrics
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn wait_for_metrics(&self, predicate: impl Fn(RuntimeMetrics) -> bool) {
        let mut generation = lock(&self.core.test_events.generation);
        while !predicate(self.metrics()) {
            generation = wait(&self.core.test_events.changed, generation);
        }
    }

    /// Installs a deterministic interposition hook for concurrency tests.
    #[cfg(test)]
    fn set_interpose(&self, hook: Arc<dyn Fn(InterposeSite) + Send + Sync>) {
        *lock(&self.core.interpose.0) = Some(hook);
    }

    /// Removes any installed interposition hook.
    #[cfg(test)]
    #[allow(dead_code)]
    fn clear_interpose(&self) {
        *lock(&self.core.interpose.0) = None;
    }
}

/// An immutable revision cannot be changed after publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionError {
    /// The revision identity is already bound to different leaf values.
    AlreadyPublished(Revision),
    /// One publication supplied two different values for the same exact leaf.
    ConflictingInput(InputIdentity),
    /// Stamp zero is reserved for a recorded absent optional leaf.
    ReservedInputStamp(InputIdentity),
    /// This publication identity is older than the bounded retired watermark.
    Retired(Revision),
    /// A successor overlay named a parent revision that is not currently a
    /// retained revision of this runtime lineage.
    OverlayParentUnavailable(Revision),
    /// A successor overlay named a parent from a different compatibility
    /// generation; an overlay never crosses a fresh observation generation.
    IncompatibleOverlayParent(Revision),
    /// A successor overlay's revision id is not strictly newer than its
    /// parent's; the overlay lineage is acyclic and monotonic.
    NonMonotonicOverlay(Revision),
}

/// Invalid family declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyError {
    /// Family names are stable identities and cannot be empty.
    EmptyName,
    /// A runtime already owns this stable family name.
    DuplicateName(Arc<str>),
}

/// A typed memo table sharing its runtime's scheduler and wait graph.
pub struct QueryFamily<K: QueryKey, V: Clone + Send + Sync + 'static> {
    core: Arc<RuntimeCore>,
    inner: Arc<FamilyInner<K, V>>,
    retention_driver: Arc<dyn RetentionFamily>,
}

/// Non-owning handle for evaluator graphs with cross-family back edges.
pub struct WeakQueryFamily<K: QueryKey, V: Clone + Send + Sync + 'static> {
    core: Weak<RuntimeCore>,
    inner: Weak<FamilyInner<K, V>>,
    retention_driver: Weak<dyn RetentionFamily>,
}

impl<K, V> Clone for WeakQueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            inner: self.inner.clone(),
            retention_driver: self.retention_driver.clone(),
        }
    }
}

impl<K, V> WeakQueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// Upgrades this handle while the family remains owned by its database.
    pub fn upgrade(&self) -> Option<QueryFamily<K, V>> {
        Some(QueryFamily {
            core: self.core.upgrade()?,
            inner: self.inner.upgrade()?,
            retention_driver: self.retention_driver.upgrade()?,
        })
    }
}

impl<K, V> fmt::Debug for QueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueryFamily")
            .field("name", &self.inner.name)
            .field("retention_limit", &self.inner.retention_limit)
            .finish_non_exhaustive()
    }
}

impl<K, V> Clone for QueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            inner: self.inner.clone(),
            retention_driver: self.retention_driver.clone(),
        }
    }
}

/// Number of memo-index shards per family. A power of two so shard selection
/// is a mask; fixed rather than host-derived so the index's shape is identical
/// on every machine. 32 shards keep expected same-shard collisions rare for
/// the worker counts Rue schedules (currently ≤ 16) while costing only a few
/// kilobytes per family.
const NODE_INDEX_SHARDS: usize = 32;

/// Sharded typed-key memo index (RUE-1241).
///
/// Every memo hit needs only to clone an existing `Arc`, yet a single-mutex
/// index serializes all hits in the family — the dominant measured lock convoy
/// at high worker counts after the registry and revision stores became
/// concurrent. A reader-writer lock was prototyped and measured flat: its
/// read-side acquisition is dearer than an uncontended mutex, which taxed the
/// serial path without repaying at `-j10`. Sharding instead keeps the cheap
/// mutex acquisition and removes cross-key contention by splitting the key
/// space; hits on the *same* key still serialize briefly on one shard, which
/// preserves the hit/removal race rules unchanged per shard.
///
/// Shard selection hashes with this index's own keyed `RandomState`, and each
/// shard map keeps its own keyed `RandomState` — the
/// adversarial-resistance property of the previous single map is preserved,
/// never weakened to a fixed or truncated hash.
///
/// Lock-order contract: a shard guard may be held while acquiring the global
/// node-incarnation registry or a node's state lock (mirroring the previous
/// whole-index mutex); nothing acquires a shard guard while holding either of
/// those, and no path holds two shard guards at once.
struct ShardedNodeIndex<K: QueryKey, V: Clone + Send + Sync + 'static> {
    selector: RandomState,
    shards: [Mutex<AHashMap<K, Arc<Node<K, V>>>>; NODE_INDEX_SHARDS],
}

impl<K, V> ShardedNodeIndex<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn new() -> Self {
        Self {
            selector: RandomState::new(),
            shards: std::array::from_fn(|_| Mutex::new(AHashMap::new())),
        }
    }

    fn shard_index(&self, key: &K) -> usize {
        self.selector.hash_one(key) as usize & (NODE_INDEX_SHARDS - 1)
    }

    /// Locks and returns the one shard that can own `key`. Exclusive per
    /// shard: get-miss-insert sequences and the removal re-checks (`users`,
    /// `attempts`, pointer identity) stay atomic under this guard exactly as
    /// they were under the whole-index mutex.
    fn shard(&self, key: &K) -> MutexGuard<'_, AHashMap<K, Arc<Node<K, V>>>> {
        lock(&self.shards[self.shard_index(key)])
    }
}

struct FamilyInner<K: QueryKey, V: Clone + Send + Sync + 'static> {
    core: Weak<RuntimeCore>,
    name: Arc<str>,
    token: FamilyToken,
    /// Registration policy: the family asserts every record is a pure function
    /// of its key alone, so no revision leaf can change the value behind an
    /// unchanged key. This registration is the SOLE minting authority for
    /// [`AdoptableTerminal`] — an ordinary input-dependent family can never
    /// endorse a stale value through adoption.
    content_addressed: bool,
    retention_limit: usize,
    value_equal: fn(&V, &V) -> bool,
    retained_value_charge: fn(&V) -> u64,
    evaluator: Option<Arc<FamilyEvaluator<K, V>>>,
    // Hashed typed-key memo index, sharded so hits on unrelated keys do not
    // convoy on one lock (RUE-1241). Exact `K` equality is authoritative: each
    // shard map is keyed by the typed key itself, so hash collisions resolve
    // through `Eq` and never conflate distinct keys. The maps are unordered:
    // eviction order lives in `retention` below (the memo index never encoded
    // eviction order), so no companion order structure is required.
    nodes: ShardedNodeIndex<K, V>,
    retention: Mutex<FamilyRetentionQueue<K, V>>,
    retained_count: AtomicUsize,
    /// Retained-count watermark for the next publish-side sweep. A pass that
    /// finds only protected entries doubles this watermark, so growing a live
    /// closure examines O(N) entries in total rather than rescanning every
    /// prefix. Releases still force an immediate pass because they can make an
    /// existing terminal newly evictable.
    next_publish_sweep: AtomicUsize,
    retained_nodes: AtomicUsize,
    retained_revisions: Mutex<BTreeMap<Revision, usize>>,
}

impl<K, V> fmt::Debug for FamilyInner<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FamilyInner")
            .field("name", &self.name)
            .field("retention_limit", &self.retention_limit)
            .field("has_evaluator", &self.evaluator.is_some())
            .finish_non_exhaustive()
    }
}

impl<K, V> Drop for FamilyInner<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        let Some(core) = self.core.upgrade() else {
            return;
        };
        let mut terminals = 0_u64;
        for shard in &self.nodes.shards {
            for node in lock(shard).values() {
                for attempt in &lock(&node.state).attempts {
                    if let AttemptState::Terminal { .. } = &attempt.state {
                        terminals += 1;
                    }
                }
            }
        }
        if terminals > 0 {
            core.metrics
                .retained_terminals
                .fetch_sub(terminals, Ordering::Relaxed);
        }
    }
}

type FamilyEvaluator<K, V> = dyn Fn(&QueryContext, &QueryFamily<K, V>, &K) -> Result<QueryOutput<V>, QueryAbort>
    + Send
    + Sync;

struct Node<K, V> {
    /// Typed key owning this node, retained so eviction can locate the node in
    /// the hashed memo index without a linear scan.
    key: K,
    identity: NodeIdentity,
    incarnation: u64,
    // Both links are weak: the runtime and node therefore remain free to die
    // independently, while the destructor can still find its registry.
    registry_core: Weak<RuntimeCore>,
    // Allocation identity makes removal ABA-safe even if an incarnation slot
    // were ever occupied by a different node.
    registry_self: Weak<dyn ErasedNode>,
    users: AtomicUsize,
    wait: Arc<WaitCell>,
    demand: Option<Arc<dyn Fn(Arc<Task>, u64) -> ValidationDemand<V> + Send + Sync>>,
    state: Mutex<NodeState<V>>,
}

/// Outcome of re-demanding a retained dependency from family-owned authority.
///
/// Re-demand resolves the family's memo by KEY, while a retained dependency
/// observation names one exact node INCARNATION. Retention removes a node from
/// its family's key memo once the node holds no terminals and no live users
/// (`NodeLease::drop` and terminal eviction), after which the next request for
/// that key builds a fresh incarnation whose stamps restart at one. A recursive
/// validation walk holds the old incarnation alive through the incarnation
/// registry, so both nodes can be live at once — and the fresh incarnation's
/// first stamp collides numerically with the retained observation's stamp
/// without describing the same computation.
enum ValidationDemand<V> {
    /// The family still memoizes this key at the demanded incarnation, so the
    /// result proves whether the retained observation is still current.
    Current(TaskQueryResult<V>),
    /// The family's memo for this key is a different incarnation. Nothing the
    /// fresh node publishes can witness the retained observation, so the
    /// dependent is dirty and recomputes against the current graph.
    Superseded,
}

impl<K, V> fmt::Debug for Node<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Node")
            .field("identity", &self.identity)
            .field("incarnation", &self.incarnation)
            .finish_non_exhaustive()
    }
}

impl<K, V> Drop for Node<K, V> {
    fn drop(&mut self) {
        let Some(core) = self.registry_core.upgrade() else {
            return;
        };
        write(&core.nodes).remove(self.incarnation, &self.registry_self);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidationCertificate {
    revision: Revision,
    stamp: u64,
    terminal_revision: Revision,
    registered_only: bool,
}

const INLINE_ACTIVE_VALIDATIONS: usize = 8;

/// Runtime incarnations on the current validation recursion path.
///
/// Validation cones are normally shallow, so retaining the first few entries
/// inline avoids constructing a tree for every traversal. Unusually deep cones
/// promote once to a hash set, preserving bounded membership checks instead of
/// making adversarial dependency depth quadratic.
#[derive(Debug, Default)]
enum ActiveValidations {
    #[default]
    Empty,
    Inline {
        entries: [u64; INLINE_ACTIVE_VALIDATIONS],
        len: u8,
    },
    Hashed(AHashSet<u64>),
}

impl ActiveValidations {
    fn insert(&mut self, incarnation: u64) -> bool {
        match self {
            Self::Empty => {
                let mut entries = [0; INLINE_ACTIVE_VALIDATIONS];
                entries[0] = incarnation;
                *self = Self::Inline { entries, len: 1 };
                true
            }
            Self::Inline { entries, len } => {
                let occupied = &entries[..usize::from(*len)];
                if occupied.contains(&incarnation) {
                    return false;
                }
                if usize::from(*len) < INLINE_ACTIVE_VALIDATIONS {
                    entries[usize::from(*len)] = incarnation;
                    *len += 1;
                    return true;
                }

                let mut promoted = AHashSet::with_capacity(INLINE_ACTIVE_VALIDATIONS + 1);
                promoted.extend(occupied.iter().copied());
                let inserted = promoted.insert(incarnation);
                assert!(
                    inserted,
                    "a distinct incarnation must remain distinct during cycle-set promotion"
                );
                *self = Self::Hashed(promoted);
                true
            }
            Self::Hashed(entries) => entries.insert(incarnation),
        }
    }

    fn remove(&mut self, incarnation: &u64) -> bool {
        match self {
            Self::Empty => false,
            Self::Inline { entries, len } => {
                let Some(position) = entries[..usize::from(*len)]
                    .iter()
                    .position(|entry| entry == incarnation)
                else {
                    return false;
                };
                *len -= 1;
                entries[position] = entries[usize::from(*len)];
                if *len == 0 {
                    *self = Self::Empty;
                }
                true
            }
            Self::Hashed(entries) => entries.remove(incarnation),
        }
    }
}

trait ErasedNode: fmt::Debug + Send + Sync {
    /// Exact runtime-local incarnation represented by this erased handle.
    fn incarnation(&self) -> u64;

    fn validated_stamp(
        &self,
        _core: &RuntimeCore,
        task: &Arc<Task>,
        active: &mut ActiveValidations,
    ) -> Result<Option<u64>, QueryAbort>;

    /// Records one exact validation certificate and reports whether it was new.
    fn mark_validated(&self, certificate: ValidationCertificate) -> bool;

    /// The typed node behind this erased handle, for the family-owned
    /// exact-terminal adoption path ([`QueryFamily::observe_adopted_terminal`])
    /// to recover its own `Node<K, V>` from the incarnation index without a
    /// key lookup.
    fn as_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync>;
}

impl<K, V> ErasedNode for Node<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn incarnation(&self) -> u64 {
        self.incarnation
    }

    fn validated_stamp(
        &self,
        core: &RuntimeCore,
        task: &Arc<Task>,
        active: &mut ActiveValidations,
    ) -> Result<Option<u64>, QueryAbort> {
        task.validation_work
            .node_visits
            .fetch_add(1, Ordering::Relaxed);
        if !active.insert(self.incarnation) {
            task.validation_work
                .active_cycle_prunes
                .fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }
        let mut proof_reacquisition_miss = false;
        {
            let state = lock(&self.state);
            if let Some(certificate) = &state.validated_at
                && certificate.revision == task.revision
                && state.attempts.iter().any(|attempt| {
                    matches!(
                        &attempt.state,
                        AttemptState::Terminal { terminal, .. }
                            if terminal.stamp == certificate.stamp
                                && terminal.revision == certificate.terminal_revision
                    )
                })
            {
                // A registered-cone endorsement is also a retention proof. A
                // memo may skip validation in that scope only when this task
                // has already leased the exact terminal or a live fallback owns
                // an equal node/stamp representative. Final promotion walks the
                // selected representative's own transitive cone.
                let endorsement_authority = task.validation_endorsement_authority_at(
                    self.incarnation,
                    certificate.stamp,
                    certificate.terminal_revision,
                );
                if matches!(
                    endorsement_authority,
                    ValidationEndorsementAuthority::Inactive
                        | ValidationEndorsementAuthority::TaskLocal
                        | ValidationEndorsementAuthority::Borrowed
                ) {
                    if endorsement_authority != ValidationEndorsementAuthority::Inactive {
                        task.record_validation_endorsement_hit();
                    }
                    if !certificate.registered_only {
                        task.taint_validation_proofs();
                    }
                    task.validation_work
                        .memo_hits
                        .fetch_add(1, Ordering::Relaxed);
                    active.remove(&self.incarnation);
                    return Ok(Some(certificate.stamp));
                }
                proof_reacquisition_miss = true;
            }
        }
        task.validation_work
            .memo_misses
            .fetch_add(1, Ordering::Relaxed);
        if proof_reacquisition_miss {
            task.validation_work
                .proof_reacquisition_misses
                .fetch_add(1, Ordering::Relaxed);
        } else {
            task.validation_work
                .certificate_misses
                .fetch_add(1, Ordering::Relaxed);
        }
        if let Some(demand) = &self.demand {
            task.validation_work.demands.fetch_add(1, Ordering::Relaxed);
            #[cfg(test)]
            core.interpose(InterposeSite::RetainedDependencyDemand);
            let request_id = task.next_nested_request();
            let ValidationDemand::Current(result) = demand(task.clone(), request_id) else {
                // Retention retired this incarnation from its family's key memo.
                // The key is still computable, but only as a fresh incarnation
                // whose stamps are unrelated to the retained observation, so the
                // dependent is dirty rather than provably green.
                task.validation_work
                    .superseded
                    .fetch_add(1, Ordering::Relaxed);
                active.remove(&self.incarnation);
                return Ok(None);
            };
            task.record_nested(request_id, || self.identity.clone(), &result);
            active.remove(&self.incarnation);
            return match result {
                TaskQueryResult::Terminal {
                    terminal,
                    execution,
                    ..
                } => {
                    match execution {
                        RequestExecution::Reused => &task.validation_work.demand_reuses,
                        RequestExecution::Computed => &task.validation_work.demand_computes,
                        RequestExecution::Joined => &task.validation_work.demand_joins,
                        RequestExecution::Aborted => &task.validation_work.demand_aborts,
                    }
                    .fetch_add(1, Ordering::Relaxed);
                    // Retention can retire this incarnation between the memo
                    // check above and the request itself. The answering
                    // incarnation is authoritative: a stamp published by any
                    // other node is a different counter and never witnesses this
                    // observation, however numerically equal it looks.
                    if terminal.node_incarnation != self.incarnation {
                        task.validation_work
                            .superseded
                            .fetch_add(1, Ordering::Relaxed);
                        return Ok(None);
                    }
                    // A validation-only computation or join returns an exact
                    // stamp but does not transfer its candidate pin into the
                    // task's proof cone. Mark every enclosing certificate for
                    // one repair traversal: now that the terminal is published,
                    // an ordinary Reused validation can capture it.
                    if execution != RequestExecution::Reused {
                        task.defer_validation_proofs();
                    }
                    Ok(Some(terminal.stamp))
                }
                // A dependency that cannot be produced under this revision
                // because one of its inputs is gone does not abort the
                // dependent: it makes the dependent's retained terminal
                // invalid, so the dependent recomputes against the current
                // graph (RUE-1137 item 8).
                //
                // The distinction is which side of the edge the demand is on.
                // Demanding an input a *computation* has not yet discovered is
                // the external-input protocol asking the host to supply it.
                // Re-demanding an input a *retained terminal already observed*,
                // and finding it absent, is an ordinary red edge — the removal
                // is the change. Without this, editing an import so a module
                // leaves the graph aborts every dependent instead of
                // recomputing it.
                //
                // Only missing inputs are absorbed. Cancellation, dependency
                // cycles, and engine invariant violations still propagate: they
                // say nothing about whether the retained terminal is stale.
                TaskQueryResult::Aborted {
                    abort: QueryAbort::MissingInput(_),
                    ..
                } => {
                    task.validation_work
                        .demand_aborts
                        .fetch_add(1, Ordering::Relaxed);
                    Ok(None)
                }
                TaskQueryResult::Aborted { abort, .. } => {
                    task.validation_work
                        .demand_aborts
                        .fetch_add(1, Ordering::Relaxed);
                    Err(abort)
                }
            };
        }
        // An externally supplied evaluator cannot participate in a reusable
        // registered-only proof: the runtime cannot re-demand and pin its
        // complete dependency cone from family-owned authority.
        task.taint_validation_proofs();
        let candidates = lock(&self.state)
            .attempts
            .iter()
            .rev()
            .filter_map(|attempt| match &attempt.state {
                AttemptState::Terminal { terminal, .. } => Some(terminal.clone()),
                AttemptState::Computing { .. } => None,
            })
            .collect::<Vec<_>>();
        let stamp = candidates.into_iter().try_fold(None, |stamp, terminal| {
            if stamp.is_some() {
                Ok(stamp)
            } else if core.valid_for_revision_inner(&terminal, task, active)? {
                Ok(Some(terminal.stamp))
            } else {
                Ok(None)
            }
        });
        active.remove(&self.incarnation);
        stamp
    }

    fn as_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }

    fn mark_validated(&self, certificate: ValidationCertificate) -> bool {
        let mut state = lock(&self.state);
        if state.attempts.iter().any(|attempt| {
            matches!(
                &attempt.state,
                AttemptState::Terminal { terminal, .. }
                    if terminal.stamp == certificate.stamp
                        && terminal.revision == certificate.terminal_revision
            )
        }) {
            if state.validated_at.as_ref() == Some(&certificate) {
                return false;
            }
            state.validated_at = Some(certificate);
            return true;
        }
        false
    }
}

#[derive(Debug)]
struct WaitCell {
    cv: Condvar,
    generation: Mutex<u64>,
}

impl WaitCell {
    fn notify_all(&self) {
        *lock(&self.generation) += 1;
        self.cv.notify_all();
    }

    fn wait_until(
        &self,
        mut predicate: impl FnMut() -> bool,
        #[cfg(test)] mut before_park: impl FnMut(),
    ) {
        let mut generation = lock(&self.generation);
        while !predicate() {
            #[cfg(test)]
            before_park();
            generation = wait(&self.cv, generation);
        }
    }
}

#[derive(Debug)]
struct NodeState<V> {
    next_attempt: u64,
    next_stamp: u64,
    attempts: VecDeque<Attempt<V>>,
    /// The exact terminal revision and stamp already proven against this
    /// immutable request revision.
    ///
    /// This is a verification skip only. The matching terminal must still be
    /// retained, and the first visit in every revision continues to validate
    /// direct inputs and the complete dependency cone authoritatively.
    validated_at: Option<ValidationCertificate>,
}

#[derive(Debug)]
struct Attempt<V> {
    id: u64,
    revision: Revision,
    state: AttemptState<V>,
}

#[derive(Debug)]
enum AttemptState<V> {
    Computing {
        owner: TaskId,
        waiters: usize,
    },
    Terminal {
        terminal: Arc<QueryTerminal<V>>,
        waiters: usize,
        handoffs: Arc<AttemptHandoffLifecycle>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FamilyToken {
    runtime: u64,
    family: u64,
}

#[derive(Debug)]
struct RetentionEntry<K, V> {
    node: Weak<Node<K, V>>,
    attempt: u64,
}

struct FamilyRetentionQueue<K, V> {
    entries: VecDeque<RetentionEntry<K, V>>,
    retained_bytes: u64,
    dependency_pins: u64,
    next_byte_probe: u64,
    next_pin_probe: u64,
    byte_probe_quantum: u64,
    pin_probe_quantum: u64,
}

impl<K, V> FamilyRetentionQueue<K, V> {
    fn new(budgets: RetentionBudgets) -> Self {
        let byte_probe_quantum =
            retention_probe_quantum(budgets.retained_bytes, 1024 * 1024, 32 * 1024 * 1024);
        let pin_probe_quantum = retention_probe_quantum(budgets.dependency_pins, 4096, 65_536);
        Self {
            entries: VecDeque::new(),
            retained_bytes: 0,
            dependency_pins: 0,
            next_byte_probe: byte_probe_quantum,
            next_pin_probe: pin_probe_quantum,
            byte_probe_quantum,
            pin_probe_quantum,
        }
    }

    fn publish(
        &mut self,
        entry: RetentionEntry<K, V>,
        retained_bytes: u64,
        dependency_pins: u64,
    ) -> bool {
        self.entries.push_back(entry);
        self.retained_bytes = self.retained_bytes.saturating_add(retained_bytes);
        self.dependency_pins = self.dependency_pins.saturating_add(dependency_pins);
        let probe = self.retained_bytes >= self.next_byte_probe
            || self.dependency_pins >= self.next_pin_probe;
        if probe {
            self.next_byte_probe = next_probe(self.retained_bytes, self.byte_probe_quantum);
            self.next_pin_probe = next_probe(self.dependency_pins, self.pin_probe_quantum);
        }
        probe
    }

    fn remove_charge(&mut self, retained_bytes: u64, dependency_pins: u64) {
        self.retained_bytes = self
            .retained_bytes
            .checked_sub(retained_bytes)
            .expect("retained byte charge releases exactly once");
        self.dependency_pins = self
            .dependency_pins
            .checked_sub(dependency_pins)
            .expect("retained dependency-pin charge releases exactly once");
        // A sweep can reclaim most of a family's charge after its publication
        // watermark advanced. Rebase both probes to the new live charge so a
        // subsequent regrowth cannot hide below the stale high watermark.
        self.next_byte_probe = next_probe(self.retained_bytes, self.byte_probe_quantum);
        self.next_pin_probe = next_probe(self.dependency_pins, self.pin_probe_quantum);
    }
}

fn retention_probe_quantum(budget: u64, normal_minimum: u64, normal_maximum: u64) -> u64 {
    if budget < normal_minimum {
        // Tiny deterministic policy tests need correspondingly exact probes.
        return (budget / 64).max(1);
    }
    (budget / 128).clamp(normal_minimum, normal_maximum)
}

fn next_probe(current: u64, quantum: u64) -> u64 {
    current
        .checked_div(quantum)
        .unwrap_or(u64::MAX)
        .saturating_add(1)
        .saturating_mul(quantum)
}

fn evict_one_from_family<K, V>(core: &Arc<RuntimeCore>, family: &Arc<FamilyInner<K, V>>) -> bool
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    let mut retention = lock(&family.retention);
    let mut remaining = retention.entries.len();
    while remaining > 0 {
        remaining -= 1;
        let entry = retention
            .entries
            .pop_front()
            .expect("retention scan is nonempty");
        core.metrics
            .retention_scan_entries
            .fetch_add(1, Ordering::Relaxed);
        let Some(node) = entry.node.upgrade() else {
            continue;
        };
        let mut state = lock(&node.state);
        let Some(index) = state
            .attempts
            .iter()
            .position(|item| item.id == entry.attempt)
        else {
            continue;
        };
        let protected = match &state.attempts[index].state {
            AttemptState::Computing { .. } => true,
            AttemptState::Terminal {
                terminal, waiters, ..
            } => {
                *waiters > 0
                    || terminal.pins.load(Ordering::Acquire) > 0
                    || lock(&family.retained_revisions).contains_key(&terminal.revision)
            }
        };
        if protected {
            drop(state);
            retention.entries.push_back(entry);
            continue;
        }
        let removed = state
            .attempts
            .remove(index)
            .expect("retention selected an existing attempt");
        let (terminal, handoffs) = match removed.state {
            AttemptState::Terminal {
                terminal, handoffs, ..
            } => (terminal, handoffs),
            AttemptState::Computing { .. } => unreachable!(),
        };
        let empty = state.attempts.is_empty();
        drop(state);
        core.metrics.evictions.fetch_add(1, Ordering::Relaxed);
        core.metrics
            .retained_terminals
            .fetch_sub(1, Ordering::Relaxed);
        family.retained_count.fetch_sub(1, Ordering::Relaxed);
        retention.remove_charge(terminal.retained_charge, terminal.dependency_pin_charge);
        if empty && node.users.load(Ordering::Acquire) == 0 {
            let mut nodes = family.nodes.shard(&node.key);
            if node.users.load(Ordering::Acquire) == 0
                && lock(&node.state).attempts.is_empty()
                && nodes
                    .get(&node.key)
                    .is_some_and(|candidate| Arc::ptr_eq(candidate, &node))
            {
                nodes.remove(&node.key);
                family.retained_nodes.fetch_sub(1, Ordering::Relaxed);
            }
        }
        drop(retention);
        handoffs.abort();
        return true;
    }
    false
}

fn family_charge_snapshot<K, V>(family: &FamilyInner<K, V>) -> FamilyChargeSnapshot
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    let retention = lock(&family.retention);
    FamilyChargeSnapshot {
        retained_bytes: retention.retained_bytes,
        dependency_pins: retention.dependency_pins,
    }
}

/// Result of attempting to share another task's in-flight attempt.
enum JoinOutcome<K: QueryKey, V: Clone + Send + Sync + 'static> {
    /// The attempt reached a terminal and its protection was handed to this pin.
    Joined(u64, Arc<AttemptHandoffLifecycle>, TerminalPin<K, V>),
    /// The attempt disappeared (detached or evicted); rediscover from scratch.
    Retry,
    /// Waiting for this attempt's owner would close a wait-graph loop. The
    /// caller claims a private attempt rather than failing the request.
    Contended,
}

enum TaskQueryResult<V> {
    Terminal {
        terminal: Arc<QueryTerminal<V>>,
        execution: RequestExecution,
        work: Vec<(Arc<str>, u64)>,
    },
    Aborted {
        abort: QueryAbort,
        dependencies: Vec<Observation>,
        inputs: Vec<InputObservation>,
        work: Vec<(Arc<str>, u64)>,
    },
}

impl<V> TaskQueryResult<V> {
    fn into_result(self) -> Result<Arc<QueryTerminal<V>>, QueryAbort> {
        match self {
            Self::Terminal { terminal, .. } => Ok(terminal),
            Self::Aborted { abort, .. } => Err(abort),
        }
    }
}

struct NodeLease<K: QueryKey, V: Clone + Send + Sync + 'static> {
    family: Weak<FamilyInner<K, V>>,
    key: K,
    node: Arc<Node<K, V>>,
}

impl<K, V> Drop for NodeLease<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        if self.node.users.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        if !lock(&self.node.state).attempts.is_empty() {
            return;
        }
        let Some(family) = self.family.upgrade() else {
            return;
        };
        let mut nodes = family.nodes.shard(&self.key);
        if self.node.users.load(Ordering::Acquire) == 0
            && lock(&self.node.state).attempts.is_empty()
            && nodes
                .get(&self.key)
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &self.node))
        {
            nodes.remove(&self.key);
            family.retained_nodes.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl<K, V> QueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// Creates a non-owning handle suitable for evaluator dependency graphs.
    pub fn downgrade(&self) -> WeakQueryFamily<K, V> {
        WeakQueryFamily {
            core: Arc::downgrade(&self.core),
            inner: Arc::downgrade(&self.inner),
            retention_driver: Arc::downgrade(&self.retention_driver),
        }
    }

    /// Returns deterministic retained-ownership gauges for this family.
    pub fn retention(&self) -> FamilyRetention {
        FamilyRetention {
            memo_nodes: self.inner.retained_nodes.load(Ordering::Relaxed),
            terminals: self.inner.retained_count.load(Ordering::Relaxed),
            terminal_limit: self.inner.retention_limit,
        }
    }

    /// Whether a currently live memo node has a key matching `predicate`.
    ///
    /// This exact O(n) Phase 1 bridge supports lifetime-coupled input
    /// identities. ADR-0063 Phase 7 replaces it for high-cardinality families.
    pub fn any_retained_key(&self, mut predicate: impl FnMut(&K) -> bool) -> bool {
        // Shards are visited one at a time; the probe was never a cross-key
        // atomic snapshot (its answer could go stale the moment the old
        // whole-index lock released), so per-shard consistency is unchanged.
        self.inner
            .nodes
            .shards
            .iter()
            .any(|shard| lock(shard).keys().any(&mut predicate))
    }

    /// Whether the exact key currently owns a live memo node in this family.
    ///
    /// "Retained" means that the key is still present in the family's memo
    /// index. It does not mean that a terminal for the current revision is
    /// valid, selected, or protected from eviction; those properties are
    /// decided by the ordinary query request. This exact-key probe preserves
    /// the memo node's lifetime semantics while using the index's O(1)
    /// average-case lookup instead of enumerating every retained key.
    pub fn contains_retained_key(&self, key: &K) -> bool {
        self.inner.nodes.shard(key).contains_key(key)
    }

    /// Caller-owned provenance identities for every retained reusable terminal.
    pub fn retained_origin_request_ids(&self) -> BTreeSet<u64> {
        let mut nodes = Vec::new();
        for shard in &self.inner.nodes.shards {
            nodes.extend(lock(shard).values().cloned());
        }
        nodes
            .iter()
            .flat_map(|node| {
                lock(&node.state)
                    .attempts
                    .iter()
                    .filter_map(|attempt| match &attempt.state {
                        AttemptState::Terminal { terminal, .. } => {
                            Some(terminal.origin_request_id())
                        }
                        AttemptState::Computing { .. } => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn node(&self, key: K) -> Result<NodeLease<K, V>, QueryAbort> {
        // ADR-0063 Phase 7 hashed memo index. Lookup is O(1) expected on the
        // typed key's `Hash`, but exact `K` equality remains authoritative:
        // `HashMap<K, _>` resolves any hash collision through `Eq`, so distinct
        // keys that hash alike still map to distinct nodes. Display identity is
        // never consulted for lookup. The guard covers the whole hit/miss
        // sequence including the `users` increment below, so removal's
        // `users == 0` re-check under the same shard guard cannot race a hit.
        let mut nodes = self.inner.nodes.shard(&key);
        let node = if let Some(node) = nodes.get(&key) {
            node.clone()
        } else {
            let stable_key = key.stable_identity();
            self.core
                .metrics
                .record_memo_node_identity(stable_key.len());
            let stable_key = stable_key.into_boxed_str();
            let incarnation = self.core.next_node.fetch_add(1, Ordering::Relaxed);
            let demand = self.inner.evaluator.as_ref().map(|_| {
                let core = self.core.clone();
                let family = Arc::downgrade(&self.inner);
                let retention_driver = self.retention_driver.clone();
                let key = key.clone();
                Arc::new(move |task: Arc<Task>, origin_request: u64| {
                    let Some(inner) = family.upgrade() else {
                        return ValidationDemand::Current(TaskQueryResult::Aborted {
                            abort: QueryAbort::ForeignRuntime,
                            dependencies: Vec::new(),
                            inputs: Vec::new(),
                            work: Vec::new(),
                        });
                    };
                    // Re-demand answers by key, so it can only witness this
                    // observation while the family's memo for the key is still
                    // this incarnation. Checking first keeps a superseded node
                    // from resurrecting its key speculatively — which would both
                    // charge retention for work the dependent must redo anyway
                    // and park this task behind whichever request is already
                    // computing the fresh incarnation.
                    let superseded = {
                        let nodes = inner.nodes.shard(&key);
                        nodes
                            .get(&key)
                            .is_none_or(|current| current.incarnation != incarnation)
                    };
                    if superseded {
                        return ValidationDemand::Superseded;
                    }
                    ValidationDemand::Current(
                        QueryFamily {
                            core: core.clone(),
                            inner,
                            retention_driver: retention_driver.clone(),
                        }
                        .query_task_registered_for_validation(
                            task,
                            key.clone(),
                            origin_request,
                        ),
                    )
                })
                    as Arc<dyn Fn(Arc<Task>, u64) -> ValidationDemand<V> + Send + Sync>
            });
            let registry_core = Arc::downgrade(&self.core);
            let node: Arc<Node<K, V>> = Arc::new_cyclic(|registry_self: &Weak<Node<K, V>>| {
                let registry_self: Weak<dyn ErasedNode> = registry_self.clone();
                Node {
                    key: key.clone(),
                    identity: NodeIdentity::registered(
                        self.inner.name.clone(),
                        stable_key,
                        self.core.identity,
                        registry_self.clone(),
                    ),
                    incarnation,
                    registry_core,
                    registry_self,
                    users: AtomicUsize::new(0),
                    wait: Arc::new(WaitCell {
                        cv: Condvar::new(),
                        generation: Mutex::new(0),
                    }),
                    demand,
                    state: Mutex::new(NodeState {
                        next_attempt: 1,
                        next_stamp: 1,
                        attempts: VecDeque::new(),
                        validated_at: None,
                    }),
                }
            });
            let erased: Arc<dyn ErasedNode> = node.clone();
            let mut registry = write(&self.core.nodes);
            let inserted = registry.insert(incarnation, Arc::downgrade(&erased));
            drop(registry);
            assert!(inserted, "node incarnations are unique within a runtime");
            nodes.insert(key.clone(), node.clone());
            self.inner.retained_nodes.fetch_add(1, Ordering::Relaxed);
            node
        };
        node.users.fetch_add(1, Ordering::AcqRel);
        Ok(NodeLease {
            family: Arc::downgrade(&self.inner),
            key,
            node,
        })
    }

    fn query_task<F>(
        &self,
        task: Arc<Task>,
        key: K,
        origin_request: u64,
        compute: F,
    ) -> TaskQueryResult<V>
    where
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        assert!(
            self.inner.evaluator.is_none(),
            "registered families must use the closure-free request API"
        );
        self.query_task_impl(task, key, origin_request, Some(compute), true)
    }

    fn query_task_registered(
        &self,
        task: Arc<Task>,
        key: K,
        origin_request: u64,
    ) -> TaskQueryResult<V> {
        self.query_task_impl::<fn(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>>(
            task,
            key,
            origin_request,
            None,
            true,
        )
    }

    fn query_task_registered_for_validation(
        &self,
        task: Arc<Task>,
        key: K,
        origin_request: u64,
    ) -> TaskQueryResult<V> {
        self.query_task_impl::<fn(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>>(
            task,
            key,
            origin_request,
            None,
            false,
        )
    }

    fn query_task_impl<F>(
        &self,
        task: Arc<Task>,
        key: K,
        origin_request: u64,
        mut compute: Option<F>,
        observe_result: bool,
    ) -> TaskQueryResult<V>
    where
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        if observe_result {
            if let Some(terminal) = task.cached_query(self.inner.token, &key) {
                let exact_node = ExactNodeIdentity {
                    display: terminal.node.clone(),
                    incarnation: terminal.node_incarnation,
                };
                if let Some(cycle) = task.stack_cycle(&exact_node) {
                    self.core.metrics.cycles.fetch_add(1, Ordering::Relaxed);
                    return TaskQueryResult::Aborted {
                        abort: QueryAbort::Cycle(cycle),
                        dependencies: Vec::new(),
                        inputs: Vec::new(),
                        work: Vec::new(),
                    };
                }
                if task.cancellation.is_canceled() {
                    self.core
                        .metrics
                        .cancellations
                        .fetch_add(1, Ordering::Relaxed);
                    return TaskQueryResult::Aborted {
                        abort: QueryAbort::Canceled,
                        dependencies: Vec::new(),
                        inputs: Vec::new(),
                        work: Vec::new(),
                    };
                }
                self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
                task.observe(&terminal);
                return TaskQueryResult::Terminal {
                    terminal,
                    execution: RequestExecution::Reused,
                    work: Vec::new(),
                };
            }
        }
        let lease = match self.node(key) {
            Ok(lease) => lease,
            Err(abort) => {
                return TaskQueryResult::Aborted {
                    abort,
                    dependencies: Vec::new(),
                    inputs: Vec::new(),
                    work: Vec::new(),
                };
            }
        };
        let node = &lease.node;
        let exact_node = ExactNodeIdentity {
            display: node.identity.clone(),
            incarnation: node.incarnation,
        };
        if let Some(cycle) = task.stack_cycle(&exact_node) {
            if observe_result {
                self.core.metrics.cycles.fetch_add(1, Ordering::Relaxed);
            }
            return TaskQueryResult::Aborted {
                abort: QueryAbort::Cycle(cycle),
                dependencies: Vec::new(),
                inputs: Vec::new(),
                work: Vec::new(),
            };
        }
        // Attempts whose owner this task cannot wait for without deadlocking.
        let mut contended = BTreeSet::new();
        loop {
            if task.cancellation.is_canceled() {
                self.core
                    .metrics
                    .cancellations
                    .fetch_add(1, Ordering::Relaxed);
                return TaskQueryResult::Aborted {
                    abort: QueryAbort::Canceled,
                    dependencies: Vec::new(),
                    inputs: Vec::new(),
                    work: Vec::new(),
                };
            }

            enum Action {
                Join { attempt: u64, owner: TaskId },
                Compute { attempt: u64 },
            }

            // Acquire a protective pin on the reuse candidate about to be
            // validated while it is still retained under the node lock, before
            // releasing the lock to validate. A candidate discovered here
            // therefore cannot be evicted in the window between discovery and
            // either lease transfer (on a validated reuse) or release (on a
            // stale candidate): its pin holds it retained through the recursive
            // validation that follows.
            //
            // Discovery is one candidate at a time, newest first, because the
            // walk below returns on the first validated candidate. Pinning the
            // node's whole retained set up front instead costs one pin per
            // attempt the node has ever accumulated and releases all but one
            // immediately, and every surplus release runs a family and runtime
            // retention enforcement pass. That makes a single request O(retained
            // attempts) and a session which republishes the same keys across
            // revisions quadratic in its revision count (RUE-1262).
            //
            // `cursor` is the id of the last candidate examined; attempt ids
            // ascend with publication, so selecting the greatest terminal id
            // below it walks the same newest-first order the snapshot did. A
            // candidate evicted before the cursor reaches it was unprotected by
            // definition, and missing it costs an ordinary recompute rather
            // than a wrong answer.
            let mut cursor = u64::MAX;
            #[cfg(test)]
            let mut discovered = false;
            loop {
                let candidate = {
                    let state = lock(&node.state);
                    state.attempts.iter().rev().find_map(|attempt| {
                        match (attempt.id < cursor).then_some(&attempt.state) {
                            Some(AttemptState::Terminal {
                                terminal, handoffs, ..
                            }) => Some((
                                attempt.id,
                                handoffs.clone(),
                                self.pin_terminal(terminal)
                                    .expect("a family pins its own retained terminal"),
                            )),
                            Some(AttemptState::Computing { .. }) | None => None,
                        }
                    })
                };
                let Some((attempt_id, handoffs, pin)) = candidate else {
                    break;
                };
                cursor = attempt_id;
                #[cfg(test)]
                if !std::mem::replace(&mut discovered, true) {
                    self.core.interpose(InterposeSite::ReuseDiscovered);
                }
                let terminal = pin.terminal().clone();
                let endorsement_authority = if self.inner.evaluator.is_some() {
                    task.validation_endorsement_authority_for_terminal(&terminal)
                } else {
                    ValidationEndorsementAuthority::Inactive
                };
                let endorsement_enabled =
                    endorsement_authority != ValidationEndorsementAuthority::Inactive;
                // Only a task-local endorsement proves this candidate's own
                // inputs and dependency edges current. Published fallbacks
                // supply retention authority for current dependency
                // certificates while the ordinary root validation still
                // checks those direct observations.
                let endorsement_hit =
                    endorsement_authority == ValidationEndorsementAuthority::TaskLocal;
                if endorsement_hit {
                    task.record_validation_endorsement_hit();
                }
                let mut validation = if endorsement_hit {
                    Ok((true, true, false))
                } else {
                    self.core.valid_for_revision(&terminal, &task)
                };
                if matches!(validation, Ok((true, false, true))) {
                    // A registered validation-only compute or join proved the
                    // value current but could not transfer its dependency pin.
                    // The joined/computed terminal is published now, so one
                    // ordinary reuse traversal repairs and leases the complete
                    // registered cone before this root can be endorsed.
                    validation = self.core.valid_for_revision(&terminal, &task);
                }
                match validation {
                    Ok((true, registered_only, _)) => {
                        if observe_result && !task.observe_handoff(handoffs) {
                            task.defer_pin_release(pin);
                            self.detach_terminal_attempt(node, attempt_id);
                            continue;
                        }
                        let endorse = endorsement_enabled && !endorsement_hit && registered_only;
                        if endorse {
                            task.endorse_validation(&terminal);
                        }
                        self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
                        if observe_result {
                            // Transfer the temporary discovery pin into the task's
                            // request-scoped lease set, so protection is continuous
                            // from discovery through the lifetime of the request.
                            task.observe(&terminal);
                        }
                        // Even a valid traversal conservatively tainted by an
                        // unregistered descendant can be a dependency of a
                        // promotable registered root. Keep this exact candidate
                        // available to the final cone walk while the scope is
                        // active, without endorsing it or disabling ordinary
                        // green reuse for mixed cones. Promotion still fails
                        // closed if the unregistered edge itself is absent.
                        if observe_result || endorsement_enabled {
                            self.lease_observed_pin(&task, pin);
                        } else {
                            task.defer_pin_release(pin);
                        }
                        if observe_result {
                            task.cache_query(self.inner.token, &lease.key, &terminal);
                        }
                        return TaskQueryResult::Terminal {
                            terminal,
                            execution: RequestExecution::Reused,
                            work: Vec::new(),
                        };
                    }
                    Ok((false, _, _)) => task.defer_pin_release(pin),
                    Err(abort) => {
                        task.defer_pin_release(pin);
                        return TaskQueryResult::Aborted {
                            abort,
                            dependencies: Vec::new(),
                            inputs: Vec::new(),
                            work: Vec::new(),
                        };
                    }
                }
            }

            let action = {
                let mut state = lock(&node.state);
                let mut action = None;
                for attempt in state.attempts.iter_mut().rev() {
                    match &mut attempt.state {
                        AttemptState::Terminal { .. } => {}
                        AttemptState::Computing { owner, waiters } => {
                            // A body has not yet frozen its observed leaves, so
                            // only the identical pinned revision may join it.
                            // An attempt this task already proved it cannot wait
                            // for is skipped, so the fallback below claims a
                            // private attempt instead of reselecting the same
                            // unwaitable one forever.
                            if attempt.revision == task.revision && !contended.contains(&attempt.id)
                            {
                                *waiters += 1;
                                action = Some(Action::Join {
                                    attempt: attempt.id,
                                    owner: *owner,
                                });
                            }
                        }
                    }
                    if action.is_some() {
                        break;
                    }
                }
                action.unwrap_or_else(|| {
                    let attempt = state.next_attempt;
                    state.next_attempt += 1;
                    state.attempts.push_back(Attempt {
                        id: attempt,
                        revision: task.revision,
                        state: AttemptState::Computing {
                            owner: task.id,
                            waiters: 0,
                        },
                    });
                    Action::Compute { attempt }
                })
            };

            match action {
                Action::Join { attempt, owner } => {
                    self.core.metrics.joins.fetch_add(1, Ordering::Relaxed);
                    #[cfg(test)]
                    self.core.test_changed();
                    match self.join(&task, node, attempt, owner) {
                        Err(abort) => {
                            return TaskQueryResult::Aborted {
                                abort,
                                dependencies: Vec::new(),
                                inputs: Vec::new(),
                                work: Vec::new(),
                            };
                        }
                        Ok(JoinOutcome::Contended) => {
                            contended.insert(attempt);
                            continue;
                        }
                        Ok(JoinOutcome::Joined(joined_attempt, handoffs, pin)) => {
                            // `join` transferred the waiter's protection into this
                            // pin before decrementing the waiter count, so the
                            // joined terminal has been continuously protected. Move
                            // that protection into the request lease (or drop it,
                            // leaving an unobserved validation join speculative).
                            let terminal = pin.terminal().clone();
                            let mut endorse_join = false;
                            if observe_result && !lock(&task.validation_endorsements).is_empty() {
                                // The owner's dependency leases remain in the
                                // owner's task. A waiter that will promote this
                                // joined root must therefore validate it in its
                                // own registered scope, reacquiring the exact
                                // descendant cone before the root becomes
                                // observable here.
                                let mut validation = self.core.valid_for_revision(&terminal, &task);
                                if matches!(validation, Ok((true, false, true))) {
                                    validation = self.core.valid_for_revision(&terminal, &task);
                                }
                                match validation {
                                    Ok((true, registered_only, _)) => {
                                        endorse_join = registered_only;
                                    }
                                    Ok((false, _, _)) => {
                                        task.defer_pin_release(pin);
                                        continue;
                                    }
                                    Err(abort) => {
                                        task.defer_pin_release(pin);
                                        return TaskQueryResult::Aborted {
                                            abort,
                                            dependencies: Vec::new(),
                                            inputs: Vec::new(),
                                            work: Vec::new(),
                                        };
                                    }
                                }
                            }
                            if observe_result && !task.observe_handoff(handoffs) {
                                self.detach_terminal_attempt(node, joined_attempt);
                                continue;
                            }
                            if observe_result {
                                if endorse_join {
                                    task.endorse_validation(&terminal);
                                }
                                task.observe(&terminal);
                                self.lease_observed_pin(&task, pin);
                                task.cache_query(self.inner.token, &lease.key, &terminal);
                            }
                            return TaskQueryResult::Terminal {
                                terminal,
                                execution: RequestExecution::Joined,
                                work: Vec::new(),
                            };
                        }
                        Ok(JoinOutcome::Retry) => continue,
                    }
                }
                Action::Compute { attempt } => {
                    self.core.metrics.claims.fetch_add(1, Ordering::Relaxed);
                    #[cfg(test)]
                    self.core.test_changed();
                    let acquired_here = task.acquire_permit(&self.core);
                    task.push(exact_node.clone());
                    self.core.metrics.body_entered();
                    let context = QueryContext {
                        task: task.clone(),
                        not_send_or_sync: PhantomData,
                    };
                    let body = catch_unwind(AssertUnwindSafe(|| match &self.inner.evaluator {
                        Some(evaluator) => evaluator(&context, self, &lease.key),
                        None => compute.take().expect("query body executes at most once")(&context),
                    }));
                    self.core.metrics.body_left();
                    self.core
                        .metrics
                        .body_completions
                        .fetch_add(1, Ordering::Relaxed);
                    let frame = task.pop(&exact_node);

                    let result = match body {
                        Ok(result) if !task.cancellation.is_canceled() => result,
                        Ok(_) => Err(QueryAbort::Canceled),
                        Err(payload) => {
                            self.abort_attempt(node, attempt);
                            if acquired_here {
                                task.release_permit(&self.core);
                            }
                            frame.abort_handoffs();
                            resume_unwind(payload)
                        }
                    };

                    match result {
                        Ok(output) => {
                            let TaskFrameOutput {
                                dependencies,
                                inputs,
                                mut work,
                                handoffs,
                            } = frame;
                            let handoffs = handoffs.into_lifecycle();
                            let terminal = self.publish(
                                node,
                                attempt,
                                task.revision,
                                origin_request,
                                output,
                                dependencies,
                                inputs,
                                &task,
                                observe_result,
                                handoffs.clone(),
                            );
                            if acquired_here {
                                task.release_permit(&self.core);
                            }
                            if observe_result && !task.observe_handoff(handoffs) {
                                self.detach_terminal_attempt(node, attempt);
                                continue;
                            }
                            work.extend(terminal.work().iter().cloned());
                            let work = canonical_reduced_work(work);
                            if observe_result {
                                task.observe(&terminal);
                                task.cache_query(self.inner.token, &lease.key, &terminal);
                            }
                            task.observe_work(&work);
                            return TaskQueryResult::Terminal {
                                terminal,
                                execution: RequestExecution::Computed,
                                work,
                            };
                        }
                        Err(abort) => {
                            self.abort_attempt(node, attempt);
                            if acquired_here {
                                task.release_permit(&self.core);
                            }
                            if matches!(abort, QueryAbort::Canceled) {
                                self.core
                                    .metrics
                                    .cancellations
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            let TaskFrameOutput {
                                dependencies,
                                inputs,
                                work,
                                handoffs,
                            } = frame;
                            task.observe_abort_prefix(&dependencies, &inputs, &work);
                            handoffs.abort();
                            return TaskQueryResult::Aborted {
                                abort,
                                dependencies,
                                inputs,
                                work,
                            };
                        }
                    }
                }
            }
        }
    }

    fn join(
        &self,
        task: &Arc<Task>,
        node: &Arc<Node<K, V>>,
        attempt_id: u64,
        owner: TaskId,
    ) -> Result<JoinOutcome<K, V>, QueryAbort> {
        let mut state = lock(&node.state);
        if task.cancellation.is_canceled() {
            let enforce = decrement_waiter(&mut state, attempt_id);
            drop(state);
            if enforce {
                self.enforce_retention();
                self.core.enforce_runtime_retention();
            }
            self.core
                .metrics
                .cancellations
                .fetch_add(1, Ordering::Relaxed);
            return Err(QueryAbort::Canceled);
        }
        let Some(attempt) = state.attempts.iter_mut().find(|item| item.id == attempt_id) else {
            return Ok(JoinOutcome::Retry);
        };
        match &mut attempt.state {
            AttemptState::Terminal {
                terminal,
                waiters,
                handoffs,
            } => {
                // Transfer this waiter's protection into a pin *before* dropping
                // the waiter count. Even when this is the last waiter — the count
                // falls to zero here — the pin is already established, so the
                // handoff leaves no instant in which the terminal is unprotected
                // and a concurrent enforcer racing it can never detach it.
                let pin = self
                    .pin_terminal(terminal)
                    .expect("a family pins its own retained terminal");
                let handoffs = handoffs.clone();
                *waiters -= 1;
                drop(state);
                #[cfg(test)]
                self.core.interpose(InterposeSite::JoinHandoff);
                return Ok(JoinOutcome::Joined(attempt_id, handoffs, pin));
            }
            AttemptState::Computing {
                owner: actual_owner,
                ..
            } => assert_eq!(*actual_owner, owner),
        }
        if self
            .core
            .begin_wait(
                task.id,
                owner,
                WaitEdgeLabel::Materialized(node.identity.clone()),
            )
            .is_err()
        {
            // Waiting here would close a wait-graph loop. That loop says two
            // tasks reached an overlapping set of nodes in opposite orders — a
            // scheduling conflict, not a statement about the query graph. A real
            // dependency cycle is a property of the request's structure and is
            // reported exactly by `Task::stack_cycle`, which sees through batch
            // boundaries via the task's ancestry.
            //
            // Failing the request here would make an ordinary interleaving
            // surface as a user-visible cycle, and which task loses the race is
            // nondeterministic. Decline the join instead and let the caller
            // claim a private attempt: query evaluation is deterministic, so a
            // duplicated computation costs work and never an answer, and
            // `publish` folds an equal result back onto the existing stamp.
            decrement_waiter(&mut state, attempt_id);
            self.core
                .metrics
                .declined_joins
                .fetch_add(1, Ordering::Relaxed);
            return Ok(JoinOutcome::Contended);
        }
        let cancellation_watch = task.cancellation.watch(&node.wait);
        let donated = task.release_permit(&self.core);
        if donated {
            self.core
                .metrics
                .donated_permits
                .fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        let mut enforce_after_cancellation = false;
        let result = loop {
            let mut state = lock(&node.state);
            if task.cancellation.is_canceled() {
                enforce_after_cancellation = decrement_waiter(&mut state, attempt_id);
                drop(state);
                break Err(QueryAbort::Canceled);
            }
            let Some(attempt) = state.attempts.iter_mut().find(|item| item.id == attempt_id) else {
                break Ok(JoinOutcome::Retry);
            };
            match &mut attempt.state {
                AttemptState::Computing { .. } => {
                    drop(state);
                    node.wait.wait_until(
                        || {
                            if task.cancellation.is_canceled() {
                                return true;
                            }
                            let state = lock(&node.state);
                            !state.attempts.iter().any(|attempt| {
                                attempt.id == attempt_id
                                    && matches!(attempt.state, AttemptState::Computing { .. })
                            })
                        },
                        #[cfg(test)]
                        || self.core.interpose(InterposeSite::NodeJoinPark),
                    );
                }
                AttemptState::Terminal {
                    terminal,
                    waiters,
                    handoffs,
                } => {
                    // Transfer waiter protection into a pin before decrementing,
                    // as above: no unprotected instant even for the last waiter.
                    let pin = self
                        .pin_terminal(terminal)
                        .expect("a family pins its own retained terminal");
                    *waiters -= 1;
                    break Ok(JoinOutcome::Joined(attempt_id, handoffs.clone(), pin));
                }
            }
        };
        task.cancellation.unwatch(cancellation_watch);
        self.core.end_wait(task.id, owner);
        if donated {
            task.acquire_permit(&self.core);
        }
        if enforce_after_cancellation {
            self.enforce_retention();
            self.core.enforce_runtime_retention();
        }
        if matches!(result, Err(QueryAbort::Canceled)) {
            self.core
                .metrics
                .cancellations
                .fetch_add(1, Ordering::Relaxed);
        }
        #[cfg(test)]
        if matches!(result, Ok(JoinOutcome::Joined(..))) {
            self.core.interpose(InterposeSite::JoinHandoff);
        }
        result
    }

    fn abort_attempt(&self, node: &Arc<Node<K, V>>, attempt_id: u64) {
        let mut state = lock(&node.state);
        if let Some(index) = state.attempts.iter().position(|item| item.id == attempt_id) {
            state.attempts.remove(index);
        }
        drop(state);
        node.wait.notify_all();
    }

    fn detach_terminal_attempt(&self, node: &Arc<Node<K, V>>, attempt_id: u64) {
        let removed = {
            let mut state = lock(&node.state);
            let Some(index) = state.attempts.iter().position(|item| item.id == attempt_id) else {
                return;
            };
            if !matches!(state.attempts[index].state, AttemptState::Terminal { .. }) {
                return;
            }
            let removed = state
                .attempts
                .remove(index)
                .expect("terminal attempt exists");
            match removed.state {
                AttemptState::Terminal { terminal, .. } => Some(terminal),
                AttemptState::Computing { .. } => unreachable!(),
            }
        };
        if let Some(terminal) = removed {
            self.core
                .metrics
                .retained_terminals
                .fetch_sub(1, Ordering::Relaxed);
            self.inner.retained_count.fetch_sub(1, Ordering::Relaxed);
            lock(&self.inner.retention)
                .remove_charge(terminal.retained_charge, terminal.dependency_pin_charge);
            node.wait.notify_all();
        }
    }

    fn publish(
        &self,
        node: &Arc<Node<K, V>>,
        attempt_id: u64,
        revision: Revision,
        origin_request: u64,
        output: QueryOutput<V>,
        dependencies: Vec<Observation>,
        inputs: Vec<InputObservation>,
        task: &Arc<Task>,
        lease: bool,
        handoffs: Arc<AttemptHandoffLifecycle>,
    ) -> Arc<QueryTerminal<V>> {
        let QueryOutput {
            outcome,
            kind,
            diagnostics,
            work,
            retained_value_charge,
        } = output;
        let diagnostics = canonical_diagnostics(diagnostics);
        let work = canonical_work(work);
        let retained_value_charge = match &outcome {
            QueryOutcome::Success(value) => Some(
                retained_value_charge.unwrap_or_else(|| (self.inner.retained_value_charge)(value)),
            ),
            QueryOutcome::Failure(_) => None,
        };
        let (retained_charge, dependency_pin_charge) = retained_terminal_charge(
            &outcome,
            retained_value_charge,
            &node.identity,
            &diagnostics,
            &work,
            &dependencies,
            &inputs,
        );
        let mut state = lock(&node.state);
        let previous = state
            .attempts
            .iter()
            .rev()
            .find_map(|attempt| match &attempt.state {
                AttemptState::Terminal { terminal, .. } => Some(terminal.clone()),
                AttemptState::Computing { .. } => None,
            });
        let red = previous.as_ref().is_some_and(|terminal| {
            terminal.kind == kind
                && outcomes_equal(self.inner.value_equal, &terminal.outcome, &outcome)
                && semantic_diagnostics_equal(&terminal.diagnostics, &diagnostics)
        });
        let stamp = if red {
            previous.expect("red publication has a predecessor").stamp
        } else {
            let stamp = state.next_stamp;
            state.next_stamp += 1;
            stamp
        };
        let terminal = Arc::new(QueryTerminal {
            family_token: self.inner.token,
            node: node.identity.clone(),
            node_incarnation: node.incarnation,
            revision,
            stamp,
            origin_request,
            outcome,
            kind,
            diagnostics: diagnostics.into(),
            work: work.into(),
            dependencies: dependencies.into(),
            inputs: inputs.into(),
            retained_charge,
            dependency_pin_charge,
            pins: AtomicUsize::new(0),
        });
        let attempt = state
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == attempt_id)
            .expect("only the claiming task may publish its attempt");
        let waiters = match &attempt.state {
            AttemptState::Computing { waiters, .. } => *waiters,
            AttemptState::Terminal { .. } => panic!("only a computing attempt may publish"),
        };
        attempt.state = AttemptState::Terminal {
            terminal: terminal.clone(),
            waiters,
            handoffs,
        };
        // Publication proves the value for this revision, but not that its
        // dependency cone was reached exclusively through registered family
        // evaluators. A later authoritative validation may strengthen this
        // conservative bit for the separate retained-cone endorsement API.
        state.validated_at = Some(ValidationCertificate {
            revision,
            stamp,
            terminal_revision: revision,
            registered_only: false,
        });
        // Acquire the request lease *under the node lock*, before the terminal is
        // enqueued for retention or made reachable to a concurrent enforcer. The
        // pin's `pins > 0` is thus established atomically with publication: there
        // is no instant in which the just-published terminal is both evictable and
        // unleased. Speculative validation publications (`lease == false`) are
        // intentionally left evictable and take no pin here.
        let lease_pin = if lease {
            Some(
                self.pin_terminal(&terminal)
                    .expect("a family pins its own retained terminal"),
            )
        } else {
            None
        };
        drop(state);

        if red {
            self.core
                .metrics
                .red_publications
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.core
                .metrics
                .green_publications
                .fetch_add(1, Ordering::Relaxed);
        }
        self.core
            .metrics
            .retained_terminals
            .fetch_add(1, Ordering::Relaxed);
        self.inner.retained_count.fetch_add(1, Ordering::Relaxed);
        let aggregate_probe = lock(&self.inner.retention).publish(
            RetentionEntry {
                node: Arc::downgrade(node),
                attempt: attempt_id,
            },
            retained_charge,
            dependency_pin_charge,
        );
        // The terminal is now exposed and enqueued — reachable to any concurrent
        // enforcer. With `lease_pin` already held (acquired under the node lock
        // above) it is protected before it becomes evictable, so an enforcer that
        // runs at this exact instant cannot detach it.
        #[cfg(test)]
        self.core.interpose(InterposeSite::PublishExposed);
        // Transfer the pin into the task's request-scoped lease set (deduplicated),
        // so a tiny bound cannot evict the terminal at birth while the rooted
        // request that produced it is still running.
        if let Some(pin) = lease_pin {
            self.lease_observed_pin(task, pin);
        }
        node.wait.notify_all();
        if lease {
            self.enforce_retention_after_publish();
        } else {
            // A validation-only publication has no birth lease and can be
            // evicted immediately. Sweep in bounded batches rather than once
            // per publication: when a large protected prefix occupies the
            // queue, evicting one new unpinned terminal per full scan is
            // quadratic in the validation cone. The rooted task schedules one
            // final strict pass, so the family is back at its configured bound
            // when the request completes.
            task.defer_family_enforcement(self.retention_enforcer());
            self.enforce_retention_after_publish_with_margin(VALIDATION_PUBLISH_SWEEP_QUANTUM);
        }
        if aggregate_probe {
            self.core.enforce_runtime_retention_after_probe();
        }
        terminal
    }

    fn enforce_retention_after_publish(&self) {
        self.enforce_retention_after_publish_with_margin(1);
    }

    fn enforce_retention_after_publish_with_margin(&self, sweep_margin: usize) {
        let retained = self.inner.retained_count.load(Ordering::Acquire);
        loop {
            let threshold = self.inner.next_publish_sweep.load(Ordering::Acquire);
            if retained < threshold {
                return;
            }
            // Claim this watermark before taking the retention lock. Concurrent
            // publishers skip while this pass is pending; the pass installs the
            // next watermark from the count it observes at completion.
            if self
                .inner
                .next_publish_sweep
                .compare_exchange(threshold, usize::MAX, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.enforce_retention_with_margin(sweep_margin);
                return;
            }
        }
    }

    fn enforce_retention(&self) {
        self.enforce_retention_with_margin(1);
    }

    fn enforce_retention_with_margin(&self, sweep_margin: usize) {
        self.core
            .metrics
            .retention_enforcements
            .fetch_add(1, Ordering::Relaxed);
        while self.inner.retained_count.load(Ordering::Relaxed) > self.inner.retention_limit
            && evict_one_from_family(&self.core, &self.inner)
        {}
        // The pass could not reach the configured bound: every remaining
        // candidate was a protected root the live closure still needs. Grow and
        // record the pressure event rather than evict a required terminal.
        let retained = self.inner.retained_count.load(Ordering::Relaxed);
        if retained > self.inner.retention_limit {
            self.core
                .metrics
                .retention_growth
                .fetch_add(1, Ordering::Relaxed);
        }
        let next_publish_sweep = if retained > self.inner.retention_limit {
            retained.saturating_mul(2).max(retained.saturating_add(1))
        } else {
            self.inner.retention_limit.saturating_add(sweep_margin)
        };
        self.inner
            .next_publish_sweep
            .store(next_publish_sweep, Ordering::Release);
    }

    fn retention_enforcer(&self) -> FamilyEnforcer {
        FamilyEnforcer {
            family_id: Arc::as_ptr(&self.inner) as *const () as usize,
            core: self.core.clone(),
            enforce: Box::new({
                let family = self.clone();
                move || family.enforce_retention()
            }),
        }
    }

    /// Pins a retained terminal against eviction.
    pub fn pin_terminal(
        &self,
        terminal: &Arc<QueryTerminal<V>>,
    ) -> Result<TerminalPin<K, V>, PinError> {
        if terminal.family_token != self.inner.token {
            return Err(PinError::ForeignFamily);
        }
        terminal.pins.fetch_add(1, Ordering::AcqRel);
        Ok(TerminalPin {
            family: self.clone(),
            terminal: terminal.clone(),
            deferred: AtomicBool::new(false),
        })
    }

    /// Mint the exact-terminal adoption capability for a terminal of THIS
    /// family. Only a family REGISTERED CONTENT-ADDRESSED
    /// ([`QueryRuntime::content_addressed_family_with_equality`]) can mint
    /// one: the registration is the mechanical assertion that the key alone
    /// pins each terminal's value, which is what makes an input-free
    /// endorsement at another revision sound. Any other family is refused.
    pub fn adoptable_terminal(
        &self,
        terminal: &Arc<QueryTerminal<V>>,
    ) -> Result<AdoptableTerminal<V>, AdoptTerminalError> {
        if terminal.family_token != self.inner.token {
            return Err(AdoptTerminalError::ForeignFamily);
        }
        if !self.inner.content_addressed {
            return Err(AdoptTerminalError::NotContentAddressed);
        }
        Ok(AdoptableTerminal {
            terminal: terminal.clone(),
        })
    }

    /// Record an ALREADY-HELD adoptable terminal of this family as a
    /// dependency of the computing task: an exact-terminal capability that
    /// never hashes or compares the terminal's content key (the node is
    /// located by incarnation). The terminal must still be retained on its
    /// node — a stale or evicted terminal is rejected, never silently
    /// re-derived. On success the task observes the exact
    /// `{node, incarnation, stamp}` identity of the held terminal and leases
    /// it for the request's lifetime, and the node is endorsed at the task's
    /// pinned revision — an input-free republication with the SAME stamp,
    /// routed through the family's ordinary retained-publication accounting
    /// (metrics, retention queue, eviction) — so red/green validation of the
    /// recorded observation succeeds at this revision and its compatible
    /// descendants without touching the leaves the original computation
    /// observed. Soundness comes from the capability: only a
    /// content-addressed registration mints [`AdoptableTerminal`].
    pub fn observe_adopted_terminal(
        &self,
        context: &QueryContext,
        adoptable: &AdoptableTerminal<V>,
    ) -> Result<(), AdoptTerminalError> {
        let terminal = &adoptable.terminal;
        if terminal.family_token != self.inner.token {
            return Err(AdoptTerminalError::ForeignFamily);
        }
        if !self.inner.content_addressed {
            return Err(AdoptTerminalError::NotContentAddressed);
        }
        if !Arc::ptr_eq(&self.core, &context.task.core) {
            return Err(AdoptTerminalError::ForeignRuntime);
        }
        // Pin FIRST (mirroring reuse-candidate discovery) so the terminal
        // cannot be evicted between validation and the lease transfer below.
        let pin = self
            .pin_terminal(terminal)
            .map_err(|_| AdoptTerminalError::ForeignFamily)?;
        // Locate the node by INCARNATION — never by key hash or equality —
        // and recover this family's typed node from the erased handle.
        let node = self
            .core
            .registered_node(terminal.node_incarnation)
            .ok_or(AdoptTerminalError::Evicted)?;
        let node = node
            .as_any()
            .downcast::<Node<K, V>>()
            .map_err(|_| AdoptTerminalError::Evicted)?;
        // An incarnation-index anomaly (a recycled slot or foreign node) must
        // never endorse an unrelated node.
        if node.incarnation != terminal.node_incarnation || node.identity != terminal.node {
            return Err(AdoptTerminalError::Evicted);
        }
        let endorsed_pin = self.endorse_adopted_stamp(&node, context.task.revision, terminal)?;
        // Record the exact observation and transfer BOTH pins into the task's
        // request-scoped lease set: the held predecessor and the endorsement
        // itself, whose lease identity differs by its adopting revision. The
        // endorsement pin was acquired under the node lock, so there is no
        // instant in which it is both exposed and evictable.
        context.task.observe(terminal);
        self.lease_adopted_pin(&context.task, pin);
        self.lease_adopted_pin(&context.task, endorsed_pin);
        Ok(())
    }

    /// Endorse a still-retained stamp of `node` at `revision` by an input-free
    /// republication of the held value with the SAME stamp, accounted exactly
    /// like an ordinary retained publication: metered, enqueued for retention,
    /// and evictable — an adoption chain is bounded by the family's retention
    /// limit, and an evicted endorsement simply dirties its dependents through
    /// ordinary red/green validation.
    fn endorse_adopted_stamp(
        &self,
        node: &Arc<Node<K, V>>,
        revision: Revision,
        held: &Arc<QueryTerminal<V>>,
    ) -> Result<TerminalPin<K, V>, AdoptTerminalError> {
        let (attempt_id, endorsed_pin) = {
            let mut state = lock(&node.state);
            // The endorsed stamp must still be retained on this node; a stale
            // or evicted terminal is rejected, never silently re-derived.
            let retained = state.attempts.iter().any(|attempt| {
                matches!(
                    &attempt.state,
                    AttemptState::Terminal { terminal, .. } if terminal.stamp == held.stamp
                )
            });
            if !retained {
                return Err(AdoptTerminalError::Evicted);
            }
            // Idempotent per (revision, stamp); the scan is bounded by the
            // retention limit because endorsements are evictable entries. The
            // existing endorsement is re-pinned under the lock so THIS
            // adopting attempt protects it too.
            let endorsed = state.attempts.iter().find_map(|attempt| {
                if attempt.revision != revision {
                    return None;
                }
                match &attempt.state {
                    AttemptState::Terminal { terminal, .. }
                        if terminal.stamp == held.stamp && terminal.inputs.is_empty() =>
                    {
                        Some(terminal.clone())
                    }
                    _ => None,
                }
            });
            if let Some(endorsed) = endorsed {
                return Ok(self
                    .pin_terminal(&endorsed)
                    .expect("a family pins its own retained terminal"));
            }
            let adopted = Arc::new(QueryTerminal {
                family_token: held.family_token,
                node: held.node.clone(),
                node_incarnation: held.node_incarnation,
                revision,
                stamp: held.stamp,
                origin_request: held.origin_request,
                outcome: held.outcome.clone(),
                kind: held.kind,
                diagnostics: held.diagnostics.clone(),
                work: held.work.clone(),
                dependencies: Arc::from([]),
                inputs: Arc::from([]),
                retained_charge: held.retained_charge,
                dependency_pin_charge: 0,
                pins: AtomicUsize::new(0),
            });
            let id = state.next_attempt;
            state.next_attempt += 1;
            state.attempts.push_back(Attempt {
                id,
                revision,
                state: AttemptState::Terminal {
                    terminal: adopted.clone(),
                    waiters: 0,
                    handoffs: AttemptHandoffLifecycle::shared_committed(),
                },
            });
            state.validated_at = Some(ValidationCertificate {
                revision,
                stamp: held.stamp,
                terminal_revision: revision,
                registered_only: true,
            });
            // Pin UNDER the node lock, before the endorsement is enqueued or
            // reachable to a concurrent enforcer: `pins > 0` is established
            // atomically with insertion, so retention pressure (including the
            // enforcement pass below) can never evict it at birth.
            let pin = self
                .pin_terminal(&adopted)
                .expect("a family pins its own retained terminal");
            (id, pin)
        };
        self.core
            .metrics
            .green_publications
            .fetch_add(1, Ordering::Relaxed);
        self.core
            .metrics
            .retained_terminals
            .fetch_add(1, Ordering::Relaxed);
        self.inner.retained_count.fetch_add(1, Ordering::Relaxed);
        let aggregate_probe = lock(&self.inner.retention).publish(
            RetentionEntry {
                node: Arc::downgrade(node),
                attempt: attempt_id,
            },
            held.retained_charge,
            0,
        );
        self.enforce_retention_after_publish();
        if aggregate_probe {
            self.core.enforce_runtime_retention_after_probe();
        }
        Ok(endorsed_pin)
    }

    /// Transfers an already-acquired [`TerminalPin`] into a task's request-scoped
    /// lease set, retaining it for the lifetime of the rooted request.
    ///
    /// The pin is acquired by the caller *while the terminal is still protected*
    /// (under the node lock at publication, before decrementing waiter protection
    /// at a join, or on a candidate still retained under the node lock at reuse),
    /// then handed here. This makes acquisition atomic with retention: the
    /// terminal is continuously protected from the instant it is observed until
    /// the request's task drops, so a terminal the live computation still needs
    /// can never be evicted out from under it. Because nested queries execute in
    /// the same `task`, a nested observation inherits the root request's lease
    /// automatically. The lease releases — leaving no permanent retention — when
    /// the task drops.
    ///
    /// If the task has already leased this exact terminal (same node
    /// incarnation, stamp, and terminal revision), the redundant `pin` is
    /// dropped here rather than double-held. Ordinary validation-only terminals
    /// remain speculative and are dropped by the caller. A live registered
    /// endorsement scope also leases a valid validation-only candidate so final
    /// cone promotion can select the exact proof path; unrelated pins remain
    /// task-scoped and are excluded from the promoted graph walk.
    fn lease_observed_pin(&self, task: &Arc<Task>, pin: TerminalPin<K, V>) {
        task.validation_work
            .terminal_lease_observations
            .fetch_add(1, Ordering::Relaxed);
        if !self.insert_task_lease(task, pin) {
            task.validation_work
                .duplicate_terminal_lease_observations
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Transfers exact terminals already held by the adoption capability.
    /// Adoption never resolves a family key or visits its memo index, so it is
    /// deliberately outside the repeated-query opportunity measured by
    /// [`ValidationWork::terminal_lease_observations`].
    fn lease_adopted_pin(&self, task: &Arc<Task>, pin: TerminalPin<K, V>) {
        self.insert_task_lease(task, pin);
    }

    /// Inserts one exact terminal into the existing task lease set and reports
    /// whether this task had not already leased it.
    fn insert_task_lease(&self, task: &Arc<Task>, pin: TerminalPin<K, V>) -> bool {
        let mut leases = lock(&task.leases);
        let identity = (
            pin.terminal.node_incarnation,
            pin.terminal.stamp,
            pin.terminal.revision,
        );
        let inserted = leases.observed.insert(identity);
        if inserted {
            leases.held.push(Box::new(pin));
            task.core.metrics.task_lease_acquired();
        }
        inserted
    }

    /// Pins terminals computed under this exact retained revision.
    pub fn retain_revision(&self, revision: Revision) -> RevisionPin<K, V> {
        *lock(&self.inner.retained_revisions)
            .entry(revision)
            .or_default() += 1;
        RevisionPin {
            family: self.clone(),
            revision,
            view: self.core.pin_revision(revision),
        }
    }

    /// Creates request/session-owned current and last-good publication roots.
    pub fn selection(&self) -> QuerySelection<K, V> {
        QuerySelection {
            family: self.clone(),
            current: None,
            last_good: None,
        }
    }
}

/// An explicit retained terminal root.
pub struct TerminalPin<K: QueryKey, V: Clone + Send + Sync + 'static> {
    family: QueryFamily<K, V>,
    terminal: Arc<QueryTerminal<V>>,
    /// When set, `Drop` performs neither the pin decrement nor the per-pin
    /// `enforce_retention`: the batched teardown path (`release_deferred`) has
    /// already decremented this pin and folded the owning family's single
    /// enforcement pass into a deduplicated [`FamilyEnforcer`]. False for every
    /// ordinary per-pin user (session pins, attempt/result leases, test pins),
    /// whose `Drop` semantics are unchanged.
    deferred: AtomicBool,
}

impl<K, V> TerminalPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// The immutable terminal protected by this root.
    pub fn terminal(&self) -> &Arc<QueryTerminal<V>> {
        &self.terminal
    }
}

/// A terminal cannot be pinned by a different family or runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinError {
    /// The terminal's unforgeable family token does not match.
    ForeignFamily,
}

/// A final-terminal cone could not be proven complete from the current task's
/// live registered-query observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainTerminalConeError {
    /// The caller did not establish a lexical registered-validation authority.
    NoRegisteredValidationScope,
    /// The proposed root was not observed by this task.
    RootNotObserved,
    /// A fallback lease universe belongs to another query runtime.
    ForeignRuntime,
    /// One immutable edge in the proposed root's transitive cone had no
    /// matching live task lease.
    DependencyNotObserved(Observation),
}

/// Errors from minting or recording an exact-terminal adoption capability
/// ([`QueryFamily::adoptable_terminal`] /
/// [`QueryFamily::observe_adopted_terminal`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptTerminalError {
    /// The terminal's unforgeable family token does not match this family.
    ForeignFamily,
    /// The computing task belongs to a different runtime than this family.
    ForeignRuntime,
    /// The family is not registered content-addressed, so it has no authority
    /// to mint adoption capabilities: an input-dependent family endorsing a
    /// held value input-free at another revision could validate a stale
    /// result green.
    NotContentAddressed,
    /// The terminal (or its node) is no longer retained: a stale or evicted
    /// terminal is rejected, never silently re-derived.
    Evicted,
}

/// The exact-terminal adoption capability: a terminal of a family whose
/// CONTENT-ADDRESSED registration is the sole minting authority
/// ([`QueryFamily::adoptable_terminal`]). Holding one proves the family
/// asserted its key alone pins the terminal's value, which is what makes an
/// input-free endorsement at another revision sound.
#[derive(Debug, Clone)]
pub struct AdoptableTerminal<V> {
    terminal: Arc<QueryTerminal<V>>,
}

impl<V> AdoptableTerminal<V> {
    /// The held terminal.
    pub fn terminal(&self) -> &Arc<QueryTerminal<V>> {
        &self.terminal
    }
}

impl<K, V> Drop for TerminalPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        // Batched teardown (`release_deferred`) already decremented this pin and
        // deferred the family's single enforcement pass; do nothing further here,
        // else the pin would be double-decremented and the linearity lost.
        if self.deferred.load(Ordering::Relaxed) {
            return;
        }
        let previous = self.terminal.pins.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "a terminal pin releases exactly once");
        if previous == 1 {
            // Only the last pin can make this terminal newly evictable. A
            // duplicate/root-overlap release cannot enable retention progress,
            // so scanning the full family here would be pure quadratic work.
            self.family.enforce_retention();
            self.family.core.enforce_runtime_retention();
        }
    }
}

/// An explicit current/last-good revision root.
pub struct RevisionPin<K: QueryKey, V: Clone + Send + Sync + 'static> {
    family: QueryFamily<K, V>,
    revision: Revision,
    view: Option<RevisionLease>,
}

impl<K, V> Drop for RevisionPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        let mut revisions = lock(&self.family.inner.retained_revisions);
        let count = revisions
            .get_mut(&self.revision)
            .expect("revision pin owns a retained root");
        *count -= 1;
        let released = *count == 0;
        if released {
            revisions.remove(&self.revision);
        }
        drop(revisions);
        if released {
            self.family.enforce_retention();
            self.family.core.enforce_runtime_retention();
        }
        // The revision-view lease drops after terminal retention bookkeeping.
        let _ = &self.view;
    }
}

/// Request/session publication over immutable terminal attempts.
///
/// This deliberately lives above memo nodes. Selecting a failed current
/// attempt preserves the preceding successful terminal as last-good.
pub struct QuerySelection<K: QueryKey, V: Clone + Send + Sync + 'static> {
    family: QueryFamily<K, V>,
    current: Option<TerminalPin<K, V>>,
    last_good: Option<TerminalPin<K, V>>,
}

impl<K, V> QuerySelection<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// Publishes one immutable attempt as the request's current result.
    pub fn publish(&mut self, terminal: &Arc<QueryTerminal<V>>) -> Result<(), PinError> {
        let current = self.family.pin_terminal(terminal)?;
        if terminal.kind() == QueryTerminalKind::Success {
            self.last_good = Some(self.family.pin_terminal(terminal)?);
        }
        self.current = Some(current);
        Ok(())
    }

    /// Current selected attempt, including a deterministic failure.
    pub fn current(&self) -> Option<&Arc<QueryTerminal<V>>> {
        self.current.as_ref().map(TerminalPin::terminal)
    }

    /// Most recently selected successful attempt.
    pub fn last_good(&self) -> Option<&Arc<QueryTerminal<V>>> {
        self.last_good.as_ref().map(TerminalPin::terminal)
    }

    /// Clears request-current publication after a non-terminal abort while
    /// preserving the independently pinned last-good success.
    pub fn clear_current(&mut self) {
        self.current = None;
    }
}

/// Task-scoped access to nested dependency queries.
#[derive(Debug)]
pub struct QueryContext {
    task: Arc<Task>,
    not_send_or_sync: PhantomData<Rc<()>>,
}

struct RegisteredBatchItem<K> {
    request_id: u64,
    key: K,
}

struct RegisteredBatchItems<K> {
    family: Arc<str>,
    items: Vec<RegisteredBatchItem<K>>,
}

impl<K> fmt::Debug for RegisteredBatchItems<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredBatchItems")
            .field("family", &self.family)
            .field("items", &self.items.len())
            .finish()
    }
}

/// Type-erased view over the one typed key table already owned by a registered
/// batch. Wait edges keep this table alive through cancellation and completed
/// children without allocating or formatting one label per edge.
trait StructuredWaitLabels: fmt::Debug + Send + Sync {
    fn node_identity(&self, index: usize) -> NodeIdentity;
}

impl<K: QueryKey> StructuredWaitLabels for RegisteredBatchItems<K> {
    fn node_identity(&self, index: usize) -> NodeIdentity {
        let item = self
            .items
            .get(index)
            .expect("a structured wait edge names one live batch item");
        NodeIdentity::new(
            self.family.clone(),
            item.key.stable_identity().into_boxed_str(),
        )
    }
}

#[derive(Debug, Clone)]
enum WaitEdgeLabel {
    Materialized(NodeIdentity),
    Structured {
        labels: Arc<dyn StructuredWaitLabels>,
        index: usize,
    },
}

impl WaitEdgeLabel {
    fn node_identity(&self, metrics: &Metrics) -> NodeIdentity {
        match self {
            Self::Materialized(node) => node.clone(),
            Self::Structured { labels, index } => {
                let node = labels.node_identity(*index);
                metrics.record_structured_wait_identity(node.key().len());
                node
            }
        }
    }
}

type BatchCompletion<V> = (usize, Arc<Task>, TaskQueryResult<V>);

fn run_registered_batch_worker<K, V>(
    queue: Arc<Mutex<VecDeque<usize>>>,
    items: Arc<RegisteredBatchItems<K>>,
    family: QueryFamily<K, V>,
    parent: Arc<Task>,
    authority: Arc<BatchValidationAuthority>,
    tracing_dispatch: tracing::Dispatch,
    tracing_parent: tracing::Span,
) -> std::thread::Result<Vec<BatchCompletion<V>>>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    tracing::dispatcher::with_default(&tracing_dispatch, || {
        let parent_span = tracing_parent.enter();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut completed = Vec::new();
            loop {
                let Some(index) = lock(&queue).pop_front() else {
                    break;
                };
                let item = &items.items[index];
                let child = parent.batch_child(item.request_id, authority.clone());
                let result =
                    family.query_task_registered(child.clone(), item.key.clone(), item.request_id);
                if matches!(result, TaskQueryResult::Terminal { .. }) {
                    authority.publish_child(&child);
                }
                completed.push((index, child, result));
            }
            completed
        }));
        drop(parent_span);
        // A tracing subscriber may buffer observations locally to avoid
        // perturbing parallel query execution. This generic lifecycle marker
        // lets it publish once at the bounded worker-completion boundary; the
        // runtime knows nothing about compiler phases or measurement schemas.
        tracing::trace!(timing_flush = true, "registered query worker complete");
        result
    })
}

impl QueryContext {
    /// Exact immutable revision pinned by this task.
    pub fn revision(&self) -> Revision {
        self.task.revision
    }

    /// Maximum number of query evaluators this runtime may execute concurrently.
    ///
    /// A scheduler can use this immutable construction-time limit to avoid
    /// creating structured child tasks when the runtime has only one permit and
    /// same-task proof sharing is therefore strictly cheaper.
    pub fn max_concurrency(&self) -> usize {
        self.task.core.permits.maximum
    }

    /// Fails cooperatively when the request is canceled.
    pub fn check_canceled(&self) -> Result<(), QueryAbort> {
        if self.task.cancellation.is_canceled() {
            Err(QueryAbort::Canceled)
        } else {
            Ok(())
        }
    }

    /// Reads and records one exact leaf from this task's pinned revision.
    pub fn input(&self, input: InputIdentity) -> Result<u64, QueryAbort> {
        let stamp = self
            .task
            .core
            .revision_input(self.task.revision, &input)
            .ok_or_else(|| QueryAbort::MissingInput(input.clone()))?;
        self.task.observe_input(input, stamp);
        Ok(stamp)
    }

    /// Reads and records an optional exact leaf from this task's revision.
    ///
    /// Absence is recorded as a negative observation. A successor which adds
    /// the leaf therefore invalidates the terminal without turning absence
    /// into a query failure. External-input coordinators use this to publish a
    /// typed demand terminal; speculative work can instead park on the same
    /// negative observation without emitting a host request.
    pub fn optional_input(&self, input: InputIdentity) -> Option<u64> {
        let stamp = self.task.core.revision_input(self.task.revision, &input);
        self.task.observe_input(input, stamp.unwrap_or(0));
        stamp
    }

    /// Records structural work as it is completed by the active body.
    ///
    /// Unlike terminal-attached output work, this prefix survives cancellation
    /// and deterministic aborts and is owned by the runtime attempt.
    pub fn record_work(&self, item: WorkItem) {
        self.task.record_work(item);
    }

    /// Registers one non-semantic resource for atomic attempt completion.
    ///
    /// The handoff belongs to the current evaluator frame and, after successful
    /// publication, to that internal terminal attempt. The whole top-level root
    /// commits all handoffs it observed, including through nested query, reuse,
    /// and join. A root abort leaves published handoffs pending; speculative
    /// validation never claims them; terminal eviction aborts them.
    pub fn register_attempt_handoff(&self, handoff: impl QueryAttemptHandoff) {
        self.task.register_attempt_handoff(Box::new(handoff));
    }

    /// Restricts operational nested-attempt ledger materialization for this
    /// scope and every nested query it evaluates.
    ///
    /// Query execution and semantic bookkeeping are unchanged: dependency
    /// observations, validation, request leases, cancellation, work,
    /// handoffs, memoization, and terminal publication all still run exactly
    /// as they do without this scope. Only [`QueryRequestAttempt::nested_attempts`]
    /// is filtered. Nested scopes intersect with their parent selection, so an
    /// inner evaluator cannot restore rows its caller chose not to retain.
    ///
    /// The returned guard restores the preceding selection when dropped,
    /// including during unwinding.
    pub fn retain_nested_attempts_for(&self, families: &[&str]) -> NestedAttemptFilterGuard {
        self.task.push_nested_attempt_filter(families)
    }

    /// Enables task-local reuse of full registered-only validation proofs.
    ///
    /// A proof is endorsed only after recursively validating the exact
    /// terminal and every dependency without crossing an unregistered node.
    /// Every terminal in that registered cone remains pinned by the task.
    /// Cache hits still follow the ordinary request path for cancellation,
    /// direct dependency observation, handoffs, work, and request-ledger
    /// classification; only repeated recursive validation is skipped.
    pub fn endorse_registered_validations(&self) -> ValidationEndorsementGuard {
        self.task
            .push_validation_endorsement_scope(&[])
            .expect("an empty validation authority cannot name a foreign runtime")
    }

    /// Enables registered-proof reuse backed by live published pin sets.
    ///
    /// A current revision certificate may skip recursive validation when this
    /// task has already endorsed its exact terminal identity or one of
    /// `fallbacks` retains the same node incarnation and semantic stamp. Query
    /// dependency edges use that same identity: final promotion selects the
    /// retained representative and walks its own complete cone, failing closed
    /// if any edge is absent. Borrowed authority never bypasses validation of a
    /// requested root itself. The returned guard owns the fallback Arcs for the
    /// complete lexical scope, so borrowed authority cannot outlive its pins.
    /// Empty sets are accepted; a set containing any foreign-runtime pin fails
    /// closed before the scope becomes active. Nested scopes form one safe
    /// authority union: a fallback first introduced by an inner scope remains
    /// pinned and usable until the oldest enclosing endorsement guard drops,
    /// because endorsements proven inside that scope are promoted outward too.
    pub fn endorse_registered_validations_from(
        &self,
        fallbacks: &[Arc<RetainedPinSet>],
    ) -> Result<ValidationEndorsementGuard, RetainTerminalConeError> {
        self.task.push_validation_endorsement_scope(fallbacks)
    }

    /// Requests a dependency in the same task and pinned revision.
    pub fn query<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        key: K,
        compute: F,
    ) -> Result<Arc<QueryTerminal<V>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        // Caller-supplied evaluators have no family-owned re-demand authority.
        // Crossing this boundary anywhere inside a registered-only validation
        // walk must fail the certificate closed, including when a structured
        // batch child computes the unregistered node rather than validating a
        // retained one.
        self.task.taint_validation_proofs();
        let request_id = self.task.next_nested_request();
        let result = if Arc::ptr_eq(&self.task_runtime(), &family.core) {
            family.query_task(self.task.clone(), key.clone(), request_id, compute)
        } else {
            TaskQueryResult::Aborted {
                abort: QueryAbort::ForeignRuntime,
                dependencies: Vec::new(),
                inputs: Vec::new(),
                work: Vec::new(),
            }
        };
        // Lazy display identity: the hot path reuses the terminal's identity in
        // `record_nested`; the key is only formatted if this request aborted.
        self.task.record_nested(
            request_id,
            move || {
                NodeIdentity::new(
                    family.inner.name.clone(),
                    key.stable_identity().into_boxed_str(),
                )
            },
            &result,
        );
        result.into_result()
    }

    /// Requests a dependency through its canonical family-owned evaluator.
    pub fn query_registered<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        key: K,
    ) -> Result<Arc<QueryTerminal<V>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "closure-free dependency requests require a registered evaluator"
        );
        let request_id = self.task.next_nested_request();
        let result = if Arc::ptr_eq(&self.task_runtime(), &family.core) {
            family.query_task_registered(self.task.clone(), key.clone(), request_id)
        } else {
            TaskQueryResult::Aborted {
                abort: QueryAbort::ForeignRuntime,
                dependencies: Vec::new(),
                inputs: Vec::new(),
                work: Vec::new(),
            }
        };
        // Lazy display identity: the hot path reuses the terminal's identity in
        // `record_nested`; the key is only formatted if this request aborted.
        self.task.record_nested(
            request_id,
            move || {
                NodeIdentity::new(
                    family.inner.name.clone(),
                    key.stable_identity().into_boxed_str(),
                )
            },
            &result,
        );
        result.into_result()
    }

    /// Requests a stable-ordered batch of dependencies through one registered
    /// family.
    ///
    /// Every item executes in a distinct child task, inheriting this task's
    /// immutable revision and cancellation token while owning its own
    /// wait-graph identity and execution permit. The parent donates its permit
    /// for the complete join interval, which makes the same interface
    /// progress-guaranteed when the runtime has one permit.
    ///
    /// Results are reduced in input order, independent of worker completion
    /// order. Dependency observations, request leases, handoffs, work, and the
    /// operational nested-attempt ledger are transferred into the parent before
    /// a child task can release them. Callers must therefore supply keys in
    /// their semantic stable order.
    ///
    /// When the parent has an active registered-validation scope, a child which
    /// reaches a terminal atomically publishes its registered-only proof and
    /// backing leases to this batch. Later siblings may borrow that authority
    /// without repeating the same recursive validation. The authority is
    /// lexical and moves into the parent before this method returns; unrelated
    /// batches, requests, and revisions share no mutable proof state.
    ///
    /// The wait graph stores each child as a compact index into this batch's
    /// typed key table. The table outlives every registered edge, including
    /// queued and already-completed children, and produces a display identity
    /// only if a scheduling cycle needs a diagnostic path.
    pub fn query_registered_batch<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        keys: impl IntoIterator<Item = K>,
    ) -> Result<Vec<Arc<QueryTerminal<V>>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "closure-free dependency requests require a registered evaluator"
        );
        if !Arc::ptr_eq(&self.task_runtime(), &family.core) {
            return Err(QueryAbort::ForeignRuntime);
        }
        let items = Arc::new(RegisteredBatchItems {
            family: family.inner.name.clone(),
            items: keys
                .into_iter()
                .map(|key| RegisteredBatchItem {
                    request_id: self.task.next_nested_request(),
                    key,
                })
                .collect(),
        });
        if items.items.is_empty() {
            return Ok(Vec::new());
        }

        let wait_labels: Arc<dyn StructuredWaitLabels> = items.clone();
        let structured_waits = StructuredWaitGuard::new(
            self.task.core.clone(),
            self.task.id,
            wait_labels,
            items
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| (TaskId(item.request_id), index)),
        )
        .map_err(QueryAbort::Cycle)?;
        let worker_claim =
            BatchWorkerClaim::new(self.task.core.clone(), items.items.len().saturating_sub(1));
        let queue = Arc::new(Mutex::new(VecDeque::from_iter(0..items.items.len())));
        let batch_authority = Arc::new(BatchValidationAuthority::new(
            self.task.core.clone(),
            self.task.batch_validation_authority.clone(),
        ));
        // `tracing` dispatch is thread-local. Carry the caller's subscriber into
        // each child so scheduled evaluation keeps compiler timing/log events.
        let tracing_dispatch = tracing::dispatcher::get_default(Clone::clone);
        let tracing_parent = tracing::Span::current();
        let donation = ParentPermitDonation::new(self.task.clone());
        if donation.donated {
            self.task
                .core
                .metrics
                .donated_permits
                .fetch_add(1, Ordering::Relaxed);
        }
        let (mut completed, panic) = std::thread::scope(|scope| {
            let mut workers = Vec::with_capacity(worker_claim.count);
            for _ in 0..worker_claim.count {
                let queue = queue.clone();
                let items = items.clone();
                let family = family.clone();
                let parent = self.task.clone();
                let authority = batch_authority.clone();
                let tracing_dispatch = tracing_dispatch.clone();
                let tracing_parent = tracing_parent.clone();
                workers.push(
                    std::thread::Builder::new()
                        .name("rue-query-batch".into())
                        .stack_size(REGISTERED_BATCH_WORKER_STACK_BYTES)
                        .spawn_scoped(scope, move || {
                            run_registered_batch_worker(
                                queue,
                                items,
                                family,
                                parent,
                                authority,
                                tracing_dispatch,
                                tracing_parent,
                            )
                        })
                        .expect("registered batch worker thread must spawn"),
                );
            }
            let inline = run_registered_batch_worker(
                queue.clone(),
                items.clone(),
                family.clone(),
                self.task.clone(),
                batch_authority.clone(),
                tracing_dispatch.clone(),
                tracing_parent.clone(),
            );
            let mut completed = Vec::new();
            let mut panic = None;
            match inline {
                Ok(inline_completed) => completed.extend(inline_completed),
                Err(payload) => panic = Some(payload),
            }
            for worker in workers {
                match worker
                    .join()
                    .expect("registered batch workers catch their own unwinds")
                {
                    Ok(worker_completed) => completed.extend(worker_completed),
                    Err(payload) if panic.is_none() => panic = Some(payload),
                    Err(_) => {}
                }
            }
            (completed, panic)
        });
        drop(structured_waits);
        drop(donation);
        if let Some(payload) = panic {
            resume_unwind(payload);
        }
        batch_authority.absorb_into_task(&self.task);

        completed.sort_unstable_by_key(|(index, ..)| *index);
        let mut terminals = Vec::with_capacity(completed.len());
        for (index, child, result) in completed {
            let item = &items.items[index];
            self.task
                .absorb_batch_child(&child, matches!(&result, TaskQueryResult::Terminal { .. }));
            match &result {
                TaskQueryResult::Terminal { terminal, work, .. } => {
                    self.task.observe(terminal);
                    self.task.observe_work(work);
                    self.task
                        .cache_query(family.inner.token, &item.key, terminal);
                }
                TaskQueryResult::Aborted {
                    dependencies,
                    inputs,
                    work,
                    ..
                } => self.task.observe_abort_prefix(dependencies, inputs, work),
            }
            self.task.record_nested(
                item.request_id,
                || {
                    NodeIdentity::new(
                        items.family.clone(),
                        item.key.stable_identity().into_boxed_str(),
                    )
                },
                &result,
            );
            terminals.push(result.into_result()?);
        }
        Ok(terminals)
    }

    /// Duplicates every terminal lease currently observed by this rooted task.
    ///
    /// The returned set is acquired while the task's original leases remain
    /// live, so a publication handoff can promote the exact dependency cone
    /// past request completion without an eviction window.
    pub fn retain_observed_terminals(&self) -> RetainedPinSet {
        retain_task_observations(&self.task)
    }

    /// Duplicates only the observed terminals reachable from one exact root.
    ///
    /// Validation can temporarily observe terminals from a superseded
    /// predecessor before recomputing a successor. This graph walk follows the
    /// immutable dependency observations of `root` through the task's live
    /// request leases, excluding unrelated validation pins while preserving
    /// continuous protection for the complete current cone.
    pub fn retain_observed_terminal_cone<V>(
        &self,
        root: &Arc<QueryTerminal<V>>,
    ) -> Result<RetainedPinSet, RetainTerminalConeError>
    where
        V: Clone + Send + Sync + 'static,
    {
        self.retain_observed_terminal_cones_from(std::slice::from_ref(root), &[])
    }

    /// Retain one exact current-task terminal cone, filling validation leaves
    /// omitted by green reuse from prioritized published snapshots.
    pub fn retain_observed_terminal_cone_from<V>(
        &self,
        root: &Arc<QueryTerminal<V>>,
        fallbacks: &[Arc<RetainedPinSet>],
    ) -> Result<RetainedPinSet, RetainTerminalConeError>
    where
        V: Clone + Send + Sync + 'static,
    {
        self.retain_observed_terminal_cones_from(std::slice::from_ref(root), fallbacks)
    }

    /// Retain the union of exact current-task terminal cones, filling
    /// validation leaves omitted by green reuse from prioritized published
    /// snapshots. Every root must be observed by this task; fallback snapshots
    /// may satisfy only dependency edges. Promotion uses both the explicitly
    /// supplied snapshots and every fallback promoted into the active lexical
    /// validation scope, so a descendant skipped under borrowed authority keeps
    /// the same backing pins available here. Current observations always
    /// override fallbacks, and within one source an edge selects the greatest
    /// terminal revision with the requested incarnation and stamp.
    ///
    /// The hash-indexed lease universe is built once and exact identities are
    /// hash-deduplicated while the union is walked once. Promotion is therefore
    /// expected O(N + E) in available leases and selected dependency edges,
    /// rather than rescanning or tree-searching leases per edge and per root.
    pub fn retain_observed_terminal_cones_from<V>(
        &self,
        roots: &[Arc<QueryTerminal<V>>],
        fallbacks: &[Arc<RetainedPinSet>],
    ) -> Result<RetainedPinSet, RetainTerminalConeError>
    where
        V: Clone + Send + Sync + 'static,
    {
        let mut batch_authorities = Vec::new();
        let mut next_authority = self.task.batch_validation_authority.clone();
        while let Some(authority) = next_authority {
            next_authority = authority.parent.clone();
            batch_authorities.push(authority);
        }
        let batch_authority_states = batch_authorities
            .iter()
            .map(|authority| read(&authority.state))
            .collect::<Vec<_>>();
        let mut promotion_fallbacks = {
            let scopes = lock(&self.task.validation_endorsements);
            let Some(scope) = scopes.first() else {
                return Err(RetainTerminalConeError::NoRegisteredValidationScope);
            };
            scope.fallbacks.clone()
        };
        for authority in &batch_authority_states {
            for fallback in &authority.fallbacks {
                if !promotion_fallbacks
                    .iter()
                    .any(|retained| Arc::ptr_eq(retained, fallback))
                {
                    promotion_fallbacks.push(fallback.clone());
                }
            }
        }
        for fallback in fallbacks {
            if !promotion_fallbacks
                .iter()
                .any(|retained| Arc::ptr_eq(retained, fallback))
            {
                promotion_fallbacks.push(fallback.clone());
            }
        }
        if promotion_fallbacks
            .iter()
            .any(|fallback| !fallback.belongs_to_runtime(self.task.core.identity))
        {
            return Err(RetainTerminalConeError::ForeignRuntime);
        }
        let leases = lock(&self.task.leases);
        let batch_lease_count = batch_authority_states
            .iter()
            .map(|authority| authority.leases.held.len())
            .sum::<usize>();
        let mut current_exact = HashMap::with_capacity(leases.held.len());
        let mut selected = HashMap::with_capacity(leases.held.len() + batch_lease_count);
        for lease in &leases.held {
            let identity = lease.identity();
            current_exact.insert(identity, lease.as_ref());
            let selected_for_stamp = selected
                .entry((identity.0, identity.1))
                .or_insert(lease.as_ref());
            if selected_for_stamp.identity().2 < identity.2 {
                *selected_for_stamp = lease.as_ref();
            }
        }
        for authority in &batch_authority_states {
            for lease in &authority.leases.held {
                let identity = lease.identity();
                let selected_for_stamp = selected
                    .entry((identity.0, identity.1))
                    .or_insert(lease.as_ref());
                if selected_for_stamp.identity().2 < identity.2 {
                    *selected_for_stamp = lease.as_ref();
                }
            }
        }
        for fallback in &promotion_fallbacks {
            let mut fallback_selected = HashMap::with_capacity(fallback.held.len());
            for lease in &fallback.held {
                let identity = lease.identity();
                let selected_for_stamp = fallback_selected
                    .entry((identity.0, identity.1))
                    .or_insert(lease.as_ref());
                if selected_for_stamp.identity().2 < identity.2 {
                    *selected_for_stamp = lease.as_ref();
                }
            }
            for (identity, lease) in fallback_selected {
                selected.entry(identity).or_insert(lease);
            }
        }

        let mut pending = Vec::with_capacity(roots.len());
        for root in roots {
            pending.push(
                *current_exact
                    .get(&(root.node_incarnation, root.stamp, root.revision))
                    .ok_or(RetainTerminalConeError::RootNotObserved)?,
            );
        }
        let mut retained = RetainedPinSet::new();
        let mut visited = HashSet::with_capacity(selected.len());
        while let Some(lease) = pending.pop() {
            if !visited.insert(lease.identity()) {
                continue;
            }
            for dependency in lease.dependencies() {
                pending.push(
                    *selected
                        .get(&(dependency.incarnation, dependency.stamp))
                        .ok_or_else(|| {
                            RetainTerminalConeError::DependencyNotObserved(dependency.clone())
                        })?,
                );
            }
            retained.lease_erased(lease.duplicate());
        }
        Ok(retained)
    }

    fn task_runtime(&self) -> Arc<RuntimeCore> {
        self.task.core.clone()
    }
}

fn retain_task_observations(task: &Task) -> RetainedPinSet {
    let leases = lock(&task.leases);
    let mut retained = RetainedPinSet::new();
    for lease in &leases.held {
        retained.lease_erased(lease.duplicate());
    }
    retained
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TaskId(u64);

#[derive(Debug, Default)]
struct ValidationEndorsementScope {
    /// Exact terminal identities are tested for membership on every validation
    /// memo probe. Their runtime-assigned numeric components need no ordering,
    /// so keep expected lookup constant instead of walking an ordered tree.
    identities: AHashSet<(u64, u64, Revision)>,
    /// Published pin sets borrowed as retention authority for this lexical
    /// scope. Holding the Arcs here keeps every indexed identity pinned until
    /// the enclosing guard drops.
    fallbacks: Vec<Arc<RetainedPinSet>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationEndorsementAuthority {
    Inactive,
    Missing,
    TaskLocal,
    Borrowed,
}

/// Read-mostly retention authority shared by siblings in one structured batch.
///
/// A child publishes only after its registered request reaches a terminal. Its
/// exact endorsements become visible in the same write transaction as the
/// leases and fallback roots which back them. Later siblings may therefore
/// reuse current validation certificates without rebuilding a cone that the
/// batch already owns. The authority is lexical: the batch and its children
/// hold the sole Arcs, and its pins move into the parent before the join returns.
struct BatchValidationAuthority {
    core: Arc<RuntimeCore>,
    parent: Option<Arc<BatchValidationAuthority>>,
    state: RwLock<BatchValidationAuthorityState>,
}

#[derive(Default)]
struct BatchValidationAuthorityState {
    endorsements: AHashSet<(u64, u64, Revision)>,
    fallbacks: Vec<Arc<RetainedPinSet>>,
    leases: BatchValidationLeases,
}

#[derive(Default)]
struct BatchValidationLeases {
    observed: BTreeSet<(u64, u64, Revision)>,
    held: Vec<Box<dyn ObservedLease>>,
}

impl Drop for BatchValidationLeases {
    fn drop(&mut self) {
        batched_release(&mut self.held);
    }
}

impl fmt::Debug for BatchValidationAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = read(&self.state);
        formatter
            .debug_struct("BatchValidationAuthority")
            .field("endorsements", &state.endorsements.len())
            .field("fallbacks", &state.fallbacks.len())
            .field("leases", &state.leases.held.len())
            .field("has_parent", &self.parent.is_some())
            .finish()
    }
}

#[derive(Default)]
struct TaskQueryCache {
    /// One type-erased typed-key map per unforgeable family token. The token is
    /// runtime-unique, so the concrete `K` and `V` behind an entry cannot vary.
    /// Each erased map uses independently keyed AHash: source-derived keys keep
    /// adversarial collision resistance while exact `Eq` remains authoritative.
    /// A task belongs to exactly one runtime, so its monotonic family id is a
    /// complete local key and can use the runtime-owned integer hasher.
    families: HashMap<u64, Box<dyn Any + Send + Sync>, BuildHasherDefault<IncarnationHasher>>,
}

impl fmt::Debug for TaskQueryCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskQueryCache")
            .field("families", &self.families.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Task {
    id: TaskId,
    core: Arc<RuntimeCore>,
    revision: Revision,
    cancellation: CancellationToken,
    owns_permit: AtomicBool,
    stack: Mutex<Vec<TaskFrame>>,
    /// Nodes whose evaluation structurally encloses this task but whose frames
    /// live on an ancestor task's stack. A registered batch runs its children on
    /// their own tasks, so without this the child's stack starts empty and a
    /// dependency cycle crossing a batch boundary is invisible to
    /// [`Task::stack_cycle`]. Carrying the enclosing chain keeps cycle detection
    /// structural and exact — a property of the request's shape, not of which
    /// task happened to reach a node first.
    ancestry: Arc<[ExactNodeIdentity]>,
    nested_attempts: Mutex<Vec<NestedQueryAttempt>>,
    /// Active operational-ledger selections. The top entry is already
    /// intersected with every parent scope, so one binary search decides
    /// whether a nested request row is materialized.
    nested_attempt_filters: Mutex<Vec<Arc<[Arc<str>]>>>,
    /// Lexical task-local registered-validation endorsements. An exact
    /// terminal identity is inserted into every active scope only after a
    /// complete registered-only validation traversal. Published fallback pin
    /// sets may also supply borrowed authority; every scope retains their Arcs
    /// so an indexed identity can never outlive its pin. Consequently the
    /// oldest active scope is the canonical union of all live authority.
    validation_endorsements: Mutex<Vec<ValidationEndorsementScope>>,
    /// Completed siblings' registered proofs and backing leases for the
    /// innermost structured batch containing this task. Nested batches link to
    /// the enclosing authority rather than copying its cone.
    batch_validation_authority: Option<Arc<BatchValidationAuthority>>,
    /// Active recursive validation certificates. Encountering an unregistered
    /// node taints every enclosing traversal.
    validation_proofs: Mutex<Vec<Arc<AtomicU8>>>,
    /// High-frequency validation work accumulated on this rooted request and
    /// merged into the runtime once at task completion.
    validation_work: AtomicValidationWork,
    /// Request-scoped retention leases. This task, which owns one rooted request
    /// and all of its nested observations (nested queries share the task), holds
    /// one pin per distinct terminal it has observed. The pins release together
    /// when the task drops — i.e. when the whole rooted request completes, is
    /// canceled, or is abandoned — so an actively computing terminal is protected
    /// automatically while the request lives, and gains no permanent retention
    /// after it ends.
    leases: Mutex<TaskLeases>,
    /// Exact successful results already resolved by this rooted task, indexed
    /// by their typed family key. A repeat can reuse the task-owned terminal
    /// before touching the shared family memo index; `leases` keeps every
    /// cached terminal pinned for precisely the same task lifetime.
    query_cache: Mutex<TaskQueryCache>,
    /// Pending terminal handoffs observed anywhere in this rooted task,
    /// including nested queries. Only successful top-level completion claims
    /// and commits this aggregate; abort and unwind leave it `Pending`.
    observed_handoffs: Mutex<Vec<Arc<AttemptHandoffLifecycle>>>,
    /// Lifecycle identities whose complete dependency DAG has already been
    /// proven live by this rooted task. Every cached identity remains owned by
    /// an observed root lifecycle for the task's lifetime.
    checked_handoffs: Mutex<HashSet<usize>>,
    #[cfg(test)]
    handoff_validation_visits: AtomicUsize,
    /// Ordered-index probes used by structural amplification tests. One
    /// identity lookup performs at most one probe, independent of endorsement
    /// count and lexical nesting depth.
    #[cfg(test)]
    validation_endorsement_index_probes: AtomicUsize,
}

struct ParentPermitDonation {
    task: Arc<Task>,
    donated: bool,
}

impl ParentPermitDonation {
    fn new(task: Arc<Task>) -> Self {
        let donated = task.release_permit(&task.core);
        Self { task, donated }
    }
}

impl Drop for ParentPermitDonation {
    fn drop(&mut self) {
        if self.donated {
            self.task.acquire_permit(&self.task.core);
        }
    }
}

struct BatchWorkerClaim {
    core: Arc<RuntimeCore>,
    count: usize,
}

impl BatchWorkerClaim {
    fn new(core: Arc<RuntimeCore>, desired: usize) -> Self {
        let limit = core.permits.maximum.saturating_sub(1);
        let mut current = core.batch_workers.load(Ordering::Acquire);
        let count = loop {
            let count = desired.min(limit.saturating_sub(current));
            match core.batch_workers.compare_exchange_weak(
                current,
                current + count,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break count,
                Err(actual) => current = actual,
            }
        };
        Self { core, count }
    }
}

impl Drop for BatchWorkerClaim {
    fn drop(&mut self) {
        self.core
            .batch_workers
            .fetch_sub(self.count, Ordering::AcqRel);
    }
}

struct StructuredWaitGuard {
    core: Arc<RuntimeCore>,
    parent: TaskId,
    children: Vec<TaskId>,
}

impl StructuredWaitGuard {
    fn new(
        core: Arc<RuntimeCore>,
        parent: TaskId,
        labels: Arc<dyn StructuredWaitLabels>,
        children: impl IntoIterator<Item = (TaskId, usize)>,
    ) -> Result<Self, Arc<[NodeIdentity]>> {
        let mut guard = Self {
            core,
            parent,
            children: Vec::new(),
        };
        for (child, index) in children {
            guard.core.begin_wait(
                parent,
                child,
                WaitEdgeLabel::Structured {
                    labels: labels.clone(),
                    index,
                },
            )?;
            guard.children.push(child);
        }
        Ok(guard)
    }
}

impl Drop for StructuredWaitGuard {
    fn drop(&mut self) {
        for child in self.children.drain(..) {
            self.core.end_wait(self.parent, child);
        }
    }
}

/// Unwind-safe scope for operational nested-attempt ledger filtering.
#[must_use = "dropping the guard immediately restores unfiltered nested-attempt recording"]
pub struct NestedAttemptFilterGuard {
    task: Arc<Task>,
    selection: Arc<[Arc<str>]>,
    not_send_or_sync: PhantomData<Rc<()>>,
}

/// Unwind-safe lexical scope for task-local registered validation proofs.
#[must_use = "dropping the guard restores the preceding validation-authority scope"]
pub struct ValidationEndorsementGuard {
    task: Arc<Task>,
    scope: usize,
    not_send_or_sync: PhantomData<Rc<()>>,
}

impl Drop for ValidationEndorsementGuard {
    fn drop(&mut self) {
        let mut scopes = lock(&self.task.validation_endorsements);
        assert_eq!(
            scopes.len(),
            self.scope + 1,
            "validation endorsement guards drop in lexical order"
        );
        scopes.pop();
    }
}

struct ValidationProofGuard {
    task: Arc<Task>,
    state: Arc<AtomicU8>,
}

impl ValidationProofGuard {
    fn registered_only(&self) -> bool {
        self.state.load(Ordering::Acquire) == VALIDATION_PROOF_REGISTERED
    }

    fn retryable(&self) -> bool {
        self.state.load(Ordering::Acquire) == VALIDATION_PROOF_RETRYABLE
    }
}

impl Drop for ValidationProofGuard {
    fn drop(&mut self) {
        let popped = lock(&self.task.validation_proofs)
            .pop()
            .expect("validation proof guard owns one traversal");
        assert!(
            Arc::ptr_eq(&popped, &self.state),
            "validation proof guards drop in lexical order"
        );
    }
}

impl Drop for NestedAttemptFilterGuard {
    fn drop(&mut self) {
        let popped = lock(&self.task.nested_attempt_filters)
            .pop()
            .expect("nested-attempt filter guard owns one active scope");
        assert!(
            Arc::ptr_eq(&popped, &self.selection),
            "nested-attempt filter guards drop in lexical order"
        );
    }
}

/// Terminals leased for the lifetime of one rooted request (its task).
#[derive(Default)]
struct TaskLeases {
    /// `(node incarnation, red/green stamp, terminal revision)` of every
    /// terminal this task has already leased, so re-observing a terminal never
    /// double-pins it. The revision is part of the identity because an
    /// adoption endorsement deliberately shares its predecessor's incarnation
    /// and stamp while being a DISTINCT terminal at the adopting revision —
    /// both must stay leased; collapsing them would leave the endorsement
    /// unprotected. Re-observations of one exact terminal still deduplicate.
    /// The runtime assigns every component and no consumer observes ordering,
    /// so high-frequency lease acquisition uses constant-expected membership.
    observed: AHashSet<(u64, u64, Revision)>,
    /// Live pins, type-erased across families. Dropping the task drops these,
    /// each of which decrements its terminal's pin count and re-enforces the
    /// owning family's retention bound.
    held: Vec<Box<dyn ObservedLease>>,
    /// Family/runtime retention passes deferred until this rooted request has
    /// finished publishing and all of its task leases can be released in the
    /// same one-per-family batch.
    deferred: DeferredEnforcements,
}

impl fmt::Debug for TaskLeases {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskLeases")
            .field("observed", &self.observed.len())
            .field("held", &self.held.len())
            .field("deferred_families", &self.deferred.enforcers.len())
            .finish()
    }
}

impl Drop for TaskLeases {
    /// Batched request-scoped lease release. A completed, canceled, or abandoned
    /// rooted request may hold thousands of observation pins in one family (the
    /// Caldera shape: a rooted request whose body publishes 10k+ terminals in a
    /// single family). Dropping the pins one at a time would run a full
    /// `enforce_retention` scan per pin — O(N²) precisely at request completion,
    /// while the family sits over its bound.
    ///
    /// Instead, release in two phases. First decrement every held pin
    /// (`release_deferred`), which never enforces: a decrement is a pure
    /// narrowing of protection, so ordering among released pins is free and no
    /// still-leased terminal is left unprotected. A release which removes a
    /// terminal's last pin yields a [`FamilyEnforcer`] keyed by its owning
    /// family's stable identity; overlapping pins need no scan. Enforcers are
    /// deduplicated so heterogeneous families collapse to one apiece.
    /// Second, run each distinct family's enforcement exactly once — after all of
    /// that family's decrements are visible. The result is O(pins) decrements
    /// plus O(distinct families) enforcement passes.
    fn drop(&mut self) {
        batched_release_into(&mut self.held, &mut self.deferred);
        std::mem::take(&mut self.deferred).enforce();
    }
}

impl BatchValidationAuthority {
    fn new(core: Arc<RuntimeCore>, parent: Option<Arc<BatchValidationAuthority>>) -> Self {
        Self {
            core,
            parent,
            state: RwLock::new(BatchValidationAuthorityState::default()),
        }
    }

    /// Atomically publishes one completed child's proof and its retention
    /// backing. A child without a registered-validation scope keeps its state
    /// for the ordinary ordered parent absorption path.
    fn publish_child(&self, child: &Task) {
        let child_endorsements = {
            let mut scopes = lock(&child.validation_endorsements);
            if scopes.is_empty() {
                return;
            }
            std::mem::take(&mut *scopes)
        };
        let endorsement = child_endorsements
            .last()
            .expect("a nonempty endorsement scope has a canonical outer union");
        let mut child_leases = lock(&child.leases);
        let mut state = write(&self.state);
        for lease in child_leases.held.drain(..) {
            let identity = lease.identity();
            if state.leases.observed.insert(identity) {
                state.leases.held.push(lease);
            } else {
                self.core.metrics.task_leases_released(1);
            }
        }
        child_leases.observed.clear();
        for fallback in &endorsement.fallbacks {
            if !state
                .fallbacks
                .iter()
                .any(|retained| Arc::ptr_eq(retained, fallback))
            {
                state.fallbacks.push(fallback.clone());
            }
        }
        // Publish identities last: every visible proof is now backed either by
        // an exact batch lease or by a fallback Arc retained in this state.
        state
            .endorsements
            .extend(endorsement.identities.iter().copied());
    }

    fn retains_endorsement(&self, incarnation: u64, stamp: u64, exact_revision: Revision) -> bool {
        let mut authority = Some(self);
        while let Some(current) = authority {
            let state = read(&current.state);
            if state
                .endorsements
                .contains(&(incarnation, stamp, exact_revision))
                || state
                    .fallbacks
                    .iter()
                    .any(|fallback| fallback.retains_stamp(incarnation, stamp))
            {
                return true;
            }
            authority = current.parent.as_deref();
        }
        false
    }

    /// Moves the joined batch's proof universe into its parent task before any
    /// completed child can drop. Ordered work, attempt, and handoff absorption
    /// remains separate and therefore preserves input-order reduction.
    fn absorb_into_task(&self, task: &Task) {
        let mut state = write(&self.state);
        let mut task_leases = lock(&task.leases);
        for lease in state.leases.held.drain(..) {
            let identity = lease.identity();
            if task_leases.observed.insert(identity) {
                task_leases.held.push(lease);
            } else {
                self.core.metrics.task_leases_released(1);
            }
        }
        state.leases.observed.clear();
        drop(task_leases);

        let fallbacks = std::mem::take(&mut state.fallbacks);
        let endorsements = std::mem::take(&mut state.endorsements);
        drop(state);
        for scope in lock(&task.validation_endorsements).iter_mut() {
            scope.identities.extend(endorsements.iter().copied());
            for fallback in &fallbacks {
                if !scope
                    .fallbacks
                    .iter()
                    .any(|retained| Arc::ptr_eq(retained, fallback))
                {
                    scope.fallbacks.push(fallback.clone());
                }
            }
        }
    }
}

impl Drop for BatchValidationAuthority {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.core
            .metrics
            .task_leases_released(state.leases.held.len());
    }
}

/// Two-phase batched release of a heterogeneous pin set. First decrement every
/// held pin (`release_deferred`), which never enforces — a decrement is a pure
/// narrowing of protection, so ordering among released pins is free and no
/// still-leased terminal is left unprotected. Only releases which remove a
/// terminal's last pin yield a [`FamilyEnforcer`]; they are keyed by stable
/// family identity and deduplicated to one enforcer per family.
/// Second, run each distinct family's enforcement exactly once — after all of
/// that family's decrements are visible. The result is O(pins) decrements plus
/// O(distinct families) enforcement passes. Shared by task-scoped
/// [`TaskLeases`] teardown and the session-held [`RetainedPinSet`].
fn batched_release(held: &mut Vec<Box<dyn ObservedLease>>) {
    if held.is_empty() {
        return;
    }
    let mut deferred = DeferredEnforcements::default();
    batched_release_into(held, &mut deferred);
    deferred.enforce();
}

fn batched_release_into(
    held: &mut Vec<Box<dyn ObservedLease>>,
    deferred: &mut DeferredEnforcements,
) {
    for lease in held.drain(..) {
        deferred.release(lease);
    }
}

/// A session-held set of request-scoped observation pins promoted out of a
/// completed rooted request and retained above the request's task.
///
/// [`TaskLeases`] retains a rooted request's observed terminals only for the
/// lifetime of its task; when the task drops the pins release. A caller that
/// wants a published root's exact observed terminals to stay retained *past*
/// the request — a session/revision selection root for a set of terminals
/// rather than a single one — acquires each pin while the request lease is
/// still live (so the terminal is continuously protected: the pin-under-lock
/// discipline that leaves no birth-eviction window) and transfers it here.
///
/// The set deduplicates by exact terminal identity `(node incarnation, red/green
/// stamp, terminal revision)`, so re-leasing the same terminal never double-pins
/// it. Release is the same two-phase batched teardown as [`TaskLeases`]: on drop
/// every held pin is decrement-released first, then each distinct family enforces
/// its retention bound exactly once — linear in the pin count, not quadratic,
/// which matters when a superseded root releases thousands of pins in one family.
///
/// This is the substrate for an atomic handoff between a superseded published
/// root and its successor: install the successor set (already holding every pin)
/// first, then drop the predecessor set, so no shared terminal is ever left
/// unprotected across the swap.
#[derive(Default)]
pub struct RetainedPinSet {
    /// `(node incarnation, stamp, terminal revision)` of every terminal already
    /// leased here, mirroring [`TaskLeases::observed`] so a redundant re-lease is
    /// dropped rather than double-held.
    observed: HashSet<(u64, u64, Revision)>,
    /// Node-incarnation/stamp identities retained by at least one exact
    /// terminal revision in this set. Query dependency edges use this same
    /// semantic identity, so any complete retained cone carrying the stamp can
    /// supply its representative terminal during final promotion.
    stamp_identities: HashSet<(u64, u64)>,
    /// Runtime identities represented by held pins. This makes same-runtime
    /// authority checks proportional to the number of fallback sets, not pins.
    runtime_identities: HashSet<u64>,
    /// Live pins, type-erased across families. Dropping the set drops these
    /// through the batched two-phase release.
    held: Vec<Box<dyn ObservedLease>>,
}

impl fmt::Debug for RetainedPinSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedPinSet")
            .field("observed", &self.observed.len())
            .field("stamp_identities", &self.stamp_identities.len())
            .field("runtime_identities", &self.runtime_identities)
            .field("held", &self.held.len())
            .finish()
    }
}

impl RetainedPinSet {
    /// An empty set holding no pins.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of distinct terminals currently held.
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// Whether the set holds no pins.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Transfer an already-acquired [`TerminalPin`] into the set, deduplicating
    /// by exact terminal identity. Returns whether the pin was newly retained; a
    /// redundant pin (same node incarnation, stamp, and revision already held) is
    /// dropped here rather than double-held, releasing it through the ordinary
    /// per-pin path. The caller must have acquired `pin` while the terminal was
    /// still protected — under the node lock at publication, or before the
    /// request lease that observed it releases — so there is no instant in which
    /// the terminal is exposed and evictable.
    pub fn lease<K, V>(&mut self, pin: TerminalPin<K, V>) -> bool
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        let identity = (
            pin.terminal.node_incarnation,
            pin.terminal.stamp,
            pin.terminal.revision,
        );
        if self.observed.insert(identity) {
            self.stamp_identities.insert((identity.0, identity.1));
            self.runtime_identities.insert(pin.family.core.identity);
            pin.family.core.metrics.retained_pin_acquired();
            self.held.push(Box::new(pin));
            true
        } else {
            false
        }
    }

    fn lease_erased(&mut self, lease: Box<dyn ObservedLease>) -> bool {
        let identity = lease.identity();
        if self.observed.insert(identity) {
            self.stamp_identities.insert((identity.0, identity.1));
            self.runtime_identities.insert(lease.runtime_identity());
            lease.metrics().retained_pin_acquired();
            self.held.push(lease);
            true
        } else {
            false
        }
    }

    fn retains_stamp(&self, incarnation: u64, stamp: u64) -> bool {
        self.stamp_identities.contains(&(incarnation, stamp))
    }

    fn belongs_to_runtime(&self, runtime_identity: u64) -> bool {
        self.runtime_identities.is_empty()
            || (self.runtime_identities.len() == 1
                && self.runtime_identities.contains(&runtime_identity))
    }
}

impl Drop for RetainedPinSet {
    fn drop(&mut self) {
        for lease in &self.held {
            lease.metrics().retained_pins_released(1);
        }
        batched_release(&mut self.held);
    }
}

/// A request-scoped retention lease. The concrete implementor is a
/// [`TerminalPin`], which releases the pinned root on drop. Erasing the family
/// type parameters lets one task hold observation leases across every family it
/// touches without a second retention structure.
trait ObservedLease: Send + Sync {
    fn metrics(&self) -> &Metrics;
    fn runtime_identity(&self) -> u64;
    fn identity(&self) -> (u64, u64, Revision);

    fn dependencies(&self) -> &[Observation];

    fn duplicate(&self) -> Box<dyn ObservedLease>;

    /// Decrement-only release for batched teardown. Consumes the lease and
    /// decrements its terminal's pin count immediately. When that was the last
    /// pin, it defers the owning family's `enforce_retention` into the returned
    /// [`FamilyEnforcer`] rather than running it inline; an overlapping pin
    /// returns `None`. Callers dropping many pins at once decrement all of them
    /// first, then run one pass per affected family and one aggregate pass per
    /// runtime, keeping release linear instead of quadratic.
    fn release_deferred(self: Box<Self>) -> Option<FamilyEnforcer>;
}

impl<K, V> ObservedLease for TerminalPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn metrics(&self) -> &Metrics {
        &self.family.core.metrics
    }

    fn runtime_identity(&self) -> u64 {
        self.family.core.identity
    }

    fn identity(&self) -> (u64, u64, Revision) {
        (
            self.terminal.node_incarnation,
            self.terminal.stamp,
            self.terminal.revision,
        )
    }

    fn dependencies(&self) -> &[Observation] {
        self.terminal.dependencies()
    }

    fn duplicate(&self) -> Box<dyn ObservedLease> {
        Box::new(
            self.family
                .pin_terminal(&self.terminal)
                .expect("a live terminal pin can be duplicated"),
        )
    }

    fn release_deferred(self: Box<Self>) -> Option<FamilyEnforcer> {
        // Narrow protection now: this pin no longer holds the terminal. This is
        // the same decrement `Drop` would perform, minus the enforcement pass.
        let previous = self.terminal.pins.fetch_sub(1, Ordering::AcqRel);
        assert!(
            previous > 0,
            "a deferred terminal pin releases exactly once"
        );
        // Suppress the `Drop` decrement/enforce so the boxed pin can free its
        // Arcs normally without double-releasing or scanning.
        self.deferred.store(true, Ordering::Relaxed);
        (previous == 1).then(|| self.family.retention_enforcer())
    }
}

/// A type-erased, per-family retention enforcement deferred out of a batched
/// lease release. Heterogeneous families produce heterogeneous enforcers; they
/// deduplicate by [`family_id`](Self::family_id) (the stable `FamilyInner`
/// address) so one enforcement runs per distinct family after every pin in that
/// family has been decrement-released.
struct FamilyEnforcer {
    family_id: usize,
    core: Arc<RuntimeCore>,
    enforce: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
struct DeferredEnforcements {
    enforcers: BTreeMap<usize, FamilyEnforcer>,
    runtimes: BTreeMap<u64, Arc<RuntimeCore>>,
}

impl DeferredEnforcements {
    fn insert(&mut self, enforcer: FamilyEnforcer) {
        self.runtimes
            .entry(enforcer.core.identity)
            .or_insert_with(|| enforcer.core.clone());
        self.enforcers.entry(enforcer.family_id).or_insert(enforcer);
    }

    fn release(&mut self, lease: Box<dyn ObservedLease>) {
        let Some(enforcer) = lease.release_deferred() else {
            return;
        };
        self.insert(enforcer);
    }

    fn enforce(self) {
        for (_family_id, enforcer) in self.enforcers {
            enforcer.enforce();
        }
        for (_runtime_id, runtime) in self.runtimes {
            runtime.enforce_runtime_retention();
        }
    }
}

impl FamilyEnforcer {
    /// Runs the deferred single-family enforcement pass exactly once.
    fn enforce(self) {
        (self.enforce)();
    }
}

type TaskDependencyEntry = (u64, NodeIdentity, u64);

#[derive(Debug)]
enum TaskDependencies {
    Empty,
    /// The common single-edge frame needs no side allocation. A second distinct
    /// edge promotes to expected-O(1) hashed membership so wide frames cannot
    /// turn repeated observation into quadratic work. Boxing the uncommon map
    /// keeps every task's frame compact, including zero- and one-edge frames.
    One(TaskDependencyEntry),
    Hashed(Box<AHashMap<u64, (NodeIdentity, u64)>>),
}

impl Default for TaskDependencies {
    fn default() -> Self {
        Self::Empty
    }
}

impl TaskDependencies {
    fn observe(&mut self, node: &NodeIdentity, incarnation: u64, stamp: u64) {
        match self {
            Self::Empty => *self = Self::One((incarnation, node.clone(), stamp)),
            Self::One((previous_incarnation, previous_node, previous_stamp)) => {
                if *previous_incarnation == incarnation {
                    assert_eq!(
                        &*previous_node, node,
                        "one runtime node incarnation must name exactly one display identity"
                    );
                    *previous_stamp = stamp;
                    return;
                }
                let mut hashed = Box::new(AHashMap::with_capacity(2));
                hashed.insert(
                    *previous_incarnation,
                    (previous_node.clone(), *previous_stamp),
                );
                hashed.insert(incarnation, (node.clone(), stamp));
                *self = Self::Hashed(hashed);
            }
            Self::Hashed(entries) => {
                if let Some((previous_node, _)) = entries.insert(incarnation, (node.clone(), stamp))
                {
                    assert_eq!(
                        &previous_node, node,
                        "one runtime node incarnation must name exactly one display identity"
                    );
                }
            }
        }
    }

    fn into_observations(self) -> Vec<Observation> {
        let mut observations: Vec<Observation> = match self {
            Self::Empty => Vec::new(),
            Self::One((incarnation, node, stamp)) => vec![Observation {
                node,
                incarnation,
                stamp,
            }],
            Self::Hashed(entries) => entries
                .into_iter()
                .map(|(incarnation, (node, stamp))| Observation {
                    node,
                    incarnation,
                    stamp,
                })
                .collect(),
        };
        observations.sort_unstable_by(|left, right| {
            left.node
                .cmp(&right.node)
                .then_with(|| left.incarnation.cmp(&right.incarnation))
        });
        observations
    }
}

#[derive(Debug)]
enum InlineOrderedMap<K, V> {
    Empty,
    /// Request-frame bookkeeping is commonly empty or has one identity. Keep
    /// that entry inline and allocate the ordered map only for a second key.
    One(K, V),
    Ordered(BTreeMap<K, V>),
}

impl<K, V> Default for InlineOrderedMap<K, V> {
    fn default() -> Self {
        Self::Empty
    }
}

impl<K: Ord, V> InlineOrderedMap<K, V> {
    fn insert_with(&mut self, key: K, value: V, merge: impl FnOnce(&mut V, V)) {
        match self {
            Self::Empty => *self = Self::One(key, value),
            Self::One(previous_key, previous_value) if previous_key == &key => {
                merge(previous_value, value);
            }
            Self::One(_, _) => {
                let Self::One(previous_key, previous_value) = std::mem::replace(self, Self::Empty)
                else {
                    unreachable!("the inline ordered map was matched as inline-one")
                };
                let mut ordered = BTreeMap::new();
                ordered.insert(previous_key, previous_value);
                ordered.insert(key, value);
                *self = Self::Ordered(ordered);
            }
            Self::Ordered(entries) => match entries.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(value);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    merge(entry.get_mut(), value);
                }
            },
        }
    }

    fn into_entries(self) -> Vec<(K, V)> {
        match self {
            Self::Empty => Vec::new(),
            Self::One(key, value) => vec![(key, value)],
            Self::Ordered(entries) => entries.into_iter().collect(),
        }
    }
}

#[derive(Debug)]
struct TaskFrame {
    node: ExactNodeIdentity,
    dependencies: TaskDependencies,
    inputs: InlineOrderedMap<InputIdentity, u64>,
    work: BTreeMap<Arc<str>, u64>,
    handoffs: Vec<Box<dyn QueryAttemptHandoff>>,
    observed_handoffs: Vec<Arc<AttemptHandoffLifecycle>>,
}

impl TaskFrame {
    fn observe_dependency(&mut self, node: &NodeIdentity, incarnation: u64, stamp: u64) {
        self.dependencies.observe(node, incarnation, stamp);
    }
}

struct TaskFrameOutput {
    dependencies: Vec<Observation>,
    inputs: Vec<InputObservation>,
    work: Vec<(Arc<str>, u64)>,
    handoffs: AttemptHandoffs,
}

fn commit_handoff(
    handoff: &mut dyn QueryAttemptHandoff,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    catch_unwind(AssertUnwindSafe(|| {
        let _phase = HandoffCallbackGuard::enter(HandoffCallbackPhase::Commit);
        handoff.commit();
    }))
}

fn abort_handoff(
    handoff: &mut dyn QueryAttemptHandoff,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    catch_unwind(AssertUnwindSafe(|| {
        let _phase = HandoffCallbackGuard::enter(HandoffCallbackPhase::Abort);
        handoff.abort();
    }))
}

impl TaskFrameOutput {
    fn abort_handoffs(self) {
        self.handoffs.abort();
    }
}

struct AttemptHandoffs {
    pending: Vec<Box<dyn QueryAttemptHandoff>>,
    observed: Vec<Arc<AttemptHandoffLifecycle>>,
}

impl AttemptHandoffs {
    fn into_lifecycle(mut self) -> Arc<AttemptHandoffLifecycle> {
        if self.pending.is_empty()
            && self
                .observed
                .iter()
                .all(|lifecycle| lifecycle.is_committed())
        {
            return AttemptHandoffLifecycle::shared_committed();
        }
        Arc::new(AttemptHandoffLifecycle::new(
            std::mem::take(&mut self.pending),
            std::mem::take(&mut self.observed),
        ))
    }

    fn abort(mut self) {
        self.abort_pending();
    }

    fn abort_pending(&mut self) {
        for handoff in self.pending.iter_mut().rev() {
            let _ = abort_handoff(&mut **handoff);
        }
        self.pending.clear();
    }
}

impl Drop for AttemptHandoffs {
    fn drop(&mut self) {
        self.abort_pending();
    }
}

#[derive(Debug)]
struct AttemptHandoffLifecycle {
    observed: Arc<[Arc<AttemptHandoffLifecycle>]>,
    state: Mutex<AttemptHandoffState>,
    completed: Arc<WaitCell>,
}

#[derive(Debug)]
enum AttemptHandoffState {
    Pending(Vec<Box<dyn QueryAttemptHandoff>>),
    Committing { owner: TaskId },
    Committed,
    Aborted,
}

enum AttemptHandoffCommit {
    Claimed(Vec<Box<dyn QueryAttemptHandoff>>),
    Committed,
    Aborted,
    Canceled,
}

enum RootHandoffCommitFailure {
    Canceled,
    Invalidated,
    Panicked(Box<dyn std::any::Any + Send>),
}

type HandoffCommitBatch = (
    Arc<AttemptHandoffLifecycle>,
    Vec<Box<dyn QueryAttemptHandoff>>,
);

fn rollback_handoff_batches(
    owner: TaskId,
    mut batches: Vec<HandoffCommitBatch>,
    attempted_callbacks: Vec<usize>,
) {
    assert_eq!(
        batches.len(),
        attempted_callbacks.len(),
        "root rollback records one attempted prefix per claimed lifecycle"
    );
    let mut rollback_safe = vec![true; batches.len()];
    for (index, (_, handoffs)) in batches.iter_mut().enumerate().rev() {
        let attempted = attempted_callbacks[index];
        assert!(
            attempted <= handoffs.len(),
            "an attempted callback prefix cannot exceed its lifecycle"
        );
        for handoff in handoffs[..attempted].iter_mut().rev() {
            if abort_handoff(&mut **handoff).is_err() {
                rollback_safe[index] = false;
            }
        }
        if !rollback_safe[index] {
            // This lifecycle can no longer be retried. Abort its untouched
            // suffix as fail-closed cleanup, but preserve untouched callbacks
            // in every lifecycle whose attempted prefix rolled back cleanly.
            for handoff in handoffs[attempted..].iter_mut().rev() {
                let _ = abort_handoff(&mut **handoff);
            }
        }
    }
    for ((lifecycle, handoffs), rollback_safe) in batches.into_iter().zip(rollback_safe) {
        if rollback_safe {
            lifecycle.rollback_commit(owner, handoffs);
        } else {
            lifecycle.abort_failed_commit(owner, handoffs);
        }
    }
}

impl AttemptHandoffLifecycle {
    fn new(
        handoffs: Vec<Box<dyn QueryAttemptHandoff>>,
        observed: Vec<Arc<AttemptHandoffLifecycle>>,
    ) -> Self {
        Self {
            observed: observed.into(),
            state: Mutex::new(AttemptHandoffState::Pending(handoffs)),
            completed: Arc::new(WaitCell {
                cv: Condvar::new(),
                generation: Mutex::new(0),
            }),
        }
    }

    fn committed() -> Self {
        Self {
            observed: Arc::from([]),
            state: Mutex::new(AttemptHandoffState::Committed),
            completed: Arc::new(WaitCell {
                cv: Condvar::new(),
                generation: Mutex::new(0),
            }),
        }
    }

    fn shared_committed() -> Arc<Self> {
        Self::shared_committed_ref().clone()
    }

    fn shared_committed_ref() -> &'static Arc<Self> {
        static COMMITTED: std::sync::OnceLock<Arc<AttemptHandoffLifecycle>> =
            std::sync::OnceLock::new();
        COMMITTED.get_or_init(|| Arc::new(AttemptHandoffLifecycle::committed()))
    }

    fn is_committed(&self) -> bool {
        matches!(*lock(&self.state), AttemptHandoffState::Committed)
    }

    #[cfg(test)]
    fn collect_observed(lifecycle: &Arc<Self>, observed: &mut Vec<Arc<Self>>) -> bool {
        let mut seen = observed
            .iter()
            .map(|lifecycle| Arc::as_ptr(lifecycle) as usize)
            .collect::<HashSet<_>>();
        Self::collect_observed_once(lifecycle, observed, &mut seen)
    }

    fn collect_observed_once(
        lifecycle: &Arc<Self>,
        observed: &mut Vec<Arc<Self>>,
        seen: &mut HashSet<usize>,
    ) -> bool {
        {
            let state = lock(&lifecycle.state);
            match &*state {
                AttemptHandoffState::Committed => return true,
                AttemptHandoffState::Aborted => return false,
                AttemptHandoffState::Pending(_) | AttemptHandoffState::Committing { .. } => {}
            }
        }
        let identity = Arc::as_ptr(lifecycle) as usize;
        if !seen.insert(identity) {
            return true;
        }
        for dependency in lifecycle.observed.iter() {
            if !Self::collect_observed_once(dependency, observed, seen) {
                return false;
            }
        }
        observed.push(lifecycle.clone());
        true
    }

    fn begin_commit(
        &self,
        owner: TaskId,
        cancellation: &CancellationToken,
        _core: &RuntimeCore,
    ) -> AttemptHandoffCommit {
        let cancellation_watch = cancellation.watch(&self.completed);
        let result = loop {
            if cancellation.is_canceled() {
                break AttemptHandoffCommit::Canceled;
            }
            let mut state = lock(&self.state);
            match &mut *state {
                AttemptHandoffState::Committed => break AttemptHandoffCommit::Committed,
                AttemptHandoffState::Aborted => break AttemptHandoffCommit::Aborted,
                AttemptHandoffState::Committing { .. } => {
                    drop(state);
                    self.completed.wait_until(
                        || {
                            cancellation.is_canceled()
                                || !matches!(
                                    *lock(&self.state),
                                    AttemptHandoffState::Committing { .. }
                                )
                        },
                        #[cfg(test)]
                        || _core.interpose(InterposeSite::HandoffCommitPark),
                    );
                }
                AttemptHandoffState::Pending(_) => {
                    let AttemptHandoffState::Pending(handoffs) =
                        std::mem::replace(&mut *state, AttemptHandoffState::Aborted)
                    else {
                        unreachable!("pending handoffs were selected above")
                    };
                    *state = AttemptHandoffState::Committing { owner };
                    break AttemptHandoffCommit::Claimed(handoffs);
                }
            }
        };
        cancellation.unwatch(cancellation_watch);
        result
    }

    fn finish_commit(&self, owner: TaskId) {
        let mut state = lock(&self.state);
        let AttemptHandoffState::Committing { owner: current } = &*state else {
            panic!("only a committing attempt handoff may finish root commit")
        };
        assert_eq!(*current, owner);
        *state = AttemptHandoffState::Committed;
        drop(state);
        self.completed.notify_all();
    }

    fn rollback_commit(&self, owner: TaskId, handoffs: Vec<Box<dyn QueryAttemptHandoff>>) {
        let mut state = lock(&self.state);
        let AttemptHandoffState::Committing { owner: current } = &*state else {
            panic!("only a committing attempt handoff may roll back")
        };
        assert_eq!(*current, owner);
        *state = AttemptHandoffState::Pending(handoffs);
        drop(state);
        self.completed.notify_all();
    }

    fn abort_failed_commit(&self, owner: TaskId, handoffs: Vec<Box<dyn QueryAttemptHandoff>>) {
        let mut state = lock(&self.state);
        let AttemptHandoffState::Committing { owner: current } = &*state else {
            panic!("only a committing attempt handoff may fail rollback")
        };
        assert_eq!(*current, owner);
        *state = AttemptHandoffState::Aborted;
        drop(state);
        drop(handoffs);
        self.completed.notify_all();
    }

    fn abort(&self) {
        let handoffs = loop {
            let mut state = lock(&self.state);
            match &*state {
                AttemptHandoffState::Committed | AttemptHandoffState::Aborted => return,
                AttemptHandoffState::Committing { .. } => {
                    drop(state);
                    self.completed.wait_until(
                        || !matches!(*lock(&self.state), AttemptHandoffState::Committing { .. }),
                        #[cfg(test)]
                        || {},
                    );
                }
                AttemptHandoffState::Pending(_) => {
                    let AttemptHandoffState::Pending(handoffs) =
                        std::mem::replace(&mut *state, AttemptHandoffState::Aborted)
                    else {
                        unreachable!("pending handoffs were selected above")
                    };
                    break handoffs;
                }
            }
        };
        for mut handoff in handoffs.into_iter().rev() {
            let _ = abort_handoff(&mut *handoff);
        }
        self.completed.notify_all();
    }
}

impl Drop for AttemptHandoffLifecycle {
    fn drop(&mut self) {
        self.abort();
    }
}

impl Task {
    fn cached_query<K, V>(&self, family: FamilyToken, key: &K) -> Option<Arc<QueryTerminal<V>>>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        let cache = lock(&self.query_cache);
        let family_cache = cache.families.get(&family.family)?;
        family_cache
            .downcast_ref::<AHashMap<K, Arc<QueryTerminal<V>>>>()
            .expect("a family token has one typed task-cache representation")
            .get(key)
            .cloned()
    }

    fn cache_query<K, V>(&self, family: FamilyToken, key: &K, terminal: &Arc<QueryTerminal<V>>)
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        let mut cache = lock(&self.query_cache);
        let family_cache = cache
            .families
            .entry(family.family)
            .or_insert_with(|| Box::new(AHashMap::<K, Arc<QueryTerminal<V>>>::new()));
        let family_cache = family_cache
            .downcast_mut::<AHashMap<K, Arc<QueryTerminal<V>>>>()
            .expect("a family token has one typed task-cache representation");
        match family_cache.entry(key.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(terminal.clone());
            }
            std::collections::hash_map::Entry::Occupied(entry) => {
                let previous = entry.get();
                assert_eq!(previous.node_incarnation, terminal.node_incarnation);
                assert_eq!(previous.stamp, terminal.stamp);
                assert_eq!(previous.revision, terminal.revision);
            }
        }
    }

    fn defer_pin_release<K, V>(&self, pin: TerminalPin<K, V>)
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        lock(&self.leases).deferred.release(Box::new(pin));
    }

    fn defer_family_enforcement(&self, enforcer: FamilyEnforcer) {
        lock(&self.leases).deferred.insert(enforcer);
    }

    fn batch_child(
        self: &Arc<Self>,
        id: u64,
        authority: Arc<BatchValidationAuthority>,
    ) -> Arc<Self> {
        let inherited_filter = lock(&self.nested_attempt_filters).last().cloned();
        let inherited_validation_fallbacks = lock(&self.validation_endorsements)
            .first()
            .map(|scope| scope.fallbacks.clone());
        // These flags are intentionally shared: crossing an unregistered
        // evaluator in any structured descendant taints every enclosing
        // registered-only validation walk.
        let inherited_validation_proofs = lock(&self.validation_proofs).clone();
        // The child evaluates underneath every node this task is currently
        // inside, so those frames enclose it exactly as if they were its own.
        let inherited_ancestry: Arc<[ExactNodeIdentity]> = self
            .ancestry
            .iter()
            .cloned()
            .chain(lock(&self.stack).iter().map(|frame| frame.node.clone()))
            .collect();
        Arc::new(Self {
            id: TaskId(id),
            core: self.core.clone(),
            revision: self.revision,
            cancellation: self.cancellation.clone(),
            owns_permit: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
            ancestry: inherited_ancestry,
            nested_attempts: Mutex::new(Vec::new()),
            nested_attempt_filters: Mutex::new(inherited_filter.into_iter().collect()),
            // A batch child is a structured descendant of the lexical proof
            // scope. It starts with no task-local endorsements, but keeps the
            // same borrowed published authority live while it validates its
            // own selected cone.
            validation_endorsements: Mutex::new(
                inherited_validation_fallbacks
                    .map(|fallbacks| ValidationEndorsementScope {
                        identities: AHashSet::new(),
                        fallbacks,
                    })
                    .into_iter()
                    .collect(),
            ),
            batch_validation_authority: Some(authority),
            validation_proofs: Mutex::new(inherited_validation_proofs),
            validation_work: AtomicValidationWork::default(),
            leases: Mutex::new(TaskLeases::default()),
            query_cache: Mutex::new(TaskQueryCache::default()),
            observed_handoffs: Mutex::new(Vec::new()),
            checked_handoffs: Mutex::new(HashSet::new()),
            #[cfg(test)]
            handoff_validation_visits: AtomicUsize::new(0),
            #[cfg(test)]
            validation_endorsement_index_probes: AtomicUsize::new(0),
        })
    }

    fn absorb_batch_child(&self, child: &Arc<Self>, transfer_handoffs: bool) {
        self.validation_work.add(child.validation_work.take());

        let mut child_leases = lock(&child.leases);
        let mut parent_leases = lock(&self.leases);
        for lease in child_leases.held.drain(..) {
            let identity = lease.identity();
            if parent_leases.observed.insert(identity) {
                parent_leases.held.push(lease);
            } else {
                self.core.metrics.task_leases_released(1);
            }
        }
        child_leases.observed.clear();
        drop(parent_leases);
        drop(child_leases);

        let child_endorsements = std::mem::take(&mut *lock(&child.validation_endorsements));
        if let Some(child_endorsements) = child_endorsements.last() {
            for scope in lock(&self.validation_endorsements).iter_mut() {
                scope
                    .identities
                    .extend(child_endorsements.identities.iter().copied());
                for fallback in &child_endorsements.fallbacks {
                    if !scope
                        .fallbacks
                        .iter()
                        .any(|retained| Arc::ptr_eq(retained, fallback))
                    {
                        scope.fallbacks.push(fallback.clone());
                    }
                }
            }
        }

        let child_attempts = std::mem::take(&mut *lock(&child.nested_attempts));
        lock(&self.nested_attempts).extend(child_attempts);

        let child_handoffs = std::mem::take(&mut *lock(&child.observed_handoffs));
        if !transfer_handoffs {
            return;
        }
        lock(&self.checked_handoffs).extend(std::mem::take(&mut *lock(&child.checked_handoffs)));
        assert!(
            child_handoffs.len() <= 1,
            "a structured child returns at most one encapsulating root lifecycle"
        );
        for handoff in child_handoffs {
            assert!(
                self.observe_handoff(handoff),
                "a successful structured child returns a live handoff lifecycle"
            );
        }
    }

    fn next_nested_request(&self) -> u64 {
        self.core.next_task.fetch_add(1, Ordering::Relaxed)
    }

    fn push_nested_attempt_filter(self: &Arc<Self>, families: &[&str]) -> NestedAttemptFilterGuard {
        let mut selection = families
            .iter()
            .copied()
            .map(Arc::<str>::from)
            .collect::<Vec<_>>();
        selection.sort();
        selection.dedup();
        let mut filters = lock(&self.nested_attempt_filters);
        if let Some(parent) = filters.last() {
            selection.retain(|family| parent.binary_search(family).is_ok());
        }
        let selection: Arc<[Arc<str>]> = selection.into();
        filters.push(selection.clone());
        NestedAttemptFilterGuard {
            task: self.clone(),
            selection,
            not_send_or_sync: PhantomData,
        }
    }

    fn records_nested_attempt(&self, family: &str) -> bool {
        lock(&self.nested_attempt_filters)
            .last()
            .is_none_or(|selection| {
                selection
                    .binary_search_by(|candidate| candidate.as_ref().cmp(family))
                    .is_ok()
            })
    }

    fn push_validation_endorsement_scope(
        self: &Arc<Self>,
        fallbacks: &[Arc<RetainedPinSet>],
    ) -> Result<ValidationEndorsementGuard, RetainTerminalConeError> {
        if fallbacks
            .iter()
            .any(|fallback| !fallback.belongs_to_runtime(self.core.identity))
        {
            return Err(RetainTerminalConeError::ForeignRuntime);
        }
        let mut scopes = lock(&self.validation_endorsements);
        let scope = scopes.len();
        let mut retained_fallbacks = scopes
            .first()
            .map(|scope| scope.fallbacks.clone())
            .unwrap_or_default();
        for fallback in fallbacks {
            if !retained_fallbacks
                .iter()
                .any(|retained| Arc::ptr_eq(retained, fallback))
            {
                retained_fallbacks.push(fallback.clone());
            }
        }
        // A fallback introduced by a nested scope remains live until the
        // oldest enclosing scope drops. Endorsements proven while nested are
        // inserted into every scope, so their borrowed authority must have the
        // same enclosing lifetime.
        for active in &mut *scopes {
            for fallback in &retained_fallbacks {
                if !active
                    .fallbacks
                    .iter()
                    .any(|retained| Arc::ptr_eq(retained, fallback))
                {
                    active.fallbacks.push(fallback.clone());
                }
            }
        }
        scopes.push(ValidationEndorsementScope {
            identities: AHashSet::new(),
            fallbacks: retained_fallbacks,
        });
        Ok(ValidationEndorsementGuard {
            task: self.clone(),
            scope,
            not_send_or_sync: PhantomData,
        })
    }

    fn validation_endorsement_authority_for_terminal<V>(
        &self,
        terminal: &QueryTerminal<V>,
    ) -> ValidationEndorsementAuthority {
        self.validation_endorsement_authority_at(
            terminal.node_incarnation,
            terminal.stamp,
            terminal.revision,
        )
    }

    fn validation_endorsement_authority_at(
        &self,
        incarnation: u64,
        stamp: u64,
        exact_revision: Revision,
    ) -> ValidationEndorsementAuthority {
        let scopes = lock(&self.validation_endorsements);
        let Some(scope) = scopes.first() else {
            return ValidationEndorsementAuthority::Inactive;
        };
        self.validation_work
            .endorsement_probes
            .fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        self.validation_endorsement_index_probes
            .fetch_add(1, Ordering::Relaxed);
        let task_local = scope
            .identities
            .contains(&(incarnation, stamp, exact_revision));
        let authority = if task_local {
            ValidationEndorsementAuthority::TaskLocal
        } else if scope
            .fallbacks
            .iter()
            .any(|fallback| fallback.retains_stamp(incarnation, stamp))
            || self
                .batch_validation_authority
                .as_ref()
                .is_some_and(|authority| {
                    authority.retains_endorsement(incarnation, stamp, exact_revision)
                })
        {
            ValidationEndorsementAuthority::Borrowed
        } else {
            ValidationEndorsementAuthority::Missing
        };
        authority
    }

    fn record_validation_endorsement_hit(&self) {
        self.validation_work
            .endorsement_hits
            .fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn validation_endorsed_identity(
        &self,
        incarnation: u64,
        stamp: u64,
        exact_revision: Revision,
    ) -> bool {
        let scopes = lock(&self.validation_endorsements);
        let Some(scope) = scopes.first() else {
            return false;
        };
        self.validation_work
            .endorsement_probes
            .fetch_add(1, Ordering::Relaxed);
        self.validation_endorsement_index_probes
            .fetch_add(1, Ordering::Relaxed);
        scope
            .identities
            .contains(&(incarnation, stamp, exact_revision))
    }

    #[cfg(test)]
    fn validation_endorsed<V>(&self, terminal: &QueryTerminal<V>) -> bool {
        self.validation_endorsement_authority_for_terminal(terminal)
            == ValidationEndorsementAuthority::TaskLocal
    }

    fn endorse_validation<V>(&self, terminal: &QueryTerminal<V>) {
        let identity = (terminal.node_incarnation, terminal.stamp, terminal.revision);
        for scope in lock(&self.validation_endorsements).iter_mut() {
            scope.identities.insert(identity);
        }
    }

    fn begin_validation(self: &Arc<Self>) -> ValidationProofGuard {
        let state = Arc::new(AtomicU8::new(VALIDATION_PROOF_REGISTERED));
        lock(&self.validation_proofs).push(state.clone());
        ValidationProofGuard {
            task: self.clone(),
            state,
        }
    }

    fn taint_validation_proofs(&self) {
        for proof in lock(&self.validation_proofs).iter() {
            proof.store(VALIDATION_PROOF_UNREGISTERED, Ordering::Release);
        }
    }

    fn defer_validation_proofs(&self) {
        for proof in lock(&self.validation_proofs).iter() {
            let _ = proof.compare_exchange(
                VALIDATION_PROOF_REGISTERED,
                VALIDATION_PROOF_RETRYABLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    /// Records a nested request's lifecycle. The display identity is materialized
    /// lazily: the hot memo-hit/compute path reuses the terminal's already-built
    /// `NodeIdentity`, so no `stable_identity()` is formatted per request. Only
    /// the cold abort branch invokes `fallback_node`, which formats the key.
    fn record_nested<V>(
        &self,
        id: u64,
        fallback_node: impl FnOnce() -> NodeIdentity,
        result: &TaskQueryResult<V>,
    ) {
        let family = match result {
            TaskQueryResult::Terminal { terminal, .. } => terminal.node.family(),
            TaskQueryResult::Aborted { .. } => {
                let node = fallback_node();
                self.core
                    .metrics
                    .record_abort_fallback_identity(node.key().len());
                if !self.records_nested_attempt(node.family()) {
                    return;
                }
                let attempt = match result {
                    TaskQueryResult::Aborted {
                        abort,
                        dependencies,
                        inputs,
                        work,
                    } => NestedQueryAttempt {
                        id,
                        node,
                        node_incarnation: None,
                        origin_request: id,
                        execution: RequestExecution::Aborted,
                        terminal_revision: None,
                        terminal_stamp: None,
                        abort: Some(abort.clone()),
                        dependencies: dependencies.clone().into(),
                        inputs: inputs.clone().into(),
                        work: work.clone().into(),
                    },
                    TaskQueryResult::Terminal { .. } => unreachable!(),
                };
                lock(&self.nested_attempts).push(attempt);
                return;
            }
        };
        if !self.records_nested_attempt(family) {
            return;
        }
        let attempt = match result {
            TaskQueryResult::Terminal {
                terminal,
                execution,
                work,
            } => NestedQueryAttempt {
                id,
                node: terminal.node.clone(),
                node_incarnation: Some(terminal.node_incarnation),
                origin_request: terminal.origin_request_id(),
                execution: *execution,
                terminal_revision: Some(terminal.revision()),
                terminal_stamp: Some(terminal.stamp()),
                abort: None,
                dependencies: terminal.dependencies.clone(),
                inputs: terminal.inputs.clone(),
                work: work.clone().into(),
            },
            TaskQueryResult::Aborted { .. } => unreachable!(),
        };
        lock(&self.nested_attempts).push(attempt);
    }

    fn acquire_permit(&self, core: &Arc<RuntimeCore>) -> bool {
        if self.owns_permit.load(Ordering::Acquire) {
            return false;
        }
        core.permits.acquire();
        assert!(!self.owns_permit.swap(true, Ordering::AcqRel));
        true
    }

    fn release_permit(&self, core: &Arc<RuntimeCore>) -> bool {
        if !self.owns_permit.swap(false, Ordering::AcqRel) {
            return false;
        }
        core.permits.release();
        true
    }

    fn observe_handoff(&self, handoff: Arc<AttemptHandoffLifecycle>) -> bool {
        if Arc::ptr_eq(&handoff, AttemptHandoffLifecycle::shared_committed_ref())
            || handoff.is_committed()
        {
            return true;
        }
        if !self.validate_handoff(&handoff) {
            return false;
        }
        let mut stack = lock(&self.stack);
        if let Some(frame) = stack.last_mut() {
            if !frame
                .observed_handoffs
                .iter()
                .any(|current| Arc::ptr_eq(current, &handoff))
            {
                frame.observed_handoffs.push(handoff);
            }
            return true;
        }
        drop(stack);
        let mut observed = lock(&self.observed_handoffs);
        if !observed
            .iter()
            .any(|current| Arc::ptr_eq(current, &handoff))
        {
            // Keep only the returned root. It owns its dependency lifecycle
            // DAG, which the commit barrier expands once in dependency order.
            observed.push(handoff);
        }
        true
    }

    fn validate_handoff(&self, handoff: &Arc<AttemptHandoffLifecycle>) -> bool {
        let mut checked = lock(&self.checked_handoffs);
        let mut newly_checked = HashSet::new();
        if !self.validate_handoff_once(handoff, &checked, &mut newly_checked) {
            return false;
        }
        checked.extend(newly_checked);
        true
    }

    fn validate_handoff_once(
        &self,
        lifecycle: &Arc<AttemptHandoffLifecycle>,
        checked: &HashSet<usize>,
        newly_checked: &mut HashSet<usize>,
    ) -> bool {
        let identity = Arc::as_ptr(lifecycle) as usize;
        if checked.contains(&identity) || !newly_checked.insert(identity) {
            return true;
        }
        #[cfg(test)]
        self.handoff_validation_visits
            .fetch_add(1, Ordering::Relaxed);
        {
            let state = lock(&lifecycle.state);
            match &*state {
                AttemptHandoffState::Committed => return true,
                AttemptHandoffState::Aborted => return false,
                AttemptHandoffState::Pending(_) | AttemptHandoffState::Committing { .. } => {}
            }
        }
        lifecycle
            .observed
            .iter()
            .all(|dependency| self.validate_handoff_once(dependency, checked, newly_checked))
    }

    fn commit_handoffs(&self) -> Result<(), RootHandoffCommitFailure> {
        assert!(
            !self.owns_permit.load(Ordering::Acquire),
            "root handoffs commit only after releasing the execution permit"
        );
        let roots = std::mem::take(&mut *lock(&self.observed_handoffs));
        let mut observed = Vec::new();
        let mut seen = HashSet::new();
        for lifecycle in roots {
            if !AttemptHandoffLifecycle::collect_observed_once(&lifecycle, &mut observed, &mut seen)
            {
                return Err(RootHandoffCommitFailure::Invalidated);
            }
        }
        let mut acquisition_order = observed.into_iter().enumerate().collect::<Vec<_>>();
        acquisition_order.sort_unstable_by_key(|(_, lifecycle)| Arc::as_ptr(lifecycle));
        let mut ordered_batches = Vec::with_capacity(acquisition_order.len());
        for (observation_order, lifecycle) in acquisition_order {
            match lifecycle.begin_commit(self.id, &self.cancellation, &self.core) {
                AttemptHandoffCommit::Claimed(handoffs) => {
                    ordered_batches.push((observation_order, lifecycle, handoffs));
                }
                AttemptHandoffCommit::Committed => {}
                AttemptHandoffCommit::Aborted => {
                    let batches = ordered_batches
                        .into_iter()
                        .map(|(_, lifecycle, handoffs)| (lifecycle, handoffs))
                        .collect::<Vec<_>>();
                    let attempted_callbacks = vec![0; batches.len()];
                    rollback_handoff_batches(self.id, batches, attempted_callbacks);
                    return Err(RootHandoffCommitFailure::Invalidated);
                }
                AttemptHandoffCommit::Canceled => {
                    let batches = ordered_batches
                        .into_iter()
                        .map(|(_, lifecycle, handoffs)| (lifecycle, handoffs))
                        .collect::<Vec<_>>();
                    let attempted_callbacks = vec![0; batches.len()];
                    rollback_handoff_batches(self.id, batches, attempted_callbacks);
                    return Err(RootHandoffCommitFailure::Canceled);
                }
            }
        }
        ordered_batches.sort_unstable_by_key(|(order, _, _)| *order);
        let mut batches = ordered_batches
            .into_iter()
            .map(|(_, lifecycle, handoffs)| (lifecycle, handoffs))
            .collect::<Vec<_>>();

        let mut failure = None;
        let mut attempted_callbacks = vec![0; batches.len()];
        'commit: for (batch_index, (_, handoffs)) in batches.iter_mut().enumerate() {
            for handoff in handoffs {
                if self.cancellation.is_canceled() {
                    failure = Some(RootHandoffCommitFailure::Canceled);
                    break 'commit;
                }
                // Count the callback before entering user code: a panicking
                // commit may already have installed state that abort must undo.
                attempted_callbacks[batch_index] += 1;
                if let Err(payload) = commit_handoff(&mut **handoff) {
                    failure = Some(RootHandoffCommitFailure::Panicked(payload));
                    break 'commit;
                }
            }
        }
        if failure.is_none() && self.cancellation.is_canceled() {
            failure = Some(RootHandoffCommitFailure::Canceled);
        }
        if let Some(failure) = failure {
            // Root publication is one transaction. Roll back every handoff,
            // in the attempted prefix, including earlier successful callbacks,
            // before making any of the terminals claimable by another root.
            // Unattempted callbacks retain their pending state for retry.
            rollback_handoff_batches(self.id, batches, attempted_callbacks);
            return Err(failure);
        }

        for (lifecycle, _) in batches {
            lifecycle.finish_commit(self.id);
        }
        Ok(())
    }

    fn discard_observed_handoffs(&self) {
        lock(&self.observed_handoffs).clear();
    }

    fn push(&self, node: ExactNodeIdentity) {
        lock(&self.stack).push(TaskFrame {
            node,
            dependencies: TaskDependencies::default(),
            inputs: InlineOrderedMap::default(),
            work: BTreeMap::new(),
            handoffs: Vec::new(),
            observed_handoffs: Vec::new(),
        });
    }

    fn pop(&self, expected: &ExactNodeIdentity) -> TaskFrameOutput {
        let frame = lock(&self.stack)
            .pop()
            .expect("query computation owns one dependency frame");
        assert_eq!(&frame.node, expected);
        let dependencies = frame.dependencies.into_observations();
        let inputs = frame
            .inputs
            .into_entries()
            .into_iter()
            .map(|(input, stamp)| InputObservation { input, stamp })
            .collect();
        TaskFrameOutput {
            dependencies,
            inputs,
            work: frame.work.into_iter().collect(),
            handoffs: AttemptHandoffs {
                pending: frame.handoffs,
                observed: frame.observed_handoffs,
            },
        }
    }

    fn register_attempt_handoff(&self, handoff: Box<dyn QueryAttemptHandoff>) {
        let mut stack = lock(&self.stack);
        let frame = stack
            .last_mut()
            .expect("attempt handoffs may be registered only inside a query evaluator");
        frame.handoffs.push(handoff);
    }

    fn observe<V>(&self, terminal: &QueryTerminal<V>) {
        if let Some(frame) = lock(&self.stack).last_mut() {
            frame.observe_dependency(&terminal.node, terminal.node_incarnation, terminal.stamp);
        }
    }

    fn observe_work(&self, work: &[(Arc<str>, u64)]) {
        if let Some(frame) = lock(&self.stack).last_mut() {
            for (identity, amount) in work {
                *frame.work.entry(identity.clone()).or_default() += amount;
            }
        }
    }

    fn record_work(&self, item: WorkItem) {
        let mut stack = lock(&self.stack);
        let frame = stack
            .last_mut()
            .expect("work recording occurs only inside a query computation");
        *frame.work.entry(item.identity).or_default() += item.amount;
    }

    fn observe_abort_prefix(
        &self,
        dependencies: &[Observation],
        inputs: &[InputObservation],
        work: &[(Arc<str>, u64)],
    ) {
        let mut stack = lock(&self.stack);
        let Some(frame) = stack.last_mut() else {
            return;
        };
        for dependency in dependencies {
            frame.observe_dependency(&dependency.node, dependency.incarnation, dependency.stamp);
        }
        for input in inputs {
            frame
                .inputs
                .insert_with(input.input.clone(), input.stamp, |previous, current| {
                    assert_eq!(*previous, current);
                });
        }
        for (identity, amount) in work {
            *frame.work.entry(identity.clone()).or_default() += amount;
        }
    }

    fn observe_input(&self, input: InputIdentity, stamp: u64) {
        let mut stack = lock(&self.stack);
        let frame = stack
            .last_mut()
            .expect("input reads occur only inside a query computation");
        frame.inputs.insert_with(input, stamp, |previous, current| {
            assert_eq!(*previous, current);
        });
    }

    fn stack_cycle(&self, node: &ExactNodeIdentity) -> Option<Arc<[NodeIdentity]>> {
        let stack = lock(&self.stack);
        let enclosing = self
            .ancestry
            .iter()
            .chain(stack.iter().map(|frame| &frame.node));
        let start = enclosing.clone().position(|frame| frame == node)?;
        Some(canonical_cycle(
            enclosing
                .skip(start)
                .map(|frame| frame.display.clone())
                .chain(std::iter::once(node.display.clone())),
        ))
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.discard_observed_handoffs();
        self.core
            .metrics
            .validation
            .add(self.validation_work.take());
        self.core
            .metrics
            .task_leases_released(lock(&self.leases).held.len());
    }
}

impl RuntimeCore {
    /// Resolves an exact node for validation, preferring the weak handle minted
    /// with its display identity. Display-only or expired identities fall back
    /// to the shared incarnation index and charge that otherwise avoidable work.
    fn validation_node(
        &self,
        identity: &NodeIdentity,
        incarnation: u64,
        work: &AtomicValidationWork,
    ) -> Option<Arc<dyn ErasedNode>> {
        if let Some(node) = identity.registered_node(self.identity, incarnation) {
            return Some(node);
        }
        work.registry_index_lookups.fetch_add(1, Ordering::Relaxed);
        self.registered_node(incarnation)
    }

    /// Upgrades one exact registry entry while holding the registry read guard,
    /// then releases that guard before the erased node can run callbacks or
    /// recurse through validation.
    fn registered_node(&self, incarnation: u64) -> Option<Arc<dyn ErasedNode>> {
        let registry = read(&self.nodes);
        let node = registry.get(&incarnation).and_then(Weak::upgrade);
        drop(registry);
        node
    }

    fn live_retention_families(&self) -> Vec<(u64, Arc<dyn RetentionFamily>)> {
        let mut registered = lock(&self.retention_families);
        let mut live = Vec::with_capacity(registered.len());
        registered.retain(|token, family| {
            if let Some(family) = family.upgrade() {
                live.push((*token, family));
                true
            } else {
                false
            }
        });
        live
    }

    fn retention_charge_snapshot_from(
        families: &[(u64, Arc<dyn RetentionFamily>)],
    ) -> RuntimeRetentionSnapshot {
        let mut snapshot = families.iter().fold(
            RuntimeRetentionSnapshot::default(),
            |mut total, (_, family)| {
                let family = family.charge_snapshot();
                total.retained_bytes = total.retained_bytes.saturating_add(family.retained_bytes);
                total.dependency_pins =
                    total.dependency_pins.saturating_add(family.dependency_pins);
                total
            },
        );
        snapshot.live_families = u64::try_from(families.len()).unwrap_or(u64::MAX);
        snapshot
    }

    fn retention_snapshot(&self) -> RuntimeRetentionSnapshot {
        Self::retention_charge_snapshot_from(&self.live_retention_families())
    }

    fn record_retention_peaks(&self, snapshot: RuntimeRetentionSnapshot) {
        self.metrics
            .peak_retained_bytes
            .fetch_max(snapshot.retained_bytes, Ordering::Relaxed);
        self.metrics
            .peak_retained_dependency_pins
            .fetch_max(snapshot.dependency_pins, Ordering::Relaxed);
    }

    fn runtime_retention_over_budget(&self, snapshot: RuntimeRetentionSnapshot) -> (bool, bool) {
        (
            snapshot.retained_bytes > self.retention_budgets.retained_bytes,
            snapshot.dependency_pins > self.retention_budgets.dependency_pins,
        )
    }

    /// A family-local deterministic charge quantum crossed. This cold probe
    /// performs the cross-family sum and touches aggregate peak/pressure state;
    /// ordinary publications remain confined to their existing family lock.
    fn enforce_runtime_retention_after_probe(&self) {
        self.metrics
            .aggregate_retention_probes
            .fetch_add(1, Ordering::Relaxed);
        let snapshot = Self::retention_charge_snapshot_from(&self.live_retention_families());
        self.record_retention_peaks(snapshot);
        if snapshot.retained_bytes < self.next_retained_byte_sweep.load(Ordering::Acquire)
            && snapshot.dependency_pins < self.next_dependency_pin_sweep.load(Ordering::Acquire)
        {
            return;
        }
        self.enforce_runtime_retention();
    }

    fn enforce_runtime_retention(&self) {
        let snapshot = Self::retention_charge_snapshot_from(&self.live_retention_families());
        self.record_retention_peaks(snapshot);
        let (bytes_over, pins_over) = self.runtime_retention_over_budget(snapshot);
        if !bytes_over && !pins_over {
            return;
        }
        self.retention_sweep_pending.store(true, Ordering::Release);
        if self
            .retention_sweep_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        loop {
            self.retention_sweep_pending.store(false, Ordering::Release);
            self.run_runtime_retention_sweep();
            #[cfg(test)]
            self.interpose(InterposeSite::RetentionSweepRelease);
            if self.retention_sweep_pending.swap(false, Ordering::AcqRel) {
                continue;
            }
            self.retention_sweep_claimed.store(false, Ordering::Release);
            // Close the handoff race: a publisher which observed the old claim
            // sets `pending` before returning. If it arrived between our final
            // pending check and release, reclaim ownership and run its pass.
            if !self.retention_sweep_pending.swap(false, Ordering::AcqRel) {
                return;
            }
            if self
                .retention_sweep_claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return;
            }
        }
    }

    fn run_runtime_retention_sweep(&self) {
        let families = self.live_retention_families();
        let initial = Self::retention_charge_snapshot_from(&families);
        self.record_retention_peaks(initial);
        let (byte_pressure, pin_pressure) = self.runtime_retention_over_budget(initial);
        if !byte_pressure && !pin_pressure {
            return;
        }
        if byte_pressure {
            self.metrics
                .retained_byte_pressure_events
                .fetch_add(1, Ordering::Relaxed);
        }
        if pin_pressure {
            self.metrics
                .dependency_pin_pressure_events
                .fetch_add(1, Ordering::Relaxed);
        }

        // Family registration is cold. Snapshot and prune its weak entries only
        // under actual pressure, then perform all eviction without the registry
        // lock. BTreeMap order is the stable family-token order.
        let mut start = families.partition_point(|(token, _)| {
            *token < self.retention_sweep_cursor.load(Ordering::Relaxed)
        });
        if start == families.len() {
            start = 0;
        }
        loop {
            let snapshot = Self::retention_charge_snapshot_from(&families);
            let (bytes_over, pins_over) = self.runtime_retention_over_budget(snapshot);
            if !bytes_over && !pins_over {
                break;
            }
            let mut progress = false;
            for offset in 0..families.len() {
                let snapshot = Self::retention_charge_snapshot_from(&families);
                let (bytes_over, pins_over) = self.runtime_retention_over_budget(snapshot);
                if !bytes_over && !pins_over {
                    break;
                }
                let index = (start + offset) % families.len();
                let (token, family) = &families[index];
                if family.evict_one() {
                    progress = true;
                    let next = families
                        .get((index + 1) % families.len())
                        .map_or(token.saturating_add(1), |(token, _)| *token);
                    self.retention_sweep_cursor.store(next, Ordering::Relaxed);
                    if bytes_over {
                        self.metrics
                            .retained_byte_evictions
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    if pins_over {
                        self.metrics
                            .dependency_pin_evictions
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            if !progress {
                break;
            }
            start = (start + 1) % families.len();
        }

        let retained = Self::retention_charge_snapshot_from(&families);
        let retained_bytes = retained.retained_bytes;
        let retained_pins = retained.dependency_pins;
        let byte_overage = retained_bytes.saturating_sub(self.retention_budgets.retained_bytes);
        let pin_overage = retained_pins.saturating_sub(self.retention_budgets.dependency_pins);
        if byte_overage > 0 {
            self.metrics
                .retained_byte_overflow_events
                .fetch_add(1, Ordering::Relaxed);
            self.metrics
                .peak_retained_byte_overage
                .fetch_max(byte_overage, Ordering::Relaxed);
        }
        if pin_overage > 0 {
            self.metrics
                .dependency_pin_overflow_events
                .fetch_add(1, Ordering::Relaxed);
            self.metrics
                .peak_dependency_pin_overage
                .fetch_max(pin_overage, Ordering::Relaxed);
        }
        let next_bytes = if byte_overage > 0 {
            retained_bytes
                .saturating_mul(2)
                .max(retained_bytes.saturating_add(1))
        } else {
            self.retention_budgets.retained_bytes.saturating_add(1)
        };
        let next_pins = if pin_overage > 0 {
            retained_pins
                .saturating_mul(2)
                .max(retained_pins.saturating_add(1))
        } else {
            self.retention_budgets.dependency_pins.saturating_add(1)
        };
        self.next_retained_byte_sweep
            .store(next_bytes, Ordering::Release);
        self.next_dependency_pin_sweep
            .store(next_pins, Ordering::Release);
    }

    fn revision_input(&self, revision: Revision, input: &InputIdentity) -> Option<u64> {
        let revisions = read(&self.revisions);
        if revisions
            .entries
            .get(&revision.id)
            .is_none_or(|entry| entry.revision != revision)
        {
            return None;
        }
        revisions.input_stamp(revision.id, input)
    }

    fn valid_for_revision<V>(
        &self,
        terminal: &QueryTerminal<V>,
        task: &Arc<Task>,
    ) -> Result<(bool, bool, bool), QueryAbort> {
        task.validation_work
            .traversals
            .fetch_add(1, Ordering::Relaxed);
        let proof = task.begin_validation();
        let valid = match self.valid_for_revision_inner(
            terminal,
            task,
            &mut ActiveValidations::default(),
        ) {
            Ok(valid) => valid,
            Err(abort) => {
                task.validation_work
                    .aborted_traversals
                    .fetch_add(1, Ordering::Relaxed);
                return Err(abort);
            }
        };
        let registered_only = proof.registered_only();
        let retryable = proof.retryable();
        if valid {
            task.validation_work
                .successful_traversals
                .fetch_add(1, Ordering::Relaxed);
            self.mark_terminal_validated(terminal, task.revision, registered_only, task);
        } else {
            task.validation_work
                .dirty_traversals
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok((valid, registered_only, retryable))
    }

    fn mark_terminal_validated<V>(
        &self,
        terminal: &QueryTerminal<V>,
        revision: Revision,
        registered_only: bool,
        task: &Task,
    ) {
        task.validation_work
            .registry_probes
            .fetch_add(1, Ordering::Relaxed);
        if let Some(node) = self.validation_node(
            &terminal.node,
            terminal.node_incarnation,
            &task.validation_work,
        ) {
            if node.mark_validated(ValidationCertificate {
                revision,
                stamp: terminal.stamp,
                terminal_revision: terminal.revision,
                registered_only,
            }) {
                task.validation_work
                    .certificates_published
                    .fetch_add(1, Ordering::Relaxed);
            }
        } else {
            task.validation_work
                .registry_misses
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn valid_for_revision_inner<V>(
        &self,
        terminal: &QueryTerminal<V>,
        task: &Arc<Task>,
        active: &mut ActiveValidations,
    ) -> Result<bool, QueryAbort> {
        if !terminal.revision.is_compatible_with(task.revision) {
            return Ok(false);
        }
        // Compatibility tokens are only a scheduling hint. Direct inputs are
        // checked exactly, while dependency stamps are validated recursively
        // against the current compatible terminal of the exact child node.
        let revisions = read(&self.revisions);
        if revisions
            .entries
            .get(&task.revision.id)
            .is_none_or(|entry| entry.revision != task.revision)
        {
            return Ok(false);
        }
        task.validation_work
            .input_observations
            .fetch_add(terminal.inputs.len() as u64, Ordering::Relaxed);
        let direct_inputs_valid = terminal.inputs.iter().all(|observed| {
            if observed.stamp == 0 {
                !revisions.input_present(task.revision.id, &observed.input)
            } else {
                revisions.input_stamp(task.revision.id, &observed.input) == Some(observed.stamp)
            }
        });
        // Registered-node validation can reenter input lookup and recursively
        // validate descendants. Never carry the revision guard across that
        // boundary: publication, lease release, and retention must remain able
        // to acquire the exclusive store guard.
        drop(revisions);
        if !direct_inputs_valid {
            return Ok(false);
        }
        for observed in terminal.dependencies.iter() {
            task.validation_work
                .dependency_observations
                .fetch_add(1, Ordering::Relaxed);
            task.validation_work
                .registry_probes
                .fetch_add(1, Ordering::Relaxed);
            let node =
                self.validation_node(&observed.node, observed.incarnation, &task.validation_work);
            let stamp = match node {
                Some(node) => match node.validated_stamp(self, task, active) {
                    Ok(stamp) => stamp,
                    // A registered descendant can depend on an externally
                    // supplied query body. If that body is unavailable while
                    // recursively validating an ancestor, the ancestor is
                    // dirty rather than canceled: its caller-supplied body may
                    // schedule or provide the missing descendant. An explicit
                    // cancellation token still aborts the whole request.
                    Err(QueryAbort::Canceled) if !task.cancellation.is_canceled() => {
                        return Ok(false);
                    }
                    // Validation follows the predecessor's dependency graph.
                    // A legal edit may reverse an edge: while computing the
                    // current parent, checking the stale child can temporarily
                    // point back to that parent. This is evidence that the
                    // retained candidate is dirty, not that the current graph
                    // is cyclic. Recompute from current inputs; a real cycle is
                    // then rediscovered by the ordinary observed request.
                    Err(QueryAbort::Cycle(_)) => return Ok(false),
                    Err(abort) => return Err(abort),
                },
                None => {
                    task.validation_work
                        .registry_misses
                        .fetch_add(1, Ordering::Relaxed);
                    None
                }
            };
            if stamp != Some(observed.stamp) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn pin_revision(self: &Arc<Self>, revision: Revision) -> Option<RevisionLease> {
        let mut revisions = write(&self.revisions);
        let entry = revisions
            .entries
            .get_mut(&revision.id)
            .filter(|entry| entry.revision == revision)?;
        entry.active_requests += 1;
        Some(RevisionLease {
            core: self.clone(),
            revision,
        })
    }

    fn enforce_revision_retention(&self, revisions: &mut RevisionStore) {
        // An overlay entry owns its parent's INPUT NODE by `Arc`, so retiring a
        // parent's revision-store entry never breaks a child's logical view;
        // retention stays a simple bounded sweep with no ancestor pinning.
        while revisions.entries.len() > REVISION_RETENTION_LIMIT {
            let Some(id) = revisions
                .entries
                .iter()
                .find_map(|(id, entry)| (entry.active_requests == 0).then_some(*id))
            else {
                break;
            };
            revisions.entries.remove(&id);
            revisions.retired_through = revisions.retired_through.max(id);
        }
    }

    #[cfg(test)]
    fn test_changed(&self) {
        *lock(&self.test_events.generation) += 1;
        self.test_events.changed.notify_all();
    }

    /// Invokes the installed interposition hook, if any, for `site`. The hook is
    /// cloned out and the lock released before calling, so the hook may reenter
    /// the runtime (issue queries, install/clear itself) without deadlocking.
    #[cfg(test)]
    fn interpose(&self, site: InterposeSite) {
        let hook = lock(&self.interpose.0).clone();
        if let Some(hook) = hook {
            hook(site);
        }
    }

    fn begin_wait(
        &self,
        waiter: TaskId,
        owner: TaskId,
        label: WaitEdgeLabel,
    ) -> Result<(), Arc<[NodeIdentity]>> {
        let mut graph = lock(&self.wait_graph);
        let previous = graph.entry(waiter).or_default().insert(owner, label);
        assert!(
            previous.is_none(),
            "one task cannot register the same wait owner twice"
        );
        let mut path = Vec::new();
        let mut visited = BTreeSet::new();
        if wait_path(&graph, owner, waiter, &mut visited, &mut path) {
            let edges = graph.get_mut(&waiter).expect("new wait edge is present");
            path.push(
                edges
                    .remove(&owner)
                    .expect("the newly inserted wait edge is present"),
            );
            if edges.is_empty() {
                graph.remove(&waiter);
            }
            drop(graph);
            return Err(canonical_cycle(
                path.into_iter()
                    .map(|label| label.node_identity(&self.metrics)),
            ));
        }
        Ok(())
    }

    fn end_wait(&self, waiter: TaskId, owner: TaskId) {
        let mut graph = lock(&self.wait_graph);
        let Some(edges) = graph.get_mut(&waiter) else {
            return;
        };
        edges.remove(&owner);
        if edges.is_empty() {
            graph.remove(&waiter);
        }
    }
}

fn wait_path(
    graph: &BTreeMap<TaskId, BTreeMap<TaskId, WaitEdgeLabel>>,
    current: TaskId,
    target: TaskId,
    visited: &mut BTreeSet<TaskId>,
    path: &mut Vec<WaitEdgeLabel>,
) -> bool {
    if !visited.insert(current) {
        return false;
    }
    let Some(edges) = graph.get(&current) else {
        return false;
    };
    for (owner, label) in edges {
        path.push(label.clone());
        if *owner == target || wait_path(graph, *owner, target, visited, path) {
            return true;
        }
        path.pop();
    }
    false
}

#[derive(Debug)]
struct PermitBudget {
    maximum: usize,
    used: Mutex<usize>,
    available: Condvar,
}

impl PermitBudget {
    fn new(maximum: usize) -> Self {
        Self {
            maximum,
            used: Mutex::new(0),
            available: Condvar::new(),
        }
    }

    fn acquire(&self) {
        let mut used = lock(&self.used);
        while *used == self.maximum {
            used = wait(&self.available, used);
        }
        *used += 1;
    }

    fn release(&self) {
        let mut used = lock(&self.used);
        assert!(*used > 0, "cannot release an unowned query permit");
        *used -= 1;
        drop(used);
        self.available.notify_one();
    }
}

/// Drop one waiter and report whether an already-terminal attempt just lost its
/// final waiter. Callers use the result only after releasing the node-state
/// lock, because retention enforcement may revisit this node.
fn decrement_waiter<V>(state: &mut NodeState<V>, attempt_id: u64) -> bool {
    if let Some(attempt) = state.attempts.iter_mut().find(|item| item.id == attempt_id) {
        match &mut attempt.state {
            AttemptState::Computing { waiters, .. } => {
                assert!(*waiters > 0, "a computing waiter releases exactly once");
                *waiters -= 1;
            }
            AttemptState::Terminal { waiters, .. } => {
                assert!(*waiters > 0, "a terminal waiter releases exactly once");
                *waiters -= 1;
                return *waiters == 0;
            }
        }
    }
    false
}

fn retained_terminal_charge<V>(
    outcome: &QueryOutcome<V>,
    retained_value_charge: Option<u64>,
    node: &NodeIdentity,
    diagnostics: &[QueryDiagnostic],
    work: &[(Arc<str>, u64)],
    dependencies: &[Observation],
    inputs: &[InputObservation],
) -> (u64, u64) {
    let mut bytes = std::mem::size_of::<QueryTerminal<V>>() as u64;
    bytes = bytes
        .saturating_add(node.family().len() as u64)
        .saturating_add(node.key().len() as u64);
    bytes = bytes.saturating_add(match outcome {
        QueryOutcome::Success(_) => retained_value_charge.unwrap_or(0),
        QueryOutcome::Failure(failure) => {
            (failure.code.len() as u64).saturating_add(failure.payload.len() as u64)
        }
    });
    for diagnostic in diagnostics {
        bytes = bytes
            .saturating_add(std::mem::size_of::<QueryDiagnostic>() as u64)
            .saturating_add(diagnostic.identity.len() as u64)
            .saturating_add(diagnostic.payload.len() as u64);
        if let Some(position) = &diagnostic.presentation {
            bytes = bytes
                .saturating_add(std::mem::size_of::<PresentationPosition>() as u64)
                .saturating_add(position.source.len() as u64);
        }
    }
    for (identity, _) in work {
        bytes = bytes
            .saturating_add(std::mem::size_of::<(Arc<str>, u64)>() as u64)
            .saturating_add(identity.len() as u64);
    }
    for dependency in dependencies {
        bytes = bytes
            .saturating_add(std::mem::size_of::<Observation>() as u64)
            .saturating_add(dependency.node.family().len() as u64)
            .saturating_add(dependency.node.key().len() as u64);
    }
    for input in inputs {
        bytes = bytes
            .saturating_add(std::mem::size_of::<InputObservation>() as u64)
            .saturating_add(input.input.family.len() as u64)
            .saturating_add(input.input.key.len() as u64);
    }
    let dependency_pins =
        u64::try_from(dependencies.len().saturating_add(inputs.len())).unwrap_or(u64::MAX);
    (bytes, dependency_pins)
}

fn canonical_diagnostics(mut diagnostics: Vec<QueryDiagnostic>) -> Vec<QueryDiagnostic> {
    diagnostics.sort_by(|left, right| {
        (&left.identity, &left.payload, &left.presentation).cmp(&(
            &right.identity,
            &right.payload,
            &right.presentation,
        ))
    });
    diagnostics
}

fn semantic_diagnostics_equal(left: &[QueryDiagnostic], right: &[QueryDiagnostic]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.identity == right.identity && left.payload == right.payload)
}

fn outcomes_equal<V>(
    value_equal: fn(&V, &V) -> bool,
    left: &QueryOutcome<V>,
    right: &QueryOutcome<V>,
) -> bool {
    match (left, right) {
        (QueryOutcome::Success(left), QueryOutcome::Success(right)) => value_equal(left, right),
        (QueryOutcome::Failure(left), QueryOutcome::Failure(right)) => left == right,
        (QueryOutcome::Success(_), QueryOutcome::Failure(_))
        | (QueryOutcome::Failure(_), QueryOutcome::Success(_)) => false,
    }
}

fn canonical_work(work: Vec<WorkItem>) -> Vec<(Arc<str>, u64)> {
    let mut aggregate = BTreeMap::<Arc<str>, u64>::new();
    for item in work {
        *aggregate.entry(item.identity).or_default() += item.amount;
    }
    aggregate.into_iter().collect()
}

fn canonical_reduced_work(work: Vec<(Arc<str>, u64)>) -> Vec<(Arc<str>, u64)> {
    let mut aggregate = BTreeMap::<Arc<str>, u64>::new();
    for (identity, amount) in work {
        *aggregate.entry(identity).or_default() += amount;
    }
    aggregate.into_iter().collect()
}

fn canonical_cycle(nodes: impl IntoIterator<Item = NodeIdentity>) -> Arc<[NodeIdentity]> {
    let mut nodes = nodes.into_iter().collect::<Vec<_>>();
    nodes.sort();
    nodes.dedup();
    nodes.into()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar
        .wait(guard)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

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

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct Key(&'static str);

    impl QueryKey for Key {
        fn stable_identity(&self) -> String {
            self.0.to_owned()
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

        assert_eq!(
            runtime.metrics().display_identities,
            DisplayIdentityMetrics {
                memo_node_materializations: 4,
                memo_node_bytes: 7,
                structured_wait_materializations: 0,
                structured_wait_bytes: 0,
                abort_fallback_materializations: 1,
                abort_fallback_bytes: 4,
            }
        );
    }

    #[test]
    fn structured_wait_labels_remain_lazy_and_live_until_cycle_rendering() {
        let runtime = QueryRuntime::new(1);
        let items = Arc::new(RegisteredBatchItems {
            family: Arc::from("lazy-structured"),
            items: vec![RegisteredBatchItem {
                request_id: 2,
                key: Key("child"),
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
                    Box::from("root"),
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

        let display_only =
            NodeIdentity::new(identity.inner.family.clone(), identity.inner.key.clone());
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
                family.inner.nodes.shard_index(candidate)
                    != family.inner.nodes.shard_index(&held_key)
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
            .family_with_evaluator::<Key, u64, _>(
                "attempt-handoff-join",
                4,
                move |context, _, _| {
                    context
                        .register_attempt_handoff(RecordingHandoff::new(evaluator_events.clone()));
                    evaluator_barrier.wait();
                    evaluator_barrier.wait();
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
        }
        .abort_handoffs();
        assert_eq!(*lock(&events), ["abort"]);

        let source = include_str!("lib.rs");
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

        let first = runtime.request_registered(
            &root,
            first_revision,
            Key("root"),
            CancellationToken::new(),
        );
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

        let first = runtime.request_registered(
            &root,
            first_revision,
            Key("root"),
            CancellationToken::new(),
        );
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
            .family_with_evaluator::<Key, u64, _>(
                "blocking-handoff-reuse",
                4,
                move |context, _, _| {
                    context.register_attempt_handoff(BlockingHandoff {
                        commit_started: commit_started_tx.clone(),
                        release_commit: evaluator_release.clone(),
                        commits: evaluator_commits.clone(),
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
            .family_with_evaluator::<Key, u64, _>(
                "blocking-handoff-permit",
                2,
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
            .family_with_evaluator::<Key, u64, _>(
                "blocking-handoff-join",
                4,
                move |context, _, _| {
                    context.register_attempt_handoff(BlockingHandoff {
                        commit_started: commit_started_tx.clone(),
                        release_commit: evaluator_release.clone(),
                        commits: evaluator_commits.clone(),
                    });
                    evaluator_body.wait();
                    evaluator_body.wait();
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
            .family_with_evaluator::<Key, u64, _>(
                "canceled-handoff-commit",
                4,
                move |context, _, _| {
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
                },
            )
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
            let attempt = runtime.request_registered(
                &family,
                revision(1),
                Key("shared"),
                cancellation.clone(),
            );
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
            cancellation: CancellationToken::new(),
            owns_permit: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
            ancestry: Arc::from([]),
            nested_attempts: Mutex::new(Vec::new()),
            nested_attempt_filters: Mutex::new(Vec::new()),
            validation_endorsements: Mutex::new(Vec::new()),
            batch_validation_authority: None,
            validation_proofs: Mutex::new(Vec::new()),
            validation_work: AtomicValidationWork::default(),
            leases: Mutex::new(TaskLeases::default()),
            query_cache: Mutex::new(TaskQueryCache::default()),
            observed_handoffs: Mutex::new(Vec::new()),
            checked_handoffs: Mutex::new(HashSet::new()),
            handoff_validation_visits: AtomicUsize::new(0),
            validation_endorsement_index_probes: AtomicUsize::new(0),
        };
        task.push(ExactNodeIdentity {
            display: NodeIdentity::new(Arc::from("handoff-test"), Box::from("root")),
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
            cancellation: CancellationToken::new(),
            owns_permit: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
            ancestry: Arc::from([]),
            nested_attempts: Mutex::new(Vec::new()),
            nested_attempt_filters: Mutex::new(Vec::new()),
            validation_endorsements: Mutex::new(Vec::new()),
            batch_validation_authority: None,
            validation_proofs: Mutex::new(Vec::new()),
            validation_work: AtomicValidationWork::default(),
            leases: Mutex::new(TaskLeases::default()),
            query_cache: Mutex::new(TaskQueryCache::default()),
            observed_handoffs: Mutex::new(Vec::new()),
            checked_handoffs: Mutex::new(HashSet::new()),
            handoff_validation_visits: AtomicUsize::new(0),
            validation_endorsement_index_probes: AtomicUsize::new(0),
        };
        task.push(ExactNodeIdentity {
            display: NodeIdentity::new(Arc::from("handoff-cache-test"), Box::from("root")),
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

        let source = include_str!("lib.rs");
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

        let source = include_str!("lib.rs");
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
            cancellation: CancellationToken::new(),
            owns_permit: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
            ancestry: Arc::from([]),
            nested_attempts: Mutex::new(Vec::new()),
            nested_attempt_filters: Mutex::new(Vec::new()),
            validation_endorsements: Mutex::new(Vec::new()),
            batch_validation_authority: None,
            validation_proofs: Mutex::new(Vec::new()),
            validation_work: AtomicValidationWork::default(),
            leases: Mutex::new(TaskLeases::default()),
            query_cache: Mutex::new(TaskQueryCache::default()),
            observed_handoffs: Mutex::new(Vec::new()),
            checked_handoffs: Mutex::new(HashSet::new()),
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
                        Ok(QueryOutput::success(value).with_diagnostics(vec![
                            QueryDiagnostic::new(
                                "leaf-warning",
                                "same semantic payload",
                                Some(PresentationPosition::new("main.rue", offset)),
                            ),
                        ]))
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
                if site == InterposeSite::NodeJoinPark && !blocked_once.swap(true, Ordering::SeqCst)
                {
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
            .family_with_evaluator::<Key, u64, _>(
                "validation-diamond-leaf",
                8,
                move |context, _, _| {
                    context.input(leaf_input.clone())?;
                    Ok(QueryOutput::success(1))
                },
            )
            .unwrap();
        let leaf_for_left = leaf.clone();
        let left = runtime
            .family_with_evaluator::<Key, u64, _>(
                "validation-diamond-left",
                8,
                move |context, _, _| {
                    context.query_registered(&leaf_for_left, Key("leaf"))?;
                    Ok(QueryOutput::success(2))
                },
            )
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
            .family_with_evaluator::<Key, u64, _>(
                "validation-diamond-root",
                8,
                move |context, _, _| {
                    context.query_registered(&left_for_root, Key("left"))?;
                    context.query_registered(&right_for_root, Key("right"))?;
                    Ok(QueryOutput::success(4))
                },
            )
            .unwrap();

        assert_eq!(
            runtime
                .request_registered(&root, first, Key("root"), CancellationToken::new())
                .execution(),
            RequestExecution::Computed
        );
        let before = runtime.metrics();
        let reused =
            runtime.request_registered(&root, second, Key("root"), CancellationToken::new());
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
            .family_with_evaluator::<Key, u64, _>(
                "proof-reacquisition-top",
                8,
                move |context, _, _| {
                    context.query_registered(&middle_for_top, Key("middle"))?;
                    Ok(QueryOutput::success(3))
                },
            )
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
        let retained_for_root = retained.clone();
        let result = runtime
            .request(
                &root,
                second,
                Key("root"),
                CancellationToken::new(),
                move |context| {
                    let _proof = context
                        .endorse_registered_validations_from(std::slice::from_ref(
                            &retained_for_root,
                        ))
                        .unwrap();
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
                        context.task.validation_endorsement_authority_for_terminal(
                            &first_terminal_for_root,
                        ),
                        ValidationEndorsementAuthority::TaskLocal,
                    );
                    assert_eq!(
                        context.task.validation_endorsement_authority_for_terminal(
                            &second_terminal_for_root,
                        ),
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
                        .endorse_registered_validations_from(std::slice::from_ref(
                            &retained_for_root,
                        ))
                        .unwrap();
                    assert_eq!(
                        context.task.validation_endorsement_authority_for_terminal(
                            &first_terminal_for_root,
                        ),
                        ValidationEndorsementAuthority::Borrowed,
                    );
                    assert_eq!(
                        context.task.validation_endorsement_authority_for_terminal(
                            &second_terminal_for_root,
                        ),
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
            .family_with_evaluator::<Key, u64, _>(
                "exact-fallback-parent",
                4,
                move |context, _, _| {
                    context.query_registered(&leaf_for_parent, Key("leaf"))?;
                    Ok(QueryOutput::success(1))
                },
            )
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
                        .endorse_registered_validations_from(std::slice::from_ref(
                            &fallback_for_root,
                        ))
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
                Ok(QueryOutput::success(10)
                    .with_work(vec![WorkItem::new("validation-leaf-work", 1)]))
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
                .any(
                    |(identity, amount)| identity.as_ref() == "validation-leaf-work"
                        && *amount == 1
                )
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
                        let _inner =
                            context.retain_nested_attempts_for(&["nested-filter-suppressed"]);
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

            let request =
                |revision, root_key, child_key, cancellation: CancellationToken, cancel| {
                    let child = child.clone();
                    let cancellation_for_body = cancellation.clone();
                    runtime.request(&root, revision, root_key, cancellation, move |context| {
                        let _filter = filtered.then(|| {
                            context.retain_nested_attempts_for(&["nested-filter-parity-child"])
                        });
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
                .filter(|nested| {
                    nested.node().family() == family && nested.node().key() == "target"
                })
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
            .family_with_evaluator::<Key, u64, _>(
                "endorsement-tainted",
                4,
                move |context, _, key| {
                    let external = context.query(&external_for_registered, key.clone(), |_| {
                        Ok(QueryOutput::success(3))
                    })?;
                    let QueryOutcome::Success(value) = external.outcome() else {
                        unreachable!()
                    };
                    Ok(QueryOutput::success(*value))
                },
            )
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
            .family_with_evaluator::<Key, u64, _>(
                "registered-self-cycle",
                4,
                |context, family, key| match key.0 {
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
                },
            )
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
            fn stable_identity(&self) -> String {
                "collision".to_owned()
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
        assert_eq!(first.node().key(), "collision");
        // The schedule-dependent incarnation stays out of canonical display
        // ordering while exact K equality still chooses distinct memo nodes even
        // under a forced hash collision.
        assert_eq!(first.node(), second.node());
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
                if site == InterposeSite::ReuseDiscovered
                    && !hook_fired.swap(true, Ordering::SeqCst)
                {
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
    // release before one strict pass per family, and the runtime-wide pass runs
    // only after those family passes have converged.
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
            3
        );
        assert_eq!(
            after.retention_scan_entries - before.retention_scan_entries,
            0
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
                            context
                                .query(&published, Slot(i), move |_| Ok(QueryOutput::success(i)))?;
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
        }

        let runtime = QueryRuntime::new(1);
        let family = runtime
            .content_addressed_family_with_equality::<FrozenKey, u64>(
                "adopt-frozen",
                8,
                PartialEq::eq,
            )
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

    fn budget_unit_charge(family: &'static str, key: &'static str, value_charge: u64) -> u64 {
        let output = QueryOutput::success(0_u64).with_retained_value_charge(value_charge);
        retained_terminal_charge(
            &output.outcome,
            output.retained_value_charge,
            &NodeIdentity::new(family.into(), key.into()),
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
        let unit = budget_unit_charge("budget-a", "0", 100);
        let runtime = QueryRuntime::with_retention_budgets(
            1,
            RetentionBudgets {
                retained_bytes: unit * 2,
                dependency_pins: u64::MAX,
            },
        );
        let first = runtime
            .family_with_equality_and_retained_charge::<Key, u64>(
                "budget-a",
                8,
                PartialEq::eq,
                |_| 100,
            )
            .unwrap();
        let second = runtime
            .family_with_equality_and_retained_charge::<Key, u64>(
                "budget-b",
                8,
                PartialEq::eq,
                |_| 100,
            )
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
        let unit = budget_unit_charge("protected", "0", 100);
        let runtime = QueryRuntime::with_retention_budgets(
            1,
            RetentionBudgets {
                retained_bytes: unit,
                dependency_pins: u64::MAX,
            },
        );
        let family = runtime
            .family_with_equality_and_retained_charge::<Key, u64>(
                "protected",
                8,
                PartialEq::eq,
                |_| 100,
            )
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
        let unit = budget_unit_charge("watermark", "0", 100);
        let runtime = QueryRuntime::with_retention_budgets(
            1,
            RetentionBudgets {
                retained_bytes: unit,
                dependency_pins: u64::MAX,
            },
        );
        let family = runtime
            .family_with_equality_and_retained_charge::<Key, u64>(
                "watermark",
                0,
                PartialEq::eq,
                |_| 100,
            )
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
}
