//! Tests for HIR builder
//!
//! These tests verify that the HIR builder correctly converts typed AST to HIR.

use super::*;
use rue_parser::parse;
use rue_lexer::Lexer;
use crate::analyze_cst;

/// Helper function to build HIR from source code
fn build_hir_from_source(source: &str) -> Result<HirProgram, String> {
    // Lex and parse
    let mut lexer = Lexer::new(source);
    let tokens = lexer
        .tokenize()
        .map_err(|e| format!("Lex error: {e}"))?;
    
    let cst = parse(tokens)
        .map_err(|e| format!("Parse error: {:?}", e))?;
    
    // Run semantic analysis
    let analysis_result = analyze_cst(&cst)
        .map_err(|e| format!("Semantic error: {:?}", e))?;
    
    // Return HIR
    Ok(analysis_result.hir)
}

#[test]
fn test_literal_expressions() {
    let source = r#"
        fn main() -> i32 {
            42
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    assert_eq!(hir.functions.len(), 1);
    
    let main_fn = &hir.functions[0];
    assert_eq!(main_fn.name, "main");
    
    // Check the body has a final expression
    let expr = main_fn.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::Literal { lit: HirLiteral::Int32(42), .. } => {},
        _ => panic!("Expected Int32 literal, got {:?}", expr),
    }
}

#[test]
fn test_bool_literal_expressions() {
    let source = r#"
        fn test_bool() -> bool {
            true
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    let expr = func.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::Literal { lit: HirLiteral::Bool(true), .. } => {},
        _ => panic!("Expected Bool literal, got {:?}", expr),
    }
}

#[test]
fn test_unit_literal_expressions() {
    let source = r#"
        fn test_unit() {
            ()
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    let expr = func.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::Literal { lit: HirLiteral::Unit, .. } => {},
        _ => panic!("Expected Unit literal, got {:?}", expr),
    }
}

#[test]
fn test_variable_expressions() {
    let source = r#"
        fn test_var(x: i32) -> i32 {
            x
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    let expr = func.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::Var { name, ty, .. } => {
            assert_eq!(name, "x");
            assert_eq!(ty, &RueType::I32);
        },
        _ => panic!("Expected Var expression, got {:?}", expr),
    }
}

#[test]
fn test_binary_expressions() {
    let source = r#"
        fn test_add(x: i32, y: i32) -> i32 {
            x + y
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    let expr = func.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::Binary { op: BinOp::Add, lhs, rhs, ty, .. } => {
            assert_eq!(ty, &RueType::I32);
            
            // Check left operand
            match lhs.as_ref() {
                HirExpr::Var { name, .. } => assert_eq!(name, "x"),
                _ => panic!("Expected Var for lhs"),
            }
            
            // Check right operand
            match rhs.as_ref() {
                HirExpr::Var { name, .. } => assert_eq!(name, "y"),
                _ => panic!("Expected Var for rhs"),
            }
        },
        _ => panic!("Expected Binary expression, got {:?}", expr),
    }
}

#[test]
fn test_comparison_expressions() {
    let source = r#"
        fn test_lt(x: i32, y: i32) -> bool {
            x < y
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    let expr = func.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::Binary { op: BinOp::Lt, ty, .. } => {
            assert_eq!(ty, &RueType::Bool);
        },
        _ => panic!("Expected Binary comparison expression, got {:?}", expr),
    }
}

#[test]
fn test_unary_expressions() {
    let source = r#"
        fn test_neg(x: i32) -> i32 {
            -x
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    let expr = func.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::Unary { op: UnaryOp::Neg, expr, ty, .. } => {
            assert_eq!(ty, &RueType::I32);
            
            match expr.as_ref() {
                HirExpr::Var { name, .. } => assert_eq!(name, "x"),
                _ => panic!("Expected Var for unary operand"),
            }
        },
        _ => panic!("Expected Unary expression, got {:?}", expr),
    }
}

#[test]
fn test_function_call_expressions() {
    let source = r#"
        fn add(x: i32, y: i32) -> i32 {
            x + y
        }
        
        fn main() -> i32 {
            add(1, 2)
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    assert_eq!(hir.functions.len(), 2);
    
    let main_fn = &hir.functions[1];
    let expr = main_fn.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::Call { func, args, ty, .. } => {
            assert_eq!(func, "add");
            assert_eq!(ty, &RueType::I32);
            assert_eq!(args.len(), 2);
            
            // Check arguments
            match &args[0] {
                HirExpr::Literal { lit: HirLiteral::Int32(1), .. } => {},
                _ => panic!("Expected Int32(1) for first arg"),
            }
            
            match &args[1] {
                HirExpr::Literal { lit: HirLiteral::Int32(2), .. } => {},
                _ => panic!("Expected Int32(2) for second arg"),
            }
        },
        _ => panic!("Expected Call expression, got {:?}", expr),
    }
}

#[test]
fn test_if_expressions() {
    let source = r#"
        fn test_if(x: i32) -> i32 {
            if x < 0 {
                -x
            } else {
                x
            }
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    let expr = func.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::If { cond, then_block, else_block, ty, .. } => {
            assert_eq!(ty, &RueType::I32);
            
            // Check condition
            match cond.as_ref() {
                HirExpr::Binary { op: BinOp::Lt, .. } => {},
                _ => panic!("Expected comparison in condition"),
            }
            
            // Check then block
            assert!(then_block.expr.is_some());
            
            // Check else block
            assert!(else_block.is_some());
        },
        _ => panic!("Expected If expression, got {:?}", expr),
    }
}

