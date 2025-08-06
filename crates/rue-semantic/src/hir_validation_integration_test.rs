//! Integration tests for HIR validation with the full semantic pipeline

#[cfg(test)]
mod tests {
    use crate::analyze_cst;
    use rue_parser::parse_with_recovery;

    #[test]
    fn test_hir_validation_catches_internal_inconsistency() {
        // This test would fail if we had an inconsistent HIR
        // For now, we just ensure validation runs on real programs
        let source = r#"
            fn add(a: i32, b: i32) -> i32 {
                a + b
            }
            
            fn main() -> i32 {
                add(10, 20)
            }
        "#;

        let ast = parse_with_recovery(source, "test.rue").unwrap();
        let result = analyze_cst(&ast);

        assert!(result.is_ok());
        let analysis = result.unwrap();

        // Verify HIR was built and validated
        assert_eq!(analysis.hir.functions.len(), 2);
    }

    #[test]
    fn test_validation_integrated_with_semantic_analysis() {
        // Test that type errors are caught during semantic analysis
        let source = r#"
            fn main() -> i32 {
                let x: i32 = true;
                x
            }
        "#;

        let ast = parse_with_recovery(source, "test.rue").unwrap();
        let result = analyze_cst(&ast);

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.message.contains("Type mismatch"));
    }

    #[test]
    fn test_complex_program_validation() {
        let source = r#"
            fn factorial(n: i64) -> i64 {
                let one: i64 = 1;
                if n <= one {
                    one
                } else {
                    n * factorial(n - one)
                }
            }
            
            fn fib(n: i32) -> i32 {
                if n <= 1 {
                    n
                } else {
                    fib(n - 1) + fib(n - 2)
                }
            }
            
            fn main() -> () {
                let result1: i64 = factorial(5);
                let result2: i32 = fib(10);
                println_i64(result1);
                println_i32(result2);
            }
        "#;

        let ast = parse_with_recovery(source, "test.rue").unwrap();
        let result = analyze_cst(&ast);

        if let Err(e) = &result {
            eprintln!("Error: {} at span {:?}", e.message, e.span);
        }
        assert!(result.is_ok());
        let analysis = result.unwrap();

        // Verify all functions are present and validated
        assert_eq!(analysis.hir.functions.len(), 3);
        assert!(analysis.scope.functions.contains_key("factorial"));
        assert!(analysis.scope.functions.contains_key("fib"));
        assert!(analysis.scope.functions.contains_key("main"));
    }
}
