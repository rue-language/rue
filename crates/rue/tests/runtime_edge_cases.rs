//! Runtime edge case tests
//!
//! Tests for edge cases in the runtime functions, including:
//! - Integer overflow in input/atoi
//! - i64::MIN with println_i64
//! - Non-numeric input handling
//! - Very long input lines

use rue_compiler::{RueDatabase, SourceFile, compile_file};
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::{NamedTempFile, TempDir};

fn compile_and_run(source: &str, input: Option<&str>) -> Result<(i32, String), String> {
    // Create a temporary directory to hold files
    let temp_dir = TempDir::new().unwrap();

    let mut source_file = NamedTempFile::new_in(&temp_dir).unwrap();
    write!(source_file, "{source}").unwrap();
    source_file.flush().unwrap();

    let output_path = temp_dir.path().join("test_executable");

    // Set up Salsa database
    let db = RueDatabase::default();
    let file = SourceFile::new(
        &db,
        source_file.path().to_string_lossy().to_string(),
        source.to_string(),
    );

    // Compile
    let executable =
        compile_file(&db, file).map_err(|e| format!("Compilation failed: {}", e.message))?;

    // Write executable
    fs::write(&output_path, &*executable)
        .map_err(|e| format!("Failed to write executable: {e}"))?;

    // Make executable on Unix systems
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&output_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&output_path, perms).unwrap();
    }

    // Ensure file is fully written and synced to disk
    // This helps avoid "Text file busy" errors in CI
    std::fs::File::open(&output_path)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("Failed to sync executable: {e}"))?;

    // Small delay to ensure filesystem has released the file
    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut cmd = Command::new(&output_path);
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to run program: {e}"))?;

    if let Some(input_data) = input {
        let stdin = child.stdin.as_mut().unwrap();
        stdin
            .write_all(input_data.as_bytes())
            .map_err(|e| format!("Failed to write input: {e}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait for program: {e}"))?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !stderr.is_empty() {
        eprintln!("STDERR: {stderr}");
    }

    Ok((exit_code, stdout))
}

#[test]
fn test_println_i64_min() {
    let source = r#"
        fn main() {
            let y: i64 = 9223372036854775807;
            let zero: i64 = 0;
            let one: i64 = 1;
            let x: i64 = zero - y - one;
            println_i64(x);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "-9223372036854775808");
}

#[test]
fn test_println_i64_max() {
    let source = r#"
        fn main() {
            println_i64(9223372036854775807);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "9223372036854775807");
}

#[test]
fn test_input_non_numeric() {
    let source = r#"
        fn main() {
            let x = input();
            println_i64(x);
        }
    "#;

    // Non-numeric input should be handled gracefully (returns 0)
    let (exit_code, output) = compile_and_run(source, Some("hello\n")).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "0");
}

#[test]
fn test_input_with_whitespace() {
    let source = r#"
        fn main() {
            let x = input();
            println_i64(x);
        }
    "#;

    // Should skip leading whitespace
    let (exit_code, output) = compile_and_run(source, Some("  \t 42\n")).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "42");
}

#[test]
fn test_input_negative_number() {
    let source = r#"
        fn main() {
            let x = input();
            println_i64(x);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, Some("-123\n")).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "-123");
}

#[test]
fn test_input_positive_sign() {
    let source = r#"
        fn main() {
            let x = input();
            println_i64(x);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, Some("+456\n")).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "456");
}

#[test]
fn test_input_overflow_large_positive() {
    let source = r#"
        fn main() {
            let x = input();
            println_i64(x);
        }
    "#;

    // Number larger than i64::MAX should return 0 (overflow)
    let (exit_code, output) =
        compile_and_run(source, Some("99999999999999999999999999\n")).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "0");
}

#[test]
fn test_input_overflow_large_negative() {
    let source = r#"
        fn main() {
            let x = input();
            println_i64(x);
        }
    "#;

    // Number smaller than i64::MIN should return 0 (overflow)
    let (exit_code, output) =
        compile_and_run(source, Some("-99999999999999999999999999\n")).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "0");
}

#[test]
fn test_input_empty_after_whitespace() {
    let source = r#"
        fn main() {
            let x = input();
            println_i64(x);
        }
    "#;

    // Only whitespace should return 0
    let (exit_code, output) = compile_and_run(source, Some("   \t\n")).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "0");
}

#[test]
fn test_input_mixed_alphanumeric() {
    let source = r#"
        fn main() {
            let x = input();
            println_i64(x);
        }
    "#;

    // Should parse until first non-digit
    let (exit_code, output) = compile_and_run(source, Some("123abc456\n")).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "123");
}

#[test]
fn test_input_max_buffer() {
    let source = r#"
        fn main() {
            let x = input();
            println_i64(x);
        }
    "#;

    // Create a very long number (but still valid)
    let long_number = "1234567890".repeat(10); // 100 digits, well within buffer
    let input = format!("{long_number}\n");

    let (exit_code, output) = compile_and_run(source, Some(&input)).unwrap();
    assert_eq!(exit_code, 0);
    // Should handle overflow and return 0
    assert_eq!(output.trim(), "0");
}

#[test]
fn test_println_i32_min() {
    let source = r#"
        fn main() {
            let x: i32 = 0 - 2147483647;
            let x = x - 1;
            println_i32(x);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "-2147483648");
}

#[test]
fn test_println_i32_max() {
    let source = r#"
        fn main() {
            let x: i32 = 2147483647;
            println_i32(x);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "2147483647");
}

#[test]
fn test_println_bool_edge_cases() {
    let source = r#"
        fn main() {
            println_bool(true);
            println_bool(false);
            println_bool(1 == 1);
            println_bool(1 == 2);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "true\nfalse\ntrue\nfalse");
}

#[test]
fn test_division_by_zero_handled() {
    let source = r#"
        fn main() {
            let x: i64 = 10;
            let y: i64 = 0;
            let z = x / y;
            println_i64(z);
        }
    "#;

    let (exit_code, _output) = compile_and_run(source, None).unwrap();
    // Should exit with division by zero error code
    assert_eq!(exit_code, 250);
}
