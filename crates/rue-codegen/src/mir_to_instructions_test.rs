//! Tests for MIR to Instruction lowering

use crate::mir_to_instructions::MirToInstructions;
use crate::{BinOp, Instruction, Value};
use rue_ir::mir::*;
use rue_ir::types::RueType;
use rue_lexer::Span;

/// Helper to create a simple MIR program with one function
fn make_simple_program(blocks: Vec<BasicBlock>) -> MirProgram {
    MirProgram {
        functions: vec![MirFunction {
            name: "test".to_string(),
            params: vec![],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            temp_types: std::collections::HashMap::new(),
            blocks,
            span: Span::dummy(),
        }],
    }
}

#[test]
fn test_lower_constant() {
    // MIR: t0 = 42; return t0
    let program = make_simple_program(vec![BasicBlock {
        id: BlockId(0),
        params: vec![],
        statements: vec![MirStatement::Assign {
            dest: Temp(0),
            value: MirValue::Const(MirConst::Int32(42)),
            span: None,
        }],
        terminator: MirTerminator::Return {
            value: Some(Temp(0)),
            span: None,
        },
    }]);

    let mut lowerer = MirToInstructions::new();
    let instructions = lowerer.lower_program(&program);

    // Should have: load immediate, return
    assert!(instructions.len() >= 2);

    // Check for load immediate (implemented as Copy)
    let has_immediate = instructions.iter().any(|inst| {
        matches!(
            inst,
            Instruction::Copy {
                src: Value::SignedImm(42),
                ..
            }
        )
    });
    assert!(has_immediate, "Should have immediate load");
}

#[test]
fn test_lower_binary_op() {
    // MIR: t0 = 10; t1 = 20; t2 = t0 + t1; return t2
    let program = make_simple_program(vec![BasicBlock {
        id: BlockId(0),
        params: vec![],
        statements: vec![
            MirStatement::Assign {
                dest: Temp(0),
                value: MirValue::Const(MirConst::Int32(10)),
                span: None,
            },
            MirStatement::Assign {
                dest: Temp(1),
                value: MirValue::Const(MirConst::Int32(20)),
                span: None,
            },
            MirStatement::Assign {
                dest: Temp(2),
                value: MirValue::BinaryOp {
                    op: MirBinOp::Add,
                    lhs: Temp(0),
                    rhs: Temp(1),
                },
                span: None,
            },
        ],
        terminator: MirTerminator::Return {
            value: Some(Temp(2)),
            span: None,
        },
    }]);

    let mut lowerer = MirToInstructions::new();
    let instructions = lowerer.lower_program(&program);

    // Should have load immediates and binary operation
    let has_binary = instructions
        .iter()
        .any(|inst| matches!(inst, Instruction::BinaryOp { op: BinOp::Add, .. }));
    assert!(has_binary, "Should have binary add instruction");
}

#[test]
fn test_lower_branch() {
    // MIR with branch: if t0 then B1 else B2
    let program = make_simple_program(vec![
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: Temp(0),
                value: MirValue::Const(MirConst::Bool(true)),
                span: None,
            }],
            terminator: MirTerminator::Branch {
                condition: Temp(0),
                then_block: BlockId(1),
                then_args: vec![],
                else_block: BlockId(2),
                else_args: vec![],
                span: None,
            },
        },
        BasicBlock {
            id: BlockId(1),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: Temp(1),
                value: MirValue::Const(MirConst::Int32(100)),
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: Some(Temp(1)),
                span: None,
            },
        },
        BasicBlock {
            id: BlockId(2),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: Temp(2),
                value: MirValue::Const(MirConst::Int32(200)),
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: Some(Temp(2)),
                span: None,
            },
        },
    ]);

    let mut lowerer = MirToInstructions::new();
    let instructions = lowerer.lower_program(&program);

    // Should have conditional branch
    let has_branch = instructions
        .iter()
        .any(|inst| matches!(inst, Instruction::Branch { .. }));
    assert!(has_branch, "Should have conditional branch");

    // Should have labels
    let label_count = instructions
        .iter()
        .filter(|inst| matches!(inst, Instruction::Label { .. }))
        .count();
    assert!(label_count >= 2, "Should have labels for blocks");
}

#[test]
fn test_lower_goto() {
    // MIR with goto
    let program = make_simple_program(vec![
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![],
            terminator: MirTerminator::Goto {
                target: BlockId(1),
                args: vec![],
                span: None,
            },
        },
        BasicBlock {
            id: BlockId(1),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: Temp(0),
                value: MirValue::Const(MirConst::Int32(42)),
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: Some(Temp(0)),
                span: None,
            },
        },
    ]);

    let mut lowerer = MirToInstructions::new();
    let instructions = lowerer.lower_program(&program);

    // Should have unconditional jump
    let has_jump = instructions
        .iter()
        .any(|inst| matches!(inst, Instruction::Jump { .. }));
    assert!(has_jump, "Should have unconditional jump");
}

