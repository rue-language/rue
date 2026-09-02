use super::*;
use lasso::Key;
use rue_rir::{Inst, RirEditor, RirValidationContext};
use std::cell::{Cell, RefCell};

thread_local! {
    static LABEL_CALLS: Cell<usize> = const { Cell::new(0) };
    static TICKET_EVENTS: RefCell<Vec<(usize, bool)>> = const { RefCell::new(Vec::new()) };
    static PRODUCER_CALLS: RefCell<Vec<(usize, usize, u32)>> = const { RefCell::new(Vec::new()) };
    static INTEGER_HINTS: RefCell<Vec<Option<FakeType>>> = const { RefCell::new(Vec::new()) };
    static FINISH_ARITH_OPERATIONS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static METHOD_FAILURES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static TYPE_RESOLUTION_CALLS: Cell<usize> = const { Cell::new(0) };
    static CHECKPOINTS: Cell<usize> = const { Cell::new(0) };
    static ABORT_AT_CHECKPOINT: Cell<Option<usize>> = const { Cell::new(None) };
    static EVALUATE_RHS_AFTER_REJECTION: Cell<bool> = const { Cell::new(true) };
    static CALL_ARGUMENTS: RefCell<Vec<(FakeValue, bool)>> = const { RefCell::new(Vec::new()) };
    static BINDING_FINISHES: Cell<usize> = const { Cell::new(0) };
    static PREPARE_CALLS: Cell<usize> = const { Cell::new(0) };
    static PREPARE_CANONICAL_PROBE: Cell<bool> = const { Cell::new(false) };
    static CANONICAL_FAILURE_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static DEPTH_FAILURE_VARIANT: Cell<bool> = const { Cell::new(false) };
    static ALLOW_MODULE_CALLS: Cell<bool> = const { Cell::new(false) };
    static EVALUATED_METHOD_RECEIVER_MODE: Cell<u8> = const { Cell::new(0) };
    static EVALUATED_METHOD_RECEIVERS: RefCell<Vec<FakeValue>> = const { RefCell::new(Vec::new()) };
    static EVALUATED_METHOD_EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static EVALUATED_METHOD_ARGUMENT_CALLS: Cell<usize> = const { Cell::new(0) };
    static EVALUATED_METHOD_FAIL_ON_UNIT: Cell<bool> = const { Cell::new(false) };
    static REJECT_QUALIFIED_ENUM: Cell<bool> = const { Cell::new(false) };
    static REJECT_ADMISSION: Cell<bool> = const { Cell::new(false) };
    static REJECT_BIND_AT: Cell<Option<usize>> = const { Cell::new(None) };
    static NAMED_VALUE_CALLS: Cell<usize> = const { Cell::new(0) };
    static REJECT_VISIBILITY: Cell<bool> = const { Cell::new(false) };
    static NAMED_TYPE_MISSING: Cell<bool> = const { Cell::new(false) };
    static TYPE_VALUE_PROGRAMS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static ARRAY_LENGTH_CALLS: Cell<usize> = const { Cell::new(0) };
    static ARRAY_LENGTH_INPUTS: RefCell<Vec<Option<i128>>> = const { RefCell::new(Vec::new()) };
    static ARRAY_LENGTH_ABORT: Cell<bool> = const { Cell::new(false) };
    static ANON_STRUCT_CAPTURES: RefCell<Vec<(Vec<(u32, FakeType)>, Vec<(u32, FakeValue)>)>> =
        const { RefCell::new(Vec::new()) };
    static ANON_ENUM_CAPTURES: RefCell<Vec<(Vec<(u32, FakeType)>, Vec<(u32, FakeValue)>)>> =
        const { RefCell::new(Vec::new()) };
    static KEYED_FILE_RESOLUTION: Cell<bool> = const { Cell::new(false) };
    static FILE_RESOLUTION_CALLS: RefCell<Vec<(usize, u32)>> = const { RefCell::new(Vec::new()) };
    static TYPE_INTRINSIC_EVENTS: RefCell<Vec<(ComptimeTypeIntrinsic, FakeType)>> =
        const { RefCell::new(Vec::new()) };
    static TYPE_INTRINSIC_FAILURE: Cell<bool> = const { Cell::new(false) };
    static TYPE_INTRINSIC_ABORT: Cell<bool> = const { Cell::new(false) };
    static TYPE_INTRINSIC_NAME: RefCell<Option<(u32, &'static str)>> = const { RefCell::new(None) };
    static MATCH_PATTERN_MATCHES: Cell<bool> = const { Cell::new(false) };
    static MATCH_PATTERN_FORCE_FALSE: Cell<bool> = const { Cell::new(false) };
    static MATCH_NO_SELECTED_FAILURE: Cell<bool> = const { Cell::new(false) };
    static MATCH_NO_SELECTED_SITES: RefCell<Vec<(usize, u32, u32)>> =
        const { RefCell::new(Vec::new()) };
    static REJECTION_EVENTS: RefCell<Vec<ComptimeSemanticRejection<FakeValue>>> =
        const { RefCell::new(Vec::new()) };
    static REJECTION_SITES: RefCell<Vec<(usize, u32, u32)>> =
        const { RefCell::new(Vec::new()) };
    static MATCH_PATTERN_EVENTS: RefCell<Vec<ComptimeMatchPattern<FakeName>>> =
        const { RefCell::new(Vec::new()) };
    static MATCH_SYMBOL_CALLS: Cell<usize> = const { Cell::new(0) };
    static DIAGNOSTIC_SITES: RefCell<Vec<(usize, u32, u32)>> =
        const { RefCell::new(Vec::new()) };
    static STRUCTURED_PREPARE_SPANS: RefCell<Vec<Span>> =
        const { RefCell::new(Vec::new()) };
    static EXPRESSION_INTRINSIC_REQUESTS:
        RefCell<Vec<ComptimeExpressionIntrinsicRequest<FakeName>>> =
        const { RefCell::new(Vec::new()) };
    static EXPRESSION_INTRINSIC_NAMES: RefCell<Vec<(u32, &'static str)>> =
        const { RefCell::new(Vec::new()) };
    static EXPRESSION_INTRINSIC_OUTCOME: Cell<FakeExpressionIntrinsicOutcome> =
        const { Cell::new(FakeExpressionIntrinsicOutcome::RuntimeDependent) };
}

#[test]
fn entered_frame_runner_remains_engine_private() {
    let source = super::COMPTIME_SOURCE;
    assert!(source.contains("pub(crate) fn evaluate_entered_frame("));
    let public_signature = ["pub", " fn evaluate_entered_frame("].concat();
    assert!(!source.contains(&public_signature));
}

#[test]
fn semantic_pattern_decoder_uses_the_supplied_program_name_authority() {
    let mut editor = RirEditor::new();
    let unit = editor.add_inst(Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 1),
    });
    let interner = lasso::ThreadedRodeo::new();
    let type_name = interner.get_or_intern("Os");
    let variant = interner.get_or_intern("Macos");
    let matched = editor
        .add_match(
            unit,
            &[(
                rue_rir::RirPattern::Path {
                    module: None,
                    ctor_head: None,
                    type_name,
                    variant,
                    bindings: Vec::new(),
                    span: Span::new(0, 1),
                },
                unit,
            )],
            Span::new(0, 1),
        )
        .unwrap();
    let rir = editor.finish();
    let InstData::Match { arms, .. } = &rir.get(matched).data else {
        panic!("expected match instruction");
    };
    let (pattern, _) = rir.match_arms(arms).iter().next().unwrap();
    let first = decode_comptime_match_pattern(&pattern, |symbol| {
        format!("program-1-{}", symbol.issuing_interner_ordinal())
    });
    let second = decode_comptime_match_pattern(&pattern, |symbol| {
        format!("program-2-{}", symbol.issuing_interner_ordinal())
    });
    assert_ne!(first, second);
    assert!(matches!(
        first,
        ComptimeMatchPattern::Path {
            module_qualified: false,
            ctor_qualified: false,
            binding_count: 0,
            ..
        }
    ));
}

#[test]
fn engine_decodes_match_patterns_lazily_per_active_program() {
    let interner = lasso::ThreadedRodeo::new();
    let type_name = interner.get_or_intern("Os");
    let variant = interner.get_or_intern("Macos");
    let later_type = interner.get_or_intern("Arch");
    let later_variant = interner.get_or_intern("X86_64");
    let make_program = || {
        let mut editor = RirEditor::new();
        let unit = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let root = editor
            .add_match(
                unit,
                &[
                    (
                        rue_rir::RirPattern::Path {
                            module: None,
                            ctor_head: None,
                            type_name,
                            variant,
                            bindings: Vec::new(),
                            span: Span::new(0, 1),
                        },
                        unit,
                    ),
                    (
                        rue_rir::RirPattern::Path {
                            module: None,
                            ctor_head: None,
                            type_name: later_type,
                            variant: later_variant,
                            bindings: vec![type_name],
                            span: Span::new(0, 1),
                        },
                        unit,
                    ),
                ],
                Span::new(0, 1),
            )
            .unwrap();
        (editor.finish(), root)
    };
    let (program0, root0) = make_program();
    let (program1, root1) = make_program();
    MATCH_PATTERN_MATCHES.with(|matches| matches.set(true));
    MATCH_PATTERN_EVENTS.with(|events| events.borrow_mut().clear());
    MATCH_SYMBOL_CALLS.with(|calls| calls.set(0));
    let mut host = FakeHost {
        programs: vec![program0, program1],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let (scrutinee, arms) = match &host.programs[0].get(root0).data {
        InstData::Match { scrutinee, arms } => (*scrutinee, arms.clone()),
        _ => panic!("expected match instruction"),
    };
    let mut engine = ComptimeEngine::new(&mut host);
    assert!(matches!(
        engine.select_match(0, scrutinee, &arms, &mut env),
        ComptimeOutcome::Known(ComptimeSelection::Match { arm: 0 })
    ));
    MATCH_PATTERN_EVENTS.with(|events| events.borrow_mut().clear());
    MATCH_SYMBOL_CALLS.with(|calls| calls.set(0));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, root0), &mut env),
        ComptimeOutcome::Known(FakeValue::Unit)
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(1, root1), &mut env),
        ComptimeOutcome::Known(FakeValue::Unit)
    ));
    let events = MATCH_PATTERN_EVENTS.with(|events| events.borrow().clone());
    assert_eq!(
        events.len(),
        2,
        "the later arm must not be decoded or offered"
    );
    assert_ne!(events[0], events[1], "active program must own symbol names");
    assert_eq!(MATCH_SYMBOL_CALLS.with(Cell::get), 4);
    MATCH_PATTERN_MATCHES.with(|matches| matches.set(false));
}

#[test]
fn match_without_a_selected_arm_uses_the_host_terminal_policy() {
    let make_program = || {
        let mut editor = RirEditor::new();
        let scrutinee = editor.add_inst(Inst {
            data: InstData::BoolConst(false),
            span: Span::new(0, 1),
        });
        let body = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(1, 2),
        });
        let root = editor
            .add_match(
                scrutinee,
                &[(rue_rir::RirPattern::Bool(true, Span::new(0, 1)), body)],
                Span::new(0, 2),
            )
            .unwrap();
        (editor.finish(), root)
    };
    let (program0, root0) = make_program();
    let (program1, root1) = make_program();
    let mut host = FakeHost {
        programs: vec![program0, program1],
        type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    MATCH_PATTERN_MATCHES.with(|matches| matches.set(true));
    MATCH_PATTERN_FORCE_FALSE.with(|force| force.set(true));
    MATCH_NO_SELECTED_FAILURE.with(|failure| failure.set(true));
    MATCH_NO_SELECTED_SITES.with(|sites| sites.borrow_mut().clear());
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root0), &mut env),
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(1, root1), &mut env),
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));
    MATCH_NO_SELECTED_SITES.with(|sites| {
        assert_eq!(sites.borrow().as_slice(), &[(0, 0, 2), (1, 0, 2)]);
    });

    MATCH_NO_SELECTED_FAILURE.with(|failure| failure.set(false));
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root0), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));

    MATCH_NO_SELECTED_FAILURE.with(|failure| failure.set(true));
    MATCH_NO_SELECTED_SITES.with(|sites| sites.borrow_mut().clear());
    MATCH_PATTERN_MATCHES.with(|matches| matches.set(false));
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root0), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    MATCH_NO_SELECTED_SITES.with(|sites| assert!(sites.borrow().is_empty()));

    MATCH_PATTERN_FORCE_FALSE.with(|force| force.set(false));
    MATCH_PATTERN_MATCHES.with(|matches| matches.set(false));
    MATCH_NO_SELECTED_FAILURE.with(|failure| failure.set(false));
}

#[test]
fn semantic_rejections_are_emitted_by_real_engine_dispatch() {
    let mut editor = RirEditor::new();
    let unit = editor.add_inst(Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 1),
    });
    let boolean = editor.add_inst(Inst {
        data: InstData::BoolConst(true),
        span: Span::new(1, 2),
    });
    let not_unit = editor.add_inst(Inst {
        data: InstData::Not { operand: unit },
        span: Span::new(2, 3),
    });
    let add_unit = editor.add_inst(Inst {
        data: InstData::Add {
            lhs: unit,
            rhs: boolean,
        },
        span: Span::new(3, 4),
    });
    let then_block = editor.add_block(&[unit], Span::new(4, 5)).unwrap();
    let branch_unit = editor.add_inst(Inst {
        data: InstData::Branch {
            cond: unit,
            then_block,
            else_block: None,
        },
        span: Span::new(5, 6),
    });
    let empty_block = editor.add_block(&[], Span::new(6, 7)).unwrap();
    let loop_unit = editor.add_inst(Inst {
        data: InstData::Loop {
            cond: boolean,
            body: unit,
        },
        span: Span::new(7, 8),
    });
    let assignment = editor.add_inst(Inst {
        data: InstData::Assign {
            name: lasso::Spur::default(),
            value: unit,
        },
        span: Span::new(8, 9),
    });
    let non_tail_assignment = editor
        .add_block(&[assignment, unit], Span::new(9, 10))
        .unwrap();
    let tail_assignment = editor
        .add_block(&[unit, assignment], Span::new(10, 11))
        .unwrap();
    let program = editor.finish();
    let mut host = FakeHost {
        programs: vec![program],
        type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    REJECTION_EVENTS.with(|events| events.borrow_mut().clear());
    let mut engine = ComptimeEngine::new(&mut host);
    for root in [
        not_unit,
        add_unit,
        branch_unit,
        empty_block,
        loop_unit,
        non_tail_assignment,
        tail_assignment,
    ] {
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, root), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
    }
    assert_eq!(
        REJECTION_EVENTS.with(|events| events.borrow().clone()),
        vec![
            ComptimeSemanticRejection::ConditionNotBoolean(FakeValue::Unit),
            ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                operation: ComptimeIntegerOperation::Add,
                lhs: FakeValue::Unit,
                rhs: Some(FakeValue::Boolean(true)),
            },
            ComptimeSemanticRejection::ConditionNotBoolean(FakeValue::Unit),
            ComptimeSemanticRejection::EmptyBlock,
            ComptimeSemanticRejection::UnsupportedExpression,
            ComptimeSemanticRejection::Assignment,
            ComptimeSemanticRejection::UnsupportedExpression,
        ]
    );
    configure_checkpoint_abort(None);
    configure_binary_rhs_policy(false);
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, add_unit), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert_eq!(checkpoint_count(), 2);
    configure_checkpoint_abort(Some(3));
    configure_binary_rhs_policy(true);
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, add_unit), &mut env),
        ComptimeOutcome::Abort(FakeFailure::Canceled)
    ));
    assert_eq!(checkpoint_count(), 3);
    configure_checkpoint_abort(None);
    configure_binary_rhs_policy(true);
}

#[test]
fn semantic_rejection_sites_preserve_program_identity_for_colliding_spans() {
    let make_program = || {
        let mut editor = RirEditor::new();
        let unit = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(40, 41),
        });
        let rejected = editor.add_inst(Inst {
            data: InstData::Neg { operand: unit },
            span: Span::new(40, 41),
        });
        (editor.finish(), rejected)
    };
    let (first, first_root) = make_program();
    let (second, second_root) = make_program();
    let mut host = FakeHost {
        programs: vec![first, second],
        type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    REJECTION_SITES.with(|sites| sites.borrow_mut().clear());

    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, first_root), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(1, second_root), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert_eq!(
        REJECTION_SITES.with(|sites| sites.borrow().clone()),
        vec![(0, 40, 41), (1, 40, 41)]
    );
}

#[test]
fn non_tail_assignment_restores_locals_before_rejection_and_reuse() {
    let mut editor = RirEditor::new();
    let unit = editor.add_inst(Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 1),
    });
    let assignment = editor.add_inst(Inst {
        data: InstData::Assign {
            name: lasso::Spur::default(),
            value: unit,
        },
        span: Span::new(1, 2),
    });
    let allocation = editor
        .add_alloc(
            &[],
            Some(lasso::Spur::default()),
            false,
            None,
            unit,
            false,
            Span::new(0, 1),
        )
        .unwrap();
    let non_tail = editor
        .add_block(&[allocation, assignment, unit], Span::new(1, 3))
        .unwrap();
    let var = editor.add_inst(Inst {
        data: InstData::VarRef {
            name: lasso::Spur::default(),
            anchor: None,
        },
        span: Span::new(3, 4),
    });
    let program = editor.finish();
    let mut host = FakeHost {
        programs: vec![program],
        type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, non_tail), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(!env.locals.contains_key(&FakeName { ordinal: 0 }));
    NAMED_TYPE_MISSING.with(|missing| missing.set(true));
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, var), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    NAMED_TYPE_MISSING.with(|missing| missing.set(false));
}

