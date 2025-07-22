//! MIR roundtrip tests
//!
//! These tests verify the complete compilation pipeline through MIR:
//! HIR → MIR → Instructions → Assembly

use crate::{compile_hir_to_assembly, compile_hir_via_mir_to_assembly};
use rue_ir::hir::*;
use rue_ir::types::RueType;
use rue_lexer::Span;

/// Helper to create a simple HIR program
fn make_hir_program(functions: Vec<HirFunction>) -> HirProgram {
    HirProgram { functions }
}

/// Compare assembly output between direct and MIR compilation
fn compare_compilation_outputs(hir: &HirProgram) {
    // Compile directly (HIR → Instructions)
    let direct_asm = compile_hir_to_assembly(hir).expect("Direct compilation failed");

    // Compile via MIR (HIR → MIR → Instructions)
    let mir_asm = compile_hir_via_mir_to_assembly(hir).expect("MIR compilation failed");

    // Both should produce valid assembly
    assert!(
        !direct_asm.is_empty(),
        "Direct compilation produced empty assembly"
    );
    assert!(
        !mir_asm.is_empty(),
        "MIR compilation produced empty assembly"
    );

    // Both should have the same entry points
    assert!(
        direct_asm.contains("_start:"),
        "Direct assembly missing _start"
    );
    assert!(mir_asm.contains("_start:"), "MIR assembly missing _start");
    assert!(direct_asm.contains("main:"), "Direct assembly missing main");
    assert!(mir_asm.contains("main:"), "MIR assembly missing main");
}

#[test]
fn test_roundtrip_simple_return() {
    let hir = make_hir_program(vec![HirFunction {
        name: "main".to_string(),
        params: vec![],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::Literal {
                lit: HirLiteral::Int32(42),
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    }]);

    compare_compilation_outputs(&hir);
}

#[test]
fn test_roundtrip_arithmetic() {
    let hir = make_hir_program(vec![HirFunction {
        name: "main".to_string(),
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
                HirStatement::Let {
                    name: "y".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(20),
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
    }]);

    compare_compilation_outputs(&hir);
}

#[test]
fn test_roundtrip_control_flow() {
    let hir = make_hir_program(vec![HirFunction {
        name: "main".to_string(),
        params: vec![],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::If {
                cond: Box::new(HirExpr::Binary {
                    op: BinOp::Gt,
                    lhs: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(10),
                        span: Span::dummy(),
                    }),
                    rhs: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(5),
                        span: Span::dummy(),
                    }),
                    ty: RueType::Bool,
                    span: Span::dummy(),
                }),
                then_block: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(100),
                        span: Span::dummy(),
                    })),
                },
                else_block: Some(HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(200),
                        span: Span::dummy(),
                    })),
                }),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    }]);

    compare_compilation_outputs(&hir);
}

#[test]
fn test_roundtrip_function_call() {
    let hir = make_hir_program(vec![
        HirFunction {
            name: "helper".to_string(),
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
                    rhs: Box::new(HirExpr::Var {
                        name: "b".to_string(),
                        ty: RueType::I32,
                        span: Span::dummy(),
                    }),
                    ty: RueType::I32,
                    span: Span::dummy(),
                })),
            },
            span: Span::dummy(),
        },
        HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Call {
                    func: "helper".to_string(),
                    args: vec![
                        HirExpr::Literal {
                            lit: HirLiteral::Int32(10),
                            span: Span::dummy(),
                        },
                        HirExpr::Literal {
                            lit: HirLiteral::Int32(20),
                            span: Span::dummy(),
                        },
                    ],
                    ty: RueType::I32,
                    span: Span::dummy(),
                })),
            },
            span: Span::dummy(),
        },
    ]);

    compare_compilation_outputs(&hir);
}

#[test]
fn test_roundtrip_while_loop() {
    let hir = make_hir_program(vec![HirFunction {
        name: "main".to_string(),
        params: vec![],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![
                HirStatement::Let {
                    name: "count".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(5),
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
                HirStatement::Let {
                    name: "sum".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(0),
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
                        statements: vec![
                            HirStatement::Assign {
                                name: "sum".to_string(),
                                value: HirExpr::Binary {
                                    op: BinOp::Add,
                                    lhs: Box::new(HirExpr::Var {
                                        name: "sum".to_string(),
                                        ty: RueType::I32,
                                        span: Span::dummy(),
                                    }),
                                    rhs: Box::new(HirExpr::Var {
                                        name: "count".to_string(),
                                        ty: RueType::I32,
                                        span: Span::dummy(),
                                    }),
                                    ty: RueType::I32,
                                    span: Span::dummy(),
                                },
                                span: Span::dummy(),
                            },
                            HirStatement::Assign {
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
                            },
                        ],
                        expr: None,
                    },
                    span: Span::dummy(),
                }),
            ],
            expr: Some(Box::new(HirExpr::Var {
                name: "sum".to_string(),
                ty: RueType::I32,
                span: Span::dummy(),
            })),
        },
        span: Span::dummy(),
    }]);

    compare_compilation_outputs(&hir);
}

#[test]
fn test_roundtrip_optimization_opportunities() {
    // Test a program that should benefit from MIR optimizations
    let hir = make_hir_program(vec![HirFunction {
        name: "main".to_string(),
        params: vec![],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![
                // Constants that should be propagated
                HirStatement::Let {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(10),
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
                HirStatement::Let {
                    name: "y".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(20),
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
                // Duplicate computation (CSE opportunity)
                HirStatement::Let {
                    name: "a".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Binary {
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
                    },
                    span: Span::dummy(),
                },
                HirStatement::Let {
                    name: "b".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Binary {
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
                    },
                    span: Span::dummy(),
                },
                // Dead code (never used)
                HirStatement::Let {
                    name: "dead".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(999),
                        span: Span::dummy(),
                    },
                    span: Span::dummy(),
                },
            ],
            expr: Some(Box::new(HirExpr::Binary {
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
            })),
        },
        span: Span::dummy(),
    }]);

    compare_compilation_outputs(&hir);

    // With optimizations, MIR should produce more efficient code
    // (though both should produce correct results)
}
