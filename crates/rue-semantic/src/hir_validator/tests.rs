//! Tests for HIR validation

use super::*;
use rue_ir::hir::{HirLiteral, HirProgram};
use rue_lexer::Span;

fn dummy_span() -> Span {
    Span { start: 0, end: 0 }
}

#[test]
fn test_validate_empty_program() {
    let program = HirProgram { functions: vec![] };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);

    // Empty program is now valid (main check moved elsewhere)
    assert!(result.is_ok());
}

#[test]
fn test_validate_simple_main() {
    let program = HirProgram {
        functions: vec![HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Literal {
                    lit: HirLiteral::Int32(42),
                    span: dummy_span(),
                })),
            },
            span: dummy_span(),
        }],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);
    assert!(result.is_ok());
}

#[test]
fn test_validate_type_mismatch_in_let() {
    let program = HirProgram {
        functions: vec![HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::Unit,
            body: HirBlock {
                statements: vec![HirStatement::Let {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Bool(true),
                        span: dummy_span(),
                    },
                    span: dummy_span(),
                }],
                expr: None,
            },
            span: dummy_span(),
        }],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);

    assert!(result.is_err());
    let errors = result.unwrap_err();
    assert!(
        errors[0]
            .message
            .contains("declares type i32 but initializer has type bool")
    );
}

#[test]
fn test_validate_undefined_variable() {
    let program = HirProgram {
        functions: vec![HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Var {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    span: dummy_span(),
                })),
            },
            span: dummy_span(),
        }],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);

    assert!(result.is_err());
    let errors = result.unwrap_err();
    assert!(
        errors[0]
            .message
            .contains("Reference to undefined variable 'x'")
    );
}

#[test]
fn test_validate_binary_type_mismatch() {
    let program = HirProgram {
        functions: vec![HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(42),
                        span: dummy_span(),
                    }),
                    rhs: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Bool(true),
                        span: dummy_span(),
                    }),
                    ty: RueType::I32,
                    span: dummy_span(),
                })),
            },
            span: dummy_span(),
        }],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);

    assert!(result.is_err());
    let errors = result.unwrap_err();
    assert!(
        errors[0]
            .message
            .contains("requires matching operand types")
    );
}

#[test]
fn test_validate_function_call() {
    let program = HirProgram {
        functions: vec![
            HirFunction {
                name: "add".to_string(),
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
                            span: dummy_span(),
                        }),
                        rhs: Box::new(HirExpr::Var {
                            name: "b".to_string(),
                            ty: RueType::I32,
                            span: dummy_span(),
                        }),
                        ty: RueType::I32,
                        span: dummy_span(),
                    })),
                },
                span: dummy_span(),
            },
            HirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                body: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Call {
                        func: "add".to_string(),
                        args: vec![
                            HirExpr::Literal {
                                lit: HirLiteral::Int32(10),
                                span: dummy_span(),
                            },
                            HirExpr::Literal {
                                lit: HirLiteral::Int32(20),
                                span: dummy_span(),
                            },
                        ],
                        ty: RueType::I32,
                        span: dummy_span(),
                    })),
                },
                span: dummy_span(),
            },
        ],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);
    assert!(result.is_ok());
}

#[test]
fn test_validate_if_expression() {
    let program = HirProgram {
        functions: vec![HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::If {
                    cond: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Bool(true),
                        span: dummy_span(),
                    }),
                    then_block: HirBlock {
                        statements: vec![],
                        expr: Some(Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(10),
                            span: dummy_span(),
                        })),
                    },
                    else_block: Some(HirBlock {
                        statements: vec![],
                        expr: Some(Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(20),
                            span: dummy_span(),
                        })),
                    }),
                    ty: RueType::I32,
                    span: dummy_span(),
                })),
            },
            span: dummy_span(),
        }],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);
    assert!(result.is_ok());
}