#[test]
fn test_while_expressions() {
    let source = r#"
        fn test_while(x: i32) {
            while x > 0 {
                x = x - 1;
            }
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    
    // While loops are expressions in the function body
    let expr = func.body.expr.as_ref().unwrap();
    
    match expr.as_ref() {
        HirExpr::While { cond, body, .. } => {
            // Check condition
            match cond.as_ref() {
                HirExpr::Binary { op: BinOp::Gt, .. } => {},
                _ => panic!("Expected comparison in while condition"),
            }
            
            // Check body has statement
            assert_eq!(body.statements.len(), 1);
        },
        _ => panic!("Expected While expression"),
    }
}

#[test]
fn test_let_statement_lowering() {
    let source = r#"
        fn test_let() -> i32 {
            let x = 42;
            x
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    
    assert_eq!(func.body.statements.len(), 1);
    
    match &func.body.statements[0] {
        HirStatement::Let { name, ty, init, .. } => {
            assert_eq!(name, "x");
            assert_eq!(ty, &RueType::I32);
            
            match init {
                HirExpr::Literal { lit: HirLiteral::Int32(42), .. } => {},
                _ => panic!("Expected Int32(42) for init"),
            }
        },
        _ => panic!("Expected Let statement"),
    }
}

#[test]
fn test_assign_statement_lowering() {
    let source = r#"
        fn test_assign(x: i32) {
            x = 10;
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    
    assert_eq!(func.body.statements.len(), 1);
    
    match &func.body.statements[0] {
        HirStatement::Assign { name, value, .. } => {
            assert_eq!(name, "x");
            
            match value {
                HirExpr::Literal { lit: HirLiteral::Int32(10), .. } => {},
                _ => panic!("Expected Int32(10) for value"),
            }
        },
        _ => panic!("Expected Assign statement"),
    }
}

#[test]
fn test_complex_expression_lowering() {
    let source = r#"
        fn complex(a: i32, b: i32, c: i32) -> i32 {
            (a + b) * c - 1
        }
    "#;
    
    let hir = build_hir_from_source(source).unwrap();
    let func = &hir.functions[0];
    let expr = func.body.expr.as_ref().unwrap();
    
    // Should be: (- (* (+ a b) c) 1)
    match expr.as_ref() {
        HirExpr::Binary { op: BinOp::Sub, lhs, rhs, .. } => {
            // Check rhs is 1
            match rhs.as_ref() {
                HirExpr::Literal { lit: HirLiteral::Int32(1), .. } => {},
                _ => panic!("Expected 1 for rhs of subtraction"),
            }
            
            // Check lhs is multiplication
            match lhs.as_ref() {
                HirExpr::Binary { op: BinOp::Mul, lhs: mul_lhs, rhs: mul_rhs, .. } => {
                    // Check mul_rhs is c
                    match mul_rhs.as_ref() {
                        HirExpr::Var { name, .. } => assert_eq!(name, "c"),
                        _ => panic!("Expected c for rhs of multiplication"),
                    }
                    
                    // Check mul_lhs is addition
                    match mul_lhs.as_ref() {
                        HirExpr::Binary { op: BinOp::Add, .. } => {},
                        _ => panic!("Expected addition for lhs of multiplication"),
                    }
                },
                _ => panic!("Expected multiplication for lhs of subtraction"),
            }
        },
        _ => panic!("Expected subtraction at top level"),
    }
}

#[test]
fn test_all_operators_lowering() {
    let operators = vec![
        ("+", BinOp::Add),
        ("-", BinOp::Sub),
        ("*", BinOp::Mul),
        ("/", BinOp::Div),
        ("%", BinOp::Mod),
    ];
    
    for (op_str, expected_op) in operators {
        let source = format!(r#"
            fn test_op(x: i32, y: i32) -> i32 {{
                x {} y
            }}
        "#, op_str);
        
        let hir = build_hir_from_source(&source).unwrap();
        let func = &hir.functions[0];
        let expr = func.body.expr.as_ref().unwrap();
        
        match expr.as_ref() {
            HirExpr::Binary { op, .. } => {
                assert_eq!(op, &expected_op, "Wrong operator for {}", op_str);
            },
            _ => panic!("Expected Binary expression for {}", op_str),
        }
    }
}

#[test]
fn test_all_comparison_operators_lowering() {
    let operators = vec![
        ("<", BinOp::Lt),
        ("<=", BinOp::Le),
        (">", BinOp::Gt),
        (">=", BinOp::Ge),
        ("==", BinOp::Eq),
        ("!=", BinOp::Ne),
    ];
    
    for (op_str, expected_op) in operators {
        let source = format!(r#"
            fn test_op(x: i32, y: i32) -> bool {{
                x {} y
            }}
        "#, op_str);
        
        let hir = build_hir_from_source(&source).unwrap();
        let func = &hir.functions[0];
        let expr = func.body.expr.as_ref().unwrap();
        
        match expr.as_ref() {
            HirExpr::Binary { op, ty, .. } => {
                assert_eq!(op, &expected_op, "Wrong operator for {}", op_str);
                assert_eq!(ty, &RueType::Bool, "Comparison should return bool");
            },
            _ => panic!("Expected Binary expression for {}", op_str),
        }
    }
}