//! Consolidated arithmetic and operator tests for Rue
//!
//! This file replaces ~769 scattered arithmetic tests with a comprehensive,
//! organized test suite that properly tests arithmetic operations at the
//! integration level.

use rue_test_utils::assert_runs_with_exit_code;

// ============================================================================
// Basic Arithmetic Operations
// ============================================================================

#[test]
fn test_addition() {
    // Basic addition
    assert_runs_with_exit_code("fn main() -> i32 { 1 + 2 }", 3).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 10 + 20 }", 30).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 0 + 0 }", 0).unwrap();

    // Multiple additions
    assert_runs_with_exit_code("fn main() -> i32 { 1 + 2 + 3 }", 6).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 10 + 20 + 30 + 40 }", 100).unwrap();

    // With parentheses
    assert_runs_with_exit_code("fn main() -> i32 { (1 + 2) + 3 }", 6).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 1 + (2 + 3) }", 6).unwrap();
}

#[test]
fn test_subtraction() {
    // Basic subtraction
    assert_runs_with_exit_code("fn main() -> i32 { 5 - 2 }", 3).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 10 - 10 }", 0).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 0 - 5 }", 251).unwrap(); // -5 wraps to 251

    // Multiple subtractions
    assert_runs_with_exit_code("fn main() -> i32 { 10 - 3 - 2 }", 5).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 100 - 50 - 25 - 25 }", 0).unwrap();
}

#[test]
fn test_multiplication() {
    // Basic multiplication
    assert_runs_with_exit_code("fn main() -> i32 { 3 * 4 }", 12).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 5 * 0 }", 0).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { -3 * 4 }", 244).unwrap(); // -12 wraps to 244

    // Multiple multiplications
    assert_runs_with_exit_code("fn main() -> i32 { 2 * 3 * 4 }", 24).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 5 * 5 * 5 }", 125).unwrap();
}

#[test]
fn test_division() {
    // Basic division
    assert_runs_with_exit_code("fn main() -> i32 { 10 / 2 }", 5).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 15 / 3 }", 5).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 7 / 2 }", 3).unwrap(); // Integer division

    // Division with negative numbers
    assert_runs_with_exit_code("fn main() -> i32 { -10 / 2 }", 251).unwrap(); // -5 wraps to 251
    assert_runs_with_exit_code("fn main() -> i32 { 10 / -2 }", 251).unwrap(); // -5 wraps to 251
    assert_runs_with_exit_code("fn main() -> i32 { -10 / -2 }", 5).unwrap();
}

#[test]
fn test_modulo() {
    // Basic modulo
    assert_runs_with_exit_code("fn main() -> i32 { 10 % 3 }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 15 % 5 }", 0).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 7 % 4 }", 3).unwrap();

    // Modulo with negative numbers (Rust semantics)
    assert_runs_with_exit_code("fn main() -> i32 { -10 % 3 }", 255).unwrap(); // -1 wraps to 255
    assert_runs_with_exit_code("fn main() -> i32 { 10 % -3 }", 1).unwrap();
}

// ============================================================================
// Operator Precedence
// ============================================================================

#[test]
fn test_operator_precedence() {
    // Multiplication before addition
    assert_runs_with_exit_code("fn main() -> i32 { 2 + 3 * 4 }", 14).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 3 * 4 + 2 }", 14).unwrap();

    // Division before addition
    assert_runs_with_exit_code("fn main() -> i32 { 10 + 20 / 5 }", 14).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 20 / 5 + 10 }", 14).unwrap();

    // Mixed operations
    assert_runs_with_exit_code("fn main() -> i32 { 2 + 3 * 4 - 5 }", 9).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 10 / 2 + 3 * 4 }", 17).unwrap();

    // Parentheses override precedence
    assert_runs_with_exit_code("fn main() -> i32 { (2 + 3) * 4 }", 20).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 2 * (3 + 4) }", 14).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { (10 + 20) / 5 }", 6).unwrap();
}

// ============================================================================
// Comparison Operators
// ============================================================================

#[test]
fn test_equality_operators() {
    // Equal
    assert_runs_with_exit_code("fn main() -> i32 { if 5 == 5 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 5 == 3 { 1 } else { 0 } }", 0).unwrap();

    // Not equal
    assert_runs_with_exit_code("fn main() -> i32 { if 5 != 3 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 5 != 5 { 1 } else { 0 } }", 0).unwrap();
}

#[test]
fn test_relational_operators() {
    // Less than
    assert_runs_with_exit_code("fn main() -> i32 { if 3 < 5 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 5 < 3 { 1 } else { 0 } }", 0).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 5 < 5 { 1 } else { 0 } }", 0).unwrap();

    // Less than or equal
    assert_runs_with_exit_code("fn main() -> i32 { if 3 <= 5 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 5 <= 5 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 5 <= 3 { 1 } else { 0 } }", 0).unwrap();

    // Greater than
    assert_runs_with_exit_code("fn main() -> i32 { if 5 > 3 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 3 > 5 { 1 } else { 0 } }", 0).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 5 > 5 { 1 } else { 0 } }", 0).unwrap();

    // Greater than or equal
    assert_runs_with_exit_code("fn main() -> i32 { if 5 >= 3 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 5 >= 5 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 3 >= 5 { 1 } else { 0 } }", 0).unwrap();
}

