//! Consolidated type system tests for Rue
//!
//! This file combines and organizes type-related tests that were previously
//! scattered across multiple files, providing comprehensive coverage of:
//! - Type inference
//! - Type checking and errors
//! - Type annotations
//! - Casting and conversions
//! - Struct/tuple/array types

use rue_test_utils::{assert_compile_error, assert_compiles, assert_runs_with_exit_code};

// ============================================================================
// Type Inference
// ============================================================================

#[test]
fn test_basic_type_inference() {
    // Integer literals default to i32
    assert_compiles("fn main() -> i32 { let x = 42; x }").unwrap();

    // Type inference from initialization
    assert_compiles("fn main() -> i32 { let x = 42; let y = x; y }").unwrap();

    // Type inference in expressions
    assert_compiles("fn main() -> i32 { let x = 1 + 2; x }").unwrap();
}

#[test]
fn test_type_inference_with_annotations() {
    // Explicit type overrides inference
    assert_compiles("fn main() -> i32 { let x: i64 = 42; to_i32(x) }").unwrap();

    // Mixed inference and annotation
    assert_compiles("fn main() -> i32 { let x: i32 = 42; let y = x; y }").unwrap();
}

#[test]
fn test_function_return_type_inference() {
    // Return type must match function signature
    assert_compiles(
        r#"
        fn get_value() -> i32 { 42 }
        fn main() -> i32 { get_value() }
    "#,
    )
    .unwrap();

    // Type error on mismatch
    assert_compile_error(
        r#"
        fn get_value() -> i64 { 42 }
        fn main() -> i32 { get_value() }
        "#,
        "Type mismatch",
    )
    .unwrap();
}

// ============================================================================
// Type Checking and Errors
// ============================================================================

#[test]
fn test_type_mismatch_in_assignment() {
    assert_compile_error(
        "fn main() { let x: i32 = 42; let y: i64 = x; }",
        "Type mismatch",
    )
    .unwrap();
}

#[test]
fn test_type_mismatch_in_binary_operations() {
    assert_compile_error(
        "fn main() { let x: i32 = 42; let y: i64 = 100; let z = x + y; }",
        "Type mismatch",
    )
    .unwrap();
}

#[test]
fn test_type_mismatch_in_function_arguments() {
    assert_compile_error(
        r#"
        fn takes_i32(x: i32) -> i32 { x }
        fn main() -> i32 { 
            let y: i64 = 42;
            takes_i32(y)
        }
        "#,
        "Type mismatch",
    )
    .unwrap();
}

#[test]
fn test_undefined_variable_error() {
    assert_compile_error("fn main() -> i32 { x }", "Undefined variable").unwrap();
}

// ============================================================================
// Type Annotations
// ============================================================================

#[test]
fn test_all_primitive_type_annotations() {
    // i32 type
    assert_compiles("fn main() -> i32 { let x: i32 = 42; x }").unwrap();

    // i64 type
    assert_compiles("fn main() -> i32 { let x: i64 = 42; to_i32(x) }").unwrap();

    // bool type
    assert_compiles("fn main() -> i32 { let x: bool = true; if x { 1 } else { 0 } }").unwrap();

    // unit type
    assert_compiles("fn main() -> i32 { let x: () = (); 0 }").unwrap();
}

#[test]
fn test_complex_type_annotations() {
    // Function parameter types
    assert_compiles(
        r#"
        fn add(x: i32, y: i32) -> i32 { x + y }
        fn main() -> i32 { add(1, 2) }
    "#,
    )
    .unwrap();

    // Multiple variables with types
    assert_compiles(
        r#"
        fn main() -> i32 {
            let x: i32 = 10;
            let y: i32 = 20;
            let z: i32 = x + y;
            z
        }
    "#,
    )
    .unwrap();
}

// ============================================================================
// Casting and Conversions
// ============================================================================

#[test]
fn test_i64_to_i32_casting() {
    // Basic casting
    assert_runs_with_exit_code("fn main() -> i32 { let x: i64 = 42; to_i32(x) }", 42).unwrap();

    // Casting with truncation
    assert_runs_with_exit_code(
        "fn main() -> i32 { let x: i64 = 256; to_i32(x) }",
        0, // Truncated to 8-bit exit code
    )
    .unwrap();
}

#[test]
fn test_i32_to_i64_casting() {
    // Basic widening
    assert_runs_with_exit_code(
        "fn main() -> i32 { let x: i32 = 42; let y: i64 = to_i64(x); to_i32(y) }",
        42,
    )
    .unwrap();

    // Sign extension
    assert_runs_with_exit_code(
        "fn main() -> i32 { let x: i32 = -1; let y: i64 = to_i64(x); to_i32(y) }",
        255, // -1 as exit code
    )
    .unwrap();
}

