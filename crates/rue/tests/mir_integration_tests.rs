//! Integration tests for MIR compilation path

use std::process::Command;
use tempfile::TempDir;

fn compile_and_run(source: &str, use_mir: bool) -> Result<i32, String> {
    // Create a temporary directory for our files
    let temp_dir = TempDir::new().map_err(|e| e.to_string())?;

    // Write source to temporary file
    let source_path = temp_dir.path().join("test.rue");
    std::fs::write(&source_path, source).map_err(|e| e.to_string())?;

    // Create output path
    let output_path = temp_dir.path().join("test_output");

    // Compile
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rue"));
    if use_mir {
        cmd.arg("--use-mir");
    }
    cmd.arg(&source_path).arg("-o").arg(&output_path);

    let compile_output = cmd.output().map_err(|e| e.to_string())?;

    if !compile_output.status.success() {
        return Err(format!(
            "Compilation failed: {}",
            String::from_utf8_lossy(&compile_output.stderr)
        ));
    }

    // Run the compiled program
    let run_output = Command::new(&output_path)
        .output()
        .map_err(|e| e.to_string())?;

    // Return the exit code
    Ok(run_output.status.code().unwrap_or(-1))
}

#[test]
fn test_mir_simple_return() {
    let source = r#"
fn main() -> i32 {
    42
}
"#;

    let normal_result = compile_and_run(source, false).unwrap();
    let mir_result = compile_and_run(source, true).unwrap();

    assert_eq!(normal_result, mir_result);
    assert_eq!(normal_result, 42);
}

#[test]
fn test_mir_arithmetic() {
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;
    let y: i32 = 20;
    x + y * 2
}
"#;

    let normal_result = compile_and_run(source, false).unwrap();
    let mir_result = compile_and_run(source, true).unwrap();

    assert_eq!(normal_result, mir_result);
    assert_eq!(normal_result, 50); // 10 + 20 * 2
}

#[test]
fn test_mir_if_expression() {
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

    let normal_result = compile_and_run(source, false).unwrap();
    let mir_result = compile_and_run(source, true).unwrap();

    assert_eq!(normal_result, mir_result);
    assert_eq!(normal_result, 100);
}

#[test]
fn test_mir_while_loop() {
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

    let normal_result = compile_and_run(source, false).unwrap();
    let mir_result = compile_and_run(source, true).unwrap();

    assert_eq!(normal_result, mir_result);
    assert_eq!(normal_result, 15); // 5 + 4 + 3 + 2 + 1
}

#[test]
fn test_mir_function_call() {
    let source = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    add(10, 20) + add(5, 3)
}
"#;

    let normal_result = compile_and_run(source, false).unwrap();
    let mir_result = compile_and_run(source, true).unwrap();

    assert_eq!(normal_result, mir_result);
    assert_eq!(normal_result, 38); // 30 + 8
}

#[test]
fn test_mir_recursive_factorial() {
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

    let normal_result = compile_and_run(source, false).unwrap();
    let mir_result = compile_and_run(source, true).unwrap();

    assert_eq!(normal_result, mir_result);
    assert_eq!(normal_result, 120); // 5!
}

#[test]
fn test_mir_optimization_const_prop() {
    // This should benefit from constant propagation
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;
    let y: i32 = 20;
    let z: i32 = x + y;  // Should be optimized to 30
    z * 2                // Should be optimized to 60
}
"#;

    let normal_result = compile_and_run(source, false).unwrap();
    let mir_result = compile_and_run(source, true).unwrap();

    assert_eq!(normal_result, mir_result);
    assert_eq!(normal_result, 60);
}

#[test]
fn test_mir_optimization_cse() {
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

    let normal_result = compile_and_run(source, false).unwrap();
    let mir_result = compile_and_run(source, true).unwrap();

    assert_eq!(normal_result, mir_result);
    assert_eq!(normal_result, 60); // 30 + 30
}

#[test]
fn test_mir_optimization_dce() {
    // This should benefit from dead code elimination
    let source = r#"
fn main() -> i32 {
    let x: i32 = 10;     // Used
    let y: i32 = 20;     // Dead - never used
    let z: i32 = 30;     // Dead - never used
    x * 5
}
"#;

    let normal_result = compile_and_run(source, false).unwrap();
    let mir_result = compile_and_run(source, true).unwrap();

    assert_eq!(normal_result, mir_result);
    assert_eq!(normal_result, 50);
}