#[test]
fn unary_aggregate_and_unknown_type_intrinsic_use_real_rejection_dispatch() {
    let mut editor = RirEditor::new();
    let unit = editor.add_inst(Inst {
        data: InstData::UnitConst,
        span: Span::new(20, 21),
    });
    let neg = editor.add_inst(Inst {
        data: InstData::Neg { operand: unit },
        span: Span::new(20, 21),
    });
    let bitnot = editor.add_inst(Inst {
        data: InstData::BitNot { operand: unit },
        span: Span::new(20, 21),
    });
    let typed = editor.add_inst(Inst {
        data: InstData::VarRef {
            name: lasso::Spur::default(),
            anchor: None,
        },
        span: Span::new(20, 21),
    });
    let typed_neg = editor.add_inst(Inst {
        data: InstData::Neg { operand: typed },
        span: Span::new(20, 21),
    });
    let typed_bitnot = editor.add_inst(Inst {
        data: InstData::BitNot { operand: typed },
        span: Span::new(20, 21),
    });
    let aggregate = editor
        .add_struct_init(
            None,
            None,
            lasso::Spur::default(),
            &[],
            None,
            Span::new(20, 21),
        )
        .unwrap();
    let type_arg = editor.add_unit_type().unwrap();
    let unknown_type_intrinsic = editor.add_inst(Inst {
        data: InstData::TypeIntrinsic {
            name: lasso::Spur::default(),
            type_arg,
        },
        span: Span::new(20, 21),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    env.locals.insert(
        FakeName { ordinal: 0 },
        FakeValue::TypedInteger(1, FakeType(99)),
    );
    REJECTION_EVENTS.with(|events| events.borrow_mut().clear());
    let mut engine = ComptimeEngine::new(&mut host);
    for root in [
        neg,
        bitnot,
        typed_neg,
        typed_bitnot,
        aggregate,
        unknown_type_intrinsic,
    ] {
        assert!(matches!(
            engine.evaluate(ComptimeFrame::expression(0, root), &mut env),
            ComptimeOutcome::RuntimeDependent
        ));
    }
    assert_eq!(
        REJECTION_EVENTS.with(|events| events.borrow().clone()),
        vec![
            ComptimeSemanticRejection::UnaryOperandNotInteger(FakeValue::Unit),
            ComptimeSemanticRejection::UnaryOperandNotInteger(FakeValue::Unit),
            ComptimeSemanticRejection::UnaryTypeNotInteger {
                operation: ComptimeUnaryOperation::Neg,
                value: FakeValue::TypedInteger(1, FakeType(99)),
            },
            ComptimeSemanticRejection::UnaryTypeNotInteger {
                operation: ComptimeUnaryOperation::BitNot,
                value: FakeValue::TypedInteger(1, FakeType(99)),
            },
            ComptimeSemanticRejection::AggregateExpression,
            ComptimeSemanticRejection::UnsupportedIntrinsic("type".to_owned()),
        ]
    );
}

#[derive(Clone, Debug, PartialEq)]
enum FakeValue {
    Integer(i128),
    TypedInteger(i128, FakeType),
    Boolean(bool),
    Unit,
    Type(FakeType),
}

#[derive(Clone, Debug, PartialEq, Copy)]
struct FakeType(u8);

impl ComptimeType for FakeType {}

impl ComptimeValue for FakeValue {
    type Type = FakeType;
    fn integer(value: i128) -> Self {
        Self::Integer(value)
    }
    fn boolean(value: bool) -> Self {
        Self::Boolean(value)
    }
    fn unit() -> Self {
        Self::Unit
    }
    fn type_value(_value: FakeType) -> Self {
        Self::Type(_value)
    }
    fn as_integer(&self) -> Option<i128> {
        match self {
            Self::Integer(value) | Self::TypedInteger(value, _) => Some(*value),
            _ => None,
        }
    }

    fn as_integer_type(&self) -> Option<FakeType> {
        match self {
            Self::TypedInteger(_, ty) => Some(*ty),
            _ => None,
        }
    }
    fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            _ => None,
        }
    }
    fn as_type(&self) -> Option<FakeType> {
        match self {
            Self::Type(value) => Some(*value),
            _ => None,
        }
    }

    fn integer_typed(value: i128, ty: Option<FakeType>) -> Self {
        ty.map_or(Self::Integer(value), |ty| Self::TypedInteger(value, ty))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FakeName {
    ordinal: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FakeFile {
    index: u32,
}

impl ComptimeName for FakeName {}
impl ComptimeFile for FakeFile {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FakeIdentity {
    token: u32,
}

impl ComptimeIdentity for FakeIdentity {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FakeFailure {
    Generic,
    Canceled,
    DepthExceeded,
    NonFunctionMethod,
    OwnComptimeTypeParameter,
}

// Keep the existing compact fixture construction readable while allowing
// the decoder regressions to assert their exact semantic failure reason.
const FAKE_FAILURE: FakeFailure = FakeFailure::Generic;

enum FakePreparedCall {
    Enter {
        program: usize,
        body: InstRef,
        expected: Option<FakeType>,
        name_bindings: AHashMap<FakeName, FakeName>,
    },
    UnnamedEnter {
        program: usize,
        body: InstRef,
    },
    Memoized(ComptimeOutcome<FakeValue, FakeFailure>),
}

#[derive(Clone)]
enum FakeFinishOutcome {
    Identity,
    Structured(Vec<FakeStructuredPreparation>),
    RuntimeDependent,
    NotReady,
    UnsupportedContext,
    Trap,
    HostFailure,
    Abort,
    AbortFromPrepare,
    AbortFromArithmetic,
    CanonicalFailure,
}

#[derive(Clone, Copy, Debug)]
enum FakeExpressionIntrinsicOutcome {
    Known,
    RuntimeDependent,
    NotReady,
    UnsupportedContext,
    Trap,
    HostFailure,
    Abort,
}

#[derive(Clone, Copy)]
enum FakeStructuredPreparation {
    Enter,
    Memoized,
    RuntimeDependent,
    NotReady,
    UnsupportedContext,
    Trap,
    HostFailure,
    Abort,
}

struct FakeStructuredSuspension {
    preparations: Vec<FakeStructuredPreparation>,
    index: usize,
}

struct FakeCallBinding {
    arguments: Vec<(FakeValue, bool)>,
}

struct FakeBoundCall {
    arguments: Vec<(FakeValue, bool)>,
}

impl super::structured_type::structured_type_seal::Sealed for FakeStructuredSuspension {}
impl ComptimeStructuredTypeSuspension for FakeStructuredSuspension {}

struct FakeHost {
    programs: Vec<Rir>,
    type_symbol: SymbolHandle,
    constant: Option<(FakeFile, FakeName, FakeConstInfo)>,
    dependencies: Vec<(FakeFile, FakeName)>,
    call_plans: AHashMap<u32, FakePreparedCall>,
    recursive: Option<(usize, InstRef, InstRef, Option<usize>)>,
    enter_count: usize,
    finish_outcome: FakeFinishOutcome,
    finished: Vec<(usize, Option<FakeType>)>,
    float_evaluations: Cell<usize>,
}

#[derive(Clone)]
struct FakeConstInfo {
    span: Span,
    value: Option<FakeValue>,
}

impl FakeHost {
    fn admits_durable_forms(&self) -> bool {
        matches!(self.finish_outcome, FakeFinishOutcome::Identity)
    }
}

fn configure_checkpoint_abort(abort_at: Option<usize>) {
    CHECKPOINTS.with(|count| count.set(0));
    ABORT_AT_CHECKPOINT.with(|configured| configured.set(abort_at));
}

fn configure_binary_rhs_policy(evaluate_rhs: bool) {
    EVALUATE_RHS_AFTER_REJECTION.with(|policy| policy.set(evaluate_rhs));
}

fn checkpoint_count() -> usize {
    CHECKPOINTS.with(Cell::get)
}

fn clear_call_argument_observations() {
    CALL_ARGUMENTS.with(|arguments| arguments.borrow_mut().clear());
    ALLOW_MODULE_CALLS.with(|allowed| allowed.set(false));
    REJECT_ADMISSION.with(|rejected| rejected.set(false));
    REJECT_BIND_AT.with(|rejected| rejected.set(None));
    BINDING_FINISHES.with(|count| count.set(0));
    PREPARE_CALLS.with(|count| count.set(0));
    EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.set(0));
    EVALUATED_METHOD_RECEIVERS.with(|receivers| receivers.borrow_mut().clear());
    EVALUATED_METHOD_EVENTS.with(|events| events.borrow_mut().clear());
    EVALUATED_METHOD_ARGUMENT_CALLS.with(|count| count.set(0));
    EVALUATED_METHOD_FAIL_ON_UNIT.with(|fail| fail.set(false));
}

fn clear_named_value_observations() {
    NAMED_VALUE_CALLS.with(|count| count.set(0));
    REJECT_VISIBILITY.with(|reject| reject.set(false));
    NAMED_TYPE_MISSING.with(|missing| missing.set(false));
}

/// RUE-1838: `ComptimeEnv::for_analysis` reports runtime locals through the
/// borrowed membership hook instead of snapshotting every in-scope local's
/// name on every per-expression probe. `is_runtime_local_name` is the only
/// reader of either spelling, so the two must be indistinguishable to it —
/// including for a name that is absent, where the hook must answer `false`
/// rather than fall through to a stale empty set.
#[test]
fn borrowed_local_membership_matches_a_snapshotted_name_set() {
    let present = FakeName { ordinal: 21 };
    let absent = FakeName { ordinal: 22 };

    let mut snapshotted =
        ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    snapshotted.runtime_local_names.insert(present.clone());

    let live: AHashSet<FakeName> = [present.clone()].into_iter().collect();
    let mut borrowed = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    borrowed.runtime_local_name_membership =
        Some(std::sync::Arc::new(move |name| live.contains(name)));

    assert!(snapshotted.is_runtime_local_name(&present));
    assert!(borrowed.is_runtime_local_name(&present));
    assert!(!snapshotted.is_runtime_local_name(&absent));
    assert!(!borrowed.is_runtime_local_name(&absent));

    // The hook-backed env carries no snapshot at all: that is the point.
    assert!(borrowed.runtime_local_names.is_empty());
}

#[test]
fn named_array_length_classifies_lexical_bindings_before_global_lookup() {
    let name = FakeName { ordinal: 7 };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();

    assert!(matches!(
        ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
        ComptimeArrayLengthBinding::Unbound
    ));
    env.value_subst.insert(name.clone(), FakeValue::Integer(4));
    assert!(matches!(
        ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
        ComptimeArrayLengthBinding::LocalValue(FakeValue::Integer(4))
    ));
    env.value_subst
        .insert(name.clone(), FakeValue::Type(FakeType(1)));
    assert!(matches!(
        ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
        ComptimeArrayLengthBinding::LocalValue(FakeValue::Type(FakeType(1)))
    ));
    env.value_subst.clear();
    env.runtime_local_names.insert(name.clone());
    assert!(matches!(
        ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
        ComptimeArrayLengthBinding::RuntimeDependent
    ));
    env.runtime_local_names.clear();
    env.locals.insert(name.clone(), FakeValue::Integer(9));
    assert!(matches!(
        ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
        ComptimeArrayLengthBinding::LocalValue(FakeValue::Integer(9))
    ));
    env.locals
        .insert(name.clone(), FakeValue::Type(FakeType(2)));
    assert!(matches!(
        ComptimeEngine::<FakeHost>::classify_array_length_binding(&env, &name),
        ComptimeArrayLengthBinding::LocalValue(FakeValue::Type(FakeType(2)))
    ));
}

#[test]
fn named_array_length_dispatch_preserves_shadow_and_abort_channels() {
    let interner = lasso::ThreadedRodeo::new();
    let count_symbol = interner.get_or_intern("N");
    let type_symbol = interner.get_or_intern("T");
    let mut editor = rue_rir::RirEditor::new();
    let type_syntax = editor.add_named_type(type_symbol).unwrap();
    let element = editor.add_inst(rue_rir::Inst {
        data: InstData::TypeConst {
            type_name: type_syntax,
        },
        span: Span::new(0, 1),
    });
    let array = editor.add_inst(rue_rir::Inst {
        data: InstData::ArrayRepeat {
            value: element,
            count: rue_rir::RepeatCount::Named(count_symbol),
        },
        span: Span::new(0, 2),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(type_symbol),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let name = FakeName {
        ordinal: count_symbol.into_usize() as u32,
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let eval =
        |host: &mut FakeHost,
         env: &mut ComptimeEnv<'_, FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>| {
            ComptimeEngine::new(host).evaluate(ComptimeFrame::expression(0, array), env)
        };

    ARRAY_LENGTH_CALLS.with(|calls| calls.set(0));
    ARRAY_LENGTH_INPUTS.with(|inputs| inputs.borrow_mut().clear());
    assert!(matches!(
        eval(&mut host, &mut env),
        ComptimeOutcome::Known(_)
    ));
    assert_eq!(ARRAY_LENGTH_CALLS.with(Cell::get), 1);
    assert_eq!(
        ARRAY_LENGTH_INPUTS.with(|inputs| inputs.borrow().clone()),
        vec![None]
    );

    env.value_subst.insert(name.clone(), FakeValue::Integer(4));
    assert!(matches!(
        eval(&mut host, &mut env),
        ComptimeOutcome::Known(_)
    ));
    assert_eq!(
        ARRAY_LENGTH_INPUTS.with(|inputs| inputs.borrow().last().copied()),
        Some(Some(4))
    );

    env.value_subst.clear();
    env.locals
        .insert(name.clone(), FakeValue::Type(FakeType(3)));
    assert!(matches!(
        eval(&mut host, &mut env),
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));

    env.locals.insert(name.clone(), FakeValue::Boolean(true));
    assert!(matches!(
        eval(&mut host, &mut env),
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));

    env.locals.clear();
    env.runtime_local_names.insert(name.clone());
    assert!(matches!(
        eval(&mut host, &mut env),
        ComptimeOutcome::RuntimeDependent
    ));

    env.runtime_local_names.clear();
    env.runtime_binding_names.insert(name.clone());
    assert!(matches!(
        eval(&mut host, &mut env),
        ComptimeOutcome::RuntimeDependent
    ));

    env.runtime_binding_names.clear();
    env.value_subst.insert(name.clone(), FakeValue::Integer(6));
    env.runtime_binding_names.insert(name.clone());
    assert!(matches!(
        eval(&mut host, &mut env),
        ComptimeOutcome::Known(_)
    ));
    assert_eq!(
        ARRAY_LENGTH_INPUTS.with(|inputs| inputs.borrow().last().copied()),
        Some(Some(6))
    );

    env.runtime_binding_names.clear();
    env.value_subst.clear();
    env.type_subst.insert(name.clone(), FakeType(4));
    assert!(matches!(
        eval(&mut host, &mut env),
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));

    env.type_subst.clear();
    ARRAY_LENGTH_ABORT.with(|abort| abort.set(true));
    assert!(matches!(
        eval(&mut host, &mut env),
        ComptimeOutcome::Abort(FAKE_FAILURE)
    ));
    ARRAY_LENGTH_ABORT.with(|abort| abort.set(false));
}

#[test]
fn local_capture_substitution_removes_the_shadowed_opposite_map() {
    let name = FakeName { ordinal: 11 };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    env.type_subst.insert(name.clone(), FakeType(1));
    env.value_subst.insert(name.clone(), FakeValue::Integer(2));

    env.locals
        .insert(name.clone(), FakeValue::Type(FakeType(3)));
    let (types, values) = env.substs_with_locals();
    assert_eq!(types.get(&name), Some(&FakeType(3)));
    assert!(!values.contains_key(&name));

    env.locals.insert(name.clone(), FakeValue::Integer(4));
    let (types, values) = env.substs_with_locals();
    assert!(!types.contains_key(&name));
    assert_eq!(values.get(&name), Some(&FakeValue::Integer(4)));
}

#[test]
fn anonymous_struct_and_enum_hooks_receive_disjoint_type_and_value_captures() {
    let mut struct_editor = rue_rir::RirEditor::new();
    let struct_root = struct_editor
        .add_anon_struct_type(
            &[],
            &[],
            rue_rir::RirStructuralAnchor::new(vec![rue_rir::RirStructuralPathSegment::Statement(
                1,
            )]),
            Span::new(0, 1),
        )
        .unwrap();
    let mut enum_editor = rue_rir::RirEditor::new();
    let enum_root = enum_editor
        .add_anon_enum_type(
            &[],
            &[],
            rue_rir::RirStructuralAnchor::new(vec![rue_rir::RirStructuralPathSegment::Statement(
                2,
            )]),
            Span::new(0, 1),
        )
        .unwrap();
    let mut host = FakeHost {
        programs: vec![struct_editor.finish(), enum_editor.finish()],
        type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let type_name = FakeName { ordinal: 4 };
    let value_name = FakeName { ordinal: 5 };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    env.canonical_identity = Some(FakeIdentity { token: 9 });
    env.type_subst.insert(type_name.clone(), FakeType(1));
    env.value_subst
        .insert(type_name.clone(), FakeValue::Integer(7));
    env.value_subst
        .insert(value_name.clone(), FakeValue::Integer(2));
    env.type_subst.insert(value_name.clone(), FakeType(8));
    env.locals
        .insert(type_name.clone(), FakeValue::Type(FakeType(3)));
    env.locals.insert(value_name.clone(), FakeValue::Integer(4));
    ANON_STRUCT_CAPTURES.with(|captures| captures.borrow_mut().clear());
    ANON_ENUM_CAPTURES.with(|captures| captures.borrow_mut().clear());

    assert!(matches!(
        ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, struct_root), &mut env,),
        ComptimeOutcome::Known(FakeValue::Type(FakeType(20)))
    ));
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(1, enum_root), &mut env,),
        ComptimeOutcome::Known(FakeValue::Type(FakeType(21)))
    ));
    ANON_STRUCT_CAPTURES.with(|captures| {
        assert_eq!(
            *captures.borrow(),
            vec![(vec![(4, FakeType(3))], vec![(5, FakeValue::Integer(4))])]
        );
    });
    ANON_ENUM_CAPTURES.with(|captures| {
        assert_eq!(
            *captures.borrow(),
            vec![(vec![(4, FakeType(3))], vec![(5, FakeValue::Integer(4))])]
        );
    });
}

fn clear_type_intrinsic_observations() {
    TYPE_INTRINSIC_EVENTS.with(|events| events.borrow_mut().clear());
    TYPE_INTRINSIC_FAILURE.with(|failure| failure.set(false));
    TYPE_INTRINSIC_ABORT.with(|abort| abort.set(false));
    TYPE_INTRINSIC_NAME.with(|name| *name.borrow_mut() = None);
}

impl ComptimeDomain for FakeHost {
    type Type = FakeType;
    type Value = FakeValue;
    type Name = FakeName;
    type File = FakeFile;
    type CanonicalIdentity = FakeIdentity;
    type AnonymousIdentity = FakeIdentity;
    type ProgramKey = usize;
    type Failure = FakeFailure;
    type CallAdmission = ();
    type CallBinding = FakeCallBinding;
    type BoundCall = FakeBoundCall;
    type CompletionTicket = usize;
    type StructuredTypeSuspension = FakeStructuredSuspension;
}

impl ComptimeInterrupts for FakeHost {
    fn check_canceled(&self) -> ComptimeHostResult<(), Self::Failure> {
        let checkpoint = CHECKPOINTS.with(|count| {
            let next = count.get() + 1;
            count.set(next);
            next
        });
        if ABORT_AT_CHECKPOINT.with(|abort_at| abort_at.get() == Some(checkpoint)) {
            return Err(ComptimeHostError::Abort(FakeFailure::Canceled));
        }
        Ok(())
    }
}

impl ComptimeProgramFacts for FakeHost {
    fn program_rir(&self, program: &Self::ProgramKey) -> &Rir {
        &self.programs[*program]
    }
    fn name_from_symbol(&self, program: &Self::ProgramKey, symbol: SymbolHandle) -> Self::Name {
        if MATCH_PATTERN_MATCHES.with(Cell::get) {
            MATCH_SYMBOL_CALLS.with(|calls| calls.set(calls.get() + 1));
        }
        FakeName {
            ordinal: symbol.issuing_interner_ordinal() as u32 + (*program as u32) * 1000,
        }
    }
    fn display_name(&self, name: &Self::Name) -> String {
        if let Some((_, intrinsic)) = EXPRESSION_INTRINSIC_NAMES.with(|names| {
            names
                .borrow()
                .iter()
                .find(|(ordinal, _)| *ordinal == name.ordinal)
                .copied()
        }) {
            return intrinsic.to_owned();
        }
        if let Some((_, intrinsic)) = TYPE_INTRINSIC_NAME.with(|configured| {
            configured
                .borrow()
                .as_ref()
                .copied()
                .filter(|(ordinal, _)| *ordinal == name.ordinal)
        }) {
            return intrinsic.to_owned();
        }
        if name.ordinal == self.type_symbol.issuing_interner_ordinal() as u32 {
            "type".to_owned()
        } else if name.ordinal % 1000 == 0 {
            "import".to_owned()
        } else {
            format!("fake-name-{}", name.ordinal)
        }
    }
    fn file_for_program_span(&self, program: &Self::ProgramKey, span: &Span) -> Self::File {
        if KEYED_FILE_RESOLUTION.with(Cell::get) {
            let file = span.file_id.index() + (*program as u32) * 100;
            FILE_RESOLUTION_CALLS.with(|calls| {
                calls.borrow_mut().push((*program, file));
            });
            return FakeFile { index: file };
        }
        FakeFile {
            index: span.file_id.index(),
        }
    }
}

impl ComptimeTypeAlgebra for FakeHost {
    fn unsupported_anon_method_type_param(
        &self,
        _method_name: &str,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        METHOD_FAILURES.with(|failures| failures.borrow_mut().push("own_type"));
        FakeFailure::OwnComptimeTypeParameter
    }
    fn non_function_anon_method(
        &self,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        METHOD_FAILURES.with(|failures| failures.borrow_mut().push("non_function"));
        FakeFailure::NonFunctionMethod
    }
    fn resolve_named_array_length(
        &mut self,
        _name: &Self::Name,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        values: Option<&AHashMap<Self::Name, Self::Value>>,
        binding: ComptimeArrayLengthBinding<Self::Value>,
    ) -> ComptimeOutcome<u64, Self::Failure> {
        ARRAY_LENGTH_CALLS.with(|calls| calls.set(calls.get() + 1));
        ARRAY_LENGTH_INPUTS.with(|inputs| {
            inputs.borrow_mut().push(
                values
                    .and_then(|values| values.values().next().and_then(ComptimeValue::as_integer)),
            )
        });
        let shadowed = match &binding {
            ComptimeArrayLengthBinding::Shadowed => true,
            ComptimeArrayLengthBinding::LocalValue(value) => value.as_integer().is_none(),
            _ => false,
        };
        if shadowed {
            return ComptimeOutcome::HostFailure(FAKE_FAILURE);
        }
        if ARRAY_LENGTH_ABORT.with(Cell::get) {
            return ComptimeOutcome::Abort(FAKE_FAILURE);
        }
        if matches!(binding, ComptimeArrayLengthBinding::RuntimeDependent) {
            return ComptimeOutcome::RuntimeDependent;
        }
        ComptimeOutcome::Known(0)
    }
    fn rir_type_named_symbol(
        &self,
        _program: &Self::ProgramKey,
        _syntax: rue_rir::RirTypeSyntaxRef,
    ) -> Option<Self::Name> {
        if matches!(self.finish_outcome, FakeFinishOutcome::Structured(_)) {
            None
        } else {
            Some(self.name_from_symbol(&0, self.type_symbol))
        }
    }
    fn render_rir_type(
        &self,
        _program: &Self::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
    ) -> String {
        format!("{syntax:?}")
    }
    fn get_or_create_array_type(&mut self, _element: Self::Type, _length: u64) -> Self::Type {
        FakeType(8)
    }
    fn find_or_create_anon_struct(
        &mut self,
        _identity: Self::AnonymousIdentity,
        _fields: &[ComptimeField<Self::Name, Self::Type>],
        _sigs: &[ComptimeMethodDescriptor<Self::Name, Self::Type>],
        type_subst: &AHashMap<Self::Name, Self::Type>,
        value_subst: &AHashMap<Self::Name, Self::Value>,
    ) -> ComptimeHostResult<(Self::Type, bool), Self::Failure> {
        ANON_STRUCT_CAPTURES.with(|captures| {
            captures.borrow_mut().push((
                type_subst
                    .iter()
                    .map(|(name, ty)| (name.ordinal, *ty))
                    .collect(),
                value_subst
                    .iter()
                    .map(|(name, value)| (name.ordinal, value.clone()))
                    .collect(),
            ));
        });
        Ok((FakeType(20), true))
    }
    fn find_or_create_anon_enum(
        &mut self,
        _identity: Self::AnonymousIdentity,
        _names: &[String],
        _payloads: &[Vec<Self::Type>],
        type_subst: &AHashMap<Self::Name, Self::Type>,
        value_subst: &AHashMap<Self::Name, Self::Value>,
    ) -> ComptimeHostResult<Self::Type, Self::Failure> {
        ANON_ENUM_CAPTURES.with(|captures| {
            captures.borrow_mut().push((
                type_subst
                    .iter()
                    .map(|(name, ty)| (name.ordinal, *ty))
                    .collect(),
                value_subst
                    .iter()
                    .map(|(name, value)| (name.ordinal, value.clone()))
                    .collect(),
            ));
        });
        Ok(FakeType(21))
    }
    fn check_require_droppable(
        &mut self,
        _ty: Self::Type,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure> {
        Ok(())
    }
    fn check_trivially_droppable(
        &mut self,
        _ty: Self::Type,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure> {
        Ok(())
    }
    fn type_name(&self, ty: &Self::Type) -> String {
        format!("fake-type-{}", ty.0)
    }
    fn type_is_unsigned(&self, _ty: &Self::Type) -> bool {
        false
    }
    fn type_integer_semantics(&self, ty: &Self::Type) -> Option<IntegerType> {
        (ty.0 != 99).then(|| IntegerType::new(8, true)).flatten()
    }
    fn resolve_comptime_type_intrinsic(
        &mut self,
        intrinsic: ComptimeTypeIntrinsic,
        ty: Self::Type,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        TYPE_INTRINSIC_EVENTS.with(|events| events.borrow_mut().push((intrinsic, ty)));
        if TYPE_INTRINSIC_ABORT.with(Cell::get) {
            return Err(ComptimeHostError::Abort(FAKE_FAILURE));
        }
        if TYPE_INTRINSIC_FAILURE.with(Cell::get) {
            return Err(ComptimeHostError::HostFailure(FAKE_FAILURE));
        }
        Ok(Some(match intrinsic {
            ComptimeTypeIntrinsic::IntegerBound(ComptimeIntegerBound::Max) => {
                FakeValue::integer_typed(127, Some(ty))
            }
            ComptimeTypeIntrinsic::IntegerBound(ComptimeIntegerBound::Min) => {
                FakeValue::integer_typed(-128, Some(ty))
            }
            ComptimeTypeIntrinsic::RequireDroppable
            | ComptimeTypeIntrinsic::RequireTriviallyDroppable => FakeValue::Unit,
        }))
    }
    fn const_expr_type(
        &self,
        _program: &Self::ProgramKey,
        _env: &ComptimeEnv<'_, Self::Value, Self::Type, Self::Name, Self::File, FakeIdentity>,
        inst_ref: InstRef,
    ) -> Option<Self::Type> {
        (inst_ref.as_u32() == 2).then_some(FakeType(8))
    }
    fn integer_operation_type(
        &self,
        resolved_type: Option<&Self::Type>,
        lhs: &Self::Value,
        rhs: &Self::Value,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        INTEGER_HINTS.with(|hints| hints.borrow_mut().push(resolved_type.copied()));
        if let (Some(lhs), Some(rhs)) = (lhs.as_integer_type(), rhs.as_integer_type()) {
            if lhs != rhs {
                return Err(FAKE_FAILURE.into());
            }
        }
        Ok(resolved_type
            .cloned()
            .or_else(|| lhs.as_integer_type())
            .or_else(|| rhs.as_integer_type()))
    }
    fn resolve_named_type_value(
        &mut self,
        program: &Self::ProgramKey,
        _name: Self::Name,
        _span: Span,
    ) -> ComptimeHostResult<Option<Self::Type>, Self::Failure> {
        TYPE_VALUE_PROGRAMS.with(|programs| programs.borrow_mut().push(*program));
        Ok((!NAMED_TYPE_MISSING.with(Cell::get)).then_some(FakeType(7)))
    }
    fn resolve_comptime_type_path(
        &mut self,
        _file: Self::File,
        _segments: &[Self::Name],
        _span: Span,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        Ok(None)
    }
    fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        _program: &Self::ProgramKey,
        _syntax: rue_rir::RirTypeSyntaxRef,
        _types: &AHashMap<Self::Name, Self::Type>,
        _values: &AHashMap<Self::Name, Self::Value>,
        _span: Span,
    ) -> Option<Self::Type> {
        TYPE_RESOLUTION_CALLS.with(|calls| calls.set(calls.get() + 1));
        TYPE_INTRINSIC_NAME
            .with(|configured| configured.borrow().is_some())
            .then_some(FakeType(7))
    }
}

impl ComptimeValueAlgebra for FakeHost {
    fn resolve_comptime_named_value(
        &mut self,
        file: Self::File,
        name: Self::Name,
        span: Span,
    ) -> ComptimeHostResult<ComptimeNamedValueResolution<Self::Value>, Self::Failure> {
        NAMED_VALUE_CALLS.with(|count| count.set(count.get() + 1));
        if EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.get() != 0) {
            EVALUATED_METHOD_EVENTS.with(|events| events.borrow_mut().push("receiver_eval"));
        }
        let info = self
            .constant
            .as_ref()
            .filter(|(constant_file, constant_name, _)| {
                *constant_file == file && *constant_name == name
            })
            .map(|(_, _, info)| info.clone());
        if let Some(info) = info {
            let defining_file = FakeFile {
                index: info.span.file_id.index(),
            };
            self.dependencies
                .push((defining_file.clone(), name.clone()));
            if REJECT_VISIBILITY.with(Cell::get) {
                return Err(FAKE_FAILURE.into());
            }
            return Ok(match info.value {
                Some(value) => ComptimeNamedValueResolution::Known(value),
                None => ComptimeNamedValueResolution::RuntimeDependent,
            });
        }
        let resolved = self.resolve_named_type_value(&0, name, span)?;
        Ok(match resolved {
            Some(ty) => ComptimeNamedValueResolution::Known(FakeValue::Type(ty)),
            None => ComptimeNamedValueResolution::Missing,
        })
    }
    fn match_pattern(
        &self,
        pattern: &ComptimeMatchPattern<Self::Name>,
        _value: &Self::Value,
    ) -> Option<bool> {
        if !MATCH_PATTERN_MATCHES.with(Cell::get) {
            return None;
        }
        MATCH_PATTERN_EVENTS.with(|events| events.borrow_mut().push(pattern.clone()));
        Some(!MATCH_PATTERN_FORCE_FALSE.with(Cell::get))
    }
    fn match_no_selected_arm(
        &self,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        MATCH_NO_SELECTED_SITES.with(|sites| {
            sites
                .borrow_mut()
                .push((*site.program(), site.span().start, site.span().end));
        });
        if MATCH_NO_SELECTED_FAILURE.with(Cell::get) {
            ComptimeOutcome::HostFailure(FAKE_FAILURE)
        } else {
            ComptimeOutcome::RuntimeDependent
        }
    }
    fn evaluate_binary_rhs_after_rejection(&self) -> bool {
        EVALUATE_RHS_AFTER_REJECTION.with(Cell::get)
    }

    fn compare_comptime_values(
        &mut self,
        lhs: &Self::Value,
        rhs: &Self::Value,
        equal: bool,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        let (Some(lhs), Some(rhs)) = (lhs.as_type(), rhs.as_type()) else {
            return ComptimeOutcome::RuntimeDependent;
        };
        ComptimeOutcome::Known(FakeValue::boolean(if equal {
            lhs == rhs
        } else {
            lhs != rhs
        }))
    }
    fn finish_arith(
        &self,
        result: CheckedIntegerResult,
        _ty: Option<Self::Type>,
        op: &str,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<Option<Self::Value>, Self::Failure> {
        FINISH_ARITH_OPERATIONS.with(|operations| operations.borrow_mut().push(op.to_owned()));
        DIAGNOSTIC_SITES.with(|sites| {
            sites
                .borrow_mut()
                .push((*site.program(), site.span().start, site.span().end))
        });
        if matches!(self.finish_outcome, FakeFinishOutcome::AbortFromArithmetic) {
            return Err(ComptimeHostError::Abort(FAKE_FAILURE));
        }
        Ok(result.checked().map(FakeValue::integer))
    }
    fn resolve_string_const(
        &mut self,
        content: Self::Name,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        self.dependencies
            .push((FakeFile { index: u32::MAX }, content));
        ComptimeOutcome::Known(FakeValue::Integer(17))
    }

    fn resolve_comptime_expression_intrinsic(
        &mut self,
        request: ComptimeExpressionIntrinsicRequest<Self::Name>,
        site: &ComptimeSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        if let ComptimeExpressionIntrinsicRequest::Import {
            sole_string_literal: Some(_),
            ..
        } = &request
        {
            self.dependencies.push((
                FakeFile {
                    index: 0xFFFF_FFFE - (*site.program() as u32),
                },
                FakeName {
                    ordinal: site.occurrence(),
                },
            ));
        }
        EXPRESSION_INTRINSIC_REQUESTS.with(|requests| requests.borrow_mut().push(request));
        match EXPRESSION_INTRINSIC_OUTCOME.with(Cell::get) {
            FakeExpressionIntrinsicOutcome::Known => ComptimeOutcome::Known(FakeValue::Integer(99)),
            FakeExpressionIntrinsicOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            FakeExpressionIntrinsicOutcome::NotReady => ComptimeOutcome::NotReady,
            FakeExpressionIntrinsicOutcome::UnsupportedContext => {
                ComptimeOutcome::UnsupportedContext
            }
            FakeExpressionIntrinsicOutcome::Trap => ComptimeOutcome::Trap(ComptimeTrap {
                operation: "fake intrinsic trap",
                span: site.span(),
            }),
            FakeExpressionIntrinsicOutcome::HostFailure => {
                ComptimeOutcome::HostFailure(FAKE_FAILURE)
            }
            FakeExpressionIntrinsicOutcome::Abort => ComptimeOutcome::Abort(FAKE_FAILURE),
        }
    }

    fn resolve_comptime_enum_variant(
        &mut self,
        module: Option<Self::Value>,
        type_name: Self::Name,
        variant: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        self.dependencies.push((
            FakeFile {
                index: module.map_or(0xFFFF_FFFD, |_| 0xFFFF_FFFC),
            },
            FakeName {
                ordinal: type_name.ordinal ^ variant.ordinal,
            },
        ));
        ComptimeOutcome::Known(FakeValue::Integer(23))
    }

    fn admit_comptime_enum_variant(
        &mut self,
        _type_name: Self::Name,
        _variant: Self::Name,
        has_module: bool,
        _site: &ComptimeSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<bool, Self::Failure> {
        if has_module && REJECT_QUALIFIED_ENUM.with(Cell::get) {
            return Err(ComptimeHostError::HostFailure(FAKE_FAILURE));
        }
        Ok(self.admits_durable_forms())
    }

    fn admit_comptime_member(
        &mut self,
        _field: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<bool, Self::Failure> {
        Ok(self.admits_durable_forms())
    }

    fn resolve_comptime_member(
        &mut self,
        _base: Self::Value,
        _field: Self::Name,
        _site: &ComptimeSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        ComptimeOutcome::Known(FakeValue::Integer(31))
    }
}

impl ComptimeCallProtocol for FakeHost {
    fn resolve_module_comptime_callable(
        &mut self,
        _file_id: Self::File,
        _segments: &[Self::Name],
        method: Self::Name,
        _span: Span,
    ) -> ComptimeHostResult<Option<Self::Name>, Self::Failure> {
        Ok(ALLOW_MODULE_CALLS
            .with(|allowed| allowed.get())
            .then_some(method))
    }
    fn comptime_method_receiver_policy(&self) -> ComptimeMethodReceiverPolicy {
        EVALUATED_METHOD_RECEIVER_MODE.with(|mode| {
            if mode.get() == 0 {
                ComptimeMethodReceiverPolicy::SyntacticModulePath
            } else {
                ComptimeMethodReceiverPolicy::EvaluateReceiver
            }
        })
    }
    fn admit_evaluated_comptime_method(
        &mut self,
        receiver: Self::Value,
        method: Self::Name,
        _arg_count: usize,
        _arg_modes: &[ComptimeArgMode],
        _env: &mut ComptimeEnv<'_, Self::Value, Self::Type, Self::Name, Self::File, FakeIdentity>,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
        _span: Span,
    ) -> ComptimeOutcome<
        Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
        Self::Failure,
    > {
        EVALUATED_METHOD_EVENTS.with(|events| events.borrow_mut().push("receiver_hook"));
        EVALUATED_METHOD_RECEIVERS.with(|receivers| receivers.borrow_mut().push(receiver.clone()));
        let mode = EVALUATED_METHOD_RECEIVER_MODE.with(Cell::get);
        if EVALUATED_METHOD_FAIL_ON_UNIT.with(Cell::get) && receiver == FakeValue::Unit {
            return ComptimeOutcome::HostFailure(FAKE_FAILURE);
        }
        match mode {
            1 => ComptimeOutcome::Known(Some(ComptimeCallAdmission {
                name: FakeName {
                    ordinal: receiver
                        .as_type()
                        .map_or(method.ordinal, |ty| method.ordinal + ty.0 as u32),
                },
                payload: (),
            })),
            2 => ComptimeOutcome::Known(None),
            3 => ComptimeOutcome::RuntimeDependent,
            4 => ComptimeOutcome::NotReady,
            5 => ComptimeOutcome::UnsupportedContext,
            6 => ComptimeOutcome::Trap(ComptimeTrap {
                operation: "receiver trap",
                span: Span::new(0, 0),
            }),
            7 => ComptimeOutcome::HostFailure(FAKE_FAILURE),
            _ => ComptimeOutcome::Abort(FAKE_FAILURE),
        }
    }
    fn admit_comptime_call(
        &mut self,
        name: Self::Name,
        _arg_count: usize,
        _arg_modes: &[ComptimeArgMode],
        _env: &mut ComptimeEnv<'_, Self::Value, Self::Type, Self::Name, Self::File, FakeIdentity>,
        _name_is_resolved_key: bool,
    ) -> ComptimeHostResult<
        Option<ComptimeCallAdmission<Self::CallAdmission, Self::Name>>,
        Self::Failure,
    > {
        if REJECT_ADMISSION.with(|rejected| rejected.get()) {
            return Ok(None);
        }
        Ok(Some(ComptimeCallAdmission { name, payload: () }))
    }
    fn begin_comptime_call_binding(
        &self,
        _admission: &ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        _argument_count: usize,
        _span: Span,
    ) -> ComptimeHostResult<Self::CallBinding, Self::Failure> {
        Ok(FakeCallBinding {
            arguments: Vec::new(),
        })
    }
    fn bind_comptime_call_argument(
        &self,
        binding: &mut Self::CallBinding,
        argument: ComptimeCallArgument<Self::Value>,
        index: usize,
        _span: Span,
    ) -> ComptimeHostResult<bool, Self::Failure> {
        if REJECT_BIND_AT.with(|rejected| rejected.get() == Some(index)) {
            return Ok(false);
        }
        if EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.get() != 0) {
            EVALUATED_METHOD_EVENTS.with(|events| events.borrow_mut().push("argument"));
            EVALUATED_METHOD_ARGUMENT_CALLS.with(|count| count.set(count.get() + 1));
        }
        binding
            .arguments
            .push((argument.value().clone(), argument.is_direct_unit_literal()));
        Ok(true)
    }
    fn finish_comptime_call_binding(
        &mut self,
        _binding: Self::CallBinding,
        _span: Span,
    ) -> ComptimeHostResult<Option<Self::BoundCall>, Self::Failure> {
        BINDING_FINISHES.with(|count| count.set(count.get() + 1));
        let arguments = _binding.arguments;
        CALL_ARGUMENTS.with(|observed| observed.borrow_mut().extend(arguments.iter().cloned()));
        Ok(Some(FakeBoundCall { arguments }))
    }
    fn prepare_comptime_call(
        &mut self,
        admission: ComptimeCallAdmission<Self::CallAdmission, Self::Name>,
        bound: Self::BoundCall,
        _span: Span,
    ) -> ComptimeHostResult<
        Option<
            ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
                Self::CompletionTicket,
            >,
        >,
        Self::Failure,
    > {
        PREPARE_CALLS.with(|count| count.set(count.get() + 1));
        let _bound_argument_count = bound.arguments.len();
        if matches!(self.finish_outcome, FakeFinishOutcome::AbortFromPrepare) {
            return Err(ComptimeHostError::Abort(FAKE_FAILURE));
        }
        if let Some((max_enters, call_body, terminal_body, memoized_at)) = self.recursive {
            if memoized_at == Some(self.enter_count) {
                return Ok(Some(ComptimeCallPreparation::Memoized(
                    ComptimeOutcome::Known(FakeValue::Integer(1)),
                )));
            }
            let expected = Some(FakeType(7 + self.enter_count as u8));
            self.enter_count += 1;
            let body = if self.enter_count == max_enters {
                terminal_body
            } else {
                call_body
            };
            let frame = ComptimeFrame {
                program: 1,
                body,
                name: Some(admission.name.clone()),
                context: Some(FakeFile { index: 0 }),
                span: Span::new(0, 0),
                function_span: Span::new(0, 0),
                type_bindings: AHashMap::new(),
                value_bindings: AHashMap::new(),
                name_bindings: AHashMap::new(),
                call_identity: None,
                expected_result: expected,
            };
            if PREPARE_CANONICAL_PROBE.with(Cell::get) {
                // Model the ordinary host's pre-lookup identity probe. A
                // failure here must remain deferrable until `run_frame`
                // has performed its depth admission check.
                let _ = self.canonical_function_producer(
                    &frame.program,
                    &self.enter_count,
                    admission.name,
                    &frame.type_bindings,
                    &frame.value_bindings,
                    frame.span,
                );
            }
            return Ok(Some(ComptimeCallPreparation::Enter {
                frame,
                ticket: self.enter_count,
            }));
        }
        let Some(plan) = self.call_plans.remove(&admission.name.ordinal) else {
            return Ok(None);
        };
        Ok(Some(match plan {
            FakePreparedCall::Enter {
                program,
                body,
                expected,
                name_bindings,
            } => ComptimeCallPreparation::Enter {
                frame: ComptimeFrame {
                    program,
                    body,
                    name: Some(admission.name),
                    context: Some(FakeFile {
                        index: program as u32,
                    }),
                    span: Span::new(0, 0),
                    function_span: Span::new(0, 0),
                    type_bindings: AHashMap::new(),
                    value_bindings: AHashMap::new(),
                    name_bindings,
                    call_identity: None,
                    expected_result: expected,
                },
                ticket: program,
            },
            FakePreparedCall::UnnamedEnter { program, body } => ComptimeCallPreparation::Enter {
                frame: ComptimeFrame {
                    program,
                    body,
                    name: None,
                    context: Some(FakeFile {
                        index: program as u32,
                    }),
                    span: Span::new(0, 0),
                    function_span: Span::new(0, 0),
                    type_bindings: AHashMap::new(),
                    value_bindings: AHashMap::new(),
                    name_bindings: AHashMap::new(),
                    call_identity: None,
                    expected_result: None,
                },
                ticket: 777,
            },
            FakePreparedCall::Memoized(outcome) => ComptimeCallPreparation::Memoized(outcome),
        }))
    }
    fn finish_comptime_call(
        &mut self,
        frame: &ComptimeFrame<
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::ProgramKey,
            Self::CanonicalIdentity,
        >,
        ticket: Self::CompletionTicket,
        result: ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        self.finished.push((frame.program, frame.expected_result));
        TICKET_EVENTS.with(|events| {
            events.borrow_mut().push((ticket, false));
        });
        match self.finish_outcome {
            FakeFinishOutcome::Identity
            | FakeFinishOutcome::AbortFromPrepare
            | FakeFinishOutcome::AbortFromArithmetic
            | FakeFinishOutcome::CanonicalFailure => result,
            FakeFinishOutcome::Structured(_) => result,
            FakeFinishOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            FakeFinishOutcome::NotReady => ComptimeOutcome::NotReady,
            FakeFinishOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            FakeFinishOutcome::Trap => ComptimeOutcome::Trap(ComptimeTrap {
                operation: "fake trap",
                span: Span::new(0, 0),
            }),
            FakeFinishOutcome::HostFailure => ComptimeOutcome::HostFailure(FAKE_FAILURE),
            FakeFinishOutcome::Abort => ComptimeOutcome::Abort(FAKE_FAILURE),
        }
    }
    fn enter_comptime_call(
        &mut self,
        _frame: &ComptimeFrame<
            Self::Value,
            Self::Type,
            Self::Name,
            Self::File,
            Self::ProgramKey,
            Self::CanonicalIdentity,
        >,
        ticket: &Self::CompletionTicket,
    ) -> ComptimeHostResult<(), Self::Failure> {
        TICKET_EVENTS.with(|events| {
            events.borrow_mut().push((*ticket, true));
        });
        Ok(())
    }
    fn canonical_function_producer(
        &self,
        program: &Self::ProgramKey,
        ticket: &Self::CompletionTicket,
        name: Self::Name,
        _types: &AHashMap<Self::Name, Self::Type>,
        _values: &AHashMap<Self::Name, Self::Value>,
        _span: Span,
    ) -> ComptimeHostResult<Self::CanonicalIdentity, Self::Failure> {
        PRODUCER_CALLS.with(|calls| {
            calls.borrow_mut().push((*program, *ticket, name.ordinal));
        });
        if matches!(self.finish_outcome, FakeFinishOutcome::CanonicalFailure)
            || CANONICAL_FAILURE_AFTER
                .with(|after| after.get().is_some_and(|after| self.enter_count >= after))
        {
            return Err(FAKE_FAILURE.into());
        }
        Ok(FakeIdentity {
            token: name.ordinal,
        })
    }
    fn issue_anonymous_identity(
        &self,
        _program: &Self::ProgramKey,
        _kind: ComptimeAnonymousKind,
        producer: &Self::CanonicalIdentity,
        _anchor: &rue_rir::RirStructuralAnchor,
    ) -> Self::AnonymousIdentity {
        producer.clone()
    }
}

impl ComptimeStructuredTypes for FakeHost {
    fn begin_comptime_type_syntax(
        &mut self,
        _program: &Self::ProgramKey,
        _syntax: rue_rir::RirTypeSyntaxRef,
        _types: &AHashMap<Self::Name, Self::Type>,
        _values: &AHashMap<Self::Name, Self::Value>,
        _span: Span,
    ) -> ComptimeOutcome<
        ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
        Self::Failure,
    > {
        if TYPE_INTRINSIC_NAME.with(|configured| configured.borrow().is_some()) {
            return ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(FakeType(7)));
        }
        let FakeFinishOutcome::Structured(preparations) =
            std::mem::replace(&mut self.finish_outcome, FakeFinishOutcome::Identity)
        else {
            return ComptimeOutcome::RuntimeDependent;
        };
        ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Suspended(
            FakeStructuredSuspension {
                preparations,
                index: 0,
            },
        ))
    }

    fn prepare_structured_type_call(
        &mut self,
        suspension: &Self::StructuredTypeSuspension,
        span: Span,
    ) -> ComptimeOutcome<
        Option<
            ComptimeCallPreparation<
                Self::Value,
                Self::Type,
                Self::Name,
                Self::File,
                Self::ProgramKey,
                Self::CanonicalIdentity,
                Self::Failure,
                Self::CompletionTicket,
            >,
        >,
        Self::Failure,
    > {
        STRUCTURED_PREPARE_SPANS.with(|spans| spans.borrow_mut().push(span));
        match suspension.preparations[suspension.index] {
            FakeStructuredPreparation::Enter => {
                ComptimeOutcome::Known(Some(ComptimeCallPreparation::Enter {
                    frame: ComptimeFrame {
                        program: 1,
                        body: InstRef::from_raw(0),
                        name: Some(FakeName { ordinal: 1 }),
                        context: Some(FakeFile { index: 0 }),
                        span: Span::new(0, 0),
                        function_span: Span::new(0, 0),
                        type_bindings: AHashMap::new(),
                        value_bindings: AHashMap::new(),
                        name_bindings: AHashMap::new(),
                        call_identity: None,
                        expected_result: None,
                    },
                    ticket: 0,
                }))
            }
            FakeStructuredPreparation::Memoized => ComptimeOutcome::Known(Some(
                ComptimeCallPreparation::Memoized(ComptimeOutcome::Known(FakeValue::Integer(1))),
            )),
            FakeStructuredPreparation::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            FakeStructuredPreparation::NotReady => ComptimeOutcome::NotReady,
            FakeStructuredPreparation::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            FakeStructuredPreparation::Trap => ComptimeOutcome::Trap(ComptimeTrap {
                operation: "structured fake trap",
                span: Span::new(0, 0),
            }),
            FakeStructuredPreparation::HostFailure => ComptimeOutcome::HostFailure(FAKE_FAILURE),
            FakeStructuredPreparation::Abort => ComptimeOutcome::Abort(FAKE_FAILURE),
        }
    }

    fn resume_structured_type_call(
        &mut self,
        suspension: Self::StructuredTypeSuspension,
        result: ComptimeOutcome<Self::Value, Self::Failure>,
    ) -> ComptimeOutcome<
        ComptimeStructuredTypeResolution<Self::Type, Self::StructuredTypeSuspension>,
        Self::Failure,
    > {
        if !matches!(&result, ComptimeOutcome::Known(_)) {
            // The sentinel makes the outcome-funnel test observe that
            // every terminal reduction was handed back to the host.
            self.finished.push((usize::MAX, None));
        }
        match result {
            ComptimeOutcome::Known(_) if suspension.index + 1 < suspension.preparations.len() => {
                ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Suspended(
                    FakeStructuredSuspension {
                        preparations: suspension.preparations,
                        index: suspension.index + 1,
                    },
                ))
            }
            ComptimeOutcome::Known(_) => {
                ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(FakeType(9)))
            }
            ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
            ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
            ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
            ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
        }
    }
}