#[test]
fn test_casting_in_expressions() {
    assert_runs_with_exit_code(
        "fn main() -> i32 { let x: i64 = 100; let y: i32 = 50; to_i32(x) + y }",
        150,
    )
    .unwrap();
}

// ============================================================================
// Composite Types (Structs, Tuples, Arrays)
// ============================================================================

#[test]
fn test_struct_type_checking() {
    // Basic struct definition and usage
    assert_compiles(
        r#"
        struct Point { x: i32, y: i32 }
        fn main() -> i32 {
            let p = Point { x: 10, y: 20 };
            p.x + p.y
        }
    "#,
    )
    .unwrap();

    // Type error in struct field
    assert_compile_error(
        r#"
        struct Point { x: i32, y: i32 }
        fn main() {
            let p = Point { x: 10, y: true };  // bool instead of i32
        }
        "#,
        "type",
    )
    .unwrap();
}

#[test]
fn test_tuple_type_checking() {
    // Basic tuple
    assert_compiles(
        r#"
        fn main() -> i32 {
            let t = (10, 20);
            t.0 + t.1
        }
    "#,
    )
    .unwrap();

    // Mixed type tuple
    assert_compiles(
        r#"
        fn main() -> i32 {
            let t = (10, true, 30);
            if t.1 { t.0 } else { t.2 }
        }
    "#,
    )
    .unwrap();
}

#[test]
fn test_array_type_checking() {
    // Basic array
    assert_compiles(
        r#"
        fn main() -> i32 {
            let arr = [1, 2, 3, 4, 5];
            arr[0] + arr[4]
        }
    "#,
    )
    .unwrap();

    // Array type consistency
    assert_compile_error(
        r#"
        fn main() {
            let arr = [1, true, 3];  // Mixed types not allowed
        }
        "#,
        "type",
    )
    .unwrap();
}

// ============================================================================
// Function Types
// ============================================================================

#[test]
fn test_function_parameter_types() {
    // Correct parameter types
    assert_compiles(
        r#"
        fn add(x: i32, y: i32) -> i32 { x + y }
        fn main() -> i32 { add(10, 20) }
    "#,
    )
    .unwrap();

    // Wrong number of arguments
    assert_compile_error(
        r#"
        fn add(x: i32, y: i32) -> i32 { x + y }
        fn main() -> i32 { add(10) }
        "#,
        "argument",
    )
    .unwrap();
}

#[test]
fn test_recursive_function_types() {
    assert_compiles(
        r#"
        fn factorial(n: i32) -> i32 {
            if n <= 1 { 1 } else { n * factorial(n - 1) }
        }
        fn main() -> i32 { factorial(5) }
    "#,
    )
    .unwrap();
}

// ============================================================================
// Advanced Type Scenarios
// ============================================================================

#[test]
fn test_shadowing_with_different_types() {
    assert_compiles(
        r#"
        fn main() -> i32 {
            let x = 42;       // i32
            let x = true;     // bool - shadows previous x
            if x { 1 } else { 0 }
        }
    "#,
    )
    .unwrap();
}

#[test]
fn test_type_preservation_through_blocks() {
    // Test that types are consistent in if/else branches
    assert_compiles(
        r#"
        fn main() -> i32 {
            if true {
                42
            } else {
                100
            }
        }
    "#,
    )
    .unwrap();

    // This would be a type error - different types in branches
    assert_compile_error(
        r#"
        fn main() -> i32 {
            if true {
                42
            } else {
                true  // bool instead of i32
            }
        }
    "#,
        "type",
    )
    .unwrap();
}

#[test]
fn test_comparison_operators_return_bool() {
    assert_compiles(
        r#"
        fn main() -> i32 {
            let x = 10;
            let y = 20;
            let is_less: bool = x < y;
            if is_less { 1 } else { 0 }
        }
    "#,
    )
    .unwrap();
}

// ============================================================================
// Type Error Messages
// ============================================================================

#[test]
fn test_clear_type_error_messages() {
    // Test that type errors provide helpful messages
    assert_compile_error(
        "fn main() { let x: i32 = true; }",
        "bool", // Should mention the actual type
    )
    .unwrap();

    assert_compile_error(
        "fn main() { let x: bool = 42; }",
        "integer", // Error mentions "integer literal"
    )
    .unwrap();
}
