//! Tests for MIR generation and display

use crate::MirBuilder;
use rue_ir::hir::*;
use rue_ir::types::RueType;
use rue_lexer::Span;

#[test]
fn test_mir_factorial() {
    // Create HIR for factorial function:
    // fn factorial(n: i32) -> i32 {
    //     if n <= 1 {
    //         1
    //     } else {
    //         n * factorial(n - 1)
    //     }
    // }

    let hir_func = HirFunction {
        name: "factorial".to_string(),
        params: vec![("n".to_string(), RueType::I32)],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::If {
                cond: Box::new(HirExpr::Binary {
                    op: BinOp::Le,
                    lhs: Box::new(HirExpr::Var {
                        name: "n".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    rhs: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(1),
                        span: Span::dummy(),
                    }),
                    ty: RueType::Bool,
                    span: Span::dummy(),
                }),
                then_block: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(1),
                        span: Span::dummy(),
                    })),
                },
                else_block: Some(HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Binary {
                        op: BinOp::Mul,
                        lhs: Box::new(HirExpr::Var {
                            name: "n".to_string(),
                            ty: RueType::I32,
                            span: Span::dummy(),
                        }),
                        rhs: Box::new(HirExpr::Call {
                            func: "factorial".to_string(),
                            args: vec![HirExpr::Binary {
                                op: BinOp::Sub,
                                lhs: Box::new(HirExpr::Var {
                                    name: "n".to_string(),
                                    ty: RueType::I32,
                                    span: Span::dummy(),
                                }),
                                rhs: Box::new(HirExpr::Literal {
                                    lit: HirLiteral::Int32(1),
                                    span: Span::dummy(),
                                }),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            }],
                            ty: RueType::I32,
                            span: Span::dummy(),
                        }),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    })),
                }),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    };

    let mir_func = MirBuilder::lower_function(&hir_func);

    // Print the MIR
    println!("MIR for factorial:");
    println!("{mir_func}");

    // Verify structure
    assert_eq!(mir_func.name, "factorial");
    assert_eq!(mir_func.blocks.len(), 3); // entry, then, else (join block optimized away since both branches return)

    // Check that we have branch and return terminators
    let has_branch = mir_func
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, rue_ir::mir::MirTerminator::Branch { .. }));
    let has_return = mir_func
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, rue_ir::mir::MirTerminator::Return { .. }));

    assert!(has_branch);
    assert!(has_return);
}

