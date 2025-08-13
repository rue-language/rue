//! End-to-end runtime execution tests

use rue_test_utils::{assert_runs_with_exit_code, assert_program_output};

#[test]
fn test_run_simple_arithmetic() {
    assert_runs_with_exit_code(r#"
        fn main() -> i32 {
            10 + 20 + 30
        }
    "#, 60).unwrap();
}

#[test]
fn test_run_factorial() {
    assert_runs_with_exit_code(r#"
        fn factorial(n: i32) -> i32 {
            if n <= 1 {
                1
            } else {
                n * factorial(n - 1)
            }
        }
        
        fn main() -> i32 {
            factorial(5)  // 5! = 120
        }
    "#, 120).unwrap();
}

#[test]
fn test_run_fibonacci() {
    assert_runs_with_exit_code(r#"
        fn fibonacci(n: i32) -> i32 {
            if n <= 1 {
                n
            } else {
                fibonacci(n - 1) + fibonacci(n - 2)
            }
        }
        
        fn main() -> i32 {
            fibonacci(10)  // 10th fibonacci number is 55
        }
    "#, 55).unwrap();
}

#[test]
fn test_run_with_loops() {
    assert_runs_with_exit_code(r#"
        fn main() -> i32 {
            let sum = 0;
            let i = 1;
            while i <= 10 {
                sum = sum + i;
                i = i + 1;
            };
            sum  // Sum of 1 to 10 is 55
        }
    "#, 55).unwrap();
}

#[test]
fn test_run_with_nested_loops() {
    assert_runs_with_exit_code(r#"
        fn main() -> i32 {
            let result = 0;
            let i = 0;
            while i < 3 {
                let j = 0;
                while j < 4 {
                    result = result + 1;
                    j = j + 1;
                };
                i = i + 1;
            };
            result  // 3 * 4 = 12
        }
    "#, 12).unwrap();
}

#[test]
fn test_run_with_complex_control_flow() {
    assert_runs_with_exit_code(r#"
        fn compute(x: i32) -> i32 {
            if x < 0 {
                -x
            } else if x == 0 {
                100
            } else if x < 10 {
                x * 2
            } else {
                x / 2
            }
        }
        
        fn main() -> i32 {
            compute(5)  // 5 * 2 = 10
        }
    "#, 10).unwrap();
}

#[test]
fn test_run_with_multiple_functions() {
    assert_runs_with_exit_code(r#"
        fn add(a: i32, b: i32) -> i32 {
            a + b
        }
        
        fn multiply(a: i32, b: i32) -> i32 {
            a * b
        }
        
        fn compute(x: i32, y: i32) -> i32 {
            add(multiply(x, 2), multiply(y, 3))
        }
        
        fn main() -> i32 {
            compute(5, 10)  // (5 * 2) + (10 * 3) = 10 + 30 = 40
        }
    "#, 40).unwrap();
}

#[test]
fn test_run_with_shadowing() {
    assert_runs_with_exit_code(r#"
        fn main() -> i32 {
            let x = 10;
            let x = x + 5;  // Shadow with 15
            let x = x * 2;  // Shadow with 30
            x
        }
    "#, 30).unwrap();
}