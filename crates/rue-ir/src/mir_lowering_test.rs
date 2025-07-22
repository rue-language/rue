//! Tests for HIR to MIR lowering
//!
//! These tests verify that each HIR construct is correctly lowered to MIR,
//! testing the transformation logic in isolation.

use crate::hir::*;
use crate::mir::*;
use crate::mir_lowering::MirBuilder;
use crate::types::RueType;
use rue_lexer::Span;

/// Helper to create a simple HIR function with a single expression body
fn make_simple_function(name: &str, expr: HirExpr) -> HirFunction {
    HirFunction {
        name: name.to_string(),
        params: vec![],
        return_type: expr.ty().clone(),
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(expr)),
        },
        span: Span::dummy(),
    }
}

#[test]
fn test_lower_literal_i32() {
    let func = make_simple_function(
        "test_literal",
        HirExpr::Literal {
            lit: HirLiteral::Int32(42),
            span: Span::dummy(),
        },
    );

    let mir = MirBuilder::lower_function(&func);

    // Should have one block that returns the constant
    assert_eq!(mir.blocks.len(), 1);
    let block = &mir.blocks[0];

    // Should assign the literal to a temp and return it
    assert_eq!(block.statements.len(), 1);
    match &block.statements[0] {
        MirStatement::Assign {
            value: MirValue::Const(MirConst::Int32(42)),
            ..
        } => {}
        _ => panic!("Expected constant assignment"),
    }
}

#[test]
fn test_lower_literal_bool() {
    let func = make_simple_function(
        "test_bool",
        HirExpr::Literal {
            lit: HirLiteral::Bool(true),
            span: Span::dummy(),
        },
    );

    let mir = MirBuilder::lower_function(&func);

    let block = &mir.blocks[0];
    assert_eq!(block.statements.len(), 1);
    match &block.statements[0] {
        MirStatement::Assign {
            value: MirValue::Const(MirConst::Bool(true)),
            ..
        } => {}
        _ => panic!("Expected bool constant assignment"),
    }
}

