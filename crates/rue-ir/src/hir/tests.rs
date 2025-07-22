//! Tests for HIR types

use super::*;
use rue_lexer::Span;

#[test]
fn test_hir_expr_type() {
    // Test literal types
    let span = Span { start: 0, end: 0 };
    assert_eq!(
        HirExpr::Literal {
            lit: HirLiteral::Int32(42),
            span,
        }
        .ty(),
        &RueType::I32
    );
    assert_eq!(
        HirExpr::Literal {
            lit: HirLiteral::Int64(42),
            span,
        }
        .ty(),
        &RueType::I64
    );
    assert_eq!(
        HirExpr::Literal {
            lit: HirLiteral::Bool(true),
            span,
        }
        .ty(),
        &RueType::Bool
    );
    assert_eq!(
        HirExpr::Literal {
            lit: HirLiteral::Unit,
            span,
        }
        .ty(),
        &RueType::Unit
    );

    // Test variable type
    let var = HirExpr::Var {
        name: "x".to_string(),
        ty: RueType::I32,
        span,
    };
    assert_eq!(var.ty(), &RueType::I32);

    // Test while loop always returns unit
    let while_expr = HirExpr::While {
        cond: Box::new(HirExpr::Literal {
            lit: HirLiteral::Bool(true),
            span,
        }),
        body: HirBlock {
            statements: vec![],
            expr: None,
        },
        span,
    };
    assert_eq!(while_expr.ty(), &RueType::Unit);
}

#[test]
fn test_hir_display() {
    // Test function display
    let span = Span { start: 0, end: 0 };
    let func = HirFunction {
        name: "add".to_string(),
        params: vec![
            ("x".to_string(), RueType::I32),
            ("y".to_string(), RueType::I32),
        ],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::Binary {
                op: BinOp::Add,
                lhs: Box::new(HirExpr::Var {
                    name: "x".to_string(),
                    ty: RueType::I32,
                    span,
                }),
                rhs: Box::new(HirExpr::Var {
                    name: "y".to_string(),
                    ty: RueType::I32,
                    span,
                }),
                ty: RueType::I32,
                span,
            })),
        },
        span,
    };

    let display = format!("{func}");
    assert!(display.contains("fn add(x: i32, y: i32) -> i32"));
    assert!(display.contains("(x + y)"));
}

#[test]
fn test_binop_display() {
    assert_eq!(format!("{}", BinOp::Add), "+");
    assert_eq!(format!("{}", BinOp::Sub), "-");
    assert_eq!(format!("{}", BinOp::Mul), "*");
    assert_eq!(format!("{}", BinOp::Div), "/");
    assert_eq!(format!("{}", BinOp::Mod), "%");
    assert_eq!(format!("{}", BinOp::Lt), "<");
    assert_eq!(format!("{}", BinOp::Le), "<=");
    assert_eq!(format!("{}", BinOp::Gt), ">");
    assert_eq!(format!("{}", BinOp::Ge), ">=");
    assert_eq!(format!("{}", BinOp::Eq), "==");
    assert_eq!(format!("{}", BinOp::Ne), "!=");
}

#[test]
fn test_unaryop_display() {
    assert_eq!(format!("{}", UnaryOp::Neg), "-");
}
