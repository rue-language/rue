//! Tests for HIR-based code generation

#[cfg(test)]
mod tests {
    use crate::compile_hir::compile_hir_to_executable;
    use rue_ir::hir::{
        BinOp, HirBlock, HirExpr, HirFunction, HirLiteral, HirProgram, HirStatement,
    };
    use rue_ir::types::RueType;
    use rue_lexer::Span;

    #[test]
    fn test_simple_hir_codegen() {
        // Create a simple HIR program: fn main() -> i32 { 42 }
        let hir = HirProgram {
            functions: vec![HirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                body: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Literal {
                        lit: HirLiteral::Int32(42),
                        span: Span { start: 0, end: 0 },
                    })),
                },
                span: Span { start: 0, end: 0 },
            }],
        };

        let result = compile_hir_to_executable(&hir);
        assert!(result.is_ok());
        let executable = result.unwrap();
        assert!(!executable.is_empty());
    }

    #[test]
    fn test_hir_with_arithmetic() {
        // Create HIR for: fn main() -> i32 { 2 + 3 }
        let hir = HirProgram {
            functions: vec![HirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                body: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(2),
                            span: Span { start: 0, end: 0 },
                        }),
                        rhs: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(3),
                            span: Span { start: 0, end: 0 },
                        }),
                        ty: RueType::I32,
                        span: Span { start: 0, end: 0 },
                    })),
                },
                span: Span { start: 0, end: 0 },
            }],
        };

        let result = compile_hir_to_executable(&hir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_hir_with_let_statement() {
        // Create HIR for: fn main() -> i32 { let x: i32 = 10; x + 5 }
        let hir = HirProgram {
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
                            span: Span { start: 0, end: 0 },
                        },
                        span: Span { start: 0, end: 0 },
                    }],
                    expr: Some(Box::new(HirExpr::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(HirExpr::Var {
                            name: "x".to_string(),
                            ty: RueType::I32,
                            span: Span { start: 0, end: 0 },
                        }),
                        rhs: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Int32(5),
                            span: Span { start: 0, end: 0 },
                        }),
                        ty: RueType::I32,
                        span: Span { start: 0, end: 0 },
                    })),
                },
                span: Span { start: 0, end: 0 },
            }],
        };

        let result = compile_hir_to_executable(&hir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_hir_if_expression() {
        // Create HIR for: fn main() -> i32 { if true { 10 } else { 20 } }
        let hir = HirProgram {
            functions: vec![HirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                body: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::If {
                        cond: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Bool(true),
                            span: Span { start: 0, end: 0 },
                        }),
                        then_block: HirBlock {
                            statements: vec![],
                            expr: Some(Box::new(HirExpr::Literal {
                                lit: HirLiteral::Int32(10),
                                span: Span { start: 0, end: 0 },
                            })),
                        },
                        else_block: Some(HirBlock {
                            statements: vec![],
                            expr: Some(Box::new(HirExpr::Literal {
                                lit: HirLiteral::Int32(20),
                                span: Span { start: 0, end: 0 },
                            })),
                        }),
                        ty: RueType::I32,
                        span: Span { start: 0, end: 0 },
                    })),
                },
                span: Span { start: 0, end: 0 },
            }],
        };

        let result = compile_hir_to_executable(&hir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_hir_while_loop() {
        // Create HIR for: fn main() -> i32 { let mut x: i32 = 0; while x < 5 { x = x + 1; } x }
        let hir = HirProgram {
            functions: vec![HirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                body: HirBlock {
                    statements: vec![
                        HirStatement::Let {
                            name: "x".to_string(),
                            ty: RueType::I32,
                            init: HirExpr::Literal {
                                lit: HirLiteral::Int32(0),
                                span: Span { start: 0, end: 0 },
                            },
                            span: Span { start: 0, end: 0 },
                        },
                        HirStatement::Expr(HirExpr::While {
                            cond: Box::new(HirExpr::Binary {
                                op: BinOp::Lt,
                                lhs: Box::new(HirExpr::Var {
                                    name: "x".to_string(),
                                    ty: RueType::I32,
                                    span: Span { start: 0, end: 0 },
                                }),
                                rhs: Box::new(HirExpr::Literal {
                                    lit: HirLiteral::Int32(5),
                                    span: Span { start: 0, end: 0 },
                                }),
                                ty: RueType::Bool,
                                span: Span { start: 0, end: 0 },
                            }),
                            body: HirBlock {
                                statements: vec![HirStatement::Assign {
                                    name: "x".to_string(),
                                    value: HirExpr::Binary {
                                        op: BinOp::Add,
                                        lhs: Box::new(HirExpr::Var {
                                            name: "x".to_string(),
                                            ty: RueType::I32,
                                            span: Span { start: 0, end: 0 },
                                        }),
                                        rhs: Box::new(HirExpr::Literal {
                                            lit: HirLiteral::Int32(1),
                                            span: Span { start: 0, end: 0 },
                                        }),
                                        ty: RueType::I32,
                                        span: Span { start: 0, end: 0 },
                                    },
                                    span: Span { start: 0, end: 0 },
                                }],
                                expr: None,
                            },
                            span: Span { start: 0, end: 0 },
                        }),
                    ],
                    expr: Some(Box::new(HirExpr::Var {
                        name: "x".to_string(),
                        ty: RueType::I32,
                        span: Span { start: 0, end: 0 },
                    })),
                },
                span: Span { start: 0, end: 0 },
            }],
        };

        let result = compile_hir_to_executable(&hir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_hir_nested_if() {
        // Create HIR for: fn main() -> i32 { if true { if false { 1 } else { 2 } } else { 3 } }
        let hir = HirProgram {
            functions: vec![HirFunction {
                name: "main".to_string(),
                params: vec![],
                return_type: RueType::I32,
                body: HirBlock {
                    statements: vec![],
                    expr: Some(Box::new(HirExpr::If {
                        cond: Box::new(HirExpr::Literal {
                            lit: HirLiteral::Bool(true),
                            span: Span { start: 0, end: 0 },
                        }),
                        then_block: HirBlock {
                            statements: vec![],
                            expr: Some(Box::new(HirExpr::If {
                                cond: Box::new(HirExpr::Literal {
                                    lit: HirLiteral::Bool(false),
                                    span: Span { start: 0, end: 0 },
                                }),
                                then_block: HirBlock {
                                    statements: vec![],
                                    expr: Some(Box::new(HirExpr::Literal {
                                        lit: HirLiteral::Int32(1),
                                        span: Span { start: 0, end: 0 },
                                    })),
                                },
                                else_block: Some(HirBlock {
                                    statements: vec![],
                                    expr: Some(Box::new(HirExpr::Literal {
                                        lit: HirLiteral::Int32(2),
                                        span: Span { start: 0, end: 0 },
                                    })),
                                }),
                                ty: RueType::I32,
                                span: Span { start: 0, end: 0 },
                            })),
                        },
                        else_block: Some(HirBlock {
                            statements: vec![],
                            expr: Some(Box::new(HirExpr::Literal {
                                lit: HirLiteral::Int32(3),
                                span: Span { start: 0, end: 0 },
                            })),
                        }),
                        ty: RueType::I32,
                        span: Span { start: 0, end: 0 },
                    })),
                },
                span: Span { start: 0, end: 0 },
            }],
        };

        let result = compile_hir_to_executable(&hir);
        assert!(result.is_ok());
    }
}