#[test]
fn test_lower_variable() {
    let func = HirFunction {
        name: "test_var".to_string(),
        params: vec![("x".to_string(), RueType::I32)],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::Var {
                name: "x".to_string(),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    };

    let mir = MirBuilder::lower_function(&func);

    // Entry block should have parameter
    let entry_block = &mir.blocks[0];
    assert_eq!(entry_block.params.len(), 1);

    // Should return the parameter
    match &entry_block.terminator {
        MirTerminator::Return { value: Some(temp) } => {
            // The returned temp should be the parameter
            assert_eq!(*temp, entry_block.params[0].0);
        }
        _ => panic!("Expected return terminator"),
    }
}

#[test]
fn test_lower_binary_op() {
    let func = make_simple_function(
        "test_binary",
        HirExpr::Binary {
            op: BinOp::Add,
            lhs: Box::new(HirExpr::Literal {
                lit: HirLiteral::Int32(10),
                span: Span::dummy(),
            }),
            rhs: Box::new(HirExpr::Literal {
                lit: HirLiteral::Int32(20),
                span: Span::dummy(),
            }),
            ty: RueType::I32,
            span: Span::dummy(),
        },
    );

    let mir = MirBuilder::lower_function(&func);

    let block = &mir.blocks[0];
    // Should have: t0 = 10, t1 = 20, t2 = t0 + t1
    assert!(block.statements.len() >= 3);

    // Find the binary operation
    let has_binary_op = block.statements.iter().any(|stmt| {
        matches!(
            stmt,
            MirStatement::Assign {
                value: MirValue::BinaryOp {
                    op: MirBinOp::Add,
                    ..
                },
                ..
            }
        )
    });
    assert!(has_binary_op, "Should have binary add operation");
}

#[test]
fn test_lower_comparison() {
    let func = make_simple_function(
        "test_cmp",
        HirExpr::Binary {
            op: BinOp::Lt,
            lhs: Box::new(HirExpr::Literal {
                lit: HirLiteral::Int32(5),
                span: Span::dummy(),
            }),
            rhs: Box::new(HirExpr::Literal {
                lit: HirLiteral::Int32(10),
                span: Span::dummy(),
            }),
            ty: RueType::Bool,
            span: Span::dummy(),
        },
    );

    let mir = MirBuilder::lower_function(&func);

    let block = &mir.blocks[0];
    let has_comparison = block.statements.iter().any(|stmt| {
        matches!(
            stmt,
            MirStatement::Assign {
                value: MirValue::BinaryOp {
                    op: MirBinOp::Lt,
                    ..
                },
                ..
            }
        )
    });
    assert!(has_comparison, "Should have comparison operation");
}

#[test]
fn test_lower_unary_op() {
    let func = make_simple_function(
        "test_unary",
        HirExpr::Unary {
            op: UnaryOp::Neg,
            expr: Box::new(HirExpr::Literal {
                lit: HirLiteral::Int32(42),
                span: Span::dummy(),
            }),
            ty: RueType::I32,
            span: Span::dummy(),
        },
    );

    let mir = MirBuilder::lower_function(&func);

    let block = &mir.blocks[0];
    let has_unary_op = block.statements.iter().any(|stmt| {
        matches!(
            stmt,
            MirStatement::Assign {
                value: MirValue::UnaryOp {
                    op: MirUnaryOp::Neg,
                    ..
                },
                ..
            }
        )
    });
    assert!(has_unary_op, "Should have unary negation operation");
}

#[test]
fn test_lower_function_call() {
    let func = make_simple_function(
        "test_call",
        HirExpr::Call {
            func: "foo".to_string(),
            args: vec![
                HirExpr::Literal {
                    lit: HirLiteral::Int32(1),
                    span: Span::dummy(),
                },
                HirExpr::Literal {
                    lit: HirLiteral::Int32(2),
                    span: Span::dummy(),
                },
            ],
            ty: RueType::I32,
            span: Span::dummy(),
        },
    );

    let mir = MirBuilder::lower_function(&func);

    let block = &mir.blocks[0];
    let has_call = block.statements.iter().any(|stmt| match stmt {
        MirStatement::Assign {
            value: MirValue::Call { func, args, .. },
            ..
        } => func == "foo" && args.len() == 2,
        _ => false,
    });
    assert!(has_call, "Should have function call");
}

#[test]
fn test_lower_if_expression() {
    let func = make_simple_function(
        "test_if",
        HirExpr::If {
            cond: Box::new(HirExpr::Literal {
                lit: HirLiteral::Bool(true),
                span: Span::dummy(),
            }),
            then_block: HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Literal {
                    lit: HirLiteral::Int32(10),
                    span: Span::dummy(),
                })),
            },
            else_block: Some(HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Literal {
                    lit: HirLiteral::Int32(20),
                    span: Span::dummy(),
                })),
            }),
            ty: RueType::I32,
            span: Span::dummy(),
        },
    );

    let mir = MirBuilder::lower_function(&func);

    // Should have at least 4 blocks: entry, then, else, join
    assert!(mir.blocks.len() >= 4);

    // Entry block should have a branch terminator
    let entry_block = &mir.blocks[0];
    match &entry_block.terminator {
        MirTerminator::Branch { .. } => {}
        _ => panic!("Expected branch terminator in entry block"),
    }
}

#[test]
fn test_lower_while_loop() {
    let func = HirFunction {
        name: "test_while".to_string(),
        params: vec![],
        return_type: RueType::Unit,
        body: HirBlock {
            statements: vec![
                HirStatement::Let {
                    name: "i".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(0),
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
                HirStatement::Expr(HirExpr::While {
                    cond: Box::new(HirExpr::Binary {
                        op: BinOp::Lt,
                        lhs: Box::new(HirExpr::Var {
                            name: "i".to_string(),
                            ty: RueType::I32,
                            span: Span::dummy(),
                        }),
                        rhs: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(10),
                            span: Span::dummy(),
                        }),
                        ty: RueType::Bool,
                        span: Span::dummy(),
                    }),
                    body: HirBlock {
                        statements: vec![HirStatement::Assign {
                            name: "i".to_string(),
                            value: HirExpr::Binary {
                                op: BinOp::Add,
                                lhs: Box::new(HirExpr::Var {
                                    name: "i".to_string(),
                                    ty: RueType::I32,
                                    span: Span::dummy(),
                                }),
                                rhs: Box::new(HirExpr::Literal {
                                    lit: HirLiteral::Int32(1),
                                    span: Span::dummy(),
                                }),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            },
                            span: Span::dummy(),
                        }],
                        expr: None,
                    },
                    span: Span::dummy(),
                }),
            ],
            expr: None,
        },
        span: Span::dummy(),
    };

    let mir = MirBuilder::lower_function(&func);

    // Should have at least 4 blocks: entry, loop header, loop body, exit
    assert!(mir.blocks.len() >= 4);

    // Should have blocks with branch terminators for the loop
    let has_branch = mir
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, MirTerminator::Branch { .. }));
    assert!(has_branch, "Should have branch for loop condition");

    // Should have a goto back to loop header
    let has_loop_back = mir
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, MirTerminator::Goto { .. }));
    assert!(has_loop_back, "Should have goto for loop back edge");
}

