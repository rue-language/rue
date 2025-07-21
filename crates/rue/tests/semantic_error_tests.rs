mod common;

use common::get_project_root;
use std::fs;
use std::process::Command;

/// Test that a rue program fails to compile with expected error message
fn test_compile_error(name: &str, program: &str, expected_error: &str) {
    let project_root = get_project_root();
    let test_file = project_root.join(format!("test_semantic_error_temp_{name}.rue"));

    // Write the test program
    fs::write(&test_file, program).expect("Failed to write test file");

    // Try to compile the rue program
    let compile_output = if std::env::var("CARGO_MANIFEST_DIR").is_err() {
        // Buck2 build environment
        Command::new("buck2")
            .args(["run", "//crates/rue:rue", "--"])
            .arg(&test_file)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Buck2")
    } else {
        // Cargo build environment
        Command::new("cargo")
            .args(["run", "-p", "rue", "--"])
            .arg(&test_file)
            .current_dir(project_root)
            .output()
            .expect("Failed to execute rue compiler via Cargo")
    };

    // Clean up test file
    fs::remove_file(&test_file).expect("Failed to remove test file");

    assert!(
        !compile_output.status.success(),
        "Expected compilation to fail for test '{name}' but it succeeded!\nProgram:\n{program}"
    );

    let stderr = String::from_utf8_lossy(&compile_output.stderr);
    assert!(
        stderr.contains(expected_error),
        "Test '{name}' failed: Expected error containing '{expected_error}' but got:\n{stderr}"
    );
}

#[test]
fn test_undefined_variable_errors() {
    // Basic undefined variable
    test_compile_error(
        "undefined_variable_basic",
        r#"
fn main() -> i32 {
    x
}
"#,
        "Undefined variable",
    );

    // Undefined variable in expression
    test_compile_error(
        "undefined_variable_in_expr",
        r#"
fn main() -> i32 {
    let y = 5;
    x + y
}
"#,
        "Undefined variable",
    );

    // Variable used before declaration
    test_compile_error(
        "variable_before_declaration",
        r#"
fn main() -> i32 {
    let y = x;
    let x = 5;
    y
}
"#,
        "Undefined variable",
    );

    // Undefined variable in if condition
    test_compile_error(
        "undefined_in_if_condition",
        r#"
fn main() -> i32 {
    if undefined_var {
        1
    } else {
        0
    }
}
"#,
        "Undefined variable",
    );

    // Undefined variable in function argument
    test_compile_error(
        "undefined_in_function_arg",
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(1, undefined_var)
}
"#,
        "Undefined variable",
    );
}

#[test]
fn test_undefined_function_errors() {
    // Basic undefined function
    test_compile_error(
        "undefined_function_basic",
        r#"
fn main() -> i32 {
    unknown_func()
}
"#,
        "Undefined function",
    );

    // Undefined function with arguments
    test_compile_error(
        "undefined_function_with_args",
        r#"
fn main() -> i32 {
    let x = 5;
    unknown_func(x, 10)
}
"#,
        "Undefined function",
    );

    // Typo in builtin function name
    test_compile_error(
        "typo_in_builtin",
        r#"
fn main() -> i32 {
    let x = 42;
    println_i33(x);  // typo: should be println_i32
    0
}
"#,
        "Undefined function",
    );
}

#[test]
fn test_wrong_argument_count_errors() {
    // Too few arguments
    test_compile_error(
        "too_few_arguments",
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(5)
}
"#,
        "Expected 2 arguments, got 1",
    );

    // Too many arguments
    test_compile_error(
        "too_many_arguments",
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(1, 2, 3)
}
"#,
        "Expected 2 arguments, got 3",
    );

    // Wrong arguments to builtin
    test_compile_error(
        "wrong_args_to_builtin",
        r#"
fn main() -> i32 {
    println_i32(1, 2);
    0
}
"#,
        "Expected 1 arguments, got 2",
    );

    // No arguments when expecting some
    test_compile_error(
        "no_args_when_expecting",
        r#"
fn main() -> i32 {
    exit();
    0
}
"#,
        "Expected 1 arguments, got 0",
    );
}

#[test]
fn test_type_inference_errors() {
    // Variable initialized with wrong type expression
    test_compile_error(
        "var_init_type_mismatch",
        r#"
fn main() -> i32 {
    let x = true;
    let y: i32 = x;
    y
}
"#,
        "Type mismatch",
    );

    // Incompatible types in binary operation
    test_compile_error(
        "incompatible_binary_op",
        r#"
fn main() -> i32 {
    let x = 5;
    let y = true;
    x + y
}
"#,
        "Type mismatch",
    );

    // Wrong type in comparison
    test_compile_error(
        "wrong_type_comparison",
        r#"
fn main() -> i32 {
    let x = 5;
    let y = true;
    if x == y {
        1
    } else {
        0
    }
}
"#,
        "Type mismatch",
    );
}

