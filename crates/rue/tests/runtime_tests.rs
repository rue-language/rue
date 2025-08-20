mod common;
mod test_utils;

use common::get_project_root;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use test_utils::get_rue_binary;

/// Compile a Rue program from a string
fn compile_rue_code(code: &str, output_path: &Path) -> Result<(), String> {
    let project_root = get_project_root();

    // Wrap code in main function if not already wrapped
    let wrapped_code = if code.trim().starts_with("fn main()") || code.trim().starts_with("fn ") {
        // Already has function definition(s), assume complete program
        if !code.contains("fn main()") {
            // Has functions but no main, add main that returns 0
            format!("{code}\n\nfn main() -> i64 {{\n    0\n}}")
        } else {
            code.to_string()
        }
    } else {
        // No function definitions, wrap in main
        format!("fn main() -> i64 {{\n{code}\n0\n}}")
    };

    // Create a temporary source file with unique name to avoid conflicts
    let pid = std::process::id();
    let tid = std::thread::current().id();
    let temp_source = project_root.join(format!("test_temp_{pid}_{tid:?}.rue"));
    fs::write(&temp_source, wrapped_code)
        .map_err(|e| format!("Failed to write source file: {e}"))?;

    // Clean up any existing executable
    if output_path.exists() {
        fs::remove_file(output_path)
            .map_err(|e| format!("Failed to remove existing executable: {e}"))?;
    }

    // Compile the rue program
    let rue_binary = get_rue_binary();
    let compile_output = Command::new(&rue_binary)
        .arg(&temp_source)
        .arg("-o")
        .arg(output_path)
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("Failed to execute rue compiler: {e}"))?;

    // Clean up temp source
    fs::remove_file(&temp_source).ok();

    if !compile_output.status.success() {
        return Err(format!(
            "Compilation failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&compile_output.stdout),
            String::from_utf8_lossy(&compile_output.stderr)
        ));
    }

    Ok(())
}

/// Test a Rue program with expected output and exit code
fn test_runtime_program(code: &str, expected_output: &str, expected_exit_code: i32) {
    let project_root = get_project_root();
    let pid = std::process::id();
    let tid = std::thread::current().id();
    let executable_path = project_root.join(format!("test_runtime_exe_{pid}_{tid:?}"));

    // Compile the program
    if let Err(e) = compile_rue_code(code, &executable_path) {
        panic!("Failed to compile program: {e}");
    }

    // Run the compiled executable
    let output = Command::new(&executable_path)
        .current_dir(project_root)
        .output()
        .expect("Failed to execute compiled program");

    // Check output
    let actual_output = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        actual_output, expected_output,
        "Output mismatch.\nExpected:\n{expected_output}\nActual:\n{actual_output}"
    );

    // Check exit code
    let actual_exit_code = output.status.code().unwrap_or(-1);
    assert_eq!(
        actual_exit_code, expected_exit_code,
        "Exit code mismatch. Expected: {expected_exit_code}, Actual: {actual_exit_code}"
    );

    // Clean up
    fs::remove_file(&executable_path).ok();
}

/// Test a Rue program with input provided via stdin
fn test_runtime_program_with_input(
    code: &str,
    input: &str,
    expected_output: &str,
    expected_exit_code: i32,
) {
    let project_root = get_project_root();
    let pid = std::process::id();
    let tid = std::thread::current().id();
    let executable_path = project_root.join(format!("test_runtime_exe_{pid}_{tid:?}"));

    // Compile the program
    if let Err(e) = compile_rue_code(code, &executable_path) {
        panic!("Failed to compile program: {e}");
    }

    // Run the compiled executable with stdin
    let mut child = Command::new(&executable_path)
        .current_dir(project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn compiled program");

    // Write input to stdin
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(input.as_bytes())
            .expect("Failed to write to stdin");
    }

    // Wait for completion and get output
    let output = child
        .wait_with_output()
        .expect("Failed to wait for program");

    // Check output
    let actual_output = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        actual_output, expected_output,
        "Output mismatch.\nExpected:\n{expected_output}\nActual:\n{actual_output}"
    );

    // Check exit code
    let actual_exit_code = output.status.code().unwrap_or(-1);
    assert_eq!(
        actual_exit_code, expected_exit_code,
        "Exit code mismatch. Expected: {expected_exit_code}, Actual: {actual_exit_code}"
    );

    // Clean up
    fs::remove_file(&executable_path).ok();
}

// Test exit function
#[test]
fn test_exit_function() {
    test_runtime_program("exit(0);", "", 0);

    test_runtime_program("exit(42);", "", 42);

    test_runtime_program("exit(255);", "", 255);
}

// Test println_i64
#[test]
fn test_println_i64() {
    test_runtime_program("println_i64(0);", "0\n", 0);

    test_runtime_program("println_i64(42);", "42\n", 0);

    test_runtime_program(
        "let zero: i64 = 0; let fortytwo: i64 = 42; println_i64(zero - fortytwo);",
        "-42\n",
        0,
    );

    test_runtime_program(
        "println_i64(9223372036854775807);", // i64::MAX
        "9223372036854775807\n",
        0,
    );

    test_runtime_program(
        "let x: i64 = 9223372036854775807; let one: i64 = 1; let y: i64 = x + one; let zero: i64 = 0; println_i64(zero - y);", // i64::MIN
        "-9223372036854775808\n",
        0,
    );
}