#[test]
fn test_lower_function_call() {
    // MIR with function call
    let program = make_simple_program(vec![BasicBlock {
        id: BlockId(0),
        params: vec![],
        statements: vec![
            MirStatement::Assign {
                dest: Temp(0),
                value: MirValue::Const(MirConst::Int32(5)),
                span: None,
            },
            MirStatement::Assign {
                dest: Temp(1),
                value: MirValue::Call {
                    func: "factorial".to_string(),
                    args: vec![Temp(0)],
                    kind: CallKind::Impure,
                },
                span: None,
            },
        ],
        terminator: MirTerminator::Return {
            value: Some(Temp(1)),
            span: None,
        },
    }]);

    let mut lowerer = MirToInstructions::new();
    let instructions = lowerer.lower_program(&program);

    // Should have call instruction
    let has_call = instructions
        .iter()
        .any(|inst| matches!(inst, Instruction::Call { .. }));
    assert!(has_call, "Should have call instruction");
}

#[test]
fn test_lower_block_parameters() {
    // MIR with block parameters
    let program = make_simple_program(vec![
        BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![
                MirStatement::Assign {
                    dest: Temp(0),
                    value: MirValue::Const(MirConst::Int32(10)),
                    span: None,
                },
                MirStatement::Assign {
                    dest: Temp(1),
                    value: MirValue::Const(MirConst::Int32(20)),
                    span: None,
                },
            ],
            terminator: MirTerminator::Goto {
                target: BlockId(1),
                args: vec![Temp(0), Temp(1)],
                span: None,
            },
        },
        BasicBlock {
            id: BlockId(1),
            params: vec![(Temp(2), RueType::I32), (Temp(3), RueType::I32)],
            statements: vec![MirStatement::Assign {
                dest: Temp(4),
                value: MirValue::BinaryOp {
                    op: MirBinOp::Add,
                    lhs: Temp(2),
                    rhs: Temp(3),
                },
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: Some(Temp(4)),
                span: None,
            },
        },
    ]);

    let mut lowerer = MirToInstructions::new();
    let instructions = lowerer.lower_program(&program);

    // Should have copy instructions for block parameters
    let copy_count = instructions
        .iter()
        .filter(|inst| matches!(inst, Instruction::Copy { .. }))
        .count();
    assert!(
        copy_count >= 2,
        "Should have copy instructions for block parameters"
    );
}

#[test]
fn test_lower_unary_op() {
    // MIR with unary operation
    let program = make_simple_program(vec![BasicBlock {
        id: BlockId(0),
        params: vec![],
        statements: vec![
            MirStatement::Assign {
                dest: Temp(0),
                value: MirValue::Const(MirConst::Int32(42)),
                span: None,
            },
            MirStatement::Assign {
                dest: Temp(1),
                value: MirValue::UnaryOp {
                    op: MirUnaryOp::Neg,
                    operand: Temp(0),
                },
                span: None,
            },
        ],
        terminator: MirTerminator::Return {
            value: Some(Temp(1)),
            span: None,
        },
    }]);

    let mut lowerer = MirToInstructions::new();
    let instructions = lowerer.lower_program(&program);

    // Should have negation (implemented as 0 - x)
    let has_neg = instructions
        .iter()
        .any(|inst| matches!(inst, Instruction::BinaryOp { op: BinOp::Sub, .. }));
    assert!(has_neg, "Should have subtraction for negation");
}

#[test]
fn test_lower_multiple_functions() {
    // Program with multiple functions
    let program = MirProgram {
        functions: vec![
            MirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                temp_types: std::collections::HashMap::new(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(0),
                        value: MirValue::Call {
                            func: "helper".to_string(),
                            args: vec![],
                            kind: CallKind::Impure,
                        },
                        span: None,
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(0)),
                        span: None,
                    },
                }],
                span: Span::dummy(),
            },
            MirFunction {
                name: "helper".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                temp_types: std::collections::HashMap::new(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(0),
                        value: MirValue::Const(MirConst::Int32(42)),
                        span: None,
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(0)),
                        span: None,
                    },
                }],
                span: Span::dummy(),
            },
        ],
    };

    let mut lowerer = MirToInstructions::new();
    let instructions = lowerer.lower_program(&program);

    // Should have labels for both functions
    let func_labels = instructions
        .iter()
        .filter(|inst| matches!(inst, Instruction::Label(_)))
        .count();
    assert!(
        func_labels >= 3,
        "Should have labels for both functions plus _start"
    );
}

#[test]
fn test_lower_comparison() {
    // MIR with comparison operations
    let program = make_simple_program(vec![BasicBlock {
        id: BlockId(0),
        params: vec![],
        statements: vec![
            MirStatement::Assign {
                dest: Temp(0),
                value: MirValue::Const(MirConst::Int32(10)),
                span: None,
            },
            MirStatement::Assign {
                dest: Temp(1),
                value: MirValue::Const(MirConst::Int32(20)),
                span: None,
            },
            MirStatement::Assign {
                dest: Temp(2),
                value: MirValue::BinaryOp {
                    op: MirBinOp::Lt,
                    lhs: Temp(0),
                    rhs: Temp(1),
                },
                span: None,
            },
        ],
        terminator: MirTerminator::Return {
            value: Some(Temp(2)),
            span: None,
        },
    }]);

    let mut lowerer = MirToInstructions::new();
    let instructions = lowerer.lower_program(&program);

    // Should have comparison instruction
    let has_cmp = instructions
        .iter()
        .any(|inst| matches!(inst, Instruction::BinaryOp { op: BinOp::Lt, .. }));
    assert!(has_cmp, "Should have comparison instruction");
}