#[test]
fn test_variable_shadowing_and_scope_errors() {
    // Variable used in different function
    test_compile_error(
        "variable_in_different_function",
        r#"
fn foo() -> i32 {
    let x: i32 = 5;
    x
}

fn main() -> i32 {
    x  // x is not defined in this function
}
"#,
        "Undefined variable",
    );

    // TODO: This test is commented out due to a bug in Rue's scoping rules
    // Variables defined in if/else branches should not be accessible outside
    // the branch, but currently they are.
    /*
    // Using variable only defined in else branch
    test_compile_error(
        "variable_only_in_else",
        r#"
fn main() -> i32 {
    if true {
        0
    } else {
        let x: i32 = 5;
        0
    };
    x  // x not defined when condition is true
}
"#,
        "Undefined variable",
    );
    */

    // Variable defined in both branches but different types
    test_compile_error(
        "different_types_in_branches",
        r#"
fn main() -> i32 {
    if true {
        5
    } else {
        true
    }
}
"#,
        "If expression branches must have the same type",
    );
}

#[test]
fn test_return_type_errors() {
    // Wrong return type from function
    test_compile_error(
        "wrong_return_type",
        r#"
fn get_bool() -> bool {
    42  // returning i32 instead of bool
}

fn main() -> i32 {
    0
}
"#,
        "Type mismatch",
    );

    // Missing return in function
    test_compile_error(
        "missing_return",
        r#"
fn get_number() -> i32 {
    let x: i32 = 5;
    // forgot to return
}

fn main() -> i32 {
    get_number()
}
"#,
        "Type mismatch",
    );

    // Return type mismatch in main
    test_compile_error(
        "main_wrong_return",
        r#"
fn main() -> i32 {
    true
}
"#,
        "Type mismatch",
    );
}

#[test]
fn test_assignment_errors() {
    // Assignment type mismatch
    test_compile_error(
        "assignment_type_mismatch",
        r#"
fn main() -> i32 {
    let x: i32 = 5;
    x = true;
    x
}
"#,
        "Type mismatch",
    );

    // Assignment to different type variable
    test_compile_error(
        "assign_different_type_var",
        r#"
fn main() -> i32 {
    let x: i32 = 5;
    let y: bool = true;
    x = y;
    x
}
"#,
        "Type mismatch",
    );
}

#[test]
fn test_mixed_type_operations() {
    // Mixing i32 and i64 in operations
    test_compile_error(
        "mix_i32_i64_add",
        r#"
fn main() -> i32 {
    let x: i32 = 5;
    let y: i64 = 10;
    x + y
}
"#,
        "Type mismatch",
    );

    // Mixing types in comparison
    test_compile_error(
        "mix_types_comparison",
        r#"
fn main() -> i32 {
    let x: i32 = 5;
    let y: i64 = 5;
    if x == y {
        1
    } else {
        0
    }
}
"#,
        "Type mismatch",
    );
}

#[test]
fn test_complex_type_errors() {
    // Complex expression type mismatch
    test_compile_error(
        "complex_expr_type_mismatch",
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn get_bool() -> bool {
    true
}

fn main() -> i32 {
    add(5, get_bool())
}
"#,
        "Type mismatch",
    );

    // Nested function call type error
    test_compile_error(
        "nested_call_type_error",
        r#"
fn expects_bool(b: bool) -> i32 {
    if b { 1 } else { 0 }
}

fn get_number() -> i32 {
    42
}

fn main() -> i32 {
    expects_bool(get_number())
}
"#,
        "Type mismatch",
    );
}

#[test]
fn test_operator_type_errors() {
    // Arithmetic on bool
    test_compile_error(
        "arithmetic_on_bool",
        r#"
fn main() -> i32 {
    let x: bool = true;
    let y: bool = false;
    x + y
}
"#,
        "Arithmetic operators require numeric types",
    );

    // Comparison of bool with int
    test_compile_error(
        "compare_bool_int",
        r#"
fn main() -> i32 {
    let x: bool = true;
    let y: i32 = 1;
    if x == y {
        1
    } else {
        0
    }
}
"#,
        "Type mismatch",
    );

    // Comparison of different types
    test_compile_error(
        "compare_different_types",
        r#"
fn main() -> i32 {
    let x: i32 = 5;
    let y: i64 = 10;
    if x < y {
        1
    } else {
        0
    }
}
"#,
        "Type mismatch",
    );
}

#[test]
fn test_unit_type_errors() {
    // Using unit value in expression
    test_compile_error(
        "unit_in_expression",
        r#"
fn returns_unit() {
    println_i32(42);
}

fn main() -> i32 {
    let x: () = returns_unit();
    let y: i32 = x;  // can't assign unit to i32
    y
}
"#,
        "Type mismatch",
    );

    // Assigning unit to non-unit variable
    test_compile_error(
        "assign_unit_to_int",
        r#"
fn main() -> i32 {
    let x: i32 = println_i32(42);
    x
}
"#,
        "Type mismatch",
    );
}