impl ComptimeRejections for FakeHost {
    fn reject_comptime_expression(
        &self,
        rejection: ComptimeSemanticRejection<Self::Value>,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        REJECTION_EVENTS.with(|events| events.borrow_mut().push(rejection));
        REJECTION_SITES.with(|sites| {
            sites
                .borrow_mut()
                .push((*site.program(), site.span().start, site.span().end));
        });
        ComptimeOutcome::RuntimeDependent
    }
    fn require_preview(
        &self,
        _feature: rue_error::PreviewFeature,
        _what: &str,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeHostResult<(), Self::Failure> {
        Ok(())
    }
    fn depth_exceeded(
        &self,
        _name: &Self::Name,
        _depth: usize,
        site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        DIAGNOSTIC_SITES.with(|sites| {
            sites
                .borrow_mut()
                .push((*site.program(), site.span().start, site.span().end))
        });
        if DEPTH_FAILURE_VARIANT.with(Cell::get) {
            FakeFailure::DepthExceeded
        } else {
            FAKE_FAILURE
        }
    }
    fn literal_out_of_range(
        &self,
        _value: u64,
        _ty: &Self::Type,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        FAKE_FAILURE
    }
    fn float_not_implemented(
        &self,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        self.float_evaluations.set(self.float_evaluations.get() + 1);
        FAKE_FAILURE
    }
    fn cannot_negate(
        &self,
        _ty: &Self::Type,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> Self::Failure {
        FAKE_FAILURE
    }
    fn label_ctor_instantiation_site(error: Self::Failure, _call_span: Span) -> Self::Failure {
        LABEL_CALLS.with(|calls| calls.set(calls.get() + 1));
        error
    }

    fn finish_checked(
        &mut self,
        value: Self::Value,
        _span: Span,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        self.dependencies
            .push((FakeFile { index: 0xFFFF_FFFB }, FakeName { ordinal: 0 }));
        ComptimeOutcome::Known(value)
    }

    fn reject_non_type_array_repeat(
        &mut self,
        _value: Self::Value,
        _site: &ComptimeDiagnosticSite<Self::ProgramKey>,
    ) -> ComptimeOutcome<Self::Value, Self::Failure> {
        self.dependencies
            .push((FakeFile { index: 0xFFFF_FFFA }, FakeName { ordinal: 0 }));
        ComptimeOutcome::RuntimeDependent
    }

    fn allow_checked_comptime(&self) -> bool {
        self.admits_durable_forms()
    }
}

impl ComptimeHost for FakeHost {}

#[test]
fn structured_type_engine_uses_one_existing_call_stack() {
    let mut editor = rue_rir::RirEditor::new();
    let root = editor.add_inst(rue_rir::Inst {
        data: InstData::TypeConst {
            type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
        },
        span: Span::new(0, 0),
    });
    let interner = lasso::ThreadedRodeo::new();
    let mut child_editor = rue_rir::RirEditor::new();
    child_editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish(), child_editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Structured(vec![FakeStructuredPreparation::Enter]),
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Known(FakeValue::Type(FakeType(9)))
    ));
    assert_eq!(host.finished.len(), 1);
    assert_eq!(host.finished[0].0, 1);
}

