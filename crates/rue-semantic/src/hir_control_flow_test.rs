//! Tests for HIR control flow lowering

#[cfg(test)]
mod tests {
    use crate::analyze_cst;
    use rue_ir::hir::{HirExpr, HirStatement};
    use rue_lexer::Lexer;

    fn parse_and_analyze(source: &str) -> (Option<rue_ir::hir::HirProgram>, Vec<String>) {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let ast = rue_parser::parse(tokens).unwrap();
        let result = analyze_cst(&ast);
        match result {
            Ok(analysis_result) => (Some(analysis_result.hir), vec![]),
            Err(e) => (None, vec![e.message]),
        }
    }

    #[test]
    fn test_if_lowering() {
        let source = r#"
            fn main() -> i32 {
                if 5 > 3 {
                    10
                } else {
                    20
                }
            }
        "#;

        let (hir, errors) = parse_and_analyze(source);
        if !errors.is_empty() {
            for error in &errors {
                eprintln!("Error: {error}");
            }
        }
        assert!(errors.is_empty());
        assert!(hir.is_some());

        let hir = hir.unwrap();
        assert_eq!(hir.functions.len(), 1);

        let main_fn = &hir.functions[0];
        assert_eq!(main_fn.name, "main");

        // Check that the body contains an if expression
        let expr = main_fn.body.expr.as_ref().unwrap();
        match expr.as_ref() {
            HirExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                // Check condition is a comparison
                match cond.as_ref() {
                    HirExpr::Binary { op, .. } => {
                        assert_eq!(*op, rue_ir::hir::BinOp::Gt);
                    }
                    _ => panic!("Expected binary expression for condition"),
                }

                // Check then block returns 10
                let then_expr = then_block.expr.as_ref().unwrap();
                match then_expr.as_ref() {
                    HirExpr::Literal { lit, .. } => {
                        assert_eq!(*lit, rue_ir::hir::HirLiteral::Int32(10));
                    }
                    _ => panic!("Expected literal in then block"),
                }

                // Check else block returns 20
                assert!(else_block.is_some());
                let else_block = else_block.as_ref().unwrap();
                let else_expr = else_block.expr.as_ref().unwrap();
                match else_expr.as_ref() {
                    HirExpr::Literal { lit, .. } => {
                        assert_eq!(*lit, rue_ir::hir::HirLiteral::Int32(20));
                    }
                    _ => panic!("Expected literal in else block"),
                }
            }
            _ => panic!("Expected if expression"),
        }
    }

    #[test]
    fn test_while_lowering() {
        let source = r#"
            fn main() -> i32 {
                let x: i32 = 0;
                while x < 10 {
                    x = x + 1;
                    ()
                };
                x
            }
        "#;

        let (hir, errors) = parse_and_analyze(source);
        assert!(errors.is_empty());
        assert!(hir.is_some());

        let hir = hir.unwrap();
        let main_fn = &hir.functions[0];

        // Check statements
        assert_eq!(main_fn.body.statements.len(), 2);

        // First statement should be let
        match &main_fn.body.statements[0] {
            HirStatement::Let { name, ty, init, .. } => {
                assert_eq!(name, "x");
                assert_eq!(*ty, rue_ir::types::RueType::I32);
                match init {
                    HirExpr::Literal { lit, .. } => {
                        assert_eq!(*lit, rue_ir::hir::HirLiteral::Int32(0));
                    }
                    _ => panic!("Expected literal in let init"),
                }
            }
            _ => panic!("Expected let statement"),
        }

        // Second statement should be while loop
        match &main_fn.body.statements[1] {
            HirStatement::Expr(HirExpr::While { cond, body, .. }) => {
                // Check condition
                match cond.as_ref() {
                    HirExpr::Binary { op, .. } => {
                        assert_eq!(*op, rue_ir::hir::BinOp::Lt);
                    }
                    _ => panic!("Expected binary expression for while condition"),
                }

                // Check body contains assignment
                assert_eq!(body.statements.len(), 1);
                match &body.statements[0] {
                    HirStatement::Assign { name, value, .. } => {
                        assert_eq!(name, "x");
                        match value {
                            HirExpr::Binary { op, .. } => {
                                assert_eq!(*op, rue_ir::hir::BinOp::Add);
                            }
                            _ => panic!("Expected binary expression in assignment"),
                        }
                    }
                    _ => panic!("Expected assignment in while body"),
                }
            }
            _ => panic!("Expected while loop"),
        }

        // Final expression should be x
        let final_expr = main_fn.body.expr.as_ref().unwrap();
        match final_expr.as_ref() {
            HirExpr::Var { name, .. } => {
                assert_eq!(name, "x");
            }
            _ => panic!("Expected variable reference"),
        }
    }

    #[test]
    fn test_if_without_else() {
        let source = r#"
            fn main() -> () {
                if true {
                    42;
                };
            }
        "#;

        let (hir, errors) = parse_and_analyze(source);
        assert!(errors.is_empty());
        assert!(hir.is_some());

        let hir = hir.unwrap();
        let main_fn = &hir.functions[0];

        // Should have one statement (the if)
        assert_eq!(main_fn.body.statements.len(), 1);
        match &main_fn.body.statements[0] {
            HirStatement::Expr(HirExpr::If { else_block, .. }) => {
                assert!(else_block.is_none());
            }
            _ => panic!("Expected if expression"),
        }
    }

    #[test]
    fn test_nested_control_flow() {
        let source = r#"
            fn main() -> i32 {
                let sum: i32 = 0;
                let i: i32 = 0;
                while i < 5 {
                    if i % 2 == 0 {
                        sum = sum + i;
                    };
                    i = i + 1;
                    ()
                };
                sum
            }
        "#;

        let (hir, errors) = parse_and_analyze(source);
        assert!(errors.is_empty());
        assert!(hir.is_some());

        let hir = hir.unwrap();
        let main_fn = &hir.functions[0];

        // Should have 3 statements: two lets and a while
        assert_eq!(main_fn.body.statements.len(), 3);

        // Check the while loop
        match &main_fn.body.statements[2] {
            HirStatement::Expr(HirExpr::While { body, .. }) => {
                // While body should have if and assignment
                assert_eq!(body.statements.len(), 2);

                // First statement in while body should be if
                match &body.statements[0] {
                    HirStatement::Expr(HirExpr::If { .. }) => {
                        // Good, found nested if
                    }
                    _ => panic!("Expected if expression in while body"),
                }
            }
            _ => panic!("Expected while loop"),
        }
    }
}
