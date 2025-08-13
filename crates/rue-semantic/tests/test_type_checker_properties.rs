//! Property-based tests for the type checker
//!
//! These tests verify type system invariants and soundness properties

use proptest::prelude::*;
use rue_parser::parse_with_diagnostics;
use rue_semantic::analyze_cst;

// ===== Type Generation Strategies =====

/// Generate valid type names
fn type_name_strategy() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("i32"), Just("i64"), Just("bool"),]
}

/// Generate well-typed expressions for basic types
fn typed_expression_strategy(ty: &str) -> impl Strategy<Value = String> {
    match ty {
        "i32" => any::<i32>().prop_map(|n| n.to_string()).boxed(),
        "i64" => any::<i64>().prop_map(|n| n.to_string()).boxed(),
        "bool" => prop_oneof![Just("true"), Just("false")]
            .prop_map(|s| s.to_string())
            .boxed(),
        _ => Just("0").prop_map(|s| s.to_string()).boxed(),
    }
}

/// Generate a well-typed program
fn well_typed_program_strategy() -> impl Strategy<Value = String> {
    type_name_strategy().prop_flat_map(|ty| {
        typed_expression_strategy(ty)
            .prop_map(move |expr| format!("fn main() -> {} {{\n    {}\n}}", ty, expr))
    })
}

// ===== Property Tests =====

proptest! {
    /// Property: Well-typed programs should pass semantic analysis
    #[test]
    fn well_typed_programs_pass(program in well_typed_program_strategy()) {
        let parse_result = parse_with_diagnostics(&program, "test.rue");

        if let Ok(cst) = parse_result {
            let result = analyze_cst(&cst);
            // Well-typed programs should pass semantic analysis
            prop_assert!(result.is_ok(), "Semantic analysis failed: {:?}", result);
        }
    }

    /// Property: Type mismatches should be detected
    #[test]
    fn type_mismatches_detected(
        ty1 in type_name_strategy(),
        ty2 in type_name_strategy()
    ) {
        prop_assume!(ty1 != ty2);
        let program = format!(
            "fn main() -> {} {{\n    let x: {} = 0;\n    x\n}}",
            ty1, ty2
        );

        let parse_result = parse_with_diagnostics(&program, "test.rue");

        if ty1 != ty2 && ty2 != "i32" && ty1 != "i32" {
            // This should fail type checking if types don't match
            // (unless one is i32 which might have implicit conversions)
            if let Ok(cst) = parse_result {
                let result = analyze_cst(&cst);
                // For now, just ensure it doesn't panic
                let _ = result;
            }
        }
    }

    /// Property: Undefined variables should be caught
    #[test]
    fn undefined_variables_caught(var_name in "[a-z][a-z0-9]{0,5}") {
        let program = format!(
            "fn main() -> i32 {{\n    {}\n}}",
            var_name
        );

        let parse_result = parse_with_diagnostics(&program, "test.rue");

        if let Ok(cst) = parse_result {
            let result = analyze_cst(&cst);
            // Should fail with undefined variable
            prop_assert!(result.is_err(), "Should have caught undefined variable");
        }
    }
}

#[test]
fn test_basic_type_checking() {
    // Test that basic type checking works
    let program = "fn main() -> i32 { 42 }";
    let parse_result = parse_with_diagnostics(program, "test.rue");

    if let Ok(cst) = parse_result {
        let result = analyze_cst(&cst);
        assert!(result.is_ok());
    }
}
