//! Tests to ensure HIR preserves all type information correctly

#[cfg(test)]
mod tests {
    use crate::{AnalysisResult, analyze_cst};
    use rue_ir::hir::{HirExpr, HirStatement};
    use rue_ir::types::RueType;

    fn parse_and_analyze(source: &str) -> Result<AnalysisResult, crate::SemanticError> {
        let ast = rue_parser::parse_with_recovery(source, "test.rue").expect("Failed to parse");
        analyze_cst(&ast)
    }

    #[test]
    fn test_literal_type_preservation() {
        let source = r#"
            fn main() -> () {
                let x: i32 = 42;
                let y: i64 = 1000000000000;
                let z: bool = true;
                let u = ();
            }
        "#;

        let result = parse_and_analyze(source).unwrap();
        let main_fn = &result.hir.functions[0];

        // Check each let statement preserves the correct type
        match &main_fn.body.statements[0] {
            HirStatement::Let { name, ty, init, .. } => {
                assert_eq!(name, "x");
                assert_eq!(ty, &RueType::I32);
                match init {
                    HirExpr::Literal { lit, .. } => {
                        assert_eq!(format!("{lit:?}"), "Int32(42)");
                    }
                    _ => panic!("Expected literal"),
                }
            }
            _ => panic!("Expected let statement"),
        }

        match &main_fn.body.statements[1] {
            HirStatement::Let { name, ty, init, .. } => {
                assert_eq!(name, "y");
                assert_eq!(ty, &RueType::I64);
                match init {
                    HirExpr::Literal { lit, .. } => {
                        assert_eq!(format!("{lit:?}"), "Int64(1000000000000)");
                    }
                    _ => panic!("Expected literal"),
                }
            }
            _ => panic!("Expected let statement"),
        }

        match &main_fn.body.statements[2] {
            HirStatement::Let { name, ty, init, .. } => {
                assert_eq!(name, "z");
                assert_eq!(ty, &RueType::Bool);
                match init {
                    HirExpr::Literal { lit, .. } => {
                        assert_eq!(format!("{lit:?}"), "Bool(true)");
                    }
                    _ => panic!("Expected literal"),
                }
            }
            _ => panic!("Expected let statement"),
        }

        match &main_fn.body.statements[3] {
            HirStatement::Let { name, ty, init, .. } => {
                assert_eq!(name, "u");
                assert_eq!(ty, &RueType::Unit);
                match init {
                    HirExpr::Literal { lit, .. } => {
                        assert_eq!(format!("{lit:?}"), "Unit");
                    }
                    _ => panic!("Expected literal"),
                }
            }
            _ => panic!("Expected let statement"),
        }
    }

    #[test]
    fn test_expression_type_preservation() {
        let source = r#"
            fn main() -> i32 {
                let x: i32 = 10;
                let y: i32 = 20;
                x + y * 2
            }
        "#;

        let result = parse_and_analyze(source).unwrap();
        let main_fn = &result.hir.functions[0];

        // Check the final expression has correct type
        let final_expr = main_fn.body.expr.as_ref().unwrap();
        assert_eq!(final_expr.ty(), &RueType::I32);

        // Check it's a binary addition
        match final_expr.as_ref() {
            HirExpr::Binary {
                op, lhs, rhs, ty, ..
            } => {
                assert_eq!(*op, rue_ir::hir::BinOp::Add);
                assert_eq!(ty, &RueType::I32);

                // Check left is variable x
                match lhs.as_ref() {
                    HirExpr::Var { name, ty, .. } => {
                        assert_eq!(name, "x");
                        assert_eq!(ty, &RueType::I32);
                    }
                    _ => panic!("Expected variable"),
                }

                // Check right is multiplication
                match rhs.as_ref() {
                    HirExpr::Binary {
                        op, lhs, rhs, ty, ..
                    } => {
                        assert_eq!(*op, rue_ir::hir::BinOp::Mul);
                        assert_eq!(ty, &RueType::I32);

                        // Check operands
                        match lhs.as_ref() {
                            HirExpr::Var { name, ty, .. } => {
                                assert_eq!(name, "y");
                                assert_eq!(ty, &RueType::I32);
                            }
                            _ => panic!("Expected variable"),
                        }

                        match rhs.as_ref() {
                            HirExpr::Literal { .. } => {
                                // Literal 2
                            }
                            _ => panic!("Expected literal"),
                        }
                    }
                    _ => panic!("Expected binary expression"),
                }
            }
            _ => panic!("Expected binary expression"),
        }
    }