#[test]
fn test_validate_scope_shadowing() {
    let program = HirProgram {
        functions: vec![HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![HirStatement::Let {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    init: HirExpr::Literal {
                        lit: HirLiteral::Int32(10),
                        span: dummy_span(),
                    },
                    span: dummy_span(),
                }],
                expr: Some(Box::new(HirExpr::If {
                    cond: Box::new(HirExpr::Literal {
                        lit: HirLiteral::Bool(true),
                        span: dummy_span(),
                    }),
                    then_block: HirBlock {
                        statements: vec![HirStatement::Let {
                            name: "x".to_string(),
                            ty: RueType::I32,
                            init: HirExpr::Literal {
                                lit: HirLiteral::Int32(20),
                                span: dummy_span(),
                            },
                            span: dummy_span(),
                        }],
                        expr: Some(Box::new(HirExpr::Var {
                            name: "x".to_string(),
                            ty: RueType::I32,
                            span: dummy_span(),
                        })),
                    },
                    else_block: Some(HirBlock {
                        statements: vec![],
                        expr: Some(Box::new(HirExpr::Var {
                            name: "x".to_string(),
                            ty: RueType::I32,
                            span: dummy_span(),
                        })),
                    }),
                    ty: RueType::I32,
                    span: dummy_span(),
                })),
            },
            span: dummy_span(),
        }],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);
    assert!(result.is_ok());
}

#[test]
fn test_validate_return_type_mismatch() {
    let program = HirProgram {
        functions: vec![HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::I32,
            body: HirBlock {
                statements: vec![],
                expr: Some(Box::new(HirExpr::Literal {
                    lit: HirLiteral::Bool(true),
                    span: dummy_span(),
                })),
            },
            span: dummy_span(),
        }],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);

    assert!(result.is_err());
    let errors = result.unwrap_err();
    assert!(
        errors[0]
            .message
            .contains("declares return type i32 but body returns bool")
    );
}

#[test]
fn test_validate_while_loop() {
    let program = HirProgram {
        functions: vec![HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::Unit,
            body: HirBlock {
                statements: vec![
                    HirStatement::Let {
                        name: "i".to_string(),
                        ty: RueType::I32,
                        init: HirExpr::Literal {
                            lit: HirLiteral::Int32(0),
                            span: dummy_span(),
                        },
                        span: dummy_span(),
                    },
                    HirStatement::Expr(HirExpr::While {
                        cond: Box::new(HirExpr::Binary {
                            op: BinOp::Lt,
                            lhs: Box::new(HirExpr::Var {
                                name: "i".to_string(),
                                ty: RueType::I32,
                                span: dummy_span(),
                            }),
                            rhs: Box::new(HirExpr::Literal {
                                lit: HirLiteral::Int32(10),
                                span: dummy_span(),
                            }),
                            ty: RueType::Bool,
                            span: dummy_span(),
                        }),
                        body: HirBlock {
                            statements: vec![HirStatement::Assign {
                                name: "i".to_string(),
                                value: HirExpr::Binary {
                                    op: BinOp::Add,
                                    lhs: Box::new(HirExpr::Var {
                                        name: "i".to_string(),
                                        ty: RueType::I32,
                                        span: dummy_span(),
                                    }),
                                    rhs: Box::new(HirExpr::Literal {
                                        lit: HirLiteral::Int32(1),
                                        span: dummy_span(),
                                    }),
                                    ty: RueType::I32,
                                    span: dummy_span(),
                                },
                                span: dummy_span(),
                            }],
                            expr: None,
                        },
                        span: dummy_span(),
                    }),
                ],
                expr: None,
            },
            span: dummy_span(),
        }],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);
    assert!(result.is_ok());
}

#[test]
fn test_validate_builtin_function_call() {
    let program = HirProgram {
        functions: vec![HirFunction {
            name: "main".to_string(),
            params: vec![],
            return_type: RueType::Unit,
            body: HirBlock {
                statements: vec![HirStatement::Expr(HirExpr::Call {
                    func: "println_i32".to_string(),
                    args: vec![HirExpr::Literal {
                        lit: HirLiteral::Int32(42),
                        span: dummy_span(),
                    }],
                    ty: RueType::Unit,
                    span: dummy_span(),
                })],
                expr: None,
            },
            span: dummy_span(),
        }],
    };

    let mut validator = HirValidator::new();
    let result = validator.validate_program(&program);
    assert!(result.is_ok());
}
