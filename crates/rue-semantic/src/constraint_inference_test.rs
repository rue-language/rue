//! Tests for constraint-based type inference

#[cfg(test)]
mod tests {
    use crate::type_checker::TypeInferenceContext;
    use rue_ir::hir::BinOp;
    use rue_ir::types::RueType;

    #[test]
    fn test_constraint_solver() {
        // Test the constraint solver directly
        let mut ctx = TypeInferenceContext::new();

        // Create type variables
        let var1 = ctx.fresh_type_var();
        let var2 = ctx.fresh_type_var();
        let var3 = ctx.fresh_type_var();

        // Add constraints: var1 = i32, var2 = var1, var3 = var2
        ctx.add_concrete_constraint(var1, RueType::I32);
        ctx.add_equal_constraint(var2, var1);
        ctx.add_equal_constraint(var3, var2);

        // Solve constraints
        ctx.solve_constraints().expect("Should solve");

        // Check solutions
        assert_eq!(ctx.get_solution(var1), Some(&RueType::I32));
        assert_eq!(ctx.get_solution(var2), Some(&RueType::I32));
        assert_eq!(ctx.get_solution(var3), Some(&RueType::I32));
    }

    #[test]
    fn test_binary_constraint_solving() {
        let mut ctx = TypeInferenceContext::new();

        // Create type variables for: a + b = result
        let lhs_var = ctx.fresh_type_var();
        let rhs_var = ctx.fresh_type_var();
        let result_var = ctx.fresh_type_var();

        // Add binary constraint: lhs + rhs = result
        ctx.add_binary_constraint(BinOp::Add, lhs_var, rhs_var, result_var);

        // Add concrete constraint: result is i64
        ctx.add_concrete_constraint(result_var, RueType::I64);

        // Solve
        ctx.solve_constraints().expect("Should solve");

        // All should be i64
        assert_eq!(ctx.get_solution(lhs_var), Some(&RueType::I64));
        assert_eq!(ctx.get_solution(rhs_var), Some(&RueType::I64));
        assert_eq!(ctx.get_solution(result_var), Some(&RueType::I64));
    }

    #[test]
    fn test_numeric_literal_inference() {
        let ctx = TypeInferenceContext::new();

        // Test default inference - always defaults to i32 now
        assert_eq!(ctx.infer_numeric_literal(42, None), RueType::I32);
        // Large literals also default to i32 (will be caught as error during checking)
        assert_eq!(
            ctx.infer_numeric_literal(i32::MAX as i64 + 1, None),
            RueType::I32
        );

        // Test with type hints
        assert_eq!(
            ctx.infer_numeric_literal(42, Some(&RueType::I64)),
            RueType::I64
        );
        assert_eq!(
            ctx.infer_numeric_literal(42, Some(&RueType::I32)),
            RueType::I32
        );

        // Test with non-numeric type hint (returns the hint for proper error handling)
        assert_eq!(
            ctx.infer_numeric_literal(42, Some(&RueType::Bool)),
            RueType::Bool
        );
    }

    #[test]
    fn test_constraint_solving_integration() {
        // Test that constraint solving is actually being used
        // When we have an i64 context, literals should be inferred as i64
        let source = r#"
            fn main() -> i64 {
                let x: i64 = to_i64(100) + to_i64(200);
                x
            }
        "#;

        // This should compile successfully with constraint-based inference
        let ast = rue_parser::parse_with_recovery(source, "test.rue").expect("Failed to parse");
        let result = crate::analyze_cst(&ast);

        assert!(
            result.is_ok(),
            "Constraint solving should handle i64 inference: {result:?}"
        );

        // Test i32 inference (default)
        let source2 = r#"
            fn main() -> i32 {
                let x: i32 = 50 + 50;
                x
            }
        "#;

        let ast2 = rue_parser::parse_with_recovery(source2, "test2.rue").expect("Failed to parse");
        let result2 = crate::analyze_cst(&ast2);

        assert!(
            result2.is_ok(),
            "Constraint solving should handle i32 inference"
        );

        // Test that constraint solver properly infers types in complex expressions
        let source3 = r#"
            fn main() -> bool {
                let x: i32 = 10;
                let y: i32 = 20;
                x < y
            }
        "#;

        let ast3 = rue_parser::parse_with_recovery(source3, "test3.rue").expect("Failed to parse");
        let result3 = crate::analyze_cst(&ast3);

        assert!(
            result3.is_ok(),
            "Constraint solving should handle comparison operations"
        );
    }
}