#[test]
fn structured_type_engine_passes_every_terminal_outcome_through_resume() {
    for preparation in [
        FakeStructuredPreparation::RuntimeDependent,
        FakeStructuredPreparation::NotReady,
        FakeStructuredPreparation::UnsupportedContext,
        FakeStructuredPreparation::Trap,
        FakeStructuredPreparation::HostFailure,
        FakeStructuredPreparation::Abort,
    ] {
        let mut editor = rue_rir::RirEditor::new();
        let root = editor.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
            },
            span: Span::new(0, 0),
        });
        let interner = lasso::ThreadedRodeo::new();
        let mut child_editor = rue_rir::RirEditor::new();
        child_editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish(), child_editor.finish()],
            type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: None,
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Structured(vec![preparation]),
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
        match preparation {
            FakeStructuredPreparation::RuntimeDependent => {
                assert!(matches!(result, ComptimeOutcome::RuntimeDependent));
            }
            FakeStructuredPreparation::NotReady => {
                assert!(matches!(result, ComptimeOutcome::NotReady));
            }
            FakeStructuredPreparation::UnsupportedContext => {
                assert!(matches!(result, ComptimeOutcome::UnsupportedContext));
            }
            FakeStructuredPreparation::Trap => {
                assert!(matches!(result, ComptimeOutcome::Trap(_)));
            }
            FakeStructuredPreparation::HostFailure => {
                assert!(matches!(result, ComptimeOutcome::HostFailure(_)));
            }
            FakeStructuredPreparation::Abort => {
                assert!(matches!(result, ComptimeOutcome::Abort(_)));
            }
            FakeStructuredPreparation::Enter | FakeStructuredPreparation::Memoized => {
                unreachable!()
            }
        }
        assert_eq!(host.finished, vec![(usize::MAX, None)]);
    }
}

#[test]
fn structured_type_engine_enters_then_memoizes_without_an_extra_frame() {
    let mut editor = rue_rir::RirEditor::new();
    let root = editor.add_inst(rue_rir::Inst {
        data: InstData::TypeConst {
            type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
        },
        span: Span::new(17, 29),
    });
    let interner = lasso::ThreadedRodeo::new();
    let mut child_editor = rue_rir::RirEditor::new();
    child_editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish(), child_editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Structured(vec![
            FakeStructuredPreparation::Enter,
            FakeStructuredPreparation::Memoized,
        ]),
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    STRUCTURED_PREPARE_SPANS.with(|spans| spans.borrow_mut().clear());
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Known(FakeValue::Type(FakeType(9)))
    ));
    assert_eq!(host.finished.len(), 1);
    assert_eq!(
        STRUCTURED_PREPARE_SPANS.with(|spans| spans.borrow().clone()),
        vec![Span::new(17, 29), Span::new(17, 29)],
        "structured successors retain the original type-expression span"
    );
    STRUCTURED_PREPARE_SPANS.with(|spans| spans.borrow_mut().clear());
}

#[test]
fn structured_type_entries_share_the_64_frame_boundary() {
    for (recursive_enters, succeeds) in [
        (MAX_COMPTIME_CALL_DEPTH, true),
        (MAX_COMPTIME_CALL_DEPTH + 1, false),
    ] {
        let mut parent = rue_rir::RirEditor::new();
        let root = parent.add_inst(rue_rir::Inst {
            data: InstData::TypeConst {
                type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
            },
            span: Span::new(0, 0),
        });
        let mut child = rue_rir::RirEditor::new();
        let symbol = lasso::ThreadedRodeo::new().get_or_intern("loop");
        let child_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let terminal = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![parent.finish(), child.finish()],
            type_symbol: SymbolHandle::new(symbol),
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::new(),
            recursive: Some((recursive_enters, child_call, terminal, None)),
            enter_count: 0,
            finish_outcome: FakeFinishOutcome::Structured(vec![FakeStructuredPreparation::Enter]),
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
        assert_eq!(
            matches!(result, ComptimeOutcome::Known(FakeValue::Type(FakeType(9)))),
            succeeds
        );
    }
}

#[test]
fn non_local_value_domain_runs_the_real_arithmetic_dispatcher() {
    let mut editor = rue_rir::RirEditor::new();
    let lhs = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(40),
        span: Span::new(0, 0),
    });
    let rhs = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(2),
        span: Span::new(0, 0),
    });
    let add = editor.add_inst(rue_rir::Inst {
        data: InstData::Add { lhs, rhs },
        span: Span::new(0, 0),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let value = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(0, add), &mut env)
        .into_result(|_| FAKE_FAILURE)
        .unwrap()
        .unwrap();
    assert_eq!(value, FakeValue::Integer(42));
}

#[test]
fn durable_only_instruction_forms_cross_the_semantic_host_boundary() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let intrinsic_name = interner.get_or_intern("import");
    let type_name = interner.get_or_intern("Color");
    let variant_name = interner.get_or_intern("Red");
    let string_name = interner.get_or_intern("dep");
    let string = editor.add_inst(rue_rir::Inst {
        data: InstData::StringConst {
            content: string_name,
            anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
        },
        span: Span::new(0, 5),
    });
    let integer = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(9),
        span: Span::new(6, 7),
    });
    let intrinsic = editor
        .add_intrinsic(intrinsic_name, &[string, integer], Span::new(0, 7))
        .unwrap();
    let checked = editor.add_inst(rue_rir::Inst {
        data: InstData::Checked { expr: integer },
        span: Span::new(8, 18),
    });
    let enum_variant = editor.add_inst(rue_rir::Inst {
        data: InstData::EnumVariant {
            module: None,
            type_name,
            variant: variant_name,
        },
        span: Span::new(19, 29),
    });
    let repeat = editor.add_inst(rue_rir::Inst {
        data: InstData::ArrayRepeat {
            value: integer,
            count: rue_rir::RepeatCount::Literal(2),
        },
        span: Span::new(30, 35),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut engine = ComptimeEngine::new(&mut host);
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();

    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, string), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(17))
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, intrinsic), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, checked), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(9))
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, enum_variant), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(23))
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, repeat), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));

    assert!(
        host.dependencies
            .iter()
            .any(|(file, _)| file.index == u32::MAX)
    );
    assert!(
        host.dependencies
            .iter()
            .any(|(file, _)| file.index == 0xFFFF_FFFB)
    );
    assert!(
        host.dependencies
            .iter()
            .any(|(file, _)| file.index == 0xFFFF_FFFD)
    );
    assert!(
        host.dependencies
            .iter()
            .any(|(file, _)| file.index == 0xFFFF_FFFA)
    );
}

#[test]
fn qualified_enum_admission_rejects_before_evaluating_module_child() {
    clear_named_value_observations();
    REJECT_QUALIFIED_ENUM.with(|reject| reject.set(true));
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let module_name = interner.get_or_intern("module");
    let module = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: module_name,
            anchor: None,
        },
        span: Span::new(0, 6),
    });
    let enum_variant = editor.add_inst(rue_rir::Inst {
        data: InstData::EnumVariant {
            module: Some(module),
            type_name: interner.get_or_intern("Arch"),
            variant: interner.get_or_intern("X86_64"),
        },
        span: Span::new(0, 14),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(0, enum_variant), &mut env);
    assert!(matches!(result, ComptimeOutcome::HostFailure(FAKE_FAILURE)));
    assert_eq!(
        NAMED_VALUE_CALLS.with(Cell::get),
        0,
        "qualified enum rejection must precede module-child evaluation"
    );
    REJECT_QUALIFIED_ENUM.with(|reject| reject.set(false));
    clear_named_value_observations();
}

#[test]
fn semantic_sites_use_import_order_and_owning_program_identity() {
    let interner = lasso::ThreadedRodeo::new();
    let import_name = interner.get_or_intern("import");
    let other_name = interner.get_or_intern("other");
    let string_name = interner.get_or_intern("dep");
    let make_program = || {
        let mut editor = rue_rir::RirEditor::new();
        let other_string = editor.add_inst(rue_rir::Inst {
            data: InstData::StringConst {
                content: string_name,
                anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
            },
            span: Span::new(0, 3),
        });
        let _other = editor
            .add_intrinsic(other_name, &[other_string], Span::new(0, 3))
            .unwrap();
        let first_string = editor.add_inst(rue_rir::Inst {
            data: InstData::StringConst {
                content: string_name,
                anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
            },
            span: Span::new(10, 13),
        });
        let first = editor
            .add_intrinsic(import_name, &[first_string], Span::new(10, 13))
            .unwrap();
        let second_string = editor.add_inst(rue_rir::Inst {
            data: InstData::StringConst {
                content: string_name,
                anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
            },
            span: Span::new(10, 13),
        });
        let second = editor
            .add_intrinsic(import_name, &[second_string], Span::new(10, 13))
            .unwrap();
        (editor.finish(), first, second)
    };
    let (program0, first0, second0) = make_program();
    let (program1, first1, _) = make_program();
    let mut host = FakeHost {
        programs: vec![program0, program1],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let mut engine = ComptimeEngine::new(&mut host);
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, first0), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, second0), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(1, first1), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    let import_observations = host
        .dependencies
        .iter()
        .filter(|(file, _)| file.index == 0xFFFF_FFFE || file.index == 0xFFFF_FFFD)
        .map(|(file, name)| (file.index, name.ordinal))
        .collect::<Vec<_>>();
    assert_eq!(
        import_observations,
        vec![(0xFFFF_FFFE, 0), (0xFFFF_FFFE, 1), (0xFFFF_FFFD, 0)]
    );
}

#[test]
fn default_admission_does_not_evaluate_intrinsic_or_enum_children() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let import_name = interner.get_or_intern("import");
    let target_arch_name = interner.get_or_intern("target_arch");
    let target_os_name = interner.get_or_intern("target_os");
    let unknown_name = interner.get_or_intern("unknown_intrinsic");
    let bad = editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("1.0"),
        },
        span: Span::new(0, 3),
    });
    let valid_string = editor.add_inst(rue_rir::Inst {
        data: InstData::StringConst {
            content: interner.get_or_intern("dep"),
            anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
        },
        span: Span::new(0, 3),
    });
    let valid_import = editor
        .add_intrinsic(import_name, &[valid_string], Span::new(0, 3))
        .unwrap();
    let intrinsic = editor
        .add_intrinsic(import_name, &[bad], Span::new(0, 3))
        .unwrap();
    let target_arch = editor
        .add_intrinsic(target_arch_name, &[], Span::new(0, 3))
        .unwrap();
    let malformed_target = editor
        .add_intrinsic(target_os_name, &[bad], Span::new(0, 3))
        .unwrap();
    let unknown_intrinsic = editor
        .add_intrinsic(unknown_name, &[bad], Span::new(0, 3))
        .unwrap();
    let enum_variant = editor.add_inst(rue_rir::Inst {
        data: InstData::EnumVariant {
            module: Some(bad),
            type_name: interner.get_or_intern("Color"),
            variant: interner.get_or_intern("Red"),
        },
        span: Span::new(0, 3),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Structured(Vec::new()),
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let mut engine = ComptimeEngine::new(&mut host);
    EXPRESSION_INTRINSIC_NAMES.with(|names| {
        names.borrow_mut().extend([
            (
                SymbolHandle::new(target_arch_name).issuing_interner_ordinal() as u32,
                "target_arch",
            ),
            (
                SymbolHandle::new(target_os_name).issuing_interner_ordinal() as u32,
                "target_os",
            ),
            (
                SymbolHandle::new(unknown_name).issuing_interner_ordinal() as u32,
                "unknown_intrinsic",
            ),
        ]);
    });
    EXPRESSION_INTRINSIC_REQUESTS.with(|requests| requests.borrow_mut().clear());
    REJECTION_EVENTS.with(|events| events.borrow_mut().clear());
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, valid_import), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, intrinsic), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, target_arch), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, malformed_target), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, unknown_intrinsic), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, enum_variant), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert_eq!(host.float_evaluations.get(), 0);
    let requests = EXPRESSION_INTRINSIC_REQUESTS.with(|requests| requests.borrow().clone());
    assert_eq!(
        requests.len(),
        4,
        "only intrinsic nodes should cross the typed request hook"
    );
    assert!(matches!(
        &requests[0],
        ComptimeExpressionIntrinsicRequest::Import {
            argument_count: 1,
            sole_string_literal: Some(_),
        }
    ));
    assert!(matches!(
        &requests[1],
        ComptimeExpressionIntrinsicRequest::Import {
            argument_count: 1,
            sole_string_literal: None,
        }
    ));
    assert!(matches!(
        &requests[2],
        ComptimeExpressionIntrinsicRequest::Target {
            intrinsic: ComptimeTargetIntrinsic::Arch,
            argument_count: 0,
        }
    ));
    assert!(matches!(
        &requests[3],
        ComptimeExpressionIntrinsicRequest::Target {
            intrinsic: ComptimeTargetIntrinsic::Os,
            argument_count: 1,
        }
    ));
    assert_eq!(
        REJECTION_EVENTS.with(|events| events.borrow().clone()),
        vec![ComptimeSemanticRejection::UnsupportedIntrinsic(
            "unknown_intrinsic".to_owned()
        )]
    );
    EXPRESSION_INTRINSIC_REQUESTS.with(|requests| requests.borrow_mut().clear());
    EXPRESSION_INTRINSIC_NAMES.with(|names| names.borrow_mut().clear());
}

