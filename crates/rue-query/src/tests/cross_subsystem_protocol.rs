//! Query runtime tests: cross subsystem protocol subsystem.

use super::fixtures::*;
use super::*;

#[test]
fn nested_attempt_terminal_kind_distinguishes_success_failure_and_abort() {
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct TerminalKey(&'static str);

    impl QueryKey for TerminalKey {
        fn stable_identity(&self) -> String {
            self.0.to_owned()
        }

        fn stable_hash(&self, hasher: &mut StableHasher) {
            self.0.hash(hasher);
        }
    }

    let runtime = QueryRuntime::new(1);
    let revision = Revision::new(1, 1);
    runtime.publish_revision(revision, []).unwrap();
    let success = runtime
        .family_with_evaluator::<TerminalKey, u64, _>("terminal-kind-success", 8, |_, _, _| {
            Ok(QueryOutput::success(1).with_terminal_kind(QueryTerminalKind::Success))
        })
        .unwrap();
    let success_for_root = success.clone();
    let root = runtime
        .family_with_evaluator::<TerminalKey, u64, _>(
            "terminal-kind-root",
            8,
            move |context, _, key| {
                context.query_registered(&success_for_root, key.clone())?;
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let computed = runtime.request_registered(
        &root,
        revision,
        TerminalKey("value"),
        CancellationToken::new(),
    );
    assert_eq!(computed.execution(), RequestExecution::Computed);
    assert_eq!(
        computed.nested_attempts()[0].terminal_kind(),
        Some(QueryTerminalKind::Success)
    );
    let reused = runtime.request_registered(
        &root,
        revision,
        TerminalKey("value"),
        CancellationToken::new(),
    );
    assert_eq!(reused.execution(), RequestExecution::Reused);

    let failure = runtime
        .family_with_evaluator::<TerminalKey, u64, _>("terminal-kind-failure", 8, |_, _, _| {
            Ok(QueryOutput::success(1).with_terminal_kind(QueryTerminalKind::Failure))
        })
        .unwrap();
    let failure_for_root = failure.clone();
    let failure_root = runtime
        .family_with_evaluator::<TerminalKey, u64, _>(
            "terminal-kind-failure-root",
            8,
            move |context, _, key| {
                context.query_registered(&failure_for_root, key.clone())?;
                Ok(QueryOutput::success(1))
            },
        )
        .unwrap();
    let failed = runtime.request_registered(
        &failure_root,
        revision,
        TerminalKey("value"),
        CancellationToken::new(),
    );
    assert_eq!(failed.execution(), RequestExecution::Computed);
    assert_eq!(
        failed.nested_attempts()[0].terminal_kind(),
        Some(QueryTerminalKind::Failure)
    );

    let canceled = runtime
        .family_with_evaluator::<TerminalKey, u64, _>("terminal-kind-abort", 8, |_, _, _| {
            Err(QueryAbort::Canceled)
        })
        .unwrap();
    let aborted = runtime.request_registered(
        &canceled,
        revision,
        TerminalKey("value"),
        CancellationToken::new(),
    );
    assert_eq!(aborted.execution(), RequestExecution::Aborted);
    assert!(aborted.terminal().is_none());
    assert_eq!(aborted.abort(), Some(&QueryAbort::Canceled));
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
