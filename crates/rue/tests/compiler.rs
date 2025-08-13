//! Integration tests for the compiler

use rue_test_utils::assert_runs_with_exit_code;

#[test]
fn test_simple_return() {
    let source = r#"
fn main() -> i32 {
    42
}
"#;

    assert_runs_with_exit_code(source, 42).unwrap();
}

#[test]
fn test_arithmetic() {
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;
    let y: i32 = 20;
    x + y * 2
}
"#;

    assert_runs_with_exit_code(source, 50).unwrap(); // 10 + 20 * 2
}

#[test]
fn test_if_expression() {
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;
    if x > 5 {
        100
    } else {
        200
    }
}
"#;

    assert_runs_with_exit_code(source, 100).unwrap();
}

#[test]
fn test_while_loop() {
    let source = r#"
fn main() -> i32 {
    let count: i32 = 5;
    let sum: i32 = 0;
    while count > 0 {
        sum = sum + count;
        count = count - 1;
    };
    sum
}
"#;

    assert_runs_with_exit_code(source, 15).unwrap(); // 5 + 4 + 3 + 2 + 1
}

#[test]
fn test_function_call() {
    let source = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(10, 20) + add(5, 3)
}
"#;

    assert_runs_with_exit_code(source, 38).unwrap(); // 30 + 8
}

#[test]
fn test_recursive_factorial() {
    let source = r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(5)
}
"#;

    assert_runs_with_exit_code(source, 120).unwrap(); // 5!
}

#[test]
fn test_optimization_const_prop() {
    // This should benefit from constant propagation
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;
    let y: i32 = 20;
    let z: i32 = x + y;  // Should be optimized to 30
    z * 2                // Should be optimized to 60
}
"#;

    assert_runs_with_exit_code(source, 60).unwrap();
}

#[test]
fn test_optimization_cse() {
    // This should benefit from common subexpression elimination
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;
    let y: i32 = 20;
    let a: i32 = x + y;
    let b: i32 = x + y;  // Same as a, should reuse
    a + b
}
"#;

    assert_runs_with_exit_code(source, 60).unwrap(); // 30 + 30
}

#[test]
fn test_optimization_dce() {
    // This should benefit from dead code elimination
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;     // Used
    let y: i32 = 20;     // Dead - never used
    let z: i32 = 30;     // Dead - never used
    x * 5
}
"#;

    assert_runs_with_exit_code(source, 50).unwrap();
}