#[test]
fn expression_intrinsic_classifier_covers_targets_and_rejects_unknown_names() {
    assert_eq!(
        ComptimeExpressionIntrinsic::from_name("import"),
        Some(ComptimeExpressionIntrinsic::Import)
    );
    assert_eq!(
        ComptimeExpressionIntrinsic::from_name("target_arch"),
        Some(ComptimeExpressionIntrinsic::Target(
            ComptimeTargetIntrinsic::Arch
        ))
    );
    assert_eq!(
        ComptimeExpressionIntrinsic::from_name("target_os"),
        Some(ComptimeExpressionIntrinsic::Target(
            ComptimeTargetIntrinsic::Os
        ))
    );
    assert_eq!(
        ComptimeExpressionIntrinsic::from_name("target_data_model"),
        Some(ComptimeExpressionIntrinsic::Target(
            ComptimeTargetIntrinsic::DataModel
        ))
    );
    assert_eq!(
        ComptimeExpressionIntrinsic::from_name("unknown_intrinsic"),
        None
    );
}

#[test]
fn expression_intrinsic_requests_preserve_terminals_without_evaluating_children() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let import_name = interner.get_or_intern("import");
    let target_name = interner.get_or_intern("target_arch");
    let bad = editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("1.0"),
        },
        span: Span::new(4, 7),
    });
    let string = editor.add_inst(rue_rir::Inst {
        data: InstData::StringConst {
            content: interner.get_or_intern("dep"),
            anchor: rue_rir::RirStructuralAnchor::new(Vec::new()),
        },
        span: Span::new(0, 3),
    });
    let valid_import = editor
        .add_intrinsic(import_name, &[string], Span::new(0, 3))
        .unwrap();
    let malformed_import = editor
        .add_intrinsic(import_name, &[bad], Span::new(4, 7))
        .unwrap();
    let malformed_target = editor
        .add_intrinsic(target_name, &[bad], Span::new(8, 11))
        .unwrap();
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    EXPRESSION_INTRINSIC_NAMES.with(|names| {
        names.borrow_mut().push((
            SymbolHandle::new(target_name).issuing_interner_ordinal() as u32,
            "target_arch",
        ));
    });
    let outcomes = [
        FakeExpressionIntrinsicOutcome::RuntimeDependent,
        FakeExpressionIntrinsicOutcome::NotReady,
        FakeExpressionIntrinsicOutcome::UnsupportedContext,
        FakeExpressionIntrinsicOutcome::Trap,
        FakeExpressionIntrinsicOutcome::HostFailure,
        FakeExpressionIntrinsicOutcome::Abort,
        FakeExpressionIntrinsicOutcome::Known,
    ];
    for expected in outcomes {
        EXPRESSION_INTRINSIC_OUTCOME.with(|outcome| outcome.set(expected));
        EXPRESSION_INTRINSIC_REQUESTS.with(|requests| requests.borrow_mut().clear());
        let mut engine = ComptimeEngine::new(&mut host);
        for expression in [valid_import, malformed_import, malformed_target] {
            let result = engine.evaluate(ComptimeFrame::expression(0, expression), &mut env);
            match (expected, result) {
                (
                    FakeExpressionIntrinsicOutcome::Known,
                    ComptimeOutcome::Known(FakeValue::Integer(99)),
                )
                | (
                    FakeExpressionIntrinsicOutcome::RuntimeDependent,
                    ComptimeOutcome::RuntimeDependent,
                )
                | (FakeExpressionIntrinsicOutcome::NotReady, ComptimeOutcome::NotReady)
                | (
                    FakeExpressionIntrinsicOutcome::HostFailure,
                    ComptimeOutcome::HostFailure(FAKE_FAILURE),
                )
                | (FakeExpressionIntrinsicOutcome::Abort, ComptimeOutcome::Abort(FAKE_FAILURE)) => {
                }
                (
                    FakeExpressionIntrinsicOutcome::UnsupportedContext,
                    ComptimeOutcome::UnsupportedContext,
                )
                | (
                    FakeExpressionIntrinsicOutcome::Trap,
                    ComptimeOutcome::Trap(ComptimeTrap {
                        operation: "fake intrinsic trap",
                        ..
                    }),
                ) => {}
                other => panic!("unexpected intrinsic outcome: {other:?}"),
            }
        }
        assert_eq!(host.float_evaluations.get(), 0);
        assert_eq!(
            EXPRESSION_INTRINSIC_REQUESTS.with(|requests| requests.borrow().len()),
            3
        );
    }
    EXPRESSION_INTRINSIC_OUTCOME
        .with(|outcome| outcome.set(FakeExpressionIntrinsicOutcome::RuntimeDependent));
    EXPRESSION_INTRINSIC_REQUESTS.with(|requests| requests.borrow_mut().clear());
    EXPRESSION_INTRINSIC_NAMES.with(|names| names.borrow_mut().clear());
}

#[test]
fn unknown_expression_intrinsic_rejects_per_program_without_calling_the_hook() {
    let interner = lasso::ThreadedRodeo::new();
    let unknown_name = interner.get_or_intern("mystery_intrinsic");
    let make_program = || {
        let mut editor = rue_rir::RirEditor::new();
        let bad = editor.add_inst(rue_rir::Inst {
            data: InstData::FloatConst {
                text: interner.get_or_intern("1.0"),
            },
            span: Span::new(12, 15),
        });
        let unknown = editor
            .add_intrinsic(unknown_name, &[bad], Span::new(12, 15))
            .unwrap();
        (editor.finish(), unknown)
    };
    let (program0, unknown0) = make_program();
    let (program1, unknown1) = make_program();
    let mut host = FakeHost {
        programs: vec![program0, program1],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    REJECTION_EVENTS.with(|events| events.borrow_mut().clear());
    REJECTION_SITES.with(|sites| sites.borrow_mut().clear());
    EXPRESSION_INTRINSIC_REQUESTS.with(|requests| requests.borrow_mut().clear());
    let unknown_ordinal = SymbolHandle::new(unknown_name).issuing_interner_ordinal() as u32;
    EXPRESSION_INTRINSIC_NAMES.with(|names| {
        names.borrow_mut().extend([
            (unknown_ordinal, "mystery_intrinsic"),
            (unknown_ordinal + 1000, "mystery_intrinsic"),
        ]);
    });
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let mut engine = ComptimeEngine::new(&mut host);
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, unknown0), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(1, unknown1), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert_eq!(host.float_evaluations.get(), 0);
    assert!(EXPRESSION_INTRINSIC_REQUESTS.with(|requests| requests.borrow().is_empty()));
    assert_eq!(
        REJECTION_EVENTS.with(|events| events.borrow().clone()),
        vec![
            ComptimeSemanticRejection::UnsupportedIntrinsic("mystery_intrinsic".to_owned()),
            ComptimeSemanticRejection::UnsupportedIntrinsic("mystery_intrinsic".to_owned()),
        ]
    );
    assert_eq!(
        REJECTION_SITES.with(|sites| sites.borrow().clone()),
        vec![(0, 12, 15), (1, 12, 15)]
    );
    REJECTION_EVENTS.with(|events| events.borrow_mut().clear());
    REJECTION_SITES.with(|sites| sites.borrow_mut().clear());
    EXPRESSION_INTRINSIC_NAMES.with(|names| names.borrow_mut().clear());
}

#[test]
fn type_intrinsic_hook_receives_typed_bound_and_preserves_failure_channel() {
    clear_type_intrinsic_observations();
    let interner = lasso::ThreadedRodeo::new();
    let intrinsic_name = interner.get_or_intern("int_max");
    let mut editor = rue_rir::RirEditor::new();
    let type_arg = editor.add_unit_type().expect("unit type syntax");
    let root = editor.add_inst(rue_rir::Inst {
        data: InstData::TypeIntrinsic {
            name: intrinsic_name,
            type_arg,
        },
        span: Span::new(0, 3),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    TYPE_INTRINSIC_NAME.with(|configured| {
        *configured.borrow_mut() = Some((
            SymbolHandle::new(intrinsic_name).issuing_interner_ordinal() as u32,
            "int_max",
        ));
    });
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
    assert!(
        matches!(
            result,
            ComptimeOutcome::Known(FakeValue::TypedInteger(127, FakeType(7)))
        ),
        "unexpected type-intrinsic result: {result:?}"
    );
    assert_eq!(
        TYPE_INTRINSIC_EVENTS.with(|events| events.borrow().clone()),
        vec![(
            ComptimeTypeIntrinsic::IntegerBound(ComptimeIntegerBound::Max),
            FakeType(7),
        )]
    );

    TYPE_INTRINSIC_FAILURE.with(|failure| failure.set(true));
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::HostFailure(FakeFailure::Generic)
    ));

    TYPE_INTRINSIC_FAILURE.with(|failure| failure.set(false));
    TYPE_INTRINSIC_ABORT.with(|abort| abort.set(true));
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Abort(FakeFailure::Generic)
    ));
    clear_type_intrinsic_observations();
}

#[test]
fn checked_propagates_a_non_known_child_terminal() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let child = editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("1.0"),
        },
        span: Span::new(0, 3),
    });
    let checked = editor.add_inst(rue_rir::Inst {
        data: InstData::Checked { expr: child },
        span: Span::new(0, 3),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, checked), &mut env);
    assert!(matches!(result, ComptimeOutcome::HostFailure(FAKE_FAILURE)));
    assert_eq!(host.float_evaluations.get(), 1);
}

#[test]
fn member_fallback_receives_a_qualified_base_value() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let base = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: interner.get_or_intern("module"),
            anchor: None,
        },
        span: Span::new(0, 6),
    });
    let field = editor.add_inst(rue_rir::Inst {
        data: InstData::FieldGet {
            base,
            field: interner.get_or_intern("VALUE"),
        },
        span: Span::new(0, 12),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(0, field), &mut env)
        .into_result(|_| FAKE_FAILURE)
        .unwrap();
    assert_eq!(result, Some(FakeValue::Integer(31)));
}

#[test]
fn typed_integer_metadata_survives_bitwise_and_non_scalar_comparison() {
    let mut editor = rue_rir::RirEditor::new();
    editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(0),
        span: Span::new(0, 1),
    });
    editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(0),
        span: Span::new(1, 2),
    });
    let lhs = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(7),
        span: Span::new(2, 3),
    });
    let rhs = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(3),
        span: Span::new(3, 4),
    });
    let bitand = editor.add_inst(rue_rir::Inst {
        data: InstData::BitAnd { lhs, rhs },
        span: Span::new(2, 4),
    });
    let left_type = editor.add_inst(rue_rir::Inst {
        data: InstData::TypeConst {
            type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
        },
        span: Span::new(5, 8),
    });
    let right_type = editor.add_inst(rue_rir::Inst {
        data: InstData::TypeConst {
            type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
        },
        span: Span::new(9, 12),
    });
    let equality = editor.add_inst(rue_rir::Inst {
        data: InstData::Eq {
            lhs: left_type,
            rhs: right_type,
        },
        span: Span::new(5, 12),
    });
    let bitnot = editor.add_inst(rue_rir::Inst {
        data: InstData::BitNot { operand: lhs },
        span: Span::new(2, 3),
    });
    let interner = lasso::ThreadedRodeo::new();
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut engine = ComptimeEngine::new(&mut host);
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let bitand_result = engine.evaluate(ComptimeFrame::expression(0, bitand), &mut env);
    assert!(
        matches!(
            bitand_result,
            ComptimeOutcome::Known(FakeValue::TypedInteger(3, FakeType(8)))
        ),
        "bitand result: {bitand_result:?}"
    );
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, equality), &mut env),
        ComptimeOutcome::Known(FakeValue::Boolean(true))
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, bitnot), &mut env),
        ComptimeOutcome::Known(FakeValue::TypedInteger(-8, FakeType(8)))
    ));
}

#[test]
fn integer_type_mismatch_is_a_host_failure_for_binary_comparisons() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let lhs_symbol = interner.get_or_intern("lhs");
    let rhs_symbol = interner.get_or_intern("rhs");
    let lhs = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: lhs_symbol,
            anchor: None,
        },
        span: Span::new(0, 1),
    });
    let rhs = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: rhs_symbol,
            anchor: None,
        },
        span: Span::new(2, 3),
    });
    let equality = editor.add_inst(rue_rir::Inst {
        data: InstData::Eq { lhs, rhs },
        span: Span::new(0, 3),
    });
    let lhs_name = FakeName {
        ordinal: SymbolHandle::new(lhs_symbol).issuing_interner_ordinal() as u32,
    };
    let rhs_name = FakeName {
        ordinal: SymbolHandle::new(rhs_symbol).issuing_interner_ordinal() as u32,
    };
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    env.value_subst
        .insert(lhs_name, FakeValue::TypedInteger(1, FakeType(8)));
    env.value_subst
        .insert(rhs_name, FakeValue::TypedInteger(1, FakeType(16)));
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, equality), &mut env),
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));
}

#[test]
fn non_local_failure_domain_receives_engine_float_failure() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let float = editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("1.0"),
        },
        span: Span::new(0, 0),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let failure =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, float), &mut env);
    assert!(matches!(failure, ComptimeOutcome::HostFailure(_)));
}

#[test]
fn typed_division_by_zero_is_a_structured_trap() {
    let mut editor = rue_rir::RirEditor::new();
    let lhs = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let rhs = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(0),
        span: Span::new(0, 0),
    });
    let div = editor.add_inst(rue_rir::Inst {
        data: InstData::Div { lhs, rhs },
        span: Span::new(4, 5),
    });
    let interner = lasso::ThreadedRodeo::new();
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, div), &mut env),
        ComptimeOutcome::Trap(ComptimeTrap {
            operation: "division by zero",
            ..
        })
    ));
}

#[test]
fn direct_negative_literal_uses_the_distinct_negation_operation() {
    let mut editor = rue_rir::RirEditor::new();
    let magnitude = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(129),
        span: Span::new(0, 3),
    });
    let negative = editor.add_inst(rue_rir::Inst {
        data: InstData::Neg { operand: magnitude },
        span: Span::new(0, 4),
    });
    let interner = lasso::ThreadedRodeo::new();
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    FINISH_ARITH_OPERATIONS.with(|operations| operations.borrow_mut().clear());

    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(
            ComptimeFrame {
                expected_result: Some(FakeType(8)),
                ..ComptimeFrame::expression(0, negative)
            },
            &mut env,
        ),
        ComptimeOutcome::RuntimeDependent
    ));
    FINISH_ARITH_OPERATIONS.with(|operations| {
        assert_eq!(operations.borrow().as_slice(), ["negation"]);
    });
}

#[test]
fn equality_evaluates_rhs_only_after_nonterminal_lhs_outcomes() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("runtime");
    let mut editor = rue_rir::RirEditor::new();
    let lhs = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: symbol,
            anchor: None,
        },
        span: Span::new(0, 1),
    });
    let rhs = editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("1.0"),
        },
        span: Span::new(2, 3),
    });
    let eq = editor.add_inst(rue_rir::Inst {
        data: InstData::Eq { lhs, rhs },
        span: Span::new(0, 3),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, eq), &mut env),
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));
    assert_eq!(host.float_evaluations.get(), 1);

    let mut editor = rue_rir::RirEditor::new();
    let one = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 1),
    });
    let zero = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(0),
        span: Span::new(2, 3),
    });
    let trap = editor.add_inst(rue_rir::Inst {
        data: InstData::Div {
            lhs: one,
            rhs: zero,
        },
        span: Span::new(0, 3),
    });
    let rhs = editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("2.0"),
        },
        span: Span::new(4, 5),
    });
    let eq = editor.add_inst(rue_rir::Inst {
        data: InstData::Eq { lhs: trap, rhs },
        span: Span::new(0, 5),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, eq), &mut env),
        ComptimeOutcome::Trap(ComptimeTrap {
            operation: "division by zero",
            ..
        })
    ));
    assert_eq!(host.float_evaluations.get(), 0);
}

#[test]
fn non_local_value_domain_runs_the_real_branch_dispatcher() {
    let mut editor = rue_rir::RirEditor::new();
    let condition = editor.add_inst(rue_rir::Inst {
        data: InstData::BoolConst(true),
        span: Span::new(0, 0),
    });
    let then_value = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(7),
        span: Span::new(0, 0),
    });
    let else_value = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(9),
        span: Span::new(0, 0),
    });
    let branch = editor.add_inst(rue_rir::Inst {
        data: InstData::Branch {
            cond: condition,
            then_block: then_value,
            else_block: Some(else_value),
        },
        span: Span::new(0, 0),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        ComptimeEngine::new(&mut host).select_branch(0, condition, &mut env),
        ComptimeOutcome::Known(ComptimeSelection::Branch { taken: true })
    ));
    let value = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(0, branch), &mut env)
        .into_result(|_| FAKE_FAILURE)
        .unwrap()
        .unwrap();
    assert_eq!(value, FakeValue::Integer(7));
}

#[test]
fn cancellation_checkpoints_abort_only_entered_block_branch_nodes() {
    let mut editor = rue_rir::RirEditor::new();
    let condition = editor.add_inst(rue_rir::Inst {
        data: InstData::BoolConst(true),
        span: Span::new(0, 0),
    });
    let then_value = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(7),
        span: Span::new(0, 0),
    });
    let else_value = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(9),
        span: Span::new(0, 0),
    });
    let then_block = editor.add_block(&[then_value], Span::new(0, 0)).unwrap();
    let else_block = editor.add_block(&[else_value], Span::new(0, 0)).unwrap();
    let branch = editor.add_inst(rue_rir::Inst {
        data: InstData::Branch {
            cond: condition,
            then_block,
            else_block: Some(else_block),
        },
        span: Span::new(0, 0),
    });
    let sibling = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(11),
        span: Span::new(0, 0),
    });
    let root = editor
        .add_block(&[branch, sibling], Span::new(0, 0))
        .unwrap();
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(lasso::ThreadedRodeo::new().get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    // root block, branch, condition, and selected block are entered; the
    // selected value is the first node rejected by this checkpoint.
    configure_checkpoint_abort(Some(5));
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Abort(FakeFailure::Canceled)
    ));
    assert_eq!(checkpoint_count(), 5);
    configure_checkpoint_abort(None);
}

#[test]
fn cancellation_abort_in_entered_frame_finishes_and_cleans_up() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("f");
    let symbol_handle = SymbolHandle::new(symbol);
    let mut root_editor = rue_rir::RirEditor::new();
    let call = root_editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let after = root_editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(20),
        span: Span::new(0, 0),
    });
    let mut child_editor = rue_rir::RirEditor::new();
    let child_body = child_editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let base = symbol_handle.issuing_interner_ordinal() as u32;
    let mut call_plans = AHashMap::new();
    call_plans.insert(
        base,
        FakePreparedCall::Enter {
            program: 1,
            body: child_body,
            expected: None,
            name_bindings: AHashMap::new(),
        },
    );
    let mut host = FakeHost {
        programs: vec![root_editor.finish(), child_editor.finish()],
        type_symbol: symbol_handle,
        constant: None,
        dependencies: Vec::new(),
        call_plans,
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    LABEL_CALLS.with(|calls| calls.set(0));
    TICKET_EVENTS.with(|events| events.borrow_mut().clear());
    configure_checkpoint_abort(Some(2));
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let aborted =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
    assert!(matches!(
        aborted,
        ComptimeOutcome::Abort(FakeFailure::Canceled)
    ));
    assert_eq!(host.finished.len(), 1);
    assert_eq!(host.finished[0].0, 1);
    TICKET_EVENTS.with(|events| {
        assert_eq!(*events.borrow(), vec![(1, true), (1, false)]);
    });
    assert_eq!(LABEL_CALLS.with(Cell::get), 0);
    configure_checkpoint_abort(None);
    let resumed =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, after), &mut env);
    assert!(matches!(
        resumed,
        ComptimeOutcome::Known(FakeValue::Integer(20))
    ));
}