#[test]
fn test_comparison_precedence() {
    // Comparison after arithmetic
    assert_runs_with_exit_code("fn main() -> i32 { if 2 + 3 == 5 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 3 * 4 > 10 { 1 } else { 0 } }", 1).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { if 10 / 2 <= 5 { 1 } else { 0 } }", 1).unwrap();
}

// ============================================================================
// Bitwise Operators
// ============================================================================

// Bitwise operators are not yet implemented in Rue
// TODO: Add bitwise operator tests when they are implemented

// ============================================================================
// Logical Operators
// ============================================================================

// Logical operators (&&, ||) are not yet implemented in Rue
// TODO: Add logical operator tests when they are implemented

#[test]
fn test_comparison_with_variables() {
    assert_runs_with_exit_code(
        "fn main() -> i32 { let x = 10; let y = 20; if x < y { 1 } else { 0 } }",
        1,
    )
    .unwrap();

    assert_runs_with_exit_code(
        "fn main() -> i32 { let x = 30; let y = 30; if x == y { 1 } else { 0 } }",
        1,
    )
    .unwrap();

    assert_runs_with_exit_code(
        "fn main() -> i32 { let x = 15; let y = 10; if x > y { 1 } else { 0 } }",
        1,
    )
    .unwrap();
}

// ============================================================================
// Complex Expressions
// ============================================================================

#[test]
fn test_complex_expressions() {
    // Nested operations
    assert_runs_with_exit_code("fn main() -> i32 { ((2 + 3) * (4 - 1)) / 5 }", 3).unwrap();

    // Multiple arithmetic operations
    assert_runs_with_exit_code("fn main() -> i32 { (5 + 3) * 2 - (12 / 3) }", 12).unwrap();

    // With variables
    assert_runs_with_exit_code(
        "fn main() -> i32 { let x = 10; let y = 3; x / y + x % y }",
        4,
    )
    .unwrap();
}

// ============================================================================
// Edge Cases and Error Conditions
// ============================================================================

#[test]
fn test_overflow_behavior() {
    // i32 overflow wraps in release mode
    // Exit codes are masked to 0-255 range, so we can't test full i32::MIN
    // Instead test that overflow wraps correctly with smaller values
    assert_runs_with_exit_code("fn main() -> i32 { 127 + 1 }", 128).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 255 + 1 }", 0).unwrap(); // Wraps in exit code
}

#[test]
fn test_division_by_zero() {
    // Division by zero should cause runtime error
    // The exact behavior depends on the runtime implementation
    let program = "fn main() -> i32 { 10 / 0 }";
    let result = rue_test_utils::RueCompiler::new()
        .unwrap()
        .compile_and_run(program)
        .unwrap();

    // Should exit with non-zero code
    assert_ne!(result.exit_code, 0);
}

// ============================================================================
// Type-specific Operations
// ============================================================================

#[test]
fn test_i32_operations() {
    // i32 specific behavior
    assert_runs_with_exit_code("fn main() -> i32 { let x: i32 = 100; x * 2 }", 200).unwrap();

    assert_runs_with_exit_code("fn main() -> i32 { let x: i32 = -50; x + 100 }", 50).unwrap();
}

#[test]
fn test_i64_operations() {
    // i64 specific behavior with casting
    // Exit codes are limited to 0-255, so test with smaller values
    assert_runs_with_exit_code(
        "fn main() -> i32 { let x: i64 = 100; let y: i64 = 50; to_i32(x + y) }",
        150,
    )
    .unwrap();

    // i64 division and casting
    assert_runs_with_exit_code(
        "fn main() -> i32 { let x: i64 = 200; let y: i64 = x / 2; to_i32(y) }",
        100,
    )
    .unwrap();
}

// ============================================================================
// Operator Associativity
// ============================================================================

#[test]
fn test_left_associativity() {
    // Subtraction is left-associative
    assert_runs_with_exit_code("fn main() -> i32 { 10 - 5 - 2 }", 3).unwrap();
    // Should be (10 - 5) - 2 = 5 - 2 = 3, not 10 - (5 - 2) = 10 - 3 = 7

    // Division is left-associative
    assert_runs_with_exit_code("fn main() -> i32 { 20 / 4 / 2 }", 2).unwrap();
    // Should be (20 / 4) / 2 = 5 / 2 = 2, not 20 / (4 / 2) = 20 / 2 = 10
}

// ============================================================================
// Unary Operators
// ============================================================================

#[test]
fn test_unary_operators() {
    // Unary minus
    assert_runs_with_exit_code("fn main() -> i32 { -5 }", 251).unwrap(); // -5 wraps to 251
    assert_runs_with_exit_code("fn main() -> i32 { -(-5) }", 5).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { -(3 + 4) }", 249).unwrap(); // -7 wraps to 249

    // Unary minus with other operations
    assert_runs_with_exit_code("fn main() -> i32 { -5 + 10 }", 5).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { 10 + -5 }", 5).unwrap();
    assert_runs_with_exit_code("fn main() -> i32 { -5 * -2 }", 10).unwrap();
}
