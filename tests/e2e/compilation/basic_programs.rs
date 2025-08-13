//! End-to-end compilation tests for basic programs

use rue_test_utils::{assert_compiles, assert_compile_error};

#[test]
fn test_compile_hello_world() {
    assert_compiles(r#"
        fn main() -> i32 {
            println("Hello, World!");
            0
        }
    "#).unwrap();
}

#[test]
fn test_compile_fibonacci() {
    assert_compiles(r#"
        fn fibonacci(n: i32) -> i32 {
            if n <= 1 {
                n
            } else {
                fibonacci(n - 1) + fibonacci(n - 2)
            }
        }
        
        fn main() -> i32 {
            fibonacci(10)
        }
    "#).unwrap();
}

#[test]
fn test_compile_with_structs() {
    assert_compiles(r#"
        struct Person {
            age: i32,
            id: i64
        }
        
        fn main() -> i32 {
            let person = Person { age: 25, id: 12345 };
            person.age
        }
    "#).unwrap();
}

#[test]
fn test_compile_with_arrays() {
    assert_compiles(r#"
        fn main() -> i32 {
            let numbers = [1, 2, 3, 4, 5];
            let sum = numbers[0] + numbers[1] + numbers[2] + numbers[3] + numbers[4];
            sum
        }
    "#).unwrap();
}

#[test]
fn test_compile_with_tuples() {
    assert_compiles(r#"
        fn main() -> i32 {
            let pair = (10, 20);
            let triple = (1, true, 3);
            pair.0 + pair.1
        }
    "#).unwrap();
}

#[test]
fn test_compile_error_undefined_variable() {
    assert_compile_error(r#"
        fn main() -> i32 {
            undefined_var
        }
    "#, "undefined").unwrap();
}

#[test]
fn test_compile_error_type_mismatch() {
    assert_compile_error(r#"
        fn main() -> i32 {
            let x: bool = 42;
            0
        }
    "#, "type").unwrap();
}

#[test]
fn test_compile_error_wrong_return_type() {
    assert_compile_error(r#"
        fn returns_bool() -> bool {
            42
        }
        
        fn main() -> i32 {
            0
        }
    "#, "type").unwrap();
}