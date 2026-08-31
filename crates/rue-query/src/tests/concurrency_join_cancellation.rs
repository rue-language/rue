//! Query runtime tests: concurrency join cancellation subsystem.

use super::fixtures::*;
use super::*;

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

    let source = include_str!("../node.rs");
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

    let source = include_str!("../task.rs");
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

    let source = include_str!("../task.rs");
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
fn noncomputing_join_primitive_has_no_claim_or_evaluator_authority() {
    let source = include_str!("../node.rs");
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
