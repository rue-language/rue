//! Query runtime tests: cycles subsystem.

use super::fixtures::*;
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