#[test]
fn non_local_type_domain_runs_the_real_type_dispatcher() {
    let mut editor = rue_rir::RirEditor::new();
    let type_const = editor.add_inst(rue_rir::Inst {
        data: InstData::TypeConst {
            type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
        },
        span: Span::new(0, 0),
    });
    let mut second_editor = rue_rir::RirEditor::new();
    let second_type_const = second_editor.add_inst(rue_rir::Inst {
        data: InstData::TypeConst {
            type_name: rue_rir::RirTypeSyntaxRef::from_u32(0),
        },
        span: Span::new(0, 0),
    });
    let interner = lasso::ThreadedRodeo::new();
    let type_symbol = SymbolHandle::new(interner.get_or_intern("T"));
    let mut host = FakeHost {
        programs: vec![editor.finish(), second_editor.finish()],
        type_symbol,
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_named_value_observations();
    TYPE_VALUE_PROGRAMS.with(|programs| programs.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let value = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(0, type_const), &mut env)
        .into_result(|_| FAKE_FAILURE)
        .unwrap()
        .unwrap();
    let second_value = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(1, second_type_const), &mut env)
        .into_result(|_| FAKE_FAILURE)
        .unwrap()
        .unwrap();
    assert_eq!(value, FakeValue::Type(FakeType(7)));
    assert_eq!(second_value, FakeValue::Type(FakeType(7)));
    assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 0);
    TYPE_VALUE_PROGRAMS.with(|programs| assert_eq!(*programs.borrow(), vec![0, 1]));
}

#[test]
fn runtime_binding_name_blocks_global_constant_fallback() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let name_symbol = SymbolHandle::new(interner.get_or_intern("n"));
    let name = FakeName {
        ordinal: name_symbol.issuing_interner_ordinal() as u32,
    };
    let reference = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: name_symbol.spur(),
            anchor: None,
        },
        span: Span::new(0, 1),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: Some((
            FakeFile { index: 0 },
            name.clone(),
            FakeConstInfo {
                span: Span::new(10, 11),
                value: Some(FakeValue::Integer(99)),
            },
        )),
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    clear_named_value_observations();
    env.runtime_binding_names.insert(name);
    let value = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(0, reference), &mut env)
        .into_result(|_| FAKE_FAILURE)
        .unwrap();
    assert_eq!(value, None);
    assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 0);
}

#[test]
fn file_resolution_is_keyed_by_the_active_program() {
    let interner = lasso::ThreadedRodeo::new();
    let name = interner.get_or_intern("value");
    let mut first = rue_rir::RirEditor::new();
    let first_reference = first.add_inst(rue_rir::Inst {
        data: InstData::VarRef { name, anchor: None },
        span: Span::with_file(rue_span::FileId::new(7), 0, 1),
    });
    let mut second = rue_rir::RirEditor::new();
    let second_reference = second.add_inst(rue_rir::Inst {
        data: InstData::VarRef { name, anchor: None },
        span: Span::with_file(rue_span::FileId::new(7), 0, 1),
    });
    let mut host = FakeHost {
        programs: vec![first.finish(), second.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    KEYED_FILE_RESOLUTION.with(|enabled| enabled.set(true));
    NAMED_TYPE_MISSING.with(|missing| missing.set(true));
    FILE_RESOLUTION_CALLS.with(|calls| calls.borrow_mut().clear());
    let mut engine = ComptimeEngine::new(&mut host);
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, first_reference), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(1, second_reference), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    FILE_RESOLUTION_CALLS.with(|calls| {
        assert_eq!(*calls.borrow(), vec![(0, 7), (1, 107)]);
    });
    KEYED_FILE_RESOLUTION.with(|enabled| enabled.set(false));
    NAMED_TYPE_MISSING.with(|missing| missing.set(false));
}

#[test]
fn constant_dependency_uses_declaration_file_not_reference_file() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let name_symbol = SymbolHandle::new(interner.get_or_intern("answer"));
    let name = FakeName {
        ordinal: name_symbol.issuing_interner_ordinal() as u32,
    };
    let reference = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: name_symbol.spur(),
            anchor: None,
        },
        span: Span::with_file(rue_span::FileId::new(3), 0, 1),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: Some((
            FakeFile { index: 3 },
            name,
            FakeConstInfo {
                span: Span::with_file(rue_span::FileId::new(9), 10, 11),
                value: Some(FakeValue::Integer(42)),
            },
        )),
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    clear_named_value_observations();
    let value = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(0, reference), &mut env)
        .into_result(|_| FAKE_FAILURE)
        .unwrap();
    assert_eq!(value, Some(FakeValue::Integer(42)));
    assert_eq!(
        host.dependencies,
        vec![(FakeFile { index: 9 }, FakeName { ordinal: 0 })]
    );
    assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 1);
}

#[test]
fn atomic_named_value_hook_preserves_states_dependency_order_and_visibility() {
    let interner = lasso::ThreadedRodeo::new();
    let name = FakeName { ordinal: 1 };
    let mut host = FakeHost {
        programs: Vec::new(),
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: Some((
            FakeFile { index: 3 },
            name.clone(),
            FakeConstInfo {
                span: Span::with_file(rue_span::FileId::new(9), 10, 11),
                value: Some(FakeValue::Integer(42)),
            },
        )),
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_named_value_observations();
    let file = FakeFile { index: 3 };
    let known = host
        .resolve_comptime_named_value(file.clone(), name.clone(), Span::new(0, 1))
        .unwrap();
    assert!(matches!(
        known,
        ComptimeNamedValueResolution::Known(FakeValue::Integer(42))
    ));
    host.constant.as_mut().unwrap().2.value = None;
    let runtime_dependent = host
        .resolve_comptime_named_value(file.clone(), name.clone(), Span::new(0, 1))
        .unwrap();
    assert!(matches!(
        runtime_dependent,
        ComptimeNamedValueResolution::RuntimeDependent
    ));
    host.constant = None;
    NAMED_TYPE_MISSING.with(|missing| missing.set(true));
    let missing = host
        .resolve_comptime_named_value(file.clone(), name.clone(), Span::new(0, 1))
        .unwrap();
    assert!(matches!(missing, ComptimeNamedValueResolution::Missing));
    assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 3);
    assert_eq!(
        host.dependencies,
        vec![
            (FakeFile { index: 9 }, name.clone()),
            (FakeFile { index: 9 }, name.clone()),
        ]
    );

    host.constant = Some((
        file,
        name.clone(),
        FakeConstInfo {
            span: Span::with_file(rue_span::FileId::new(9), 10, 11),
            value: Some(FakeValue::Integer(7)),
        },
    ));
    REJECT_VISIBILITY.with(|reject| reject.set(true));
    assert!(
        host.resolve_comptime_named_value(FakeFile { index: 3 }, name, Span::new(0, 1))
            .is_err()
    );
    assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 4);
    assert_eq!(host.dependencies.len(), 3);
    clear_named_value_observations();
}

#[test]
fn earlier_terminal_skips_atomic_named_value_hook_and_later_sibling() {
    let interner = lasso::ThreadedRodeo::new();
    let mut editor = rue_rir::RirEditor::new();
    let terminal = editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("1.0"),
        },
        span: Span::new(0, 3),
    });
    let later = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: interner.get_or_intern("later"),
            anchor: None,
        },
        span: Span::new(0, 8),
    });
    let block = editor
        .add_block(&[terminal, later], Span::new(0, 8))
        .unwrap();
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_named_value_observations();
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, block), &mut env);
    assert!(matches!(result, ComptimeOutcome::HostFailure(FAKE_FAILURE)));
    assert_eq!(host.float_evaluations.get(), 1);
    assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 0);
}

#[test]
fn runtime_local_name_precedes_same_named_comptime_substitutions() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let name_symbol = SymbolHandle::new(interner.get_or_intern("n"));
    let name = FakeName {
        ordinal: name_symbol.issuing_interner_ordinal() as u32,
    };
    let reference = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: name_symbol.spur(),
            anchor: None,
        },
        span: Span::new(0, 1),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    env.type_subst.insert(name.clone(), FakeType(1));
    env.value_subst.insert(name.clone(), FakeValue::Integer(2));
    env.runtime_local_names.insert(name);
    let value = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(0, reference), &mut env)
        .into_result(|_| FAKE_FAILURE)
        .unwrap();
    assert_eq!(value, None);
}

fn call_fixture() -> (FakeHost, InstRef, InstRef, u32) {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("f");
    let symbol_handle = SymbolHandle::new(symbol);
    let base = symbol_handle.issuing_interner_ordinal() as u32;

    let mut first = rue_rir::RirEditor::new();
    let first_call = first.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let first_rhs = first.add_inst(rue_rir::Inst {
        data: InstData::IntConst(2),
        span: Span::new(0, 0),
    });
    let first_root = first.add_inst(rue_rir::Inst {
        data: InstData::Add {
            lhs: first_call,
            rhs: first_rhs,
        },
        span: Span::new(0, 0),
    });

    let mut second = rue_rir::RirEditor::new();
    let second_call = second.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    second.add_inst(rue_rir::Inst {
        data: InstData::IntConst(20),
        span: Span::new(0, 0),
    });

    let mut third = rue_rir::RirEditor::new();
    let third_terminal = third.add_inst(rue_rir::Inst {
        data: InstData::IntConst(20),
        span: Span::new(0, 0),
    });
    let second_name = FakeName {
        ordinal: base + 1000,
    };
    let third_name = FakeName {
        ordinal: base + 2000,
    };
    let mut name_bindings = AHashMap::new();
    name_bindings.insert(second_name, third_name.clone());
    let mut call_plans = AHashMap::new();
    call_plans.insert(
        base,
        FakePreparedCall::Enter {
            program: 1,
            body: second_call,
            expected: Some(FakeType(7)),
            name_bindings,
        },
    );
    call_plans.insert(
        third_name.ordinal,
        FakePreparedCall::Enter {
            program: 2,
            body: third_terminal,
            expected: Some(FakeType(7)),
            name_bindings: AHashMap::new(),
        },
    );

    let host = FakeHost {
        programs: vec![first.finish(), second.finish(), third.finish()],
        type_symbol: symbol_handle,
        constant: None,
        dependencies: Vec::new(),
        call_plans,
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    (host, first_root, first_rhs, base)
}

#[test]
fn call_argument_provenance_is_left_to_right_and_engine_owned() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("f");
    let symbol_handle = SymbolHandle::new(symbol);
    let mut editor = rue_rir::RirEditor::new();
    let direct_unit = editor.add_inst(rue_rir::Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 2),
    });
    let wrapped_unit = editor.add_inst(rue_rir::Inst {
        data: InstData::Comptime { expr: direct_unit },
        span: Span::new(0, 2),
    });
    let call = editor
        .add_call(
            symbol,
            &[
                rue_rir::RirCallArg {
                    value: direct_unit,
                    mode: rue_rir::RirArgMode::Normal,
                },
                rue_rir::RirCallArg {
                    value: wrapped_unit,
                    mode: rue_rir::RirArgMode::Normal,
                },
            ],
            Span::new(0, 2),
        )
        .unwrap();
    let mut child = rue_rir::RirEditor::new();
    let child_body = child.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let base = symbol_handle.issuing_interner_ordinal() as u32;
    let mut call_plans = AHashMap::new();
    call_plans.insert(
        base,
        FakePreparedCall::Enter {
            program: 1,
            body: child_body,
            expected: None,
            name_bindings: AHashMap::new(),
        },
    );
    let mut host = FakeHost {
        programs: vec![editor.finish(), child.finish()],
        type_symbol: symbol_handle,
        constant: None,
        dependencies: Vec::new(),
        call_plans,
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_call_argument_observations();
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Known(FakeValue::Integer(1))
    ));
    CALL_ARGUMENTS.with(|observed| {
        assert_eq!(
            *observed.borrow(),
            vec![(FakeValue::Unit, true), (FakeValue::Unit, false)]
        );
    });
}

#[test]
fn qualified_call_uses_the_same_argument_provenance_helper() {
    let interner = lasso::ThreadedRodeo::new();
    let method = interner.get_or_intern("Id");
    let method_handle = SymbolHandle::new(method);
    let mut editor = rue_rir::RirEditor::new();
    let receiver = editor.add_inst(rue_rir::Inst {
        data: InstData::VarRef {
            name: interner.get_or_intern("lib"),
            anchor: None,
        },
        span: Span::new(0, 3),
    });
    let direct_unit = editor.add_inst(rue_rir::Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 2),
    });
    let call = editor
        .add_method_call(
            receiver,
            method,
            &[rue_rir::RirCallArg {
                value: direct_unit,
                mode: rue_rir::RirArgMode::Normal,
            }],
            Span::new(0, 3),
        )
        .unwrap();
    let mut child = rue_rir::RirEditor::new();
    let child_body = child.add_inst(rue_rir::Inst {
        data: InstData::IntConst(2),
        span: Span::new(0, 0),
    });
    let method_ordinal = method_handle.issuing_interner_ordinal() as u32;
    let mut call_plans = AHashMap::new();
    call_plans.insert(
        method_ordinal,
        FakePreparedCall::Enter {
            program: 1,
            body: child_body,
            expected: None,
            name_bindings: AHashMap::new(),
        },
    );
    let mut host = FakeHost {
        programs: vec![editor.finish(), child.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans,
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_call_argument_observations();
    ALLOW_MODULE_CALLS.with(|allowed| allowed.set(true));
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    env.defining_file = Some(FakeFile { index: 0 });
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Known(FakeValue::Integer(2))
    ));
    CALL_ARGUMENTS.with(|observed| {
        assert_eq!(*observed.borrow(), vec![(FakeValue::Unit, true)]);
    });
    clear_call_argument_observations();
}

#[test]
fn evaluated_method_receiver_is_admitted_before_arguments_and_preserves_terminals() {
    let interner = lasso::ThreadedRodeo::new();
    let receiver_symbol = interner.get_or_intern("lib");
    let method_symbol = interner.get_or_intern("run");
    let receiver_handle = SymbolHandle::new(receiver_symbol);
    let method_handle = SymbolHandle::new(method_symbol);
    let inner_symbol = interner.get_or_intern("inner");
    let inner_handle = SymbolHandle::new(inner_symbol);
    let mut parent = rue_rir::RirEditor::new();
    let receiver = parent.add_inst(Inst {
        data: InstData::VarRef {
            name: receiver_symbol,
            anchor: None,
        },
        span: Span::new(0, 3),
    });
    let argument = parent.add_inst(Inst {
        data: InstData::UnitConst,
        span: Span::new(4, 6),
    });
    let call = parent
        .add_method_call(
            receiver,
            method_symbol,
            &[rue_rir::RirCallArg {
                value: argument,
                mode: rue_rir::RirArgMode::Normal,
            }],
            Span::new(0, 7),
        )
        .unwrap();
    let terminal_receiver = parent
        .add_call(inner_symbol, &[], Span::new(8, 13))
        .unwrap();
    let terminal_call = parent
        .add_method_call(
            terminal_receiver,
            method_symbol,
            &[rue_rir::RirCallArg {
                value: argument,
                mode: rue_rir::RirArgMode::Normal,
            }],
            Span::new(8, 20),
        )
        .unwrap();
    let non_module_receiver = parent.add_inst(Inst {
        data: InstData::UnitConst,
        span: Span::new(21, 22),
    });
    let non_module_call = parent
        .add_method_call(
            non_module_receiver,
            method_symbol,
            &[rue_rir::RirCallArg {
                value: argument,
                mode: rue_rir::RirArgMode::Normal,
            }],
            Span::new(21, 29),
        )
        .unwrap();
    let mut child = rue_rir::RirEditor::new();
    let child_body = child.add_inst(Inst {
        data: InstData::IntConst(42),
        span: Span::new(10, 12),
    });
    let receiver_name = FakeName {
        ordinal: receiver_handle.issuing_interner_ordinal() as u32,
    };
    let method_name = method_handle.issuing_interner_ordinal() as u32;
    let selected_name = method_name + 7;
    let inner_name = inner_handle.issuing_interner_ordinal() as u32;
    let mut call_plans = AHashMap::new();
    call_plans.insert(
        selected_name,
        FakePreparedCall::Enter {
            program: 1,
            body: child_body,
            expected: None,
            name_bindings: AHashMap::new(),
        },
    );
    let mut host = FakeHost {
        programs: vec![parent.finish(), child.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: Some((
            FakeFile { index: 0 },
            receiver_name.clone(),
            FakeConstInfo {
                span: Span::new(0, 3),
                value: Some(FakeValue::Type(FakeType(7))),
            },
        )),
        dependencies: Vec::new(),
        call_plans,
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };

    // Ordinary hosts retain the path-only shortcut and do not evaluate the
    // receiver before module-path resolution.
    clear_call_argument_observations();
    clear_named_value_observations();
    host.dependencies.clear();
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env),
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(EVALUATED_METHOD_EVENTS.with(|events| events.borrow().is_empty()));
    assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 0);
    assert!(host.dependencies.is_empty());

    // Durable-style hosts evaluate even a syntactically decodable path.
    // The receiver token is retained by the admission hook, so the caller
    // cannot accidentally select a same-spelled callable in its own module.
    EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.set(1));
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Known(FakeValue::Integer(42))
    ));
    assert_eq!(
        EVALUATED_METHOD_EVENTS.with(|events| events.borrow().clone()),
        vec!["receiver_eval", "receiver_hook", "argument"]
    );
    assert_eq!(
        EVALUATED_METHOD_RECEIVERS.with(|receivers| receivers.borrow().clone()),
        vec![FakeValue::Type(FakeType(7))]
    );
    assert_eq!(
        EVALUATED_METHOD_ARGUMENT_CALLS.with(Cell::get),
        1,
        "arguments are evaluated only after receiver admission"
    );
    assert_eq!(NAMED_VALUE_CALLS.with(Cell::get), 1);
    assert_eq!(
        host.dependencies,
        vec![(FakeFile { index: 0 }, receiver_name.clone())]
    );

    // A known non-module receiver is rejected by its semantic value, and
    // never reaches ordinary argument binding or preparation.
    clear_call_argument_observations();
    EVALUATED_METHOD_RECEIVER_MODE.with(|mode| mode.set(1));
    EVALUATED_METHOD_FAIL_ON_UNIT.with(|fail| fail.set(true));
    let non_module_result = ComptimeEngine::new(&mut host)
        .evaluate(ComptimeFrame::expression(0, non_module_call), &mut env);
    assert!(matches!(
        non_module_result,
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));
    assert_eq!(
        EVALUATED_METHOD_RECEIVERS.with(|receivers| receivers.borrow().clone()),
        vec![FakeValue::Unit]
    );
    assert_eq!(EVALUATED_METHOD_ARGUMENT_CALLS.with(Cell::get), 0);
    assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
    assert_eq!(PREPARE_CALLS.with(Cell::get), 0);

    // A receiver hook terminal propagates before argument evaluation.
    for mode in 2..=8 {
        clear_call_argument_observations();
        EVALUATED_METHOD_RECEIVER_MODE.with(|configured| configured.set(mode));
        let result =
            ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
        match mode {
            2 | 3 => assert!(matches!(result, ComptimeOutcome::RuntimeDependent)),
            4 => assert!(matches!(result, ComptimeOutcome::NotReady)),
            5 => assert!(matches!(result, ComptimeOutcome::UnsupportedContext)),
            6 => assert!(matches!(result, ComptimeOutcome::Trap(_))),
            7 => assert!(matches!(result, ComptimeOutcome::HostFailure(_))),
            8 => assert!(matches!(result, ComptimeOutcome::Abort(_))),
            _ => unreachable!(),
        }
        assert_eq!(
            EVALUATED_METHOD_EVENTS.with(|events| events.borrow().clone()),
            vec!["receiver_eval", "receiver_hook"]
        );
        assert_eq!(EVALUATED_METHOD_ARGUMENT_CALLS.with(Cell::get), 0);
    }

    // The same terminals must also propagate when they are genuinely
    // produced while evaluating the receiver, before the receiver hook is
    // reached. This covers the legacy receiver-evaluation ordering.
    for mode in 3..=8 {
        clear_call_argument_observations();
        EVALUATED_METHOD_RECEIVER_MODE.with(|configured| configured.set(1));
        let receiver_outcome = match mode {
            3 => ComptimeOutcome::RuntimeDependent,
            4 => ComptimeOutcome::NotReady,
            5 => ComptimeOutcome::UnsupportedContext,
            6 => ComptimeOutcome::Trap(ComptimeTrap {
                operation: "receiver trap",
                span: Span::new(0, 0),
            }),
            7 => ComptimeOutcome::HostFailure(FAKE_FAILURE),
            _ => ComptimeOutcome::Abort(FAKE_FAILURE),
        };
        host.call_plans
            .insert(inner_name, FakePreparedCall::Memoized(receiver_outcome));
        let result = ComptimeEngine::new(&mut host)
            .evaluate(ComptimeFrame::expression(0, terminal_call), &mut env);
        match mode {
            3 => assert!(matches!(result, ComptimeOutcome::RuntimeDependent)),
            4 => assert!(matches!(result, ComptimeOutcome::NotReady)),
            5 => assert!(matches!(result, ComptimeOutcome::UnsupportedContext)),
            6 => assert!(matches!(result, ComptimeOutcome::Trap(_))),
            7 => assert!(matches!(result, ComptimeOutcome::HostFailure(_))),
            8 => assert!(matches!(result, ComptimeOutcome::Abort(_))),
            _ => unreachable!(),
        }
        assert!(EVALUATED_METHOD_EVENTS.with(|events| events.borrow().is_empty()));
        assert!(EVALUATED_METHOD_RECEIVERS.with(|receivers| receivers.borrow().is_empty()));
        assert_eq!(EVALUATED_METHOD_ARGUMENT_CALLS.with(Cell::get), 0);
    }
    clear_call_argument_observations();
    clear_named_value_observations();
    host.dependencies.clear();
}