#[test]
fn test_lower_let_statement() {
    let func = HirFunction {
        name: "test_let".to_string(),
        params: vec![],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![HirStatement::Let {
                name: "x".to_string(),
                ty: RueType::I32,
                init: HirExpr::Literal {
                    lit: HirLiteral::Int32(42),
                    span: Span::dummy(),
                },
                span: Span::dummy(),
            }],
            expr: Some(Box::new(HirExpr::Var {
                name: "x".to_string(),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    };

    let mir = MirBuilder::lower_function(&func);

    let block = &mir.blocks[0];
    // Should have assignment for the let binding
    assert!(!block.statements.is_empty());

    // Should return the variable
    match &block.terminator {
        MirTerminator::Return { value: Some(_) } => {}
        _ => panic!("Expected return with value"),
    }
}

#[test]
fn test_lower_assignment() {
    let func = HirFunction {
        name: "test_assign".to_string(),
        params: vec![],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![
                HirStatement::Let {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(10),
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
                HirStatement::Assign {
                    name: "x".to_string(),
                    value: HirExpr::Literal {
                        lit: HirLiteral::Int32(20),
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
            ],
            expr: Some(Box::new(HirExpr::Var {
                name: "x".to_string(),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    };

    let mir = MirBuilder::lower_function(&func);

    let block = &mir.blocks[0];
    // Should have at least 2 assignments (initial and update)
    assert!(block.statements.len() >= 2);
}

#[test]
fn test_lower_nested_expressions() {
    // Test complex nested expression: (a + b) * (c - d)
    let func = HirFunction {
        name: "test_nested".to_string(),
        params: vec![
            ("a".to_string(), RueType::I32),
            ("b".to_string(), RueType::I32),
            ("c".to_string(), RueType::I32),
            ("d".to_string(), RueType::I32),
        ],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::Binary {
                op: BinOp::Mul,
                lhs: Box::new(HirExpr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(HirExpr::Var {
                        name: "a".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    rhs: Box::new(HirExpr::Var {
                        name: "b".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    ty: RueType::I32,
                    span: Span::dummy(),
                }),
                rhs: Box::new(HirExpr::Binary {
                    op: BinOp::Sub,
                    lhs: Box::new(HirExpr::Var {
                        name: "c".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    rhs: Box::new(HirExpr::Var {
                        name: "d".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    ty: RueType::I32,
                    span: Span::dummy(),
                }),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    };

    let mir = MirBuilder::lower_function(&func);

    let block = &mir.blocks[0];
    // Should have statements for each sub-expression
    let binary_ops = block
        .statements
        .iter()
        .filter(|stmt| {
            matches!(
                stmt,
                MirStatement::Assign {
                    value: MirValue::BinaryOp { .. },
                    ..
                }
            )
        })
        .count();

    assert_eq!(
        binary_ops, 3,
        "Should have 3 binary operations (add, sub, mul)"
    );
}

#[test]
fn test_lower_block_with_statements() {
    let func = HirFunction {
        name: "test_block".to_string(),
        params: vec![],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![
                HirStatement::Let {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(5),
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
                HirStatement::Let {
                    name: "y".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Binary {
                        op: BinOp::Mul,
                        lhs: Box::new(HirExpr::Var {
                            name: "x".to_string(),
                            ty: RueType::I32,
                            span: Span::dummy(),
                        }),
                        rhs: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(2),
                            span: Span::dummy(),
                        }),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
            ],
            expr: Some(Box::new(HirExpr::Binary {
                op: BinOp::Add,
                lhs: Box::new(HirExpr::Var {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    span: Span::dummy(),
                }),
                rhs: Box::new(HirExpr::Var {
                    name: "y".to_string(),
                    ty: RueType::I32,
                    span: Span::dummy(),
                }),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    };

    let mir = MirBuilder::lower_function(&func);

    let block = &mir.blocks[0];
    // Should have assignments for both let statements and the final expression
    assert!(block.statements.len() >= 4); // x = 5, y = x * 2, result = x + y
}