    #[test]
    fn test_function_call_type_preservation() {
        let source = r#"
            fn add(a: i32, b: i32) -> i32 {
                a + b
            }
            
            fn main() -> i32 {
                let x: i64 = 100;
                add(42, to_i32(x))
            }
        "#;

        let result = parse_and_analyze(source).unwrap();
        let main_fn = &result.hir.functions[1];

        // Check the final expression (function call) has correct type
        let final_expr = main_fn.body.expr.as_ref().unwrap();
        assert_eq!(final_expr.ty(), &RueType::I32);

        match final_expr.as_ref() {
            HirExpr::Call { func, args, ty, .. } => {
                assert_eq!(func, "add");
                assert_eq!(ty, &RueType::I32);
                assert_eq!(args.len(), 2);

                // First argument is literal 42
                match &args[0] {
                    HirExpr::Literal { .. } => {}
                    _ => panic!("Expected literal"),
                }

                // Second argument is to_i32 call
                match &args[1] {
                    HirExpr::Call { func, args, ty, .. } => {
                        assert_eq!(func, "to_i32");
                        assert_eq!(ty, &RueType::I32);
                        assert_eq!(args.len(), 1);

                        // Argument to to_i32 is variable x
                        match &args[0] {
                            HirExpr::Var { name, ty, .. } => {
                                assert_eq!(name, "x");
                                assert_eq!(ty, &RueType::I64);
                            }
                            _ => panic!("Expected variable"),
                        }
                    }
                    _ => panic!("Expected function call"),
                }
            }
            _ => panic!("Expected function call"),
        }
    }

    #[test]
    fn test_comparison_type_preservation() {
        let source = r#"
            fn main() -> bool {
                let x: i32 = 10;
                x < 15
            }
        "#;

        let result = parse_and_analyze(source).unwrap();
        let main_fn = &result.hir.functions[0];

        // Final expression should be bool
        let final_expr = main_fn.body.expr.as_ref().unwrap();
        assert_eq!(final_expr.ty(), &RueType::Bool);

        match final_expr.as_ref() {
            HirExpr::Binary {
                op, lhs, rhs, ty, ..
            } => {
                assert_eq!(*op, rue_ir::hir::BinOp::Lt);
                assert_eq!(ty, &RueType::Bool);

                // Left operand is variable x (i32)
                match lhs.as_ref() {
                    HirExpr::Var { name, ty, .. } => {
                        assert_eq!(name, "x");
                        assert_eq!(ty, &RueType::I32);
                    }
                    _ => panic!("Expected variable"),
                }

                // Right operand is literal 15 (i32)
                match rhs.as_ref() {
                    HirExpr::Literal { lit, .. } => {
                        assert_eq!(format!("{lit:?}"), "Int32(15)");
                    }
                    _ => panic!("Expected literal"),
                }
            }
            _ => panic!("Expected binary expression"),
        }
    }

    #[test]
    fn test_if_expression_type_preservation() {
        let source = r#"
            fn main() -> i64 {
                let x: i32 = 10;
                if x > 5 {
                    let hundred: i64 = 100;
                    to_i64(x) + hundred
                } else {
                    let twohundred: i64 = 200;
                    twohundred
                }
            }
        "#;

        let result = parse_and_analyze(source).unwrap();
        let main_fn = &result.hir.functions[0];

        let final_expr = main_fn.body.expr.as_ref().unwrap();
        assert_eq!(final_expr.ty(), &RueType::I64);

        match final_expr.as_ref() {
            HirExpr::If {
                cond,
                then_block,
                else_block,
                ty,
                ..
            } => {
                assert_eq!(ty, &RueType::I64);

                // Condition should be bool
                assert_eq!(cond.ty(), &RueType::Bool);

                // Then block should return i64
                let then_expr = then_block.expr.as_ref().unwrap();
                assert_eq!(then_expr.ty(), &RueType::I64);

                // Else block should return i64
                let else_block = else_block.as_ref().unwrap();
                let else_expr = else_block.expr.as_ref().unwrap();
                assert_eq!(else_expr.ty(), &RueType::I64);
            }
            _ => panic!("Expected if expression"),
        }
    }

    #[test]
    fn test_unary_operator_type_preservation() {
        let source = r#"
            fn main() -> i32 {
                let x: i32 = 10;
                -x
            }
        "#;

        let result = parse_and_analyze(source).unwrap();
        let main_fn = &result.hir.functions[0];

        let final_expr = main_fn.body.expr.as_ref().unwrap();
        assert_eq!(final_expr.ty(), &RueType::I32);

        match final_expr.as_ref() {
            HirExpr::Unary { op, expr, ty, .. } => {
                assert_eq!(*op, rue_ir::hir::UnaryOp::Neg);
                assert_eq!(ty, &RueType::I32);

                // Inner expression should also be i32
                assert_eq!(expr.ty(), &RueType::I32);
            }
            _ => panic!("Expected unary expression"),
        }
    }

    #[test]
    fn test_mixed_type_operations() {
        let source = r#"
            fn compute(a: i32, b: i64) -> i64 {
                let tmp: i64 = 2;
                to_i64(a) + b * tmp
            }
            
            fn main() -> bool {
                let fifty: i64 = 50;
                compute(10, 20) > fifty
            }
        "#;

        let result = parse_and_analyze(source).unwrap();

        // Check compute function
        let compute_fn = &result.hir.functions[0];
        assert_eq!(compute_fn.return_type, RueType::I64);

        // Check main function
        let main_fn = &result.hir.functions[1];
        assert_eq!(main_fn.return_type, RueType::Bool);

        let final_expr = main_fn.body.expr.as_ref().unwrap();
        assert_eq!(final_expr.ty(), &RueType::Bool);
    }
}