#[test]
fn argument_provenance_restores_parent_program_after_a_foreign_argument() {
    let interner = lasso::ThreadedRodeo::new();
    let outer_symbol = interner.get_or_intern("outer");
    let inner_symbol = interner.get_or_intern("inner");
    let outer_handle = SymbolHandle::new(outer_symbol);
    let inner_handle = SymbolHandle::new(inner_symbol);
    let mut parent = rue_rir::RirEditor::new();
    let inner_call = parent.add_call(inner_symbol, &[], Span::new(0, 0)).unwrap();
    let direct_unit = parent.add_inst(rue_rir::Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 0),
    });
    let outer_call = parent
        .add_call(
            outer_symbol,
            &[
                rue_rir::RirCallArg {
                    value: inner_call,
                    mode: rue_rir::RirArgMode::Normal,
                },
                rue_rir::RirCallArg {
                    value: direct_unit,
                    mode: rue_rir::RirArgMode::Normal,
                },
            ],
            Span::new(0, 0),
        )
        .unwrap();
    let mut inner_program = rue_rir::RirEditor::new();
    let inner_body = inner_program.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let mut outer_program = rue_rir::RirEditor::new();
    let outer_body = outer_program.add_inst(rue_rir::Inst {
        data: InstData::IntConst(2),
        span: Span::new(0, 0),
    });
    let outer_ordinal = outer_handle.issuing_interner_ordinal() as u32;
    let inner_ordinal = inner_handle.issuing_interner_ordinal() as u32;
    let mut call_plans = AHashMap::new();
    call_plans.insert(
        outer_ordinal,
        FakePreparedCall::Enter {
            program: 2,
            body: outer_body,
            expected: None,
            name_bindings: AHashMap::new(),
        },
    );
    call_plans.insert(
        inner_ordinal,
        FakePreparedCall::Enter {
            program: 1,
            body: inner_body,
            expected: None,
            name_bindings: AHashMap::new(),
        },
    );
    let mut host = FakeHost {
        programs: vec![
            parent.finish(),
            inner_program.finish(),
            outer_program.finish(),
        ],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans,
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_call_argument_observations();
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, outer_call), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Known(FakeValue::Integer(2))
    ));
    CALL_ARGUMENTS.with(|observed| {
        assert_eq!(
            *observed.borrow(),
            vec![(FakeValue::Integer(1), false), (FakeValue::Unit, true)]
        );
    });
}

#[test]
fn argument_checkpoint_abort_precedes_provenance_and_binding() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("f");
    let mut editor = rue_rir::RirEditor::new();
    let first = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let later = editor.add_inst(rue_rir::Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 0),
    });
    let call = editor
        .add_call(
            symbol,
            &[
                rue_rir::RirCallArg {
                    value: first,
                    mode: rue_rir::RirArgMode::Normal,
                },
                rue_rir::RirCallArg {
                    value: later,
                    mode: rue_rir::RirArgMode::Normal,
                },
            ],
            Span::new(0, 0),
        )
        .unwrap();
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_call_argument_observations();
    // Checkpoint 1 enters the call; checkpoint 2 is the first argument.
    // The abort must happen before that argument's provenance lookup,
    // binding, or the later argument's evaluation.
    configure_checkpoint_abort(Some(2));
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let mut engine = ComptimeEngine::new(&mut host);
    let result = engine.evaluate(ComptimeFrame::expression(0, call), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Abort(FakeFailure::Canceled)
    ));
    assert_eq!(checkpoint_count(), 2);
    assert_eq!(engine.provenance_classification_count(), 0);
    CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));
    assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
    assert_eq!(PREPARE_CALLS.with(Cell::get), 0);
    configure_checkpoint_abort(None);
}

#[test]
fn incremental_binding_rejects_before_evaluating_later_arguments() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("f");
    let mut editor = rue_rir::RirEditor::new();
    let first = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(7),
        span: Span::new(0, 0),
    });
    let later_trap = editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("2.0"),
        },
        span: Span::new(0, 0),
    });
    let call = editor
        .add_call(
            symbol,
            &[
                rue_rir::RirCallArg {
                    value: first,
                    mode: rue_rir::RirArgMode::Normal,
                },
                rue_rir::RirCallArg {
                    value: later_trap,
                    mode: rue_rir::RirArgMode::Normal,
                },
            ],
            Span::new(0, 0),
        )
        .unwrap();
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_call_argument_observations();
    REJECT_BIND_AT.with(|rejected| rejected.set(Some(0)));
    configure_checkpoint_abort(None);
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
    assert!(matches!(result, ComptimeOutcome::RuntimeDependent));
    assert_eq!(host.float_evaluations.get(), 0);
    CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));
    assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
    assert_eq!(PREPARE_CALLS.with(Cell::get), 0);
    clear_call_argument_observations();
}

#[test]
fn ordinary_binding_shape_mismatch_does_not_mask_later_terminal() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("f");
    let type_symbol = interner.get_or_intern("i32");
    let mut editor = rue_rir::RirEditor::new();
    let type_syntax = editor.add_named_type(type_symbol).unwrap();
    // A type value is deliberately supplied where an ordinary value
    // parameter would reject it. Ordinary binding stores that shape in
    // its owned transaction; the later terminal must still win before
    // finish performs whole-batch validation.
    let invalid_for_value = editor.add_inst(rue_rir::Inst {
        data: InstData::TypeConst {
            type_name: type_syntax,
        },
        span: Span::new(0, 0),
    });
    let later_terminal = editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("2.0"),
        },
        span: Span::new(0, 0),
    });
    let call = editor
        .add_call(
            symbol,
            &[
                rue_rir::RirCallArg {
                    value: invalid_for_value,
                    mode: rue_rir::RirArgMode::Normal,
                },
                rue_rir::RirCallArg {
                    value: later_terminal,
                    mode: rue_rir::RirArgMode::Normal,
                },
            ],
            Span::new(0, 0),
        )
        .unwrap();
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(type_symbol),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_call_argument_observations();
    configure_checkpoint_abort(None);
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, call), &mut env);
    assert!(matches!(result, ComptimeOutcome::HostFailure(FAKE_FAILURE)));
    assert_eq!(host.float_evaluations.get(), 1);
    CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));
    assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
    assert_eq!(PREPARE_CALLS.with(Cell::get), 0);
}

#[test]
fn admission_rejection_and_argument_terminal_stop_before_binding_or_later_args() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("f");
    let mut rejected_editor = rue_rir::RirEditor::new();
    let trapped = rejected_editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("1.0"),
        },
        span: Span::new(0, 3),
    });
    let rejected_call = rejected_editor
        .add_call(
            symbol,
            &[rue_rir::RirCallArg {
                value: trapped,
                mode: rue_rir::RirArgMode::Normal,
            }],
            Span::new(0, 3),
        )
        .unwrap();
    let mut rejected_host = FakeHost {
        programs: vec![rejected_editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_call_argument_observations();
    REJECT_ADMISSION.with(|rejected| rejected.set(true));
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result = ComptimeEngine::new(&mut rejected_host)
        .evaluate(ComptimeFrame::expression(0, rejected_call), &mut env);
    assert!(matches!(result, ComptimeOutcome::RuntimeDependent));
    assert_eq!(rejected_host.float_evaluations.get(), 0);
    CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));

    let mut terminal_editor = rue_rir::RirEditor::new();
    let first = terminal_editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let terminal = terminal_editor.add_inst(rue_rir::Inst {
        data: InstData::FloatConst {
            text: interner.get_or_intern("2.0"),
        },
        span: Span::new(0, 3),
    });
    let later = terminal_editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(3),
        span: Span::new(0, 0),
    });
    let terminal_call = terminal_editor
        .add_call(
            symbol,
            &[
                rue_rir::RirCallArg {
                    value: first,
                    mode: rue_rir::RirArgMode::Normal,
                },
                rue_rir::RirCallArg {
                    value: terminal,
                    mode: rue_rir::RirArgMode::Normal,
                },
                rue_rir::RirCallArg {
                    value: later,
                    mode: rue_rir::RirArgMode::Normal,
                },
            ],
            Span::new(0, 3),
        )
        .unwrap();
    let mut terminal_host = FakeHost {
        programs: vec![terminal_editor.finish()],
        type_symbol: SymbolHandle::new(interner.get_or_intern("T2")),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    clear_call_argument_observations();
    configure_checkpoint_abort(None);
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result = ComptimeEngine::new(&mut terminal_host)
        .evaluate(ComptimeFrame::expression(0, terminal_call), &mut env);
    assert!(matches!(result, ComptimeOutcome::HostFailure(FAKE_FAILURE)));
    assert_eq!(terminal_host.float_evaluations.get(), 1);
    assert_eq!(checkpoint_count(), 3);
    CALL_ARGUMENTS.with(|observed| assert!(observed.borrow().is_empty()));
    assert_eq!(BINDING_FINISHES.with(Cell::get), 0);
    assert_eq!(PREPARE_CALLS.with(Cell::get), 0);
}

#[test]
fn entered_programs_switch_on_colliding_refs_and_resume_the_parent() {
    let (mut host, root, rhs, base) = call_fixture();
    PRODUCER_CALLS.with(|calls| calls.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let (value, resumed) = {
        let mut engine = ComptimeEngine::new(&mut host);
        let value = engine
            .evaluate(ComptimeFrame::expression(0, root), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap();
        // A second root evaluation proves the parent frame was popped
        // after the child program returned; no ambient program or stack
        // state leaks.
        let resumed = engine
            .evaluate(ComptimeFrame::expression(0, rhs), &mut env)
            .into_result(|_| FAKE_FAILURE)
            .unwrap();
        (value, resumed)
    };
    assert_eq!(value, Some(FakeValue::Integer(22)));
    assert_eq!(
        host.finished,
        vec![(2, Some(FakeType(7))), (1, Some(FakeType(7)))]
    );
    assert_eq!(resumed, Some(FakeValue::Integer(2)));
    PRODUCER_CALLS.with(|calls| {
        assert_eq!(
            calls.borrow().as_slice(),
            &[(1, 1, base), (2, 2, base + 2000)]
        );
    });
}

#[test]
fn ordered_same_program_calls_keep_distinct_tickets_in_lifo_order() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("nested");
    let symbol_handle = SymbolHandle::new(symbol);
    let mut root = rue_rir::RirEditor::new();
    let root_call = root.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let mut child = rue_rir::RirEditor::new();
    let nested_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let outer_rhs = child.add_inst(rue_rir::Inst {
        data: InstData::IntConst(2),
        span: Span::new(0, 0),
    });
    let _type_hint_probe = child.add_inst(rue_rir::Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 0),
    });
    let outer_add = child.add_inst(rue_rir::Inst {
        data: InstData::Add {
            lhs: nested_call,
            rhs: outer_rhs,
        },
        span: Span::new(0, 0),
    });
    let inner_lhs = child.add_inst(rue_rir::Inst {
        data: InstData::IntConst(3),
        span: Span::new(0, 0),
    });
    let inner_rhs = child.add_inst(rue_rir::Inst {
        data: InstData::IntConst(4),
        span: Span::new(0, 0),
    });
    let inner_add = child.add_inst(rue_rir::Inst {
        data: InstData::Add {
            lhs: inner_lhs,
            rhs: inner_rhs,
        },
        span: Span::new(0, 0),
    });
    let mut host = FakeHost {
        programs: vec![root.finish(), child.finish()],
        type_symbol: symbol_handle,
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: Some((2, outer_add, inner_add, None)),
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    PRODUCER_CALLS.with(|calls| calls.borrow_mut().clear());
    TICKET_EVENTS.with(|events| events.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root_call), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Known(FakeValue::Integer(9))
    ));
    TICKET_EVENTS.with(|events| {
        assert_eq!(
            *events.borrow(),
            vec![(1, true), (2, true), (2, false), (1, false)]
        );
    });
    PRODUCER_CALLS.with(|calls| {
        let calls = calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, 1);
        assert_eq!(calls[1].0, 1);
        assert_eq!(calls[0].1, 1);
        assert_eq!(calls[1].1, 2);
        assert_ne!(calls[0].2, calls[1].2);
    });
}

#[test]
fn nested_expected_integer_contexts_restore_for_the_parent_and_root() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("typed_nested");
    let symbol_handle = SymbolHandle::new(symbol);
    let mut root = rue_rir::RirEditor::new();
    let root_call = root.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let root_value = root.add_inst(rue_rir::Inst {
        data: InstData::IntConst(9),
        span: Span::new(0, 0),
    });
    let mut child = rue_rir::RirEditor::new();
    let nested_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let outer_rhs = child.add_inst(rue_rir::Inst {
        data: InstData::IntConst(2),
        span: Span::new(0, 0),
    });
    let _type_hint_probe = child.add_inst(rue_rir::Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 0),
    });
    let outer_add = child.add_inst(rue_rir::Inst {
        data: InstData::Add {
            lhs: nested_call,
            rhs: outer_rhs,
        },
        span: Span::new(0, 0),
    });
    let inner_lhs = child.add_inst(rue_rir::Inst {
        data: InstData::IntConst(3),
        span: Span::new(0, 0),
    });
    let inner_rhs = child.add_inst(rue_rir::Inst {
        data: InstData::IntConst(4),
        span: Span::new(0, 0),
    });
    let inner_add = child.add_inst(rue_rir::Inst {
        data: InstData::Add {
            lhs: inner_lhs,
            rhs: inner_rhs,
        },
        span: Span::new(0, 0),
    });
    let mut host = FakeHost {
        programs: vec![root.finish(), child.finish()],
        type_symbol: symbol_handle,
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: Some((2, outer_add, inner_add, None)),
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    INTEGER_HINTS.with(|hints| hints.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root_call), &mut env);
    assert!(matches!(
        result,
        ComptimeOutcome::Known(FakeValue::Integer(9))
    ));
    INTEGER_HINTS.with(|hints| {
        assert_eq!(*hints.borrow(), vec![Some(FakeType(8)), Some(FakeType(7))]);
    });
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root_value), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(9))
    ));
    INTEGER_HINTS
        .with(|hints| assert_eq!(*hints.borrow(), vec![Some(FakeType(8)), Some(FakeType(7))]));
}

#[test]
fn nested_child_and_parent_diagnostics_use_the_active_program_in_one_evaluation() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("diagnostic_child");
    let symbol_handle = SymbolHandle::new(symbol);
    let base = symbol_handle.issuing_interner_ordinal() as u32;

    let mut root = RirEditor::new();
    let call = root.add_call(symbol, &[], Span::new(1, 2)).unwrap();
    let root_rhs = root.add_inst(Inst {
        data: InstData::IntConst(2),
        span: Span::new(2, 3),
    });
    let root_add = root.add_inst(Inst {
        data: InstData::Add {
            lhs: call,
            rhs: root_rhs,
        },
        span: Span::new(3, 4),
    });

    let mut child = RirEditor::new();
    let child_lhs = child.add_inst(Inst {
        data: InstData::IntConst(4),
        span: Span::new(10, 11),
    });
    let child_rhs = child.add_inst(Inst {
        data: InstData::IntConst(5),
        span: Span::new(11, 12),
    });
    let child_add = child.add_inst(Inst {
        data: InstData::Add {
            lhs: child_lhs,
            rhs: child_rhs,
        },
        span: Span::new(12, 13),
    });
    let mut call_plans = AHashMap::new();
    call_plans.insert(
        base,
        FakePreparedCall::Enter {
            program: 1,
            body: child_add,
            expected: None,
            name_bindings: AHashMap::new(),
        },
    );
    let mut host = FakeHost {
        programs: vec![root.finish(), child.finish()],
        type_symbol: symbol_handle,
        constant: None,
        dependencies: Vec::new(),
        call_plans,
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    DIAGNOSTIC_SITES.with(|sites| sites.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root_add), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(11))
    ));
    DIAGNOSTIC_SITES.with(|sites| {
        let programs = sites
            .borrow()
            .iter()
            .map(|(program, _, _)| *program)
            .collect::<Vec<_>>();
        assert_eq!(programs, vec![1, 0]);
    });
}

#[test]
fn entered_frames_use_the_real_64_frame_budget_and_prepared_outcomes_remain_ticket_free() {
    let mut editor = rue_rir::RirEditor::new();
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("loop");
    let root_call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let mut child = rue_rir::RirEditor::new();
    let child_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let terminal = child.add_inst(rue_rir::Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let symbol_handle = SymbolHandle::new(symbol);
    let host_base = FakeHost {
        programs: vec![editor.finish(), child.finish()],
        type_symbol: symbol_handle,
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: Some((MAX_COMPTIME_CALL_DEPTH, child_call, terminal, None)),
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut host = host_base;
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let mut engine = ComptimeEngine::new(&mut host);
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, root_call), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(1))
    ));
    assert_eq!(host.enter_count, MAX_COMPTIME_CALL_DEPTH);

    let mut editor = rue_rir::RirEditor::new();
    let root_call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let mut child = rue_rir::RirEditor::new();
    let child_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let mut host = FakeHost {
        programs: vec![editor.finish(), child.finish()],
        type_symbol: SymbolHandle::new(symbol),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: Some((MAX_COMPTIME_CALL_DEPTH, child_call, child_call, None)),
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut engine = ComptimeEngine::new(&mut host);
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    TICKET_EVENTS.with(|events| events.borrow_mut().clear());
    DIAGNOSTIC_SITES.with(|sites| sites.borrow_mut().clear());
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, root_call), &mut env),
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));
    assert_eq!(host.enter_count, MAX_COMPTIME_CALL_DEPTH + 2);
    TICKET_EVENTS.with(|events| {
        assert_eq!(events.borrow().len(), (MAX_COMPTIME_CALL_DEPTH + 1) * 2);
    });
    DIAGNOSTIC_SITES.with(|sites| {
        assert_eq!(
            sites.borrow().as_slice(),
            &[(1, 0, 0)],
            "depth rejection uses the rejected child program"
        );
    });

    let mut editor = rue_rir::RirEditor::new();
    let root_call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let mut child = rue_rir::RirEditor::new();
    let child_call = child.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let mut host = FakeHost {
        programs: vec![editor.finish(), child.finish()],
        type_symbol: SymbolHandle::new(symbol),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: Some((
            MAX_COMPTIME_CALL_DEPTH,
            child_call,
            child_call,
            Some(MAX_COMPTIME_CALL_DEPTH),
        )),
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut engine = ComptimeEngine::new(&mut host);
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, root_call), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(1))
    ));
    assert_eq!(host.enter_count, MAX_COMPTIME_CALL_DEPTH);
}

#[test]
fn typed_outcomes_survive_enter_finish_and_memoized_calls_cleanup_frames() {
    let run_enter = |finish_outcome| {
        let interner = lasso::ThreadedRodeo::new();
        let symbol = interner.get_or_intern("f");
        let symbol_handle = SymbolHandle::new(symbol);
        let base = symbol_handle.issuing_interner_ordinal() as u32;
        let mut editor = rue_rir::RirEditor::new();
        let call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
        let direct = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 0),
        });
        let mut child = rue_rir::RirEditor::new();
        let child_body = child.add_inst(rue_rir::Inst {
            data: InstData::IntConst(4),
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            programs: vec![editor.finish(), child.finish()],
            type_symbol: symbol_handle,
            constant: None,
            dependencies: Vec::new(),
            call_plans: AHashMap::from([(
                base,
                FakePreparedCall::Enter {
                    program: 1,
                    body: child_body,
                    expected: Some(FakeType(7)),
                    name_bindings: AHashMap::new(),
                },
            )]),
            recursive: None,
            enter_count: 0,
            finish_outcome,
            finished: Vec::new(),
            float_evaluations: Cell::new(0),
        };
        let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
        let (result, resumed) = {
            let mut engine = ComptimeEngine::new(&mut host);
            let result = engine.evaluate(ComptimeFrame::expression(0, call), &mut env);
            let resumed = engine.evaluate(ComptimeFrame::expression(0, direct), &mut env);
            (result, resumed)
        };
        (result, resumed, host.finished.len())
    };

    assert!(matches!(
        run_enter(FakeFinishOutcome::RuntimeDependent).0,
        ComptimeOutcome::RuntimeDependent
    ));
    assert!(matches!(
        run_enter(FakeFinishOutcome::NotReady).0,
        ComptimeOutcome::NotReady
    ));
    assert!(matches!(
        run_enter(FakeFinishOutcome::UnsupportedContext).0,
        ComptimeOutcome::UnsupportedContext
    ));
    assert!(matches!(
        run_enter(FakeFinishOutcome::Trap).0,
        ComptimeOutcome::Trap(_)
    ));
    assert!(matches!(
        run_enter(FakeFinishOutcome::HostFailure).0,
        ComptimeOutcome::HostFailure(_)
    ));
    let (abort, resumed, finished) = run_enter(FakeFinishOutcome::Abort);
    assert!(matches!(abort, ComptimeOutcome::Abort(_)));
    assert!(matches!(
        resumed,
        ComptimeOutcome::Known(FakeValue::Integer(3))
    ));
    assert_eq!(finished, 1);

    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("memoized");
    let symbol_handle = SymbolHandle::new(symbol);
    let base = symbol_handle.issuing_interner_ordinal() as u32;
    let mut editor = rue_rir::RirEditor::new();
    let call = editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let direct = editor.add_inst(rue_rir::Inst {
        data: InstData::IntConst(5),
        span: Span::new(0, 0),
    });
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: symbol_handle,
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::from([(base, FakePreparedCall::Memoized(ComptimeOutcome::NotReady))]),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let (memoized, resumed) = {
        let mut engine = ComptimeEngine::new(&mut host);
        let memoized = engine.evaluate(ComptimeFrame::expression(0, call), &mut env);
        let resumed = engine.evaluate(ComptimeFrame::expression(0, direct), &mut env);
        (memoized, resumed)
    };
    assert!(matches!(memoized, ComptimeOutcome::NotReady));
    assert!(matches!(
        resumed,
        ComptimeOutcome::Known(FakeValue::Integer(5))
    ));
    assert!(host.finished.is_empty());
}

