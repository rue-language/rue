//! Integration tests for type casting functions

use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_i64_to_i32_truncation() {
    let code = r#"
        fn main() -> i32 {
            let big: i64 = 4294967296;  // 2^32
            let result: i32 = to_i32(big);
            result  // Should be 0 (lower 32 bits)
        }
    "#;

    let dir = tempdir().unwrap();
    let source_path = dir.path().join("test.rue");
    let output_path = dir.path().join("test");

    fs::write(&source_path, code).unwrap();

    // Compile
    let output = Command::new(env!("CARGO_BIN_EXE_rue"))
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run compiler");

    assert!(
        output.status.success(),
        "Compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Run
    let output = Command::new(&output_path)
        .output()
        .expect("Failed to run compiled program");

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn test_i64_to_i32_negative() {
    let code = r#"
        fn main() -> i32 {
            let neg: i64 = -42;
            let result: i32 = to_i32(neg);
            result  // Should be -42
        }
    "#;

    let dir = tempdir().unwrap();
    let source_path = dir.path().join("test.rue");
    let output_path = dir.path().join("test");

    fs::write(&source_path, code).unwrap();

    // Compile
    let output = Command::new(env!("CARGO_BIN_EXE_rue"))
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run compiler");

    assert!(
        output.status.success(),
        "Compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Run
    let output = Command::new(&output_path)
        .output()
        .expect("Failed to run compiled program");

    // -42 as exit code will be 214 (256 - 42)
    assert_eq!(output.status.code(), Some(214));
}

#[test]
fn test_i32_to_i64_positive() {
    let code = r#"
        fn main() -> i64 {
            let small: i32 = 42;
            let result: i64 = to_i64(small);
            result  // Should be 42
        }
    "#;

    let dir = tempdir().unwrap();
    let source_path = dir.path().join("test.rue");
    let output_path = dir.path().join("test");

    fs::write(&source_path, code).unwrap();

    // Compile
    let output = Command::new(env!("CARGO_BIN_EXE_rue"))
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run compiler");

    assert!(
        output.status.success(),
        "Compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Run
    let output = Command::new(&output_path)
        .output()
        .expect("Failed to run compiled program");

    assert_eq!(output.status.code(), Some(42));
}

#[test]
fn test_i32_to_i64_negative() {
    let code = r#"
        fn main() -> i64 {
            let neg: i32 = -1;
            let result: i64 = to_i64(neg);
            let expected: i64 = -1;
            let zero: i64 = 0;
            let one: i64 = 1;
            if result == expected {
                zero  // Success
            } else {
                one  // Failure
            }
        }
    "#;

    let dir = tempdir().unwrap();
    let source_path = dir.path().join("test.rue");
    let output_path = dir.path().join("test");

    fs::write(&source_path, code).unwrap();

    // Compile
    let output = Command::new(env!("CARGO_BIN_EXE_rue"))
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run compiler");

    assert!(
        output.status.success(),
        "Compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Run
    let output = Command::new(&output_path)
        .output()
        .expect("Failed to run compiled program");

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn test_casting_with_operations() {
    let code = r#"
        fn main() -> i64 {
            let a: i32 = 100;
            let b: i64 = 200;
            let sum: i64 = to_i64(a) + b;
            
            // Verify sum is 300
            let expected: i64 = 300;
            let zero: i64 = 0;
            let one: i64 = 1;
            if sum == expected {
                zero
            } else {
                one
            }
        }
    "#;

    let dir = tempdir().unwrap();
    let source_path = dir.path().join("test.rue");
    let output_path = dir.path().join("test");

    fs::write(&source_path, code).unwrap();

    // Compile
    let output = Command::new(env!("CARGO_BIN_EXE_rue"))
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run compiler");

    assert!(
        output.status.success(),
        "Compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Run
    let output = Command::new(&output_path)
        .output()
        .expect("Failed to run compiled program");

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn test_casting_chain() {
    let code = r#"
        fn main() -> i32 {
            let original: i32 = 42;
            let extended: i64 = to_i64(original);
            let truncated: i32 = to_i32(extended);
            
            // Should get back original value
            if truncated == original {
                0
            } else {
                1
            }
        }
    "#;

    let dir = tempdir().unwrap();
    let source_path = dir.path().join("test.rue");
    let output_path = dir.path().join("test");

    fs::write(&source_path, code).unwrap();

    // Compile
    let output = Command::new(env!("CARGO_BIN_EXE_rue"))
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run compiler");

    assert!(
        output.status.success(),
        "Compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Run
    let output = Command::new(&output_path)
        .output()
        .expect("Failed to run compiled program");

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn test_casting_type_errors() {
    // Test wrong argument type for to_i32
    let code = r#"
        fn main() -> i32 {
            let x: i32 = 42;
            let result: i32 = to_i32(x);  // Should error: expecting i64
            result
        }
    "#;

    let dir = tempdir().unwrap();
    let source_path = dir.path().join("test.rue");
    let output_path = dir.path().join("test");

    fs::write(&source_path, code).unwrap();

    // Compile - should fail
    let output = Command::new(env!("CARGO_BIN_EXE_rue"))
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run compiler");

    assert!(!output.status.success(), "Compilation should have failed");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Type mismatch"));
}

#[test]
fn test_casting_with_print() {
    let code = r#"
        fn main() -> i32 {
            let big: i64 = 4294967338;  // 2^32 + 42
            let small: i32 = to_i32(big);
            println_i32(small);  // Should print 42
            
            let neg: i32 = -100;
            let extended: i64 = to_i64(neg);
            println_i64(extended);  // Should print -100
            
            0
        }
    "#;

    let dir = tempdir().unwrap();
    let source_path = dir.path().join("test.rue");
    let output_path = dir.path().join("test");

    fs::write(&source_path, code).unwrap();

    // Compile
    let output = Command::new(env!("CARGO_BIN_EXE_rue"))
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("Failed to run compiler");

    assert!(
        output.status.success(),
        "Compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Run
    let output = Command::new(&output_path)
        .output()
        .expect("Failed to run compiled program");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42\n"));
    assert!(stdout.contains("-100\n"));
}