// Test println_i32
#[test]
fn test_println_i32() {
    test_runtime_program("let x: i32 = 0; println_i32(x);", "0\n", 0);

    test_runtime_program("let x: i32 = 42; println_i32(x);", "42\n", 0);

    test_runtime_program(
        "let zero: i32 = 0; let x: i32 = zero - 42; println_i32(x);",
        "-42\n",
        0,
    );

    test_runtime_program(
        "let x: i32 = 2147483647; println_i32(x);", // i32::MAX
        "2147483647\n",
        0,
    );

    test_runtime_program(
        "let x: i32 = 2147483647; let y: i32 = x + 1; let zero: i32 = 0; println_i32(zero - y);", // i32::MIN
        "-2147483648\n",
        0,
    );
}

// Test println_bool
#[test]
fn test_println_bool() {
    test_runtime_program("println_bool(true);", "true\n", 0);

    test_runtime_program("println_bool(false);", "false\n", 0);

    test_runtime_program("println_bool(5 > 3);", "true\n", 0);

    test_runtime_program("println_bool(2 == 3);", "false\n", 0);
}

// Test println_unit
#[test]
fn test_println_unit() {
    test_runtime_program(
        r#"
fn get_unit() -> () {}

fn main() -> i64 {
    let u: () = get_unit();
    println_unit(u);
    0
}
        "#,
        "()\n",
        0,
    );

    test_runtime_program(
        r#"
fn make_unit() -> () { 
    let x: i64 = 42;
}

fn main() -> i64 {
    let x: () = make_unit();
    println_unit(x);
    0
}
        "#,
        "()\n",
        0,
    );
}

// Test multiple prints
#[test]
fn test_multiple_prints() {
    test_runtime_program(
        r#"
println_i64(1);
println_i64(2);
println_i64(3);
        "#,
        "1\n2\n3\n",
        0,
    );

    test_runtime_program(
        r#"
println_bool(true);
let x: i32 = 42;
println_i32(x);
// Skip unit test in this multi-test since we can't define functions in main
let hundred: i64 = 100;
let zero: i64 = 0;
println_i64(zero - hundred);
        "#,
        "true\n42\n-100\n",
        0,
    );
}

// Test input function
#[test]
fn test_input_function() {
    test_runtime_program_with_input(
        r#"
let x: i64 = input();
println_i64(x);
        "#,
        "42\n",
        "42\n",
        0,
    );

    test_runtime_program_with_input(
        r#"
let x: i64 = input();
let two: i64 = 2;
println_i64(x * two);
        "#,
        "21\n",
        "42\n",
        0,
    );

    // Note: Current implementation reads all input at once
    // so multiple input() calls need separate invocations
    test_runtime_program_with_input(
        r#"
let x: i64 = input();
println_i64(x);
        "#,
        "30\n",
        "30\n",
        0,
    );
}

// Test input with whitespace
#[test]
fn test_input_whitespace() {
    test_runtime_program_with_input(
        r#"
let x: i64 = input();
println_i64(x);
        "#,
        "  42  \n",
        "42\n",
        0,
    );

    test_runtime_program_with_input(
        r#"
let x: i64 = input();
println_i64(x);
        "#,
        "\t\n  -123  \n",
        "-123\n",
        0,
    );
}

// Test input parse errors (should return 0)
#[test]
fn test_input_parse_errors() {
    test_runtime_program_with_input(
        r#"
let x: i64 = input();
println_i64(x);
        "#,
        "abc\n",
        "0\n",
        0,
    );

    test_runtime_program_with_input(
        r#"
let x: i64 = input();
println_i64(x);
        "#,
        "123abc\n",
        "123\n", // Should parse the valid prefix
        0,
    );

    test_runtime_program_with_input(
        r#"
let x: i64 = input();
println_i64(x);
        "#,
        "\n", // Empty input
        "0\n",
        0,
    );
}

// Test division by zero error
#[test]
fn test_division_by_zero() {
    test_runtime_program(
        r#"
let ten: i64 = 10;
let zero: i64 = 0;
let x: i64 = ten / zero;
        "#,
        "",
        250, // Division by zero exit code
    );

    test_runtime_program(
        r#"
let ten: i64 = 10;
let zero: i64 = 0;
let x: i64 = ten % zero;
        "#,
        "",
        250, // Division by zero exit code
    );
}

// Test complex program with I/O
#[test]
fn test_interactive_program() {
    // Simplified test due to input buffering limitations
    test_runtime_program_with_input(
        r#"
println_i64(1234);
let a: i64 = input();
println_i64(a);
let five: i64 = 5;
println_bool(a > five);
let diff: i64 = a - five;
exit(diff);
        "#,
        "10\n",
        "1234\n10\ntrue\n",
        5, // 10 - 5 = 5
    );
}