#[test]
fn rejected_calls_never_activate_or_finish_their_ticket() {
    let (mut host, root, rhs, base) = call_fixture();
    host.finish_outcome = FakeFinishOutcome::CanonicalFailure;
    PRODUCER_CALLS.with(|calls| calls.borrow_mut().clear());
    TICKET_EVENTS.with(|events| events.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let mut engine = ComptimeEngine::new(&mut host);
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, root), &mut env),
        ComptimeOutcome::HostFailure(FAKE_FAILURE)
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, rhs), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(2))
    ));
    drop(engine);
    assert!(host.finished.is_empty());
    TICKET_EVENTS.with(|events| assert!(events.borrow().is_empty()));
    PRODUCER_CALLS.with(|calls| {
        assert_eq!(calls.borrow().as_slice(), &[(1, 1, base)]);
    });
}

#[test]
fn depth_rejection_precedes_prelookup_canonicalization_failure() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("deep");
    let mut root = rue_rir::RirEditor::new();
    let root_call = root.add_call(symbol, &[], Span::new(13, 19)).unwrap();
    let mut child = rue_rir::RirEditor::new();
    let child_call = child.add_call(symbol, &[], Span::new(23, 29)).unwrap();
    let mut host = FakeHost {
        programs: vec![root.finish(), child.finish()],
        type_symbol: SymbolHandle::new(symbol),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: Some((MAX_COMPTIME_CALL_DEPTH + 2, child_call, child_call, None)),
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    PREPARE_CANONICAL_PROBE.with(|enabled| enabled.set(true));
    CANONICAL_FAILURE_AFTER.with(|after| after.set(Some(MAX_COMPTIME_CALL_DEPTH + 2)));
    DEPTH_FAILURE_VARIANT.with(|enabled| enabled.set(true));
    DIAGNOSTIC_SITES.with(|sites| sites.borrow_mut().clear());
    PRODUCER_CALLS.with(|calls| calls.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root_call), &mut env);
    PREPARE_CANONICAL_PROBE.with(|enabled| enabled.set(false));
    CANONICAL_FAILURE_AFTER.with(|after| after.set(None));
    DEPTH_FAILURE_VARIANT.with(|enabled| enabled.set(false));
    assert!(matches!(
        result,
        ComptimeOutcome::HostFailure(FakeFailure::DepthExceeded)
    ));
    DIAGNOSTIC_SITES.with(|sites| {
        assert_eq!(sites.borrow().as_slice(), &[(1, 0, 0)]);
    });
    PRODUCER_CALLS.with(|calls| {
        assert!(calls.borrow().len() >= MAX_COMPTIME_CALL_DEPTH);
    });
}

#[test]
fn unnamed_enter_is_rejected_before_ticket_lifecycle() {
    let (mut host, root, rhs, base) = call_fixture();
    host.call_plans.insert(
        base,
        FakePreparedCall::UnnamedEnter {
            program: 1,
            body: InstRef::from_raw(0),
        },
    );
    TICKET_EVENTS.with(|events| events.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result =
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, root), &mut env);
    assert!(matches!(result, ComptimeOutcome::UnsupportedContext));
    assert!(host.finished.is_empty());
    TICKET_EVENTS.with(|events| assert!(events.borrow().is_empty()));

    // The invalid preparation did not leave a frame on the stack.
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(ComptimeFrame::expression(0, rhs), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(2))
    ));
}

#[test]
fn named_frames_cannot_bypass_the_entered_call_lifecycle() {
    let (mut host, _root, _rhs, _base) = call_fixture();
    TICKET_EVENTS.with(|events| events.borrow_mut().clear());
    let mut frame = ComptimeFrame::expression(0, InstRef::from_raw(0));
    frame.name = Some(FakeName { ordinal: 99 });
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let result = ComptimeEngine::new(&mut host).evaluate(frame, &mut env);
    assert!(matches!(result, ComptimeOutcome::UnsupportedContext));
    assert!(host.finished.is_empty());
    TICKET_EVENTS.with(|events| assert!(events.borrow().is_empty()));
}

#[test]
fn anonymous_method_decoder_rejects_non_function_entries_exactly() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("method");
    let mut editor = RirEditor::new();
    let non_function = editor.add_inst(Inst {
        data: InstData::IntConst(1),
        span: Span::new(4, 5),
    });
    let root = editor
        .add_anon_struct_type(
            &[],
            &[non_function],
            rue_rir::RirStructuralAnchor::new(Vec::new()),
            Span::new(0, 1),
        )
        .unwrap();
    let rir = editor.finish();
    let methods = match &rir.get(root).data {
        InstData::AnonStructType { methods, .. } => methods.clone(),
        _ => unreachable!(),
    };
    let mut host = FakeHost {
        programs: vec![rir],
        type_symbol: SymbolHandle::new(symbol),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    METHOD_FAILURES.with(|failures| failures.borrow_mut().clear());
    let result = ComptimeEngine::new(&mut host).decode_anon_method_descriptors(
        &0,
        &methods,
        &AHashMap::new(),
        &AHashMap::new(),
    );
    assert!(matches!(
        result,
        ComptimeOutcome::HostFailure(FakeFailure::NonFunctionMethod)
    ));
    METHOD_FAILURES.with(|failures| assert_eq!(*failures.borrow(), vec!["non_function"]));
}

#[test]
fn own_comptime_type_parameter_wins_before_later_type_resolution() {
    let interner = lasso::ThreadedRodeo::new();
    let method_name = interner.get_or_intern("method");
    let type_name = interner.get_or_intern("type");
    let mut editor = RirEditor::new();
    let type_syntax = editor.add_named_type(type_name).unwrap();
    let body = editor.add_inst(Inst {
        data: InstData::UnitConst,
        span: Span::new(10, 11),
    });
    let bounds = editor.add_parameter_bounds(&[]).unwrap();
    let method = editor
        .add_fn_decl(
            &[],
            false,
            false,
            false,
            false,
            method_name,
            &[rue_rir::RirParam {
                name: type_name,
                ty: type_syntax,
                mode: rue_rir::RirParamMode::Normal,
                is_comptime: true,
                span: Span::new(12, 13),
                bounds,
            }],
            type_syntax,
            body,
            false,
            rue_rir::RirParamMode::Normal,
            false,
            false,
            Span::new(8, 9),
        )
        .unwrap();
    let root = editor
        .add_anon_struct_type(
            &[],
            &[method],
            rue_rir::RirStructuralAnchor::new(Vec::new()),
            Span::new(0, 1),
        )
        .unwrap();
    let rir = editor.finish();
    let methods = match &rir.get(root).data {
        InstData::AnonStructType { methods, .. } => methods.clone(),
        _ => unreachable!(),
    };
    let mut host = FakeHost {
        programs: vec![rir],
        type_symbol: SymbolHandle::new(type_name),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    METHOD_FAILURES.with(|failures| failures.borrow_mut().clear());
    TYPE_RESOLUTION_CALLS.with(|calls| calls.set(0));
    let result = ComptimeEngine::new(&mut host).decode_anon_method_descriptors(
        &0,
        &methods,
        &AHashMap::new(),
        &AHashMap::new(),
    );
    assert!(matches!(
        result,
        ComptimeOutcome::HostFailure(FakeFailure::OwnComptimeTypeParameter)
    ));
    METHOD_FAILURES.with(|failures| assert_eq!(*failures.borrow(), vec!["own_type"]));
    TYPE_RESOLUTION_CALLS.with(|calls| assert_eq!(calls.get(), 0));
}

#[test]
fn expected_integer_context_is_frame_local_and_integer_only() {
    let mut editor = RirEditor::new();
    let lhs = editor.add_inst(Inst {
        data: InstData::IntConst(1),
        span: Span::new(0, 0),
    });
    let rhs = editor.add_inst(Inst {
        data: InstData::IntConst(2),
        span: Span::new(0, 0),
    });
    let _unused = editor.add_inst(Inst {
        data: InstData::UnitConst,
        span: Span::new(0, 0),
    });
    let add = editor.add_inst(Inst {
        data: InstData::Add { lhs, rhs },
        span: Span::new(0, 0),
    });
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("context");
    let mut host = FakeHost {
        programs: vec![editor.finish()],
        type_symbol: SymbolHandle::new(symbol),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::Identity,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    env.expected_result = Some(FakeType(3));
    INTEGER_HINTS.with(|hints| hints.borrow_mut().clear());
    let frame = ComptimeFrame {
        program: 0,
        body: add,
        name: None,
        context: None,
        span: Span::new(0, 0),
        function_span: Span::new(0, 0),
        type_bindings: AHashMap::new(),
        value_bindings: AHashMap::new(),
        name_bindings: AHashMap::new(),
        call_identity: None,
        expected_result: Some(FakeType(16)),
    };
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(frame, &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(3))
    ));
    assert_eq!(env.expected_result, Some(FakeType(3)));
    INTEGER_HINTS.with(|hints| assert_eq!(*hints.borrow(), vec![Some(FakeType(16))]));

    INTEGER_HINTS.with(|hints| hints.borrow_mut().clear());
    let non_integer_frame = ComptimeFrame {
        program: 0,
        body: add,
        name: None,
        context: None,
        span: Span::new(0, 0),
        function_span: Span::new(0, 0),
        type_bindings: AHashMap::new(),
        value_bindings: AHashMap::new(),
        name_bindings: AHashMap::new(),
        call_identity: None,
        expected_result: Some(FakeType(99)),
    };
    assert!(matches!(
        ComptimeEngine::new(&mut host).evaluate(non_integer_frame, &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(3))
    ));
    INTEGER_HINTS.with(|hints| assert_eq!(*hints.borrow(), vec![None]));
}

#[test]
fn host_abort_channel_cleans_entered_frames_and_preserves_labels() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("abort");
    let symbol_handle = SymbolHandle::new(symbol);
    let base = symbol_handle.issuing_interner_ordinal() as u32;

    let mut root_editor = RirEditor::new();
    let call = root_editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let direct = root_editor.add_inst(Inst {
        data: InstData::IntConst(9),
        span: Span::new(0, 0),
    });
    let mut child_editor = RirEditor::new();
    let lhs = child_editor.add_inst(Inst {
        data: InstData::IntConst(2),
        span: Span::new(0, 0),
    });
    let rhs = child_editor.add_inst(Inst {
        data: InstData::IntConst(3),
        span: Span::new(0, 0),
    });
    let child_body = child_editor.add_inst(Inst {
        data: InstData::Add { lhs, rhs },
        span: Span::new(0, 0),
    });

    let mut host = FakeHost {
        programs: vec![root_editor.finish(), child_editor.finish()],
        type_symbol: symbol_handle,
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::from([(
            base,
            FakePreparedCall::Enter {
                program: 1,
                body: child_body,
                expected: Some(FakeType(7)),
                name_bindings: AHashMap::new(),
            },
        )]),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::AbortFromArithmetic,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    LABEL_CALLS.with(|calls| calls.set(0));
    TICKET_EVENTS.with(|events| events.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let mut engine = ComptimeEngine::new(&mut host);
    let aborted = engine.evaluate(ComptimeFrame::expression(0, call), &mut env);
    assert!(matches!(aborted, ComptimeOutcome::Abort(FAKE_FAILURE)));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, direct), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(9))
    ));
    drop(engine);
    assert_eq!(host.finished, vec![(1, Some(FakeType(7)))]);
    LABEL_CALLS.with(|calls| assert_eq!(calls.get(), 0));
    TICKET_EVENTS.with(|events| assert_eq!(*events.borrow(), vec![(1, true), (1, false)]));

    let mut root_editor = RirEditor::new();
    let call = root_editor.add_call(symbol, &[], Span::new(0, 0)).unwrap();
    let direct = root_editor.add_inst(Inst {
        data: InstData::IntConst(11),
        span: Span::new(0, 0),
    });
    let mut host = FakeHost {
        programs: vec![root_editor.finish()],
        type_symbol: SymbolHandle::new(symbol),
        constant: None,
        dependencies: Vec::new(),
        call_plans: AHashMap::new(),
        recursive: None,
        enter_count: 0,
        finish_outcome: FakeFinishOutcome::AbortFromPrepare,
        finished: Vec::new(),
        float_evaluations: Cell::new(0),
    };
    TICKET_EVENTS.with(|events| events.borrow_mut().clear());
    let mut env = ComptimeEnv::<FakeValue, FakeType, FakeName, FakeFile, FakeIdentity>::new();
    let mut engine = ComptimeEngine::new(&mut host);
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, call), &mut env),
        ComptimeOutcome::Abort(FAKE_FAILURE)
    ));
    assert!(matches!(
        engine.evaluate(ComptimeFrame::expression(0, direct), &mut env),
        ComptimeOutcome::Known(FakeValue::Integer(11))
    ));
    drop(engine);
    assert!(host.finished.is_empty());
    TICKET_EVENTS.with(|events| assert!(events.borrow().is_empty()));
}

fn registry_program(value: u64) -> (Arc<ValidatedRir>, InstRef) {
    let mut editor = RirEditor::new();
    let root = editor.add_inst(Inst {
        data: InstData::IntConst(value),
        span: Span::new(0, 0),
    });
    let context = RirValidationContext {
        symbol_count: 0,
        source_lengths: &[(rue_span::FileId::DEFAULT, 1)],
    };
    (
        Arc::new(ValidatedRir::finish(editor, &context).expect("valid test RIR")),
        root,
    )
}

#[test]
fn registry_keeps_colliding_instruction_refs_program_local() {
    let (first_rir, first_ref) = registry_program(11);
    let (second_rir, second_ref) = registry_program(22);
    assert_eq!(first_ref, second_ref);
    let mut registry = ComptimeProgramRegistry::<u8, u8, u8, u8>::new();
    registry
        .register(
            ComptimeProgramKey {
                declaration: 1,
                configuration: 10,
            },
            ComptimeProgram {
                rir: first_rir,
                symbols: Arc::from([]),
                imports: 0,
            },
        )
        .unwrap();
    registry
        .register(
            ComptimeProgramKey {
                declaration: 2,
                configuration: 20,
            },
            ComptimeProgram {
                rir: second_rir,
                symbols: Arc::from([2]),
                imports: 22,
            },
        )
        .unwrap();
    assert_eq!(registry.len(), 2);
    assert!(!registry.is_empty());
    assert_eq!(
        registry
            .get(&ComptimeProgramKey {
                declaration: 1,
                configuration: 10,
            })
            .unwrap()
            .rir
            .get(first_ref)
            .data,
        InstData::IntConst(11)
    );
    assert_eq!(
        registry
            .get(&ComptimeProgramKey {
                declaration: 2,
                configuration: 20,
            })
            .unwrap()
            .rir
            .get(second_ref)
            .data,
        InstData::IntConst(22)
    );
    let second = registry
        .get(&ComptimeProgramKey {
            declaration: 2,
            configuration: 20,
        })
        .unwrap();
    assert_eq!(&*second.symbols, &[2]);
    assert_eq!(second.imports, 22);
    assert_eq!(
        registry.register(
            ComptimeProgramKey {
                declaration: 1,
                configuration: 10,
            },
            ComptimeProgram {
                rir: registry_program(99).0,
                symbols: Arc::from([]),
                imports: 0,
            },
        ),
        Err(ComptimeProgramRegistrationError::AlreadyRegistered)
    );
}

#[test]
fn registry_admits_exact_structured_arena_and_matching_symbol_authority() {
    let interner = lasso::ThreadedRodeo::new();
    let symbol = interner.get_or_intern("T");
    let mut editor = RirEditor::new();
    let root = editor.add_named_type(symbol).expect("named type syntax");
    let symbol_index = symbol.into_usize();
    let symbol_count = symbol_index + 1;
    let context = RirValidationContext {
        symbol_count,
        source_lengths: &[(rue_span::FileId::DEFAULT, 1)],
    };
    let rir = Arc::new(ValidatedRir::finish(editor, &context).expect("valid structured RIR"));
    let mut symbols = vec![Arc::<str>::from(""); symbol_count];
    symbols[symbol_index] = Arc::from("T");
    let key = ComptimeProgramKey {
        declaration: 7_u8,
        configuration: 9_u8,
    };
    let mut registry = ComptimeProgramRegistry::<u8, u8, Arc<str>, ()>::new();
    registry
        .register(
            key.clone(),
            ComptimeProgram {
                rir: Arc::clone(&rir),
                symbols: Arc::from(symbols),
                imports: (),
            },
        )
        .unwrap();
    assert!(
        registry
            .structured_type_authority(&key, "scope", root)
            .is_some()
    );
    let bad_key = ComptimeProgramKey {
        declaration: 8_u8,
        configuration: 9_u8,
    };
    registry
        .register(
            bad_key.clone(),
            ComptimeProgram {
                rir,
                symbols: Arc::from([]),
                imports: (),
            },
        )
        .unwrap();
    assert!(
        registry
            .structured_type_authority(&bad_key, "scope", root)
            .is_none()
    );
    let richer = registry
        .structured_type_authority_with_program(&key, "richer", "scope", root)
        .expect("richer identity uses the registered arena");
    assert_eq!(richer.program(), &"richer");
    assert!(
        registry
            .structured_type_authority(&key, "scope", rue_rir::RirTypeSyntaxRef::from_u32(99))
            .is_none()
    );
}

#[test]
fn completed_memo_distinguishes_callable_target_and_ordered_args() {
    // The declaration tuple stands in for the real producer's callable,
    // imported-module, and generic-identity components. Keeping those
    // components in one exact field catches counterfeit near-collision
    // keys without depending on a particular issuer's token numbers.
    type Memo = ComptimeCompletedCallMemo<(u8, u8, u8), u8, u8, u8, u8>;
    let key = |declaration, configuration, types: &[u8], values: &[u8]| ComptimeCallKey {
        declaration,
        configuration,
        type_arguments: Arc::from(types),
        value_arguments: Arc::from(values),
    };
    let mut memo = Memo::new();
    let base = key((7, 11, 13), 3, &[1, 2], &[3, 4]);
    assert!(matches!(memo.lookup(&base), ComptimeCallMemoLookup::Miss));
    memo.insert(base.clone(), ComptimeMemoizedOutcome::NotReady)
        .unwrap();
    assert!(matches!(
        memo.lookup(&base),
        ComptimeCallMemoLookup::Memoized(ComptimeMemoizedOutcome::NotReady)
    ));
    assert!(matches!(
        memo.lookup(&key((8, 11, 13), 3, &[1, 2], &[3, 4])),
        ComptimeCallMemoLookup::Miss
    ));
    assert!(matches!(
        memo.lookup(&key((7, 11, 14), 3, &[1, 2], &[3, 4])),
        ComptimeCallMemoLookup::Miss
    ));
    assert!(matches!(
        memo.lookup(&key((7, 11, 13), 4, &[1, 2], &[3, 4])),
        ComptimeCallMemoLookup::Miss
    ));
    assert!(matches!(
        memo.lookup(&key((7, 11, 13), 3, &[2, 1], &[3, 4])),
        ComptimeCallMemoLookup::Miss
    ));
    assert!(matches!(
        memo.lookup(&key((7, 11, 13), 3, &[1, 2], &[4, 3])),
        ComptimeCallMemoLookup::Miss
    ));
    assert_eq!(memo.len(), 1);
    assert!(!memo.is_empty());
    assert_eq!(
        memo.insert(base, ComptimeMemoizedOutcome::Known(9)),
        Err(ComptimeMemoInsertError::AlreadyMemoized)
    );
    let trap_key = key((7, 11, 13), 3, &[1, 2], &[5]);
    let trap = ComptimeTrap {
        operation: "division by zero",
        span: Span::new(0, 0),
    };
    memo.insert(trap_key.clone(), ComptimeMemoizedOutcome::Trap(trap))
        .unwrap();
    assert!(matches!(
        memo.lookup(&trap_key),
        ComptimeCallMemoLookup::Memoized(ComptimeMemoizedOutcome::Trap(value))
            if *value == trap
    ));
}
