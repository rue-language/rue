//! Query runtime tests: revision red green subsystem.

use super::fixtures::*;
use super::*;

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
fn validation_proof_stack_keeps_common_depth_inline_without_growing_tasks() {
    assert!(std::mem::size_of::<ValidationProofStack>() <= std::mem::size_of::<Vec<u8>>());

    let mut proofs = ValidationProofStack::new();
    proofs.resize(8, VALIDATION_PROOF_REGISTERED);
    assert!(!proofs.spilled());

    proofs.push(VALIDATION_PROOF_REGISTERED);
    assert!(proofs.spilled());
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