#[test]
fn test_mir_while_loop() {
    // Create HIR for countdown function:
    // fn countdown(n: i32) -> i32 {
    //     let count: i32 = n;
    //     while count > 0 {
    //         count = count - 1;
    //     };
    //     count
    // }

    let hir_func = HirFunction {
        name: "countdown".to_string(),
        params: vec![("n".to_string(), RueType::I32)],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![
                HirStatement::Let {
                    name: "count".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Var {
                        name: "n".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
                HirStatement::Expr(HirExpr::While {
                    cond: Box::new(HirExpr::Binary {
                        op: BinOp::Gt,
                        lhs: Box::new(HirExpr::Var {
                            name: "count".to_string(),
                            ty: RueType::I32,
                            span: Span::dummy(),
                        }),
                        rhs: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(0),
                            span: Span::dummy(),
                        }),
                        ty: RueType::Bool,
                        span: Span::dummy(),
                    }),
                    body: HirBlock {
                        statements: vec![HirStatement::Assign {
                            name: "count".to_string(),
                            value: HirExpr::Binary {
                                op: BinOp::Sub,
                                lhs: Box::new(HirExpr::Var {
                                    name: "count".to_string(),
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
            expr: Some(Box::new(HirExpr::Var {
                name: "count".to_string(),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    };

    let mir_func = MirBuilder::lower_function(&hir_func);

    // Print the MIR
    println!("\nMIR for countdown:");
    println!("{mir_func}");

    // Verify structure - should have entry, loop header, loop body, and exit blocks
    assert_eq!(mir_func.name, "countdown");
    assert!(mir_func.blocks.len() >= 4);
}

#[test]
fn test_mir_simple_arithmetic() {
    // Test simple arithmetic operations
    // fn add_mul(a: i32, b: i32) -> i32 {
    //     a + b * 2
    // }

    let hir_func = HirFunction {
        name: "add_mul".to_string(),
        params: vec![
            ("a".to_string(), RueType::I32),
            ("b".to_string(), RueType::I32),
        ],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::Binary {
                op: BinOp::Add,
                lhs: Box::new(HirExpr::Var {
                    name: "a".to_string(),
                    ty: RueType::I32,
                    span: Span::dummy(),
                }),
                rhs: Box::new(HirExpr::Binary {
                    op: BinOp::Mul,
                    lhs: Box::new(HirExpr::Var {
                        name: "b".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    rhs: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(2),
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

    let mir_func = MirBuilder::lower_function(&hir_func);

    // Verify basic structure
    assert_eq!(mir_func.name, "add_mul");
    assert_eq!(mir_func.params.len(), 2);

    // Should have at least one block with some arithmetic operations
    assert!(!mir_func.blocks.is_empty());

    // Check that we have statements for the arithmetic
    let entry_block = &mir_func.blocks[0];
    assert!(entry_block.statements.len() >= 2); // At least mul and add operations
}

#[test]
fn test_mir_nested_if() {
    // Test nested if expressions
    // fn max3(a: i32, b: i32, c: i32) -> i32 {
    //     if a > b {
    //         if a > c { a } else { c }
    //     } else {
    //         if b > c { b } else { c }
    //     }
    // }

    let hir_func = HirFunction {
        name: "max3".to_string(),
        params: vec![
            ("a".to_string(), RueType::I32),
            ("b".to_string(), RueType::I32),
            ("c".to_string(), RueType::I32),
        ],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::If {
                cond: Box::new(HirExpr::Binary {
                    op: BinOp::Gt,
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
                    ty: RueType::Bool,
                    span: Span::dummy(),
                }),
                then_block: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::If {
                        cond: Box::new(HirExpr::Binary {
                            op: BinOp::Gt,
                            lhs: Box::new(HirExpr::Var {
                                name: "a".to_string(),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            }),
                            rhs: Box::new(HirExpr::Var {
                                name: "c".to_string(),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            }),
                            ty: RueType::Bool,
                            span: Span::dummy(),
                        }),
                        then_block: HirBlock {
                            statements: vec![],
                            expr: Some(Box::new(HirExpr::Var {
                                name: "a".to_string(),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            })),
                        },
                        else_block: Some(HirBlock {
                            statements: vec![],
                            expr: Some(Box::new(HirExpr::Var {
                                name: "c".to_string(),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            })),
                        }),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    })),
                },
                else_block: Some(HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::If {
                        cond: Box::new(HirExpr::Binary {
                            op: BinOp::Gt,
                            lhs: Box::new(HirExpr::Var {
                                name: "b".to_string(),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            }),
                            rhs: Box::new(HirExpr::Var {
                                name: "c".to_string(),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            }),
                            ty: RueType::Bool,
                            span: Span::dummy(),
                        }),
                        then_block: HirBlock {
                            statements: vec![],
                            expr: Some(Box::new(HirExpr::Var {
                                name: "b".to_string(),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            })),
                        },
                        else_block: Some(HirBlock {
                            statements: vec![],
                            expr: Some(Box::new(HirExpr::Var {
                                name: "c".to_string(),
                                ty: RueType::I32,
                                span: Span::dummy(),
                            })),
                        }),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    })),
                }),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    };

    let mir_func = MirBuilder::lower_function(&hir_func);

    println!("\nMIR for max3:");
    println!("{mir_func}");

    // Should have many blocks due to nested ifs
    assert_eq!(mir_func.name, "max3");
    assert!(mir_func.blocks.len() >= 6); // At least 6 blocks for nested ifs
}

#[test]
fn test_mir_unary_ops() {
    // Test unary operations (negation)
    // fn abs_value(x: i32) -> i32 {
    //     if x < 0 { -x } else { x }
    // }

    let hir_func = HirFunction {
        name: "abs_value".to_string(),
        params: vec![("x".to_string(), RueType::I32)],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::If {
                cond: Box::new(HirExpr::Binary {
                    op: BinOp::Lt,
                    lhs: Box::new(HirExpr::Var {
                        name: "x".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    rhs: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(0),
                        span: Span::dummy(),
                    }),
                    ty: RueType::Bool,
                    span: Span::dummy(),
                }),
                then_block: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Unary {
                        op: UnaryOp::Neg,
                        expr: Box::new(HirExpr::Var {
                            name: "x".to_string(),
                            ty: RueType::I32,
                            span: Span::dummy(),
                        }),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    })),
                },
                else_block: Some(HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Var {
                        name: "x".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    })),
                }),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    };

    let mir_func = MirBuilder::lower_function(&hir_func);

    // Verify it handles unary operations
    assert_eq!(mir_func.name, "abs_value");

    // Check for unary operations in the MIR
    let has_unary = mir_func.blocks.iter().any(|block| {
        block.statements.iter().any(|stmt| match stmt {
            rue_ir::mir::MirStatement::Assign { value, .. } => {
                matches!(value, rue_ir::mir::MirValue::UnaryOp { .. })
            }
            _ => false,
        })
    });
    assert!(has_unary);
}
